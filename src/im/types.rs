//! 不依赖平台 JSON 形状的公共消息与操作类型。

use serde::{Deserialize, Serialize};

/// 带账号连接范围的会话引用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationRef {
    /// 调用方分配的稳定连接编号。
    pub connection_id: String,
    /// 平台原生会话编号。
    pub id: String,
}
impl ConversationRef {
    /// 构造会话引用；客户端调用时会复核账号范围。
    pub fn new(connection_id: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            connection_id: connection_id.into(),
            id: id.into(),
        }
    }
}

/// 带会话和账号范围的消息引用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageRef {
    /// 消息所属会话。
    pub conversation: ConversationRef,
    /// 平台原生消息编号。
    pub id: String,
}

/// 发送者或被提及对象。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActorRef {
    /// 平台给出的身份编号；没有人物身份的系统通知可以为空。
    pub id: String,
    /// 编号命名空间（平台原样，如飞书的 open_id）；系统通知没有编号时为空。
    pub id_type: String,
    /// 身份种类：人与应用是平台原样的叫法（如 user、app）；系统通知固定为 [`SYSTEM_SENDER_KIND`]，各平台都遵守。
    pub kind: String,
    /// 可获得的显示名称。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// 通过用户凭据核验的本人身份。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    /// 稳定连接编号。
    pub connection_id: String,
    /// 平台名称。
    pub platform: String,
    /// 本人身份。
    pub user: ActorRef,
    /// 租户编号；未提供时不推断。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    /// 平台提供的头像地址。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
}

/// 会话的实际类别。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConversationKind {
    /// 多人群。
    Group,
    /// 双方单聊。
    Direct,
    /// 平台频道。
    Channel,
    /// 平台未提供可判定类别。
    Unknown,
}

/// 可展示的会话资料。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    /// 带连接范围的引用。
    pub reference: ConversationRef,
    /// 会话名称，可能为空。
    pub name: String,
    /// 会话类别。
    pub kind: ConversationKind,
    /// 平台提供的头像地址。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    /// 私聊的对方；群、或还认不出对方时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<UserRef>,
    /// 频道是私有的（Slack 私有频道，契约 client v22）；群、私聊、群私聊与认不出类别的恒为 false。
    /// 以前存下的会话资料没有这一项，读出为 false，下一次资料刷新补上。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub private: bool,
}

/// 带连接范围的用户引用；飞书适配器使用 open_id。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserRef {
    /// 所属的已核验账号连接。
    pub connection_id: String,
    /// 平台用户编号。
    pub id: String,
}

/// 用户资料：用户搜索或按编号批量读取的单项结果，不等同于好友关系或单聊访问权限。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchUser {
    /// 可交给私聊定位接口的用户引用。
    pub reference: UserRef,
    /// 平台返回的姓名。
    pub name: String,
    /// 平台提供的头像地址；飞书取 72px 规格，缺失时取 240px。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
}

/// 搜索每页最多 20 条，统一保守限制以控制终端与内存开销。
pub const SEARCH_PAGE_MAX: u16 = 20;
/// 关键词使用群搜索的官方上限，统一限制为 50 个字符。
pub const SEARCH_TEXT_MAX_CHARS: usize = 50;

/// 群名或用户名关键词查询；匹配语义由平台提供。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchQuery {
    /// 非空关键词，最多 50 个字符。
    pub text: String,
    /// 每页数量，封装统一限制为 1 至 20 条。
    pub limit: u16,
    /// 与连接、搜索种类、关键词和页大小绑定的游标。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<PageCursor>,
}
impl SearchQuery {
    /// 从第一页搜索，默认最多返回 20 条。
    ///
    /// # Examples
    ///
    /// ```
    /// let query = hhh_im_feishu::SearchQuery::new("研发");
    /// assert_eq!(query.text, "研发");
    /// assert_eq!(query.limit, 20);
    /// assert!(query.cursor.is_none());
    /// ```
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            limit: SEARCH_PAGE_MAX,
            cursor: None,
        }
    }
}

/// 一页搜索结果；平台的不完整结果提示保留给调用方。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchPage<T> {
    /// 当前页的有限结果。
    pub items: Vec<T>,
    /// 后续页；没有下一页时省略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<PageCursor>,
    /// 平台的搜索范围或结果完整性提示。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

/// 最近聊天最多返回的会话数；只读近期有界的消息，不遍历完整历史。
pub const RECENT_LIMIT_MAX: u16 = 20;

/// 最近聊天查询：从 `since`（Unix 秒）起，按最近一条消息时间取群与单聊。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentQuery {
    /// 最多返回的会话数，1 至 20。
    pub limit: u16,
    /// 只看这个时间之后的消息，Unix 秒。
    pub since: i64,
}
impl RecentQuery {
    /// 取 `since` 之后最近的 `limit` 个会话。
    ///
    /// # Examples
    ///
    /// ```
    /// let query = hhh_im_feishu::RecentQuery::new(10, 1_758_000_000);
    /// assert_eq!(query.limit, 10);
    /// assert_eq!(query.since, 1_758_000_000);
    /// ```
    pub fn new(limit: u16, since: i64) -> Self {
        Self { limit, since }
    }
}

/// 一个最近有过消息的会话；单聊的名称与头像是对方的资料。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentConversation {
    /// 可展示的会话资料，类别为 `Group` 或 `Direct`。
    pub conversation: Conversation,
    /// 该会话最近一条可见消息的时间，Unix 秒。
    pub last_active_at: i64,
    /// 单聊的对方；上层据此在之后按编号刷新对方资料。群为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<UserRef>,
}

/// 最近聊天结果；因权限不足或资料不可读而跳过的部分用提示说明，不静默丢弃。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentConversations {
    /// 按最近一条消息时间倒序的会话。
    pub items: Vec<RecentConversation>,
    /// 跳过部分会话时的说明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

/// 要找哪段时间里有人发过消息的会话（[`crate::ImClient::recent_activity`]，契约 client v29 §2）。
///
/// # Examples
///
/// ```
/// let query = hhh_im_feishu::ActivityQuery { since_ms: 1_791_000_000_000, pages_max: 10 };
/// assert_eq!(query.pages_max, 10);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivityQuery {
    /// 从这个时间（Unix 毫秒）起，含这一刻。
    pub since_ms: i64,
    /// 一次最多翻几页；翻满还没到起点时结果标为没翻完。
    pub pages_max: u16,
}

/// 一段时间里有人（不算机器人与应用）发过消息的一个会话。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveConversation {
    /// 会话。
    pub conversation: ConversationRef,
    /// 群还是私聊；平台给不出时为 [`ConversationKind::Unknown`]。
    pub kind: ConversationKind,
    /// 这段时间里最早一条的时间（Unix 毫秒）。
    pub first_ms: i64,
    /// 这段时间里最新一条的时间（Unix 毫秒）。
    pub latest_ms: i64,
    /// 私聊的对方：这段时间里认出的不是本人的发送者；只有本人发过、或是群时为空。
    pub peer: Option<UserRef>,
    /// 适配器手边就有的会话资料（Slack 缓存的会话列表）；为空时由调用方另读。
    pub profile: Option<Conversation>,
}

/// [`crate::ImClient::recent_activity`] 的结果：按最新一条从新到旧。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecentActivity {
    /// 有人发过消息的会话，每个会话一项。
    pub items: Vec<ActiveConversation>,
    /// 翻到了起点（或没有更多了）；翻满页数上限还没到时为假。
    pub complete: bool,
}

/// 从对方发来的可搜索消息中定位既有单聊；未找到不表示从未有过会话。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectConversationLookup {
    /// 已校验发送者和单聊类型的会话引用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<ConversationRef>,
    /// 平台的搜索补充提示。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

/// 仅属于消息的资源引用，不包含下载后的大文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceRef {
    /// 资源所属消息。
    pub message: MessageRef,
    /// 平台资源编号。
    pub key: String,
    /// 下载方式。
    pub kind: ResourceKind,
}

/// 平台消息资源下载种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResourceKind {
    /// 图片资源。
    Image,
    /// 文件、音频或视频资源。
    File,
    /// 表情包：单独一条消息的那张图（可能是动图）。显示、宽高、占用与清理都按图片，
    /// 只是平台的取法不同（飞书开放接口读不到，改从图片 CDN 读，契约 client v15 §1）。
    Sticker,
}

impl ResourceKind {
    /// 显示成一张图的资源（图片与表情包）：记宽高、整份读、计入图片占用。
    ///
    /// # Examples
    ///
    /// ```
    /// assert!(hhh_im_feishu::ResourceKind::Sticker.shows_as_image());
    /// assert!(!hhh_im_feishu::ResourceKind::File.shows_as_image());
    /// ```
    pub fn shows_as_image(self) -> bool {
        matches!(self, Self::Image | Self::Sticker)
    }
}

