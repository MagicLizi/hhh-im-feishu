//! 协议样例：飞书的取样响应（`tests/fixtures/feishu/`）翻成线上消息，存在 `tests/fixtures/feishu-protocol/`。
//!
//! 每份样例都核对两件事：插件给出的线上消息与样例逐项相同（样例锁住插件的输出）；宿主把它翻回来（照抄的
//! [`super::host_inverse`]）与 HHH 内置飞书翻出来的一字不差（`*.expected.json`）。这些样例会拷进 HHH，宿主那边
//! 再拿真的翻译核对一遍。
//!
//! 改了翻译、样例要跟着变时：`HHH_UPDATE_GOLDENS=1 cargo test goldens` 重写样例，再看一遍差异。

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::host_inverse;
use crate::feishu::{card, translate};
use crate::*;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn read(path: &Path) -> Value {
    serde_json::from_slice(
        &std::fs::read(path).unwrap_or_else(|e| panic!("{}：{e}", path.display())),
    )
    .unwrap_or_else(|e| panic!("{}：{e}", path.display()))
}

/// 样例与这次算出的比对；设了 `HHH_UPDATE_GOLDENS` 时改写样例。
fn lock(name: &str, actual: &Value) {
    let path = fixtures().join("feishu-protocol").join(name);
    if std::env::var_os("HHH_UPDATE_GOLDENS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut text = serde_json::to_string_pretty(actual).unwrap();
        text.push('\n');
        std::fs::write(&path, text).unwrap();
        return;
    }
    assert!(
        path.exists(),
        "样例 {} 还没有：HHH_UPDATE_GOLDENS=1 cargo test goldens 生成",
        path.display()
    );
    assert_eq!(&read(&path), actual, "样例 {} 对不上", path.display());
}

/// 一条内部消息走一趟线上：插件翻出去、宿主翻回来，必须一字不差；返回线上的 JSON。
fn round_trip(internal: &ImMessage, what: &str) -> Value {
    let wire = super::message(internal);
    let value = serde_json::to_value(&wire).unwrap();
    let parsed: hhh_im_protocol::types::Message = serde_json::from_value(value.clone()).unwrap();
    let back = host_inverse::message("test", parsed);
    assert_eq!(
        serde_json::to_value(&back).unwrap(),
        serde_json::to_value(internal).unwrap(),
        "{what}：宿主翻回来与内置飞书的不一样"
    );
    value
}

/// 24 份取样消息：内部翻译与 `*.expected.json` 一致（同 translate 的单测），线上样例锁住，宿主翻回来还是它。
#[test]
fn every_sampled_response_has_a_wire_golden_that_translates_back_exactly() {
    let directory = fixtures().join("feishu");
    let mut count = 0;
    for entry in std::fs::read_dir(&directory).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.starts_with("response_")
            || !name.ends_with(".json")
            || name.ends_with(".expected.json")
        {
            continue;
        }
        let raw = read(&path);
        let expected = read(&path.with_extension("expected.json"));
        let internal = translate::message("test", &raw).unwrap();
        assert_eq!(serde_json::to_value(&internal).unwrap(), expected, "{name}");
        let wire = round_trip(&internal, &name);
        lock(&name, &wire);
        count += 1;
    }
    assert_eq!(count, 24, "取样消息没读全");
}

/// 表情回复的取样：线上样例锁住，宿主翻回来与 `reactions/list.expected.json` 一致。
#[test]
fn sampled_reactions_have_a_wire_golden() {
    let expected = read(&fixtures().join("feishu/reactions/list.expected.json"));
    let internal: Vec<MessageReaction> = serde_json::from_value(expected.clone()).unwrap();
    let wire: Vec<_> = internal.iter().map(super::reaction).collect();
    let back: Vec<MessageReaction> = wire
        .iter()
        .cloned()
        .map(|r| host_inverse::reaction("test", r))
        .collect();
    assert_eq!(serde_json::to_value(&back).unwrap(), expected);
    lock("reactions_list.json", &serde_json::to_value(&wire).unwrap());
}

