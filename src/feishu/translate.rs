//! 消息查询和事件入口共同使用的格式翻译。

use super::{card, optional, required};
use crate::*;
use serde_json::Value;

/// 群通知拼出的一行文字上限；更长的按暂不支持处理。
const SYSTEM_TEXT_MAX_CHARS: usize = 500;
/// 发送文字预算小于平台上限，为转义和结构留出空间。
const SEND_TEXT_MAX_BYTES: usize = 24 * 1024;
/// 富文本块预算，避免单条消息产生无界小对象。
const CONTENT_BLOCKS_MAX: usize = 2048;
/// 提及显示名保留足够空间，同时限制标签构造时的分配。
const MENTION_NAME_MAX_BYTES: usize = 512;
/// 文字消息正文里 @所有人 的占位符。真实取样（契约 client v25）：HHH 与飞书 App 发的都留它，
/// 提及名单里没有这一项；正文里手打的同样几个字读回一模一样，分不出来。
const MENTION_ALL_KEY: &str = "@_all";
const MENTION_EVERYONE: ImContent = ImContent::Mention {
    actor: None,
    everyone: true,
};

pub(crate) fn conversation(
    connection: &str,
    raw: &Value,
    fallback: Option<&str>,
) -> ImResult<Conversation> {
    let id = optional(raw, "chat_id")
        .or_else(|| fallback.map(str::to_owned))
        .ok_or_else(|| ImError::new(ErrorKind::ProtocolError, "会话响应缺少编号。"))?;
    let kind = match raw.get("chat_mode").and_then(Value::as_str) {
        Some("p2p") => ConversationKind::Direct,
        Some("group") => ConversationKind::Group,
        None if fallback.is_none() => ConversationKind::Group,
        _ => ConversationKind::Unknown,
    };
    Ok(Conversation {
        reference: ConversationRef::new(connection, id),
        name: optional(raw, "name").unwrap_or_default(),
        kind,
        avatar_url: optional(raw, "avatar"),
        peer: None,
        private: false,
    })
}

/// 飞书里暂不支持显示的各类消息的叫法（界面写成“[投票] · 暂不支持”）；不认识的种类叫“消息”。
pub(crate) fn unsupported_label(kind: &str) -> &'static str {
    match kind {
        "interactive" => "卡片",
        "sticker" => "表情",
        "share_chat" => "群名片",
        "share_user" => "个人名片",
        "merge_forward" => "合并转发",
        "vote" => "投票",
        "todo" => "任务",
        "location" => "位置",
        "hongbao" => "红包",
        "share_calendar_event" | "calendar" | "general_calendar" => "日程",
        "video_chat" => "视频会议",
        "folder" => "文件夹",
        "audio" => "语音",
        "media" => "视频",
        "system" => "群通知",
        _ => "消息",
    }
}

/// 暂不支持的内容块：记下平台种类与飞书里的叫法。
pub(crate) fn unsupported(kind: impl Into<String>) -> ImContent {
    let kind = kind.into();
    ImContent::Unsupported {
        label: Some(unsupported_label(&kind).into()),
        kind,
    }
}

/// 本机库里升级前存下的暂不支持块没有叫法：按种类补上，与界面原来的叫法一字不差。
/// 没有要补的返回 None（不改）。
pub(crate) fn repair_unsupported_labels(content: &[ImContent]) -> Option<Vec<ImContent>> {
    let missing = content
        .iter()
        .any(|c| matches!(c, ImContent::Unsupported { label: None, .. }));
    missing.then(|| {
        content
            .iter()
            .map(|c| match c {
                ImContent::Unsupported { kind, label: None } => unsupported(kind.clone()),
                other => other.clone(),
            })
            .collect()
    })
}

/// 本机库里升级前存下、整张卡片只记成“暂不支持”的消息：要向平台重读一次。
pub(crate) fn is_unsupported_card(message: &ImMessage) -> bool {
    !message.deleted
        && message
            .content
            .iter()
            .any(|c| matches!(c, ImContent::Unsupported { kind, .. } if kind == "interactive"))
}

/// 本机库里升级前存下、标了不完整的卡片（卡片 2.0 以前只能退回简化内容，只剩标题与升级提示；
/// 契约 client v16 §6）：要向平台重读一次。1.0 卡片里有认不出的组件的也在其中，重读一次无害。
pub(crate) fn is_incomplete_card(message: &ImMessage) -> bool {
    !message.deleted
        && message.incomplete
        && message
            .content
            .iter()
            .any(|c| matches!(c, ImContent::Card(_)))
}

/// 去掉升级前按“被更新过”记下的编辑时间（大多是误标，见 [`EDITED_MARKS_REPAIR`](crate::feishu::EDITED_MARKS_REPAIR)）；
/// 没记着的不用改。
pub(crate) fn drop_old_edit_mark(message: &ImMessage) -> Option<ImMessage> {
    message.edited_at_ms.map(|_| ImMessage {
        edited_at_ms: None,
        ..message.clone()
    })
}

/// 这条消息的卡片里还有没有对齐信息的表格（卡片 2.0 的 Markdown 表格读回时丢了对齐，契约 client v19 §2）：
/// 要按飞书内部结构补读一次。本机库里升级前存下的（每页 5 行、对齐为空）、补读没成功的也按它找出来重读（§3）。
pub(crate) fn has_unaligned_card_tables(message: &ImMessage) -> bool {
    !message.deleted
        && message
            .content
            .iter()
            .any(|c| matches!(c, ImContent::Card(card) if card::has_unaligned_tables(card)))
}

/// 卡片里的提及在消息的 `mentions` 里的，带上那一项的名字（契约 client v31 §2 第 2 条）。
fn name_card_mentions(card: &mut ImCard, mentions: Option<&Vec<Value>>) {
    let Some(mentions) = mentions else {
        return;
    };
    card::for_each_person_mention(card, &mut |actor| {
        let entry = mentions
            .iter()
            .find(|m| m.get("id").and_then(Value::as_str) == Some(actor.id.as_str()));
        if let Some(name) = entry
            .and_then(|m| m.get("name"))
            .and_then(Value::as_str)
            .filter(|n| !n.trim().is_empty())
        {
            actor.name = Some(name.to_owned());
        }
    });
}

/// 这条消息卡片里对不上消息 `mentions` 的提及编号（发卡片的是别的应用，契约 client v31 §2 第 3 条），按出现先后、不重复。
/// `raw` 是平台给的这条消息。
pub(crate) fn unknown_card_mentions(message: &ImMessage, raw: &Value) -> Vec<String> {
    let known: Vec<&str> = raw
        .get("mentions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .collect();
    let mut unknown: Vec<String> = Vec::new();
    for content in &message.content {
        if let ImContent::Card(card) = content {
            for id in card::person_mention_ids(card) {
                if !known.contains(&id.as_str()) && !unknown.contains(&id) {
                    unknown.push(id);
                }
            }
        }
    }
    unknown
}

/// 按 `raw_card_content` 的 `json_attachment.at_users` 把卡片里的编号对到消息的 `mentions`：卡片里的编号 → 换成的人
/// （`mentions` 里那一项的编号与名字；没有那一项时只带 `at_users` 给的名字）。`unknown` 之外的不看。
pub(crate) fn card_at_users(
    at_users: &serde_json::Map<String, Value>,
    mentions: Option<&Vec<Value>>,
    unknown: &[String],
) -> Vec<(String, ActorRef)> {
    unknown
        .iter()
        .filter_map(|id| {
            let user = at_users.get(id)?;
            let name = user
                .get("content")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|n| !n.is_empty());
            let entry = user
                .get("mention_key")
                .and_then(Value::as_str)
                .and_then(|key| {
                    mentions?
                        .iter()
                        .find(|m| m.get("key").and_then(Value::as_str) == Some(key))
                });
            let actor = match entry.and_then(|m| Some((m, m.get("id")?.as_str()?))) {
                Some((m, reader_id)) if !reader_id.is_empty() => ActorRef {
                    id: reader_id.to_owned(),
                    id_type: m
                        .get("id_type")
                        .and_then(Value::as_str)
                        .unwrap_or("open_id")
                        .to_owned(),
                    kind: "user".into(),
                    name: m
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|n| !n.is_empty())
                        .or(name)
                        .map(str::to_owned),
                },
                _ => ActorRef {
                    id: id.clone(),
                    id_type: "open_id".into(),
                    kind: "user".into(),
                    name: Some(name?.to_owned()),
                },
            };
            Some((id.clone(), actor))
        })
        .collect()
}

/// 卡片里编号是 `pairs` 左边的提及换成右边的人，返回换了几处。
pub(crate) fn replace_card_mentions(
    message: &mut ImMessage,
    pairs: &[(String, ActorRef)],
) -> usize {
    let mut replaced = 0;
    for content in &mut message.content {
        if let ImContent::Card(card) = content {
            card::for_each_person_mention(card, &mut |actor| {
                if let Some((_, to)) = pairs.iter().find(|(from, _)| *from == actor.id) {
                    *actor = to.clone();
                    replaced += 1;
                }
            });
        }
    }
    replaced
}

/// 升级前存下的卡片 1.0 段落里还留着飞书 @ 标签原文（[`CARD_MENTION_REREAD`](crate::feishu::CARD_MENTION_REREAD)）。
pub(crate) fn has_raw_card_at_text(message: &ImMessage) -> bool {
    !message.deleted
        && message
            .content
            .iter()
            .any(|c| matches!(c, ImContent::Card(card) if card::has_raw_at_text(card)))
}

/// 这条消息里没有对齐信息的表格的表头（补读结果按它与消息编号记住，卡片被更新、表头变了就重新读）。
pub(crate) fn unaligned_table_headers(message: &ImMessage) -> Vec<Vec<String>> {
    message
        .content
        .iter()
        .filter_map(|c| match c {
            ImContent::Card(card) => Some(card::unaligned_table_headers(card)),
            _ => None,
        })
        .flatten()
        .collect()
}

/// 按飞书内部结构的表格排法补上这条消息卡片里表格的对齐与每页行数，返回补上了几张。
pub(crate) fn apply_table_layouts(message: &mut ImMessage, layouts: &[card::TableLayout]) -> usize {
    message
        .content
        .iter_mut()
        .map(|c| match c {
            ImContent::Card(card) => card::apply_table_layouts(card, layouts),
            _ => 0,
        })
        .sum()
}

