//! `initialize`（protocol-v1 §4.1、§5）：插件信息、平台说明、能力与登录方式，全部照 HHH 内置飞书的平台说明
//! （`facts.rs`，原来的 `PLATFORM` 与连接表单）一项一项写出来。`tests/fixtures/feishu-protocol/initialize.json` 锁住结果。

use hhh_im_protocol::method::{self as pm, CreateLink, LoginKind, LoginMethod};
use hhh_im_protocol::platform::{
    ActionRules, Adjustable, AvatarIdentity, AvatarShape, Broadcast, Capabilities, ContentKind,
    ConversationCaps, ConversationNouns, DiscoveryDesc, EmojiRules, HistoryMode, Limits,
    MessageCaps, OwnWithin, PeopleCaps, PlatformDesc, RateBudget, ReactionCaps, ReadIntervalsSecs,
    ResourceCaps, RichFormat, RichSendCaps, SendCaps, SessionCaps, Threads, UnlistedEmoji,
};
use hhh_im_protocol::rpc::RpcError;
use serde_json::Value;

use super::errors;
use crate::ErrorKind;
use crate::facts::{self, app, login, read_defaults};

/// 插件编号（同清单 `id`）。
pub(crate) const PROVIDER_ID: &str = "feishu";
/// 表情编号的写法（同 `facts::valid_emoji_name`：字母、数字、下划线，至多 64 字节）。
const EMOJI_NAME_PATTERN: &str = "^[A-Za-z0-9_]{1,64}$";
/// 头像键里的头像编号（同 `FeishuClient::avatar_identity`）：只认飞书 CDN（`*.feishucdn.com`、`*.pstatp.com`）上的 https
/// 地址，编号是路径里第一段以 `v3_` 开头的整段。头像键就是内置时存下的完整地址，本机已下好的头像照样认得。
const AVATAR_KEY_PATTERN: &str = r"^(?i:https)://[^/?#]*(?i:\.feishucdn\.com|\.pstatp\.com)(?::[0-9]*)?(?:/[^/?#]*)*?/(v3_[^/?#]*)";
/// 头像键里的尺寸（同内置：查询参数 `image_size` 的值，第一个为准；没有时没有变体）。
const AVATAR_VARIANT_PATTERN: &str = "^[^#]*?[?&]image_size=([^&#]*)";