/// 编辑取样（契约 client v26）：每一对改前、改后都走一趟线上。
#[test]
fn edit_samples_have_wire_goldens() {
    let fixture = read(&fixtures().join("feishu/edits/samples-v26.json"));
    let mut pairs = Vec::new();
    for pair in fixture["pairs"].as_array().unwrap() {
        let name = pair["name"].as_str().unwrap();
        let before = translate::message("test", &pair["before"]).unwrap();
        let after = translate::message("test", &pair["after"]).unwrap();
        pairs.push(json!({
            "name": name,
            "before": round_trip(&before, name),
            "after": round_trip(&after, name),
        }));
    }
    assert_eq!(pairs.len(), 5);
    lock("edits_samples_v26.json", &json!({ "pairs": pairs }));
}

/// 一份 raw_card_content 读回里的卡片内容（`body.content` 解开）。
fn raw_card_content(name: &str) -> Value {
    let raw = read(&fixtures().join("feishu/raw_card").join(name));
    serde_json::from_str(raw["data"]["items"][0]["body"]["content"].as_str().unwrap()).unwrap()
}

/// 卡片补读飞书内部结构之后的样子（契约 client v19 §2、v31 §2）：表格补上对齐、别的应用的 @ 换成能认的人，
/// 再走一趟线上。这两份另带内部的样子（`*.expected.json`），宿主那边拿它核对。
#[test]
fn cards_completed_from_the_raw_card_structure_have_wire_goldens() {
    let tables = read(&fixtures().join("feishu/response_card_v2_tables.json"));
    let mut message = translate::message("test", &tables).unwrap();
    let json_card: Value = serde_json::from_str(
        raw_card_content("tables.json")["json_card"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let layouts = card::table_layouts(&json_card);
    assert!(translate::apply_table_layouts(&mut message, &layouts) > 0);
    let name = "response_card_v2_tables.with_raw_card.json";
    lock(name, &round_trip(&message, name));
    lock(
        "response_card_v2_tables.with_raw_card.expected.json",
        &serde_json::to_value(&message).unwrap(),
    );

    let at = read(&fixtures().join("feishu/response_card_at.json"));
    let mut message = translate::message("test", &at).unwrap();
    let unknown = translate::unknown_card_mentions(&message, &at);
    assert!(!unknown.is_empty());
    let content = raw_card_content("at.json");
    let at_users = content
        .pointer("/json_attachment/at_users")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let pairs = translate::card_at_users(
        &at_users,
        at.get("mentions").and_then(Value::as_array),
        &unknown,
    );
    assert!(translate::replace_card_mentions(&mut message, &pairs) > 0);
    let name = "response_card_at.with_raw_card.json";
    lock(name, &round_trip(&message, name));
    lock(
        "response_card_at.with_raw_card.expected.json",
        &serde_json::to_value(&message).unwrap(),
    );
}

/// 规范写法（§7.6、附录 B）：格式正文与卡片里的人、资源不带内部的 `actor`、`message`、`size`。
#[test]
fn rich_and_card_people_and_resources_use_the_written_form() {
    let mut texts = Vec::new();
    for entry in std::fs::read_dir(fixtures().join("feishu-protocol")).unwrap() {
        texts.push(std::fs::read_to_string(entry.unwrap().path()).unwrap());
    }
    for text in texts.iter().filter(|text| !text.contains("\"reference\"")) {
        assert!(
            !text.contains("\"actor\""),
            "线上不该有内部的 actor：{text}"
        );
        assert!(
            !text.contains("\"connectionId\""),
            "线上不该有连接编号：{text}"
        );
        assert!(
            !text.contains("\"size\": {"),
            "线上的宽高写成 width、height：{text}"
        );
    }
}

/// 搜群、搜人、定位私聊的取样（`search_responses.expected.json`）与最近聊天里的会话：线上样例锁住，宿主翻回来一样。
#[test]
fn search_and_conversation_samples_have_wire_goldens() {
    let search = read(&fixtures().join("search_responses.expected.json"));
    let groups: Vec<Conversation> =
        serde_json::from_value(search["groups"]["items"].clone()).unwrap();
    let users: Vec<SearchUser> = serde_json::from_value(search["users"]["items"].clone()).unwrap();
    let wire_groups: Vec<_> = groups.iter().map(super::conversation).collect();
    let wire_users: Vec<_> = users.iter().map(super::person).collect();
    for (wire, expected) in wire_groups.iter().zip(&groups) {
        assert_eq!(&host_inverse::conversation("test", wire.clone()), expected);
    }
    for (wire, expected) in wire_users.iter().zip(&users) {
        let back = host_inverse::search_user(wire.clone());
        assert_eq!(
            (
                back.reference.id.as_str(),
                back.name.as_str(),
                back.avatar_url.as_deref()
            ),
            (
                expected.reference.id.as_str(),
                expected.name.as_str(),
                expected.avatar_url.as_deref()
            )
        );
    }
    lock(
        "search_responses.json",
        &json!({
            "groups": { "items": wire_groups, "notice": search["groups"]["notice"] },
            "users": { "items": wire_users },
            "direct": {
                "conversationId": search["direct"]["conversation"]["id"],
                "notice": search["direct"]["notice"],
            },
        }),
    );

    let recent = read(&fixtures().join("recent_responses.expected.json"));
    let conversations: Vec<Conversation> = recent["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            let mut conversation: Conversation =
                serde_json::from_value(item["conversation"].clone()).unwrap();
            conversation.peer = serde_json::from_value(item["peer"].clone()).unwrap_or_default();
            conversation
        })
        .collect();
    let wire: Vec<_> = conversations.iter().map(super::conversation).collect();
    for (wire, expected) in wire.iter().zip(&conversations) {
        assert_eq!(&host_inverse::conversation("test", wire.clone()), expected);
    }
    lock("conversations.json", &serde_json::to_value(&wire).unwrap());
}

/// 账号：编号种类写 open_id，宿主翻回来与内置时代核验出的一样（工作区不给）。
#[test]
fn the_account_translates_back_with_its_id_type() {
    let account = Account {
        connection_id: "test".into(),
        platform: "feishu".into(),
        user: ActorRef {
            id: "ou_me".into(),
            id_type: "open_id".into(),
            kind: "user".into(),
            name: Some("Alice".into()),
        },
        tenant_id: None,
        avatar_url: Some(
            "https://s1-imfile.feishucdn.com/static-resource/v1/v3_me~?image_size=72x72".into(),
        ),
    };
    let wire = super::account(&account);
    assert_eq!(
        host_inverse::account("test", "feishu", wire.clone()),
        account
    );
    lock("account.json", &serde_json::to_value(&wire).unwrap());
}

/// 表情目录（开放平台目录补上随包快照之后，同内置读目录时那一步）：中文名、进不进面板、肤色名字（“深色”这类，
/// 肤色版本不进面板、不说是哪个表情的肤色）都带上；宿主翻回来除了图片地址（宿主记成类型、按类型向插件要图）一字不差。
#[test]
fn the_emoji_catalog_keeps_names_tones_and_the_panel() {
    let official = EmojiCatalog {
        emojis: vec![
            EmojiInfo::new(
                "THUMBSUP",
                "https://sf3-ttcdn-tos.pstatp.com/obj/lark-reaction-cn/emoji_thumbsup_v2.png",
            ),
            EmojiInfo::new(
                "SMILE",
                "https://sf3-ttcdn-tos.pstatp.com/obj/lark-reaction-cn/emoji_smile_v2.png",
            ),
        ],
        source_updated_ms: 1_700_000_000_000,
        fetched_at_ms: 1_700_000_000_000,
        categories: Vec::new(),
        tones: Vec::new(),
    };
    let catalog = crate::facts::complete_emoji_catalog(official);
    let wire = super::emoji_catalog(&catalog);
    assert_eq!(wire.updated_at_ms, catalog.source_updated_ms);
    assert_eq!(wire.emojis.len(), catalog.emojis.len());
    for (sent, internal) in wire.emojis.iter().zip(&catalog.emojis) {
        let mut back = host_inverse::emoji(sent.clone());
        assert_eq!(back.image_url, internal.emoji_type, "宿主按类型要图");
        back.image_url = internal.image_url.clone();
        assert_eq!(&back, internal, "{}", internal.emoji_type);
    }
    let dark = wire
        .emojis
        .iter()
        .find(|emoji| emoji.emoji_type == "DarkThumbsup")
        .expect("快照里有深色的赞");
    assert_eq!(dark.tone.as_deref(), Some("深色"));
    assert!(!dark.panel && dark.tone_of.is_none());
    // 写不进文字的（HHH 契约 provider-protocol v31 取样）标 `inText: false`，别的不写这一项（宿主读成能写进文字）。
    let json = super::emoji_catalog_json(&catalog);
    let in_text = |t: &str| {
        json["emojis"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["emojiType"] == t)
            .map(|e| e.get("inText").cloned())
    };
    assert_eq!(
        in_text("2021"),
        Some(Some(json!(false))),
        "快照里有 [2021年]，标成写不进文字"
    );
    assert_eq!(in_text("THUMBSUP"), Some(None));
    lock("emoji_catalog.json", &json);
}

/// 提及机器人（内部记成应用）：线上写 `kind: bot`，宿主记回应用；提及人的写 `kind: user`。飞书提及里原样带来的
/// `mentioned_type = bot` 也按机器人写（内置照原样记 `bot`、界面当人显示；协议只分人和机器人）。
#[test]
fn mentions_of_bots_carry_the_bot_kind() {
    let raw = json!({
        "message_id": "om_mention_bot", "chat_id": "oc_group", "msg_type": "text",
        "create_time": "1700000001000", "update_time": "1700000001000",
        "sender": {"id": "ou_sender", "id_type": "open_id", "sender_type": "user"},
        "body": {"content": "{\"text\":\"@_user_1 @_user_2 看一下\"}"},
        "mentions": [
            {"key": "@_user_1", "id": "cli_bot", "id_type": "app_id", "name": "日报机器人", "mentioned_type": "app"},
            {"key": "@_user_2", "id": "ou_member", "id_type": "open_id", "name": "Bob", "mentioned_type": "user"}
        ]
    });
    let internal = translate::message("test", &raw).unwrap();
    let wire = round_trip(&internal, "提及机器人");
    let kinds: Vec<_> = wire["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|block| block["type"] == "mention")
        .map(|block| (block["userId"].clone(), block["kind"].clone()))
        .collect();
    assert_eq!(
        kinds,
        vec![
            (json!("cli_bot"), json!("bot")),
            (json!("ou_member"), json!("user"))
        ]
    );

    let mut raw_bot = raw.clone();
    raw_bot["mentions"][0]["mentioned_type"] = "bot".into();
    let internal = translate::message("test", &raw_bot).unwrap();
    let sent = super::message(&internal);
    let back = host_inverse::message("test", sent);
    let ImContent::Mention {
        actor: Some(actor), ..
    } = &back.content[0]
    else {
        panic!("第一块是提及");
    };
    assert_eq!(
        actor.kind, APP_SENDER_KIND,
        "飞书说是机器人的，宿主记成应用"
    );
}

/// 视频消息（`media`）在宿主那边一定是视频：文件名认不出是视频、也没有封面时，照样带视频的 MIME（按 MP4）。
#[test]
fn media_messages_always_reach_the_host_as_videos() {
    let mut raw = read(&fixtures().join("feishu/response_media.json"));
    let content: Value = serde_json::from_str(raw["body"]["content"].as_str().unwrap()).unwrap();
    for (name, mime) in [
        (Some("clip.unknownext"), "video/mp4"),
        (None, "video/mp4"),
        (Some("IMG_0001.MOV"), "video/quicktime"),
    ] {
        let mut body = content.clone();
        let object = body.as_object_mut().unwrap();
        object.remove("image_key");
        match name {
            Some(name) => {
                object.insert("file_name".into(), name.into());
            }
            None => {
                object.remove("file_name");
            }
        }
        raw["body"]["content"] = body.to_string().into();
        let internal = translate::message("test", &raw).unwrap();
        let wire = round_trip(&internal, "视频消息");
        assert_eq!(wire["content"][0]["resource"]["mime"], mime, "{name:?}");
        let back = host_inverse::message("test", serde_json::from_value(wire).unwrap());
        assert!(
            matches!(&back.content[0], ImContent::Resource { video: Some(_), .. }),
            "{name:?}：宿主认成视频"
        );
    }
}
