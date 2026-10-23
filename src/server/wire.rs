//! 适配器的统一类型 → 协议的线上类型（protocol-v1 §7、附录 B）。
//!
//! 这里的每一步都是宿主 `hhh-im/src/provider/translate.rs` 的反方向：宿主把线上的消息翻回来，要和 HHH 内置飞书时
//! 存下的一字不差（`tests/fixtures/feishu-protocol/` 的样例锁住这一点）。协议里没有连接编号，消息、会话、人的引用
//! 只留平台编号，宿主收到时自己补上；格式正文与卡片里的人和资源按规范的写法给（§7.6、附录 B），其余节点与内部写法相同。

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hhh_im_protocol::method as pm;
use hhh_im_protocol::rpc::RpcError;
use hhh_im_protocol::types as wire;
use serde_json::{Map, Value};

use crate::*;

/// 账号。`tenant` 不给：协议里它是给人看的工作区名（卡片写“账号 · 工作区”），飞书只有一串租户编号，
/// HHH 内置飞书的卡片也不写它。
pub(crate) fn account(account: &Account) -> wire::Account {
    wire::Account {
        id: account.user.id.clone(),
        id_type: Some(account.user.id_type.clone()),
        name: account.user.name.clone().unwrap_or_default(),
        avatar: account.avatar_url.clone(),
        tenant: None,
    }
}

/// 会话。头像键就是飞书给的头像地址（与内置时代本机记下的一样，读头像时宿主交还）。
pub(crate) fn conversation(conversation: &Conversation) -> wire::Conversation {
    wire::Conversation {
        id: conversation.reference.id.clone(),
        kind: conversation_kind(&conversation.kind),
        name: conversation.name.clone(),
        avatar: conversation.avatar_url.clone(),
        peer: conversation.peer.as_ref().map(|peer| peer.id.clone()),
        private: conversation.private,
    }
}

pub(crate) fn conversation_kind(kind: &ConversationKind) -> wire::ConversationKind {
    match kind {
        ConversationKind::Group => wire::ConversationKind::Group,
        ConversationKind::Direct => wire::ConversationKind::Direct,
        ConversationKind::Channel => wire::ConversationKind::Channel,
        ConversationKind::Unknown => wire::ConversationKind::Unknown,
    }
}

/// 搜到的、读到的一个人（飞书用户编号都是 open_id）。
pub(crate) fn person(user: &SearchUser) -> wire::Person {
    wire::Person {
        id: user.reference.id.clone(),
        id_type: Some("open_id".into()),
        kind: wire::PersonKind::User,
        name: Some(user.name.clone()).filter(|name| !name.is_empty()),
        avatar: user.avatar_url.clone(),
    }
}

/// 应用（机器人）在内部记成 [`APP_SENDER_KIND`]，协议里叫 `bot`。
fn is_app(kind: &str) -> bool {
    kind == APP_SENDER_KIND
}

/// 提及的是不是机器人：内部记成应用的，和飞书提及里原样带来的 `mentioned_type = bot`（内置照原样记 `bot`，界面
/// 当人显示；协议只分人和机器人，这里按机器人写，宿主记成应用）。
fn mentions_bot(kind: &str) -> bool {
    is_app(kind) || kind == "bot"
}

/// 一个人或应用的身份 → 协议的 Person（表情的操作者）。
fn actor_person(actor: &ActorRef) -> wire::Person {
    wire::Person {
        id: actor.id.clone(),
        id_type: Some(actor.id_type.clone()),
        kind: if is_app(&actor.kind) {
            wire::PersonKind::Bot
        } else {
            wire::PersonKind::User
        },
        name: actor.name.clone(),
        avatar: None,
    }
}

/// 发送者。飞书的发送者种类还有 `anonymous`、`unknown`：协议只有 user / bot / system，按真人给。
fn sender(actor: &ActorRef) -> wire::Sender {
    wire::Sender {
        id: actor.id.clone(),
        id_type: Some(actor.id_type.clone()),
        kind: match actor.kind.as_str() {
            SYSTEM_SENDER_KIND => wire::SenderKind::System,
            kind if is_app(kind) => wire::SenderKind::Bot,
            _ => wire::SenderKind::User,
        },
        name: actor.name.clone(),
    }
}

