//! 飞书的平台事实：原来登记在 HHH 平台表里的 `PLATFORM`（契约 hhh-im client v17 §3、§6）与连接表单的说明文字
//! （原 `connection.rs`），搬成插件里的普通常量与函数，一项一项与原来对得上。
//!
//! HHH 内置时这些写在 hhh-im 的 `Platform` 结构里，由宿主按它限速、分页、画界面、算每条消息能做什么；插件里
//! 宿主拿不到 Rust 结构，第二步由插件在 `initialize` 里把它们写成协议的平台说明（`PlatformDesc`）与登录方式交给宿主。
//! 只有宿主自己才有的几项没有搬：参数声明（宿主按读取间隔的默认值自己画）、外部平台规则、本机库回答的查询。

use super::{FeishuClient, reactions, resource, upload};
use crate::*;

/// 给界面的叫法、链接与限制（平台说明里给界面的那部分）。
///
/// # Examples
///
/// ```
/// let info = hhh_im_feishu::facts::INFO;
/// assert_eq!(info.id, "feishu");
/// assert_eq!(info.chat_link("oc_a b"), "https://applink.feishu.cn/client/chat/open?openChatId=oc_a%20b");
/// ```
pub const INFO: PlatformInfo = PlatformInfo {
    id: "feishu",
    name: "飞书",
    chat_link_template: "https://applink.feishu.cn/client/chat/open?openChatId={conversation}",
    image_name_prefix: "飞书图片",
    image_upload_max_bytes: upload::IMAGE_UPLOAD_MAX_BYTES,
    file_upload_max_bytes: upload::FILE_UPLOAD_MAX_BYTES,
    attachment_name_max_bytes: ATTACHMENT_NAME_MAX_BYTES,
    unlisted_emoji: super::emoji::UNLISTED_EMOJI,
    // 飞书开放接口的回复没有“同时发送到群”（2026-09-30 取样），内核发两次：话题里一条，被接受后群里再一条；
    // 图片、文件用同一份上传的编号再发一次（契约 client v18 §2.1）。
    thread_broadcast: Some(ThreadBroadcast {
        native: false,
        attachments: true,
    }),
    // 飞书客户端里的头像是圆的（契约 client v24；Friend im-watch v21 起就照这个画）。
    avatar_shape: AvatarShape::Circle,
    // 飞书正文里的表情写成 `[中文名]`（如 `[微笑]`），表情图是白底的（契约 provider-protocol v12 缺口 9：
    // 原来界面对所有 IM 都这样做，改成飞书在自己的平台说明里声明，飞书看到的不变）。
    emoji_text_pattern: Some(r"\[([^\[\]\r\n]{1,12})\]"),
    emoji_white_background: true,
    // 飞书开放平台官方 Logo 的 32×32 图形区域，官方配色（资源地址：https://lf-package-cn.feishucdn.com/obj/feishu-static/lark/open/website/images/899fa60e60151c73aaea2e25871102dc.svg）。
    // 图形自带留白，外框长边占画布 0.83（视觉尺寸规范见 Friend 组件目录“IM 平台标志的视觉尺寸”）；换图形时重量外框。
    // 插件清单 `hhh-im-provider.json` 里的 `logoStyle`、`logoInk` 与这里同一份，换图形时两处一起改。
    logo: Some(PlatformLogo {
        media_type: "image/svg+xml",
        bytes: include_bytes!("logo.svg"),
        style: LogoStyle::Mark,
        ink: Some(LogoInk {
            x: 3.16,
            y: 5.48,
            width: 26.49,
            height: 21.07,
        }),
    }),
};

/// 表情写进文字的写法（HHH 契约 provider-protocol v31）：与正文写法 `[中文名]` 对应，HHH 输入框的表情按钮按它把表情插进文字。
pub const EMOJI_TEXT_TEMPLATE: &str = "[{name}]";

