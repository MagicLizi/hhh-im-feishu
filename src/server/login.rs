//! 登录（protocol-v1 §6.1、§6.1a）：两种方式，叫法与 HHH 内置飞书的登录面板一样（`facts::login`）。
//!
//! - 通过中继（`relay`）：照 HHH Friend 内置飞书的中继登录（`server/im_login.rs`）——先用一次性 state 在中继登记、
//!   要求中继代换令牌，拿中继的 App ID 拼授权页；推“打开授权页”一步，按中继建议的间隔领取，领到的是令牌。
//! - 使用自己的应用（`own`）：回调到宿主在本机收的地址（宿主交来完整地址与一次性 state），宿主经 `login/callback`
//!   把回调交回来，用这次的 App ID 与 Secret 在本机换令牌，不经过任何服务器。
//!
//! 拿到授权后核验账号，先经 `credential/save` 存好、宿主回了成功，再上线、发 `login/finished`（§6.1：先存后用）。
//! 同一时间只有一次登录：新的 `login/start` 取消没结束的旧的（旧的不再报结局）。说给用户的话同 Friend。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use hhh_im_protocol::method::{self as pm, login_value};
use hhh_im_protocol::rpc::{RpcError, RpcId};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

use super::credential::{Credential, KnownAccount, Login};
use super::relay::{self, Claim, Relay};
use super::session::{Online, now_secs};
use super::{Server, errors, wire};
use crate::{AuthGrant, ErrorKind, FeishuAuth, LoginRequest, PendingLogin};

/// 申请的权限（同 Friend 的 `DEFAULT_FEISHU_SCOPES`）。
pub(crate) const DEFAULT_SCOPES: &[&str] = &[
    "offline_access",
    "im:chat:readonly",
    "im:message",
    "im:message.group_msg:get_as_user",
    "im:message.p2p_msg:get_as_user",
    "im:message.send_as_user",
    "contact:user:search",
    "search:message",
    // 最近聊天里显示私聊对方的名字与头像；已有授权需重新授权才生效。
    "contact:user.base:readonly",
    // 会话页即聊天里以本人身份上传图片与文件再发送；已有授权需重新授权才生效。
    "im:resource",
    // 读消息上已有的表情（别人点的、在飞书客户端里点的）；要先在飞书开发者后台开通，再重新授权才生效。
    // 点表情与取消只要 im:message。
    "im:message.reactions:read",
];

/// 自己的应用等本机回调的最长时间：同一次授权的有效期（5 分钟，同 Friend）。
const LOCAL_LOGIN_TTL: Duration = Duration::from_secs(300);
/// 中继暂时连不上时多久再领一次（同 Friend）。
const RELAY_RETRY: Duration = Duration::from_secs(2);
/// “打开授权页”一步里再打开一次的按钮文字。
const OPEN_AGAIN: &str = "再打开一次";

const TIMED_OUT: &str = "飞书登录等待已超时，请重新登录。";
const CANCELLED: &str = "已取消飞书登录。";

/// 一次登录为什么停下。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// 用户在宿主里取消（`login/cancel`）。
    Cancelled,
    /// 被新的登录取代、或退出登录：不再报结局。
    Superseded,
}

/// 正在进行的那一次登录。
struct Active {
    flow_id: String,
    stop: watch::Sender<Option<Stop>>,
    /// 自己的应用：宿主交回的回调地址从这里送进流程。
    callback: Option<mpsc::Sender<String>>,
}

/// 登录的状态：同一时间只有一次。
#[derive(Default)]
pub(crate) struct Logins {
    active: tokio::sync::Mutex<Option<Active>>,
    next: std::sync::atomic::AtomicU64,
}

fn default_scopes() -> Vec<String> {
    DEFAULT_SCOPES
        .iter()
        .map(|scope| (*scope).to_owned())
        .collect()
}

/// 准备好的一次登录：授权页、等结果的方式与登录方式。
struct Prepared {
    pending: PendingLogin,
    route: Route,
    login: Login,
}

enum Route {
    /// 经中继领取。
    Relay { relay: Relay, flow: relay::Flow },
    /// 本机回调：等宿主交回来，在本机换令牌。
    Local { auth: FeishuAuth },
}

/// 这次填的内容里的一项（去掉首尾空白，空的算没填）。
fn value<'a>(values: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    values
        .get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
}

