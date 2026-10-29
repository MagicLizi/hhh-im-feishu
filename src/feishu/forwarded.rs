//! 合并转发只从外层消息的获取结果读取；内部编号不能脱离外层直接作为资源入口。
use super::*;
use hhh_im_protocol::budget;

impl FeishuClient {
    pub(crate) async fn forwarded_items(
        &self,
        reference: &MessageRef,
    ) -> ImResult<(Vec<Value>, bool)> {
        self.check_conversation(&reference.conversation)?;
        validate_id(&reference.id)?;
        let data = self
            .get(
                &["open-apis", "im", "v1", "messages", &reference.id],
                &[(CARD_CONTENT_PARAM.0.into(), CARD_CONTENT_PARAM.1.into())],
            )
            .await?;
        let items = data
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| ImError::new(ErrorKind::ProtocolError, "平台没有提供转发记录。"))?;
        let root = items
            .iter()
            .find(|item| item["message_id"] == reference.id)
            .filter(|item| {
                item["chat_id"] == reference.conversation.id
                    && item["msg_type"] == "merge_forward"
                    && item["deleted"] != true
            })
            .ok_or_else(|| {
                ImError::new(
                    ErrorKind::PermissionDenied,
                    "平台没有提供这条会话中的转发记录。",
                )
            })?;
        let mut selected = vec![root.clone()];
        selected.extend(
            items
                .iter()
                .filter(|item| item["message_id"] != reference.id)
                .take(budget::FORWARDED_MESSAGES_MAX)
                .cloned(),
        );
        let truncated = items.len() > selected.len();
        Ok((selected, truncated))
    }

    pub(crate) async fn forwarded_resource(
        &self,
        reference: &ResourceRef,
    ) -> ImResult<ResourceRef> {
        let Some(key) = reference.key.strip_prefix("fw/") else {
            return Ok(reference.clone());
        };
        let (source, key) = key
            .split_once('/')
            .ok_or_else(|| ImError::new(ErrorKind::InvalidRequest, "转发资源编号不完整。"))?;
        validate_id(source)?;
        validate_id(key)?;
        let (items, _) = self.forwarded_items(&reference.message).await?;
        if !belongs(&items, &reference.message.id, source) {
            return Err(ImError::new(
                ErrorKind::PermissionDenied,
                "这部分资源不属于当前转发记录。",
            ));
        }
        let mut resolved = reference.clone();
        resolved.message.id = source.to_owned();
        resolved.key = key.to_owned();
        Ok(resolved)
    }
}

fn belongs(items: &[Value], outer: &str, source: &str) -> bool {
    let mut current = source;
    for _ in 0..budget::FORWARDED_DEPTH_MAX {
        let mut matches = items.iter().filter(|item| item["message_id"] == current);
        let Some(item) = matches.next() else {
            return false;
        };
        if matches.next().is_some() {
            return false;
        }
        if item["deleted"] == true {
            return false;
        }
        let Some(parent) = item["upper_message_id"].as_str() else {
            return false;
        };
        if parent == outer {
            return true;
        }
        if parent == current {
            return false;
        }
        current = parent;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forwarded_resources_require_an_unbroken_parent_chain() {
        let items = vec![
            json!({"message_id":"a","upper_message_id":"outer"}),
            json!({"message_id":"b","upper_message_id":"a"}),
            json!({"message_id":"foreign","upper_message_id":"elsewhere"}),
        ];
        assert!(belongs(&items, "outer", "b"));
        assert!(!belongs(&items, "outer", "foreign"));
        assert!(!belongs(&items, "outer", "outer"));
        assert!(!belongs(
            &[json!({"message_id":"a","upper_message_id":"a"})],
            "outer",
            "a"
        ));
    }
}
