//! 飞书 HTTP 接口封装；私有响应格式在本模块完成翻译。

mod auth;
pub(crate) mod card;
mod emoji;
mod errors;
mod events;
mod forwarded;
// 平台事实：原来登记在 HHH 平台表里的 `PLATFORM` 与连接表单，第二步写进 `initialize` 的声明。
pub mod facts;
mod im_client;
mod markdown;
mod members;
mod post_send;
mod reactions;
mod reading;
mod resource;
mod rich;
mod search;
pub(crate) mod translate;
mod upload;

pub use auth::FeishuAuth;
pub use emoji::EMOJI_IMAGE_MAX_BYTES;
pub use events::{FeishuEvents, WebhookDelivery};
pub use members::MEMBER_PAGE_MAX;
pub use resource::RESOURCE_SEGMENT_BYTES;
pub use upload::{FILE_UPLOAD_MAX_BYTES, IMAGE_UPLOAD_MAX_BYTES};

use crate::*;
use reqwest::{Response, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

/// 只改内容的修正：按内容规则改写，规则说不用改时为空。
fn content_repair(
    message: &ImMessage,
    rule: fn(&[ImContent]) -> Option<Vec<ImContent>>,
) -> Option<ImMessage> {
    rule(&message.content).map(|content| ImMessage {
        content,
        ..message.clone()
    })
}

/// 升级前存下的“编辑过的文字消息”露出 `<p>` 标签（契约 friend/im-watch v37）：本地库打开时修正一次。
pub(crate) const EDITED_TEXT_REPAIR: ContentRepair = ContentRepair {
    name: "feishu-edited-text-v1",
    contains: "<p>",
    rewrite: |message| content_repair(message, translate::repair_edited_content),
};

/// 升级前存下的暂不支持块没有叫法（契约 friend/im-watch v46：叫法从界面搬到平台适配器）：补上一次。
pub(crate) const UNSUPPORTED_LABEL_REPAIR: ContentRepair = ContentRepair {
    name: "feishu-unsupported-label-v1",
    contains: "\"type\":\"unsupported\"",
    rewrite: |message| content_repair(message, translate::repair_unsupported_labels),
};

/// 升级前按“被更新过”记下的编辑时间大多是误标（带提及、链接、图的消息发出后飞书会自己更新一次，契约 client v26 §3）：
/// 本地库打开时一次去掉，不联网，界面不会有误标一闪而过。之后按现在的规则判断：同步复核读到的近期消息里，
/// 真改过的文字照样标出；更早的真编辑无从核实，不再标。取代 v65 的 `feishu-thread-root-edited-v1` 重读。
pub(crate) const EDITED_MARKS_REPAIR: ContentRepair = ContentRepair {
    name: "feishu-edited-marks-v2",
    contains: "\"editedAtMs\"",
    rewrite: translate::drop_old_edit_mark,
};

/// 升级前整张卡片只记成“暂不支持”（契约 friend/im-watch v46）：出现在视野里时重读一次，翻成卡片。
pub(crate) const CARD_REREAD: ContentReread = ContentReread {
    name: "feishu-card-v1",
    contains: "\"kind\":\"interactive\"",
    applies: translate::is_unsupported_card,
};

/// 升级前卡片 2.0 只能退回简化内容（只剩标题与升级提示、标了不完整；契约 friend/im-watch v60、client v16 §6）：
/// 出现在视野里时重读一次，按原始结构翻成完整卡片。
pub(crate) const CARD_V2_REREAD: ContentReread = ContentReread {
    name: "feishu-card-v2",
    contains: "\"type\":\"card\"",
    applies: translate::is_incomplete_card,
};

/// 卡片 2.0 的 Markdown 表格读回时丢了对齐，升级前存下的还是每页 5 行（契约 friend/im-watch v63、client v19 §3）：
/// 出现在视野里时重读一次，按飞书内部结构补上对齐与每页行数；同步时补读超过上限、补读失败的也在其中。
pub(crate) const CARD_TABLE_REREAD: ContentReread = ContentReread {
    name: "feishu-card-table-v1",
    contains: "\"type\":\"table\"",
    applies: translate::has_unaligned_card_tables,
};

/// 升级前存下的卡片 1.0 段落里 @ 还是 `<at id=…></at>` 原文（契约 client v31 §2 第 6 条）：出现在视野里时重读一次，
/// 翻成提及，发卡片的是别的应用时按 `raw_card_content` 对到能认的人。
pub(crate) const CARD_MENTION_REREAD: ContentReread = ContentReread {
    name: "feishu-card-mention-v1",
    contains: "<at ",
    applies: translate::has_raw_card_at_text,
};

/// 升级前存下的文字里的网址只是文字（契约 friend/im-watch v51、client v6 §5）：本地库打开时变成链接一次，
/// 不联网。排在编辑文字修正之后（先去掉 `<p>` 再找网址）。
pub(crate) const LINKIFY_TEXT_REPAIR: ContentRepair = ContentRepair {
    name: "feishu-linkify-text-v1",
    contains: "\"type\":\"text\"",
    rewrite: |message| content_repair(message, rich::repair_links),
};

/// 升级前存下的文字消息里 @所有人 只是文字 `@_all`（契约 client v25 §3.3）：本地库打开时拆成提及所有人一次，
/// 不联网。排在网址修正之前：先拆出提及，带网址的再整理成格式正文。
pub(crate) const MENTION_ALL_REPAIR: ContentRepair = ContentRepair {
    name: "feishu-mention-all-v1",
    contains: "@_all",
    rewrite: |message| content_repair(message, translate::repair_mention_all),
};

/// 升级前带网址的文字消息已整理成格式正文，`@_all` 留在文字节点里（契约 client v25 §3.3）：
/// 出现在视野里时重读一次，按现在的翻译变成提及；富文本里手打的照旧是文字。
pub(crate) const MENTION_ALL_RICH_REREAD: ContentReread = ContentReread {
    name: "feishu-mention-all-rich-v1",
    contains: "@_all",
    applies: translate::has_rich_mention_all_text,
};

/// 飞书的本地修正，按这个顺序做：先去掉编辑留下的 `<p>`，再拆出 @所有人，然后把带网址的整理成格式正文，
/// 最后去掉升级前误标的编辑时间。
pub(crate) const CONTENT_REPAIRS: [ContentRepair; 5] = [
    EDITED_TEXT_REPAIR,
    UNSUPPORTED_LABEL_REPAIR,
    MENTION_ALL_REPAIR,
    LINKIFY_TEXT_REPAIR,
    EDITED_MARKS_REPAIR,
];

/// 升级前富文本里的表情、分割线、代码块只记成“暂不支持”（契约 client v6 §5）：出现在视野里时重读一次。
pub(crate) const RICH_POST_REREAD: ContentReread = ContentReread {
    name: "feishu-rich-post-v1",
    contains: "\"type\":\"unsupported\"",
    applies: rich::is_unsupported_rich_post,
};

/// 升级前表情包只记成“暂不支持”、没记下图片编号（契约 client v15 §1、friend/im-watch v59）：出现在视野里时重读一次，
/// 翻成表情包资源。
/// 升级前存下的视频消息向飞书重读一次，补上封面与时长（契约 client v35 §1.7）。
pub(crate) const VIDEO_REREAD: ContentReread = ContentReread {
    name: "feishu-video-v35",
    contains: "\"kind\":\"file\"",
    applies: translate::is_video_without_details,
};

pub(crate) const STICKER_REREAD: ContentReread = ContentReread {
    name: "feishu-sticker-v1",
    contains: "\"kind\":\"sticker\"",
    applies: translate::is_unsupported_sticker,
};

pub(crate) use emoji::{
    complete_catalog as complete_emoji_catalog, guessed_images as guessed_emoji_images,
};

/// JSON 响应的解压后上限，避免历史或错误响应无界读入。
pub const RESPONSE_MAX_BYTES: usize = 4 * 1024 * 1024;
/// 平台规定的消息分页上限。
pub const MESSAGE_PAGE_MAX: u16 = 50;
/// 读消息时要发送时的原始卡片 JSON，而不是飞书简化过的内容（简化内容会把多栏压扁）。
const CARD_CONTENT_PARAM: (&str, &str) = ("card_msg_content_type", "user_card_content");
/// 原始卡片认不出结构时，一页里最多几条不带上面的参数再读一次、改用简化内容；每条多一次请求。
const CARD_FALLBACK_REREADS_MAX: usize = 3;
/// 飞书客户端自己用的卡片结构（`json_card`，没写进文档）：Markdown 表格的对齐只在这里还留着（契约 client v19 §2）。
const RAW_CARD_PARAM: (&str, &str) = ("card_msg_content_type", "raw_card_content");
/// 卡片表格没有对齐信息时，一页里最多几条当场按飞书内部结构补读；每条多一次请求，其余在视野里重读时补上。
const CARD_TABLE_LAYOUT_READS_MAX: usize = 5;
/// 补读过的卡片表格排法记住几条：新消息每隔几秒按复核窗口重读一遍，同一张卡片不为此反复多请求。
const CARD_TABLE_LAYOUTS_REMEMBERED: usize = 256;
/// 卡片里的 @ 对不上消息的提及名单时，一页里最多几条当场按飞书内部结构补读；每条多一次请求，其余在视野里重读时补上。
const CARD_MENTION_READS_MAX: usize = 5;
/// 补读过的卡片 @ 对应记住几条（同 [`CARD_TABLE_LAYOUTS_REMEMBERED`]）。
const CARD_MENTIONS_REMEMBERED: usize = 256;
/// 平台规定的群列表分页上限。
pub const CONVERSATION_PAGE_MAX: u16 = 100;
/// 所有单次 HTTP 操作的总超时。
const REQUEST_TIMEOUT_SECS: u64 = 30;
/// 连接池里闲置多久的连接就丢掉、下次新建（v9）：闲置太久的连接可能已被网络设备悄悄断掉，
/// 复用它发出去的请求既收不到回应也收不到断开，只能等超时。
const POOL_IDLE_SECS: u64 = 20;
/// TCP 保活探测的间隔（v9）：让系统及早发现对端已经不在的连接。
const TCP_KEEPALIVE_SECS: u64 = 15;
/// 飞书发送 uuid 的官方长度上限，供宿主在写操作记录前校验。
pub const OPERATION_ID_MAX_BYTES: usize = 50;
/// 对象编号保留 512 字节预算，避免把任意大文本当 URL 路径。
const PLATFORM_ID_MAX_BYTES: usize = 512;

/// 可注入的飞书端点；默认使用官方服务，HTTP 仅允许本机测试地址。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeishuConfig {
    /// OpenAPI 根地址。
    pub api_base: String,
    /// 官方授权页面地址。
    pub authorize_url: String,
    /// 官方 OAuth 令牌地址。
    pub token_url: String,
}
impl Default for FeishuConfig {
    fn default() -> Self {
        Self {
            api_base: "https://open.feishu.cn".into(),
            authorize_url: "https://accounts.feishu.cn/open-apis/authen/v1/authorize".into(),
            token_url: "https://accounts.feishu.cn/oauth/v3/token".into(),
        }
    }
}
impl FeishuConfig {
    pub(crate) fn validate(&self) -> ImResult<()> {
        for value in [&self.api_base, &self.authorize_url, &self.token_url] {
            let url = Url::parse(value)
                .map_err(|_| ImError::new(ErrorKind::InvalidRequest, "飞书服务地址格式不正确。"))?;
            validate_url(&url)?;
            if url.query().is_some() || url.fragment().is_some() {
                return Err(ImError::new(
                    ErrorKind::InvalidRequest,
                    "服务端点不能包含查询参数或片段。",
                ));
            }
        }
        Ok(())
    }
}