/// 这条原始消息是卡片、而卡片内容认不出结构：调用方可以不带原始卡片参数再读一次，拿简化内容。
pub(crate) fn needs_card_fallback(raw: &Value) -> bool {
    raw.get("msg_type").and_then(Value::as_str) == Some("interactive")
        && !raw.get("deleted").and_then(Value::as_bool).unwrap_or(false)
        && raw
            .pointer("/body/content")
            .and_then(Value::as_str)
            .and_then(|encoded| serde_json::from_str::<Value>(encoded).ok())
            .is_some_and(|body| card::needs_fallback(&body))
}

fn timestamp(raw: &Value, key: &str) -> Option<i64> {
    raw.get(key)
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .filter(|t| *t >= 0)
}

pub(crate) fn message(connection: &str, raw: &Value) -> ImResult<ImMessage> {
    let conversation = ConversationRef::new(connection, required(raw, "chat_id")?);
    let reference = MessageRef {
        conversation: conversation.clone(),
        id: required(raw, "message_id")?,
    };
    let deleted = raw.get("deleted").and_then(Value::as_bool).unwrap_or(false);
    let kind = required(raw, "msg_type")?;
    let sender = raw
        .get("sender")
        .ok_or_else(|| ImError::new(ErrorKind::ProtocolError, "消息缺少发送者。"))?;
    let sender = if kind == "system" {
        // 系统通知可返回空身份；保留“无发送者”的事实，不伪造用户编号。
        ActorRef {
            id: optional(sender, "id").unwrap_or_default(),
            id_type: optional(sender, "id_type").unwrap_or_default(),
            kind: optional(sender, "sender_type").unwrap_or_else(|| "system".into()),
            name: optional(sender, "name"),
        }
    } else {
        ActorRef {
            id: required(sender, "id")?,
            id_type: required(sender, "id_type")?,
            kind: required(sender, "sender_type")?,
            name: optional(sender, "name"),
        }
    };
    let encoded = raw.pointer("/body/content").and_then(Value::as_str);
    let mut incomplete = false;
    let mut edit_trace = false;
    let mut content = Vec::new();
    if !deleted {
        if let Some(encoded) = encoded {
            match serde_json::from_str::<Value>(encoded) {
                Ok(body) => {
                    // 飞书客户端改过的文字消息整段包在 <p>…</p> 里（v37；client v26 §2 第 1 条，两种来源取样都对上），
                    // 这是能认定“编辑过”的唯一痕迹。
                    edit_trace = kind == "text"
                        && body
                            .get("text")
                            .and_then(Value::as_str)
                            .is_some_and(|text| edited_text(text).is_some());
                    content = match kind.as_str() {
                        "text" => {
                            text_content(&body, raw.get("mentions").and_then(Value::as_array))?
                        }
                        "image" => vec![resource(&reference, &body, ResourceKind::Image)?],
                        "file" | "audio" => {
                            vec![resource(&reference, &body, ResourceKind::File)?]
                        }
                        // 视频消息自带封面与时长（契约 client v35 §1.3）。
                        "media" => vec![media_content(&reference, &body)?],
                        "sticker" => vec![sticker_content(&reference, &body)],
                        "post" => post_content(
                            &reference,
                            &body,
                            raw.get("mentions").and_then(Value::as_array),
                        )?,
                        "system" => system_content(&body),
                        "interactive" => match card::translate(&body, &reference)? {
                            Some(mut translated) => {
                                incomplete |= translated.incomplete;
                                name_card_mentions(
                                    &mut translated.card,
                                    raw.get("mentions").and_then(Value::as_array),
                                );
                                vec![ImContent::Card(translated.card)]
                            }
                            None => vec![unsupported(kind.clone())],
                        },
                        _ => vec![unsupported(kind.clone())],
                    };
                }
                Err(_) => {
                    incomplete = true;
                    content.push(unsupported(kind.clone()));
                }
            }
        } else {
            incomplete = true;
            content.push(unsupported(kind.clone()));
        }
    }
    if content.len() > CONTENT_BLOCKS_MAX {
        return Err(ImError::new(
            ErrorKind::LimitExceeded,
            "消息内容块超过读取预算。",
        ));
    }
    incomplete |= content.iter().any(ImContent::has_unsupported);
    Ok(ImMessage {
        reference,
        sender,
        created_at_ms: timestamp(raw, "create_time")
            .ok_or_else(|| ImError::new(ErrorKind::ProtocolError, "消息缺少有效创建时间。"))?,
        updated_at_ms: timestamp(raw, "update_time"),
        content,
        reply_to: optional(raw, "parent_id").map(|id| MessageRef {
            conversation: conversation.clone(),
            id,
        }),
        root_message: optional(raw, "root_id").map(|id| MessageRef { conversation, id }),
        thread_id: optional(raw, "thread_id"),
        in_thread: in_thread(raw),
        // 飞书的“被更新过”不等于编辑：带提及、链接、图的消息发出后飞书会自己更新一次，内容不变；开话题、
        // 机器人更新卡片也会（契约 client v26 §2，2026-10-03 取样）。只有客户端编辑留下的写法才算，编辑时间取更新时间；
        // 改过的富文本没有痕迹，由本地库比较见过的内容认（平台说明的 `edits_shown_by_change`）。
        edited_at_ms: (!deleted
            && edit_trace
            && raw.get("updated").and_then(Value::as_bool) == Some(true))
        .then(|| timestamp(raw, "update_time"))
        .flatten(),
        deleted,
        incomplete,
        broadcast: false,
        reactions: None,
        thread_copy: thread_copy(raw),
        thread_activity: None,
        edit_source: None,
        raw: Default::default(),
    })
}

/// 飞书“回复话题时同时发送到群”是两条消息，用 `sync_to_chat_info` 互相指着（2026-09-30 取样，契约 client v18 §2.1）：
/// `type` 1 是群里那条（`thread_id` 指向话题），2 是话题里那条。
fn thread_copy(raw: &Value) -> Option<ThreadCopy> {
    let info = raw.get("sync_to_chat_info")?;
    let peer = info.get("related_message_id")?.as_str()?.to_owned();
    let kind = info.get("type")?;
    let kind = kind.as_i64().or_else(|| kind.as_str()?.parse().ok())?;
    let role = match kind {
        1 => ThreadCopyRole::InConversation,
        2 => ThreadCopyRole::InThread,
        _ => return None,
    };
    Some(ThreadCopy {
        role,
        peer,
        thread_id: info
            .get("thread_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

/// 平台给的“这条消息在话题里的第几条”：话题里的回复从 0 起数，会话消息流里的消息
/// （包括后来被人开了话题的那条）是 -1。平台没给或给的不是数字时返回空，交给推断。
///
/// 旧的推断只看回复关系：先被引用回复、后来才有人对它开话题的消息，会被当成别人话题里的回复，
/// 所以平台说了就以平台为准。
fn in_thread(raw: &Value) -> Option<bool> {
    let position = match raw.get("thread_message_position")? {
        Value::String(text) => text.trim().parse::<i64>().ok()?,
        Value::Number(number) => number.as_i64()?,
        _ => return None,
    };
    Some(position >= 0)
}

/// 群通知（入群、改名等）：平台给的是带占位符的模板与人名列表，拼成一行文字；
/// 没有模板或拼不出内容时仍记为暂不支持，不猜。
fn system_content(body: &Value) -> Vec<ImContent> {
    let unsupported = || vec![unsupported("system")];
    let Some(template) = body.get("template").and_then(Value::as_str) else {
        return unsupported();
    };
    let mut text = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        text.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}') else {
            text.push_str(&rest[open..]);
            rest = "";
            break;
        };
        let key = &rest[open + 1..open + close];
        let value = match body.get(key) {
            Some(Value::String(name)) => name.clone(),
            Some(Value::Array(names)) => names
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("、"),
            _ => String::new(),
        };
        text.push_str(&value);
        rest = &rest[open + close + 1..];
    }
    text.push_str(rest);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() || text.chars().count() > SYSTEM_TEXT_MAX_CHARS {
        return unsupported();
    }
    vec![ImContent::Text { text }]
}

pub(super) fn resource(
    message: &MessageRef,
    raw: &Value,
    kind: ResourceKind,
) -> ImResult<ImContent> {
    Ok(ImContent::Resource {
        resource: ResourceRef {
            message: message.clone(),
            key: required(
                raw,
                match kind {
                    ResourceKind::Image => "image_key",
                    ResourceKind::File | ResourceKind::Sticker => "file_key",
                },
            )?,
            kind,
        },
        name: optional(raw, "file_name"),
        size: None,
        video: None,
    })
}

/// 视频消息（`media`，契约 client v35 §1.3；取样见 friend/video-preview 回溯 §3）：内容是
/// `{file_key, file_name, image_key, duration}`。视频本身按文件取；封面 `image_key` 按这条消息里的图片取
/// （`type=image`，取样核实过）；`duration` 是毫秒。缺封面或时长的照样是视频，缺的项为空。
fn media_content(message: &MessageRef, raw: &Value) -> ImResult<ImContent> {
    let mut content = resource(message, raw, ResourceKind::File)?;
    if let ImContent::Resource { video, .. } = &mut content {
        *video = Some(VideoInfo {
            cover: optional(raw, "image_key").map(|key| VideoCover {
                resource: ResourceRef {
                    message: message.clone(),
                    key,
                    kind: ResourceKind::Image,
                },
                size: None,
            }),
            duration_ms: raw
                .get("duration")
                .and_then(Value::as_u64)
                .filter(|ms| *ms > 0),
            size: None,
        });
    }
    Ok(content)
}

/// 升级前存下的视频消息（契约 client v35 §1.7）：当时 `media` 只记成文件，封面与时长没留下，本地补不回来，
/// 只能向飞书重读一次。只看名字是视频、还没有封面与时长的文件；普通文件发的视频重读后仍没有，按名字记下不再读。
pub(crate) fn is_video_without_details(message: &ImMessage) -> bool {
    message.content.iter().any(|block| {
        matches!(block, ImContent::Resource { resource, name: Some(name), video, .. }
            if resource.kind == ResourceKind::File
                && is_video_name(name)
                && video.as_ref().is_none_or(|v| v.cover.is_none() && v.duration_ms.is_none()))
    })
}

/// 表情包消息（契约 client v15 §1）：内容只有 `{"file_key":"v3_…"}`，官方表情包与自己添加的分不出来、
/// 取法一样。编号认不出（不是图片 CDN 的编号）时仍记成暂不支持（叫法“表情”），不猜。
fn sticker_content(message: &MessageRef, raw: &Value) -> ImContent {
    match optional(raw, "file_key").filter(|key| super::resource::is_sticker_key(key)) {
        Some(key) => ImContent::Resource {
            resource: ResourceRef {
                message: message.clone(),
                key,
                kind: ResourceKind::Sticker,
            },
            name: None,
            size: None,
            video: None,
        },
        None => {
            tracing::warn!("表情包消息里没有认得出的图片编号，按暂不支持显示");
            unsupported("sticker")
        }
    }
}

/// 升级前存下、只记成“暂不支持”的表情包（以前没记下图片编号）：要向平台重读一次（契约 client v15 §1）。
pub(crate) fn is_unsupported_sticker(message: &ImMessage) -> bool {
    !message.deleted
        && message
            .content
            .iter()
            .any(|c| matches!(c, ImContent::Unsupported { kind, .. } if kind == "sticker"))
}

fn mention(raw: &Value) -> ImResult<ImContent> {
    let id = raw
        .get("id")
        .ok_or_else(|| ImError::new(ErrorKind::ProtocolError, "提及信息缺少身份。"))?;
    let (id, id_type) = if let Some(s) = id.as_str() {
        (
            s.to_owned(),
            optional(raw, "id_type").unwrap_or_else(|| "open_id".into()),
        )
    } else {
        (required(id, "open_id")?, "open_id".into())
    };
    if id == "all" {
        return Ok(ImContent::Mention {
            actor: None,
            everyone: true,
        });
    }
    Ok(ImContent::Mention {
        actor: Some(ActorRef {
            id,
            id_type,
            kind: optional(raw, "mentioned_type").unwrap_or_else(|| "user".into()),
            name: optional(raw, "name"),
        }),
        everyone: false,
    })
}

/// 飞书里编辑过的文字消息：`text` 整段包在 `<p>…</p>` 里（真实取样：本机库
/// `om_x100b000000000000000000000000a06`，2026-09-26；没编辑过的同类消息不带）。
/// 还原成纯文字：段落之间与 `<br>` 换成换行，常见字符实体还原成字符；其它标签原样保留，
/// 不猜它们的意思。不是整段包着的（包括没编辑过、恰好以尖括号开头的文字）返回 None，调用方照原样用。
pub(crate) fn edited_text(text: &str) -> Option<String> {
    let inner = text.strip_prefix("<p>")?.strip_suffix("</p>")?;
    let mut out = String::with_capacity(inner.len());
    let mut rest = inner;
    while let Some(at) = rest.find(['<', '&']) {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let (consumed, replacement): (usize, &str) = if let Some(tag) =
            ["</p><p>", "<br>", "<br/>", "<br />"]
                .into_iter()
                .find(|t| rest.starts_with(t))
        {
            (tag.len(), "\n")
        } else if let Some((entity, ch)) = [
            ("&lt;", "<"),
            ("&gt;", ">"),
            ("&amp;", "&"),
            ("&quot;", "\""),
            ("&#39;", "'"),
            ("&apos;", "'"),
            ("&nbsp;", " "),
        ]
        .into_iter()
        .find(|(e, _)| rest.starts_with(e))
        {
            (entity.len(), ch)
        } else if let Some(decoded) = numeric_entity(rest) {
            out.push(decoded.1);
            rest = &rest[decoded.0..];
            continue;
        } else {
            (1, &rest[..1])
        };
        out.push_str(replacement);
        rest = &rest[consumed..];
    }
    out.push_str(rest);
    Some(out)
}

/// `&#20013;` / `&#x4E2D;` 形式的数字实体：返回占用的字节数与字符。
fn numeric_entity(rest: &str) -> Option<(usize, char)> {
    let body = rest.strip_prefix("&#")?;
    let end = body.find(';').filter(|&n| n > 0 && n <= 8)?;
    let digits = &body[..end];
    let code = match digits.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => digits.parse().ok()?,
    };
    Some((2 + end + 1, char::from_u32(code)?))
}

