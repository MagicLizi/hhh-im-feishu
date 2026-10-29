//! 测试用：宿主（HHH 的 hhh-im `provider/translate.rs`，契约 provider-protocol v14）把线上类型翻回内部类型的那一半，
//! 只覆盖插件旧内核能表达的内容（表情一项在宿主的 `provider.rs`）。上一级模块对这些内容须满足：内部 → 线上 → 这里 → 内部，一字不差。
//! 转发快照由协议边界直接组装，不经过旧内核；用 `server::forwarded` 的协议测试核对，不能投影成普通卡片。

use hhh_im_protocol::types as wire;

use crate::*;

/// 外部平台身份编号的命名空间：插件没说编号是哪一种（`idType`）时记成这个。
const EXT_ID_TYPE: &str = "ext";

fn id_type_of(given: Option<String>) -> String {
    given.unwrap_or_else(|| EXT_ID_TYPE.to_owned())
}

/// 翻译核验账号。
pub(crate) fn account(connection_id: &str, platform: &str, a: wire::Account) -> Account {
    Account {
        connection_id: connection_id.to_owned(),
        platform: platform.to_owned(),
        user: ActorRef {
            id: a.id,
            id_type: id_type_of(a.id_type),
            kind: "user".to_owned(),
            name: (!a.name.is_empty()).then_some(a.name),
        },
        tenant_id: a.tenant,
        avatar_url: a.avatar,
    }
}

/// 翻译会话。
pub(crate) fn conversation(connection_id: &str, c: wire::Conversation) -> Conversation {
    Conversation {
        reference: ConversationRef::new(connection_id, &c.id),
        name: c.name,
        kind: conversation_kind(c.kind),
        avatar_url: c.avatar,
        peer: c.peer.map(|id| UserRef {
            connection_id: connection_id.to_owned(),
            id,
        }),
        private: c.private,
    }
}

fn conversation_kind(k: wire::ConversationKind) -> ConversationKind {
    match k {
        wire::ConversationKind::Group => ConversationKind::Group,
        wire::ConversationKind::Direct => ConversationKind::Direct,
        wire::ConversationKind::Channel => ConversationKind::Channel,
        wire::ConversationKind::Unknown => ConversationKind::Unknown,
    }
}

fn actor(p: wire::Person) -> ActorRef {
    ActorRef {
        id: p.id,
        id_type: id_type_of(p.id_type),
        kind: match p.kind {
            wire::PersonKind::Bot => APP_SENDER_KIND.to_owned(),
            wire::PersonKind::User => "user".to_owned(),
        },
        name: p.name,
    }
}

fn sender(s: wire::Sender) -> ActorRef {
    ActorRef {
        id: s.id,
        id_type: id_type_of(s.id_type),
        kind: match s.kind {
            wire::SenderKind::System => SYSTEM_SENDER_KIND.to_owned(),
            wire::SenderKind::Bot => APP_SENDER_KIND.to_owned(),
            wire::SenderKind::User => "user".to_owned(),
        },
        name: s.name,
    }
}