/// 平台说明。
pub(crate) fn platform() -> PlatformDesc {
    let info = facts::INFO;
    let noun = |kind| facts::conversation_noun(kind).to_owned();
    let broadcast = info.thread_broadcast.map(|b| Broadcast {
        native: b.native,
        attachments: b.attachments,
    });
    PlatformDesc {
        avatar_shape: match info.avatar_shape {
            crate::AvatarShape::Circle => AvatarShape::Circle,
            crate::AvatarShape::Rounded => AvatarShape::Rounded,
        },
        avatar_identity: Some(AvatarIdentity {
            key_pattern: AVATAR_KEY_PATTERN.into(),
            variant_pattern: Some(AVATAR_VARIANT_PATTERN.into()),
        }),
        history: if facts::PUSH_ONLY {
            HistoryMode::None
        } else {
            HistoryMode::Server
        },
        // 同 `facts::actions`：群通知与撤回的消息什么都不能做（宿主统一规则）；其余能回复、回话题、点表情，
        // 本人发的还能撤回（时限由平台裁决，不写）；飞书不能以本人身份编辑。
        actions: ActionRules {
            reply: true,
            thread: true,
            react: true,
            recall: Some(OwnWithin {
                own: true,
                within_secs: None,
            }),
            edit: None,
        },
        chat_link: Some(info.chat_link_template.into()),
        image_name_prefix: Some(info.image_name_prefix.into()),
        conversation_nouns: Some(ConversationNouns {
            group: noun(crate::ConversationKind::Group),
            direct: noun(crate::ConversationKind::Direct),
            channel: noun(crate::ConversationKind::Channel),
            unknown: noun(crate::ConversationKind::Unknown),
        }),
        page_max: Some(u32::from(facts::HISTORY_PAGE_MAX)),
        reads: Some(RateBudget {
            per_second: facts::READS_PER_SECOND,
            per_minute: facts::READS_PER_MINUTE,
        }),
        downloads: Some(RateBudget {
            per_second: facts::DOWNLOADS_PER_SECOND,
            per_minute: facts::DOWNLOADS_PER_MINUTE,
        }),
        file_segment_bytes: Some(facts::FILE_SEGMENT_BYTES),
        limits: Some(Limits {
            image_upload_max_bytes: info.image_upload_max_bytes as u64,
            file_upload_max_bytes: info.file_upload_max_bytes as u64,
            attachment_name_max_bytes: info.attachment_name_max_bytes as u32,
        }),
        read_defaults: Some(ReadIntervalsSecs {
            watching: read_defaults::WATCHING_SECS,
            active: read_defaults::ACTIVE_SECS,
            quiet: read_defaults::QUIET_SECS,
            dormant: read_defaults::DORMANT_SECS,
            stale: read_defaults::STALE_SECS,
            profile: read_defaults::PROFILE_SECS,
            people: read_defaults::PEOPLE_SECS,
            thread_days: read_defaults::THREAD_DAYS,
            discover: Some(read_defaults::DISCOVER_SECS),
        }),
        threads: Some(Threads {
            activity_on_root: facts::THREAD_ACTIVITY_ON_ROOT,
            reads_after: facts::THREAD_READS_AFTER,
            broadcast,
        }),
        deleted_messages_vanish: facts::DELETED_MESSAGES_VANISH,
        reactions_in_reads: facts::REACTIONS_IN_READS,
        emoji_per_account: facts::EMOJI_PER_ACCOUNT,
        emoji: Some(EmojiRules {
            name_pattern: Some(EMOJI_NAME_PATTERN.into()),
            unlisted: UnlistedEmoji {
                placeholder_pattern: info.unlisted_emoji.placeholder_pattern.map(str::to_owned),
                placeholder_label: info.unlisted_emoji.placeholder_label.into(),
                guess: info.unlisted_emoji.guess_pattern.is_some(),
                guess_pattern: info.unlisted_emoji.guess_pattern.map(str::to_owned),
            },
            text_pattern: info.emoji_text_pattern.map(str::to_owned),
            white_background: info.emoji_white_background,
            // 表情写进文字的写法（协议 v31）：宿主据此给输入框的表情按钮。
            text_template: Some(facts::EMOJI_TEXT_TEMPLATE.into()),
        }),
        discovery: Some(DiscoveryDesc {
            fastest_secs: facts::DISCOVERY_FASTEST_SECS,
            pages: facts::DISCOVERY_PAGES,
            cold_pages: facts::DISCOVERY_COLD_PAGES,
        }),
        user_adjustable: facts::USER_ADJUSTABLE
            .iter()
            .filter_map(|name| serde_json::from_value::<Adjustable>(Value::from(*name)).ok())
            .collect(),
        // 同 `facts::edits_shown_by_change`：文字、提及、格式正文组成的消息按内容认编辑。
        edits_by_content: vec![ContentKind::Text, ContentKind::Mention, ContentKind::Rich],
        retranslate_rules: Vec::new(),
        content_repairs: Vec::new(),
        content_rereads: Vec::new(),
    }
}

/// 能力：飞书适配器实现了的都声明；不推送（飞书没有本人的实时推送，靠定时读）。
pub(crate) fn capabilities() -> Capabilities {
    Capabilities {
        stickers: Default::default(),
        receipts: Some(hhh_im_protocol::reading::ReceiptCaps {
            scopes: hhh_im_protocol::reading::ReadScopes {
                conversations: true,
                threads: false,
            },
            readers: true,
            unreaders: false,
            max_age_secs: Some(7 * 24 * 60 * 60),
        }),
        mark_read: None,
        read_sync: Some(hhh_im_protocol::reading::ReadSyncCaps {
            scopes: hhh_im_protocol::reading::ReadScopes {
                conversations: true,
                threads: true,
            },
            watermark: false,
            individual: true,
        }),
        conversations: ConversationCaps {
            list: true,
            search: true,
            members: true,
            find_direct: true,
            recent_activity: true,
        },
        messages: MessageCaps {
            list: true,
            get: true,
            forwarded: false,
        },
        people: PeopleCaps {
            contacts: false,
            get: true,
            search: true,
            bots: false,
            batch_max: None,
        },
        send: Some(SendCaps {
            text: true,
            mentions: true,
            mention_all: true,
            image: true,
            file: true,
            lookup: false,
        }),
        recall: true,
        edit: false,
        // 发送时能带的格式（协议 v40）：全部；写法见 `feishu::post_send`。飞书不能以本人身份编辑消息，不声明带格式改。
        rich_send: RichSendCaps {
            formats: vec![
                RichFormat::Strong,
                RichFormat::Emphasis,
                RichFormat::Underline,
                RichFormat::Delete,
                RichFormat::InlineCode,
                RichFormat::Link,
                RichFormat::Blockquote,
                RichFormat::List,
                RichFormat::Code,
            ],
            edit: false,
        },
        reactions: ReactionCaps {
            list: true,
            add: true,
            remove: true,
            catalog: true,
        },
        resources: ResourceCaps {
            variants: true,
            read: true,
            avatars: true,
        },
        events: Default::default(),
        retranslate: false,
        message_maintenance: false,
        session: SessionCaps { renew: true },
        // 不跨账号发收伙伴署名、不收宿主头像、不自报性能。
        agent_attribution: Default::default(),
        account: Default::default(),
        perf: false,
    }
}