/// 本机库里升级前存下的编辑过的文字消息：按 [`edited_text`] 同样还原。
/// 这类消息由文字与提及组成；提及用占位符留住位置，还原后原样放回。其它形状返回 None（不改）。
pub(crate) fn repair_edited_content(content: &[ImContent]) -> Option<Vec<ImContent>> {
    const SLOT: char = '\u{FFFC}';
    let mut joined = String::new();
    let mut mentions = Vec::new();
    for block in content {
        match block {
            ImContent::Text { text } if !text.contains(SLOT) => joined.push_str(text),
            ImContent::Mention { .. } => {
                joined.push(SLOT);
                mentions.push(block.clone());
            }
            _ => return None,
        }
    }
    let fixed = edited_text(&joined)?;
    let mut mentions = mentions.into_iter();
    let mut result = Vec::new();
    for (i, piece) in fixed.split(SLOT).enumerate() {
        if i > 0 {
            result.extend(mentions.next());
        }
        if !piece.is_empty() {
            result.push(ImContent::Text { text: piece.into() });
        }
    }
    if result.is_empty() {
        result.push(ImContent::Text {
            text: String::new(),
        });
    }
    Some(result)
}

/// 本机一次性修正（契约 client v25 §3.3）：升级前存成文字块的 `@_all` 拆成提及所有人，其余内容原样。
/// 存下的内容看不出来自文字消息还是没有格式的富文本，与翻译同一取舍。不需要改时返回 None。
pub(crate) fn repair_mention_all(content: &[ImContent]) -> Option<Vec<ImContent>> {
    let has =
        |c: &ImContent| matches!(c, ImContent::Text { text } if text.contains(MENTION_ALL_KEY));
    if !content.iter().any(has) {
        return None;
    }
    let mut result = Vec::with_capacity(content.len() + 2);
    for block in content {
        let ImContent::Text { text } = block else {
            result.push(block.clone());
            continue;
        };
        for (i, piece) in text.split(MENTION_ALL_KEY).enumerate() {
            if i > 0 {
                result.push(MENTION_EVERYONE);
            }
            if !piece.is_empty() {
                result.push(ImContent::Text { text: piece.into() });
            }
        }
    }
    Some(result)
}

/// 升级前带网址的文字消息已整理成格式正文，`@_all` 留在了文字节点里（契约 client v25 §3.3）。
/// 存下的树看不出它来自文字消息还是富文本（富文本里的只会是手打的），不在本地改树，出现在视野里时重读一次。
pub(crate) fn has_rich_mention_all_text(message: &ImMessage) -> bool {
    !message.deleted
        && message.content.iter().any(|c| {
            let ImContent::Rich(rich) = c else {
                return false;
            };
            let mut found = false;
            rich.walk(&mut |node| {
                found |=
                    matches!(node, RichNode::Text { value } if value.contains(MENTION_ALL_KEY));
            });
            found
        })
}

fn text_content(raw: &Value, mentions: Option<&Vec<Value>>) -> ImResult<Vec<ImContent>> {
    let text = raw
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| ImError::new(ErrorKind::ProtocolError, "文字消息缺少 text。"))?;
    // 编辑过的消息先还原成纯文字；提及的占位符（@_user_N）不受影响，下面照常拆出来。
    let edited = edited_text(text);
    let text = edited.as_deref().unwrap_or(text);
    if mentions.is_some_and(|m| m.len() > CONTENT_BLOCKS_MAX) {
        return Err(ImError::new(
            ErrorKind::LimitExceeded,
            "提及数量超过读取预算。",
        ));
    }
    let mut pairs = mentions
        .into_iter()
        .flatten()
        .map(|m| Ok((required(m, "key")?, mention(m)?)))
        .collect::<ImResult<Vec<_>>>()?;
    // @所有人只在正文留占位符、不进提及名单（契约 client v25 §3.1，真实取样）；名单里有它时照名单。
    if !pairs.iter().any(|(key, _)| key == MENTION_ALL_KEY) {
        pairs.push((MENTION_ALL_KEY.into(), MENTION_EVERYONE));
    }
    let mut remaining = text;
    let mut result = Vec::new();
    while !remaining.is_empty() {
        let found = pairs
            .iter()
            .filter_map(|(key, item)| remaining.find(key).map(|i| (i, key, item)))
            .min_by_key(|(i, _, _)| *i);
        let Some((index, key, item)) = found else {
            result.push(ImContent::Text {
                text: remaining.into(),
            });
            break;
        };
        if index > 0 {
            result.push(ImContent::Text {
                text: remaining[..index].into(),
            });
        }
        result.push(item.clone());
        remaining = &remaining[index + key.len()..];
        if result.len() > CONTENT_BLOCKS_MAX {
            return Err(ImError::new(
                ErrorKind::LimitExceeded,
                "提及数量超过消息预算。",
            ));
        }
    }
    if result.is_empty() {
        result.push(ImContent::Text {
            text: String::new(),
        });
    }
    // 文字里有网址时整条改成格式正文，网址成为链接（契约 client v6 §2）。
    Ok(super::rich::linkify_blocks(result))
}