/// 一条消息。
pub(crate) fn message(message: &ImMessage) -> wire::Message {
    let has_thread = message.root_message.is_some()
        || message.thread_id.is_some()
        || message.in_thread.is_some()
        || message.broadcast
        || message.thread_activity.is_some()
        || message.thread_copy.is_some();
    wire::Message {
        id: message.reference.id.clone(),
        conversation_id: message.reference.conversation.id.clone(),
        sender: sender(&message.sender),
        created_at_ms: message.created_at_ms,
        updated_at_ms: message.updated_at_ms,
        edited_at_ms: message.edited_at_ms,
        deleted: message.deleted,
        incomplete: message.incomplete,
        content: message.content.iter().map(block).collect(),
        reply_to: message.reply_to.as_ref().map(|reply| reply.id.clone()),
        thread: has_thread.then(|| wire::ThreadInfo {
            id: message.thread_id.clone(),
            root_id: message.root_message.as_ref().map(|root| root.id.clone()),
            in_thread: message.in_thread,
            broadcast: message.broadcast,
            activity: message
                .thread_activity
                .as_ref()
                .map(|activity| wire::ThreadActivity {
                    latest_reply_id: activity.latest_reply_id.clone(),
                    latest_reply_at_ms: activity.latest_reply_at_ms,
                    reply_count: activity.reply_count,
                }),
            copy: message.thread_copy.as_ref().map(|copy| wire::ThreadCopy {
                role: match copy.role {
                    ThreadCopyRole::InConversation => wire::ThreadCopyRole::Conversation,
                    ThreadCopyRole::InThread => wire::ThreadCopyRole::Thread,
                },
                peer_id: copy.peer.clone(),
                thread_id: copy.thread_id.clone(),
            }),
        }),
        reactions: message
            .reactions
            .as_ref()
            .map(|reactions| reactions.iter().map(reaction).collect()),
        raw: None,
        edit_draft: message
            .edit_source
            .as_ref()
            .map(|draft| draft.iter().map(outgoing).collect()),
        agent_attribution: None,
    }
}

/// 一个内容块。
fn block(content: &ImContent) -> wire::Block {
    match content {
        ImContent::Text { text } => wire::Block::Text { text: text.clone() },
        ImContent::Mention { actor, everyone } => wire::Block::Mention {
            user_id: actor.as_ref().map(|actor| actor.id.clone()),
            id_type: actor.as_ref().map(|actor| actor.id_type.clone()),
            // 提及的是机器人（内置记成应用）写 `bot`，宿主记回应用、先用这里的名字；其余是人。
            kind: actor.as_ref().map(|actor| {
                if mentions_bot(&actor.kind) {
                    wire::PersonKind::Bot
                } else {
                    wire::PersonKind::User
                }
            }),
            name: actor.as_ref().and_then(|actor| actor.name.clone()),
            everyone: *everyone,
        },
        ImContent::Resource {
            resource,
            name,
            size,
            video,
        } => {
            // 视频的宽高是画面宽高（在视频信息里），图片的在资源上；宿主按同一规则翻回来。
            let shown = match video {
                Some(video) => video.size,
                None => *size,
            };
            wire::Block::Resource {
                resource: wire::Resource {
                    key: resource.key.clone(),
                    kind: resource_kind(resource.kind),
                    // 宿主认视频看 `mime` 是视频或带了封面；飞书的视频消息有时没有封面，按文件名给一个视频 MIME，
                    // 让宿主照样认出是视频（内部不记 MIME，翻回来不多出东西）。
                    mime: video.as_ref().map(|_| video_mime(name.as_deref())),
                    bytes: None,
                    duration_ms: video.as_ref().and_then(|video| video.duration_ms),
                    cover: video
                        .as_ref()
                        .and_then(|video| video.cover.as_ref())
                        .map(|cover| wire::ResourceCover {
                            key: cover.resource.key.clone(),
                            width: cover.size.map(|size| size.width),
                            height: cover.size.map(|size| size.height),
                        }),
                },
                name: name.clone(),
                width: shown.map(|size| size.width),
                height: shown.map(|size| size.height),
            }
        }
        ImContent::Unsupported { kind, label } => wire::Block::Unsupported {
            kind: kind.clone(),
            label: label.clone(),
        },
        ImContent::Card(card) => wire::Block::Card {
            card: card_out(card),
        },
        ImContent::Rich(rich) => wire::Block::Rich {
            root: rich_out(rich),
        },
    }
}