/// 写不进文字的表情（只能点成回应），HHH 输入框的表情面板不列。2026-10-22 取样：目录里有名字的 207 个逐个写成 `[名字]`
/// 从 HHH 发进测试群，`[2021年]` 被飞书服务端改写成 `[NotFound]`（读回的正文就是它），其余 206 个在飞书客户端都显示成表情。
pub const NOT_IN_TEXT: &[&str] = &["2021"];

/// 附件文件名上限（飞书上传接口对文件名有长度限制）。
const ATTACHMENT_NAME_MAX_BYTES: usize = 250;

// ---------- 读取与下载的额度 ----------

/// 读取（消息、话题、表情、资料与成员）合计每秒最多几次。
///
/// 飞书文档写“获取会话历史消息”1000 次/分钟、50 次/秒（文档值，未实测，也没写按应用、用户还是租户计），
/// 每个账号合计留出余量（契约 client v13 §3）。
pub const READS_PER_SECOND: u32 = 40;
/// 读取合计每分钟最多几次（同 [`READS_PER_SECOND`]）。
pub const READS_PER_MINUTE: u32 = 800;
/// 下载消息资源每秒最多几次（飞书“获取消息中的资源文件”的接口限速）。
pub const DOWNLOADS_PER_SECOND: u32 = resource::RESOURCE_RATE_PER_SECOND;
/// 下载消息资源每分钟最多几次。
pub const DOWNLOADS_PER_MINUTE: u32 = resource::RESOURCE_RATE_PER_MINUTE;
/// 收到的文件按多少字节一段读、断了接着下（图片总是整份读）。
pub const FILE_SEGMENT_BYTES: u64 = resource::RESOURCE_SEGMENT_BYTES;
/// 飞书消息列表一页最多 50 条（按会话读一页最多几条，契约 client v27 §7）。
pub const HISTORY_PAGE_MAX: u16 = 50;
/// 人名册一次最多交给适配器读多少人：按通讯录批量接口读，一次请求 50 人；一批两次请求（契约 client v23 §4）。
pub const PEOPLE_BATCH_MAX: usize = 100;

/// 读取间隔的默认值（秒）：飞书用 HHH 的标准档（`ReadIntervals::STANDARD`）——正在看 2 秒、1 小时内 5 秒、
/// 1 天内 1 分钟、1 周内 5 分钟、更早 10 分钟，资料与人名 10 分钟，话题跟踪 7 天，会话发现 30 秒。
pub mod read_defaults {
    /// 正在看。
    pub const WATCHING_SECS: u64 = 2;
    /// 1 小时内有动静。
    pub const ACTIVE_SECS: u64 = 5;
    /// 1 天内有动静。
    pub const QUIET_SECS: u64 = 60;
    /// 1 周内有动静。
    pub const DORMANT_SECS: u64 = 5 * 60;
    /// 更早。
    pub const STALE_SECS: u64 = 10 * 60;
    /// 会话资料与群成员多久刷新一次。
    pub const PROFILE_SECS: u64 = 10 * 60;
    /// 人名与头像多久算过期。
    pub const PEOPLE_SECS: u64 = 10 * 60;
    /// 跟踪最近这么多天有动静的话题。
    pub const THREAD_DAYS: u32 = 7;
    /// 会话发现间隔。
    pub const DISCOVER_SECS: u64 = 30;
}

/// 参数配置里开放给用户改的读取与下载项：只开放读取频率（协议写法 `readIntervals`），与今天一样
/// （契约 provider-protocol v12 缺口 2）。
pub const USER_ADJUSTABLE: &[&str] = &["readIntervals"];

// ---------- 会话发现（契约 client v29） ----------

/// 不带关键词、按时间范围的消息搜索（契约 client v29，2026-10-04 取样）：结果从新到旧、每页 30 条，发出后
/// 8–12 秒收录；工作日一天约 26 页、最忙的一分钟不到 3 条。限额没读到（未核实），最快 15 秒一次。
pub const DISCOVERY_FASTEST_SECS: u64 = 15;
/// 平时一次检查最多翻几页。
pub const DISCOVERY_PAGES: u16 = 10;
/// 冷启动（补最近 1 天）一次最多翻几页。
pub const DISCOVERY_COLD_PAGES: u16 = 40;

