//! 协议服务（HHH-IM-Protocol v1）：HHH 从标准输入一行一条发 JSON-RPC 请求，插件从标准输出一行一条回。
//!
//! - 读手：一行一条地读，宿主的请求各开一个任务并发处理（查询不被下载、登录堵住，§13），回复与插件发出的请求、
//!   通知都排进同一条队列，由写手按顺序写（一行一条，不会搅在一起）；
//! - 插件发给宿主的请求（`credential/save`）按编号等回复（[`peer`]）；`$/cancel` 停掉对应的任务；
//! - 每个方法交给对应的模块：握手 [`declare`]、会话与续期 [`session`]、登录 [`login`]、读取与动作 [`methods`]、
//!   资源与上传 [`resources`]；统一类型翻成线上类型在 [`wire`]。
//!
//! 一个进程只服务一个账号（§1），所以适配器里的连接编号固定写 [`CONNECTION`]，线上不出现。

mod credential;
mod declare;
mod errors;
mod log;
mod login;
mod methods;
mod peer;
mod relay;
mod resources;
mod session;
pub(crate) mod wire;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use hhh_im_protocol::Method;
use hhh_im_protocol::rpc::{RpcId, RpcMessage, codec};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::task::AbortHandle;

use self::methods::params;
use self::peer::Peer;
use crate::{ErrorKind, FeishuConfig};

/// 适配器里的连接编号：一个进程只有一个账号，固定这一个（协议里不出现）。
pub(crate) const CONNECTION: &str = "feishu";

/// 插件服务的设置（正式运行用默认值；测试换成本机的假飞书、调短等待）。
#[derive(Debug, Clone)]
pub struct Options {
    /// 飞书的服务地址。
    pub config: FeishuConfig,
    /// 通过中继登录时，打开授权页后多久第一次领取结果（之后按中继建议的间隔）。
    pub relay_first_poll: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            config: FeishuConfig::default(),
            relay_first_poll: relay::RETRY_DEFAULT,
        }
    }
}

/// 一个插件进程里的全部状态。
pub(crate) struct Server {
    pub(crate) peer: Peer,
    pub(crate) options: Options,
    /// 账号的会话。
    pub(crate) state: RwLock<session::State>,
    /// 续期、登录保存串行。
    pub(crate) gate: Mutex<()>,
    pub(crate) login: login::Logins,
    pub(crate) resources: resources::Resources,
    pub(crate) sends: methods::Sends,
    /// 正在处理的宿主请求（编号 → 任务），`$/cancel` 用。
    tasks: std::sync::Mutex<HashMap<String, AbortHandle>>,
}

/// 用标准输入、标准输出跑协议服务，直到宿主关掉标准输入；日志交给宿主（`log/write`）。
pub async fn run_stdio() {
    let (server, writes) = Server::new(Options::default());
    log::install(server.peer.sender());
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "飞书插件已启动");
    serve_with(server, tokio::io::stdin(), tokio::io::stdout(), writes).await;
}

/// 在给定的输入、输出上跑协议服务（测试用进程内的管道），直到输入关掉。
pub async fn serve<R, W>(input: R, output: W, options: Options)
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (server, writes) = Server::new(options);
    serve_with(server, input, output, writes).await;
}

async fn serve_with<R, W>(
    server: Arc<Server>,
    input: R,
    output: W,
    writes: mpsc::UnboundedReceiver<RpcMessage>,
) where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(write_loop(output, writes));
    let mut lines = BufReader::new(input).lines();
    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(%error, "标准输入读不下去了，停下");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let message = match codec::decode_line(&line) {
            Ok(message) => message,
            Err(error) => {
                tracing::warn!(%error, "宿主发来一行不是 JSON-RPC，跳过");
                continue;
            }
        };
        server.receive(message);
    }
    // 宿主关了标准输入：停掉还在跑的请求。
    let handles: Vec<AbortHandle> = server
        .tasks
        .lock()
        .expect("任务表锁中毒")
        .drain()
        .map(|(_, handle)| handle)
        .collect();
    for handle in handles {
        handle.abort();
    }
    tracing::info!("宿主关了标准输入，飞书插件退出");
}

