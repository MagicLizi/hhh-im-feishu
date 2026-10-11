//! 表情列表仅是当前状态；差分和监听生命周期由宿主负责。加、删表情以本人令牌调用平台，
//! 结果以平台为准，不自动重试。

use super::*;

impl FeishuClient {
    /// 用本人令牌读取消息表情，先核验目标消息的会话归属。
    ///
    /// # Errors
    /// 账号、分页、权限、会话归属或响应无效时返回错误。
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example(client: &hhh_im_feishu::FeishuClient, message: hhh_im_feishu::MessageRef) -> hhh_im_feishu::ImResult<()> {
    /// let page = client.list_reactions(&hhh_im_feishu::ReactionQuery::new(message)).await?;
    /// for reaction in page.items {
    ///     println!("{} {} {}", reaction.message.id, reaction.operator.id, reaction.emoji_type);
    /// }
    /// # Ok(()) }
    /// ```
    pub async fn list_reactions(&self, query: &ReactionQuery) -> ImResult<Page<MessageReaction>> {
        self.check_conversation(&query.message.conversation)?;
        validate_id(&query.message.id)?;
        if query.limit == 0 || query.limit > REACTION_PAGE_MAX {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "表情每页数量应在 1 至 50 之间。",
            ));
        }
        let scope = serde_json::to_string(&("reactions", &query.message, query.limit))
            .map_err(|_| ImError::new(ErrorKind::InvalidRequest, "无法编码表情查询范围。"))?;
        let mut params = vec![
            ("user_id_type".into(), "open_id".into()),
            ("page_size".into(), query.limit.to_string()),
        ];
        add_cursor(&mut params, query.cursor.as_ref(), &scope)?;
        if self.grant.scopes().is_some_and(|scopes| {
            !scopes
                .iter()
                .any(|s| s == "im:message.reactions:read" || s == "im:message:readonly")
        }) {
            return Err(reaction_permission_error(ImError::new(
                ErrorKind::PermissionDenied,
                "缺少表情读取权限。",
            )));
        }
        // 表情响应没有 chat_id，不能直接把调用方声称的会话贴在结果上。
        self.get_message(&query.message).await?;
        let data = self
            .get(
                &[
                    "open-apis",
                    "im",
                    "v1",
                    "messages",
                    &query.message.id,
                    "reactions",
                ],
                &params,
            )
            .await
            .map_err(reaction_permission_error)?;
        let items = data
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid_reaction("表情响应缺少 items。"))?;
        if items.len() > usize::from(query.limit)
            || data.get("has_more").and_then(Value::as_bool).is_none()
        {
            return Err(invalid_reaction("表情响应超过页大小或缺少分页完整性标志。"));
        }
        let mut ids = std::collections::HashSet::new();
        let items = items
            .iter()
            .map(|raw| {
                let reaction = reaction_of(raw, &query.message)?;
                if !ids.insert(reaction.id.clone()) {
                    return Err(invalid_reaction("表情分页出现重复编号，请稍后重新读取。"));
                }
                Ok(reaction)
            })
            .collect::<ImResult<Vec<_>>>()?;
        Ok(Page {
            items,
            next: page_cursor(&data, scope)?,
        })
    }

    /// 以本人令牌给消息点一个表情，返回平台记下的这条表情（删除时要用它的编号）。
    ///
    /// # Errors
    /// 消息不属于当前连接、表情类型或编号格式不对、缺少权限，或平台拒绝（不在会话、已撤回、
    /// 这类消息不能点表情等）时返回错误；不自动重试。
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example(client: &hhh_im_feishu::FeishuClient, message: hhh_im_feishu::MessageRef) -> hhh_im_feishu::ImResult<()> {
    /// let reaction = client.add_reaction(&message, "THUMBSUP").await?;
    /// client.remove_reaction(&message, &reaction.id).await?;
    /// # Ok(()) }
    /// ```
    pub async fn add_reaction(
        &self,
        message: &MessageRef,
        emoji_type: &str,
    ) -> ImResult<MessageReaction> {
        self.check_conversation(&message.conversation)?;
        validate_id(&message.id)?;
        validate_emoji_type(emoji_type)?;
        self.ensure_reaction_write()?;
        // 表情响应没有 chat_id：先核对消息属于这个会话。
        self.get_message(message).await?;
        let url = self.url(&[
            "open-apis",
            "im",
            "v1",
            "messages",
            &message.id,
            "reactions",
        ])?;
        let data = self
            .query_data(
                self.http
                    .post(url)
                    .json(&json!({ "reaction_type": { "emoji_type": emoji_type } })),
            )
            .await
            .map_err(reaction_write_error)?;
        let reaction = reaction_of(&data, message)?;
        if reaction.emoji_type != emoji_type {
            return Err(invalid_reaction("平台返回的表情与请求的不一致。"));
        }
        tracing::info!("飞书已添加本人表情");
        Ok(reaction)
    }

    /// 以本人令牌删掉自己点的一个表情；`reaction_id` 来自添加或读取时平台给的编号。
    ///
    /// # Errors
    /// 编号格式不对、缺少权限或平台拒绝时返回错误；不自动重试。
    pub async fn remove_reaction(&self, message: &MessageRef, reaction_id: &str) -> ImResult<()> {
        self.check_conversation(&message.conversation)?;
        validate_id(&message.id)?;
        validate_reaction_id(reaction_id)?;
        self.ensure_reaction_write()?;
        self.get_message(message).await?;
        let url = self.url(&[
            "open-apis",
            "im",
            "v1",
            "messages",
            &message.id,
            "reactions",
            reaction_id,
        ])?;
        self.query_data(self.http.delete(url))
            .await
            .map_err(reaction_write_error)?;
        tracing::info!("飞书已删除本人表情");
        Ok(())
    }

    // 授权范围已知且没有能写表情的权限时，本地拒绝，不发请求。
    fn ensure_reaction_write(&self) -> ImResult<()> {
        if self.grant.scopes().is_some_and(|scopes| {
            !scopes
                .iter()
                .any(|s| s == "im:message" || s == "im:message.reactions:write_only")
        }) {
            return Err(ImError::new(
                ErrorKind::PermissionDenied,
                "缺少给消息点表情的权限（im:message 或 im:message.reactions:write_only），请重新登录。",
            ));
        }
        Ok(())
    }
}