impl Server {
    /// `login/start`：备好这次登录（中继要先登记成功），回 `{flowId}`，再推“打开授权页”一步、在后台等结果。
    /// 回复由这里直接发出（`None`），保证宿主先拿到流程编号、再收到这一步。
    pub(crate) async fn login_start(
        self: &Arc<Self>,
        id: &RpcId,
        params: pm::LoginStartParams,
    ) -> Option<Result<Value, RpcError>> {
        let prepared = match self.prepare_login(&params).await {
            Ok(prepared) => prepared,
            Err(error) => return Some(Err(error)),
        };
        let flow_id = format!(
            "feishu-login-{}",
            self.login
                .next
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1
        );
        let (stop, stopped) = watch::channel(None);
        let (callback, callbacks) = match prepared.route {
            Route::Local { .. } => {
                let (tx, rx) = mpsc::channel(1);
                (Some(tx), Some(rx))
            }
            Route::Relay { .. } => (None, None),
        };
        {
            let mut active = self.login.active.lock().await;
            if let Some(old) = active.take() {
                tracing::info!(flow = %old.flow_id, "新的飞书登录取代了没结束的旧登录");
                let _ = old.stop.send(Some(Stop::Superseded));
            }
            *active = Some(Active {
                flow_id: flow_id.clone(),
                stop,
                callback,
            });
        }
        let mode = prepared.login.method_id();
        self.peer
            .respond(id.clone(), Ok(json!({ "flowId": flow_id })));
        self.peer.notify(
            hhh_im_protocol::Method::LoginStep.as_str(),
            pm::LoginStepParams {
                flow_id: flow_id.clone(),
                step: pm::LoginStep::OpenUrl {
                    url: prepared.pending.authorization_url().to_owned(),
                    label: OPEN_AGAIN.into(),
                    note: None,
                    links: Vec::new(),
                },
            },
        );
        tracing::info!(mode, flow = %flow_id, "飞书登录已开始，等待浏览器授权");
        let server = self.clone();
        tokio::spawn(async move {
            match prepared.route {
                Route::Relay { relay, flow } => {
                    server
                        .run_relay(flow_id, relay, flow, prepared.login, stopped)
                        .await;
                }
                Route::Local { auth } => {
                    let callbacks = callbacks.expect("本机回调的登录带着回调通道");
                    server
                        .run_local(
                            flow_id,
                            auth,
                            prepared.pending,
                            prepared.login,
                            callbacks,
                            stopped,
                        )
                        .await;
                }
            }
        });
        None
    }

    /// 核对这次的方式与内容、备好授权页；不对时回给用户看的原因（同 Friend 开始登录时就地说）。
    async fn prepare_login(&self, params: &pm::LoginStartParams) -> Result<Prepared, RpcError> {
        let refuse = |message: String| errors::of(ErrorKind::InvalidRequest, message);
        // 没带方式（旧宿主）时同 Friend 没登录过：通过中继。
        match params.method.as_deref().unwrap_or("relay") {
            "relay" => {
                // 旧宿主不带方式与内容时用默认中继（同 Friend 没登录过时）；带了方式就要填地址。
                let raw = value(&params.values, login_value::RELAY_URL)
                    .or_else(|| params.method.is_none().then_some(relay::DEFAULT_RELAY))
                    .ok_or_else(|| refuse("请填写中继地址。".into()))?;
                let url = relay::normalize_relay_url(raw).map_err(refuse)?;
                let relay = Relay::new(&url).map_err(refuse)?;
                let state = crate::random_id().map_err(|error| errors::rpc(&error))?;
                let registered = relay.register(&state).await.map_err(|error| {
                    errors::of(
                        if error.transient {
                            ErrorKind::Disconnected
                        } else {
                            ErrorKind::InvalidRequest
                        },
                        error.message,
                    )
                })?;
                let pending =
                    FeishuAuth::authorize_only(self.options.config.clone(), registered.client_id)
                        .and_then(|auth| {
                            auth.begin_login_with_state(
                                LoginRequest {
                                    redirect_uri: relay.callback_url(),
                                    scopes: default_scopes(),
                                },
                                state,
                            )
                        })
                        .map_err(|error| errors::rpc(&error))?;
                Ok(Prepared {
                    pending,
                    route: Route::Relay {
                        relay,
                        flow: registered.flow,
                    },
                    login: Login::Relay {
                        relay_url: relay::stored_relay_url(&url),
                    },
                })
            }
            "own" => {
                let app_id = value(&params.values, login_value::APP_ID)
                    .ok_or_else(|| refuse("请填写飞书 App ID。".into()))?
                    .to_owned();
                let app_secret = match value(&params.values, login_value::APP_SECRET) {
                    Some(secret) => secret.to_owned(),
                    // 没填 Secret：只在现在用的就是自己的应用、App ID 没换时沿用凭据里的（同 Friend）。
                    None => match self.saved_credential().await.map(|saved| saved.login) {
                        Some(Login::Own {
                            app_id: saved_id,
                            app_secret,
                        }) if saved_id == app_id => app_secret,
                        Some(Login::Own { .. }) => {
                            return Err(refuse("换了 App ID 需要同时填写它的 App Secret。".into()));
                        }
                        _ => return Err(refuse("首次使用自己的应用需要填写 App Secret。".into())),
                    },
                };
                let callback_url = params.callback_url.clone().ok_or_else(|| {
                    refuse("宿主没有给本机回调地址，不能用自己的应用登录。".into())
                })?;
                let auth = FeishuAuth::new(
                    self.options.config.clone(),
                    app_id.clone(),
                    app_secret.clone(),
                )
                .map_err(|error| errors::rpc(&error))?;
                let request = LoginRequest {
                    redirect_uri: callback_url,
                    scopes: default_scopes(),
                };
                let pending = match &params.state {
                    Some(state) => auth.begin_login_with_state(request, state.clone()),
                    None => auth.begin_login(request),
                }
                .map_err(|error| errors::rpc(&error))?;
                Ok(Prepared {
                    pending,
                    route: Route::Local { auth },
                    login: Login::Own { app_id, app_secret },
                })
            }
            other => Err(refuse(format!("飞书没有“{other}”这种登录方式。"))),
        }
    }

