//! 上层只依赖此接口和统一类型，平台装配发生在边界。
//!
//! 从 hhh-im 搬来时去掉了实时推送（`live`、`LiveEvent`、`LiveFeed`）：那是宿主同步层收推送用的，飞书不推送。

use crate::*;
use std::future::Future;

/// 单次查询最多等多久（契约 hhh-im client v9）：读新消息、话题、表情、群成员这类小查询，正常 0.3～0.7 秒。
/// 网络卡住（例如复用了已被中间设备断掉的旧连接）时，与其等满发送用的 30 秒，不如早点放弃换条连接；
/// hub 把“用时达到这个值才失败”的读取认作卡住，立刻重读一次。发送、上传、下载不用它。
pub(crate) const QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// [`ImClient::look_up_people`] 的默认实现估算请求数时，按一次请求读这么多人算（批量读用户资料的接口
/// 常见的每批上限）；平台自己实现时报实际的请求数。
const DEFAULT_PEOPLE_PER_REQUEST: usize = 50;

/// 已绑定本人账号的 IM 统一接口。
///
/// 网络方法返回可在线程间调度的 Future；接入方只负责数据获取与发送。
/// 事件传输由宿主装配，不把用户关注或 Agent 生命周期混入客户端。
pub trait ImClient: Send + Sync {
    /// 已核验的本人身份。
    fn account(&self) -> &Account;
    /// 声明的权限与平台能力边界。
    fn capabilities(&self) -> CapabilitySet;
    /// 分页列出当前接口可发现的会话。
    fn list_conversations(
        &self,
        limit: u16,
        cursor: Option<&PageCursor>,
    ) -> impl Future<Output = ImResult<Page<Conversation>>> + Send;
    /// 按群名关键词分页搜索；可见范围与匹配语义由平台决定。
    fn search_conversations(
        &self,
        query: &SearchQuery,
    ) -> impl Future<Output = ImResult<SearchPage<Conversation>>> + Send;
    /// 按用户名关键词分页搜索，不推断好友关系。
    fn search_users(
        &self,
        query: &SearchQuery,
    ) -> impl Future<Output = ImResult<SearchPage<SearchUser>>> + Send;
    /// 只读定位对方曾发来消息的既有单聊；不创建会话或发送消息。
    fn find_direct_conversation(
        &self,
        user: &UserRef,
    ) -> impl Future<Output = ImResult<DirectConversationLookup>> + Send;
    /// 获取指定会话。
    fn get_conversation(
        &self,
        conversation: &ConversationRef,
    ) -> impl Future<Output = ImResult<Conversation>> + Send;
    /// 分页读取消息，页内从旧到新。
    fn list_messages(
        &self,
        query: &MessageQuery,
    ) -> impl Future<Output = ImResult<Page<ImMessage>>> + Send;
    /// 补读指定消息。
    fn get_message(&self, message: &MessageRef)
    -> impl Future<Output = ImResult<ImMessage>> + Send;
    /// 分页读取指定消息的现有表情；不保证捕获两次查询间的短暂变化。
    fn list_reactions(
        &self,
        query: &ReactionQuery,
    ) -> impl Future<Output = ImResult<Page<MessageReaction>>> + Send;
    /// 以绑定的本人账号发送或回复。
    fn send(&self, request: &SendRequest) -> impl Future<Output = ImResult<SendOutcome>> + Send;
    /// 平台可支持时按操作编号查询。
    fn lookup_send(&self, operation_id: &str)
    -> impl Future<Output = ImResult<SendOutcome>> + Send;
    /// 打开消息资源的一段（契约 hhh-im client v10 §9）：按块读的流，尽早给出总大小与内容指纹。
    ///
    /// 排队、并发、超时、重试与存放都由下载层负责，适配器只管打开与错误分级；
    /// 会话归属由调用方核对（本地库有这条消息时以本地为准）。
    fn open_resource(
        &self,
        resource: &ResourceRef,
        range: ResourceRange,
    ) -> impl Future<Output = ImResult<OpenedResource>> + Send;
    /// 打开一个头像（平台 CDN 上的图片）；只接受平台自己的头像地址。
    fn open_avatar(&self, url: &str) -> impl Future<Output = ImResult<OpenedResource>> + Send;
    /// 分页读取群成员；平台不给头像时为空。
    fn list_members(
        &self,
        conversation: &ConversationRef,
        limit: u16,
        cursor: Option<&PageCursor>,
    ) -> impl Future<Output = ImResult<Page<SearchUser>>> + Send;
    /// 按编号批量读取用户的名字与头像；平台没返回的人不出现。
    fn users(&self, users: &[UserRef]) -> impl Future<Output = ImResult<Vec<SearchUser>>> + Send;
    /// 人名册按编号读一批人与机器人（契约 client v23 §3、§4）：读到的、平台明确说没有的分开给，这次没读的
    /// 两边都不在（下次再读）。默认按 [`ImClient::users`] 读人：没返回的人算没有；机器人不会读，一律算没有。
    /// 能读机器人、或有更省的读法的平台自己实现。
    fn look_up_people(
        &self,
        people: &[PersonKey],
    ) -> impl Future<Output = ImResult<PeopleLookup>> + Send {
        async move {
            let connection_id = &self.account().connection_id;
            let users: Vec<UserRef> = people
                .iter()
                .filter(|p| !p.bot)
                .map(|p| UserRef {
                    connection_id: connection_id.clone(),
                    id: p.id.clone(),
                })
                .collect();
            let found = if users.is_empty() {
                Vec::new()
            } else {
                self.users(&users).await?
            };
            let missing = people
                .iter()
                .filter(|p| p.bot || !found.iter().any(|f| f.reference.id == p.id))
                .map(|p| p.id.clone())
                .collect();
            Ok(PeopleLookup {
                found: found.into_iter().map(PersonProfile::from).collect(),
                missing,
                requests: u32::try_from(users.len().div_ceil(DEFAULT_PEOPLE_PER_REQUEST))
                    .unwrap_or(u32::MAX),
            })
        }
    }
    /// 上传一个要发送的图片或文件，返回平台资源编号。
    fn upload_resource(
        &self,
        kind: ResourceKind,
        name: &str,
        bytes: Vec<u8>,
    ) -> impl Future<Output = ImResult<String>> + Send;
    /// 以本人身份撤回自己发出的一条消息；时限与权限由平台裁决，不自动重试。
    fn recall(&self, message: &MessageRef) -> impl Future<Output = ImResult<()>> + Send;
    /// 以本人身份编辑自己的一条消息（契约 client v11 §3.6），新内容是文字与提及；`message` 是本地记着的这条，
    /// 返回平台编辑后的这条消息。时限与权限由平台裁决，不自动重试。平台不能以本人身份编辑时（飞书）用默认的，
    /// 返回 [`ErrorKind::Unsupported`]。
    fn edit(
        &self,
        message: &ImMessage,
        content: &[OutgoingContent],
    ) -> impl Future<Output = ImResult<ImMessage>> + Send {
        let _ = (message, content);
        async {
            Err(ImError::new(
                ErrorKind::Unsupported,
                "这个平台不能以本人身份编辑消息。",
            ))
        }
    }
    /// 列出 `query.since_ms` 之后有人（不算机器人与应用）发过消息的会话（契约 client v29 §2），按最新一条从新到旧，
    /// 一个会话一项；只读，一次最多翻 `query.pages_max` 页。平台搜索返回的内容只用来认会话，不交出来。
    /// 不能这样查的平台用默认的，返回 [`ErrorKind::Unsupported`]（平台事实里的会话发现也为空）。
    fn recent_activity(
        &self,
        query: &ActivityQuery,
    ) -> impl Future<Output = ImResult<RecentActivity>> + Send {
        let _ = query;
        async {
            Err(ImError::new(
                ErrorKind::Unsupported,
                "这个平台不能查最近有新消息的会话。",
            ))
        }
    }
    /// 以本人身份给消息点一个表情，返回平台记下的这条表情。
    fn add_reaction(
        &self,
        message: &MessageRef,
        emoji_type: &str,
    ) -> impl Future<Output = ImResult<MessageReaction>> + Send;
    /// 以本人身份删掉自己点的一个表情（平台给的表情编号）。
    fn remove_reaction(
        &self,
        message: &MessageRef,
        reaction_id: &str,
    ) -> impl Future<Output = ImResult<()>> + Send;
    /// 平台当前可用于表情回复的表情目录。
    fn emoji_catalog(&self) -> impl Future<Output = ImResult<EmojiCatalog>> + Send;
    /// 下载一个表情的官方图片。
    fn emoji_image(&self, emoji: &EmojiInfo) -> impl Future<Output = ImResult<Vec<u8>>> + Send;
    /// 翻译规则改了、本地库里已存的旧消息需要一次性修正时给出修正（本地改写，不访问平台），按顺序执行；默认没有。
    fn content_repairs(&self) -> Vec<ContentRepair> {
        Vec::new()
    }
    /// 翻译规则改了、本地库里已存的某类旧消息要重新向平台读一次时给出规则；默认没有。
    fn content_rereads(&self) -> Vec<ContentReread> {
        Vec::new()
    }
    /// 按一条重译规则把这批存着原始数据的旧消息重新翻译一遍（外部插件的 `messages/retranslate`，契约 provider-protocol v12
    /// 缺口 4），不联网；只回这批里的消息。默认不支持。
    fn retranslate(
        &self,
        rule: &str,
        items: &[RetranslateItem],
    ) -> impl Future<Output = ImResult<Vec<ImMessage>>> + Send {
        let _ = (rule, items);
        async {
            Err(ImError::new(
                ErrorKind::Unsupported,
                "这个平台不能按原始数据重新翻译。",
            ))
        }
    }
}

/// 交给 [`ImClient::retranslate`] 的一条旧消息：编号与存着的原始数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetranslateItem {
    /// 消息。
    pub message: MessageRef,
    /// 存着的原始数据。
    pub raw: RawData,
}

/// 要读资源的哪一段：从 `start` 字节起，最多 `len` 字节（`None` 读到末尾）；默认整份。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResourceRange {
    /// 起始字节。
    pub start: u64,
    /// 最多读多少字节；`None` 读到末尾。
    pub len: Option<u64>,
}

impl ResourceRange {
    /// 是不是从头读整份。
    pub fn is_whole(&self) -> bool {
        self.start == 0 && self.len.is_none()
    }
}

/// 打开的一个资源（契约 hhh-im client v10 §9）。
pub struct OpenedResource {
    /// 按块读的数据。
    pub download: ResourceDownload,
    /// 整个资源的字节数（不是这一段）；平台没给时为空。
    pub total: Option<u64>,
    /// 平台在正文前给出的内容指纹（SHA-256，小写十六进制）；平台不给时为空。
    pub content_sha256: Option<String>,
}