pub(crate) fn validate_url(url: &Url) -> ImResult<()> {
    let local = url.host_str().is_some_and(|h| {
        h == "localhost"
            || h.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if !url.username().is_empty()
        || url.password().is_some()
        || (url.scheme() != "https" && !(url.scheme() == "http" && local))
    {
        return Err(ImError::new(
            ErrorKind::InvalidRequest,
            "地址必须使用 HTTPS；仅本机测试或回调可使用 HTTP，地址中不能包含凭据。",
        ));
    }
    Ok(())
}

pub(crate) fn http_client() -> ImResult<reqwest::Client> {
    // 总超时给发送、上传、下载用；查询在请求上另设更短的 QUERY_TIMEOUT。
    crate::net::client_builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .pool_idle_timeout(Duration::from_secs(POOL_IDLE_SECS))
        .tcp_keepalive(Duration::from_secs(TCP_KEEPALIVE_SECS))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| {
            let cause = crate::net::network_cause(error);
            tracing::warn!(%cause, "创建 IM 网络客户端失败");
            ImError::new(ErrorKind::ProtocolError, "无法创建 IM 网络客户端。")
        })
}

/// 下载资源与头像用的客户端：连接超时与保活同 HHH 的下载引擎（[`crate::net::download_client_builder`]），
/// 重定向不跟随（令牌不带去别的主机）。
pub(crate) fn download_client() -> ImResult<reqwest::Client> {
    crate::net::download_client_builder()
        .pool_idle_timeout(Duration::from_secs(POOL_IDLE_SECS))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| {
            let cause = crate::net::network_cause(error);
            tracing::warn!(%cause, "创建 IM 下载客户端失败");
            ImError::new(ErrorKind::ProtocolError, "无法创建 IM 下载客户端。")
        })
}

