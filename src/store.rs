//! 测试用：从 HHH 的 hhh-im 本地消息库（`store.rs`）原样搬来的几个判断。
//!
//! 插件不存消息（聊天记录归宿主），这几个判断只给飞书的翻译测试核对“翻出来的消息在宿主那边会被怎么认”；
//! 宿主那边的正本改了，这里跟着改。

use crate::*;

/// 话题回复所属的根消息。平台说了这条在不在话题里（[`ImMessage::in_thread`]）就以平台为准；
/// 没说时按旧判断：带话题编号且带根消息的才是话题回复，根消息本身与普通引用回复都不是。
///
/// 旧判断会把“引用回复之后又被人开了话题”的消息错当成被引用那条的话题回复，所以平台给了就不用它。
pub fn thread_root_of(message: &ImMessage) -> Option<&str> {
    if message.in_thread == Some(false) {
        return None;
    }
    match (&message.thread_id, &message.root_message) {
        (Some(_), Some(root)) if root.id != message.reference.id => Some(root.id.as_str()),
        _ => None,
    }
}

/// 消息里有没有真实提及本人；提及所有人不算（与“有人@我”的口径一致）。格式正文里的提及也算（契约 client v6）。
pub fn mentions(message: &ImMessage, me: &ActorRef) -> bool {
    mentions_user(message, &me.id)
}

/// 同 [`mentions`]，按编号认本人。
fn mentions_user(message: &ImMessage, me: &str) -> bool {
    let mut found = false;
    for block in &message.content {
        block.for_each_mention(&mut |actor, everyone| {
            found |= !everyone && actor.is_some_and(|a| a.id == me);
        });
    }
    found
}

/// 消息里有没有提及所有人。
pub(crate) fn mentions_everyone(message: &ImMessage) -> bool {
    let mut found = false;
    for block in &message.content {
        block.for_each_mention(&mut |_, everyone| found |= everyone);
    }
    found
}
