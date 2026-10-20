//! 群名、用户名搜索与只读私聊定位。

use super::*;

// 只读定位只需一条由目标用户发来的单聊消息，不下载完整消息历史。
const DIRECT_LOOKUP_LIMIT: u16 = 1;
// 最近聊天每页取平台允许的最大 30 条消息，最多 3 页，热闹的群刷屏时也能凑出不同会话。
const RECENT_PAGE_SIZE: u16 = 30;
const RECENT_PAGES_MAX: usize = 3;
// 单聊对方资料按 open_id 批量读取；最近聊天最多 20 个会话，一次请求就够。
const CONTACT_SCOPE: &str = "contact:user.base:readonly";
// 通讯录批量接口单次最多 50 个 open_id；超出时分批请求。
const USERS_BATCH_MAX: usize = 50;
// 批量读取资料的总人数上限，防止调用方一次交来无界列表。
const USERS_MAX: usize = 500;
// 通讯录批量接口的“编号不存在或格式不对”：一批里只要有一个，整批被拒（400，取样 2026-09-30，契约 friend/im-watch v60）。
const USER_ID_NOT_FOUND: &str = "99992351";
// 编号属于另一应用也会拒绝整批（400 + open_id cross app，取样 2026-10-20）；只隔离这个编号。
const USER_ID_CROSS_APP: &str = "99992361";
// 认出私聊对方只读最近一页消息。
const PEER_LOOKUP_LIMIT: u16 = 20;
const CONTACT_SCOPE_NOTICE: &str =
    "开通 contact:user.base:readonly 并重新登录后，才能读取联系人资料。";

/// 最近聊天去重时暂存的一个会话。
struct RecentEntry {
    p2p: bool,
    /// 最近一条的时间（Unix 秒）。
    at: i64,
    /// 最早一条的时间（Unix 秒）。
    first: i64,
    /// 单聊里非本人的发送者；只有本人发过消息时无法确认对方。
    peer: Option<String>,
}

impl FeishuClient {
    /// 使用本人身份按群名关键词搜索，关闭通过成员姓名匹配群的行为。
    ///
    /// # Errors
    /// 关键词、分页、权限或平台响应不合法时返回明确错误。
    pub async fn search_conversations(
        &self,
        query: &SearchQuery,
    ) -> ImResult<SearchPage<Conversation>> {
        let (scope, mut params) = self.search_params("groups", query)?;
        params.push(("user_id_type".into(), "open_id".into()));
        let data = self.post_search(
            &["open-apis", "im", "v2", "chats", "search"],
            &params,
            &json!({"query":query.text,"filter":{"disable_search_by_user":true,"search_types":["private","external","public_joined"]}}),
        ).await?;
        let items = search_items(&data, "items", query.limit)?;
        let items = items
            .iter()
            .map(|item| {
                let raw = metadata(item)?;
                let mut chat = translate::conversation(&self.account.connection_id, raw, None)?;
                validate_id(&chat.reference.id)?;
                // 搜索端点只返回群，meta_data.chat_mode 的 group/topic 均为群。
                chat.kind = ConversationKind::Group;
                Ok(chat)
            })
            .collect::<ImResult<_>>()?;
        Ok(SearchPage {
            items,
            next: page_cursor(&data, scope)?,
            notice: optional(&data, "notice"),
        })
    }

    /// 用本人令牌搜索可见在职用户，不枚举外部企业用户或推断好友关系。
    ///
    /// # Errors
    /// 缺少 contact:user:search、查询或响应不合法时返回明确错误。
    pub async fn search_users(&self, query: &SearchQuery) -> ImResult<SearchPage<SearchUser>> {
        let (scope, mut params) = self.search_params("users", query)?;
        self.require_search_scope("contact:user:search")?;
        params.push(("query".into(), query.text.clone()));
        let data = self
            .get(&["open-apis", "search", "v1", "user"], &params)
            .await
            .map_err(|e| search_permission_error(e, "contact:user:search"))?;
        let items = search_items(&data, "users", query.limit)?
            .iter()
            .map(|raw| {
                let id = required(raw, "open_id")?;
                validate_id(&id)?;
                Ok(SearchUser {
                    reference: UserRef {
                        connection_id: self.account.connection_id.clone(),
                        id,
                    },
                    name: required(raw, "name")?,
                    avatar_url: avatar_of(raw),
                })
            })
            .collect::<ImResult<_>>()?;
        Ok(SearchPage {
            items,
            next: page_cursor(&data, scope)?,
            notice: optional(&data, "notice"),
        })
    }

