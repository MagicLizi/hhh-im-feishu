//! 本人逐条阅读事实；查询没有写副作用，不能由一条 true 推导连续水位。
use super::*;
use hhh_im_protocol::budget::{READ_CURSOR_MAX_BYTES, READ_PEOPLE_MAX};
use hhh_im_protocol::reading::{
    MessageReceipt, ReadState, ReadSyncParams, ReceiptPeopleParams, ReceiptPeopleResult,
    ReceiptsParams, ReceiptsResult, valid_read_ids, valid_read_scope,
};
use std::collections::HashSet;

impl FeishuClient {
    /// 本人消息的已读人数。平台只给已读人员，不提供可用分母与未读名单。
    /// 完整首屏才可数人数；超过首屏时保持未知，不把部分名单长度当总人数。
    ///
    /// # Errors
    /// 查询范围错误、授权失效或平台限流时返回错误。
    pub async fn reading_receipts(&self, params: &ReceiptsParams) -> ImResult<ReceiptsResult> {
        if !valid_read_scope(&params.scope) || !valid_read_ids(&params.message_ids) {
            return Err(invalid_reading("回执查询范围不正确。"));
        }
        let mut items = Vec::new();
        for id in &params.message_ids {
            let query = ReceiptPeopleParams {
                scope: params.scope.clone(),
                message_id: id.clone(),
                read: true,
                cursor: None,
                limit: READ_PEOPLE_MAX as u16,
            };
            match self.reading_people(&query).await {
                Ok(page) => items.push(MessageReceipt {
                    message_id: id.clone(),
                    read_count: page.complete.then_some(page.people.len() as u32),
                    total_count: None,
                    readers: true,
                    unreaders: false,
                }),
                Err(error)
                    if matches!(error.kind, ErrorKind::AuthExpired | ErrorKind::RateLimited) =>
                {
                    return Err(error);
                }
                Err(error) => {
                    tracing::warn!(kind=?error.kind, code=?error.platform_code, "该消息回执不可用，保持未知")
                }
            }
        }
        Ok(ReceiptsResult { items })
    }

    /// 按平台的真实分页读已读名单；仅用户点击后由宿主请求后续页。
    ///
    /// # Errors
    /// 范围、消息归属、权限、分页或平台响应不可靠时返回错误。
    pub async fn reading_people(
        &self,
        params: &ReceiptPeopleParams,
    ) -> ImResult<ReceiptPeopleResult> {
        if !valid_read_scope(&params.scope)
            || params
                .scope
                .thread_root
                .as_ref()
                .is_some_and(|root| !root.starts_with("om_"))
            || !params.message_id.starts_with("om_")
            || !valid_read_ids(std::slice::from_ref(&params.message_id))
            || params.limit == 0
            || usize::from(params.limit) > READ_PEOPLE_MAX
            || params
                .cursor
                .as_ref()
                .is_some_and(|c| c.is_empty() || c.len() > READ_CURSOR_MAX_BYTES)
        {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "阅读名单请求不正确。",
            ));
        }
        if !params.read {
            return Err(ImError::new(
                ErrorKind::Unsupported,
                "飞书公开接口只提供已读名单。",
            ));
        }
        let reference = MessageRef {
            conversation: ConversationRef::new(
                &self.account.connection_id,
                &params.scope.conversation_id,
            ),
            id: params.message_id.clone(),
        };
        // read_users 响应不含会话与发送者，先沿用消息查询核验归属。
        let message = self.get_message(&reference).await?;
        let reply_root = message
            .root_message
            .as_ref()
            .filter(|root| {
                message.in_thread != Some(false)
                    && message.thread_id.is_some()
                    && root.id != message.reference.id
            })
            .map(|root| root.id.as_str());
        let in_scope = match params.scope.thread_root.as_deref() {
            None => reply_root.is_none() && message.in_thread != Some(true),
            Some(root) => {
                reply_root == Some(root)
                    || (message.reference.id == root
                        && message.thread_id.is_some()
                        && message.in_thread != Some(true))
            }
        };
        if message.sender.id != self.account.user.id || message.deleted || !in_scope {
            return Err(ImError::new(
                ErrorKind::Unsupported,
                "只能查询当前本人发送且属于指定范围的消息回执。",
            ));
        }
        let mut query = vec![
            ("user_id_type".into(), "open_id".into()),
            ("page_size".into(), params.limit.to_string()),
        ];
        if let Some(cursor) = &params.cursor {
            query.push(("page_token".into(), cursor.clone()));
        }
        let data = self
            .get(
                &[
                    "open-apis",
                    "im",
                    "v1",
                    "messages",
                    &params.message_id,
                    "read_users",
                ],
                &query,
            )
            .await?;
        reader_page(&data, params, &self.account.user.id)
    }

    /// 查询宿主已核验属于本范围的消息，仅保留平台明确给出的 true。
    ///
    /// # Errors
    /// 输入、权限、平台响应或连接异常时返回错误，不将失败当作未读。
    pub async fn read_state(&self, params: &ReadSyncParams) -> ImResult<ReadState> {
        if !valid_read_scope(&params.scope)
            || !valid_read_ids(&params.message_ids)
            || params.message_ids.iter().any(|id| !id.starts_with("om_"))
        {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "阅读查询的消息编号或范围不正确。",
            ));
        }
        self.check_conversation(&ConversationRef::new(
            &self.account.connection_id,
            &params.scope.conversation_id,
        ))?;
        let mut read_message_ids = Vec::new();
        for ids in params.message_ids.chunks(50) {
            let data = self
                .query_data(
                    self.http
                        .post(self.url(&["open-apis", "im", "v1", "messages", "read_status"])?)
                        .json(&json!({"message_ids":ids}))
                        .timeout(crate::im::client::QUERY_TIMEOUT),
                )
                .await?;
            let items = data["items"]
                .as_array()
                .ok_or_else(|| invalid_reading("响应缺少逐条阅读结果。"))?;
            let mut seen = HashSet::new();
            for item in items {
                let id = item["message_id"]
                    .as_str()
                    .ok_or_else(|| invalid_reading("阅读结果缺少消息编号。"))?;
                if !ids.iter().any(|requested| requested == id) || !seen.insert(id) {
                    return Err(invalid_reading("阅读结果包含未查询或重复的消息。"));
                }
                match item["is_read"].as_bool() {
                    Some(true) => read_message_ids.push(id.to_owned()),
                    Some(false) => {}
                    None => return Err(invalid_reading("阅读结果没有明确的已读事实。")),
                }
            }
            // invalid_message_ids 是不可确定项，不填 false，也不越过它们清未读。
            if let Some(invalid) = data.get("invalid_message_ids") {
                let invalid = invalid
                    .as_array()
                    .ok_or_else(|| invalid_reading("无效消息列表格式不正确。"))?;
                for id in invalid {
                    let id = id
                        .as_str()
                        .ok_or_else(|| invalid_reading("无效消息编号格式不正确。"))?;
                    if !ids.iter().any(|requested| requested == id) || seen.contains(id) {
                        return Err(invalid_reading("阅读结果与无效消息列表冲突。"));
                    }
                }
            }
        }
        Ok(ReadState {
            scope: params.scope.clone(),
            through: None,
            read_message_ids,
        })
    }
}