/// 图片按显示方向的宽高（像素）：照片的旋转方向已算进去，与浏览器显示出来的一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageSize {
    /// 宽。
    pub width: u32,
    /// 高。
    pub height: u32,
}

/// 视频文件的扩展名（小写）：平台没说是不是视频时，框架按它认定（契约 client v35 §1.2）。
/// 宿主认本机文件（伙伴附件、输入框里的文件）是不是视频时用同一份清单。
pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mov", "m4v", "webm", "mkv", "avi", "3gp", "wmv", "flv", "mpg", "mpeg",
];

/// 文件名的扩展名是不是视频。
///
/// # Examples
///
/// ```
/// assert!(hhh_im_feishu::is_video_name("彩排 录屏.MP4"));
/// assert!(hhh_im_feishu::is_video_name("IMG_3856.MOV"));
/// assert!(!hhh_im_feishu::is_video_name("mp4"));
/// assert!(!hhh_im_feishu::is_video_name("报价.pdf"));
/// ```
pub fn is_video_name(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty() && VIDEO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
    })
}

/// MIME 是不是视频（`video/*`）。
///
/// # Examples
///
/// ```
/// assert!(hhh_im_feishu::is_video_mime("video/mp4"));
/// assert!(hhh_im_feishu::is_video_mime("Video/QuickTime"));
/// assert!(!hhh_im_feishu::is_video_mime("audio/mp4"));
/// ```
pub fn is_video_mime(mime: &str) -> bool {
    mime.get(..6)
        .is_some_and(|head| head.eq_ignore_ascii_case("video/"))
}

/// 视频资源的信息（契约 client v35）：带了它的文件资源就是视频；资源种类仍是文件，下载、只下一次、
/// 占用、清理与本机原件规则都按文件。平台给了什么填什么，没给的为空，不编造。
///
/// # Examples
///
/// ```
/// let info = hhh_im_feishu::VideoInfo { duration_ms: Some(3400), ..Default::default() };
/// let json = serde_json::to_value(&info).unwrap();
/// assert_eq!(json, serde_json::json!({ "durationMs": 3400 }));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoInfo {
    /// 封面：同一条消息里的一张图片，与消息图片一样下载、计入图片占用、第一次读到记下宽高。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<VideoCover>,
    /// 时长（毫秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// 平台给的画面宽高（按显示方向）；不知道时界面按封面比例或 16:9。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<ImageSize>,
}

/// 视频的封面：一个图片资源及它的显示宽高（宽高与消息图片一样存在 resource_sizes，读消息时带上）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoCover {
    /// 封面图片（种类是图片，属于视频所在的那条消息）。
    pub resource: ResourceRef,
    /// 封面的显示宽高；还没读过时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<ImageSize>,
}

/// 平台翻译规则改了之后，要把本地库里已存的某类旧消息重新向平台读一次（按规则名字，每条只读一次）。
///
/// 本地库只存统一模型、不存平台原始响应，有些旧记录没法在本地修正（例如当时整张卡片只记成“暂不支持”）。
/// 由平台客户端给出（[`crate::ImClient::content_rereads`]）；消息出现在打开会话的视野里时，hub 重新读一次、
/// 按新规则翻译并覆盖本地记录。读过的按规则名字记在库里，仍看不懂也不再读，规则换了新名字才再读。
#[derive(Debug, Clone, Copy)]
pub struct ContentReread {
    /// 规则的名字；读过的消息按名字记下，不重复读。
    pub name: &'static str,
    /// 只检查消息记录里含这段文字的，免得逐条解析。
    pub contains: &'static str,
    /// 这条已存的消息要不要按这个规则重读。
    pub applies: fn(&ImMessage) -> bool,
}

/// 一个 IM 平台给界面的叫法、链接与限制：界面拼“在飞书查看”、打开会话、图片文件名与选附件时用它，
/// 不写死平台名与平台的数值。它是平台说明里给界面的那部分（飞书的见 [`crate::facts::INFO`]）。
///
/// # Examples
///
/// ```
/// let feishu = hhh_im_feishu::facts::INFO;
/// assert_eq!(feishu.name, "飞书");
/// assert_eq!(feishu.chat_link("oc_a b"), "https://applink.feishu.cn/client/chat/open?openChatId=oc_a%20b");
/// assert_eq!(feishu.file_upload_max_bytes, 30 * 1024 * 1024);
/// assert_eq!(feishu.unlisted_emoji.placeholder_label, "企业表情");
/// assert_eq!(feishu.avatar_shape, hhh_im_feishu::AvatarShape::Circle);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformInfo {
    /// 平台编号，与账号、关注里记的平台一致。
    pub id: &'static str,
    /// 给用户看的平台名。
    pub name: &'static str,
    /// 在平台客户端里打开一个会话的链接；`{conversation}` 处换成 URL 编码后的会话编号。
    pub chat_link_template: &'static str,
    /// 下载图片时文件名的开头，后面由界面接日期与时间。
    pub image_name_prefix: &'static str,
    /// 发送的图片最大多少字节。
    pub image_upload_max_bytes: usize,
    /// 发送的文件最大多少字节。
    pub file_upload_max_bytes: usize,
    /// 发送的附件文件名最长多少字节（UTF-8）。
    pub attachment_name_max_bytes: usize,
    /// 表情目录里没有的表情怎么显示。
    pub unlisted_emoji: UnlistedEmoji,
    /// 回复话题时能不能同时发送到会话（契约 client v18 §2.1）；不能时为空。
    pub thread_broadcast: Option<ThreadBroadcast>,
    /// 这个平台自己的客户端里头像是什么形状（契约 client v24）：每个平台在自己的平台说明里给，没有默认值。
    /// 只说形状，圆角多大、哪些位置照它画归宿主的界面规范。
    pub avatar_shape: AvatarShape,
    /// 正文里表情的写法（JavaScript 正则，第一个括号是表情名，契约 provider-protocol v12 缺口 9）：界面把正文里这样写的
    /// 换成表情图；为空时不换。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emoji_text_pattern: Option<&'static str>,
    /// 表情图是白底的：界面显示前抠成透明（深色主题下不露白底）；为假时原样显示。
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub emoji_white_background: bool,
    /// 这个 IM 的标志（契约 friend/im-list v2）：内置平台的模块随代码带着自己的标志文件；外部插件的标志写在它的
    /// 清单里（`logo`），由宿主从插件文件夹读，这里为空。宿主把它和插件的一起放进给界面的 IM 清单，不随平台说明发给界面。
    #[serde(skip)]
    pub logo: Option<PlatformLogo>,
}

/// 一个 IM 的标志：图片与它的画法（契约 friend/im-list v2 §2）。宿主按画法画，不按平台编号判断。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformLogo {
    /// 图片格式（`image/svg+xml`、`image/png`…）。
    pub media_type: &'static str,
    /// 图片本身。
    pub bytes: &'static [u8],
    /// 画法。
    pub style: LogoStyle,
    /// 图形在画布里的外框（SVG 的 viewBox 坐标），宿主按“IM 平台标志的视觉尺寸”核对占比；不是 SVG 的为空。
    pub ink: Option<LogoInk>,
}

/// 标志的画法。
///
/// # Examples
///
/// ```
/// use hhh_im_feishu::{facts, LogoStyle};
///
/// assert_eq!(facts::INFO.logo.unwrap().style, LogoStyle::Mark);
/// assert_eq!(serde_json::to_value(LogoStyle::Icon).unwrap(), "icon");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LogoStyle {
    /// 透明底的标志图形（飞书、Slack）：宿主垫上自己的底。
    Mark,
    /// 自带底色的完整应用图标（插件清单里的 logo）：原样铺满标志位。
    Icon,
}

/// 标志图形在自己画布里的外框（viewBox 坐标）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogoInk {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

// 外框只用来核对，不会是 NaN。
impl Eq for LogoInk {}

/// 平台客户端里头像的形状（契约 client v24）：宿主照它画，不按平台编号判断。
///
/// # Examples
///
/// ```
/// use hhh_im_feishu::{facts, AvatarShape};
///
/// assert_eq!(facts::INFO.avatar_shape, AvatarShape::Circle);
/// assert_eq!(serde_json::to_value(AvatarShape::Rounded).unwrap(), "rounded");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AvatarShape {
    /// 圆形（飞书）。
    Circle,
    /// 圆角方形（Slack）。
    Rounded,
}

/// 平台怎么做“回复话题时同时发送到会话”。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadBroadcast {
    /// 平台原生支持：一条消息话题与会话两处都有（Slack）；为假时内核发两次（飞书）。
    pub native: bool,
    /// 带图片、文件的回复也能同时发送。
    pub attachments: bool,
}