pub(crate) async fn read_json(mut response: Response) -> ImResult<Value> {
    let status = response.status().as_u16();
    let retry = response
        .headers()
        .get("retry-after")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse().ok());
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| ImError::network(error, "读取飞书响应时连接中断。"))?
    {
        if bytes.len().saturating_add(chunk.len()) > RESPONSE_MAX_BYTES {
            return Err(ImError::new(
                ErrorKind::LimitExceeded,
                "飞书响应超过读取预算。",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) if !(200..300).contains(&status) => {
            return Err(errors::from_code(-1, status, retry));
        }
        Err(_) => {
            return Err(ImError::new(
                ErrorKind::ProtocolError,
                "飞书未返回有效的 JSON 响应。",
            ));
        }
    };
    let code = value.get("code").and_then(Value::as_i64);
    if !(200..300).contains(&status) || code.is_some_and(|c| c != 0) || value.get("error").is_some()
    {
        return Err(errors::from_code(code.unwrap_or(-1), status, retry));
    }
    if code.is_none() && value.get("access_token").is_none() {
        return Err(ImError::new(
            ErrorKind::ProtocolError,
            "飞书响应缺少成功状态。",
        ));
    }
    Ok(value)
}

/// 绑定一个已核验用户账号的飞书客户端。
pub struct FeishuClient {
    config: FeishuConfig,
    http: reqwest::Client,
    /// 下载资源与头像用：不设总超时（大文件慢慢下），等首个响应与读停滞的超时由下载层计。
    download: reqwest::Client,
    grant: AuthGrant,
    account: Account,
    /// 补读过的卡片表格排法：键是消息编号 + 没对齐的表格表头（卡片被更新、表头变了就重新读），
    /// 值为空表示读过但飞书没给出能用的（不再为它请求）。按先后淘汰，最多 [`CARD_TABLE_LAYOUTS_REMEMBERED`] 条。
    table_layouts:
        std::sync::Mutex<std::collections::VecDeque<(String, std::sync::Arc<[card::TableLayout]>)>>,
    /// 补读过的卡片 @ 对应：键是消息编号 + 对不上的编号，值是卡片里的编号 → 换成的人（为空表示读过但没有能用的）。
    /// 按先后淘汰，最多 [`CARD_MENTIONS_REMEMBERED`] 条。
    card_mentions: CardMentions,
}

/// 补读过的卡片 @ 对应（[`FeishuClient`] 的 `card_mentions`）。
type CardMentions =
    std::sync::Mutex<std::collections::VecDeque<(String, std::sync::Arc<[(String, ActorRef)]>)>>;
impl std::fmt::Debug for FeishuClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeishuClient")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

impl FeishuClient {
    /// 用用户令牌核验本人身份；不接受机器人令牌冒充个人连接。
    pub async fn connect(
        config: FeishuConfig,
        connection_id: impl Into<String>,
        grant: AuthGrant,
    ) -> ImResult<Self> {
        config.validate()?;
        grant.ensure_current()?;
        let connection_id = connection_id.into();
        if connection_id.is_empty() {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "连接编号不能为空。",
            ));
        }
        let http = http_client()?;
        let response = http
            .get(format!(
                "{}/open-apis/authen/v1/user_info",
                config.api_base.trim_end_matches('/')
            ))
            .bearer_auth(grant.access_token())
            .send()
            .await
            .map_err(|error| ImError::network(error, "无法连接飞书以核验用户身份。"))?;
        let raw = read_json(response).await?;
        let data = raw
            .get("data")
            .ok_or_else(|| ImError::new(ErrorKind::ProtocolError, "身份响应缺少用户信息。"))?;
        let account = Account {
            connection_id,
            platform: "feishu".into(),
            user: ActorRef {
                id: required(data, "open_id")?,
                id_type: "open_id".into(),
                kind: "user".into(),
                name: optional(data, "name"),
            },
            tenant_id: optional(data, "tenant_key"),
            avatar_url: optional(data, "avatar_url"),
        };
        tracing::info!("飞书用户身份核验完成");
        Ok(Self {
            config,
            http,
            download: download_client()?,
            grant,
            account,
            table_layouts: std::sync::Mutex::default(),
            card_mentions: std::sync::Mutex::default(),
        })
    }

    /// 当前连接已核验的本人身份。
    pub fn account(&self) -> &Account {
        &self.account
    }

    /// 返回权限声明和平台限制；Grant 不表示指定会话已成功实测。
    pub fn capabilities(&self) -> CapabilitySet {
        let permission = |scopes: &[&str], all: bool| {
            self.grant
                .scopes()
                .map_or(CapabilityState::Unknown, |granted| {
                    let contains = |s: &&str| granted.iter().any(|v| v == s);
                    if (all && scopes.iter().all(contains)) || (!all && scopes.iter().any(contains))
                    {
                        CapabilityState::Granted
                    } else {
                        CapabilityState::MissingScope
                    }
                })
        };
        let items = [
            (
                "identity",
                CapabilityState::Verified,
                "已通过用户信息接口核验本人身份。",
            ),
            (
                "listGroups",
                permission(&["im:chat", "im:chat:readonly", "im:chat:read"], false),
                "仅列出用户所在群，不包含单聊；具体会话访问以请求结果为准。",
            ),
            (
                "listDirectConversations",
                CapabilityState::Unsupported,
                "飞书群列表接口不包含个人单聊，可用已知 chat_id 查询历史。",
            ),
            (
                "searchGroups",
                permission(&["im:chat", "im:chat:readonly", "im:chat:read"], false),
                "按群名关键词搜索可见群；结果不保证可读取消息。",
            ),
            (
                "searchUsers",
                permission(&["contact:user:search"], true),
                "按姓名搜索可见的在职用户，不包含外部企业用户，也不代表好友关系。",
            ),
            (
                "findDirectConversation",
                permission(&["search:message"], true),
                "从对方发来的可搜索私聊消息定位会话；找不到不代表不存在，不会发送消息。",
            ),
            (
                "readGroupHistory",
                self.history_permission("im:message.group_msg:get_as_user"),
                "需消息基础权限和以用户身份获取群聊消息权限；具体群仍需实际查询核验。",
            ),
            (
                "readDirectHistory",
                self.history_permission("im:message.p2p_msg:get_as_user"),
                "需消息基础权限和以用户身份获取单聊消息权限；已知 chat_id 也不保证实际可见。",
            ),
            (
                "readReactions",
                permission(&["im:message.reactions:read", "im:message:readonly"], false),
                "读取可访问消息的现有表情；需应用机器人能力及相应用户权限，姓名未必可得。轮询不保证捕获短暂变化。",
            ),
            (
                "sendAsUser",
                permission(&["im:message", "im:message.send_as_user"], true),
                "始终使用本人令牌，仍需目标会话访问和发言权限。",
            ),
            (
                "personalRealtime",
                CapabilityState::Unsupported,
                "已封装的官方消息事件是应用／机器人可见范围，不能用于声明个人全部群／好友实时订阅。",
            ),
            (
                "applicationWebhook",
                CapabilityState::Unknown,
                "支持处理已配置应用的明文校验回调；需独立应用事件权限，不等于本人私聊监听。",
            ),
            (
                "lookupSendByOperation",
                CapabilityState::Unsupported,
                "没有已核验的按发送 uuid 查询接口；有消息 ID 时可 get_message 核验。",
            ),
        ];
        CapabilitySet {
            account: self.account.clone(),
            capabilities: items
                .into_iter()
                .map(|(name, state, detail)| Capability {
                    name: name.into(),
                    state,
                    detail: detail.into(),
                })
                .collect(),
        }
    }

    fn history_permission(&self, specific: &str) -> CapabilityState {
        self.grant
            .scopes()
            .map_or(CapabilityState::Unknown, |scopes| {
                if scopes
                    .iter()
                    .any(|s| s == "im:message" || s == "im:message:readonly")
                    && scopes.iter().any(|s| s == specific)
                {
                    CapabilityState::Granted
                } else {
                    CapabilityState::MissingScope
                }
            })
    }

    /// 更换已安全保存的新凭据，拒绝切换为不同用户。
    pub async fn replace_auth(&mut self, grant: AuthGrant) -> ImResult<()> {
        let replacement = Self::connect(
            self.config.clone(),
            self.account.connection_id.clone(),
            grant,
        )
        .await?;
        if replacement.account.user.id != self.account.user.id
            || replacement.account.tenant_id != self.account.tenant_id
        {
            return Err(ImError::new(
                ErrorKind::PermissionDenied,
                "新凭据属于不同用户或租户，请创建另一连接。",
            ));
        }
        *self = replacement;
        Ok(())
    }

    pub(crate) fn check_conversation(&self, conversation: &ConversationRef) -> ImResult<()> {
        if conversation.connection_id != self.account.connection_id {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "会话不属于当前连接。",
            ));
        }
        validate_id(&conversation.id)?;
        Ok(())
    }

    fn url(&self, parts: &[&str]) -> ImResult<Url> {
        let mut url = Url::parse(&self.config.api_base)
            .map_err(|_| ImError::new(ErrorKind::InvalidRequest, "服务地址格式不正确。"))?;
        url.path_segments_mut()
            .map_err(|_| ImError::new(ErrorKind::InvalidRequest, "服务地址无法构造请求路径。"))?
            .pop_if_empty()
            .extend(parts);
        Ok(url)
    }

    /// 只读查询：卡住时最多等 [`crate::im::client::QUERY_TIMEOUT`]，不等满发送用的总超时。
    async fn get(&self, parts: &[&str], params: &[(String, String)]) -> ImResult<Value> {
        self.query_data(
            self.http
                .get(self.url(parts)?)
                .query(params)
                .timeout(crate::im::client::QUERY_TIMEOUT),
        )
        .await
    }

    // 查询请求共用有界响应和身份校验；发送仍保留自己的 Unknown 语义。
    async fn query_data(&self, request: reqwest::RequestBuilder) -> ImResult<Value> {
        self.grant.ensure_current()?;
        crate::metering::request();
        let response = request
            .bearer_auth(self.grant.access_token())
            .send()
            .await
            .map_err(|error| ImError::network(error, "飞书查询连接失败，请稍后重试。"))?;
        let raw = read_json(response).await?;
        raw.get("data")
            .cloned()
            .ok_or_else(|| ImError::new(ErrorKind::ProtocolError, "飞书成功响应缺少数据。"))
    }

    /// 分页列出用户所在群；平台接口不包含单聊。
    pub async fn list_conversations(
        &self,
        limit: u16,
        cursor: Option<&PageCursor>,
    ) -> ImResult<Page<Conversation>> {
        if limit == 0 || limit > CONVERSATION_PAGE_MAX {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "群列表每页数量应在 1 至 100 之间。",
            ));
        }
        let scope = format!("groups:{}:{limit}", self.account.connection_id);
        let mut params = vec![
            ("page_size".into(), limit.to_string()),
            ("user_id_type".into(), "open_id".into()),
            ("sort_type".into(), "ByCreateTimeAsc".into()),
        ];
        add_cursor(&mut params, cursor, &scope)?;
        let data = self
            .get(&["open-apis", "im", "v1", "chats"], &params)
            .await?;
        let items = data
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| ImError::new(ErrorKind::ProtocolError, "群列表响应缺少 items。"))?;
        if items.len() > usize::from(limit) {
            return Err(ImError::new(
                ErrorKind::ProtocolError,
                "平台返回的群数量超过请求页大小。",
            ));
        }
        Ok(Page {
            items: items
                .iter()
                .map(|v| translate::conversation(&self.account.connection_id, v, None))
                .collect::<ImResult<_>>()?,
            next: page_cursor(&data, scope)?,
        })
    }

    /// 取得指定会话资料。
    pub async fn get_conversation(&self, reference: &ConversationRef) -> ImResult<Conversation> {
        self.check_conversation(reference)?;
        let data = self
            .get(
                &["open-apis", "im", "v1", "chats", &reference.id],
                &[("user_id_type".into(), "open_id".into())],
            )
            .await?;
        translate::conversation(&self.account.connection_id, &data, Some(&reference.id))
    }

    /// 按时间、话题和游标读取一页消息，页内统一由旧到新。
    pub async fn list_messages(&self, query: &MessageQuery) -> ImResult<Page<ImMessage>> {
        self.check_conversation(&query.conversation)?;
        if query.limit == 0
            || query.limit > MESSAGE_PAGE_MAX
            || query.start_time.is_some_and(|t| t < 0)
            || query.end_time.is_some_and(|t| t < 0)
            || query
                .start_time
                .zip(query.end_time)
                .is_some_and(|(a, b)| a > b)
        {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "消息每页数量应在 1 至 50 之间，时间范围必须有效。",
            ));
        }
        let mut scope_query = query.clone();
        if let Some(thread) = &query.thread_id {
            validate_id(thread)?;
            if query.start_time.is_some() || query.end_time.is_some() {
                return Err(ImError::new(
                    ErrorKind::InvalidRequest,
                    "按话题读取消息不支持时间范围，请只按页读取。",
                ));
            }
        }
        scope_query.cursor = None;
        let scope = serde_json::to_string(&scope_query)
            .map_err(|_| ImError::new(ErrorKind::InvalidRequest, "无法编码消息查询范围。"))?;
        let mut params = vec![
            (
                "container_id_type".into(),
                if query.thread_id.is_some() {
                    "thread"
                } else {
                    "chat"
                }
                .into(),
            ),
            (
                "container_id".into(),
                query
                    .thread_id
                    .clone()
                    .unwrap_or_else(|| query.conversation.id.clone()),
            ),
            ("page_size".into(), query.limit.to_string()),
            (CARD_CONTENT_PARAM.0.into(), CARD_CONTENT_PARAM.1.into()),
            (
                "sort_type".into(),
                if query.newest_first {
                    "ByCreateTimeDesc"
                } else {
                    "ByCreateTimeAsc"
                }
                .into(),
            ),
        ];
        if let Some(t) = query.start_time {
            params.push(("start_time".into(), t.to_string()));
        }
        if let Some(t) = query.end_time {
            params.push(("end_time".into(), t.to_string()));
        }
        add_cursor(&mut params, query.cursor.as_ref(), &scope)?;
        let data = self
            .get(&["open-apis", "im", "v1", "messages"], &params)
            .await?;
        let raw = data
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| ImError::new(ErrorKind::ProtocolError, "消息列表响应缺少 items。"))?;
        if raw.len() > usize::from(query.limit) {
            return Err(ImError::new(
                ErrorKind::ProtocolError,
                "平台返回的消息数量超过请求页大小。",
            ));
        }
        let mut items = raw
            .iter()
            .map(|v| translate::message(&self.account.connection_id, v))
            .collect::<ImResult<Vec<_>>>()?;
        if items
            .iter()
            .any(|m| m.reference.conversation != query.conversation)
        {
            return Err(ImError::new(
                ErrorKind::ProtocolError,
                "平台返回了其它会话的消息。",
            ));
        }
        // 原始卡片认不出结构（如还没取样过的新版卡片）：改读简化内容，每页有上限，超出的保持暂不支持。
        let mut rereads = 0;
        for (item, source) in items.iter_mut().zip(raw) {
            if !translate::needs_card_fallback(source) {
                continue;
            }
            if rereads == CARD_FALLBACK_REREADS_MAX {
                tracing::debug!(message = %item.reference.id, "卡片结构认不出，本页补读已到上限，按暂不支持显示");
                continue;
            }
            rereads += 1;
            if let Some(simplified) = self.simplified_card(&item.reference).await {
                *item = simplified;
            }
        }
        // 卡片表格读回时丢了对齐：按飞书内部结构补上，每页当场至多补读几条（记住过的不算），其余等视野里重读。
        let mut layout_reads = 0;
        for item in &mut items {
            if !translate::has_unaligned_card_tables(item) {
                continue;
            }
            if self.remembered_table_layouts(item).is_none() {
                if layout_reads == CARD_TABLE_LAYOUT_READS_MAX {
                    tracing::debug!(message = %item.reference.id, "卡片表格要补读对齐，本页补读已到上限，等视野里重读");
                    continue;
                }
                layout_reads += 1;
            }
            self.fill_table_layouts(item).await;
        }
        // 卡片里的 @ 是别的应用的编号：按飞书内部结构对到能认的人，每页当场至多补读几条（记住过的不算），其余等视野里重读。
        let mut mention_reads = 0;
        for (item, source) in items.iter_mut().zip(raw) {
            let unknown = translate::unknown_card_mentions(item, source);
            if unknown.is_empty() {
                continue;
            }
            if self.remembered_card_mentions(item, &unknown).is_none() {
                if mention_reads == CARD_MENTION_READS_MAX {
                    tracing::debug!(message = %item.reference.id, "卡片里的 @ 要补读对应，本页补读已到上限，等视野里重读");
                    continue;
                }
                mention_reads += 1;
            }
            self.fill_card_mentions(item, source, &unknown).await;
        }
        items.sort_by_key(|m| m.created_at_ms);
        Ok(Page {
            items,
            next: page_cursor(&data, scope)?,
        })
    }

    /// 按 ID 补读一条消息，并核对所属会话。
    pub async fn get_message(&self, reference: &MessageRef) -> ImResult<ImMessage> {
        self.check_conversation(&reference.conversation)?;
        validate_id(&reference.id)?;
        let data = self
            .get(
                &["open-apis", "im", "v1", "messages", &reference.id],
                &[(CARD_CONTENT_PARAM.0.into(), CARD_CONTENT_PARAM.1.into())],
            )
            .await?;
        let raw = data
            .get("items")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .ok_or_else(|| ImError::new(ErrorKind::NotFound, "平台没有返回这条消息。"))?;
        let mut message = translate::message(&self.account.connection_id, raw)?;
        if translate::needs_card_fallback(raw)
            && message.reference == *reference
            && let Some(simplified) = self.simplified_card(reference).await
        {
            message = simplified;
        }
        if message.reference != *reference {
            return Err(ImError::new(
                ErrorKind::ProtocolError,
                "消息响应与请求的账号或会话不一致。",
            ));
        }
        if translate::has_unaligned_card_tables(&message) {
            self.fill_table_layouts(&mut message).await;
        }
        let unknown = translate::unknown_card_mentions(&message, raw);
        if !unknown.is_empty() {
            self.fill_card_mentions(&mut message, raw, &unknown).await;
        }
        Ok(message)
    }

    /// 记住的卡片 @ 对应的键：消息编号 + 对不上的编号。
    fn card_mention_key(message: &ImMessage, unknown: &[String]) -> String {
        format!("{}\u{1f}{}", message.reference.id, unknown.join(","))
    }

    fn remembered_card_mentions(
        &self,
        message: &ImMessage,
        unknown: &[String],
    ) -> Option<std::sync::Arc<[(String, ActorRef)]>> {
        let key = Self::card_mention_key(message, unknown);
        self.card_mentions
            .lock()
            .expect("飞书卡片 @ 对应锁中毒：另一线程带着锁崩溃")
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, pairs)| pairs.clone())
    }

    fn remember_card_mentions(&self, key: String, pairs: std::sync::Arc<[(String, ActorRef)]>) {
        let mut remembered = self
            .card_mentions
            .lock()
            .expect("飞书卡片 @ 对应锁中毒：另一线程带着锁崩溃");
        remembered.retain(|(k, _)| *k != key);
        if remembered.len() == CARD_MENTIONS_REMEMBERED {
            remembered.pop_front();
        }
        remembered.push_back((key, pairs));
    }

    /// 卡片里的 @ 是发卡片那个应用的编号、对不上消息的 `mentions`（契约 client v31 §2 第 3 条）：带 `raw_card_content`
    /// 再读一次这条消息，按 `json_attachment.at_users` 对到 `mentions`，换成能认的编号与名字。读过的按消息记住；
    /// 读不到时照原样、只记 warn，断网、限流、授权失效下次再试，其余记成“没有能用的”不再请求。`raw` 是平台给的这条消息。
    async fn fill_card_mentions(&self, message: &mut ImMessage, raw: &Value, unknown: &[String]) {
        let key = Self::card_mention_key(message, unknown);
        let (pairs, fetched) = match self.remembered_card_mentions(message, unknown) {
            Some(pairs) => (pairs, false),
            None => match self.read_card_at_users(&message.reference).await {
                Ok(at_users) => {
                    let mentions = raw.get("mentions").and_then(Value::as_array);
                    let pairs: std::sync::Arc<[(String, ActorRef)]> =
                        translate::card_at_users(&at_users, mentions, unknown).into();
                    self.remember_card_mentions(key, pairs.clone());
                    (pairs, true)
                }
                Err(error) => {
                    tracing::warn!(message = %message.reference.id, %error, "补读卡片里 @ 的对应失败，这些 @ 照原样显示");
                    if !matches!(
                        error.kind,
                        ErrorKind::Disconnected | ErrorKind::RateLimited | ErrorKind::AuthExpired
                    ) {
                        self.remember_card_mentions(key, std::sync::Arc::from([]));
                    }
                    return;
                }
            },
        };
        let replaced = translate::replace_card_mentions(message, &pairs);
        if fetched {
            tracing::debug!(message = %message.reference.id, unknown = unknown.len(), replaced, "读了一次飞书内部结构，卡片里的 @ 对上了人");
        }
    }

    /// 记住的表格排法的键：消息编号 + 还没对齐的表格表头。
    fn table_layout_key(message: &ImMessage) -> String {
        format!(
            "{}\u{1f}{:?}",
            message.reference.id,
            translate::unaligned_table_headers(message)
        )
    }

    fn remembered_table_layouts(
        &self,
        message: &ImMessage,
    ) -> Option<std::sync::Arc<[card::TableLayout]>> {
        let key = Self::table_layout_key(message);
        self.table_layouts
            .lock()
            .expect("飞书卡片表格排法锁中毒：另一线程带着锁崩溃")
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, layouts)| layouts.clone())
    }

    fn remember_table_layouts(&self, key: String, layouts: std::sync::Arc<[card::TableLayout]>) {
        let mut remembered = self
            .table_layouts
            .lock()
            .expect("飞书卡片表格排法锁中毒：另一线程带着锁崩溃");
        remembered.retain(|(k, _)| *k != key);
        if remembered.len() == CARD_TABLE_LAYOUTS_REMEMBERED {
            remembered.pop_front();
        }
        remembered.push_back((key, layouts));
    }

    /// 卡片 2.0 的 Markdown 表格读回时丢了对齐（契约 client v19 §2）：带 `raw_card_content` 再读一次这条消息，
    /// 按飞书客户端自己用的结构补上每列对齐与每页行数。读过的按消息记住（复核窗口里反复读到同一条不再请求）；
    /// 读不到时照原样、只记 warn，断网、限流这类下次再试，其余记成“没有能用的”不再请求。
    async fn fill_table_layouts(&self, message: &mut ImMessage) {
        let key = Self::table_layout_key(message);
        // 记住过的直接用，不记日志（复核窗口里每隔几秒就会再读到同一条）；只有真的向飞书读了一次才记。
        let (layouts, fetched) = match self.remembered_table_layouts(message) {
            Some(layouts) => (layouts, false),
            None => match self.read_table_layouts(&message.reference).await {
                Ok(layouts) => {
                    let layouts: std::sync::Arc<[card::TableLayout]> = layouts.into();
                    self.remember_table_layouts(key, layouts.clone());
                    (layouts, true)
                }
                Err(error) => {
                    tracing::warn!(message = %message.reference.id, %error, "补读卡片表格的对齐失败，这些表格按没标对齐显示");
                    if !matches!(
                        error.kind,
                        ErrorKind::Disconnected | ErrorKind::RateLimited | ErrorKind::AuthExpired
                    ) {
                        self.remember_table_layouts(key, std::sync::Arc::from([]));
                    }
                    return;
                }
            },
        };
        let applied = translate::apply_table_layouts(message, &layouts);
        if fetched {
            tracing::debug!(message = %message.reference.id, tables = layouts.len(), applied, "读了一次飞书内部结构，卡片表格补上了对齐");
        }
    }

    /// 读一条消息飞书内部结构里的表格排法（`raw_card_content` 的 `json_card`）。
    async fn read_table_layouts(&self, reference: &MessageRef) -> ImResult<Vec<card::TableLayout>> {
        let content = self.read_raw_card(reference).await?;
        let json_card = content
            .get("json_card")
            .and_then(Value::as_str)
            .and_then(|card| serde_json::from_str::<Value>(card).ok())
            .ok_or_else(|| {
                ImError::new(
                    ErrorKind::ProtocolError,
                    "卡片内部结构认不出（没有 json_card）。",
                )
            })?;
        Ok(card::table_layouts(&json_card))
    }

    /// 读一条消息飞书内部结构里卡片 @ 的对应（`raw_card_content` 的 `json_attachment.at_users`：卡片里的编号 →
    /// 名字与 `mention_key`，2026-10-04 取样）；没有这一项时是空的。
    async fn read_card_at_users(
        &self,
        reference: &MessageRef,
    ) -> ImResult<serde_json::Map<String, Value>> {
        let content = self.read_raw_card(reference).await?;
        Ok(content
            .pointer("/json_attachment/at_users")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default())
    }

    /// 带 `raw_card_content` 读一条消息，取出卡片内容（`body.content` 解开的对象）；核对是请求的那条消息。
    async fn read_raw_card(&self, reference: &MessageRef) -> ImResult<Value> {
        let data = self
            .get(
                &["open-apis", "im", "v1", "messages", &reference.id],
                &[(RAW_CARD_PARAM.0.into(), RAW_CARD_PARAM.1.into())],
            )
            .await?;
        let raw = data
            .get("items")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .ok_or_else(|| ImError::new(ErrorKind::NotFound, "平台没有返回这条消息。"))?;
        if raw.get("message_id").and_then(Value::as_str) != Some(reference.id.as_str()) {
            return Err(ImError::new(
                ErrorKind::ProtocolError,
                "卡片内部结构的响应不是请求的那条消息。",
            ));
        }
        raw.pointer("/body/content")
            .and_then(Value::as_str)
            .and_then(|content| serde_json::from_str::<Value>(content).ok())
            .filter(Value::is_object)
            .ok_or_else(|| {
                ImError::new(
                    ErrorKind::ProtocolError,
                    "卡片内部结构认不出（内容不是对象）。",
                )
            })
    }

    /// 原始卡片认不出结构时，不带原始卡片参数再读一次这条消息，按飞书简化过的内容翻译。
    /// 读不到、或简化内容也翻不出卡片时返回 None，调用方保持暂不支持；补读失败不影响这一页。
    async fn simplified_card(&self, reference: &MessageRef) -> Option<ImMessage> {
        let data = match self
            .get(&["open-apis", "im", "v1", "messages", &reference.id], &[])
            .await
        {
            Ok(data) => data,
            Err(error) => {
                tracing::warn!(%error, message = %reference.id, "卡片简化内容补读失败，按暂不支持显示");
                return None;
            }
        };
        let raw = data
            .get("items")
            .and_then(Value::as_array)
            .and_then(|a| a.first())?;
        match translate::message(&self.account.connection_id, raw) {
            Ok(message)
                if message.reference == *reference
                    && matches!(message.content.as_slice(), [ImContent::Card(_)]) =>
            {
                Some(message)
            }
            Ok(_) => {
                tracing::debug!(message = %reference.id, "卡片简化内容也翻不出，按暂不支持显示");
                None
            }
            Err(error) => {
                tracing::warn!(%error, message = %reference.id, "卡片简化内容格式不对，按暂不支持显示");
                None
            }
        }
    }

    /// 以本人身份发送或回复；不自动重试可能已生效的请求。
    pub async fn send(&self, request: &SendRequest) -> ImResult<SendOutcome> {
        self.grant.ensure_current()?;
        if request.operation_id.is_empty()
            || request.operation_id.len() > OPERATION_ID_MAX_BYTES
            || !request
                .operation_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "操作编号须为不超过 50 字节的字母、数字、短横线或下划线。",
            ));
        }
        let (msg_type, content) = translate::outgoing(&request.content)?;
        let (mut url, body, expected) = match &request.target {
            SendTarget::Conversation { conversation } => {
                self.check_conversation(conversation)?;
                (
                    self.url(&["open-apis", "im", "v1", "messages"])?,
                    json!({"receive_id":conversation.id,"msg_type":msg_type,"content":content,"uuid":request.operation_id}),
                    conversation,
                )
            }
            SendTarget::Reply {
                message, in_thread, ..
            } => {
                // 平台回复 URL 只含 message_id，先核验会话以免引用串到其它目标。
                self.get_message(message).await?;
                (
                    self.url(&["open-apis", "im", "v1", "messages", &message.id, "reply"])?,
                    json!({"msg_type":msg_type,"content":content,"uuid":request.operation_id,"reply_in_thread":in_thread}),
                    &message.conversation,
                )
            }
        };
        if matches!(&request.target, SendTarget::Conversation { .. }) {
            url.query_pairs_mut()
                .append_pair("receive_id_type", "chat_id");
        }
        let uncertain = || {
            tracing::warn!("飞书发送结果不确定，禁止盲目重发");
            SendOutcome::Unknown {
                operation_id: request.operation_id.clone(),
                reason: "请求可能已生效，但未取得可核验回执；请在原会话核对，勿直接重发。".into(),
            }
        };
        let response = match self
            .http
            .post(url)
            .bearer_auth(self.grant.access_token())
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(error) => {
                let cause = crate::net::network_cause(error);
                tracing::warn!(%cause, "发送消息时网络出错，发送结果未确认");
                return Ok(uncertain());
            }
        };
        if response.status().is_server_error()
            || response.status() == reqwest::StatusCode::REQUEST_TIMEOUT
        {
            return Ok(uncertain());
        }
        let raw = match read_json(response).await {
            Ok(v) => v,
            Err(e) if e.platform_code.is_some() => return Err(e),
            Err(_) => return Ok(uncertain()),
        };
        let message = match raw
            .get("data")
            .map(|v| translate::message(&self.account.connection_id, v))
        {
            Some(Ok(m)) => m,
            _ => return Ok(uncertain()),
        };
        if message.reference.conversation != *expected {
            return Ok(uncertain());
        }
        tracing::info!("飞书已接受本人消息");
        Ok(SendOutcome::Accepted {
            operation_id: request.operation_id.clone(),
            message: Box::new(message),
        })
    }

    /// 以本人身份撤回自己发出的一条消息。
    ///
    /// 撤回接口的地址只含消息编号，先核验消息属于这个会话，免得撤回串到别处。
    /// 撤回的时限由企业管理员设置，超出时返回 [`ErrorKind::LimitExceeded`]；不自动重试。
    ///
    /// # Errors
    /// 消息不属于当前连接、不存在、超出撤回时限、不是本人的消息或平台拒绝时返回错误。
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example(client: &hhh_im_feishu::FeishuClient, message: hhh_im_feishu::MessageRef) -> hhh_im_feishu::ImResult<()> {
    /// client.recall(&message).await?;
    /// # Ok(()) }
    /// ```
    pub async fn recall(&self, message: &MessageRef) -> ImResult<()> {
        self.check_conversation(&message.conversation)?;
        validate_id(&message.id)?;
        self.get_message(message).await?;
        self.query_data(self.http.delete(self.url(&[
            "open-apis",
            "im",
            "v1",
            "messages",
            &message.id,
        ])?))
        .await?;
        tracing::info!("飞书已撤回本人消息");
        Ok(())
    }

    /// 明确报告无法按操作编号可靠查询发送结果。
    pub async fn lookup_send(&self, _operation_id: &str) -> ImResult<SendOutcome> {
        Err(ImError::new(
            ErrorKind::Unsupported,
            "飞书未提供本封装可用的 uuid 发送查询；已有消息 ID 可用 get_message 核验，未知结果请在原会话确认。",
        ))
    }
}