fn invalid_reading(message: &str) -> ImError {
    ImError::new(ErrorKind::ProtocolError, message)
}

fn reader_page(
    data: &Value,
    query: &ReceiptPeopleParams,
    me: &str,
) -> ImResult<ReceiptPeopleResult> {
    let items = data["items"]
        .as_array()
        .ok_or_else(|| invalid_reading("已读名单缺少 items。"))?;
    let more = data["has_more"]
        .as_bool()
        .ok_or_else(|| invalid_reading("已读名单缺少分页完整性标志。"))?;
    if items.len() > usize::from(query.limit) {
        return Err(invalid_reading("已读名单超过请求页大小。"));
    }
    let next_cursor = if more {
        Some(
            data["page_token"]
                .as_str()
                .filter(|v| {
                    !v.is_empty()
                        && v.len() <= READ_CURSOR_MAX_BYTES
                        && query.cursor.as_deref() != Some(*v)
                })
                .ok_or_else(|| invalid_reading("已读名单没有可靠的下一页。"))?
                .to_owned(),
        )
    } else {
        None
    };
    let mut people = Vec::new();
    let mut seen = HashSet::new();
    for item in items {
        let id = item["user_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| invalid_reading("已读名单缺少用户编号。"))?;
        if item["user_id_type"].as_str() != Some("open_id") || !seen.insert(id) {
            return Err(invalid_reading("已读名单身份类型不正确或重复。"));
        }
        if id != me {
            people.push(hhh_im_protocol::Person {
                id: id.to_owned(),
                id_type: Some("open_id".into()),
                kind: hhh_im_protocol::types::PersonKind::User,
                name: None,
                avatar: None,
            });
        }
    }
    Ok(ReceiptPeopleResult {
        people,
        next_cursor,
        complete: !more,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn query() -> ReceiptPeopleParams {
        ReceiptPeopleParams {
            scope: hhh_im_protocol::reading::ReadScope {
                conversation_id: "oc_test".into(),
                thread_root: None,
            },
            message_id: "om_test".into(),
            read: true,
            cursor: None,
            limit: 100,
        }
    }
    #[test]
    fn sampled_reader_pages_require_explicit_completion_and_never_invent_unread_people() {
        // 2026-10-28 当前用户令牌对两条本人消息的只读样本；编号和时间已替换。
        let raw = json!({"has_more":false,"items":[{"tenant_key":"test","timestamp":"1790000000","user_id":"ou_reader","user_id_type":"open_id"}]});
        let page = reader_page(&raw, &query(), "ou_me").unwrap();
        assert!(page.complete);
        assert_eq!(page.people.len(), 1);
        assert!(page.next_cursor.is_none());
        assert!(reader_page(&json!({"items":[]}), &query(), "ou_me").is_err());
        assert!(reader_page(&json!({"has_more":true,"items":[]}), &query(), "ou_me").is_err());
    }
    #[test]
    fn partial_reader_pages_keep_cursor_exclude_sender_and_reject_repeated_cursors() {
        let raw = json!({"has_more":true,"page_token":"next","items":[{"user_id":"ou_me","user_id_type":"open_id"}]});
        let mut query = query();
        let page = reader_page(&raw, &query, "ou_me").unwrap();
        assert!(!page.complete);
        assert!(page.people.is_empty());
        assert_eq!(page.next_cursor.as_deref(), Some("next"));
        query.cursor = Some("next".into());
        assert!(reader_page(&raw, &query, "ou_me").is_err());
    }
}