/// 视频的 MIME：按扩展名，认不出时按 MP4。
fn video_mime(name: Option<&str>) -> String {
    let extension = name
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "mov" => "video/quicktime",
        "m4v" => "video/x-m4v",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "3gp" => "video/3gpp",
        "wmv" => "video/x-ms-wmv",
        "flv" => "video/x-flv",
        "mpg" | "mpeg" => "video/mpeg",
        _ => "video/mp4",
    }
    .to_owned()
}

pub(crate) fn resource_kind(kind: ResourceKind) -> wire::ResourceKind {
    match kind {
        ResourceKind::Image => wire::ResourceKind::Image,
        ResourceKind::File => wire::ResourceKind::File,
        ResourceKind::Sticker => wire::ResourceKind::Sticker,
    }
}

pub(crate) fn resource_kind_in(kind: wire::ResourceKind) -> ResourceKind {
    match kind {
        wire::ResourceKind::Image => ResourceKind::Image,
        wire::ResourceKind::File => ResourceKind::File,
        wire::ResourceKind::Sticker => ResourceKind::Sticker,
    }
}

/// 格式正文：内部写法的 JSON，再把人与资源改成规范的写法（宿主 `rich_root_in` 的反方向）。
fn rich_out(rich: &ImRich) -> Value {
    let mut root = serde_json::to_value(&rich.root).unwrap_or(Value::Null);
    if let Some(children) = root.get_mut("children") {
        rich_nodes_out(children);
    }
    root
}

/// 卡片：块里的格式正文、图片组、人员组改成规范的写法（宿主 `card_in` 的反方向）。
fn card_out(card: &ImCard) -> Value {
    let mut card = serde_json::to_value(card).unwrap_or(Value::Null);
    if let Some(blocks) = card.get_mut("blocks") {
        card_blocks_out(blocks);
    }
    card
}

fn rich_nodes_out(nodes: &mut Value) {
    let Some(nodes) = nodes.as_array_mut() else {
        return;
    };
    for node in nodes {
        let Some(object) = node.as_object_mut() else {
            continue;
        };
        match object.get("type").and_then(Value::as_str) {
            // `mention{actor, everyone}` → `mention{userId, name, idType, kind, everyone}`；提及所有人没有 actor，原样。
            // `person{actor}` → `person{userId, name, idType, kind}`。
            Some("mention" | "person") => {
                if let Some(actor) = object.remove("actor") {
                    put_actor(object, &actor);
                }
            }
            Some("resource") => resource_out(object),
            _ => {}
        }
        if let Some(children) = object.get_mut("children") {
            rich_nodes_out(children);
        }
    }
}

fn card_blocks_out(blocks: &mut Value) {
    let Some(blocks) = blocks.as_array_mut() else {
        return;
    };
    for block in blocks {
        let Some(object) = block.as_object_mut() else {
            continue;
        };
        match object.get("type").and_then(Value::as_str) {
            Some("rich") => {
                if let Some(children) = object.get_mut("children") {
                    rich_nodes_out(children);
                }
            }
            Some("columns") => {
                if let Some(columns) = object.get_mut("columns").and_then(Value::as_array_mut) {
                    for column in columns {
                        card_blocks_out(column);
                    }
                }
            }
            Some("panel") => {
                if let Some(title) = object.get_mut("title") {
                    rich_nodes_out(title);
                }
                if let Some(inner) = object.get_mut("blocks") {
                    card_blocks_out(inner);
                }
            }
            Some("images") => {
                if let Some(images) = object.get_mut("images").and_then(Value::as_array_mut) {
                    for image in images.iter_mut().filter_map(Value::as_object_mut) {
                        resource_out(image);
                    }
                }
            }
            Some("people") => {
                if let Some(people) = object.get_mut("people").and_then(Value::as_array_mut) {
                    for person in people.iter_mut() {
                        let mut entry = Map::new();
                        put_actor(&mut entry, person);
                        *person = Value::Object(entry);
                    }
                }
            }
            _ => {}
        }
    }
}

