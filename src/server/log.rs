//! 日志（protocol-v1 §11.5）：库里的 tracing 日志转成 `log/write` 通知交给宿主，宿主并进 HHH 的日志。
//!
//! 插件自己的日志（`hhh_im_feishu`）从 debug 起都交；别的库（网络库这类）只交 warn 与 error，免得刷屏。
//! 日志里本来就不写凭据与消息正文（适配器的约定），这里另把每条截到 4 KiB。

use std::collections::BTreeMap;
use std::fmt::Write as _;

use hhh_im_protocol::method::{LogLevel, LogWriteParams};
use hhh_im_protocol::rpc::RpcMessage;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt as _};

/// 一条日志最多多少字节（协议 §14）。
const LINE_MAX_BYTES: usize = 4 * 1024;

/// 把 tracing 事件写成 `log/write` 的一层。
struct ToHost {
    out: mpsc::UnboundedSender<RpcMessage>,
}

/// 收一条事件的消息与字段。
#[derive(Default)]
struct Fields {
    message: String,
    fields: BTreeMap<String, Value>,
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let mut text = String::new();
        let _ = write!(text, "{value:?}");
        if field.name() == "message" {
            self.message = text;
        } else {
            self.fields
                .insert(field.name().to_owned(), Value::String(text));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_owned();
        } else {
            self.fields
                .insert(field.name().to_owned(), Value::String(value.to_owned()));
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields.insert(field.name().to_owned(), value.into());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields.insert(field.name().to_owned(), value.into());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields.insert(field.name().to_owned(), value.into());
    }
}

/// 截到 `max` 字节以内（不切开一个字）。
fn truncate(text: &mut String, max: usize) {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
}

impl<S: Subscriber> Layer<S> for ToHost {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let metadata = event.metadata();
        let ours = metadata.target().starts_with("hhh_im_feishu");
        let level = match *metadata.level() {
            Level::ERROR => LogLevel::Error,
            Level::WARN => LogLevel::Warn,
            Level::INFO if ours => LogLevel::Info,
            Level::DEBUG if ours => LogLevel::Debug,
            _ => return,
        };
        let mut fields = Fields::default();
        event.record(&mut fields);
        truncate(&mut fields.message, LINE_MAX_BYTES);
        fields
            .fields
            .insert("target".into(), metadata.target().into());
        let params = LogWriteParams {
            level,
            message: fields.message,
            fields: Some(fields.fields),
        };
        if let Ok(params) = serde_json::to_value(params) {
            let _ = self.out.send(RpcMessage::notification("log/write", params));
        }
    }
}

/// 装上：之后的 tracing 日志都交给宿主。已经装过（测试里起了好几个服务）时什么也不做。
pub(crate) fn install(out: mpsc::UnboundedSender<RpcMessage>) {
    let subscriber = tracing_subscriber::registry().with(ToHost { out });
    let _ = tracing::subscriber::set_global_default(subscriber);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_lines_are_cut_on_a_character_boundary() {
        let mut text = "飞".repeat(2000);
        truncate(&mut text, LINE_MAX_BYTES);
        assert!(text.len() <= LINE_MAX_BYTES);
        assert!(text.chars().all(|c| c == '飞'));
    }

    #[test]
    fn events_become_log_write_notifications() {
        let (out, mut rx) = mpsc::unbounded_channel();
        let subscriber = tracing_subscriber::registry().with(ToHost { out });
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(status = 401, "飞书拒绝了请求");
            tracing::debug!(target: "hyper", "别的库的过程日志不交");
        });
        let message = rx.try_recv().unwrap();
        assert_eq!(message.method.as_deref(), Some("log/write"));
        let params = message.params.unwrap();
        assert_eq!(params["level"], "warn");
        assert_eq!(params["message"], "飞书拒绝了请求");
        assert_eq!(params["fields"]["status"], 401);
        assert!(rx.try_recv().is_err());
    }
}