    /// 从目标用户发来的可搜索消息中定位既有私聊，绝不发消息或创建会话。
    ///
    /// 返回空值仅表示搜索没有命中；未收过对方消息或未进入索引时也会为空。
    ///
    /// # Errors
    /// 拒绝跨账号引用及选择本人；缺权限或响应身份不符时返回错误。
    pub async fn find_direct_conversation(
        &self,
        user: &UserRef,
    ) -> ImResult<DirectConversationLookup> {
        if user.connection_id != self.account.connection_id {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "用户不属于当前连接，请重新搜索。",
            ));
        }
        validate_id(&user.id)?;
        if user.id == self.account.user.id {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "不能通过本人发送的消息定位某一私聊，请选择其他用户。",
            ));
        }
        self.require_search_scope("search:message")?;
        let data = self
            .post_search(
                &["open-apis", "im", "v1", "messages", "search"],
                &[
                    ("user_id_type".into(), "open_id".into()),
                    ("page_size".into(), DIRECT_LOOKUP_LIMIT.to_string()),
                ],
                &json!({"filter":{"from_ids":[user.id],"from_types":["user"],"chat_type":"p2p"}}),
            )
            .await
            .map_err(|e| search_permission_error(e, "search:message"))?;
        let items = search_items(&data, "items", DIRECT_LOOKUP_LIMIT)?;
        let conversation = items
            .first()
            .map(|item| {
                let raw = metadata(item)?;
                if raw.get("is_p2p_chat").and_then(Value::as_bool) != Some(true)
                    || raw.get("from_id").and_then(Value::as_str) != Some(user.id.as_str())
                {
                    return Err(ImError::new(
                        ErrorKind::ProtocolError,
                        "搜索结果不属于所选用户的私聊，未切换会话。",
                    ));
                }
                let id = required(raw, "chat_id")?;
                validate_id(&id)?;
                Ok(ConversationRef::new(&self.account.connection_id, id))
            })
            .transpose()?;
        Ok(DirectConversationLookup {
            conversation,
            notice: optional(&data, "notice"),
        })
    }

    /// 列出最近有消息往来的群与单聊，按最近一条消息时间倒序。
    ///
    /// 只读：用本人可见的消息搜索（不填关键词、限定时间范围、排除机器人消息）取得会话与时间，
    /// 群资料逐个读取，单聊对方资料用通讯录批量读取。缺通讯录读取权限、资料不可见或只有本人
    /// 发过消息的会话会跳过，并在 `notice` 中说明。
    ///
    /// # Errors
    /// 参数不合法、缺少 search:message、网络失败或平台响应不合法时返回错误。
    pub async fn recent_conversations(&self, query: &RecentQuery) -> ImResult<RecentConversations> {
        if !(1..=RECENT_LIMIT_MAX).contains(&query.limit) || query.since < 0 {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "最近聊天数量须为 1–20，起始时间不能为负数。",
            ));
        }
        let (mut entries, _) = self
            .scan_recent(
                query.since,
                RECENT_PAGES_MAX,
                Some(usize::from(query.limit)),
            )
            .await?;
        entries.truncate(usize::from(query.limit));

        let mut notices: Vec<&str> = Vec::new();
        let mut peers: Vec<String> = Vec::new();
        let direct_peers = entries
            .iter()
            .filter(|(_, entry)| entry.p2p)
            .filter_map(|(_, entry)| entry.peer.as_ref());
        for peer in direct_peers {
            if !peers.contains(peer) {
                peers.push(peer.clone());
            }
        }
        let people = if peers.is_empty() {
            std::collections::HashMap::new()
        } else if self.lacks_scope(CONTACT_SCOPE) {
            notices.push("开通 contact:user.base:readonly 并重新登录后，最近私聊才会显示。");
            std::collections::HashMap::new()
        } else {
            match self.people_by_id(&peers).await {
                Ok(people) => people,
                Err(error) if error.kind == ErrorKind::PermissionDenied => {
                    notices
                        .push("开通 contact:user.base:readonly 并重新登录后，最近私聊才会显示。");
                    std::collections::HashMap::new()
                }
                Err(error) => return Err(error),
            }
        };

        let mut items = Vec::with_capacity(entries.len());
        for (chat_id, entry) in entries {
            let reference = ConversationRef::new(&self.account.connection_id, &chat_id);
            if entry.p2p {
                let Some(person) = entry.peer.as_ref().and_then(|peer| people.get(peer)) else {
                    if entry.peer.is_none() {
                        notices.push("只有你发过消息的私聊无法确认对方，已跳过。");
                    }
                    continue;
                };
                items.push(RecentConversation {
                    // 对方放在外层的 `peer`（这个结果的既有格式），会话资料里不再重复。
                    conversation: Conversation {
                        reference,
                        name: person.0.clone(),
                        kind: ConversationKind::Direct,
                        avatar_url: person.1.clone(),
                        peer: None,
                        private: false,
                    },
                    last_active_at: entry.at,
                    peer: entry.peer.map(|id| UserRef {
                        connection_id: self.account.connection_id.clone(),
                        id,
                    }),
                });
            } else {
                match self.get_conversation(&reference).await {
                    Ok(mut conversation) => {
                        conversation.kind = ConversationKind::Group;
                        items.push(RecentConversation {
                            conversation,
                            last_active_at: entry.at,
                            peer: None,
                        });
                    }
                    Err(error)
                        if matches!(
                            error.kind,
                            ErrorKind::PermissionDenied | ErrorKind::NotFound
                        ) =>
                    {
                        notices.push("部分群资料暂时无法读取，已跳过。");
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        notices.dedup();
        Ok(RecentConversations {
            items,
            notice: (!notices.is_empty()).then(|| notices.join("")),
        })
    }

    /// 列出 `query.since_ms` 之后有人发过消息的群与单聊（契约 client v29 §2）：与最近聊天同一个不带关键词、
    /// 排除机器人的消息搜索，翻到没有下一页或满 `query.pages_max` 页为止；不读资料。单聊的对方取这段时间里
    /// 第一个不是本人的发送者，只有本人发过时为空。
    ///
    /// # Errors
    /// 起点为负、页数为 0、缺少 search:message、网络失败或平台响应不合法时返回错误。
    pub async fn recent_activity(&self, query: &ActivityQuery) -> ImResult<RecentActivity> {
        if query.since_ms < 0 || query.pages_max == 0 {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "起始时间不能为负数，至少翻一页。",
            ));
        }
        let (entries, complete) = self
            .scan_recent(
                query.since_ms.div_euclid(1000),
                usize::from(query.pages_max),
                None,
            )
            .await?;
        let items = entries
            .into_iter()
            .map(|(chat_id, entry)| ActiveConversation {
                conversation: ConversationRef::new(&self.account.connection_id, &chat_id),
                kind: if entry.p2p {
                    ConversationKind::Direct
                } else {
                    ConversationKind::Group
                },
                first_ms: entry.first.saturating_mul(1000),
                latest_ms: entry.at.saturating_mul(1000),
                // 群里的发送者不是“对方”：只有单聊才带。
                peer: entry.peer.filter(|_| entry.p2p).map(|id| UserRef {
                    connection_id: self.account.connection_id.clone(),
                    id,
                }),
                profile: None,
            })
            .collect();
        Ok(RecentActivity { items, complete })
    }

    /// 不带关键词、排除机器人的消息搜索：`since` 之后（Unix 秒）的消息按会话归并，按最近一条从新到旧。
    /// 最多翻 `pages` 页；给了 `enough` 时凑够这么多个会话就停。第二项为真表示翻完了（没有下一页）。
    async fn scan_recent(
        &self,
        since: i64,
        pages: usize,
        enough: Option<usize>,
    ) -> ImResult<(Vec<(String, RecentEntry)>, bool)> {
        self.require_search_scope("search:message")?;
        let start = iso8601_utc(since)?;
        let body =
            json!({"filter":{"time_range":{"start_time":start},"exclude_from_types":["bot"]}});
        let mut order: Vec<String> = Vec::new();
        let mut seen: std::collections::HashMap<String, RecentEntry> =
            std::collections::HashMap::new();
        let mut token: Option<String> = None;
        let mut complete = false;
        for _ in 0..pages {
            let mut params = vec![
                ("page_size".into(), RECENT_PAGE_SIZE.to_string()),
                ("user_id_type".into(), "open_id".into()),
            ];
            if let Some(token) = &token {
                params.push(("page_token".into(), token.clone()));
            }
            let data = self
                .post_search(
                    &["open-apis", "im", "v1", "messages", "search"],
                    &params,
                    &body,
                )
                .await
                .map_err(|e| search_permission_error(e, "search:message"))?;
            for item in search_items(&data, "items", RECENT_PAGE_SIZE)? {
                let raw = metadata(item)?;
                let chat_id = required(raw, "chat_id")?;
                validate_id(&chat_id)?;
                let at = parse_iso8601(&required(raw, "create_time")?)?;
                let p2p = raw.get("is_p2p_chat").and_then(Value::as_bool) == Some(true);
                let peer = optional(raw, "from_id").filter(|id| *id != self.account.user.id);
                if let Some(peer) = &peer {
                    validate_id(peer)?;
                }
                let entry = seen.entry(chat_id.clone()).or_insert_with(|| {
                    order.push(chat_id.clone());
                    RecentEntry {
                        p2p,
                        at,
                        first: at,
                        peer: None,
                    }
                });
                entry.at = entry.at.max(at);
                entry.first = entry.first.min(at);
                if entry.peer.is_none() {
                    entry.peer = peer;
                }
            }
            if enough.is_some_and(|enough| seen.len() >= enough) {
                break;
            }
            match data.get("has_more").and_then(Value::as_bool) {
                Some(true) => token = Some(required(&data, "page_token")?),
                _ => {
                    complete = true;
                    break;
                }
            }
        }
        // 平台排序未承诺按时间；这里按最近一条消息时间重排，同一时间保持首次出现的顺序。
        let mut entries: Vec<(String, RecentEntry)> = order
            .into_iter()
            .filter_map(|id| seen.remove(&id).map(|entry| (id, entry)))
            .collect();
        entries.sort_by_key(|(_, entry)| std::cmp::Reverse(entry.at));
        Ok((entries, complete))
    }

    /// 按用户引用批量读取姓名与头像，结果按请求顺序排列（重复引用只出现一次）；
    /// 平台没有返回的用户（离职或不可见）与平台说不存在的编号都不出现在结果里，调用方按“读不到”处理。
    ///
    /// # Errors
    /// 引用不属于当前连接、编号不合法、超过 500 人、缺少 contact:user.base:readonly
    /// 或平台响应不合法时返回错误。
    pub async fn users(&self, users: &[UserRef]) -> ImResult<Vec<SearchUser>> {
        if users.len() > USERS_MAX {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "一次最多读取 500 位用户的资料。",
            ));
        }
        let mut ids: Vec<String> = Vec::new();
        for user in users {
            if user.connection_id != self.account.connection_id {
                return Err(ImError::new(
                    ErrorKind::InvalidRequest,
                    "用户不属于当前连接，请重新搜索。",
                ));
            }
            validate_id(&user.id)?;
            if !ids.contains(&user.id) {
                ids.push(user.id.clone());
            }
        }
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        if self.lacks_scope(CONTACT_SCOPE) {
            return Err(ImError::new(
                ErrorKind::PermissionDenied,
                CONTACT_SCOPE_NOTICE,
            ));
        }
        let mut found = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(USERS_BATCH_MAX) {
            let people = self.people_skipping_missing(chunk).await?;
            for id in chunk {
                if let Some((name, avatar)) = people.get(id) {
                    found.push(SearchUser {
                        reference: UserRef {
                            connection_id: self.account.connection_id.clone(),
                            id: id.clone(),
                        },
                        name: name.clone(),
                        avatar_url: avatar.clone(),
                    });
                }
            }
        }
        Ok(found)
    }

    /// 从单聊最近一页消息里认出对方：取非本人的用户发送者。只读，不发消息、不创建会话。
    ///
    /// 返回空值表示最近一页只有本人或系统消息，暂时认不出，之后可以再试。
    ///
    /// # Errors
    /// 会话不属于当前连接、缺少消息读取权限、页内出现两位以上的其他发送者（不是单聊）
    /// 或平台响应不合法时返回错误。
    pub async fn find_direct_peer(
        &self,
        conversation: &ConversationRef,
    ) -> ImResult<Option<UserRef>> {
        let mut query = MessageQuery::recent(conversation.clone());
        query.limit = PEER_LOOKUP_LIMIT;
        let page = self.list_messages(&query).await?;
        let mut peer: Option<String> = None;
        for message in &page.items {
            let sender = &message.sender;
            if sender.kind != "user"
                || sender.id_type != "open_id"
                || sender.id == self.account.user.id
            {
                continue;
            }
            match &peer {
                None => {
                    validate_id(&sender.id)?;
                    peer = Some(sender.id.clone());
                }
                Some(existing) if *existing == sender.id => {}
                Some(_) => {
                    return Err(ImError::new(
                        ErrorKind::ProtocolError,
                        "这段聊天里有多位其他发送者，不是单聊。",
                    ));
                }
            }
        }
        Ok(peer.map(|id| UserRef {
            connection_id: self.account.connection_id.clone(),
            id,
        }))
    }

    /// 按 open_id 批量读取用户姓名与头像；返回 open_id → (姓名, 头像)。单次不超过 50 人。
    /// 读一批人；平台因为其中有不存在或不属于当前应用的编号整批拒绝时，对半分开再读，直到把读不到的单独挑出来跳过
    /// （k 个读不到的约多 2k·log₂n 次请求，没有读不到的只一次）。别的错误照常返回。
    async fn people_skipping_missing(
        &self,
        ids: &[String],
    ) -> ImResult<std::collections::HashMap<String, (String, Option<String>)>> {
        let mut people = std::collections::HashMap::new();
        let mut parts = vec![ids];
        while let Some(part) = parts.pop() {
            match self.people_by_id(part).await {
                Ok(found) => people.extend(found),
                Err(error)
                    if matches!(
                        error.platform_code.as_deref(),
                        Some(USER_ID_NOT_FOUND | USER_ID_CROSS_APP)
                    ) =>
                {
                    if part.len() > 1 {
                        let (front, back) = part.split_at(part.len() / 2);
                        parts.push(back);
                        parts.push(front);
                    } else {
                        tracing::debug!(code = ?error.platform_code, "飞书确认这个用户编号不可读，只跳过这个人");
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(people)
    }

    async fn people_by_id(
        &self,
        ids: &[String],
    ) -> ImResult<std::collections::HashMap<String, (String, Option<String>)>> {
        let mut params: Vec<(String, String)> = vec![("user_id_type".into(), "open_id".into())];
        params.extend(ids.iter().map(|id| ("user_ids".to_string(), id.clone())));
        let data = self
            .get(&["open-apis", "contact", "v3", "users", "batch"], &params)
            .await?;
        let items = data
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| ImError::new(ErrorKind::ProtocolError, "用户资料响应缺少 items。"))?;
        if items.len() > ids.len() {
            return Err(ImError::new(
                ErrorKind::ProtocolError,
                "用户资料数量超过请求数量。",
            ));
        }
        let mut people = std::collections::HashMap::new();
        for raw in items {
            let id = required(raw, "open_id")?;
            // 只接受本次请求的用户，避免平台多返回的身份混进会话列表。
            if !ids.contains(&id) {
                return Err(ImError::new(
                    ErrorKind::ProtocolError,
                    "用户资料不属于本次请求。",
                ));
            }
            people.insert(id, (required(raw, "name")?, avatar_of(raw)));
        }
        Ok(people)
    }

    /// 授权范围已知且不含该权限时为真；范围未知时交给平台裁决。
    // 上传等兄弟模块也按同一规则判断授权是否缺项。
    pub(super) fn lacks_scope(&self, scope: &str) -> bool {
        self.grant
            .scopes()
            .is_some_and(|scopes| !scopes.iter().any(|s| s == scope))
    }

    fn require_search_scope(&self, scope: &str) -> ImResult<()> {
        if self
            .grant
            .scopes()
            .is_some_and(|scopes| !scopes.iter().any(|s| s == scope))
        {
            return Err(missing_search_scope(scope));
        }
        Ok(())
    }

    fn search_params(
        &self,
        kind: &str,
        query: &SearchQuery,
    ) -> ImResult<(String, Vec<(String, String)>)> {
        if query.text.trim().is_empty()
            || query.text.chars().count() > SEARCH_TEXT_MAX_CHARS
            || query.text.chars().any(char::is_control)
            || !(1..=SEARCH_PAGE_MAX).contains(&query.limit)
        {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "搜索关键词须为 1–50 个字符且不含控制字符，每页数量须为 1–20。",
            ));
        }
        let scope = serde_json::to_string(&(
            "search",
            kind,
            &self.account.connection_id,
            &query.text,
            query.limit,
        ))
        .map_err(|_| ImError::new(ErrorKind::InvalidRequest, "无法建立搜索范围。"))?;
        let mut params = vec![("page_size".into(), query.limit.to_string())];
        add_cursor(&mut params, query.cursor.as_ref(), &scope)?;
        Ok((scope, params))
    }

    async fn post_search(
        &self,
        parts: &[&str],
        params: &[(String, String)],
        body: &Value,
    ) -> ImResult<Value> {
        self.query_data(self.http.post(self.url(parts)?).query(params).json(body))
            .await
    }
}