/// 有总量限制的附件读取流，释放即取消后续下载。
pub struct ResourceDownload {
    body: Body,
    remaining: usize,
    ended: bool,
}

enum Body {
    Http(Response),
    /// 已经在内存里的数据（如表情图），只有一块。
    Bytes(Option<Vec<u8>>),
}

impl ResourceDownload {
    // 平台适配器先校验自己的响应，再复用统一的有界 HTTP 字节流。
    pub(crate) fn new(response: Response, max_bytes: usize) -> Self {
        Self {
            body: Body::Http(response),
            remaining: max_bytes,
            ended: false,
        }
    }

    /// 把已经在内存里的数据当成只有一块的流（平台接口本来就整份返回时用）。
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self {
            remaining: bytes.len(),
            body: Body::Bytes(Some(bytes)),
            ended: false,
        }
    }

    /// 取下一块原始字节；到达末尾返回 None，超限后不继续读。
    pub async fn next_chunk(&mut self) -> ImResult<Option<Vec<u8>>> {
        if self.ended {
            return Ok(None);
        }
        let response = match &mut self.body {
            Body::Http(response) => response,
            Body::Bytes(bytes) => {
                let chunk = bytes.take();
                self.ended = chunk.is_none();
                return Ok(chunk);
            }
        };
        let result = response.chunk().await;
        match result {
            Ok(Some(chunk)) if chunk.len() <= self.remaining => {
                self.remaining -= chunk.len();
                Ok(Some(chunk.to_vec()))
            }
            Ok(Some(_)) => {
                self.ended = true;
                Err(ImError::new(
                    ErrorKind::LimitExceeded,
                    "附件实际内容超过读取预算。",
                ))
            }
            Ok(None) => {
                self.ended = true;
                Ok(None)
            }
            Err(error) => {
                self.ended = true;
                Err(ImError::network(error, "附件读取中断。"))
            }
        }
    }
}