/// 内部的 [`ActorRef`] JSON → 规范写在人身上的几项：`userId`、`name`、`idType`、`kind`（应用是 `bot`）。
fn put_actor(object: &mut Map<String, Value>, actor: &Value) {
    let text = |key: &str| actor.get(key).and_then(Value::as_str);
    object.insert("userId".into(), text("id").unwrap_or_default().into());
    if let Some(name) = text("name") {
        object.insert("name".into(), name.into());
    }
    if let Some(id_type) = text("idType") {
        object.insert("idType".into(), id_type.into());
    }
    let kind = if text("kind").is_some_and(mentions_bot) {
        "bot"
    } else {
        "user"
    };
    object.insert("kind".into(), kind.into());
}

/// 资源节点（格式正文的 `resource`、卡片图片组的一张）：引用只留 `{key, kind}`（宿主按所在的消息重建），
/// 宽高从 `size` 拿出来写成 `width`、`height`。
fn resource_out(object: &mut Map<String, Value>) {
    if let Some(resource) = object.get_mut("resource").and_then(Value::as_object_mut) {
        resource.remove("message");
    }
    if let Some(size) = object.remove("size") {
        for side in ["width", "height"] {
            if let Some(value) = size.get(side) {
                object.insert(side.into(), value.clone());
            }
        }
    }
}

/// 一条表情回复。
pub(crate) fn reaction(reaction: &MessageReaction) -> wire::Reaction {
    wire::Reaction {
        id: reaction.id.clone(),
        conversation_id: reaction.message.conversation.id.clone(),
        message_id: reaction.message.id.clone(),
        operator: actor_person(&reaction.operator),
        emoji_type: reaction.emoji_type.clone(),
        at_ms: reaction.action_time_ms,
    }
}

/// 表情目录里的一个表情。图片地址不交给宿主：宿主按类型向 `emoji/image` 要图。肤色照内置带上：飞书的肤色版本
/// 只写肤色名字（如“深色”）、不进面板，不说是哪个表情的肤色（`toneOf` 为空）。
pub(crate) fn emoji(emoji: &EmojiInfo) -> wire::Emoji {
    wire::Emoji {
        emoji_type: emoji.emoji_type.clone(),
        name: emoji.name.clone(),
        panel: emoji.panel,
        unicode: emoji.unicode.clone(),
        category: emoji.category.clone(),
        aliases: emoji.aliases.clone(),
        tone: emoji.tone.clone(),
        tone_of: emoji.tone_of.as_ref().map(|of| wire::ToneOf {
            base: of.base.clone(),
            tone: of.tone.clone(),
        }),
        in_text: true,
    }
}

/// `emoji/catalog` 的 JSON：写不进文字的表情（[`crate::facts::NOT_IN_TEXT`]）带 `inText: false`（协议 v31）。
pub(crate) fn emoji_catalog_json(catalog: &EmojiCatalog) -> Value {
    serde_json::to_value(emoji_catalog(catalog)).expect("表情目录总能写成 JSON")
}

/// 表情目录（分类、肤色照目录的顺序）。
pub(crate) fn emoji_catalog(catalog: &EmojiCatalog) -> pm::EmojiCatalogResult {
    pm::EmojiCatalogResult {
        emojis: catalog
            .emojis
            .iter()
            .map(|item| wire::Emoji {
                in_text: !crate::facts::NOT_IN_TEXT.contains(&item.emoji_type.as_str()),
                ..emoji(item)
            })
            .collect(),
        categories: catalog
            .categories
            .iter()
            .map(|category| wire::EmojiCategory {
                id: category.id.clone(),
                name: category.name.clone(),
                icon: None,
            })
            .collect(),
        tones: catalog
            .tones
            .iter()
            .map(|tone| wire::EmojiTone {
                id: tone.id.clone(),
                name: tone.name.clone(),
            })
            .collect(),
        updated_at_ms: catalog.source_updated_ms,
    }
}