/// 表情目录里没有的表情怎么显示（契约 client v17 §7）：界面照这组规则决定去不去找图、画不画占位图标，
/// 不自己认平台编号的样子。规则是正则（JavaScript 语法，界面直接用）；为空表示不做这一步。
///
/// # Examples
///
/// ```
/// let rules = hhh_im_feishu::facts::INFO.unlisted_emoji;
/// assert_eq!(rules.placeholder_pattern, Some("^[0-9]+$"));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnlistedEmoji {
    /// 能按平台的命名规律去找图的表情类型；与平台说明的猜图（飞书的 [`crate::facts::guess_emoji_images`]）
    /// 给出地址的类型一致。平台不猜图时为空。
    pub guess_pattern: Option<&'static str>,
    /// 找不到图也没有名字时画成占位图标的类型（飞书的企业表情是一串数字）；为空时都写类型名。
    pub placeholder_pattern: Option<&'static str>,
    /// 占位图标在读屏与悬停里的叫法。
    pub placeholder_label: &'static str,
}

impl PlatformInfo {
    /// 在平台客户端里打开这个会话的链接。
    pub fn chat_link(&self, conversation_id: &str) -> String {
        let mut encoded = String::with_capacity(conversation_id.len());
        for byte in conversation_id.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                encoded.push(char::from(byte));
            } else {
                encoded.push_str(&format!("%{byte:02X}"));
            }
        }
        self.chat_link_template.replace("{conversation}", &encoded)
    }
}

/// 平台翻译规则改了之后，对本地库里已存消息做的一次性修正（按名字只做一次）。
///
/// 由平台客户端给出（[`crate::ImClient::content_repairs`]），本地库打开时执行；
/// 本地库只管“找出候选、交给 `rewrite`、写回并记下做过”，不懂平台格式。
#[derive(Debug, Clone, Copy)]
pub struct ContentRepair {
    /// 修正的名字；做过的记在库里，不重复做。
    pub name: &'static str,
    /// 只检查消息记录里含这段文字的，免得逐条解析整个库。
    pub contains: &'static str,
    /// 给出一条消息修正后的样子（内容，或编辑时间这类消息事实，契约 client v26 §3）；不需要改时返回 None。
    pub rewrite: fn(&ImMessage) -> Option<ImMessage>,
}

