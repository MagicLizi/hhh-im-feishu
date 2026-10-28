//! 开放平台转发快照在协议边界组装，普通消息继续由既有翻译器处理。
use super::{CONNECTION, Server};
use crate::{ImContent, ImMessage};
use hhh_im_protocol::{budget, types as wire};
use serde_json::{Value, json};

pub(super) fn is_forwarded(message: &ImMessage) -> bool {
    message
        .content
        .iter()
        .any(|part| matches!(part,ImContent::Unsupported{kind,..} if kind=="merge_forward"))
}

/// 只选择升级前的合并转发占位，由宿主按现有权限重读外层消息。
pub(super) fn maintain(params: Value) -> Result<Value, hhh_im_protocol::RpcError> {
    if params["mode"] != "reread" || params["rule"] != "forwarded-record-v1" {
        return Err(hhh_im_protocol::RpcError::from_kind(
            hhh_im_protocol::ErrorKind::Unsupported,
            "没有这条消息维护规则。",
        ));
    }
    let messages = params["messages"]
        .as_array()
        .ok_or_else(|| super::errors::invalid_params("缺少待核对的消息。"))?;
    if messages.len() > budget::MAINTENANCE_BATCH_MAX {
        return Err(hhh_im_protocol::RpcError::from_kind(
            hhh_im_protocol::ErrorKind::LimitExceeded,
            "待核对的消息太多。",
        ));
    }
    let reread = messages
        .iter()
        .filter(|message| {
            message["content"].as_array().is_some_and(|parts| {
                parts
                    .iter()
                    .any(|part| part["type"] == "unsupported" && part["kind"] == "merge_forward")
            })
        })
        .map(|message| json!({"conversationId":message["conversationId"],"id":message["id"]}))
        .collect::<Vec<_>>();
    Ok(json!({"reread":reread}))
}

fn record(parent: &str, items: &[Value], depth: usize, used: &mut usize) -> wire::ForwardedRecord {
    let mut messages = Vec::new();
    let mut incomplete = depth > budget::FORWARDED_DEPTH_MAX;
    if !incomplete {
        for item in items
            .iter()
            .filter(|item| item["upper_message_id"] == parent)
            .take(budget::FORWARDED_MESSAGES_PER_RECORD_MAX)
        {
            if *used >= budget::FORWARDED_MESSAGES_MAX {
                incomplete = true;
                break;
            }
            *used += 1;
            let id = item["message_id"].as_str().filter(|s| !s.is_empty());
            let translated = crate::feishu::translate::message(CONNECTION, item)
                .ok()
                .map(|message| super::wire::message(&message));
            let (mut content, mut partial) = match translated.as_ref() {
                Some(message) => (message.content.clone(), message.incomplete),
                None => (
                    vec![wire::Block::Unsupported {
                        kind: "forwarded-message".into(),
                        label: Some("部分内容无法读取".into()),
                    }],
                    true,
                ),
            };
            if item["deleted"] == true {
                partial = true;
                content = vec![wire::Block::Unsupported {
                    kind: "recalled".into(),
                    label: Some("原消息已被撤回".into()),
                }];
            } else if item["msg_type"] == "merge_forward" {
                let nested = id
                    .map(|id| record(id, items, depth + 1, used))
                    .unwrap_or_else(|| wire::ForwardedRecord {
                        incomplete: true,
                        ..Default::default()
                    });
                partial |= nested.incomplete;
                content = vec![wire::Block::Forwarded {
                    record: Box::new(nested),
                }];
            } else if let Some(id) = id {
                let mut value = json!(content);
                prefix(&mut value, id);
                content = serde_json::from_value(value).expect("只更改资源键，内容类型保持不变");
            }
            let sender = translated
                .as_ref()
                .map(|m| wire::ForwardedSender {
                    id: (!m.sender.id.is_empty()).then(|| m.sender.id.clone()),
                    id_type: m.sender.id_type.clone(),
                    kind: Some(m.sender.kind),
                    name: m.sender.name.clone(),
                    avatar: None,
                })
                .unwrap_or_default();
            incomplete |= partial;
            messages.push(wire::ForwardedMessage {
                id: id.map(str::to_owned),
                sender,
                created_at_ms: translated.as_ref().map(|m| m.created_at_ms),
                content,
                incomplete: partial,
            });
        }
        incomplete |= messages.is_empty()
            || items
                .iter()
                .filter(|item| item["upper_message_id"] == parent)
                .count()
                > messages.len();
    }
    wire::ForwardedRecord {
        title: "合并转发的聊天记录".into(),
        messages,
        total_count: None,
        incomplete,
    }
}