/// 翻译一条消息。认不出的内容块标成「暂不支持」并把整条标不完整。
pub(crate) fn message(connection_id: &str, m: wire::Message) -> ImMessage {
    let conversation = ConversationRef::new(connection_id, &m.conversation_id);
    let reference = MessageRef {
        conversation: conversation.clone(),
        id: m.id,
    };
    let mut incomplete = m.incomplete;
    let content: Vec<ImContent> = m
        .content
        .into_iter()
        .map(|b| block(&reference, b, &mut incomplete))
        .collect();
    let reply_to = m.reply_to.map(|id| MessageRef {
        conversation: conversation.clone(),
        id,
    });
    let (root_message, thread_id, in_thread, broadcast, thread_activity, thread_copy) =
        match m.thread {
            Some(t) => (
                t.root_id.map(|id| MessageRef {
                    conversation: conversation.clone(),
                    id,
                }),
                t.id,
                t.in_thread,
                t.broadcast,
                t.activity.map(|a| ThreadActivity {
                    latest_reply_id: a.latest_reply_id,
                    latest_reply_at_ms: a.latest_reply_at_ms,
                    reply_count: a.reply_count,
                }),
                t.copy
                    .filter(|copy| !copy.peer_id.is_empty())
                    .map(|copy| ThreadCopy {
                        role: match copy.role {
                            wire::ThreadCopyRole::Conversation => ThreadCopyRole::InConversation,
                            wire::ThreadCopyRole::Thread => ThreadCopyRole::InThread,
                        },
                        peer: copy.peer_id,
                        thread_id: copy.thread_id,
                    }),
            ),
            None => (None, None, None, false, None, None),
        };
    let raw = m.raw.map(|raw| {
        Box::new(RawData {
            format: raw.format,
            data: raw.data,
        })
    });
    let edit_source = m
        .edit_draft
        .filter(|draft| !draft.is_empty())
        .map(|draft| draft.into_iter().filter_map(outgoing_in).collect());
    let reactions = m
        .reactions
        .map(|rs| rs.into_iter().map(|r| reaction(connection_id, r)).collect());
    ImMessage {
        reference,
        sender: sender(m.sender),
        created_at_ms: m.created_at_ms,
        updated_at_ms: m.updated_at_ms,
        content,
        reply_to,
        root_message,
        thread_id,
        in_thread,
        deleted: m.deleted,
        incomplete,
        edited_at_ms: m.edited_at_ms,
        broadcast,
        reactions,
        thread_copy,
        thread_activity,
        edit_source,
        raw: RawSlot(raw),
    }
}

fn outgoing_in(content: wire::Outgoing) -> Option<OutgoingContent> {
    match content {
        wire::Outgoing::Text { text } => Some(OutgoingContent::Text { text }),
        wire::Outgoing::Mention { user_id, name } => {
            Some(OutgoingContent::Mention { user_id, name })
        }
        wire::Outgoing::MentionAll => Some(OutgoingContent::MentionAll),
        // 宿主把带格式的编辑原文读成格式正文（v40）；飞书不给编辑原文，照宿主的形状收下。
        wire::Outgoing::Rich { root } => hhh_im_protocol::content::rich_nodes(&root)
            .ok()
            .map(|children| OutgoingContent::Rich { children }),
        wire::Outgoing::Image { .. }
        | wire::Outgoing::File { .. }
        | wire::Outgoing::Sticker { .. } => None,
    }
}

fn block(message: &MessageRef, b: wire::Block, incomplete: &mut bool) -> ImContent {
    match b {
        // 这个逆变换只核对迁出前内核已经表达过的内容；新增快照由协议级测试单独核对。
        wire::Block::Forwarded { .. } => {
            panic!("转发快照不能放进旧内核模型的对拍，请使用转发记录的协议测试")
        }
        wire::Block::Text { text } => ImContent::Text { text },
        wire::Block::Mention {
            user_id,
            id_type,
            kind,
            name,
            everyone,
        } => ImContent::Mention {
            actor: (!everyone).then(|| ActorRef {
                id: user_id.unwrap_or_default(),
                id_type: id_type_of(id_type),
                // 机器人记成应用（v14）：界面先用提及里带的名字，不当成人去查人名册。
                kind: match kind {
                    Some(wire::PersonKind::Bot) => APP_SENDER_KIND.to_owned(),
                    _ => "user".to_owned(),
                },
                name,
            }),
            everyone,
        },
        wire::Block::Resource {
            resource,
            name,
            width,
            height,
        } => {
            let size = match (width, height) {
                (Some(width), Some(height)) => Some(ImageSize { width, height }),
                _ => None,
            };
            let video = video_of(message, &resource, size);
            ImContent::Resource {
                resource: ResourceRef {
                    message: message.clone(),
                    key: resource.key,
                    kind: resource_kind(resource.kind),
                },
                name,
                size: if video.is_some() { None } else { size },
                video,
            }
        }
        wire::Block::Rich { root } => {
            match serde_json::from_value::<RichRoot>(rich_root_in(message, root)) {
                Ok(root) => ImContent::Rich(ImRich { root }),
                Err(_) => {
                    *incomplete = true;
                    ImContent::Unsupported {
                        kind: "rich".to_owned(),
                        label: None,
                    }
                }
            }
        }
        wire::Block::Markdown { text } => ImContent::Text { text },
        wire::Block::Card { card } => match serde_json::from_value(card_in(message, card)) {
            Ok(card) => ImContent::Card(card),
            Err(_) => {
                *incomplete = true;
                ImContent::Unsupported {
                    kind: "card".to_owned(),
                    label: None,
                }
            }
        },
        wire::Block::Unsupported { kind, label } => {
            *incomplete = true;
            ImContent::Unsupported { kind, label }
        }
    }
}