/// 一条消息内可表达的内容。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ImContent {
    /// 可读文字。没有格式也没有链接的消息用它；有链接或格式时整条消息改用 [`ImContent::Rich`]。
    Text {
        /// 文本内容。
        text: String,
    },
    /// 通过平台结构确认的提及。
    Mention {
        /// 被提及对象；提及全体时为空。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor: Option<ActorRef>,
        /// 是否为全体成员。
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        everyone: bool,
    },
    /// 可按需下载的图片或文件。
    Resource {
        /// 资源引用。
        resource: ResourceRef,
        /// 可得的文件名。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// 图片的显示宽高：本地库第一次读到这张图时按文件头记下，读消息时带上；
        /// 还没读过的为空（界面先按占位尺寸显示）。平台消息本身不带宽高。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<ImageSize>,
        /// 视频信息：有它的文件资源就是视频（契约 client v35）。平台知道是视频时由适配器填；
        /// 平台没说、文件名是视频的，交给宿主前由框架补上一个空的（[`ImMessage::complete_videos`]）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        video: Option<VideoInfo>,
        /// 明确的音频信息；平台语音与普通音频文件分别保留语义。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audio: Option<Box<AudioInfo>>,
    },
    /// 当前无法完整翻译的消息内容。
    Unsupported {
        /// 平台内容种类。
        kind: String,
        /// 平台里这类消息的叫法（如飞书的“投票”），界面写成“\[投票\] · 暂不支持”；平台没给时为空。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// 卡片：标题、标题色调与按顺序排列的若干块。各平台卡片的写法已在平台适配器里归一，
    /// 上层只按块的类型显示，不看是哪个平台。
    Card(ImCard),
    /// 格式正文：带格式或链接的普通消息，结构已在平台适配器里整理成标准 Markdown 语法树
    /// （mdast）的子集。上层按节点显示，不解析文字（契约 client v6）。
    Rich(ImRich),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioInfo {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub voice: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

impl ImContent {
    /// 这一块里的每个资源（图片、文件），包括格式正文里的，按出现顺序；可改写图片宽高。
    pub fn for_each_resource_mut(
        &mut self,
        f: &mut impl FnMut(&ResourceRef, &mut Option<ImageSize>),
    ) {
        fn rich(nodes: &mut [RichNode], f: &mut impl FnMut(&ResourceRef, &mut Option<ImageSize>)) {
            RichNode::walk_all_mut(nodes, &mut |node| {
                if let RichNode::Resource { resource, size, .. } = node {
                    f(resource, size);
                }
            });
        }
        match self {
            Self::Resource { resource, size, .. } => f(resource, size),
            Self::Rich(content) => rich(&mut content.root.children, f),
            // 卡片 2.0：图片组里的图与富文本、面板标题里的图（契约 client v16 §5）。
            Self::Card(card) => card.walk_blocks_mut(&mut |block| match block {
                CardBlock::Images { images, .. } => images
                    .iter_mut()
                    .for_each(|image| f(&image.resource, &mut image.size)),
                CardBlock::Rich { children }
                | CardBlock::Panel {
                    title: children, ..
                } => rich(children, f),
                _ => {}
            }),
            _ => {}
        }
    }

    /// 这一块里的每个资源（图片、文件），包括格式正文与卡片里的，按出现顺序。
    pub fn for_each_resource(&self, f: &mut impl FnMut(&ResourceRef, Option<&ImageSize>)) {
        self.for_each_resource_detail(&mut |resource, size, _| f(resource, size));
    }

    /// 这一块里的每个资源及平台给的文件名（包括格式正文与卡片里的），按出现顺序；卡片图片以说明文字作名字。
    pub fn for_each_named_resource(&self, f: &mut impl FnMut(&ResourceRef, Option<&str>)) {
        self.for_each_resource_detail(&mut |resource, _, name| f(resource, name));
    }

    fn for_each_resource_detail(
        &self,
        f: &mut impl FnMut(&ResourceRef, Option<&ImageSize>, Option<&str>),
    ) {
        fn rich(
            nodes: &[RichNode],
            f: &mut impl FnMut(&ResourceRef, Option<&ImageSize>, Option<&str>),
        ) {
            RichNode::walk_all(nodes, &mut |node| {
                if let RichNode::Resource {
                    resource,
                    size,
                    name,
                } = node
                {
                    f(resource, size.as_ref(), name.as_deref());
                }
            });
        }
        match self {
            Self::Resource {
                resource,
                size,
                name,
                ..
            } => f(resource, size.as_ref(), name.as_deref()),
            Self::Rich(content) => rich(&content.root.children, f),
            Self::Card(card) => card.walk_blocks(&mut |block| match block {
                CardBlock::Images { images, .. } => images.iter().for_each(|image| {
                    f(&image.resource, image.size.as_ref(), image.alt.as_deref())
                }),
                CardBlock::Rich { children }
                | CardBlock::Panel {
                    title: children, ..
                } => rich(children, f),
                _ => {}
            }),
            _ => {}
        }
    }

    /// 同 [`Self::for_each_resource_mut`]，另加视频的封面（契约 client v35）：本地库拆出、带回封面宽高时用。
    /// 封面不是消息内容，交给伙伴的附件、预览文字只用 [`Self::for_each_resource_mut`]。
    pub fn for_each_stored_resource_mut(
        &mut self,
        f: &mut impl FnMut(&ResourceRef, &mut Option<ImageSize>),
    ) {
        self.for_each_resource_mut(f);
        if let Self::Resource {
            video: Some(VideoInfo {
                cover: Some(cover), ..
            }),
            ..
        } = self
        {
            f(&cover.resource, &mut cover.size);
        }
    }

    /// 同 [`Self::for_each_resource`]，另加视频的封面（契约 client v35）：本地库判断有没有宽高要拆出时用。
    pub fn for_each_stored_resource(&self, f: &mut impl FnMut(&ResourceRef, Option<&ImageSize>)) {
        self.for_each_resource(f);
        if let Self::Resource {
            video: Some(VideoInfo {
                cover: Some(cover), ..
            }),
            ..
        } = self
        {
            f(&cover.resource, cover.size.as_ref());
        }
    }

    /// 同 [`Self::for_each_named_resource`]，另加视频的封面（没有文件名）：本地库登记资源时用，
    /// 这样封面与消息图片一样能下载、记宽高、算占用。
    pub fn for_each_stored_named_resource(&self, f: &mut impl FnMut(&ResourceRef, Option<&str>)) {
        self.for_each_named_resource(f);
        if let Self::Resource {
            video: Some(VideoInfo {
                cover: Some(cover), ..
            }),
            ..
        } = self
        {
            f(&cover.resource, None);
        }
    }

    /// 是不是视频（带了视频信息的资源）。
    pub fn is_video(&self) -> bool {
        matches!(self, Self::Resource { video: Some(_), .. })
    }

    /// 平台没说是不是视频、文件名是视频的文件资源，补上一个空的视频信息（契约 client v35 §1.2）。
    fn complete_video(&mut self) {
        if let Self::Resource {
            resource,
            name: Some(name),
            video: video @ None,
            ..
        } = self
            && resource.kind == ResourceKind::File
            && is_video_name(name)
        {
            *video = Some(VideoInfo::default());
        }
    }

    /// 这一块里的每个提及（包括格式正文与卡片里的）：被提及对象与是否全体。
    pub fn for_each_mention(&self, f: &mut impl FnMut(Option<&ActorRef>, bool)) {
        let mut rich = |node: &RichNode| {
            if let RichNode::Mention { actor, everyone } = node {
                f(actor.as_ref(), *everyone);
            }
        };
        match self {
            Self::Mention { actor, everyone } => f(actor.as_ref(), *everyone),
            Self::Rich(content) => content.walk(&mut rich),
            Self::Card(card) => card.walk_rich(&mut rich),
            _ => {}
        }
    }

    /// 这一块里有没有暂不支持的内容（包括格式正文与卡片里的）；有就说明整条消息不完整。
    pub fn has_unsupported(&self) -> bool {
        let mut found = false;
        let mut rich = |node: &RichNode| found |= matches!(node, RichNode::Unsupported { .. });
        match self {
            Self::Unsupported { .. } => return true,
            Self::Rich(content) => content.walk(&mut rich),
            Self::Card(card) => card.walk_rich(&mut rich),
            _ => {}
        }
        found
    }
}

/// 格式正文：一棵标准 Markdown 语法树（mdast，unist 规范）的子集，外加几种 IM 常见、
/// Markdown 里没有的扩展节点（下划线、提及、表情、资源、暂不支持）。
///
/// JSON 形状与 mdast 一致（`type` 内部标签、字段名 camelCase），上层可以直接交给
/// 现成的 mdast 工具显示；存储用本库自己的类型，不随解析库的升级改变。
///
/// # Examples
///
/// ```
/// use hhh_im_feishu::{ImContent, ImRich, RichNode};
/// let rich = ImContent::Rich(ImRich::new(vec![RichNode::Paragraph {
///     children: vec![
///         RichNode::Text { value: "看这个 ".into() },
///         RichNode::Link {
///             url: "https://open.feishu.cn".into(),
///             children: vec![RichNode::Text { value: "开放平台".into() }],
///         },
///     ],
/// }]));
/// let json = serde_json::to_value(&rich).unwrap();
/// assert_eq!(json["type"], "rich");
/// assert_eq!(json["root"]["type"], "root");
/// assert_eq!(json["root"]["children"][0]["children"][1]["url"], "https://open.feishu.cn");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImRich {
    /// 语法树的根。
    pub root: RichRoot,
}

impl ImRich {
    /// 用若干块做一棵树。
    pub fn new(children: Vec<RichNode>) -> Self {
        Self {
            root: RichRoot { children },
        }
    }

    /// 深度优先逐个访问节点（先父后子）。
    pub fn walk(&self, f: &mut impl FnMut(&RichNode)) {
        RichNode::walk_all(&self.root.children, f);
    }

    /// 深度优先逐个访问节点，可改写（先父后子）。
    pub fn walk_mut(&mut self, f: &mut impl FnMut(&mut RichNode)) {
        RichNode::walk_all_mut(&mut self.root.children, f);
    }

    /// 树里一共有多少个节点（不含根），用于读取预算。
    pub fn node_count(&self) -> usize {
        let mut count = 0;
        self.walk(&mut |_| count += 1);
        count
    }
}

/// 语法树的根：`{"type":"root","children":[...]}`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename = "root")]
pub struct RichRoot {
    /// 按顺序的块。
    pub children: Vec<RichNode>,
}

/// 格式正文的节点。块与行内节点共用一个类型，嵌套关系遵守 mdast：
/// 例如 `listItem` 里放 `paragraph`，样式节点里放文字或别的样式节点。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RichNode {
    /// 一段（一行）。
    Paragraph {
        /// 行内节点。
        children: Vec<RichNode>,
    },
    /// 标题；只来自平台明确标为 Markdown 的内容，界面只加粗、不放大。
    Heading {
        /// 级别 1–6。
        depth: u8,
        /// 行内节点。
        children: Vec<RichNode>,
    },
    /// 分割线。
    ThematicBreak,
    /// 引用；只来自平台明确标为 Markdown 的内容。
    Blockquote {
        /// 块。
        children: Vec<RichNode>,
    },
    /// 列表。
    List {
        /// 有序（编号）还是无序（圆点）。
        ordered: bool,
        /// 有序列表的起始编号；从 1 开始时为空。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start: Option<u32>,
        /// 列表项（`listItem`）。
        children: Vec<RichNode>,
    },
    /// 列表项；里面是段落，嵌套列表跟在段落后面。
    ListItem {
        /// 块。
        children: Vec<RichNode>,
    },
    /// 代码块。
    Code {
        /// 语言；平台没给时为空。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lang: Option<String>,
        /// 原文，行与行之间一个换行，末尾不带换行。
        value: String,
    },
    /// 表格；只来自平台明确标为 Markdown 的内容或卡片里的表格组件。第一行是表头。
    Table {
        /// 各列的对齐；没给时为空。
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        align: Vec<Option<RichAlign>>,
        /// 平台分页时每页几行（不含表头，卡片里的表格照平台分页）；不分页时为空。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        page_size: Option<u16>,
        /// 行（`tableRow`）。
        children: Vec<RichNode>,
    },
    /// 表格的一行。
    TableRow {
        /// 单元格（`tableCell`）。
        children: Vec<RichNode>,
    },
    /// 表格的一格。
    TableCell {
        /// 行内节点。
        children: Vec<RichNode>,
    },
    /// 文字。
    Text {
        /// 原文，不含任何格式标记。
        value: String,
    },
    /// 加粗。
    Strong {
        /// 行内节点。
        children: Vec<RichNode>,
    },
    /// 斜体。
    Emphasis {
        /// 行内节点。
        children: Vec<RichNode>,
    },
    /// 删除线。
    Delete {
        /// 行内节点。
        children: Vec<RichNode>,
    },
    /// 下划线（HHH 扩展：Markdown 没有下划线）。
    Underline {
        /// 行内节点。
        children: Vec<RichNode>,
    },
    /// 行内代码；只来自平台明确标为 Markdown 的内容。
    InlineCode {
        /// 原文。
        value: String,
    },
    /// 行内换行。
    Break,
    /// 链接；地址原样保存，能不能点由上层决定。
    Link {
        /// 地址。
        url: String,
        /// 显示的行内节点。
        children: Vec<RichNode>,
    },
    /// 提及（HHH 扩展），与 [`ImContent::Mention`] 同一语义。
    Mention {
        /// 被提及对象；提及全体时为空。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor: Option<ActorRef>,
        /// 是否为全体成员。
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        everyone: bool,
    },
    /// 平台表情（HHH 扩展），编号与表情回复同一套。
    Emoji {
        /// 平台表情编号，如 `SMILE`。
        emoji_type: String,
    },
    /// 图片或文件（HHH 扩展），与 [`ImContent::Resource`] 同一语义。
    Resource {
        /// 资源引用。
        resource: ResourceRef,
        /// 可得的文件名。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// 图片的显示宽高，规则同 [`ImContent::Resource`]。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<ImageSize>,
    },
    /// 暂不支持的内容（HHH 扩展），与 [`ImContent::Unsupported`] 同一语义。
    Unsupported {
        /// 平台内容种类。
        kind: String,
        /// 平台里这类内容的叫法；平台没给时为空。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// 彩色文字（HHH 扩展，卡片正文）：平台颜色已归到色调，认不出的颜色不产生这个节点。
    Color {
        /// 色调。
        tone: CardTone,
        /// 行内节点。
        children: Vec<RichNode>,
    },
    /// 小号彩色标签（HHH 扩展，卡片正文与表格选项）。
    Tag {
        /// 标签文字。
        text: String,
        /// 色调。
        tone: CardTone,
    },
    /// 一个人（HHH 扩展，卡片正文与表格）：头像 + 名字，名字由上层按编号取。
    Person {
        /// 用户。
        actor: ActorRef,
    },
    /// 一个时间点（HHH 扩展，卡片正文与表格），由上层按本机时区显示。
    Time {
        /// Unix 毫秒。
        ms: i64,
        /// 平台的格式：格式种类（如 `date_num`）或格式串（如 `YYYY-MM-DD`）。
        format: String,
    },
}

impl RichNode {
    /// 深度优先逐个访问一串节点（先父后子）。
    pub fn walk_all(nodes: &[RichNode], f: &mut impl FnMut(&RichNode)) {
        for node in nodes {
            f(node);
            Self::walk_all(node.children(), f);
        }
    }

    /// 深度优先逐个访问一串节点，可改写（先父后子）。
    pub fn walk_all_mut(nodes: &mut [RichNode], f: &mut impl FnMut(&mut RichNode)) {
        for node in nodes {
            f(node);
            if let Some(children) = node.children_mut() {
                Self::walk_all_mut(children, f);
            }
        }
    }

    /// 子节点；叶子节点为空。
    pub fn children(&self) -> &[RichNode] {
        match self {
            Self::Paragraph { children }
            | Self::Heading { children, .. }
            | Self::Blockquote { children }
            | Self::List { children, .. }
            | Self::ListItem { children }
            | Self::Table { children, .. }
            | Self::TableRow { children }
            | Self::TableCell { children }
            | Self::Strong { children }
            | Self::Emphasis { children }
            | Self::Delete { children }
            | Self::Underline { children }
            | Self::Color { children, .. }
            | Self::Link { children, .. } => children,
            _ => &[],
        }
    }

    fn children_mut(&mut self) -> Option<&mut Vec<RichNode>> {
        match self {
            Self::Paragraph { children }
            | Self::Heading { children, .. }
            | Self::Blockquote { children }
            | Self::List { children, .. }
            | Self::ListItem { children }
            | Self::Table { children, .. }
            | Self::TableRow { children }
            | Self::TableCell { children }
            | Self::Strong { children }
            | Self::Emphasis { children }
            | Self::Delete { children }
            | Self::Underline { children }
            | Self::Color { children, .. }
            | Self::Link { children, .. } => Some(children),
            _ => None,
        }
    }
}

/// 表格列的对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RichAlign {
    /// 靠左。
    Left,
    /// 居中。
    Center,
    /// 靠右。
    Right,
}