fn post_content(
    reference: &MessageRef,
    raw: &Value,
    mentions: Option<&Vec<Value>>,
) -> ImResult<Vec<ImContent>> {
    if mentions.is_some_and(|m| m.len() > CONTENT_BLOCKS_MAX) {
        return Err(ImError::new(
            ErrorKind::LimitExceeded,
            "提及数量超过读取预算。",
        ));
    }
    let post = if raw.get("content").is_some() {
        raw
    } else {
        raw.get("zh_cn")
            .or_else(|| raw.get("en_us"))
            .or_else(|| {
                raw.as_object()
                    .and_then(|o| o.values().find(|v| v.get("content").is_some()))
            })
            .ok_or_else(|| ImError::new(ErrorKind::ProtocolError, "富文本消息缺少内容。"))?
    };
    let title = optional(post, "title");
    // 平台明确标为 Markdown 的内容只在 content_v2 里（接口用 md 发的消息，机器人与集成常用）；
    // 编辑器写的消息两者相同、不带 md（真实取样，回溯 v51 §2.5–2.6）。
    if let Some(rows) = post.get("content_v2").and_then(Value::as_array)
        && rows
            .iter()
            .filter_map(Value::as_array)
            .flatten()
            .any(|n| n.get("tag").and_then(Value::as_str) == Some("md"))
    {
        let rich = super::rich::post_rich(reference, title.as_deref(), rows, mentions)?;
        return Ok(vec![ImContent::Rich(rich)]);
    }
    let rows = post
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ImError::new(
                ErrorKind::ProtocolError,
                "富文本消息的 content 不是行列表。",
            )
        })?;
    // 带格式或链接的整理成格式正文（契约 client v6）；只有文字、@、图片的照旧，翻译结果与升级前相同。
    if super::rich::post_needs_rich(title.as_deref(), rows) {
        let rich = super::rich::post_rich(reference, title.as_deref(), rows, mentions)?;
        return Ok(vec![ImContent::Rich(rich)]);
    }
    let mut result = Vec::new();
    for row in rows {
        if !result.is_empty() {
            result.push(ImContent::Text { text: "\n".into() });
        }
        for node in row
            .as_array()
            .ok_or_else(|| ImError::new(ErrorKind::ProtocolError, "富文本行格式无效。"))?
        {
            let tag = required(node, "tag")?;
            result.push(match tag.as_str() {
                "text" => ImContent::Text {
                    // 富文本编辑器会保留空文字节点；空字符串与字段缺失不同。
                    text: node
                        .get("text")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            ImError::new(ErrorKind::ProtocolError, "富文本节点缺少文字字段。")
                        })?
                        .into(),
                },
                "at" => post_mention(node, mentions)?,
                "img" => resource(reference, node, ResourceKind::Image)?,
                _ => unsupported(tag),
            });
            if result.len() > CONTENT_BLOCKS_MAX {
                return Err(ImError::new(
                    ErrorKind::LimitExceeded,
                    "富文本消息超过内容块预算。",
                ));
            }
        }
    }
    Ok(result)
}

/// 富文本里的 `at` 节点：占位符（`@_user_N`）靠消息的 `mentions` 对回真实身份；
/// 占位符对不上时标暂不支持，不把它伪装成可用于回复的用户编号。
pub(super) fn post_mention(node: &Value, mentions: Option<&Vec<Value>>) -> ImResult<ImContent> {
    let id = required(node, "user_id")?;
    if let Some(mapped) = mentions
        .into_iter()
        .flatten()
        .find(|m| m.get("key").and_then(Value::as_str) == Some(id.as_str()))
    {
        return mention(mapped);
    }
    if id.starts_with("@_") {
        return Ok(unsupported("mention"));
    }
    Ok(ImContent::Mention {
        everyone: id == "all",
        actor: (id != "all").then_some(ActorRef {
            id,
            id_type: "open_id".into(),
            kind: "user".into(),
            name: optional(node, "user_name"),
        }),
    })
}