/// 两种登录方式，叫法与 HHH 内置飞书的登录面板一样（`facts::login`、`facts::app`）。回调路径写相对的 `callback`：
/// 宿主拼成 `/hhh-friend-im/feishu/callback`，就是用户已经登记在飞书后台的那个地址。
pub(crate) fn login_methods() -> Vec<LoginMethod> {
    vec![
        LoginMethod {
            id: login::RELAY_MODE.into(),
            kind: LoginKind::Relay,
            label: login::RELAY_NAME.into(),
            note: login::RELAY_NOTE.into(),
            placeholder: Some(login::RELAY_FIELD_PLACEHOLDER.into()),
            // 同内置（im-auth-relay v6）：没登录过的人中继框不预填；交接来的照旧记着用过的地址（`adoptedSettings`）。
            default_value: None,
            id_label: None,
            id_placeholder: None,
            secret_label: None,
            secret_placeholder: None,
            secret_help: None,
            callback_path: None,
            callback_help: None,
            create_link: None,
        },
        LoginMethod {
            id: login::OWN_MODE.into(),
            kind: LoginKind::OwnApp,
            label: login::OWN_NAME.into(),
            note: login::OWN_NOTE.into(),
            placeholder: None,
            default_value: None,
            id_label: Some(app::ID_LABEL.into()),
            id_placeholder: Some(app::ID_PLACEHOLDER.into()),
            secret_label: Some(app::SECRET_LABEL.into()),
            secret_placeholder: Some(app::SECRET_PLACEHOLDER.into()),
            secret_help: Some(app::SECRET_HELP.into()),
            callback_path: Some("callback".into()),
            callback_help: Some(app::CALLBACK_HELP.into()),
            create_link: Some(CreateLink {
                label: login::CREATE_LINK_LABEL.into(),
                url: login::CREATE_LINK_URL.into(),
                after: login::CREATE_LINK_AFTER.into(),
            }),
        },
    ]
}

/// `initialize` 的结果。宿主只说得出别的协议版本时拒绝。
pub(crate) fn initialize(params: pm::InitializeParams) -> Result<Value, RpcError> {
    if !params
        .protocol_versions
        .contains(&hhh_im_protocol::PROTOCOL_VERSION)
    {
        return Err(errors::of(
            ErrorKind::Unsupported,
            "飞书插件只支持 HHH-IM-Protocol v1，请更新 HHH。",
        ));
    }
    Ok(result())
}