/// 一张卡片：可选标题、标题色调与按顺序排列的若干块。
///
/// 段落文字是标准 Markdown（CommonMark + GFM），由平台适配器从平台自己的卡片写法转过来；
/// 界面用 Markdown 渲染器显示。只有标题的卡片是合法的。
///
/// # Examples
///
/// ```
/// use hhh_im_feishu::{CardBlock, CardInline, CardTone, ImCard, ImContent};
/// let card = ImContent::Card(ImCard {
///     title: Some("PR Review 汇总".into()),
///     tone: CardTone::Blue,
///     blocks: vec![CardBlock::Paragraph {
///         parts: vec![CardInline::Markdown { text: "**238**\n当前 PR".into() }],
///     }],
///     ..ImCard::default()
/// });
/// let json = serde_json::to_value(&card).unwrap();
/// assert_eq!(json["type"], "card");
/// assert_eq!(json["tone"], "blue");
/// assert_eq!(json["blocks"][0]["parts"][0]["type"], "markdown");
/// // 没有副标题、标签、撑满时这三个字段不出现，形状与卡片 1.0 时一样。
/// assert!(json.get("subtitle").is_none() && json.get("tags").is_none() && json.get("wide").is_none());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImCard {
    /// 标题；没有标题时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 副标题（卡片 2.0）；没有时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    /// 标题后面的标签（卡片 2.0），按平台的顺序。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<CardTag>,
    /// 标题色调；平台没给颜色时为中性。
    pub tone: CardTone,
    /// 原卡片要求撑满消息宽度（卡片 2.0 的 `width_mode: fill`），上层可放宽显示。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wide: bool,
    /// 标题下面的块，按平台卡片里的顺序。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<CardBlock>,
}

impl ImCard {
    /// 深度优先逐个访问卡片里的块（多栏、折叠面板里的也算，先外后内）。
    pub fn walk_blocks(&self, f: &mut impl FnMut(&CardBlock)) {
        fn visit(blocks: &[CardBlock], f: &mut impl FnMut(&CardBlock)) {
            for block in blocks {
                f(block);
                match block {
                    CardBlock::Columns { columns } => columns.iter().for_each(|c| visit(c, f)),
                    CardBlock::Panel { blocks, .. } => visit(blocks, f),
                    _ => {}
                }
            }
        }
        visit(&self.blocks, f);
    }

    /// 深度优先逐个访问卡片里的块，可改写（先外后内）。
    pub fn walk_blocks_mut(&mut self, f: &mut impl FnMut(&mut CardBlock)) {
        fn visit(blocks: &mut [CardBlock], f: &mut impl FnMut(&mut CardBlock)) {
            for block in blocks {
                f(block);
                match block {
                    CardBlock::Columns { columns } => columns.iter_mut().for_each(|c| visit(c, f)),
                    CardBlock::Panel { blocks, .. } => visit(blocks, f),
                    _ => {}
                }
            }
        }
        visit(&mut self.blocks, f);
    }

    /// 卡片里每个格式正文节点（富文本块、折叠面板标题），深度优先。
    pub fn walk_rich(&self, f: &mut impl FnMut(&RichNode)) {
        self.walk_blocks(&mut |block| match block {
            CardBlock::Rich { children }
            | CardBlock::Panel {
                title: children, ..
            } => RichNode::walk_all(children, f),
            _ => {}
        });
    }
}

/// 卡片标题后面的一个标签。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardTag {
    /// 标签文字。
    pub text: String,
    /// 色调。
    pub tone: CardTone,
}

/// 卡片里的一张图：这条消息的图片资源。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardImage {
    /// 资源引用（属于这条消息）。
    pub resource: ResourceRef,
    /// 说明文字；平台没给时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
    /// 图片的显示宽高，规则同 [`ImContent::Resource`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<ImageSize>,
}

/// 卡片里人员的显示大小。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PersonSize {
    /// 小。
    Small,
    /// 中。
    Medium,
    /// 大。
    Large,
}

/// 卡片的色调：平台的标题、文字、标签颜色归到这几种，界面再对到自己的配色。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CardTone {
    /// 不上色。
    #[default]
    Neutral,
    /// 灰（次要文字色；标题栏不用）。
    Grey,
    /// 蓝。
    Blue,
    /// 绿。
    Green,
    /// 黄。
    Yellow,
    /// 橙。
    Orange,
    /// 红。
    Red,
    /// 紫。
    Purple,
}

/// 卡片里的一块。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum CardBlock {
    /// 一段文字，由若干片按顺序拼成。
    Paragraph {
        /// 段落里的各片。
        parts: Vec<CardInline>,
    },
    /// 并排的几栏；每栏又是按顺序的若干块。
    Columns {
        /// 各栏，从左到右。
        columns: Vec<Vec<CardBlock>>,
    },
    /// 一条分割线。
    Divider,
    /// 一段格式正文（卡片 2.0 的富文本与文字，与 [`ImRich`] 同一套节点，外加彩色、标签、人员、时间）。
    Rich {
        /// 块。
        children: Vec<RichNode>,
    },
    /// 一组图片，每行 `columns` 张。
    Images {
        /// 一行几张（1–3）。
        columns: u8,
        /// 图片，按平台的顺序。
        images: Vec<CardImage>,
    },
    /// 一组人：头像 + 名字，名字由上层按编号取。
    People {
        /// 用户，按平台的顺序。
        people: Vec<ActorRef>,
        /// 大小。
        size: PersonSize,
        /// 显示头像。
        avatar: bool,
        /// 显示名字。
        name: bool,
    },
    /// 折叠面板。
    Panel {
        /// 面板标题（行内节点）。
        title: Vec<RichNode>,
        /// 初始是否展开。
        expanded: bool,
        /// 是否画边框。
        border: bool,
        /// 面板里的块。
        blocks: Vec<CardBlock>,
    },
}

/// 卡片段落里的一片。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum CardInline {
    /// 标准 Markdown 文字；平台的纯文字已转义，不会被当成格式。
    Markdown {
        /// Markdown 原文。
        text: String,
    },
}