/// 表情操作编号：平台给的是 URL 安全的 base64（字母、数字、`-`、`_`，末尾可有 `=` 补位），
/// 作为删除请求的路径段前按这个字符集校验，其它字符本地拒绝。
fn validate_reaction_id(reaction_id: &str) -> ImResult<()> {
    let body = reaction_id.trim_end_matches('=');
    if body.is_empty()
        || reaction_id.len() > PLATFORM_ID_MAX_BYTES
        || reaction_id.len() - body.len() > 2
        || !body
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        tracing::warn!("表情操作编号格式不正确，已拒绝");
        return Err(ImError::new(
            ErrorKind::InvalidRequest,
            "表情操作编号格式不正确。",
        ));
    }
    Ok(())
}

/// 表情类型最长多少字节。
const EMOJI_TYPE_MAX_BYTES: usize = 64;

/// 表情类型是否合规：平台原样的字符串（如 THUMBSUP、EatingFood、2022），只允许字母、数字与下划线。
/// 这是飞书平台说明里的表情名规则（契约 hhh-im client v17 §3）。
pub(crate) fn is_valid_emoji_type(emoji_type: &str) -> bool {
    !emoji_type.is_empty()
        && emoji_type.len() <= EMOJI_TYPE_MAX_BYTES
        && emoji_type
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 表情类型不合规时报错。
pub(crate) fn validate_emoji_type(emoji_type: &str) -> ImResult<()> {
    if !is_valid_emoji_type(emoji_type) {
        tracing::debug!(len = emoji_type.len(), "表情类型格式不正确，拒绝");
        return Err(ImError::new(
            ErrorKind::InvalidRequest,
            "表情类型格式不正确。",
        ));
    }
    Ok(())
}

/// 平台的一条表情对象（列表的一项或添加的返回）翻成统一类型。
fn reaction_of(raw: &Value, message: &MessageRef) -> ImResult<MessageReaction> {
    let id = required(raw, "reaction_id")?;
    // reaction_id 是不透明键；删除时作为路径段前另按表情操作编号的字符集校验。
    if id.len() > PLATFORM_ID_MAX_BYTES || id.chars().any(char::is_control) {
        return Err(invalid_reaction("表情编号超出预算或包含控制字符。"));
    }
    let operator = &raw["operator"];
    let kind = required(operator, "operator_type")?;
    let id_type = match kind.as_str() {
        "user" => "open_id",
        "app" => "app_id",
        _ => return Err(invalid_reaction("表情操作者类型无法识别。")),
    };
    let operator_id = required(operator, "operator_id")?;
    validate_id(&operator_id)?;
    let emoji_type = required(&raw["reaction_type"], "emoji_type")?;
    validate_id(&emoji_type)?;
    let action_time_ms = raw
        .get("action_time")
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .filter(|t| *t >= 0)
        .ok_or_else(|| invalid_reaction("表情添加时间无效。"))?;
    Ok(MessageReaction {
        message: message.clone(),
        id,
        emoji_type,
        action_time_ms: Some(action_time_ms),
        operator: ActorRef {
            id: operator_id,
            id_type: id_type.into(),
            kind,
            name: None,
        },
    })
}

/// 加、删表情时平台的拒绝原因：与搜索等接口共用错误码段，这里按表情接口的含义重写说明。
fn reaction_write_error(mut error: ImError) -> ImError {
    let (kind, message) = match super::errors::code_of(&error) {
        Some(231001) => (ErrorKind::InvalidRequest, "这个表情不能用来回复。"),
        Some(231002 | 231008) => (
            ErrorKind::PermissionDenied,
            "你不在这个会话里，或没有权限给这条消息点表情。",
        ),
        Some(231003 | 230110) => (ErrorKind::NotFound, "消息已撤回或不存在，不能点表情。"),
        Some(231004 | 232009) => (ErrorKind::NotFound, "会话已解散或归档，不能点表情。"),
        Some(231005) => (ErrorKind::NotFound, "话题已被删除，不能点表情。"),
        Some(231015) => (
            ErrorKind::RateLimited,
            "上一次表情操作还在处理，请稍后再试。",
        ),
        Some(231017) => (ErrorKind::Unsupported, "这类消息不能点表情。"),
        Some(231018) => (
            ErrorKind::PermissionDenied,
            "这条消息对你不可见，不能点表情。",
        ),
        Some(231019 | 231020) => (
            ErrorKind::PermissionDenied,
            "消息发送者或企业设置不允许点表情。",
        ),
        Some(231021) => (ErrorKind::PermissionDenied, "外部群里不能点表情。"),
        _ => return error,
    };
    error.kind = kind;
    error.message = message.into();
    error
}

fn invalid_reaction(message: &str) -> ImError {
    ImError::new(ErrorKind::ProtocolError, message)
}

fn reaction_permission_error(mut error: ImError) -> ImError {
    if error.kind == ErrorKind::PermissionDenied {
        error.message = "请在飞书后台启用机器人能力，开通 im:message.reactions:read 用户权限并发布/审批，再执行 login --reactions 重新授权；已有 im:message:readonly 也可读取，且本人必须能访问该会话。".into();
    }
    error
}
