//! 应用事件的认证和消息翻译；入站 HTTP 服务及耐久确认归宿主。

use crate::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 单个回调正文的读取预算。
pub const EVENT_MAX_BYTES: usize = 1024 * 1024;

/// 已通过应用验证令牌校验的回调结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WebhookDelivery {
    /// 应用回调地址校验。
    Challenge {
        /// 需要原样回传的 challenge。
        challenge: String,
    },
    /// 应用可见的新消息；不代表已接入本人全部消息。
    Message {
        /// 平台事件编号，仅供追踪；消息去重应使用 message_id。
        event_id: String,
        /// 统一消息。
        message: Box<ImMessage>,
    },
    /// 本封装未处理的事件类型。
    Unhandled {
        /// 平台事件类型。
        event_type: String,
    },
}

/// 飞书明文应用回调的校验器，不持有用户访问令牌。
pub struct FeishuEvents {
    account: Account,
    app_id: String,
    verification_token: String,
}
impl std::fmt::Debug for FeishuEvents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeishuEvents")
            .field("app_id", &self.app_id)
            .finish_non_exhaustive()
    }
}
impl FeishuEvents {
    /// 绑定账号范围、应用与事件验证令牌。
    pub fn new(account: Account, app_id: String, verification_token: String) -> ImResult<Self> {
        if app_id.is_empty() || verification_token.is_empty() {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "事件接收需要应用 ID 和 Verification Token。",
            ));
        }
        Ok(Self {
            account,
            app_id,
            verification_token,
        })
    }

    /// 校验并翻译一次回调；宿主落盘成功后才能向飞书应答成功。
    pub fn decode(&self, bytes: &[u8]) -> ImResult<WebhookDelivery> {
        if bytes.len() > EVENT_MAX_BYTES {
            return Err(ImError::new(
                ErrorKind::LimitExceeded,
                "飞书事件超过读取预算。",
            ));
        }
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|_| ImError::new(ErrorKind::ProtocolError, "飞书事件不是有效 JSON。"))?;
        if value.get("encrypt").is_some() {
            return Err(ImError::new(
                ErrorKind::Unsupported,
                "当前入口仅处理明文验证回调，尚未配置加密事件解密；已拒绝处理。",
            ));
        }
        let token = value
            .pointer("/header/token")
            .or_else(|| value.get("token"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !equal_secret(token, &self.verification_token) {
            return Err(ImError::new(
                ErrorKind::PermissionDenied,
                "事件验证令牌不匹配。",
            ));
        }
        if value.get("type").and_then(Value::as_str) == Some("url_verification") {
            return Ok(WebhookDelivery::Challenge {
                challenge: super::required(&value, "challenge")?,
            });
        }
        if value.get("schema").and_then(Value::as_str) != Some("2.0")
            || value.pointer("/header/app_id").and_then(Value::as_str) != Some(&self.app_id)
        {
            return Err(ImError::new(
                ErrorKind::PermissionDenied,
                "事件模式或应用身份不匹配。",
            ));
        }
        if self.account.tenant_id.as_deref().is_some_and(|tenant| {
            value.pointer("/header/tenant_key").and_then(Value::as_str) != Some(tenant)
        }) {
            return Err(ImError::new(
                ErrorKind::PermissionDenied,
                "事件属于其它租户。",
            ));
        }
        let kind = value
            .pointer("/header/event_type")
            .and_then(Value::as_str)
            .ok_or_else(|| ImError::new(ErrorKind::ProtocolError, "事件缺少类型。"))?;
        if kind != "im.message.receive_v1" {
            return Ok(WebhookDelivery::Unhandled {
                event_type: kind.into(),
            });
        }
        let mut raw = value
            .pointer("/event/message")
            .cloned()
            .ok_or_else(|| ImError::new(ErrorKind::ProtocolError, "消息事件缺少消息。"))?;
        if !raw.is_object() {
            return Err(ImError::new(
                ErrorKind::ProtocolError,
                "事件中的 message 必须为对象。",
            ));
        }
        let sender = value
            .pointer("/event/sender")
            .ok_or_else(|| ImError::new(ErrorKind::ProtocolError, "消息事件缺少发送者。"))?;
        raw["sender"] = serde_json::json!({"id": sender.pointer("/sender_id/open_id").and_then(Value::as_str), "id_type":"open_id","sender_type":sender.get("sender_type").and_then(Value::as_str)});
        raw["msg_type"] = raw.get("message_type").cloned().unwrap_or(Value::Null);
        raw["body"] = serde_json::json!({"content":raw.get("content")});
        Ok(WebhookDelivery::Message {
            event_id: value
                .pointer("/header/event_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            message: Box::new(super::translate::message(
                &self.account.connection_id,
                &raw,
            )?),
        })
    }
}