/// 保留发送者、引用和原生话题关系的统一消息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImMessage {
    /// 消息引用。
    pub reference: MessageRef,
    /// 真实发送者。
    pub sender: ActorRef,
    /// 平台创建时间，单位为 Unix 毫秒。
    pub created_at_ms: i64,
    /// 平台更新时间，单位为 Unix 毫秒。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_ms: Option<i64>,
    /// 内容块。
    pub content: Vec<ImContent>,
    /// 被回复的消息。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<MessageRef>,
    /// 回复树的根消息，可能不同于直接回复对象。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_message: Option<MessageRef>,
    /// 平台原生话题编号。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// 平台明确说了它是不是话题里的回复：是话题回复为真，是会话消息流里的消息为假；
    /// 平台没说时为空，按话题编号与根消息推断（宿主本地消息库的 `thread_root_of`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_thread: Option<bool>,
    /// 消息是否已被撤回。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
    /// 是否有无法完整表达的内容。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub incomplete: bool,
    /// 平台说这条消息编辑过：编辑的时间，Unix 毫秒；没编辑过为空（契约 client v11 §3.6）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_at_ms: Option<i64>,
    /// 话题回复同时发到了会话消息流（Slack 的“回复同时发到频道”）：它既在话题里，也在会话消息流里
    /// （契约 client v11 §3.3）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub broadcast: bool,
    /// 平台随消息一起给的、这条消息现在的全部表情（Slack 的历史里表情是内嵌的，契约 client v11 §3.7）；
    /// 平台没随消息给时为空，由 hub 另按条读。本地库把它存进表情，不记在消息里；交给宿主的消息不带它。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reactions: Option<Vec<MessageReaction>>,
    /// 平台把这条与另一条配成“话题回复 + 同时发到会话的那条”（飞书的 `sync_to_chat_info`，契约 client v18 §2.1）；
    /// 没有时为空。Slack 是同一条消息两处都有，用 [`ImMessage::broadcast`]，不用它。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_copy: Option<ThreadCopy>,
    /// 话题根消息上平台随消息给出的话题现状（最后一条回复、回复数）：读会话时就知道话题有没有新回复
    /// （平台事实 [`crate::facts::THREAD_ACTIVITY_ON_ROOT`]）；平台不给、或这条不是话题根消息时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_activity: Option<ThreadActivity>,
    /// 平台逐条给的编辑原文（外部平台声明了“每条消息自己说能不能编辑”时，契约 provider-protocol v12 缺口 12）：本人能编辑
    /// 这条时有，编辑框用它；内置平台与没这样声明的插件恒为空。交给宿主的“编辑原文”仍以宿主算出的消息事实为准。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_source: Option<Vec<OutgoingContent>>,
    /// 插件随消息给的原始数据（外部平台声明了重译，契约 provider-protocol v12 缺口 4）：本地库另存一张表、留着以后重译；
    /// 不进消息记录、不交给宿主，比较消息时不算在内。
    #[serde(skip)]
    pub raw: RawSlot,
}

/// [`ImMessage::raw`] 的容器：比较两条消息时一律当作相同——原始数据不是消息事实，只为以后重译留底。装箱放：只有声明了重译
/// 的插件的消息才有，不让每条消息（以及装着消息的事件）都多占两个字符串的位置。
///
/// # Examples
///
/// ```
/// use hhh_im_feishu::{RawData, RawSlot};
/// let kept = RawSlot(Some(Box::new(RawData { format: "sample-msg-v1".into(), data: "{}".into() })));
/// assert_eq!(kept, RawSlot::default());
/// ```
#[derive(Debug, Clone, Default)]
pub struct RawSlot(pub Option<Box<RawData>>);

impl PartialEq for RawSlot {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for RawSlot {}

/// 插件给的一条消息的原始数据（已去掉凭据）：格式名由插件定。
#[derive(Clone, PartialEq, Eq)]
pub struct RawData {
    /// 格式名（如 `wechat-addmsg-v1`）。
    pub format: String,
    /// 原始数据（字符串或 base64）。
    pub data: String,
}

// 原始数据可能很长，日志里只写格式与长度。
impl std::fmt::Debug for RawData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawData")
            .field("format", &self.format)
            .field("bytes", &self.data.len())
            .finish()
    }
}

/// 话题根消息上平台给出的话题现状：读会话时拿到，与上次按话题读时记下的比对，不同就是有了新回复或删了回复
/// （契约 client v27 §3 第 3 条）。
///
/// # Examples
///
/// ```
/// use hhh_im_feishu::ThreadActivity;
/// let before = ThreadActivity { latest_reply_id: "1.000200".into(), latest_reply_at_ms: 1_000, reply_count: 2 };
/// let after = ThreadActivity { latest_reply_id: "1.000300".into(), latest_reply_at_ms: 1_100, reply_count: 3 };
/// assert!(!after.went_back_from(&before));
/// let deleted = ThreadActivity { reply_count: 1, ..before.clone() };
/// assert!(deleted.went_back_from(&before));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadActivity {
    /// 最后一条回复的消息编号。
    pub latest_reply_id: String,
    /// 最后一条回复的时间，Unix 毫秒。
    pub latest_reply_at_ms: i64,
    /// 回复数。
    pub reply_count: u32,
}

impl ThreadActivity {
    /// 比上次记下的往回退了：回复数变少，或最后一条回复变早——有回复被删，要整段读一次才知道删的是哪条。
    pub fn went_back_from(&self, before: &ThreadActivity) -> bool {
        self.reply_count < before.reply_count || self.latest_reply_at_ms < before.latest_reply_at_ms
    }
}

/// 平台标出的一对“话题回复 + 同时发到会话的那条”里，这一条是哪一边（契约 client v18 §2.1）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadCopy {
    /// 这一条在哪：会话消息流里的那条，还是话题里的那条。
    pub role: ThreadCopyRole,
    /// 另一边那条的消息编号。
    pub peer: String,
    /// 平台的话题编号（会话里那条带着，用来找话题第一条）；不知道时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
}

/// [`ThreadCopy`] 的哪一边。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ThreadCopyRole {
    /// 会话消息流里的那条（话题回复的副本）。
    InConversation,
    /// 话题里的那条（原本的回复）。
    InThread,
}

/// 系统通知（入群、改名等）的发送者种类：各平台翻译时都用这个值。
pub const SYSTEM_SENDER_KIND: &str = "system";
/// 机器人（平台应用）的身份种类：各平台翻译机器人发的消息、点的表情时都用这个值（契约 client v23 §2）。
pub const APP_SENDER_KIND: &str = "app";

impl ImMessage {
    /// 是不是系统通知。
    pub fn is_system(&self) -> bool {
        self.sender.kind == SYSTEM_SENDER_KIND
    }

    /// “是不是视频”只在框架判断一次（契约 client v35 §1.2）：适配器已按平台事实填了视频信息的不动；
    /// 平台没说、文件名是视频的文件资源补上空的视频信息。交给宿主前（hub）调用，本地库不因此改写。
    ///
    /// # Examples
    ///
    /// ```
    /// use hhh_im_feishu::{ActorRef, ConversationRef, ImContent, ImMessage, MessageRef, ResourceKind, ResourceRef};
    /// let reference = MessageRef { conversation: ConversationRef::new("work", "C01"), id: "1".into() };
    /// let file = |name: &str| ImContent::Resource {
    ///     resource: ResourceRef { message: reference.clone(), key: name.into(), kind: ResourceKind::File },
    ///     name: Some(name.into()), size: None, video: None, audio: None,
    /// };
    /// let mut message = ImMessage {
    ///     reference: reference.clone(),
    ///     sender: ActorRef { id: "U01".into(), id_type: "user_id".into(), kind: "user".into(), name: None },
    ///     created_at_ms: 1, updated_at_ms: None,
    ///     content: vec![file("录屏.mp4"), file("报价.pdf")],
    ///     reply_to: None, root_message: None, thread_id: None, in_thread: None,
    ///     edited_at_ms: None, deleted: false, incomplete: false, broadcast: false,
    ///     reactions: None, thread_copy: None, thread_activity: None,
    ///     edit_source: None, raw: Default::default(),
    /// };
    /// message.complete_videos();
    /// assert!(message.content[0].is_video());
    /// assert!(!message.content[1].is_video());
    /// ```
    pub fn complete_videos(&mut self) {
        self.content.iter_mut().for_each(ImContent::complete_video);
    }

    /// 这条消息里出现的人（契约 client v23 §3 第 1 条）：发送者（系统通知除外）、提及、卡片里的人员块与人员节点、
    /// 内嵌的表情里点的人。按出现顺序，可能重复；没有编号的（提及全体、系统通知）不在里面。
    ///
    /// # Examples
    ///
    /// ```
    /// use hhh_im_feishu::{ActorRef, ConversationRef, ImContent, ImMessage, MessageRef};
    /// let actor = |id: &str| ActorRef { id: id.into(), id_type: "user_id".into(), kind: "user".into(), name: None };
    /// let message = ImMessage {
    ///     reference: MessageRef { conversation: ConversationRef::new("work", "C01"), id: "1".into() },
    ///     sender: actor("U01"),
    ///     created_at_ms: 1,
    ///     updated_at_ms: None,
    ///     content: vec![ImContent::Mention { actor: Some(actor("U02")), everyone: false }],
    ///     reply_to: None, root_message: None, thread_id: None, in_thread: None,
    ///     deleted: false, incomplete: false, edited_at_ms: None, broadcast: false,
    ///     reactions: None, thread_copy: None, thread_activity: None,
    ///     edit_source: None, raw: Default::default(),
    /// };
    /// let ids: Vec<&str> = message.actors().iter().map(|a| a.id.as_str()).collect();
    /// assert_eq!(ids, ["U01", "U02"]);
    /// ```
    pub fn actors(&self) -> Vec<&ActorRef> {
        let mut out = Vec::new();
        if !self.is_system() && !self.sender.id.is_empty() {
            out.push(&self.sender);
        }
        for content in &self.content {
            content.for_each_actor(&mut |actor| out.push(actor));
        }
        for reaction in self.reactions.iter().flatten() {
            out.push(&reaction.operator);
        }
        out.retain(|actor| !actor.id.is_empty() && actor.kind != SYSTEM_SENDER_KIND);
        out
    }
}

