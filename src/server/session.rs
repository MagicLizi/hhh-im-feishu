//! 账号的一次会话（protocol-v1 §4.2、§6）：打开（含附录 D 的交接）、续期、授权失效、退出登录。
//!
//! 续期照 HHH Friend 对内置飞书的做法（`im/grant.rs`、`im/chat.rs`）：
//! - 用之前看一眼：离过期不足 5 分钟就换新，临时失败时先用旧的；已过期必须换新，临时失败回“暂时连不上”；
//! - 飞书说授权无效：强制续期一次再试一次；续期后仍被拒、或续期被拒，就是登录失效（发 `session/expired`）；
//! - 换新先存后用：新的授权经 `credential/save` 存进宿主、宿主回了成功才用（飞书的刷新令牌只能用一次）；
//! - 同一时间只换一次：续期、登录保存串行，并发的请求共用一次换新；
//! - 借来的凭据（`credentialReadOnly`）只用不续：打开时已经过期，回“暂时连不上”并说下一步（同 Friend 借用的授权），
//!   宿主稍后再开时带上借出方续过的那份；用着用着到期或被拒，发 `session/expired` 并说下一步，宿主据此重读借出方存着的
//!   凭据、变了就重开（契约 provider-protocol v12 缺口 7），这次请求同 Friend 回暂时连不上。
//!
//! 话分两处（同内置）：授权失效的错误（宿主的提示条、弹窗）写 Friend 的 `ReauthReason::message`（[`error_text`]）；
//! `session/expired` 与 `session/open` 的需要登录写重新登录卡片上的那几句（[`card_text`]，宿主画在卡片上）。
//!
//! 另外存凭据时带 `renewAtMs`（过期前 5 分钟），宿主到时叫 `session/renew`——插件没在跑时也能续上，同 Friend 给
//! 关着的平台照常续期。

use std::future::Future;
use std::sync::Arc;

use hhh_im_protocol::method as pm;
use hhh_im_protocol::rpc::RpcError;
use serde_json::{Value, json};

use super::credential::{self, Credential, Freshness, KnownAccount, Login};
use super::{CONNECTION, Server, errors, relay, wire};
use crate::{AuthGrant, ErrorKind, FeishuAuth, FeishuClient, ImError, ImResult, ReauthReason};

/// 一次会话的状态。
pub(crate) enum State {
    /// 没打开，或需要登录：留着宿主还回来的凭据（自己的应用登录时沿用里面的 App Secret）。
    Closed { saved: Option<Credential> },
    /// 在线。
    Online(Online),
    /// 发过 `session/expired`：重新登录之前，之后的请求一律回授权失效（借来的凭据回暂时连不上）。
    Expired {
        saved: Option<Credential>,
        reason: ReauthReason,
        /// 失效的是借来的凭据。
        borrowed: bool,
    },
}

/// 在线时的客户端与它用的凭据。
#[derive(Clone)]
pub(crate) struct Online {
    pub(crate) client: Arc<FeishuClient>,
    pub(crate) credential: Credential,
    /// 凭据是借来的：不换新、不存。
    pub(crate) read_only: bool,
}

/// 授权失效的错误里的话：同 Friend 对内置飞书的 `ReauthReason::message`（宿主显示在提示条、弹窗里）。
pub(crate) fn error_text(reason: ReauthReason) -> &'static str {
    match reason {
        ReauthReason::Expired => "飞书登录已失效，请重新登录。",
        ReauthReason::RefreshDisabled => crate::facts::reauth::REFRESH_DISABLED,
        ReauthReason::AppCredentialInvalid => crate::facts::reauth::APP_SECRET_INVALID,
    }
}

/// 重新登录卡片上的话：内置飞书按原因写的那几句（`connection.rs` 的 `reauth_text`）。宿主把 `session/expired` 与
/// `session/open` 需要登录里的话画在卡片上，三种原因照样说得不一样。
pub(crate) fn card_text(reason: ReauthReason) -> &'static str {
    match reason {
        ReauthReason::Expired => crate::facts::reauth::EXPIRED,
        ReauthReason::RefreshDisabled => crate::facts::reauth::REFRESH_DISABLED,
        ReauthReason::AppCredentialInvalid => crate::facts::reauth::APP_SECRET_INVALID,
    }
}