/// 会话发现的一项（只用来认会话，资料宿主另读）。
pub(crate) fn activity(item: &ActiveConversation) -> pm::ActivityItem {
    pm::ActivityItem {
        conversation_id: item.conversation.id.clone(),
        kind: conversation_kind(&item.kind),
        first_ms: item.first_ms,
        latest_ms: item.latest_ms,
        peer_id: item.peer.as_ref().map(|peer| peer.id.clone()),
        conversation: None,
    }
}

/// 发送结果。
pub(crate) fn send_outcome(outcome: &SendOutcome) -> wire::SendOutcome {
    match outcome {
        SendOutcome::Accepted { message: m, .. } => wire::SendOutcome::Accepted {
            message: Box::new(message(m)),
        },
        SendOutcome::Unknown { reason, .. } => wire::SendOutcome::Unknown {
            reason: reason.clone(),
        },
    }
}

/// 内部发送内容 → 线上（编辑原文用；飞书不给编辑原文，留着与宿主对称）。
fn outgoing(content: &OutgoingContent) -> wire::Outgoing {
    match content {
        OutgoingContent::Text { text } => wire::Outgoing::Text { text: text.clone() },
        OutgoingContent::Mention { user_id, name } => wire::Outgoing::Mention {
            user_id: user_id.clone(),
            name: name.clone(),
        },
        OutgoingContent::MentionAll => wire::Outgoing::MentionAll,
        OutgoingContent::Image { key } => wire::Outgoing::Image { key: key.clone() },
        OutgoingContent::File { key } => wire::Outgoing::File { key: key.clone() },
    }
}

/// 线上发送内容 → 内部。
pub(crate) fn outgoing_in(content: wire::Outgoing) -> OutgoingContent {
    match content {
        wire::Outgoing::Text { text } => OutgoingContent::Text { text },
        wire::Outgoing::Mention { user_id, name } => OutgoingContent::Mention { user_id, name },
        wire::Outgoing::MentionAll => OutgoingContent::MentionAll,
        wire::Outgoing::Image { key } => OutgoingContent::Image { key },
        wire::Outgoing::File { key } => OutgoingContent::File { key },
    }
}

/// 线上发送目标 → 内部（补上插件自己的连接编号）。
pub(crate) fn target_in(target: wire::SendTarget, connection: &str) -> SendTarget {
    match target {
        wire::SendTarget::Conversation { conversation_id } => SendTarget::Conversation {
            conversation: ConversationRef::new(connection, conversation_id),
        },
        wire::SendTarget::Reply {
            conversation_id,
            message_id,
            in_thread,
            thread_root,
            also_to_conversation,
        } => SendTarget::Reply {
            message: MessageRef {
                conversation: ConversationRef::new(connection, conversation_id),
                id: message_id,
            },
            in_thread,
            thread_root,
            also_to_conversation,
        },
    }
}

/// 分页游标 → 协议的不透明字符串：适配器的游标（平台位置 + 查询范围）编成 base64 的 JSON。
pub(crate) fn cursor_out(cursor: Option<&PageCursor>) -> Option<String> {
    cursor
        .and_then(|cursor| serde_json::to_vec(cursor).ok())
        .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
}

/// 协议的游标 → 适配器的游标；读不懂时是参数错误。
pub(crate) fn cursor_in(cursor: Option<&str>) -> Result<Option<PageCursor>, RpcError> {
    let Some(cursor) = cursor.filter(|cursor| !cursor.is_empty()) else {
        return Ok(None);
    };
    URL_SAFE_NO_PAD
        .decode(cursor)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .map(Some)
        .ok_or_else(|| {
            super::errors::of(
                ErrorKind::InvalidRequest,
                "分页游标不属于当前账号、会话或查询条件。",
            )
        })
}

// 测试用：宿主那一侧把线上消息翻回内部类型的代码（照抄 hhh-im 的 provider/translate.rs），核对这里是它的反方向。
#[cfg(test)]
pub(crate) mod host_inverse;

// 测试用：协议样例（`tests/fixtures/feishu-protocol/`）的生成与核对。
#[cfg(test)]
mod goldens;