impl ImContent {
    /// 这一块里出现的每个人（提及、格式正文与卡片里的提及和人员节点、卡片人员块）；提及全体不算。
    pub fn for_each_actor<'a>(&'a self, f: &mut impl FnMut(&'a ActorRef)) {
        fn rich<'a>(nodes: &'a [RichNode], f: &mut impl FnMut(&'a ActorRef)) {
            for node in nodes {
                match node {
                    RichNode::Mention {
                        actor: Some(actor), ..
                    }
                    | RichNode::Person { actor } => f(actor),
                    _ => {}
                }
                rich(node.children(), f);
            }
        }
        fn blocks<'a>(list: &'a [CardBlock], f: &mut impl FnMut(&'a ActorRef)) {
            for block in list {
                match block {
                    CardBlock::Rich { children } => rich(children, f),
                    CardBlock::People { people, .. } => people.iter().for_each(&mut *f),
                    CardBlock::Columns { columns } => {
                        columns.iter().for_each(|column| blocks(column, f));
                    }
                    CardBlock::Panel {
                        title,
                        blocks: inner,
                        ..
                    } => {
                        rich(title, f);
                        blocks(inner, f);
                    }
                    CardBlock::Paragraph { .. } | CardBlock::Divider | CardBlock::Images { .. } => {
                    }
                }
            }
        }
        match self {
            Self::Mention {
                actor: Some(actor), ..
            } => f(actor),
            Self::Rich(content) => rich(&content.root.children, f),
            Self::Card(card) => blocks(&card.blocks, f),
            _ => {}
        }
    }
}

/// 人名册里的一个人或机器人（契约 client v23 §2）：每个账号一份，所有会话共用。
///
/// # Examples
///
/// ```
/// let person: hhh_im_feishu::Person = serde_json::from_str(r#"{"id":"U01","name":"陈言","checkedAtMs":1}"#).unwrap();
/// assert!(!person.bot && !person.missing);
/// assert_eq!(person.name.as_deref(), Some("陈言"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    /// 平台身份编号（与 [`ActorRef::id`] 相同）。
    pub id: String,
    /// 是机器人（[`APP_SENDER_KIND`]）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bot: bool,
    /// 名字；还没读到时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 头像地址；平台没给时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    /// 最后一次向平台读到它的时间（Unix 毫秒）；只有成员名单给过名字、还没按编号读过时为 0。
    #[serde(default)]
    pub checked_at_ms: i64,
    /// 平台明确说没有这个编号，或按平台规则读不到（界面写“未知用户”“机器人”）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub missing: bool,
}

/// 按编号读人时要读的一个（契约 client v23 §3）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PersonKey {
    /// 平台身份编号。
    pub id: String,
    /// 是机器人。
    pub bot: bool,
}

/// 平台读到的一个人的资料。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonProfile {
    /// 平台身份编号。
    pub id: String,
    /// 名字；平台没给时为空。
    pub name: Option<String>,
    /// 头像地址；平台没给时为空。
    pub avatar_url: Option<String>,
}

impl From<SearchUser> for PersonProfile {
    fn from(user: SearchUser) -> Self {
        let name = user.name.trim();
        Self {
            id: user.reference.id,
            name: (!name.is_empty()).then(|| name.to_owned()),
            avatar_url: user.avatar_url,
        }
    }
}

/// 按编号读一批人的结果（[`crate::ImClient::look_up_people`]）。
///
/// 既不在 `found` 也不在 `missing` 里的编号这次没读（例如一批里的机器人太多），下次再读。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeopleLookup {
    /// 读到的人。
    pub found: Vec<PersonProfile>,
    /// 平台明确说没有、或按平台规则读不到的编号。
    pub missing: Vec<String>,
    /// 这次实际发出的请求数（记进读取额度）。
    pub requests: u32,
}

/// 平台允许对一条消息做的操作（契约 hhh-im client v17 §6）。
///
/// 只看平台规则与这条消息本身，不看连接状态：没连接时由宿主不给操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageAction {
    /// 在会话里引用回复。
    Reply,
    /// 回复这条消息的话题，或为它新建话题。
    Thread,
    /// 点表情。
    React,
    /// 撤回（只有本人发的）。
    Recall,
    /// 编辑（只有本人发的、内容是文字与提及的，契约 client v11 §3.6）。
    Edit,
}

/// 与查询范围绑定的续页游标。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageCursor {
    /// 不透明的平台分页位置。
    pub(crate) token: String,
    /// 查询范围，防止跨会话或改变查询条件后复用。
    pub(crate) scope: String,
}

/// 一页有界结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    /// 本页条目。
    pub items: Vec<T>,
    /// 后续页游标。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<PageCursor>,
}

/// 消息查询条件；时间边界由平台定义。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageQuery {
    /// 目标会话。
    pub conversation: ConversationRef,
    /// 可选原生话题。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// 起始时间，单位为 Unix 秒。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<i64>,
    /// 结束时间，单位为 Unix 秒。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_time: Option<i64>,
    /// 从较新的消息开始分页；页内仍从旧到新返回。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub newest_first: bool,
    /// 请求条数，飞书支持 1 至 50。
    pub limit: u16,
    /// 前一页返回的位置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<PageCursor>,
}
impl MessageQuery {
    /// 读取最近一页消息。
    pub fn recent(conversation: ConversationRef) -> Self {
        Self {
            conversation,
            thread_id: None,
            start_time: None,
            end_time: None,
            newest_first: true,
            limit: 20,
            cursor: None,
        }
    }
}

/// 平台单页表情的数量上限。
pub const REACTION_PAGE_MAX: u16 = 50;

/// 一条消息上的一个表情操作，保留人物或应用身份。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageReaction {
    /// 表情所属消息。
    pub message: MessageRef,
    /// 平台表情操作编号；相同表情由不同人添加时编号不同。
    pub id: String,
    /// 操作者；飞书用户为 open_id，应用为 app_id。
    pub operator: ActorRef,
    /// 平台原生表情名，例如 THUMBSUP。
    pub emoji_type: String,
    /// 平台返回的添加时间，Unix 毫秒；不是轮询发现时间或取消时间。平台不给时间时为空（Slack，
    /// 契约 client v11 §11 第 4 项），这时按平台给的顺序排。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_time_ms: Option<i64>,
}

/// 平台可用于表情回复的一个表情：类型与官方图片地址，另可带中文名、Unicode 字符、进不进面板与肤色。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmojiInfo {
    /// 平台原生表情名，例如 THUMBSUP。
    pub emoji_type: String,
    /// 官方图片地址（HTTPS）；只有 Unicode 字符、没有图的表情为空。
    pub image_url: String,
    /// 平台里的中文名（例如“赞”）：悬停提示、读屏与文字里的 `[赞]`；不知道时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 放不放进表情面板：肤色版本与实测点不上的只用于显示（契约 client v15 §2）。
    /// 旧的本机目录没有这一项，读出来按进面板。
    #[serde(default = "in_panel_by_default")]
    pub panel: bool,
    /// 肤色版本的肤色（如“深色”）；不是肤色版本时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<String>,
    /// 这个表情对应的 Unicode 字符（契约 client v17 §7）：没有图或图读不到时界面显示它；平台不给时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unicode: Option<String>,
    /// 所属分类的编号（[`EmojiCatalog::categories`] 里的）；目录不分类时为空（契约 client v18 §2.3）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// 其余名字（如 `thumbsup` 之于 `+1`），搜索时用，不单独占一格。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// 肤色版本：它是哪个表情的哪种肤色；不是肤色版本时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone_of: Option<ToneOf>,
}

/// 肤色版本对应的原表情与肤色。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToneOf {
    /// 原表情的类型。
    pub base: String,
    /// 肤色编号（[`EmojiCatalog::tones`] 里的）。
    pub tone: String,
}