/// 借来的授权过期、没人续期时的话（同 Friend 的 `borrowed_grant_expired`）。
const BORROWED_EXPIRED: &str =
    "隔离测试不刷新你的飞书授权：请打开你的 Friend 刷新，或在测试里重新授权。";
/// 续期暂时失败、旧授权又已过期时的话（同 Friend）。
const UNAVAILABLE: &str = "暂时无法连接飞书，请稍后重试。";
/// 还没登录。
const NOT_LOGGED_IN: &str = "飞书还没有登录，请先登录。";

/// 网络中断、限流与读取预算属于临时故障。
fn transient(error: &ImError) -> bool {
    matches!(
        error.kind,
        ErrorKind::Disconnected | ErrorKind::RateLimited | ErrorKind::LimitExceeded
    )
}

/// 续期失败要用户做什么；临时故障为空（同 Friend 的 `grant::reauth_reason`）。
fn reauth_reason(error: &ImError) -> Option<ReauthReason> {
    if transient(error) {
        return None;
    }
    Some(match error.reauth {
        Some(ReauthReason::RefreshDisabled) => ReauthReason::RefreshDisabled,
        Some(ReauthReason::AppCredentialInvalid) => ReauthReason::AppCredentialInvalid,
        Some(ReauthReason::Expired) | None => ReauthReason::Expired,
    })
}

/// 需要登录的原因。
fn needs_login(reason: ReauthReason, message: &str) -> Value {
    let reason = match reason {
        ReauthReason::AppCredentialInvalid => pm::NeedsLoginReason::CredentialInvalid,
        ReauthReason::Expired | ReauthReason::RefreshDisabled => pm::NeedsLoginReason::Expired,
    };
    serde_json::to_value(pm::SessionOpenResult::NeedsLogin {
        reason,
        message: message.to_owned(),
    })
    .expect("结果总能写成 JSON")
}

/// 现在的 Unix 秒。
pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// 开会话的结局：在线，或需要登录（原因与说明）。
enum Opened {
    Online(Online, bool),
    NeedsLogin(ReauthReason, String),
}

impl Server {
    /// `session/open`。
    pub(crate) async fn session_open(
        &self,
        params: pm::SessionOpenParams,
    ) -> Result<Value, RpcError> {
        let read_only = params.credential_read_only;
        if params.credential.is_none()
            && let Some(legacy) = params.legacy.as_ref()
            && legacy.format == pm::LEGACY_FORMAT_BUILTIN_V1
        {
            return self.adopt_legacy(legacy, read_only).await;
        }
        let Some(text) = params.credential.as_deref() else {
            *self.state.write().await = State::Closed { saved: None };
            return Ok(serde_json::to_value(pm::SessionOpenResult::NeedsLogin {
                reason: pm::NeedsLoginReason::NoCredential,
                message: NOT_LOGGED_IN.into(),
            })
            .expect("结果总能写成 JSON"));
        };
        let Some(saved) = Credential::parse(text) else {
            tracing::warn!("宿主还回来的飞书凭据读不懂，需要重新登录");
            *self.state.write().await = State::Closed { saved: None };
            return Ok(needs_login(
                ReauthReason::AppCredentialInvalid,
                card_text(ReauthReason::Expired),
            ));
        };
        match self.establish(saved.clone(), read_only).await? {
            Opened::Online(online, _) => {
                let account = wire::account(online.client.account());
                self.go_online(online).await;
                Ok(serde_json::to_value(pm::SessionOpenResult::Online {
                    account,
                    adopted_settings: None,
                })
                .expect("结果总能写成 JSON"))
            }
            Opened::NeedsLogin(reason, message) => {
                *self.state.write().await = State::Closed { saved: Some(saved) };
                Ok(needs_login(reason, &message))
            }
        }
    }