/// 写手：一条一行写到输出，每条都立刻送出去。
async fn write_loop<W: AsyncWrite + Unpin>(
    mut output: W,
    mut writes: mpsc::UnboundedReceiver<RpcMessage>,
) {
    while let Some(message) = writes.recv().await {
        let Ok(line) = codec::encode_line(&message) else {
            continue;
        };
        if output.write_all(line.as_bytes()).await.is_err() || output.flush().await.is_err() {
            break;
        }
    }
}

impl Server {
    fn new(options: Options) -> (Arc<Self>, mpsc::UnboundedReceiver<RpcMessage>) {
        let (peer, writes) = Peer::new();
        (
            Arc::new(Self {
                peer,
                options,
                state: RwLock::new(session::State::Closed { saved: None }),
                gate: Mutex::new(()),
                login: login::Logins::default(),
                resources: resources::Resources::default(),
                sends: methods::Sends::default(),
                tasks: std::sync::Mutex::new(HashMap::new()),
            }),
            writes,
        )
    }

    /// 收到宿主的一条消息：回复交给在等的请求，通知当场处理，请求各开一个任务。
    fn receive(self: &Arc<Self>, message: RpcMessage) {
        if message.is_response() {
            self.peer.answer(message);
            return;
        }
        let (Some(method), params) = (message.method, message.params.unwrap_or(Value::Null)) else {
            return;
        };
        let Some(id) = message.id else {
            let server = self.clone();
            tokio::spawn(async move { server.notified(&method, params).await });
            return;
        };
        let key = id.to_string();
        let server = self.clone();
        // 先拿着任务表再开任务：任务做完删自己那一项时，表里一定已经记上了。
        let mut tasks = self.tasks.lock().expect("任务表锁中毒");
        let task = tokio::spawn(async move {
            if let Some(result) = server.handle(&id, &method, params).await {
                server.peer.respond(id.clone(), result);
            }
            server
                .tasks
                .lock()
                .expect("任务表锁中毒")
                .remove(&id.to_string());
        });
        tasks.insert(key, task.abort_handle());
    }

    /// 宿主发来的通知。
    async fn notified(&self, method: &str, params: Value) {
        match method {
            "$/cancel" => {
                let Ok(cancel) =
                    serde_json::from_value::<hhh_im_protocol::method::CancelParams>(params)
                else {
                    return;
                };
                let handle = self
                    .tasks
                    .lock()
                    .expect("任务表锁中毒")
                    .remove(&cancel.id.to_string());
                if let Some(handle) = handle {
                    handle.abort();
                    self.peer.respond(
                        cancel.id,
                        Err(errors::of(ErrorKind::Cancelled, "宿主取消了这个请求。")),
                    );
                }
            }
            "login/cancel" => {
                if let Ok(cancel) =
                    serde_json::from_value::<hhh_im_protocol::method::LoginCancelParams>(params)
                {
                    self.cancel_login(Some(&cancel.flow_id)).await;
                }
            }
            // 飞书的登录没有表单这一步。
            "login/submit" => tracing::debug!("飞书登录没有表单，忽略 login/submit"),
            other => tracing::debug!(method = other, "不认识的通知，忽略"),
        }
    }