    /// 经中继领取，直到领到令牌、出错、超时或取消。取消时再领最后一次：已经授权了照样完成（同 Friend）。
    async fn run_relay(
        &self,
        flow_id: String,
        relay: Relay,
        flow: relay::Flow,
        login: Login,
        mut stopped: watch::Receiver<Option<Stop>>,
    ) {
        let mut wait = self.options.relay_first_poll;
        let outcome: Result<AuthGrant, String> = loop {
            if flow.expired() {
                break Err(TIMED_OUT.into());
            }
            let cancelling = tokio::select! {
                () = tokio::time::sleep(wait) => false,
                changed = stopped.changed() => {
                    if changed.is_err() || *stopped.borrow() == Some(Stop::Superseded) {
                        return;
                    }
                    true
                }
            };
            match relay.claim(&flow).await {
                Ok(Claim::Pending { retry_after }) => {
                    if cancelling {
                        break Err(CANCELLED.into());
                    }
                    wait = retry_after;
                }
                // 中继代换好的令牌：核验账号后连同这次的方式一起保存。
                Ok(Claim::Token(token)) => match token.into_grant(now_secs()) {
                    Ok(grant) => break Ok(grant),
                    Err(reason) => break Err(format!("飞书登录失败：{reason}")),
                },
                Ok(Claim::ExchangeFailed { message }) => {
                    break Err(format!("飞书登录失败：{message}"));
                }
                // 平台在回调里给了错误（用户拒绝等）：算取消。
                Ok(Claim::Denied) => break Err(CANCELLED.into()),
                // 应当领到令牌；领到授权码说明中继没按要求代换，本机没有中继的 Secret，换不了。
                Ok(Claim::Code) => break Err("授权回调服务没有代换飞书令牌，请重新登录。".into()),
                // 网络或中继暂时有问题：还在等就下次再领（领取凭证与令牌都还在中继），取消时如实说。
                Err(error) if error.transient && !error.gone && !cancelling => wait = RELAY_RETRY,
                Err(error) => break Err(error.message),
            }
        };
        match outcome {
            Ok(grant) => self.finish_login(&flow_id, login, grant).await,
            Err(message) => self.fail_login(&flow_id, message).await,
        }
    }