    /// 附录 D：接过 HHH 内置飞书的授权。先换成插件的凭据、核验账号，经 `credential/save` 存好、宿主回了成功，
    /// 再回在线与 `adoptedSettings`；没接成（内置那份用不了）就回需要登录，内置那份宿主留着。
    async fn adopt_legacy(
        &self,
        legacy: &pm::LegacyHandover,
        read_only: bool,
    ) -> Result<Value, RpcError> {
        let credential = match credential::from_legacy(&legacy.credential, &legacy.settings) {
            Ok(credential) => credential,
            Err(message) => {
                tracing::warn!(%message, "内置飞书交来的授权用不了，不接");
                *self.state.write().await = State::Closed { saved: None };
                return Ok(needs_login(ReauthReason::AppCredentialInvalid, &message));
            }
        };
        let known = credential.account.clone();
        let (online, saved) = match self.establish(credential, read_only).await? {
            Opened::Online(online, saved) => (online, saved),
            Opened::NeedsLogin(reason, message) => {
                tracing::warn!(?reason, "内置飞书交来的授权已经失效，不接");
                *self.state.write().await = State::Closed { saved: None };
                return Ok(needs_login(reason, &message));
            }
        };
        let account = online.client.account().clone();
        if let Some(known) = &known
            && known.id != account.user.id
        {
            tracing::warn!("交接来的授权核验出的账号与内置飞书记着的不同，按核验出的账号上线");
        }
        // 先存后用（附录 D、§6.3）：续期时已经存过就不再存一遍。
        if !read_only && !saved {
            let credential = online.credential.clone();
            if let Err(reason) = self.save_credential(&credential).await {
                tracing::warn!(%reason, "交接来的飞书凭据没存进宿主，这次不算接过");
                return Err(errors::of(
                    ErrorKind::Disconnected,
                    format!("飞书登录没交接成：{reason}"),
                ));
            }
        }
        let adopted = (!read_only).then(|| pm::AdoptedSettings {
            parameters: Default::default(),
            login: Some(adopted_login(&online.credential.login)),
        });
        tracing::info!(
            method = online.credential.login.method_id(),
            "接过了 HHH 内置飞书的登录"
        );
        self.go_online(online).await;
        Ok(serde_json::to_value(pm::SessionOpenResult::Online {
            account: wire::account(&account),
            adopted_settings: adopted,
        })
        .expect("结果总能写成 JSON"))
    }

    /// 用一份凭据上线：按需续期（续了就先存），核验账号；飞书说授权无效时强制续期一次。
    /// 回在线（第二项：这次已经存过凭据）或需要登录；网络这类暂时故障回错误，宿主稍后再开。
    async fn establish(&self, credential: Credential, read_only: bool) -> Result<Opened, RpcError> {
        let _turn = self.gate.lock().await;
        let mut credential = credential;
        let mut grant = credential.grant().map_err(|error| errors::rpc(&error))?;
        let mut renewed = false;
        let mut saved = false;
        match credential::freshness(&grant, now_secs()) {
            Freshness::Current => {}
            Freshness::RefreshSoon if read_only => {}
            _ if read_only => return Err(errors::of(ErrorKind::Disconnected, BORROWED_EXPIRED)),
            Freshness::Unrenewable => {
                return Ok(Opened::NeedsLogin(
                    ReauthReason::Expired,
                    card_text(ReauthReason::Expired).into(),
                ));
            }
            need => match self.refresh(&credential, &grant).await {
                Ok(fresh) => {
                    credential = credential.with_grant(&fresh);
                    saved = self.save_or_keep(&credential).await;
                    grant = fresh;
                    renewed = true;
                }
                Err(error) => match reauth_reason(&error) {
                    None if need == Freshness::RefreshSoon => {
                        tracing::warn!(%error, "飞书授权续期暂时失败，继续使用尚未过期的授权");
                    }
                    None => return Err(errors::of(ErrorKind::Disconnected, UNAVAILABLE)),
                    Some(reason) => {
                        tracing::warn!(%error, ?reason, "飞书拒绝续期授权");
                        return Ok(Opened::NeedsLogin(reason, card_text(reason).into()));
                    }
                },
            },
        }
        loop {
            match self.connect(grant.clone()).await {
                Ok(client) => {
                    let account = client.account();
                    credential.account = Some(KnownAccount {
                        id: account.user.id.clone(),
                        name: account.user.name.clone(),
                    });
                    return Ok(Opened::Online(
                        Online {
                            client: Arc::new(client),
                            credential,
                            read_only,
                        },
                        saved,
                    ));
                }
                Err(error) if error.kind == ErrorKind::AuthExpired && read_only => {
                    tracing::warn!(%error, "借来的飞书授权被拒绝，不替用户续期");
                    return Err(errors::of(ErrorKind::Disconnected, BORROWED_EXPIRED));
                }
                Err(error) if error.kind == ErrorKind::AuthExpired && !renewed => {
                    tracing::warn!(%error, "飞书认为授权无效，强制续期一次");
                    renewed = true;
                    if grant.refresh_token().is_none() {
                        return Ok(Opened::NeedsLogin(
                            ReauthReason::Expired,
                            card_text(ReauthReason::Expired).into(),
                        ));
                    }
                    match self.refresh(&credential, &grant).await {
                        Ok(fresh) => {
                            credential = credential.with_grant(&fresh);
                            saved = self.save_or_keep(&credential).await;
                            grant = fresh;
                        }
                        Err(error) => match reauth_reason(&error) {
                            None => return Err(errors::of(ErrorKind::Disconnected, UNAVAILABLE)),
                            Some(reason) => {
                                return Ok(Opened::NeedsLogin(reason, card_text(reason).into()));
                            }
                        },
                    }
                }
                Err(error) if error.kind == ErrorKind::AuthExpired => {
                    tracing::warn!(%error, "续期后的飞书授权仍被拒绝");
                    return Ok(Opened::NeedsLogin(
                        ReauthReason::Expired,
                        card_text(ReauthReason::Expired).into(),
                    ));
                }
                Err(error) => return Err(errors::rpc(&error)),
            }
        }
    }