/// 表情目录里的一个分类：编号、中文名；顺序就是面板里的顺序。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmojiCategory {
    /// 分类编号（`smileys`、`custom` 等）。
    pub id: String,
    /// 中文名。
    pub name: String,
}

/// 表情目录里的一种肤色。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmojiTone {
    /// 肤色编号（Slack 的 `skin-tone-2` 等）。
    pub id: String,
    /// 中文名（浅色、中浅色……）。
    pub name: String,
}

fn in_panel_by_default() -> bool {
    true
}

impl EmojiInfo {
    /// 平台目录里的一个表情：进面板，还不知道中文名。
    ///
    /// # Examples
    ///
    /// ```
    /// let ok = hhh_im_feishu::EmojiInfo::new("OK", "https://sf3-ttcdn-tos.pstatp.com/obj/lark-reaction-cn/emoji_ok_v2.png");
    /// assert!(ok.panel && ok.name.is_none() && ok.tone.is_none());
    /// ```
    pub fn new(emoji_type: impl Into<String>, image_url: impl Into<String>) -> Self {
        Self {
            emoji_type: emoji_type.into(),
            image_url: image_url.into(),
            name: None,
            panel: true,
            tone: None,
            unicode: None,
            category: None,
            aliases: Vec::new(),
            tone_of: None,
        }
    }
}

/// 平台的表情目录：有序、按类型去重；这是平台级数据，不分账号。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmojiCatalog {
    /// 表情，顺序与平台一致。
    pub emojis: Vec<EmojiInfo>,
    /// 来源（平台文档）的更新时间，Unix 毫秒；没变就不用重新解析。
    pub source_updated_ms: i64,
    /// 这份目录从平台读到的时间，Unix 毫秒。
    pub fetched_at_ms: i64,
    /// 分类，按面板里的顺序；目录不分类时为空，界面照旧一格一格铺开（契约 client v18 §2.3）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<EmojiCategory>,
    /// 平台有的肤色；没有肤色选择时为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tones: Vec<EmojiTone>,
}

/// 指定消息的表情分页查询，不代表连续事件订阅。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReactionQuery {
    /// 必须属于当前连接与会话的消息引用。
    pub message: MessageRef,
    /// 单页数量，1 至 50。
    pub limit: u16,
    /// 绑定连接、消息与页大小的续页游标。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<PageCursor>,
}
impl ReactionQuery {
    /// 读取指定消息的第一页表情，默认 50 条。
    ///
    /// # Examples
    ///
    /// ```
    /// use hhh_im_feishu::{ConversationRef, MessageRef, ReactionQuery};
    /// let query = ReactionQuery::new(MessageRef {
    ///     conversation: ConversationRef::new("personal", "oc_group"),
    ///     id: "om_message".into(),
    /// });
    /// assert_eq!(query.limit, 50);
    /// assert!(query.cursor.is_none());
    /// ```
    pub fn new(message: MessageRef) -> Self {
        Self {
            message,
            limit: REACTION_PAGE_MAX,
            cursor: None,
        }
    }
}

/// 一项能力的核验状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CapabilityState {
    /// 当前 API 实际成功调用过。
    Verified,
    /// 声明了相关授权，仍需对目标会话实际调用。
    Granted,
    /// 已知缺少用户授权。
    MissingScope,
    /// 此接口不提供该能力。
    Unsupported,
    /// 尚未取得足够证据。
    Unknown,
}

/// 能力状态及其适用范围。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capability {
    /// 稳定能力名。
    pub name: String,
    /// 状态。
    pub state: CapabilityState,
    /// 边界说明。
    pub detail: String,
}

/// 当前用户授权能够支持的操作概览。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitySet {
    /// 已核验账号。
    pub account: Account,
    /// 各项能力与限制。
    pub capabilities: Vec<Capability>,
}

/// 明确的消息发送目标。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SendTarget {
    /// 向会话发言。
    Conversation {
        /// 目标会话。
        conversation: ConversationRef,
    },
    /// 回复指定消息。
    Reply {
        /// 被回复消息。
        message: MessageRef,
        /// 明确要求进入原生话题。
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        in_thread: bool,
        /// 进话题时话题的根消息编号：被回复的本身是话题回复时，是它所在话题的根（hub 按本地记录填）。
        /// 平台只收根消息的（Slack）用它；不知道时为空，由平台自己认。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread_root: Option<String>,
        /// 话题回复同时发送到会话（契约 client v18 §2.1）：平台原生支持时一次发出；
        /// 不支持时 hub 在这条被接受后另发一条到会话。旧记录读出来为不发。
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        also_to_conversation: bool,
    },
}

/// 发送的文字与结构化提及。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum OutgoingContent {
    /// 普通文字；飞书提及标签须使用结构化 Mention。
    Text {
        /// 文字内容。
        text: String,
    },
    /// 明确提及某个 open_id。
    Mention {
        /// 被提及的用户编号。
        user_id: String,
        /// 显示名称。
        name: String,
    },
    /// 提及群里所有人；群设置不允许时平台会拒绝发送。
    MentionAll,
    /// 已上传的图片；图片必须单独成一条消息。
    Image {
        /// 上传后平台返回的资源编号。
        key: String,
    },
    /// 已上传的文件；文件必须单独成一条消息。
    File {
        /// 上传后平台返回的资源编号。
        key: String,
    },
    /// 格式正文（协议 provider-protocol v40）：宿主只交给声明了格式的插件，是这一条的全部内容，翻成飞书富文本。
    Rich {
        /// 格式正文的块（协议的节点）。
        children: Vec<hhh_im_protocol::content::RichNode>,
    },
}

/// 一次本人账号发送；操作编号由调用方先保存。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendRequest {
    /// 稳定操作编号，飞书映射为一小时有效的 uuid 去重字段。
    pub operation_id: String,
    /// 目标会话或回复对象。
    pub target: SendTarget,
    /// 待发送内容。
    pub content: Vec<OutgoingContent>,
}

/// 发送结果；平台接受不代表对方已读。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SendOutcome {
    /// 平台已接受消息。
    Accepted {
        /// 对应操作编号。
        operation_id: String,
        /// 平台返回的消息。
        message: Box<ImMessage>,
    },
    /// 请求可能已生效，禁止按普通失败盲目重发。
    Unknown {
        /// 对应操作编号。
        operation_id: String,
        /// 脱敏后的说明。
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(key: &str) -> ResourceRef {
        ResourceRef {
            message: MessageRef {
                conversation: ConversationRef::new("test", "oc_test"),
                id: "om_card".into(),
            },
            key: key.into(),
            kind: ResourceKind::Image,
        }
    }

    #[test]
    fn card_images_mentions_and_unsupported_nodes_are_visible_to_the_walks() {
        // 卡片 2.0 的图片组、富文本与折叠面板（含套在多栏里的）都要被找到：文件库登记、@我、看不全都靠这几个遍历。
        let me = ActorRef {
            id: "ou_me".into(),
            id_type: "open_id".into(),
            kind: "user".into(),
            name: None,
        };
        let mut content = ImContent::Card(ImCard {
            blocks: vec![
                CardBlock::Images {
                    columns: 2,
                    images: vec![CardImage {
                        resource: image("img_a"),
                        alt: Some("图一".into()),
                        size: None,
                    }],
                },
                CardBlock::Columns {
                    columns: vec![vec![CardBlock::Panel {
                        title: vec![RichNode::Mention {
                            actor: Some(me),
                            everyone: false,
                        }],
                        expanded: false,
                        border: false,
                        blocks: vec![CardBlock::Rich {
                            children: vec![RichNode::Paragraph {
                                children: vec![
                                    RichNode::Resource {
                                        resource: image("img_b"),
                                        name: None,
                                        size: None,
                                    },
                                    RichNode::Unsupported {
                                        kind: "audio".into(),
                                        label: None,
                                    },
                                ],
                            }],
                        }],
                    }]],
                },
            ],
            ..ImCard::default()
        });
        let mut named = Vec::new();
        content.for_each_named_resource(&mut |r, name| {
            named.push((r.key.clone(), name.map(str::to_owned)));
        });
        assert_eq!(
            named,
            [
                ("img_a".to_owned(), Some("图一".to_owned())),
                ("img_b".to_owned(), None)
            ]
        );
        let mut mentions = Vec::new();
        content.for_each_mention(&mut |actor, _| mentions.push(actor.map(|a| a.id.clone())));
        assert_eq!(mentions, [Some("ou_me".to_owned())]);
        assert!(content.has_unsupported());
        // 本地库按图片宽高回填：卡片里的图也能改写。
        content.for_each_resource_mut(&mut |_, size| {
            *size = Some(ImageSize {
                width: 640,
                height: 360,
            });
        });
        let mut sizes = 0;
        content.for_each_resource(&mut |_, size| sizes += usize::from(size.is_some()));
        assert_eq!(sizes, 2);
    }
}