fn video_of(
    message: &MessageRef,
    resource: &wire::Resource,
    size: Option<ImageSize>,
) -> Option<VideoInfo> {
    let is_video = resource.kind == wire::ResourceKind::File
        && (resource.mime.as_deref().is_some_and(is_video_mime) || resource.cover.is_some());
    is_video.then(|| VideoInfo {
        cover: resource
            .cover
            .as_ref()
            .filter(|cover| !cover.key.is_empty())
            .map(|cover| VideoCover {
                resource: ResourceRef {
                    message: message.clone(),
                    key: cover.key.clone(),
                    kind: ResourceKind::Image,
                },
                size: match (cover.width, cover.height) {
                    (Some(width), Some(height)) if width > 0 && height > 0 => {
                        Some(ImageSize { width, height })
                    }
                    _ => None,
                },
            }),
        duration_ms: resource.duration_ms.filter(|ms| *ms > 0),
        size,
    })
}

fn rich_root_in(message: &MessageRef, mut root: serde_json::Value) -> serde_json::Value {
    if let Some(children) = root.get_mut("children") {
        rich_nodes_in(message, children);
    }
    root
}

fn card_in(message: &MessageRef, mut card: serde_json::Value) -> serde_json::Value {
    if let Some(blocks) = card.get_mut("blocks") {
        card_blocks_in(message, blocks);
    }
    card
}

fn rich_nodes_in(message: &MessageRef, nodes: &mut serde_json::Value) {
    let Some(nodes) = nodes.as_array_mut() else {
        return;
    };
    for node in nodes {
        let Some(object) = node.as_object_mut() else {
            continue;
        };
        match object.get("type").and_then(serde_json::Value::as_str) {
            Some("mention") => {
                let everyone = object
                    .get("everyone")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                if !object.contains_key("actor") && !everyone {
                    let actor = actor_json(object);
                    object.insert("actor".into(), actor);
                }
                strip_identity(object);
            }
            Some("person") => {
                if !object.contains_key("actor") {
                    let actor = actor_json(object);
                    object.insert("actor".into(), actor);
                }
                strip_identity(object);
            }
            Some("resource") => resource_in(message, object),
            _ => {}
        }
        if let Some(children) = object.get_mut("children") {
            rich_nodes_in(message, children);
        }
    }
}