    /// 核验授权、建客户端（读一次本人身份）。
    pub(crate) async fn connect(&self, grant: AuthGrant) -> ImResult<FeishuClient> {
        FeishuClient::connect(self.options.config.clone(), CONNECTION, grant).await
    }

    /// 按凭据里的登录方式续期：通过中继的经登录时的中继，自己的应用在本机。
    async fn refresh(&self, credential: &Credential, grant: &AuthGrant) -> ImResult<AuthGrant> {
        match &credential.login {
            Login::Relay { .. } => {
                let base = credential.login.relay_base().unwrap_or_default();
                let relay = relay::Relay::new(&base)
                    .map_err(|reason| ImError::new(ErrorKind::InvalidRequest, reason))?;
                relay.refresh_feishu(grant, now_secs()).await
            }
            Login::Own { app_id, app_secret } => {
                FeishuAuth::new(
                    self.options.config.clone(),
                    app_id.clone(),
                    app_secret.clone(),
                )?
                .refresh_auth(grant)
                .await
            }
        }
    }

    /// 把凭据交给宿主存（`credential/save`，带下次续期的时间），等宿主回复。
    pub(crate) async fn save_credential(&self, credential: &Credential) -> Result<(), String> {
        let params = pm::CredentialSaveParams {
            credential: credential.to_text(),
            renew_at_ms: credential.renew_at_ms(),
        };
        self.peer
            .request(hhh_im_protocol::Method::CredentialSave.as_str(), params)
            .await
            .map(|_| ())
            .map_err(|error| {
                error
                    .data
                    .and_then(|data| data.user_message)
                    .unwrap_or(error.message)
            })
    }

    /// 续期换到的新授权存进宿主。存不进时照样用新的（刷新令牌只能用一次，旧的已经作废）：本次进程里照常读写，
    /// 进程重启后宿主还回来的旧凭据续不上，会请用户重新登录（同 Friend 续期后保存失败的结局）。
    async fn save_or_keep(&self, credential: &Credential) -> bool {
        match self.save_credential(credential).await {
            Ok(()) => true,
            Err(reason) => {
                tracing::error!(%reason, "飞书登录已续期，但没存进宿主");
                false
            }
        }
    }

    /// 上线：换上新的客户端与凭据。
    pub(crate) async fn go_online(&self, online: Online) {
        *self.state.write().await = State::Online(online);
    }

    /// 宿主还回来过的凭据（自己的应用登录时沿用里面的 App Secret）。
    pub(crate) async fn saved_credential(&self) -> Option<Credential> {
        match &*self.state.read().await {
            State::Closed { saved } | State::Expired { saved, .. } => saved.clone(),
            State::Online(online) => Some(online.credential.clone()),
        }
    }

    /// 登录失效：记下、发 `session/expired`（卡片上的话）；返回给这次请求的授权失效错误（Friend 的话）。
    async fn expire(&self, reason: ReauthReason) -> RpcError {
        self.mark_expired(reason, false, card_text(reason)).await;
        tracing::warn!(?reason, "飞书登录已失效，请宿主请用户重新登录");
        expired_error(reason, false)
    }

