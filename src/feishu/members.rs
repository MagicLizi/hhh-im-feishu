//! 群成员列表：@ 选择与显示名字用；平台成员接口不带头像，头像另按编号批量补读。

use super::*;

/// 成员接口单页上限；平台最大 100。
pub const MEMBER_PAGE_MAX: u16 = 100;

impl FeishuClient {
    /// 用本人令牌分页读取群成员；本人必须在群里，平台不返回群里的机器人。
    ///
    /// 结果只有编号与名字，头像为空；需要头像时把编号交给 [`FeishuClient::users`]。
    ///
    /// # Errors
    /// 会话不属于当前连接、页大小越界、缺少群权限或本人不在群里时返回错误。
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example(client: &hhh_im_feishu::FeishuClient, chat: hhh_im_feishu::ConversationRef) -> hhh_im_feishu::ImResult<()> {
    /// let page = client.list_members(&chat, 100, None).await?;
    /// for member in &page.items {
    ///     println!("{} {}", member.reference.id, member.name);
    /// }
    /// # Ok(()) }
    /// ```
    pub async fn list_members(
        &self,
        conversation: &ConversationRef,
        limit: u16,
        cursor: Option<&PageCursor>,
    ) -> ImResult<Page<SearchUser>> {
        self.check_conversation(conversation)?;
        if limit == 0 || limit > MEMBER_PAGE_MAX {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "群成员每页数量应在 1 至 100 之间。",
            ));
        }
        let scope = serde_json::to_string(&("members", conversation, limit))
            .map_err(|_| ImError::new(ErrorKind::InvalidRequest, "无法编码成员查询范围。"))?;
        let mut params = vec![
            ("member_id_type".into(), "open_id".into()),
            ("page_size".into(), limit.to_string()),
        ];
        add_cursor(&mut params, cursor, &scope)?;
        let data = self
            .get(
                &[
                    "open-apis",
                    "im",
                    "v1",
                    "chats",
                    &conversation.id,
                    "members",
                ],
                &params,
            )
            .await?;
        let mut items = Vec::new();
        for raw in data
            .get("items")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let (Some(id), Some(name)) = (
                raw.get("member_id").and_then(Value::as_str),
                raw.get("name").and_then(Value::as_str),
            ) else {
                return Err(ImError::new(
                    ErrorKind::ProtocolError,
                    "飞书群成员缺少编号或名字。",
                ));
            };
            validate_id(id)?;
            items.push(SearchUser {
                reference: UserRef {
                    connection_id: self.account.connection_id.clone(),
                    id: id.to_owned(),
                },
                name: name.to_owned(),
                avatar_url: None,
            });
        }
        let next = page_cursor(&data, scope)?;
        Ok(Page { items, next })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_page_limit_is_the_platform_maximum() {
        // 平台成员接口 page_size 最大 100；超出时在本地就拒绝，不发请求。
        assert_eq!(MEMBER_PAGE_MAX, 100);
    }
}