fn card_blocks_in(message: &MessageRef, blocks: &mut serde_json::Value) {
    let Some(blocks) = blocks.as_array_mut() else {
        return;
    };
    for block in blocks {
        let Some(object) = block.as_object_mut() else {
            continue;
        };
        match object.get("type").and_then(serde_json::Value::as_str) {
            Some("rich") => {
                if let Some(children) = object.get_mut("children") {
                    rich_nodes_in(message, children);
                }
            }
            Some("columns") => {
                if let Some(columns) = object
                    .get_mut("columns")
                    .and_then(serde_json::Value::as_array_mut)
                {
                    for column in columns {
                        card_blocks_in(message, column);
                    }
                }
            }
            Some("panel") => {
                if let Some(title) = object.get_mut("title") {
                    rich_nodes_in(message, title);
                }
                if let Some(inner) = object.get_mut("blocks") {
                    card_blocks_in(message, inner);
                }
            }
            Some("images") => {
                if let Some(images) = object
                    .get_mut("images")
                    .and_then(serde_json::Value::as_array_mut)
                {
                    for image in images
                        .iter_mut()
                        .filter_map(serde_json::Value::as_object_mut)
                    {
                        resource_in(message, image);
                    }
                }
            }
            Some("people") => {
                if let Some(people) = object
                    .get_mut("people")
                    .and_then(serde_json::Value::as_array_mut)
                {
                    for person in people.iter_mut() {
                        if let Some(entry) = person.as_object_mut()
                            && entry.contains_key("userId")
                        {
                            *person = actor_json(entry);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn actor_json(object: &serde_json::Map<String, serde_json::Value>) -> serde_json::Value {
    let text = |key: &str| {
        object
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let kind = match text("kind").as_deref() {
        Some("bot") => APP_SENDER_KIND,
        _ => "user",
    };
    let actor = ActorRef {
        id: text("userId").unwrap_or_default(),
        id_type: id_type_of(text("idType")),
        kind: kind.to_owned(),
        name: text("name"),
    };
    serde_json::to_value(actor).unwrap_or(serde_json::Value::Null)
}

fn strip_identity(object: &mut serde_json::Map<String, serde_json::Value>) {
    for key in ["userId", "idType", "kind", "name"] {
        object.remove(key);
    }
}

fn resource_in(message: &MessageRef, object: &mut serde_json::Map<String, serde_json::Value>) {
    let key = object
        .get("resource")
        .and_then(|r| r.get("key"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let kind = object
        .get("resource")
        .and_then(|r| r.get("kind"))
        .cloned()
        .and_then(|k| serde_json::from_value::<wire::ResourceKind>(k).ok());
    if let (Some(key), Some(kind)) = (key, kind) {
        let reference = ResourceRef {
            message: message.clone(),
            key,
            kind: resource_kind(kind),
        };
        if let Ok(value) = serde_json::to_value(reference) {
            object.insert("resource".into(), value);
        }
    }
    let side = |key: &str| {
        object
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
    };
    if let (Some(width), Some(height)) = (side("width"), side("height"))
        && width > 0
        && height > 0
        && let Ok(size) = serde_json::to_value(ImageSize { width, height })
    {
        object.insert("size".into(), size);
    }
    object.remove("width");
    object.remove("height");
}

fn resource_kind(k: wire::ResourceKind) -> ResourceKind {
    match k {
        wire::ResourceKind::Image => ResourceKind::Image,
        wire::ResourceKind::File => ResourceKind::File,
        wire::ResourceKind::Sticker => ResourceKind::Sticker,
    }
}

/// 翻译一条表情回复。
pub(crate) fn reaction(connection_id: &str, r: wire::Reaction) -> MessageReaction {
    MessageReaction {
        message: MessageRef {
            conversation: ConversationRef::new(connection_id, &r.conversation_id),
            id: r.message_id,
        },
        id: r.id,
        operator: actor(r.operator),
        emoji_type: r.emoji_type,
        action_time_ms: r.at_ms,
    }
}

/// 插件目录里的表情（宿主 `provider.rs` 的 `emoji`）：图片地址记成类型本身（宿主按类型向 `emoji/image` 要图）。
pub(crate) fn emoji(e: wire::Emoji) -> EmojiInfo {
    EmojiInfo {
        image_url: e.emoji_type.clone(),
        emoji_type: e.emoji_type,
        name: e.name,
        panel: e.panel,
        tone: e.tone,
        unicode: e.unicode,
        category: e.category,
        aliases: e.aliases,
        tone_of: e.tone_of.map(|t| crate::ToneOf {
            base: t.base,
            tone: t.tone,
        }),
    }
}

/// 搜到的人（宿主 `search_user`）。
pub(crate) fn search_user(p: wire::Person) -> SearchUser {
    SearchUser {
        reference: UserRef {
            connection_id: String::new(),
            id: p.id,
        },
        name: p.name.unwrap_or_default(),
        avatar_url: p.avatar,
    }
}