    /// 借来的凭据用着用着到期或被拒：发 `session/expired` 并说下一步，宿主据此重读借出方存着的凭据、变了就重开；
    /// 这次请求同 Friend 回暂时连不上。
    async fn expire_borrowed(&self) -> RpcError {
        self.mark_expired(ReauthReason::Expired, true, BORROWED_EXPIRED)
            .await;
        tracing::warn!("借来的飞书授权已不可用，不替用户续期，请宿主重读借出方的凭据");
        expired_error(ReauthReason::Expired, true)
    }

    /// 记下失效、发 `session/expired`。
    async fn mark_expired(&self, reason: ReauthReason, borrowed: bool, message: &str) {
        {
            let mut state = self.state.write().await;
            let saved = match &*state {
                State::Online(online) => Some(online.credential.clone()),
                State::Closed { saved } | State::Expired { saved, .. } => saved.clone(),
            };
            *state = State::Expired {
                saved,
                reason,
                borrowed,
            };
        }
        self.peer.notify(
            hhh_im_protocol::Method::SessionExpired.as_str(),
            pm::SessionExpiredParams {
                reason: errors::reauth(reason),
                message: message.to_owned(),
            },
        );
    }

    /// 现在能用的客户端：用之前看一眼，临近过期就换新（同 Friend 的 `OauthSource::client`）。
    async fn ready_client(&self) -> Result<Arc<FeishuClient>, RpcError> {
        let online = match &*self.state.read().await {
            State::Online(online) => online.clone(),
            State::Expired {
                reason, borrowed, ..
            } => return Err(expired_error(*reason, *borrowed)),
            State::Closed { .. } => {
                return Err(errors::of(ErrorKind::AuthExpired, NOT_LOGGED_IN));
            }
        };
        let grant = online
            .credential
            .grant()
            .map_err(|error| errors::rpc(&error))?;
        match credential::freshness(&grant, now_secs()) {
            Freshness::Current => Ok(online.client),
            need => self.renew_in_use(&online, need).await,
        }
    }

    /// 用着用着要换新了（临近过期或已过期）：在门里再看一次（别的请求可能刚换过），然后换新。
    async fn renew_in_use(
        &self,
        stale: &Online,
        need: Freshness,
    ) -> Result<Arc<FeishuClient>, RpcError> {
        let _turn = self.gate.lock().await;
        let current = match &*self.state.read().await {
            State::Online(online) => online.clone(),
            _ => return Err(errors::of(ErrorKind::AuthExpired, NOT_LOGGED_IN)),
        };
        if current.credential.access_token() != stale.credential.access_token() {
            return Ok(current.client);
        }
        if current.read_only {
            if need == Freshness::RefreshSoon {
                tracing::debug!("借来的飞书授权临近过期，交给原 Friend 续期");
                return Ok(current.client);
            }
            tracing::debug!(?need, "借来的飞书授权已过期");
            return Err(self.expire_borrowed().await);
        }
        if need == Freshness::Unrenewable {
            tracing::warn!("飞书授权已过期且没有刷新令牌，需要重新登录");
            return Err(self.expire(ReauthReason::Expired).await);
        }
        let grant = current
            .credential
            .grant()
            .map_err(|error| errors::rpc(&error))?;
        match self.renew_with(&current, &grant).await {
            Ok(fresh) => Ok(fresh),
            Err(Failure::Transient(error)) if need == Freshness::RefreshSoon => {
                tracing::warn!(%error, "飞书授权续期暂时失败，继续使用尚未过期的授权");
                Ok(current.client)
            }
            Err(Failure::Transient(error)) => {
                tracing::warn!(%error, "飞书授权续期暂时失败");
                Err(errors::of(ErrorKind::Disconnected, UNAVAILABLE))
            }
            Err(Failure::Rejected(reason)) => Err(self.expire(reason).await),
        }
    }