    /// 等宿主交回本机回调，在本机换令牌；超时或取消就结束（回调先到的照样完成）。
    async fn run_local(
        &self,
        flow_id: String,
        auth: FeishuAuth,
        pending: PendingLogin,
        login: Login,
        mut callbacks: mpsc::Receiver<String>,
        mut stopped: watch::Receiver<Option<Stop>>,
    ) {
        let url = tokio::select! {
            biased;
            url = callbacks.recv() => match url {
                Some(url) => url,
                None => return,
            },
            changed = stopped.changed() => {
                if changed.is_err() || *stopped.borrow() == Some(Stop::Superseded) {
                    return;
                }
                return self.fail_login(&flow_id, CANCELLED.into()).await;
            }
            () = tokio::time::sleep(LOCAL_LOGIN_TTL) => {
                return self.fail_login(&flow_id, TIMED_OUT.into()).await;
            }
        };
        match auth.finish_login(pending, &url).await {
            Ok(grant) => self.finish_login(&flow_id, login, grant).await,
            Err(error) if error.kind == ErrorKind::Cancelled => {
                self.fail_login(&flow_id, CANCELLED.into()).await;
            }
            Err(error) => {
                self.fail_login(&flow_id, format!("飞书登录失败：{error}"))
                    .await
            }
        }
    }

    /// 拿到授权：核验账号，先存凭据、宿主回了成功，再上线、发 `login/finished`。
    async fn finish_login(&self, flow_id: &str, login: Login, grant: AuthGrant) {
        let client = match self.connect(grant.clone()).await {
            Ok(client) => client,
            Err(error) => {
                return self
                    .fail_login(flow_id, format!("飞书登录失败：{error}"))
                    .await;
            }
        };
        let account = client.account().clone();
        let credential = Credential::new(
            &grant,
            login,
            Some(KnownAccount {
                id: account.user.id.clone(),
                name: account.user.name.clone(),
            }),
        );
        {
            let _turn = self.gate.lock().await;
            if !self.login_is(flow_id).await {
                tracing::info!(flow = flow_id, "飞书登录已被取代，结果不提交");
                return;
            }
            if let Err(reason) = self.save_credential(&credential).await {
                return self
                    .fail_login(flow_id, format!("飞书登录未完成：{reason}"))
                    .await;
            }
            self.go_online(Online {
                client: Arc::new(client),
                credential,
                read_only: false,
            })
            .await;
        }
        self.end_login(flow_id).await;
        tracing::info!(flow = flow_id, "飞书登录完成");
        self.peer.notify(
            hhh_im_protocol::Method::LoginFinished.as_str(),
            pm::LoginFinishedParams {
                flow_id: flow_id.to_owned(),
                account: wire::account(&account),
                title: None,
                message: None,
            },
        );
    }

    /// 登录没完成：发 `login/failed`（被取代的不报）。
    async fn fail_login(&self, flow_id: &str, message: String) {
        if !self.login_is(flow_id).await {
            return;
        }
        self.end_login(flow_id).await;
        tracing::info!(flow = flow_id, reason = %message, "飞书登录未完成");
        self.peer.notify(
            hhh_im_protocol::Method::LoginFailed.as_str(),
            pm::LoginFailedParams {
                flow_id: flow_id.to_owned(),
                message,
                retryable: true,
                title: None,
                retry_label: None,
            },
        );
    }

    async fn login_is(&self, flow_id: &str) -> bool {
        self.login
            .active
            .lock()
            .await
            .as_ref()
            .is_some_and(|active| active.flow_id == flow_id)
    }

    async fn end_login(&self, flow_id: &str) {
        let mut active = self.login.active.lock().await;
        if active.as_ref().is_some_and(|a| a.flow_id == flow_id) {
            *active = None;
        }
    }

    /// `login/callback`：宿主在本机收到这次登录的回调、state 对上了，交给等着的流程。
    pub(crate) async fn login_callback(
        &self,
        params: pm::LoginCallbackParams,
    ) -> Result<Value, RpcError> {
        let sender = self
            .login
            .active
            .lock()
            .await
            .as_ref()
            .filter(|active| active.flow_id == params.flow_id)
            .and_then(|active| active.callback.clone());
        match sender {
            Some(sender) if sender.try_send(params.url).is_ok() => Ok(json!({})),
            _ => Err(errors::of(
                ErrorKind::InvalidRequest,
                "没有等待中的飞书登录，或这次登录已经结束。",
            )),
        }
    }

    /// `login/cancel`（`flow_id` 为空时是退出登录：不再报结局）。
    pub(crate) async fn cancel_login(&self, flow_id: Option<&str>) {
        let mut active = self.login.active.lock().await;
        match flow_id {
            Some(id) => {
                if let Some(active) = active.as_ref().filter(|active| active.flow_id == id) {
                    let _ = active.stop.send(Some(Stop::Cancelled));
                }
            }
            None => {
                if let Some(active) = active.take() {
                    let _ = active.stop.send(Some(Stop::Superseded));
                }
            }
        }
    }
}