/// `initialize` 的结果（不看宿主参数的那部分）。
pub(crate) fn result() -> Value {
    serde_json::to_value(pm::InitializeResult {
        protocol_version: hhh_im_protocol::PROTOCOL_VERSION,
        provider: pm::ProviderInfo {
            id: PROVIDER_ID.into(),
            name: facts::INFO.name.into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        platform: platform(),
        capabilities: capabilities(),
        settings: Vec::new(),
        login_methods: login_methods(),
        // 读取不经宿主逐条放行，没有插件操作。
        read_request_gating: false,
        actions: Vec::new(),
    })
    .expect("握手结果总能写成 JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FeishuClient;

    /// HHH 契约 provider-protocol v31：写进文字的写法与正文写法对得上——目录里能写进文字的，按写法写出来，正文写法整段认出、
    /// 认出的就是原来的名字（HHH 的自检工具 hhh-im-check 也这样查）。
    #[test]
    fn the_text_template_round_trips_through_the_text_pattern() {
        let declared = result();
        let emoji = &declared["platform"]["emoji"];
        assert_eq!(emoji["textTemplate"], "[{name}]");
        let pattern = regex_lite::Regex::new(emoji["textPattern"].as_str().unwrap()).unwrap();
        let catalog = facts::complete_emoji_catalog(crate::EmojiCatalog {
            emojis: Vec::new(),
            source_updated_ms: 1,
            fetched_at_ms: 1,
            categories: Vec::new(),
            tones: Vec::new(),
        });
        let mut checked = 0;
        for emoji in catalog
            .emojis
            .iter()
            .filter(|e| !facts::NOT_IN_TEXT.contains(&e.emoji_type.as_str()))
        {
            let Some(name) = emoji.name.as_deref() else {
                continue;
            };
            let text = facts::EMOJI_TEXT_TEMPLATE.replace("{name}", name);
            let found = pattern.captures(&text).expect("认得出");
            assert_eq!(
                (&found[0], &found[1]),
                (text.as_str(), name),
                "{}",
                emoji.emoji_type
            );
            checked += 1;
        }
        // 开放平台的那份目录要联网读，这里查随包快照里有名字的（客户端专有表情与中文名）。
        assert!(checked > 50, "随包快照里有名字的都查了：{checked}");
    }

    /// 宿主按声明认头像身份（hhh-im `Platform::avatar_identity_of`）：编号对不上时整个键就是身份，变体对不上时没有变体。
    fn declared_identity(key: &str) -> (String, String) {
        let first = |pattern: &str| {
            regex_lite::Regex::new(pattern)
                .expect("宿主编得出这个写法")
                .captures(key)
                .and_then(|captures| captures.get(1))
                .map(|found| found.as_str().to_owned())
        };
        (
            first(AVATAR_KEY_PATTERN).unwrap_or_else(|| key.to_owned()),
            first(AVATAR_VARIANT_PATTERN).unwrap_or_default(),
        )
    }

    /// 真实形状的头像地址（编号是编的）：同一个头像换了 CDN 主机、参数换了顺序、换了尺寸。
    const SAME_AVATAR: &[&str] = &[
        "https://s1-imfile.feishucdn.com/static-resource/v1/v3_00aa_0a1b2c3d-0000-4000-8000-00000000000g~?image_size=72x72&cut_type=&quality=&format=png&sticker_format=.webp",
        "https://s3-imfile.feishucdn.com/static-resource/v1/v3_00aa_0a1b2c3d-0000-4000-8000-00000000000g~?image_size=72x72&cut_type=&quality=&format=png&sticker_format=.webp",
        "https://p3-lark-file.pstatp.com/static-resource/v1/v3_00aa_0a1b2c3d-0000-4000-8000-00000000000g~?format=png&image_size=72x72",
        "https://S9-IMFILE.FEISHUCDN.COM/static-resource/v1/v3_00aa_0a1b2c3d-0000-4000-8000-00000000000g~?image_size=72x72",
    ];

    #[test]
    fn declared_avatar_identity_matches_the_builtin_on_every_cdn_host() {
        let first = declared_identity(SAME_AVATAR[0]);
        assert_eq!(
            first,
            (
                "v3_00aa_0a1b2c3d-0000-4000-8000-00000000000g~".to_owned(),
                "72x72".to_owned()
            )
        );
        for url in SAME_AVATAR {
            assert_eq!(
                declared_identity(url),
                first,
                "换了主机还是同一个头像：{url}"
            );
            assert_eq!(
                Some(declared_identity(url)),
                FeishuClient::avatar_identity(url),
                "与内置飞书认法一致：{url}"
            );
        }
        // 换尺寸：编号不变、变体跟着变（各下一份），与内置一致。
        let bigger = SAME_AVATAR[1].replace("72x72", "240x240");
        assert_eq!(
            declared_identity(&bigger),
            (first.0.clone(), "240x240".to_owned())
        );
        assert_eq!(
            Some(declared_identity(&bigger)),
            FeishuClient::avatar_identity(&bigger)
        );
        // 没有尺寸参数：没有变体，与内置一致。
        let plain = "https://s1-imfile.feishucdn.com/static-resource/v1/v3_00aa_x~";
        assert_eq!(
            Some(declared_identity(plain)),
            FeishuClient::avatar_identity(plain)
        );
        // 路径里有好几段 v3_：同内置取第一段。
        let nested = "https://s1-imfile.feishucdn.com/v3_first/v3_second?image_size=72x72";
        assert_eq!(
            Some(declared_identity(nested)),
            FeishuClient::avatar_identity(nested)
        );
    }

    #[test]
    fn avatars_off_the_feishu_cdn_are_their_whole_key() {
        // 内置不认（不是飞书 CDN、不是 https）；声明的写法也对不上编号，宿主拿整个键当身份，不会认错成别的头像。
        for url in [
            "https://example.com/static-resource/v1/v3_00aa_x~?image_size=72x72",
            "http://s1-imfile.feishucdn.com/static-resource/v1/v3_00aa_x~?image_size=72x72",
            "https://feishucdn.com.example.com/v3_00aa_x~",
        ] {
            assert_eq!(FeishuClient::avatar_identity(url), None, "{url}");
            assert_eq!(declared_identity(url).0, url, "{url}");
        }
    }

    #[test]
    fn login_methods_carry_the_builtin_texts() {
        let methods = login_methods();
        assert_eq!(
            methods[0].default_value, None,
            "中继框不预填（同内置 im-auth-relay v6）"
        );
        assert_eq!(
            methods[1].secret_help.as_deref(),
            Some("飞书开放平台 → 凭证与基础信息。只保存在本机。")
        );
    }
}