    /// 飞书在使用中说授权无效：强制续期一次（同 Friend 的 `recover`）。别的请求已经换过就直接用新的。
    async fn recover(&self, rejected: &Arc<FeishuClient>) -> Result<Arc<FeishuClient>, RpcError> {
        let _turn = self.gate.lock().await;
        let current = match &*self.state.read().await {
            State::Online(online) => online.clone(),
            _ => return Err(errors::of(ErrorKind::AuthExpired, NOT_LOGGED_IN)),
        };
        if !Arc::ptr_eq(&current.client, rejected) {
            return Ok(current.client);
        }
        let grant = current
            .credential
            .grant()
            .map_err(|error| errors::rpc(&error))?;
        if current.read_only {
            tracing::debug!("借来的飞书授权被飞书拒绝");
            return Err(self.expire_borrowed().await);
        }
        if grant.refresh_token().is_none() {
            return Err(self.expire(ReauthReason::Expired).await);
        }
        match self.renew_with(&current, &grant).await {
            Ok(fresh) => {
                tracing::info!("飞书授权已强制续期，恢复读取");
                Ok(fresh)
            }
            Err(Failure::Transient(error)) => {
                tracing::warn!(%error, "飞书授权暂时续不上");
                Err(errors::of(ErrorKind::Disconnected, UNAVAILABLE))
            }
            Err(Failure::Rejected(reason)) => Err(self.expire(reason).await),
        }
    }

    /// 在门里换新：续期、先存、再核验建新客户端、换上。
    async fn renew_with(
        &self,
        current: &Online,
        grant: &AuthGrant,
    ) -> Result<Arc<FeishuClient>, Failure> {
        let fresh = match self.refresh(&current.credential, grant).await {
            Ok(fresh) => fresh,
            Err(error) => {
                return Err(match reauth_reason(&error) {
                    None => Failure::Transient(error),
                    Some(reason) => {
                        tracing::warn!(%error, ?reason, "飞书拒绝续期授权");
                        Failure::Rejected(reason)
                    }
                });
            }
        };
        let credential = current.credential.with_grant(&fresh);
        self.save_or_keep(&credential).await;
        match self.connect(fresh).await {
            Ok(client) => {
                if client.account().user.id != current.client.account().user.id {
                    tracing::error!("续期后的飞书授权属于另一个账号，不换上");
                    return Err(Failure::Rejected(ReauthReason::Expired));
                }
                let client = Arc::new(client);
                *self.state.write().await = State::Online(Online {
                    client: client.clone(),
                    credential,
                    read_only: current.read_only,
                });
                tracing::info!("飞书授权已续期");
                Ok(client)
            }
            Err(error) if error.kind == ErrorKind::AuthExpired => {
                tracing::warn!(%error, "续期后的飞书授权仍被拒绝");
                Err(Failure::Rejected(ReauthReason::Expired))
            }
            Err(error) => Err(Failure::Transient(error)),
        }
    }

    /// 用现在能用的客户端做一件事；飞书说授权无效时强制续期一次、再做一次（同 Friend：平台判定授权无效时续期一次）。
    pub(crate) async fn with_client<T, F, Fut>(&self, work: F) -> Result<T, RpcError>
    where
        F: Fn(Arc<FeishuClient>) -> Fut,
        Fut: Future<Output = ImResult<T>>,
    {
        let client = self.ready_client().await?;
        match work(client.clone()).await {
            Ok(value) => Ok(value),
            Err(error) if error.kind == ErrorKind::AuthExpired => {
                tracing::warn!(%error, "飞书认为授权无效，强制续期一次");
                let fresh = self.recover(&client).await?;
                match work(fresh).await {
                    Ok(value) => Ok(value),
                    Err(error) if error.kind == ErrorKind::AuthExpired => {
                        tracing::warn!(%error, "续期后的飞书授权仍被拒绝");
                        Err(self.expire(ReauthReason::Expired).await)
                    }
                    Err(error) => Err(errors::rpc(&error)),
                }
            }
            Err(error) => Err(errors::rpc(&error)),
        }
    }

    /// 只用现在能用的客户端做一件事（下载资源、头像这类：不为它强制续期）。
    pub(crate) async fn with_ready_client<T, F, Fut>(&self, work: F) -> Result<T, RpcError>
    where
        F: FnOnce(Arc<FeishuClient>) -> Fut,
        Fut: Future<Output = ImResult<T>>,
    {
        let client = self.ready_client().await?;
        work(client).await.map_err(|error| errors::rpc(&error))
    }