fn prefix(value: &mut Value, message: &str) {
    match value {
        Value::Array(items) => {
            for item in items {
                prefix(item, message);
            }
        }
        Value::Object(object) => {
            if object
                .get("kind")
                .and_then(Value::as_str)
                .is_some_and(|kind| matches!(kind, "image" | "file" | "sticker"))
            {
                if let Some(Value::String(key)) = object.get_mut("key") {
                    *key = format!("fw/{message}/{key}");
                }
                if let Some(Value::String(key)) = object
                    .get_mut("cover")
                    .and_then(|cover| cover.get_mut("key"))
                {
                    *key = format!("fw/{message}/{key}");
                }
            }
            for child in object.values_mut() {
                prefix(child, message);
            }
        }
        _ => {}
    }
}

impl Server {
    pub(crate) async fn forwarded_message(&self, message: &ImMessage) -> wire::Message {
        let mut result = super::wire::message(message);
        if !is_forwarded(message) {
            return result;
        }
        let reference = message.reference.clone();
        let record = match self
            .with_client(|client| {
                let reference = reference.clone();
                async move { client.forwarded_items(&reference).await }
            })
            .await
        {
            Ok((items, truncated)) => {
                let mut record = record(&message.reference.id, &items, 1, &mut 0);
                record.incomplete |= truncated;
                record
            }
            Err(_) => {
                tracing::warn!("开放平台未提供可读取的合并转发快照");
                wire::ForwardedRecord {
                    title: "合并转发的聊天记录".into(),
                    incomplete: true,
                    ..Default::default()
                }
            }
        };
        let mut content = json!([wire::Block::Forwarded {
            record: Box::new(record)
        }]);
        result.incomplete |= hhh_im_protocol::forwarded::sanitize_content(&mut content);
        result.content = serde_json::from_value(content).expect("核对后的协议内容可以读取");
        // 原占位的不完整标记由本次快照的实际结果替换。
        result.incomplete = result
            .content
            .iter()
            .any(|part| matches!(part,wire::Block::Forwarded{record} if record.incomplete));
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forwarded_audio_keeps_voice_metadata_and_scopes_its_resource() {
        let item = json!({
            "message_id":"om_voice", "upper_message_id":"outer", "chat_id":"oc_demo",
            "msg_type":"audio", "create_time":"1700000000000",
            "sender":{"id":"ou_old","id_type":"open_id","sender_type":"user"},
            "body":{"content":json!({"file_key":"file_fixture_audio","duration":1000}).to_string()}
        });
        let record = record("outer", &[item], 1, &mut 0);
        assert!(!record.incomplete);
        let wire::Block::Resource { resource, .. } = &record.messages[0].content[0] else {
            panic!("应保留转发记录中的语音资源")
        };
        assert!(resource.voice);
        assert_eq!(resource.duration_ms, Some(1000));
        assert_eq!(resource.key, "fw/om_voice/file_fixture_audio");
    }

    #[test]
    fn maintenance_only_rereads_legacy_forwarded_and_recalled_snapshots_stay_hidden() {
        let reply=maintain(json!({"mode":"reread","rule":"forwarded-record-v1","messages":[
            {"id":"outer","conversationId":"chat","content":[{"type":"unsupported","kind":"merge_forward"}]},
            {"id":"normal","conversationId":"chat","content":[{"type":"text","text":"保留"}]}
        ]})).unwrap();
        assert_eq!(
            reply["reread"],
            json!([{"conversationId":"chat","id":"outer"}])
        );
        let hidden = record(
            "outer",
            &[
                json!({"message_id":"recalled","upper_message_id":"outer","msg_type":"merge_forward","deleted":true}),
            ],
            1,
            &mut 0,
        );
        assert!(hidden.incomplete);
        assert!(
            matches!(&hidden.messages[0].content[0],wire::Block::Unsupported{kind,..} if kind=="recalled")
        );
    }
    #[test]
    fn forwarded_snapshot_groups_by_parent_in_platform_order() {
        let text = |id: &str, parent: &str, text: &str| {
            json!({"message_id":id,"upper_message_id":parent,"chat_id":"oc_demo","msg_type":"text","create_time":"1700000000000",
            "sender":{"id":"ou_old","id_type":"open_id","sender_type":"user"},"body":{"content":json!({"text":text}).to_string()}})
        };
        let items = vec![
            text("om_second", "outer", "先出现"),
            text("om_first", "outer", "后出现"),
            text("om_foreign", "other", "不在这条记录"),
        ];
        let record = record("outer", &items, 1, &mut 0);
        assert_eq!(
            record
                .messages
                .iter()
                .map(|m| m.id.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["om_second", "om_first"]
        );
        assert_eq!(record.messages[0].created_at_ms, Some(1700000000000));
        assert!(!record.incomplete);
        assert_eq!(record.total_count, None);
    }
}