/// 把统一的发送内容翻译成飞书的消息种类与内容 JSON。
///
/// 图片与文件必须单独成一条消息；文字、提及与提及所有人拼成一条 text 消息。
pub(crate) fn outgoing(content: &[OutgoingContent]) -> ImResult<(&'static str, String)> {
    if content.is_empty() || content.len() > CONTENT_BLOCKS_MAX {
        return Err(ImError::new(
            ErrorKind::InvalidRequest,
            "发送内容为空或内容块过多。",
        ));
    }
    let resource = |kind: &'static str, field: &str, key: &str| {
        if content.len() != 1 {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "图片或文件须单独成一条消息。",
            ));
        }
        if key.is_empty()
            || key.len() > MENTION_NAME_MAX_BYTES
            || !key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "附件资源编号格式不正确。",
            ));
        }
        serde_json::to_string(&serde_json::json!({ field: key }))
            .map(|json| (kind, json))
            .map_err(|_| ImError::new(ErrorKind::InvalidRequest, "无法编码发送内容。"))
    };
    match content {
        [OutgoingContent::Image { key }] => return resource("image", "image_key", key),
        [OutgoingContent::File { key }] => return resource("file", "file_key", key),
        _ => {}
    }
    let escape = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('\"', "&quot;")
    };
    let mut text = String::new();
    for item in content {
        match item {
            OutgoingContent::Text { text: value } => {
                if value.len() > SEND_TEXT_MAX_BYTES {
                    return Err(ImError::new(
                        ErrorKind::LimitExceeded,
                        "发送文字超过长度预算。",
                    ));
                }
                // 官方 text 会解释 <at>，但没有承诺 HTML 实体还原；不擅自改写普通文字。
                if value.to_ascii_lowercase().contains("<at") {
                    return Err(ImError::new(
                        ErrorKind::InvalidRequest,
                        "文字中包含飞书提及标签，请改用结构化 Mention 内容。",
                    ));
                }
                text.push_str(value);
            }
            OutgoingContent::Mention { user_id, name } => {
                if user_id.is_empty()
                    || !user_id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_')
                    || name.len() > MENTION_NAME_MAX_BYTES
                {
                    return Err(ImError::new(
                        ErrorKind::InvalidRequest,
                        "提及对象编号或名称格式不正确。",
                    ));
                }
                text.push_str(&format!(
                    "<at user_id=\"{}\">{}</at>",
                    user_id,
                    escape(name)
                ));
            }
            OutgoingContent::MentionAll => text.push_str("<at user_id=\"all\"></at>"),
            OutgoingContent::Image { .. } | OutgoingContent::File { .. } => {
                return Err(ImError::new(
                    ErrorKind::InvalidRequest,
                    "图片或文件须单独成一条消息。",
                ));
            }
        }
        if text.len() > SEND_TEXT_MAX_BYTES {
            return Err(ImError::new(
                ErrorKind::LimitExceeded,
                "发送内容转义后超过长度预算。",
            ));
        }
    }
    if text.trim().is_empty() {
        return Err(ImError::new(
            ErrorKind::InvalidRequest,
            "不能发送空白消息。",
        ));
    }
    serde_json::to_string(&serde_json::json!({"text":text}))
        .map(|json| ("text", json))
        .map_err(|_| ImError::new(ErrorKind::InvalidRequest, "无法编码发送内容。"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        serde_json::from_str(include_str!(
            "../../tests/fixtures/feishu/response_text.json"
        ))
        .unwrap()
    }

    #[test]
    fn official_response_fixtures_match_independent_expected_values() {
        let directory =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/feishu");
        let mut count = 0;
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "json")
                || path.to_string_lossy().ends_with(".expected.json")
            {
                continue;
            }
            let raw: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            let expected: Value = serde_json::from_slice(
                &std::fs::read(path.with_extension("expected.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(
                serde_json::to_value(message("test", &raw).unwrap()).unwrap(),
                expected,
                "{}",
                path.display()
            );
            count += 1;
        }
        assert!(count >= 3);
    }

    #[test]
    fn the_platforms_position_in_a_thread_says_whether_a_message_is_a_thread_reply() {
        // 真实取样（回溯 v12 S1/S4/S5）：话题里的回复从 "0" 起数；会话消息流里的消息是 "-1"，
        // 包括引用回复之后又被人开了话题的那条（它带着自己的话题编号和被引用那条的根消息编号）。
        let mut raw = sample();
        raw["root_id"] = "om_quoted".into();
        raw["parent_id"] = "om_quoted".into();
        raw["thread_id"] = "omt_own".into();
        for (position, expected) in [
            (json!("-1"), Some(false)),
            (json!("0"), Some(true)),
            (json!("12"), Some(true)),
            (json!(3), Some(true)),
            (json!("不是数字"), None),
        ] {
            raw["thread_message_position"] = position.clone();
            assert_eq!(
                message("test", &raw).unwrap().in_thread,
                expected,
                "{position}"
            );
        }
        // 平台没给：留空，交给旧判断。
        raw.as_object_mut()
            .unwrap()
            .remove("thread_message_position");
        let result = message("test", &raw).unwrap();
        assert_eq!(result.in_thread, None);
        assert_eq!(crate::thread_root_of(&result), Some("om_quoted"));
        raw["thread_message_position"] = "-1".into();
        assert_eq!(crate::thread_root_of(&message("test", &raw).unwrap()), None);
    }

    #[test]
    fn edited_text_messages_show_plain_text_not_paragraph_markup() {
        // 真实取样（2026-09-26，本机库 om_x100b000000000000000000000000a06）：飞书里编辑过的文字消息，
        // text 整段包在 <p>…</p> 里；没编辑过的同类消息不带。
        let mut raw = sample();
        raw["body"]["content"] = json!({"text": "<p>下次注意！ 努力保持素质[看]</p>"})
            .to_string()
            .into();
        let result = message("test", &raw).unwrap();
        assert!(
            matches!(&result.content[..], [ImContent::Text { text }] if text == "下次注意！ 努力保持素质[看]"),
            "{:?}",
            result.content
        );
    }

    #[test]
    fn edited_text_turns_paragraphs_breaks_and_entities_back_into_text() {
        assert_eq!(
            edited_text("<p>一</p><p>二</p><p></p><p>三</p>").as_deref(),
            Some("一\n二\n\n三")
        );
        assert_eq!(
            edited_text("<p>a<br>b<br/>c<br />d</p>").as_deref(),
            Some("a\nb\nc\nd")
        );
        // 实体只解一次：&amp;lt; 是字面的 &lt;，不是 <。
        assert_eq!(
            edited_text(
                "<p>a&lt;b&gt;c &amp; &quot;x&quot; &#39;y&#39; &#20013;&#x6587;&nbsp;&amp;lt;</p>"
            )
            .as_deref(),
            Some("a<b>c & \"x\" 'y' 中文 &lt;")
        );
        // 不认识的标签与不完整的实体原样保留，不猜。
        assert_eq!(
            edited_text("<p><b>粗</b> &unknown; &#;</p>").as_deref(),
            Some("<b>粗</b> &unknown; &#;")
        );
        // 不是整段包着的：没编辑过、恰好以尖括号开头的文字，原样不动。
        for text in ["<p>只有开头", "结尾</p>", "普通文字", "a <p>b</p>", ""] {
            assert_eq!(edited_text(text), None, "{text}");
        }
    }

    #[test]
    fn edited_text_keeps_mention_placeholders_so_mentions_still_resolve() {
        let mut raw = sample();
        raw["body"]["content"] = json!({"text": "<p>@_user_1 你好&amp;再见</p>"})
            .to_string()
            .into();
        let result = message("test", &raw).unwrap();
        assert!(
            matches!(&result.content[..], [ImContent::Mention { actor: Some(a), .. }, ImContent::Text { text }] if a.id == "ou_me" && text == " 你好&再见"),
            "{:?}",
            result.content
        );
    }

    #[test]
    fn stored_edited_messages_are_repaired_with_mentions_in_place() {
        let mention = ImContent::Mention {
            actor: Some(ActorRef {
                id: "ou_me".into(),
                id_type: "open_id".into(),
                kind: "user".into(),
                name: Some("Alice".into()),
            }),
            everyone: false,
        };
        let text = |t: &str| ImContent::Text { text: t.into() };
        // 升级前的翻译：<p> 与 </p> 落在提及两边的文字块里。
        assert_eq!(
            repair_edited_content(&[
                text("<p>"),
                mention.clone(),
                text(" 下次注意！</p><p>第二行</p>")
            ]),
            Some(vec![mention.clone(), text(" 下次注意！\n第二行")])
        );
        assert_eq!(
            repair_edited_content(&[text("<p>下次注意！ 努力保持素质[看]</p>")]),
            Some(vec![text("下次注意！ 努力保持素质[看]")])
        );
        assert_eq!(
            repair_edited_content(&[text("<p></p>")]),
            Some(vec![text("")])
        );
        // 没包着的、带图片或文件的，不改。
        assert_eq!(repair_edited_content(&[text("a <p>b</p>")]), None);
        assert_eq!(
            repair_edited_content(&[text("<p>"), unsupported("x"), text("</p>")]),
            None
        );
    }

    #[test]
    fn mention_all_placeholders_mention_everyone_without_a_list_entry() {
        let everyone = ImContent::Mention {
            actor: None,
            everyone: true,
        };
        let text = |t: &str| ImContent::Text { text: t.into() };
        let me = ActorRef {
            id: "ou_me".into(),
            id_type: "open_id".into(),
            kind: "user".into(),
            name: None,
        };
        // 真实取样（2026-10-03）：只有 @所有人 时连 mentions 这个键都没有；夹在文字中间时前后文字原样。
        let mut raw = sample();
        raw.as_object_mut().unwrap().remove("mentions");
        raw["body"]["content"] = json!({"text": "前文 @_all 后文"}).to_string().into();
        let shown = message("test", &raw).unwrap();
        assert_eq!(
            shown.content,
            vec![text("前文 "), everyone.clone(), text(" 后文")]
        );
        assert!(crate::store::mentions_everyone(&shown));
        assert!(!crate::store::mentions(&shown, &me), "@所有人不算 @我");
        // 带网址：先拆出提及，再整理成格式正文，提及是树里的节点，文字里不留占位符。
        raw["body"]["content"] = json!({"text": "@_all 请填 https://open.feishu.cn"})
            .to_string()
            .into();
        let linked = message("test", &raw).unwrap();
        let ImContent::Rich(rich) = &linked.content[0] else {
            panic!("带网址的文字消息应整理成格式正文：{:?}", linked.content);
        };
        let mut nodes = Vec::new();
        rich.walk(&mut |node| nodes.push(node.clone()));
        assert!(nodes.iter().any(|n| matches!(
            n,
            RichNode::Mention {
                actor: None,
                everyone: true
            }
        )));
        assert!(nodes.iter().any(|n| matches!(n, RichNode::Link { .. })));
        assert!(
            !nodes
                .iter()
                .any(|n| matches!(n, RichNode::Text { value } if value.contains("@_all")))
        );
        assert!(!has_rich_mention_all_text(&linked), "新翻译的不需要重读");
        // 名单里有这一项时照名单（已有规则）。
        raw["body"]["content"] = json!({"text": "@_all"}).to_string().into();
        raw["mentions"] =
            json!([{"key": "@_all", "id": "ou_me", "id_type": "open_id", "name": "Alice"}]);
        assert!(matches!(
            &message("test", &raw).unwrap().content[..],
            [ImContent::Mention { actor: Some(actor), everyone: false }] if actor.id == "ou_me"
        ));
    }

    #[test]
    fn stored_mention_all_text_is_repaired_and_formatted_ones_are_reread() {
        let everyone = ImContent::Mention {
            actor: None,
            everyone: true,
        };
        let alice = ImContent::Mention {
            actor: Some(ActorRef {
                id: "ou_me".into(),
                id_type: "open_id".into(),
                kind: "user".into(),
                name: Some("Alice".into()),
            }),
            everyone: false,
        };
        let text = |t: &str| ImContent::Text { text: t.into() };
        // 升级前的翻译：@所有人 留成文字块；其余块原样、位置不变。
        assert_eq!(
            repair_mention_all(&[text("@_all")]),
            Some(vec![everyone.clone()])
        );
        assert_eq!(
            repair_mention_all(&[text("前文 @_all 后文"), alice.clone(), text(" 和 @_all")]),
            Some(vec![
                text("前文 "),
                everyone.clone(),
                text(" 后文"),
                alice.clone(),
                text(" 和 "),
                everyone.clone()
            ])
        );
        // 没有占位符的、已经是提及的，不改。
        assert_eq!(repair_mention_all(&[text("@Alice 你好"), everyone]), None);
        assert_eq!(MENTION_ALL_KEY, crate::feishu::MENTION_ALL_REPAIR.contains);
        // 升级前带网址的：已整理成格式正文，本地不改树，出现在视野里时重读。
        let mut raw = sample();
        raw.as_object_mut().unwrap().remove("mentions");
        let mut old = message("test", &raw).unwrap();
        old.content =
            super::super::rich::linkify_blocks(vec![text("@_all 请填 https://open.feishu.cn")]);
        assert!(matches!(old.content[..], [ImContent::Rich(_)]));
        assert_eq!(repair_mention_all(&old.content), None);
        assert!(has_rich_mention_all_text(&old));
        assert_eq!(
            MENTION_ALL_KEY,
            crate::feishu::MENTION_ALL_RICH_REREAD.contains
        );
        // 文字块交给本地修正、不重读；撤回了的不重读。
        let mut plain = old.clone();
        plain.content = vec![text("@_all")];
        assert!(!has_rich_mention_all_text(&plain));
        old.deleted = true;
        assert!(!has_rich_mention_all_text(&old));
    }

    #[test]
    fn unsupported_content_carries_the_feishu_label_and_old_records_get_it_once() {
        // 叫法与界面原来的叫法表一字不差；不认识的叫“消息”。
        for (kind, label) in [
            ("interactive", "卡片"),
            ("vote", "投票"),
            ("hongbao", "红包"),
            ("share_calendar_event", "日程"),
            ("general_calendar", "日程"),
            ("system", "群通知"),
            ("future", "消息"),
        ] {
            assert_eq!(unsupported_label(kind), label);
            assert_eq!(
                unsupported(kind),
                ImContent::Unsupported {
                    kind: kind.into(),
                    label: Some(label.into())
                }
            );
        }
        let text = ImContent::Text { text: "a".into() };
        let old = ImContent::Unsupported {
            kind: "vote".into(),
            label: None,
        };
        assert_eq!(
            repair_unsupported_labels(&[text.clone(), old]),
            Some(vec![text.clone(), unsupported("vote")])
        );
        assert_eq!(
            repair_unsupported_labels(&[text, unsupported("vote")]),
            None
        );
    }

    // 飞书客户端“同时发送到群”的一对：群里那条 type 1 带话题编号，话题里那条 type 2（2026-09-30 取样的结构）。
    #[test]
    fn sync_to_chat_info_pairs_the_thread_reply_with_its_copy() {
        let mut in_chat = sample();
        in_chat["sync_to_chat_info"] =
            json!({"related_message_id": "om_reply", "thread_id": "omt_1", "type": 1});
        assert_eq!(
            message("test", &in_chat).unwrap().thread_copy,
            Some(ThreadCopy {
                role: ThreadCopyRole::InConversation,
                peer: "om_reply".into(),
                thread_id: Some("omt_1".into()),
            })
        );
        let mut in_thread = sample();
        in_thread["sync_to_chat_info"] = json!({"related_message_id": "om_copy", "type": "2"});
        assert_eq!(
            message("test", &in_thread).unwrap().thread_copy,
            Some(ThreadCopy {
                role: ThreadCopyRole::InThread,
                peer: "om_copy".into(),
                thread_id: None,
            })
        );
        for odd in [
            json!({"related_message_id": "om_x", "type": 3}),
            json!({"type": 1}),
            json!(null),
        ] {
            let mut raw = sample();
            raw["sync_to_chat_info"] = odd;
            assert_eq!(
                message("test", &raw).unwrap().thread_copy,
                None,
                "认不出的不配对"
            );
        }
        assert_eq!(message("test", &sample()).unwrap().thread_copy, None);
    }

    // 真实取样（契约 client v26 §2）：飞书的“被更新过”不等于编辑。带提及、链接、图的消息发出后飞书会自己更新一次，
    // 内容一字不变，不标；网页版改过的文字消息整段包在 <p>…</p> 里，标；改过的富文本没有痕迹，翻译时不标，
    // 交给本地库比较见过的内容（这几种都在比较范围里，改过的内容才变）。
    #[test]
    fn only_the_client_edit_trace_marks_feishu_text_as_edited() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/feishu/edits/samples-v26.json"
        ))
        .unwrap();
        let pairs = fixture["pairs"].as_array().unwrap();
        assert_eq!(pairs.len(), 5, "取样夹具没读全");
        for pair in pairs {
            let name = pair["name"].as_str().unwrap();
            let before = message("test", &pair["before"]).unwrap();
            let after = message("test", &pair["after"]).unwrap();
            assert_eq!(before.edited_at_ms, None, "{name}：发出时没改过");
            let expected = (name == "text_edited_in_web")
                .then(|| timestamp(&pair["after"], "update_time"))
                .flatten();
            assert_eq!(after.edited_at_ms, expected, "{name}");
            assert!(
                crate::feishu::facts::edits_shown_by_change(&after),
                "{name}：能编辑的内容交给本地库比较"
            );
            assert_eq!(
                after.content != before.content,
                pair["edited"].as_bool().unwrap(),
                "{name}：改过的内容才变"
            );
        }
    }

    // 机器人更新卡片、开话题都会让飞书记成更新过（v18 §2.4、im-watch v65 验收），都不是编辑；卡片不交给本地库比较。
    // 话题第一条真被改过（包着 <p>）照样标，撤回的不标（契约 client v26 §2）。
    #[test]
    fn updated_cards_and_thread_roots_are_not_edited_but_an_edited_root_is() {
        let mut text = sample();
        text["updated"] = true.into();
        text["update_time"] = "1759000060000".into();
        assert_eq!(
            message("test", &text).unwrap().edited_at_ms,
            None,
            "只是更新过"
        );
        let mut card = text.clone();
        card["msg_type"] = "interactive".into();
        card["body"]["content"] = json!({"elements": [{"tag": "hr"}]}).to_string().into();
        let card = message("test", &card).unwrap();
        assert_eq!(card.edited_at_ms, None);
        assert!(!crate::feishu::facts::edits_shown_by_change(&card));
        let mut root = text.clone();
        let fields = root.as_object_mut().unwrap();
        fields.remove("root_id");
        fields.remove("parent_id");
        assert_eq!(
            message("test", &root).unwrap().edited_at_ms,
            None,
            "开过话题"
        );
        root["body"]["content"] = json!({"text": "<p>改过的第一条</p>"}).to_string().into();
        assert_eq!(
            message("test", &root).unwrap().edited_at_ms,
            Some(1_759_000_060_000),
            "话题第一条真改过照样标"
        );
        root["deleted"] = true.into();
        let recalled = message("test", &root).unwrap();
        assert_eq!(recalled.edited_at_ms, None, "撤回的不标");
        assert!(!crate::feishu::facts::edits_shown_by_change(&recalled));
    }

    // 升级前按“被更新过”记下的编辑时间大多是误标：本地库打开时一次去掉，不联网（契约 client v26 §3）；
    // 内容不动，没记着的不改。
    //
    // HHH 里这条测试经本地消息库（ImStore）跑一遍修正，还核对“做过一次不再做”；插件没有本地库，那部分留在 HHH
    // （`stored_edit_marks_are_dropped_once_when_the_store_opens`）。这里只核对插件给出的修正本身。
    #[test]
    fn stored_edit_marks_are_dropped_by_the_repair() {
        let chat = ConversationRef::new("test", "oc_group");
        let at = |id: &str| MessageRef {
            conversation: chat.clone(),
            id: id.into(),
        };
        let fresh = message("test", &sample()).unwrap();
        let marked = |id: &str| ImMessage {
            reference: at(id),
            edited_at_ms: Some(1_700_000_002_000),
            ..fresh.clone()
        };
        let plain = ImMessage {
            reference: at("om_plain"),
            ..fresh.clone()
        };
        let repair = crate::feishu::EDITED_MARKS_REPAIR;
        assert!(
            serde_json::to_string(&marked("om_text"))
                .unwrap()
                .contains(repair.contains)
        );
        for id in ["om_text", "om_root"] {
            let stored = (repair.rewrite)(&marked(id)).unwrap();
            assert_eq!(stored.edited_at_ms, None, "{id}");
            assert_eq!(stored.content, fresh.content, "{id}：内容不动");
        }
        assert_eq!((repair.rewrite)(&plain), None, "没记着的不改");
    }

    #[test]
    fn cards_translate_from_raw_json_and_unknown_structures_ask_for_a_fallback() {
        let mut raw = sample();
        raw["msg_type"] = "interactive".into();
        raw["body"]["content"] =
            json!({"header": {"template": "blue", "title": {"content": "t", "tag": "plain_text"}},
            "elements": [{"tag": "hr"}, {"tag": "note", "elements": []}]})
            .to_string()
            .into();
        let result = message("test", &raw).unwrap();
        assert!(result.incomplete, "认不出的组件让消息标不完整");
        assert!(
            matches!(&result.content[..], [ImContent::Card(card)] if card.tone == CardTone::Blue && card.blocks == [CardBlock::Divider])
        );
        assert!(!needs_card_fallback(&raw));
        // 卡片 2.0 按原始结构读，不补读简化内容（契约 client v16 §1）；什么都没有的仍是暂不支持。
        raw["body"]["content"] = json!({"schema": "2.0", "body": {}}).to_string().into();
        assert!(!needs_card_fallback(&raw));
        assert_eq!(
            message("test", &raw).unwrap().content,
            [unsupported("interactive")]
        );
        raw["body"]["content"] = json!({"schema": "3.0", "body": {}}).to_string().into();
        assert!(needs_card_fallback(&raw));
        let result = message("test", &raw).unwrap();
        assert_eq!(result.content, [unsupported("interactive")]);
        raw["deleted"] = true.into();
        assert!(!needs_card_fallback(&raw), "撤回的不补读");
        let mut text = sample();
        text["body"]["content"] = json!({"schema": "2.0"}).to_string().into();
        assert!(!needs_card_fallback(&text), "只看卡片消息");
    }

    #[test]
    fn media_messages_become_videos_with_their_cover_and_duration_and_old_ones_are_reread() {
        // 契约 client v35 §1.3：视频消息带封面（按这条消息的图片取）与毫秒时长；缺的项为空，照样是视频。
        let mut raw = sample();
        raw["msg_type"] = "media".into();
        raw["body"]["content"] = json!({"file_key": "file_v", "file_name": "转动.mp4"})
            .to_string()
            .into();
        let bare = message("test", &raw).unwrap();
        assert!(
            matches!(&bare.content[..], [ImContent::Resource { resource, video: Some(info), .. }]
            if resource.kind == ResourceKind::File && *info == VideoInfo::default())
        );
        raw["body"]["content"] =
            json!({"file_key": "file_v", "file_name": "转动.mp4", "image_key": "img_c", "duration": 3400})
                .to_string()
                .into();
        let full = message("test", &raw).unwrap();
        let ImContent::Resource {
            video: Some(info), ..
        } = &full.content[0]
        else {
            panic!("不是视频：{:?}", full.content)
        };
        let cover = info.cover.as_ref().unwrap();
        assert_eq!(
            (cover.resource.key.as_str(), cover.resource.kind),
            ("img_c", ResourceKind::Image)
        );
        assert_eq!(cover.resource.message, full.reference);
        assert_eq!(info.duration_ms, Some(3400));
        // 音频仍是普通文件。
        raw["msg_type"] = "audio".into();
        assert!(!message("test", &raw).unwrap().content[0].is_video());
        // 升级前存下的视频（当时只记成文件）重读一次；有封面或时长的、不是视频的不读。
        let mut stored = full.clone();
        if let ImContent::Resource { video, .. } = &mut stored.content[0] {
            *video = None;
        }
        assert!(is_video_without_details(&stored));
        assert!(!is_video_without_details(&full));
        raw["msg_type"] = "file".into();
        raw["body"]["content"] = json!({"file_key": "file_p", "file_name": "报价.pdf"})
            .to_string()
            .into();
        assert!(!is_video_without_details(&message("test", &raw).unwrap()));
        assert!(
            serde_json::to_string(&stored)
                .unwrap()
                .contains(crate::feishu::VIDEO_REREAD.contains)
        );
    }

    #[test]
    fn stored_cards_that_were_unsupported_are_marked_for_rereading() {
        let mut stored = message("test", &sample()).unwrap();
        stored.content = vec![unsupported("interactive")];
        assert!(is_unsupported_card(&stored));
        stored.content = vec![unsupported("vote")];
        assert!(!is_unsupported_card(&stored));
        stored.content = vec![unsupported("interactive")];
        stored.deleted = true;
        assert!(!is_unsupported_card(&stored));
    }

    #[test]
    fn stickers_become_sticker_resources_and_unrecognised_keys_stay_unsupported() {
        // 真实取样（回溯 v59 §2-1）：内容只有 file_key，是图片 CDN 的编号。
        let mut raw = sample();
        raw["msg_type"] = "sticker".into();
        raw["body"]["content"] =
            json!({"file_key": "v3_0010a_22222222-2222-4222-8222-22222222222g"})
                .to_string()
                .into();
        let result = message("test", &raw).unwrap();
        assert!(!result.incomplete);
        assert!(matches!(
            &result.content[..],
            [ImContent::Resource { resource, name: None, size: None, video: None }]
                if resource.kind == ResourceKind::Sticker && resource.key == "v3_0010a_22222222-2222-4222-8222-22222222222g"
        ));
        assert!(!is_unsupported_sticker(&result));
        // 没有编号、编号不是图片 CDN 的样子（路径符号、别的前缀）：仍是暂不支持，叫法“表情”，消息标不完整。
        for body in [
            json!({}),
            json!({"file_key": "file_v3_abc"}),
            json!({"file_key": "v3_../../etc"}),
            json!({"file_key": "v3_"}),
            json!({"file_key": format!("v3_{}", "a".repeat(200))}),
        ] {
            raw["body"]["content"] = body.to_string().into();
            let result = message("test", &raw).unwrap();
            assert!(result.incomplete, "{body}");
            assert_eq!(result.content, [unsupported("sticker")], "{body}");
            assert_eq!(unsupported_label("sticker"), "表情");
        }
    }

    #[test]
    fn stored_stickers_that_were_unsupported_are_marked_for_rereading() {
        let mut stored = message("test", &sample()).unwrap();
        stored.content = vec![unsupported("sticker")];
        assert!(is_unsupported_sticker(&stored));
        assert!((crate::feishu::STICKER_REREAD.applies)(&stored));
        stored.content = vec![unsupported("interactive")];
        assert!(!is_unsupported_sticker(&stored));
        stored.content = vec![unsupported("sticker")];
        stored.deleted = true;
        assert!(!is_unsupported_sticker(&stored), "撤回的不重读");
        // 规则只看含这段文字的记录（本地库先按文字筛）。
        assert!(
            serde_json::to_string(&unsupported("sticker"))
                .unwrap()
                .contains(crate::feishu::STICKER_REREAD.contains)
        );
    }

    #[test]
    fn stored_incomplete_cards_are_reread_once_under_the_v2_rule() {
        // 升级前 2.0 卡片退回简化内容后存成的样子：标题（副标题并在里面）+ 一段升级提示，标了不完整。
        let mut stored = message("test", &sample()).unwrap();
        stored.content = vec![ImContent::Card(ImCard {
            title: Some("日报\n日期：2026-09-29".into()),
            blocks: vec![CardBlock::Paragraph {
                parts: vec![CardInline::Markdown {
                    text: "请升级至最新版本客户端，以查看内容".into(),
                }],
            }],
            ..ImCard::default()
        })];
        stored.incomplete = true;
        assert!(is_incomplete_card(&stored));
        let rule = crate::feishu::CARD_V2_REREAD;
        assert!(
            serde_json::to_string(&stored)
                .unwrap()
                .contains(rule.contains)
        );
        // 完整的卡片、别的内容、撤回的都不重读。
        stored.incomplete = false;
        assert!(!is_incomplete_card(&stored));
        stored.incomplete = true;
        stored.deleted = true;
        assert!(!is_incomplete_card(&stored));
        let mut text = message("test", &sample()).unwrap();
        text.incomplete = true;
        assert!(!is_incomplete_card(&text));
    }

    #[test]
    fn malformed_and_unknown_content_stays_visibly_incomplete() {
        for (kind, body) in [
            ("text", "not-json"),
            ("interactive", "{}"),
            ("future", "{}"),
        ] {
            let mut raw = sample();
            raw["msg_type"] = kind.into();
            raw["body"]["content"] = body.into();
            let result = message("test", &raw).unwrap();
            assert!(result.incomplete);
            assert!(matches!(result.content[0], ImContent::Unsupported { .. }));
        }
    }

    #[test]
    fn rich_text_mentions_resolve_platform_placeholders() {
        let mut raw = sample();
        raw["msg_type"] = "post".into();
        raw["body"]["content"] = json!({
            "content": [[{"tag": "at", "user_id": "@_user_1", "user_name": "Alice"}]]
        })
        .to_string()
        .into();
        let result = message("test", &raw).unwrap();
        assert!(
            matches!(&result.content[0], ImContent::Mention { actor: Some(actor), everyone: false } if actor.id == "ou_me")
        );
        raw["mentions"][0]["id"] = "all".into();
        assert!(matches!(
            message("test", &raw).unwrap().content[0],
            ImContent::Mention {
                actor: None,
                everyone: true
            }
        ));
        raw["mentions"] = json!([]);
        let missing = message("test", &raw).unwrap();
        assert!(missing.incomplete);
        assert!(matches!(missing.content[0], ImContent::Unsupported { .. }));
    }

    #[test]
    fn empty_rich_text_nodes_are_valid_but_missing_text_is_rejected() {
        let mut raw = sample();
        raw["msg_type"] = "post".into();
        raw["body"]["content"] = json!({
            "title": "",
            "content": [[{"tag": "text", "text": ""}, {"tag": "text", "text": "after"}]]
        })
        .to_string()
        .into();
        assert_eq!(
            message("test", &raw).unwrap().content,
            vec![
                ImContent::Text {
                    text: String::new()
                },
                ImContent::Text {
                    text: "after".into()
                }
            ]
        );
        raw["body"]["content"] = json!({"content": [[{"tag": "text"}]]}).to_string().into();
        assert_eq!(
            message("test", &raw).unwrap_err().kind,
            ErrorKind::ProtocolError
        );
    }

    #[test]
    fn system_notifications_fill_their_template_with_names() {
        let mut raw: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/feishu/response_system.json"
        ))
        .unwrap();
        raw["body"]["content"] = json!({
            "template": "{from_user} 邀请 {to_chatters} 加入群聊",
            "from_user": ["王磊"],
            "to_chatters": ["陈言", "林然"]
        })
        .to_string()
        .into();
        let result = message("test", &raw).unwrap();
        assert_eq!(
            result.content,
            vec![ImContent::Text {
                text: "王磊 邀请 陈言、林然 加入群聊".into()
            }]
        );
        assert!(!result.incomplete);
        raw["body"]["content"] = json!({"template": "{unknown}"}).to_string().into();
        assert!(
            message("test", &raw).unwrap().incomplete,
            "拼不出内容时不猜"
        );
    }

    #[test]
    fn system_notifications_without_actor_do_not_break_history() {
        let raw: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/feishu/response_system.json"
        ))
        .unwrap();
        let result = message("test", &raw).unwrap();
        assert_eq!(result.sender.kind, "system");
        assert!(result.sender.id.is_empty());
        assert!(result.incomplete);
        assert_eq!(result.reference.id, "om_system");
        let mut broken_user = sample();
        broken_user["sender"]["id"] = "".into();
        assert_eq!(
            message("test", &broken_user).unwrap_err().kind,
            ErrorKind::ProtocolError
        );
    }

    #[test]
    fn deleted_messages_keep_identity_and_relationships() {
        let mut raw = sample();
        raw["deleted"] = true.into();
        raw["body"] = Value::Null;
        let result = message("test", &raw).unwrap();
        assert!(result.deleted);
        assert!(result.content.is_empty());
        assert_eq!(result.root_message.unwrap().id, "om_root");
    }

    #[test]
    fn text_mentions_are_structural_and_empty_keys_are_rejected() {
        let mut raw = sample();
        raw["mentions"][0]["key"] = "".into();
        assert_eq!(
            message("test", &raw).unwrap_err().kind,
            ErrorKind::ProtocolError
        );
        raw["mentions"] = json!([]);
        assert!(
            message("test", &raw)
                .unwrap()
                .content
                .iter()
                .all(|c| matches!(c, ImContent::Text { .. }))
        );
    }

    #[test]
    fn outgoing_keeps_text_exact_and_only_explicit_mentions_become_tags() {
        let content = vec![
            OutgoingContent::Text {
                text: "A & <B> \"C\"\n@Alice ".into(),
            },
            OutgoingContent::Mention {
                user_id: "ou_me".into(),
                name: "Alice".into(),
            },
        ];
        let (kind, json_text) = outgoing(&content).unwrap();
        assert_eq!(kind, "text");
        let actual: Value = serde_json::from_str(&json_text).unwrap();
        assert_eq!(
            actual,
            json!({"text":"A & <B> \"C\"\n@Alice <at user_id=\"ou_me\">Alice</at>"})
        );
        assert!(
            outgoing(&[OutgoingContent::Text {
                text: "<at user_id=\"all\"></at>".into()
            }])
            .is_err()
        );
        assert!(
            outgoing(&[OutgoingContent::Mention {
                user_id: "bad\" id".into(),
                name: "x".into()
            }])
            .is_err()
        );
    }

    #[test]
    fn mention_all_joins_text_and_attachments_are_their_own_message() {
        // 提及所有人与文字拼成一条 text；图片与文件各自成一条对应种类的消息。
        let (kind, json_text) = outgoing(&[
            OutgoingContent::MentionAll,
            OutgoingContent::Text {
                text: " 今晚八点发版。".into(),
            },
        ])
        .unwrap();
        assert_eq!(kind, "text");
        assert_eq!(
            serde_json::from_str::<Value>(&json_text).unwrap(),
            json!({"text":"<at user_id=\"all\"></at> 今晚八点发版。"})
        );
        let (kind, json_text) = outgoing(&[OutgoingContent::Image {
            key: "img_v3_example".into(),
        }])
        .unwrap();
        assert_eq!(kind, "image");
        assert_eq!(
            serde_json::from_str::<Value>(&json_text).unwrap(),
            json!({"image_key":"img_v3_example"})
        );
        let (kind, json_text) = outgoing(&[OutgoingContent::File {
            key: "file_v3_example".into(),
        }])
        .unwrap();
        assert_eq!(kind, "file");
        assert_eq!(
            serde_json::from_str::<Value>(&json_text).unwrap(),
            json!({"file_key":"file_v3_example"})
        );
        // 附件不能与文字混在一条里，资源编号也不能带引号等符号。
        for bad in [
            vec![
                OutgoingContent::Image { key: "img".into() },
                OutgoingContent::Text {
                    text: "说明".into(),
                },
            ],
            vec![OutgoingContent::File {
                key: "bad\" key".into(),
            }],
        ] {
            assert_eq!(outgoing(&bad).unwrap_err().kind, ErrorKind::InvalidRequest);
        }
    }

    #[test]
    fn message_and_send_budgets_are_enforced() {
        assert!(outgoing(&[]).is_err());
        assert!(outgoing(&[OutgoingContent::Text { text: " ".into() }]).is_err());
        assert_eq!(
            outgoing(&[OutgoingContent::Text {
                text: "x".repeat(SEND_TEXT_MAX_BYTES + 1)
            }])
            .unwrap_err()
            .kind,
            ErrorKind::LimitExceeded
        );
        let mut raw = sample();
        raw["body"]["content"] = json!({"text":"@_user_1 ".repeat(CONTENT_BLOCKS_MAX)})
            .to_string()
            .into();
        assert_eq!(
            message("test", &raw).unwrap_err().kind,
            ErrorKind::LimitExceeded
        );
        raw["create_time"] = "invalid".into();
        raw["body"]["content"] = json!({"text":"ok"}).to_string().into();
        assert_eq!(
            message("test", &raw).unwrap_err().kind,
            ErrorKind::ProtocolError
        );
    }

    // —— 卡片表格的对齐与每页行数（契约 client v19；夹具取自 2026-09-30 取样，Friend 回溯 v63 §2） ——

    fn fixture(name: &str) -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/feishu")
            .join(name);
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    /// `raw_card_content` 读回里的 `json_card`。
    fn json_card(name: &str) -> Value {
        let raw = fixture(name);
        let content: Value = serde_json::from_str(
            raw.pointer("/data/items/0/body/content")
                .and_then(Value::as_str)
                .unwrap(),
        )
        .unwrap();
        serde_json::from_str(content["json_card"].as_str().unwrap()).unwrap()
    }

    /// 卡片里各表格的（对齐，每页行数）。
    fn tables(message: &ImMessage) -> Vec<(Vec<Option<RichAlign>>, Option<u16>)> {
        let mut out = Vec::new();
        for content in &message.content {
            if let ImContent::Card(card) = content {
                card.walk_rich(&mut |node| {
                    if let RichNode::Table {
                        align, page_size, ..
                    } = node
                    {
                        out.push((align.clone(), *page_size));
                    }
                });
            }
        }
        out
    }

    use RichAlign::{Center, Left, Right};

    #[test]
    fn markdown_tables_take_alignment_and_page_size_from_the_feishu_card_structure() {
        // 取样卡 A：T1 T2 T4 没标对齐、T3 左中右；读回（user_card_content）里对齐全没了，每页 10 行。
        let mut message = message("test", &fixture("response_card_v2_tables.json")).unwrap();
        assert!(has_unaligned_card_tables(&message));
        assert!(
            tables(&message)
                .iter()
                .all(|(align, size)| align.iter().all(Option::is_none) && *size == Some(10))
        );
        let layouts = card::table_layouts(&json_card("raw_card/tables.json"));
        assert_eq!(layouts.len(), 4);
        assert_eq!(apply_table_layouts(&mut message, &layouts), 4);
        assert_eq!(
            tables(&message),
            [
                (vec![Some(Left); 5], Some(10)),
                (vec![Some(Left); 2], Some(10)),
                (vec![Some(Left), Some(Center), Some(Right)], Some(10)),
                (vec![Some(Left); 3], Some(10)),
            ]
        );
        // 补过的表格都带了对齐：不再算“要补读”。
        assert!(!has_unaligned_card_tables(&message));
    }

    #[test]
    fn the_daily_report_gets_its_right_aligned_number_columns_back() {
        // 日报卡读回只剩两张表（夹具）；内部结构里有五张：按表头依次对上前两张，数字列靠右、“当前”列靠左。
        let mut message = message("test", &fixture("response_card_v2_report.json")).unwrap();
        let layouts = card::table_layouts(&json_card("raw_card/report.json"));
        assert_eq!(layouts.len(), 5);
        assert_eq!(apply_table_layouts(&mut message, &layouts), 2);
        assert_eq!(
            tables(&message),
            [
                (vec![Some(Left), Some(Left)], Some(10)),
                (
                    vec![
                        Some(Left),
                        Some(Right),
                        Some(Right),
                        Some(Right),
                        Some(Right),
                        Some(Left)
                    ],
                    Some(10)
                ),
            ]
        );
    }

    #[test]
    fn tables_that_do_not_match_keep_what_was_read_and_later_ones_still_match() {
        let mut message = message("test", &fixture("response_card_v2_tables.json")).unwrap();
        let mut layouts = card::table_layouts(&json_card("raw_card/tables.json"));
        // 第二张的表头对不上（比如飞书以后改了写法）：它照原样，第三、四张照样对上。
        layouts.remove(1);
        assert_eq!(apply_table_layouts(&mut message, &layouts), 3);
        let got = tables(&message);
        assert_eq!(got[1], (vec![None, None], Some(10)));
        assert_eq!(got[2].0, [Some(Left), Some(Center), Some(Right)]);
        assert!(
            has_unaligned_card_tables(&message),
            "对不上的那张还要在视野里重读一次"
        );
        // 内部结构认不出、没有表格：什么都不改。
        let mut again = message.clone();
        assert!(card::table_layouts(&json!({"body": {"property": {"elements": [{"tag": "table", "property": {"columns": "x"}}]}}})).is_empty());
        assert_eq!(apply_table_layouts(&mut again, &[]), 0);
        assert_eq!(again, message);
    }

    #[test]
    fn table_components_already_aligned_are_left_alone() {
        // 表格组件读回时每列都带对齐与每页行数：不补读、不被内部结构改掉。
        let mut message = message("test", &fixture("response_card_v2_components.json")).unwrap();
        let before = tables(&message);
        assert!(
            before
                .iter()
                .all(|(align, _)| align.iter().any(Option::is_some))
        );
        assert!(!has_unaligned_card_tables(&message));
        let headers = ["项目", "文档", "预算", "阶段", "负责人", "截止", "备注"];
        let layouts = card::table_layouts(
            &json!({"body": {"elements": [{"tag": "table", "property": {
            "columns": headers.iter().map(|h| json!({"displayName": h, "horizontalAlign": "center"})).collect::<Vec<_>>(),
            "pageSize": 3}}]}}),
        );
        assert_eq!(apply_table_layouts(&mut message, &layouts), 0);
        assert_eq!(tables(&message), before);
    }

    #[test]
    fn page_sizes_outside_what_feishu_allows_are_ignored() {
        let columns =
            json!([{"displayName": "名"}, {"displayName": "数", "horizontalAlign": "right"}]);
        for (size, expected) in [
            (json!(0), Some(10)),
            (json!(11), Some(10)),
            (json!("5"), Some(10)),
            (json!(3), Some(3)),
        ] {
            let mut message = message("test", &fixture("response_card_v2_tables.json")).unwrap();
            let layouts = card::table_layouts(
                &json!({"body": [{"tag": "table", "property": {"columns": columns, "pageSize": size}}]}),
            );
            assert_eq!(apply_table_layouts(&mut message, &layouts), 1);
            assert_eq!(
                tables(&message)[1],
                (vec![Some(Left), Some(Right)], expected),
                "{size}"
            );
        }
    }

    // —— 卡片里的 @（契约 client v31；夹具取自 2026-10-04 真实取样，编号、名字与链接换成假的） ——

    fn at_users_fixture() -> serde_json::Map<String, Value> {
        let structure = fixture("raw_card/at.json");
        let content: Value = serde_json::from_str(
            structure["data"]["items"][0]["body"]["content"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        content
            .pointer("/json_attachment/at_users")
            .and_then(Value::as_object)
            .cloned()
            .unwrap()
    }

    fn card_mention_actors(message: &ImMessage) -> Vec<ActorRef> {
        let mut out = Vec::new();
        for content in &message.content {
            if let ImContent::Card(card) = content {
                card.walk_rich(&mut |node| {
                    if let RichNode::Mention {
                        actor: Some(actor), ..
                    } = node
                    {
                        out.push(actor.clone());
                    }
                });
            }
        }
        out
    }

    #[test]
    fn card_mentions_from_another_app_are_matched_through_the_card_structure() {
        let raw = fixture("response_card_at.json");
        let mut message = message("test", &raw).unwrap();
        // 卡片里的编号是发卡片那个应用的，消息的提及名单里没有：要按飞书内部结构对。
        assert_eq!(
            unknown_card_mentions(&message, &raw),
            ["ou_card_app_trigger"]
        );
        let reader = ActorRef {
            id: "ou_reader_trigger".into(),
            id_type: "open_id".into(),
            kind: "user".into(),
            name: Some("测试同事".into()),
        };
        let at_users = at_users_fixture();
        let unknown = ["ou_card_app_trigger".to_owned()];
        let pairs = card_at_users(
            &at_users,
            raw.get("mentions").and_then(Value::as_array),
            &unknown,
        );
        assert_eq!(pairs, [("ou_card_app_trigger".to_owned(), reader.clone())]);
        assert_eq!(replace_card_mentions(&mut message, &pairs), 1);
        assert_eq!(card_mention_actors(&message), [reader]);
        assert!(unknown_card_mentions(&message, &raw).is_empty());
        // 提及名单里没有那一项：编号照旧，只带上内部结构给的名字。
        let pairs = card_at_users(&at_users, None, &unknown);
        assert_eq!(pairs[0].1.id, "ou_card_app_trigger");
        assert_eq!(pairs[0].1.name.as_deref(), Some("测试同事"));
        // 内部结构里也没有的：不换。
        assert!(card_at_users(&at_users, None, &["ou_other".to_owned()]).is_empty());
    }

    #[test]
    fn card_mentions_from_the_same_app_take_their_names_from_the_mention_list() {
        let mut raw = fixture("response_card_at.json");
        // 同一个应用发的卡片：卡片里的编号就在提及名单里，不用补读，名字照名单。
        let content = raw["body"]["content"]
            .as_str()
            .unwrap()
            .replace("ou_card_app_trigger", "ou_reader_trigger");
        raw["body"]["content"] = content.into();
        let message = message("test", &raw).unwrap();
        assert!(unknown_card_mentions(&message, &raw).is_empty());
        let actors = card_mention_actors(&message);
        assert_eq!(actors.len(), 1);
        assert_eq!(actors[0].id, "ou_reader_trigger");
        assert_eq!(actors[0].name.as_deref(), Some("测试同事"));
    }

    #[test]
    fn cards_stored_with_raw_at_text_are_reread_once() {
        // 升级前存下的：@ 还在卡片 1.0 的 Markdown 段落里；删掉的不重读；现在翻出来的不再重读。
        let fresh = message("test", &fixture("response_card_at.json")).unwrap();
        assert!(!has_raw_card_at_text(&fresh));
        let mut stored = fresh.clone();
        stored.content = vec![ImContent::Card(ImCard {
            blocks: vec![CardBlock::Paragraph {
                parts: vec![CardInline::Markdown {
                    text: "触发人：<at id=ou_card_app_trigger></at>".into(),
                }],
            }],
            ..ImCard::default()
        })];
        assert!(has_raw_card_at_text(&stored));
        let payload = serde_json::to_string(&stored).unwrap();
        assert!(payload.contains(super::super::CARD_MENTION_REREAD.contains));
        assert!((super::super::CARD_MENTION_REREAD.applies)(&stored));
        stored.deleted = true;
        assert!(!has_raw_card_at_text(&stored));
    }

    #[test]
    fn cards_stored_before_the_upgrade_are_reread_once() {
        // 升级前存下的：每页 5 行、对齐为空；删掉的不重读；不是卡片的格式正文表格不算。
        let mut stored = message("test", &fixture("response_card_v2_report.json")).unwrap();
        for content in &mut stored.content {
            if let ImContent::Card(card) = content {
                card.walk_blocks_mut(&mut |block| {
                    if let CardBlock::Rich { children } = block {
                        RichNode::walk_all_mut(children, &mut |node| {
                            if let RichNode::Table { page_size, .. } = node {
                                *page_size = Some(5);
                            }
                        });
                    }
                });
            }
        }
        assert!(has_unaligned_card_tables(&stored));
        let payload = serde_json::to_string(&stored).unwrap();
        assert!(payload.contains(super::super::CARD_TABLE_REREAD.contains));
        assert!((super::super::CARD_TABLE_REREAD.applies)(&stored));
        stored.deleted = true;
        assert!(!has_unaligned_card_tables(&stored));
        let rich = message("test", &fixture("response_post_markdown.json")).unwrap();
        assert!(!has_unaligned_card_tables(&rich));
    }
}