    /// `session/renew`：用宿主存着的凭据续期，先存新的、再回成功（§6.6）。插件在线时用手里的那份（最新、宿主也存着）。
    pub(crate) async fn session_renew(
        &self,
        params: pm::SessionRenewParams,
    ) -> Result<Value, RpcError> {
        let _turn = self.gate.lock().await;
        // 在门里读：别的请求可能刚续过，用它换来的那份（宿主也存着）。
        let online = match &*self.state.read().await {
            State::Online(online) => Some(online.clone()),
            _ => None,
        };
        if let Some(online) = online {
            if online.read_only {
                return Err(errors::of(
                    ErrorKind::PermissionDenied,
                    "这份飞书凭据是借来的，不替用户续期。",
                ));
            }
            let grant = online
                .credential
                .grant()
                .map_err(|error| errors::rpc(&error))?;
            return match self.renew_with(&online, &grant).await {
                Ok(_) => Ok(json!({})),
                Err(Failure::Transient(error)) => Err(errors::rpc(&error)),
                Err(Failure::Rejected(reason)) => Err(self.expire(reason).await),
            };
        }
        let Some(credential) = Credential::parse(&params.credential) else {
            return Err(errors::rpc(&ImError {
                kind: ErrorKind::AuthExpired,
                message: error_text(ReauthReason::Expired).into(),
                platform_code: None,
                reauth: Some(ReauthReason::Expired),
                retry_after_secs: None,
            }));
        };
        let grant = credential.grant().map_err(|error| errors::rpc(&error))?;
        match self.refresh(&credential, &grant).await {
            Ok(fresh) => {
                let credential = credential.with_grant(&fresh);
                self.save_credential(&credential).await.map_err(|reason| {
                    errors::of(
                        ErrorKind::Disconnected,
                        format!("飞书登录已续期，但保存失败：{reason}。请重新登录。"),
                    )
                })?;
                if let State::Closed { saved } = &mut *self.state.write().await {
                    *saved = Some(credential);
                }
                tracing::info!("没在线时续上了飞书授权");
                Ok(json!({}))
            }
            Err(error) => match reauth_reason(&error) {
                None => Err(errors::rpc(&error)),
                Some(reason) => Err(errors::rpc(&ImError {
                    kind: ErrorKind::AuthExpired,
                    message: error_text(reason).into(),
                    platform_code: error.platform_code.clone(),
                    reauth: Some(reason),
                    retry_after_secs: None,
                })),
            },
        }
    }

    /// `session/logout`：丢掉内存里的会话（飞书没有以本人身份登出的接口，宿主随后删凭据）。
    pub(crate) async fn session_logout(&self) -> Result<Value, RpcError> {
        *self.state.write().await = State::Closed { saved: None };
        self.cancel_login(None).await;
        tracing::info!("飞书已退出登录");
        Ok(json!({}))
    }
}

/// 登录失效后回给请求的错误：Friend 的话；借来的凭据同 Friend 回暂时连不上并说下一步。
fn expired_error(reason: ReauthReason, borrowed: bool) -> RpcError {
    if borrowed {
        return errors::of(ErrorKind::Disconnected, BORROWED_EXPIRED);
    }
    errors::rpc(&ImError {
        kind: ErrorKind::AuthExpired,
        message: error_text(reason).into(),
        platform_code: None,
        reauth: Some(reason),
        retry_after_secs: None,
    })
}

/// 在门里换新没成。
enum Failure {
    /// 临时故障：授权状态不变，稍后再试。
    Transient(ImError),
    /// 平台拒绝：要用户处理。
    Rejected(ReauthReason),
}

/// 交接后请宿主记下的“现在在用”的登录方式与填的内容（中继地址；自己的应用的 App ID 与 Secret，Friend 照内置时
/// 一样记着 Secret，登录框里显示“已保存”）。
fn adopted_login(login: &Login) -> pm::AdoptedLogin {
    use hhh_im_protocol::method::login_value;
    let mut values = std::collections::BTreeMap::new();
    match login {
        Login::Relay { .. } => {
            values.insert(
                login_value::RELAY_URL.to_owned(),
                login.relay_base().unwrap_or_default(),
            );
        }
        Login::Own { app_id, app_secret } => {
            values.insert(login_value::APP_ID.to_owned(), app_id.clone());
            values.insert(login_value::APP_SECRET.to_owned(), app_secret.clone());
        }
    }
    pm::AdoptedLogin {
        method: login.method_id().to_owned(),
        values,
    }
}