// ---------- 平台的固定行为 ----------

/// 页面可以直接显示的图片来源（内容安全策略的写法）。
pub const IMAGE_SOURCES: &[&str] = &["https://*.feishucdn.com"];
/// 飞书把撤回的消息留在历史里标出来：复核窗口里本地有、平台没有了的不算撤回。
pub const DELETED_MESSAGES_VANISH: bool = false;
/// 表情目录不按账号分：同平台的账号共用一份。
pub const EMOJI_PER_ACCOUNT: bool = false;
/// 飞书根消息上没有回复数与最后回复时间（`update_time` 不随回复走，契约 client v27 取样）：话题按冷热定时读。
pub const THREAD_ACTIVITY_ON_ROOT: bool = false;
/// 飞书按话题读带时间条件也不筛（取样）：从新往旧读到已有就停。
pub const THREAD_READS_AFTER: bool = false;
/// 读消息不带表情，表情按条读（client v13 §7）。
pub const REACTIONS_IN_READS: bool = false;
/// 不是只推一次的平台：消息随时能向飞书重读（协议 `history = server`）。
pub const PUSH_ONLY: bool = false;

/// 头像地址里的身份（平台头像编号与变体）：同一个头像每次给的地址不同时靠它认出没变；认不出为空。
pub fn avatar_identity(url: &str) -> Option<(String, String)> {
    FeishuClient::avatar_identity(url)
}

/// 表情编号是否符合飞书的规则。
pub fn valid_emoji_name(emoji_type: &str) -> bool {
    reactions::is_valid_emoji_type(emoji_type)
}

/// 把随包带的表情与中文名并进飞书给的表情目录（契约 client v15 §2）。
pub fn complete_emoji_catalog(catalog: EmojiCatalog) -> EmojiCatalog {
    super::complete_emoji_catalog(catalog)
}

/// 目录里没有的表情类型按飞书的命名规律猜图时依次试的地址；企业表情与格式不对的不猜。
pub fn guess_emoji_images(emoji_type: &str) -> Vec<EmojiInfo> {
    super::guessed_emoji_images(emoji_type)
}

/// 飞书允许的操作：群通知与撤回的消息什么都不能做；其余都能回复、回话题、点表情，本人发的还能撤回
/// （撤回的时限由平台裁决，不预判）。第二个参数是“这条是本人发的”。
pub fn actions(message: &ImMessage, mine: bool) -> Vec<MessageAction> {
    if message.deleted || message.is_system() {
        return Vec::new();
    }
    let mut actions = vec![
        MessageAction::Reply,
        MessageAction::Thread,
        MessageAction::React,
    ];
    if mine {
        actions.push(MessageAction::Recall);
    }
    actions
}

/// 飞书会话的叫法：群叫“群”，单聊叫“会话”。
pub fn conversation_noun(kind: ConversationKind) -> &'static str {
    match kind {
        ConversationKind::Group | ConversationKind::Channel => "群",
        _ => "会话",
    }
}

/// 能编辑的消息的编辑用原文：飞书不能以本人身份编辑消息（契约 client v11 §3.6），没有编辑用原文。
pub fn edit_draft(_message: &ImMessage) -> Option<Vec<OutgoingContent>> {
    None
}

/// 飞书改过的富文本只有内容变了、没有别的痕迹（契约 client v26 §2，2026-10-03 取样）：文字、提及、格式正文组成的消息
/// 交给本地库比较内容认。图片、文件、表情包、卡片（机器人更新卡片不算编辑，v18 §2.4）、系统通知、撤回的都不参与。
pub fn edits_shown_by_change(message: &ImMessage) -> bool {
    !message.deleted
        && !message.is_system()
        && !message.content.is_empty()
        && message.content.iter().all(|c| {
            matches!(
                c,
                ImContent::Text { .. } | ImContent::Mention { .. } | ImContent::Rich(_)
            )
        })
}

// ---------- 连接与登录的说明文字（原 connection.rs，宿主按声明显示） ----------