    /// 宿主发来的请求；回 `None` 表示已经自己回过了（`login/start`）。
    async fn handle(
        self: &Arc<Self>,
        id: &RpcId,
        method: &str,
        value: Value,
    ) -> Option<Result<Value, hhh_im_protocol::RpcError>> {
        let Some(known) = Method::from_name(method) else {
            return Some(Err(errors::method_not_found(method)));
        };
        Some(match known {
            Method::Initialize => match params(value) {
                Ok(params) => declare::initialize(params),
                Err(error) => Err(error),
            },
            Method::Ping | Method::Shutdown => Ok(serde_json::json!({})),
            Method::SessionOpen => match params(value) {
                Ok(params) => self.session_open(params).await,
                Err(error) => Err(error),
            },
            Method::SessionLogout => self.session_logout().await,
            Method::SessionRenew => match params(value) {
                Ok(params) => self.session_renew(params).await,
                Err(error) => Err(error),
            },
            Method::LoginStart => match params(value) {
                Ok(params) => return self.login_start(id, params).await,
                Err(error) => Err(error),
            },
            Method::LoginCallback => match params(value) {
                Ok(params) => self.login_callback(params).await,
                Err(error) => Err(error),
            },
            Method::ConversationsList => match params(value) {
                Ok(params) => self.conversations_list(params).await,
                Err(error) => Err(error),
            },
            Method::ConversationsGet => match params(value) {
                Ok(params) => self.conversations_get(params).await,
                Err(error) => Err(error),
            },
            Method::ConversationsSearch => match params(value) {
                Ok(params) => self.conversations_search(params).await,
                Err(error) => Err(error),
            },
            Method::ConversationsMembers => match params(value) {
                Ok(params) => self.conversations_members(params).await,
                Err(error) => Err(error),
            },
            Method::ConversationsFindDirect => match params(value) {
                Ok(params) => self.conversations_find_direct(params).await,
                Err(error) => Err(error),
            },
            Method::ConversationsRecentActivity => match params(value) {
                Ok(params) => self.conversations_recent_activity(params).await,
                Err(error) => Err(error),
            },
            Method::PeopleGet => match params(value) {
                Ok(params) => self.people_get(params).await,
                Err(error) => Err(error),
            },
            Method::PeopleSearch => match params(value) {
                Ok(params) => self.people_search(params).await,
                Err(error) => Err(error),
            },
            Method::MessagesList => match params(value) {
                Ok(params) => self.messages_list(params).await,
                Err(error) => Err(error),
            },
            Method::MessagesGet => match params(value) {
                Ok(params) => self.messages_get(params).await,
                Err(error) => Err(error),
            },
            Method::ReactionsList => match params(value) {
                Ok(params) => self.reactions_list(params).await,
                Err(error) => Err(error),
            },
            Method::EmojiCatalog => self.emoji_catalog().await,
            Method::ResourcesRead => match params(value) {
                Ok(params) => self.read_resource(params).await,
                Err(error) => Err(error),
            },
            Method::AvatarsRead => match params(value) {
                Ok(params) => self.read_avatar(params).await,
                Err(error) => Err(error),
            },
            Method::EmojiImage => match params(value) {
                Ok(params) => self.read_emoji_image(params).await,
                Err(error) => Err(error),
            },
            Method::UploadsBegin => params(value).and_then(|params| self.upload_begin(params)),
            Method::UploadsChunk => params(value).and_then(|params| self.upload_chunk(params)),
            Method::UploadsFinish => match params(value) {
                Ok(params) => self.upload_finish(params).await,
                Err(error) => Err(error),
            },
            Method::UploadsAbort => params(value).and_then(|params| self.upload_abort(params)),
            Method::MessagesSend => match params(value) {
                Ok(params) => self.messages_send(params).await,
                Err(error) => Err(error),
            },
            Method::MessagesLookupSend => self.messages_lookup_send().await,
            Method::MessagesRecall => match params(value) {
                Ok(params) => self.messages_recall(params).await,
                Err(error) => Err(error),
            },
            Method::ReactionsAdd => match params(value) {
                Ok(params) => self.reactions_add(params).await,
                Err(error) => Err(error),
            },
            Method::ReactionsRemove => match params(value) {
                Ok(params) => self.reactions_remove(params).await,
                Err(error) => Err(error),
            },
            // 飞书不能以本人身份编辑、不带原始数据（能力里都没声明），也不认宿主发来的插件端方法。
            Method::MessagesEdit
            | Method::MessagesRetranslate
            | Method::LoginSubmit
            | Method::LoginCancel
            | Method::LoginStep
            | Method::LoginFinished
            | Method::LoginFailed
            | Method::CredentialSave
            | Method::SessionExpired
            | Method::SessionNotice
            | Method::EventsPush
            | Method::Status
            | Method::LogWrite
            | Method::Cancel => Err(errors::method_not_found(method)),
        })
    }
}

/// 握手结果（测试与样例用）。
#[doc(hidden)]
pub fn initialize_result() -> Value {
    declare::result()
}
