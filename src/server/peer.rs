//! 和宿主说话的那一头（protocol-v1 §3）：所有发往标准输出的消息都排进一条队列、由一个写手按顺序写，
//! 一行一条、不会两条搅在一起；插件发给宿主的请求（`credential/save`）按编号等回复。

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use hhh_im_protocol::rpc::{ErrorKind as WireKind, RpcError, RpcId, RpcMessage};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

/// 等宿主回一个请求最多等多久（宿主写系统凭据库通常不到一秒）。
const REPLY_WAIT: Duration = Duration::from_secs(30);

/// 宿主那一头：写消息的队列与在等回复的请求。
pub(crate) struct Peer {
    out: mpsc::UnboundedSender<RpcMessage>,
    /// 插件发出的请求的编号：字符串 `p1`、`p2`…，不会和宿主的数字编号撞号（rpc 模块的约定）。
    next: AtomicU64,
    waiting: Mutex<HashMap<String, oneshot::Sender<Result<Value, RpcError>>>>,
}

impl Peer {
    /// 一个新的宿主那一头；写手从返回的接收端取消息写出去。
    pub(crate) fn new() -> (Self, mpsc::UnboundedReceiver<RpcMessage>) {
        let (out, rx) = mpsc::unbounded_channel();
        (
            Self {
                out,
                next: AtomicU64::new(1),
                waiting: Mutex::new(HashMap::new()),
            },
            rx,
        )
    }

    /// 写消息的队列（日志层也往这里写 `log/write`）。
    pub(crate) fn sender(&self) -> mpsc::UnboundedSender<RpcMessage> {
        self.out.clone()
    }

    fn send(&self, message: RpcMessage) {
        // 写手已经停了（标准输出关了）：宿主那头已经不在，丢掉就是。
        let _ = self.out.send(message);
    }

    /// 回宿主的一个请求。
    pub(crate) fn respond(&self, id: RpcId, result: Result<Value, RpcError>) {
        self.send(match result {
            Ok(value) => RpcMessage::ok(id, value),
            Err(error) => RpcMessage::err(id, error),
        });
    }

    /// 发一个通知（不等回复）。
    pub(crate) fn notify(&self, method: &str, params: impl Serialize) {
        match serde_json::to_value(params) {
            Ok(params) => self.send(RpcMessage::notification(method, params)),
            Err(error) => tracing::error!(method, %error, "通知的参数写不出来，没发"),
        }
    }

    /// 发一个请求给宿主，等它回复。宿主回了错误、或连接断了、等太久时返回错误。
    pub(crate) async fn request(
        &self,
        method: &str,
        params: impl Serialize,
    ) -> Result<Value, RpcError> {
        let params = serde_json::to_value(params).map_err(|error| {
            RpcError::from_kind(
                WireKind::ProtocolError,
                format!("发给宿主的参数写不出来：{error}"),
            )
        })?;
        let id = format!("p{}", self.next.fetch_add(1, Ordering::SeqCst));
        let (tx, rx) = oneshot::channel();
        self.waiting
            .lock()
            .expect("在等的请求表锁中毒")
            .insert(id.clone(), tx);
        self.send(RpcMessage::request(RpcId::Text(id.clone()), method, params));
        let reply = tokio::time::timeout(REPLY_WAIT, rx).await;
        self.waiting.lock().expect("在等的请求表锁中毒").remove(&id);
        match reply {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(RpcError::from_kind(
                WireKind::Disconnected,
                "和宿主的连接已经断了。",
            )),
            Err(_) => Err(RpcError::from_kind(
                WireKind::Disconnected,
                format!("宿主没有在 {} 秒内回复 {method}。", REPLY_WAIT.as_secs()),
            )),
        }
    }

    /// 宿主回了插件的某个请求：交给在等它的那一个；没人在等（等超时了）就丢掉。
    pub(crate) fn answer(&self, message: RpcMessage) {
        let Some(id) = message.id.as_ref().map(ToString::to_string) else {
            return;
        };
        let waiting = self.waiting.lock().expect("在等的请求表锁中毒").remove(&id);
        let Some(waiting) = waiting else {
            tracing::debug!(%id, "宿主回了一个没人在等的请求，丢掉");
            return;
        };
        let result = match (message.result, message.error) {
            (_, Some(error)) => Err(error),
            (Some(result), None) => Ok(result),
            (None, None) => Ok(Value::Null),
        };
        let _ = waiting.send(result);
    }
}