/// 用户自己的应用：应用编号字段与密钥字段，平台后台的回调配置说明。
pub mod app {
    /// 应用编号字段的名字。
    pub const ID_LABEL: &str = "App ID";
    /// 应用编号输入框的占位文字。
    pub const ID_PLACEHOLDER: &str = "cli_…";
    /// 密钥字段的名字。
    pub const SECRET_LABEL: &str = "App Secret";
    /// 密钥输入框的占位文字。
    pub const SECRET_PLACEHOLDER: &str = "输入 App Secret";
    /// 首次配置时密钥不能留空。
    pub const SECRET_OPTIONAL: bool = false;
    /// 从哪里取得密钥。
    pub const SECRET_HELP: &str = "飞书开放平台 → 凭证与基础信息。只保存在本机。";
    /// 平台后台的回调配置说明；地址由宿主提供。
    pub const CALLBACK_HELP: &str = "登记到飞书开发者后台 → 安全设置 → 重定向 URL。地址里有 Friend 的端口，换了端口要再登记一个（飞书可以登记多个）。";
}

/// 两种登录方式（契约 friend/im-auth-relay v5）：中继上配置的飞书应用，或自己的应用（字段见 [`app`]）。
/// v6：中继地址不替用户填，提示字只说要填什么；自己的应用带创建应用的链接（创建飞书机器人的入口）。
pub mod login {
    /// 经中继登录：方式编号。
    pub const RELAY_MODE: &str = "relay";
    /// 经中继登录：页签上的名字。
    pub const RELAY_NAME: &str = "通过中继登录";
    /// 经中继登录：页签下面一行说明。
    pub const RELAY_NOTE: &str =
        "用中继上配置的飞书应用，只有这个应用所在企业的成员能登录；令牌会经过中继，中继不保存。";
    /// 中继地址字段的名字。
    pub const RELAY_FIELD_LABEL: &str = "中继地址";
    /// 中继地址输入框的占位文字。
    pub const RELAY_FIELD_PLACEHOLDER: &str = "输入中继地址，https://…";
    /// 用自己的应用：方式编号。
    pub const OWN_MODE: &str = "own";
    /// 用自己的应用：页签上的名字。
    pub const OWN_NAME: &str = "使用自己的应用";
    /// 用自己的应用：页签下面一行说明。
    pub const OWN_NOTE: &str = "填你企业的飞书自建应用，登录与续期只在这台电脑上。";
    /// 去飞书开放平台创建应用的链接字（宿主在后面加 ↗、新窗口打开）。
    pub const CREATE_LINK_LABEL: &str = "前往飞书开放平台创建";
    /// 创建应用的地址。
    pub const CREATE_LINK_URL: &str = "https://open.feishu.cn/page/launcher?from=backend_oneclick";
    /// 接在链接后面的说明。
    pub const CREATE_LINK_AFTER: &str = "，创建完成后填入 App ID 与 App Secret。";
}

/// 三类授权失效原因的处理说明，与缺权限的说明。
pub mod reauth {
    /// 已不能续期时怎么处理。
    pub const EXPIRED: &str = "重新登录后，就能继续选择群聊或好友。";
    /// 飞书应用没有开启刷新授权时怎么处理。
    pub const REFRESH_DISABLED: &str = "飞书应用没有开启刷新授权。请在开发者后台「安全设置」开启刷新 user_access_token 并发布新版本，再重新登录。";
    /// 应用密钥失效时怎么处理。
    pub const APP_SECRET_INVALID: &str =
        "飞书 App Secret 已失效，请在设置里重新登录并填写新的 App Secret。";
    /// 缺权限的说明，`{scopes}` 由宿主替换为权限名列表。
    pub const MISSING_SCOPES_TEMPLATE: &str =
        "缺少权限 {scopes}：在飞书开发者后台开通并发布新版本后点重新登录";
    /// 读取消息表情缺权限时的处理说明。
    pub const REACTIONS_TIP: &str = "要显示群里其他人点的表情，需要在飞书开发者后台给应用开通“查看消息表情回复”（im:message.reactions:read），再在设置里重新登录。";
}