pub(crate) fn optional(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}
fn validate_id(id: &str) -> ImResult<()> {
    if id.is_empty()
        || id.len() > PLATFORM_ID_MAX_BYTES
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(ImError::new(
            ErrorKind::InvalidRequest,
            "平台对象编号格式不正确。",
        ));
    }
    Ok(())
}
pub(crate) fn required(v: &Value, key: &str) -> ImResult<String> {
    optional(v, key).ok_or_else(|| {
        ImError::new(
            ErrorKind::ProtocolError,
            format!("飞书响应缺少字段 {key}。"),
        )
    })
}
fn add_cursor(
    params: &mut Vec<(String, String)>,
    cursor: Option<&PageCursor>,
    scope: &str,
) -> ImResult<()> {
    if let Some(cursor) = cursor {
        if cursor.scope != scope {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "分页游标不属于当前账号、会话或查询条件。",
            ));
        }
        params.push(("page_token".into(), cursor.token.clone()));
    }
    Ok(())
}
fn page_cursor(data: &Value, scope: String) -> ImResult<Option<PageCursor>> {
    if data.get("has_more").and_then(Value::as_bool) == Some(true) {
        Ok(Some(PageCursor {
            token: required(data, "page_token")?,
            scope,
        }))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_reject_cleartext_remote_hosts_and_credentials() {
        for endpoint in [
            "http://example.com",
            "https://user:secret@example.com",
            "ftp://example.com",
        ] {
            let config = FeishuConfig {
                api_base: endpoint.into(),
                ..Default::default()
            };
            assert!(config.validate().is_err());
        }
        assert!(
            FeishuConfig {
                api_base: "http://127.0.0.1:1234".into(),
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
        assert!(
            FeishuConfig {
                api_base: "http://[::1]:1234".into(),
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
    }

    #[tokio::test]
    async fn resource_stream_stops_after_actual_byte_budget_is_exceeded() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("too-large"))
            .mount(&server)
            .await;
        let response = http_client()
            .unwrap()
            .get(server.uri())
            .send()
            .await
            .unwrap();
        let mut stream = ResourceDownload::new(response, 2);
        assert_eq!(
            stream.next_chunk().await.unwrap_err().kind,
            ErrorKind::LimitExceeded
        );
        assert!(stream.next_chunk().await.unwrap().is_none());
    }
}