/// Unix 秒转成 UTC 的 ISO 8601（`YYYY-MM-DDTHH:MM:SSZ`），供消息搜索的时间范围使用。
fn iso8601_utc(seconds: i64) -> ImResult<String> {
    if seconds < 0 {
        return Err(ImError::new(ErrorKind::InvalidRequest, "时间不能为负数。"));
    }
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    ))
}

/// 解析飞书返回的 ISO 8601 时间（允许小数秒，时区为 `Z`、`±HH:MM` 或 `±HHMM`），得到 Unix 秒。
fn parse_iso8601(value: &str) -> ImResult<i64> {
    let invalid = || ImError::new(ErrorKind::ProtocolError, "飞书返回的时间格式不正确。");
    let bytes = value.as_bytes();
    let digits = |from: usize, len: usize| -> ImResult<i64> {
        let part = value.get(from..from + len).ok_or_else(invalid)?;
        if !part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }
        part.parse::<i64>().map_err(|_| invalid())
    };
    if bytes.len() < 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return Err(invalid());
    }
    let (year, month, day) = (digits(0, 4)?, digits(5, 2)?, digits(8, 2)?);
    let (hour, minute, second) = (digits(11, 2)?, digits(14, 2)?, digits(17, 2)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return Err(invalid());
    }
    let mut rest = &value[19..];
    if let Some(fraction) = rest.strip_prefix('.') {
        let end = fraction
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(fraction.len());
        if end == 0 {
            return Err(invalid());
        }
        rest = &fraction[end..];
    }
    let offset = match rest {
        "Z" => 0,
        _ => {
            let sign = match rest.as_bytes().first() {
                Some(b'+') => 1,
                Some(b'-') => -1,
                _ => return Err(invalid()),
            };
            let zone = rest[1..].replace(':', "");
            if zone.len() != 4 || !zone.bytes().all(|b| b.is_ascii_digit()) {
                return Err(invalid());
            }
            let (h, m) = (
                zone[..2].parse::<i64>().map_err(|_| invalid())?,
                zone[2..].parse::<i64>().map_err(|_| invalid())?,
            );
            if h > 23 || m > 59 {
                return Err(invalid());
            }
            sign * (h * 3600 + m * 60)
        }
    };
    Ok(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

// 公历与 1970-01-01 起的天数互转（Howard Hinnant 的 civil 算法），不引入时间库。
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

fn search_items<'a>(data: &'a Value, key: &str, limit: u16) -> ImResult<&'a Vec<Value>> {
    let items = data
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| ImError::new(ErrorKind::ProtocolError, "搜索响应缺少结果列表。"))?;
    if items.len() > usize::from(limit) {
        return Err(ImError::new(
            ErrorKind::ProtocolError,
            "搜索结果超过请求的页大小。",
        ));
    }
    Ok(items)
}
/// 用户资料里的头像：优先 72px（界面最大 44px，二倍屏也够用），缺失时取 240px。
fn avatar_of(raw: &Value) -> Option<String> {
    raw.get("avatar")
        .and_then(|avatar| optional(avatar, "avatar_72").or_else(|| optional(avatar, "avatar_240")))
}
fn metadata(item: &Value) -> ImResult<&Value> {
    item.get("meta_data")
        .filter(|v| v.is_object())
        .ok_or_else(|| ImError::new(ErrorKind::ProtocolError, "搜索结果缺少对象信息。"))
}
fn missing_search_scope(scope: &str) -> ImError {
    ImError::new(
        ErrorKind::PermissionDenied,
        format!("请在飞书应用后台开通 {scope}，再在测试台执行 login --search 重新授权。"),
    )
}
fn search_permission_error(error: ImError, scope: &str) -> ImError {
    if error.kind == ErrorKind::PermissionDenied {
        let mut explained = missing_search_scope(scope);
        explained.platform_code.clone_from(&error.platform_code);
        explained.retry_after_secs = error.retry_after_secs;
        explained
    } else {
        error
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso8601_round_trips_utc_and_honors_offsets() {
        assert_eq!(iso8601_utc(0).unwrap(), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601_utc(1_774_080_930).unwrap(), "2026-03-21T08:15:30Z");
        assert_eq!(
            parse_iso8601("2026-03-21T16:15:30+08:00").unwrap(),
            1_774_080_930
        );
        assert_eq!(
            parse_iso8601("2026-03-21T08:15:30Z").unwrap(),
            1_774_080_930
        );
        assert_eq!(
            parse_iso8601("2026-03-21T08:15:30.250Z").unwrap(),
            1_774_080_930
        );
        assert_eq!(
            parse_iso8601("2026-03-21T03:15:30-0500").unwrap(),
            1_774_080_930
        );
        assert_eq!(
            parse_iso8601("2024-02-29T00:00:00Z").unwrap(),
            1_709_164_800
        );
        for value in (0..800).map(|n| n * 86_399 * 37) {
            assert_eq!(parse_iso8601(&iso8601_utc(value).unwrap()).unwrap(), value);
        }
    }

    #[test]
    fn iso8601_rejects_malformed_or_out_of_range_values() {
        for bad in [
            "",
            "2026-03-21",
            "2026-13-01T00:00:00Z",
            "2026-03-21T24:00:00Z",
            "2026-03-21T08:15:30",
            "2026-03-21T08:15:30+8",
            "2026-03-21T08:15:30.Z",
            "2026/03/21T08:15:30Z",
        ] {
            assert!(parse_iso8601(bad).is_err(), "{bad}");
        }
        assert!(iso8601_utc(-1).is_err());
    }
}