fn equal_secret(a: &str, b: &str) -> bool {
    // 固定遍历长度，避免按第一个不匹配字节提前返回。
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= usize::from(
            a.as_bytes().get(i).copied().unwrap_or(0) ^ b.as_bytes().get(i).copied().unwrap_or(0),
        );
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decoder() -> FeishuEvents {
        FeishuEvents::new(
            Account {
                connection_id: "test".into(),
                platform: "feishu".into(),
                user: ActorRef {
                    id: "ou_me".into(),
                    id_type: "open_id".into(),
                    kind: "user".into(),
                    name: None,
                },
                tenant_id: Some("tenant".into()),
                avatar_url: None,
            },
            "app".into(),
            "verify_secret".into(),
        )
        .unwrap()
    }
    fn event() -> Value {
        let mut raw: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/feishu/response_text.json"
        ))
        .unwrap();
        raw["message_type"] = raw["msg_type"].clone();
        raw["content"] = raw["body"]["content"].clone();
        raw["mentions"][0]["id"] = json!({"open_id":"ou_me", "union_id":"unused"});
        json!({"schema":"2.0","header":{"token":"verify_secret","app_id":"app","tenant_key":"tenant","event_type":"im.message.receive_v1","event_id":"event_1"},"event":{"sender":{"sender_id":{"open_id":"ou_sender"},"sender_type":"user"},"message":raw}})
    }
    fn decode(value: &Value) -> ImResult<WebhookDelivery> {
        decoder().decode(&serde_json::to_vec(value).unwrap())
    }

    #[test]
    fn webhook_and_history_translate_to_identical_messages() {
        let result = decode(&event()).unwrap();
        let WebhookDelivery::Message { event_id, message } = result else {
            panic!("应为消息");
        };
        assert_eq!(event_id, "event_1");
        let expected: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/feishu/response_text.expected.json"
        ))
        .unwrap();
        assert_eq!(serde_json::to_value(message).unwrap(), expected);
        let mut duplicate = event();
        duplicate["header"]["event_id"] = "event_2".into();
        let WebhookDelivery::Message { message, .. } = decode(&duplicate).unwrap() else {
            panic!("应为消息");
        };
        assert_eq!(
            message.reference.id, "om_reply",
            "重复投递的去重键是消息 ID"
        );
    }

    #[test]
    fn callbacks_check_token_app_tenant_schema_and_size() {
        for key in ["token", "app_id", "tenant_key"] {
            let mut raw = event();
            raw["header"][key] = "wrong".into();
            assert_eq!(decode(&raw).unwrap_err().kind, ErrorKind::PermissionDenied);
        }
        let mut raw = event();
        raw["schema"] = "1.0".into();
        assert!(decode(&raw).is_err());
        assert_eq!(
            decoder()
                .decode(&vec![b' '; EVENT_MAX_BYTES + 1])
                .unwrap_err()
                .kind,
            ErrorKind::LimitExceeded
        );
        assert!(!format!("{:?}", decoder()).contains("verify_secret"));
    }

    #[test]
    fn challenge_unknown_encryption_and_malformed_message_are_explicit() {
        assert!(
            matches!(decode(&json!({"type":"url_verification","token":"verify_secret","challenge":"abc"})).unwrap(), WebhookDelivery::Challenge { challenge } if challenge == "abc")
        );
        assert_eq!(
            decode(&json!({"encrypt":"opaque"})).unwrap_err().kind,
            ErrorKind::Unsupported
        );
        let mut raw = event();
        raw["header"]["event_type"] = "im.future".into();
        assert!(matches!(
            decode(&raw).unwrap(),
            WebhookDelivery::Unhandled { .. }
        ));
        for message in [Value::Null, json!([]), json!(1), json!("bad")] {
            let mut raw = event();
            raw["event"]["message"] = message;
            assert_eq!(decode(&raw).unwrap_err().kind, ErrorKind::ProtocolError);
        }
    }
}
