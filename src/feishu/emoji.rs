//! 飞书的表情目录：开放平台没有取表情列表的接口，全部可用于表情回复的表情列在开放平台文档
//! “表情文案说明”里。这里从文档站读这篇文档的内容，解析出表情类型与官方图片地址；图片从飞书 CDN 下载。
//! 两者都不需要、也不带用户令牌。文档格式看不懂时报错，由调用方保留上次成功的目录。
//!
//! 文档只列一部分表情、也没有中文名（契约 client v15 §2）：飞书客户端里其余的表情与全部中文名来自随包快照
//! `emoji_extra.json`（读目录时并进去）；目录与快照里都没有的类型按命名规律猜图。

use super::*;

/// 文档站里“表情文案说明”的内容地址（相对 OpenAPI 根地址）。
const CATALOG_PATH: &[&str] = &["document_portal", "v1", "document", "get_detail"];
const CATALOG_DOC: &str = "/server-docs/im-v1/message-reaction/emojis-introduce";
/// 一张表情图的下载上限（官方图约 7–20 KB）。
pub const EMOJI_IMAGE_MAX_BYTES: usize = 512 * 1024;
/// 目录里最多收多少个表情（文档目前 182 个）。
const CATALOG_MAX: usize = 1000;
/// 表情图只从飞书的 CDN 下载。
const IMAGE_HOST_SUFFIXES: &[&str] = &[".pstatp.com", ".feishucdn.com"];

impl FeishuClient {
    /// 从开放平台文档读当前可用于表情回复的表情目录（有序，按类型去重）。
    ///
    /// # Errors
    /// 读不到文档、文档格式变了（找不到表情表、图片地址不在飞书 CDN）时返回错误。
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example(client: &hhh_im_feishu::FeishuClient) -> hhh_im_feishu::ImResult<()> {
    /// let catalog = client.emoji_catalog().await?;
    /// println!("{} 个表情，文档更新于 {}", catalog.emojis.len(), catalog.source_updated_ms);
    /// # Ok(()) }
    /// ```
    pub async fn emoji_catalog(&self) -> ImResult<EmojiCatalog> {
        let url = self.url(CATALOG_PATH)?;
        // 文档站是公开内容：不带用户令牌。
        let response = self
            .http
            .get(url)
            .query(&[("fullPath", CATALOG_DOC)])
            .timeout(crate::im::client::QUERY_TIMEOUT)
            .send()
            .await
            .map_err(|error| ImError::network(error, "表情列表读取失败，请稍后重试。"))?;
        let raw = read_json(response).await?;
        let data = raw
            .get("data")
            .ok_or_else(|| invalid_catalog("表情文档缺少内容。"))?;
        let content = data
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_catalog("表情文档缺少正文。"))?;
        let source_updated_ms = data
            .get("updateTime")
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .ok_or_else(|| invalid_catalog("表情文档缺少更新时间。"))?;
        let emojis = parse_catalog(content, &self.config.api_base)?;
        Ok(EmojiCatalog {
            emojis,
            source_updated_ms,
            fetched_at_ms: now_ms(),
            categories: Vec::new(),
            tones: Vec::new(),
        })
    }

    /// 下载一个表情的官方图片（只接受飞书 CDN 上的 PNG、GIF、JPEG 或 WebP）。
    ///
    /// # Errors
    /// 地址不在飞书 CDN、下载失败、超过 [`EMOJI_IMAGE_MAX_BYTES`] 或不是图片时返回错误。
    pub async fn emoji_image(&self, emoji: &EmojiInfo) -> ImResult<Vec<u8>> {
        let url = image_url(&emoji.image_url, &self.config.api_base)?;
        let mut response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|error| ImError::network(error, "表情图下载失败，请稍后重试。"))?;
        if !response.status().is_success() {
            return Err(super::errors::from_code(
                -1,
                response.status().as_u16(),
                None,
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| ImError::network(error, "表情图下载中断。"))?
        {
            if bytes.len().saturating_add(chunk.len()) > EMOJI_IMAGE_MAX_BYTES {
                return Err(ImError::new(
                    ErrorKind::LimitExceeded,
                    "表情图超过大小上限。",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        if !is_image(&bytes) {
            return Err(invalid_catalog("下载到的表情图不是图片。"));
        }
        Ok(bytes)
    }
}

fn invalid_catalog(message: &str) -> ImError {
    ImError::new(ErrorKind::ProtocolError, message)
}

/// 随包快照（契约 client v15 §2）：开放平台目录以外、飞书客户端有的表情，与全部表情的中文名。
/// 由 `scripts/emoji-snapshot.mjs` 从飞书文档网页的公开前端资源生成；运行时不改它。
const SNAPSHOT_JSON: &str = include_str!("emoji_extra.json");
/// 表情图所在的飞书 CDN 目录（快照里只记文件名）。
const REACTION_IMAGE_BASE: &str = "https://sf3-ttcdn-tos.pstatp.com/obj/lark-reaction-cn/";
/// 猜图时依次试的版本后缀：飞书的表情图名是 `emoji_<小写类型>[_vN].png`（2026-09-30 核对现有 270 个，猜中 254 个）。
const GUESS_SUFFIXES: [&str; 5] = ["", "_v1", "_v2", "_v3", "_v4"];
/// 能拿来猜图的表情类型最长多少字节（现有最长约 30）。
const GUESS_TYPE_MAX_BYTES: usize = 48;
/// 给界面的“目录外表情”规则（契约 client v17 §7）：能猜图的类型与 [`guessed_images`] 一致
/// （`guess_pattern_agrees_with_guessed_images` 对拍），找不到图的纯数字编号是企业表情。
pub(crate) const UNLISTED_EMOJI: UnlistedEmoji = UnlistedEmoji {
    guess_pattern: Some("^(?=.*[A-Za-z_])[A-Za-z0-9_]{1,48}$"),
    placeholder_pattern: Some("^[0-9]+$"),
    placeholder_label: "企业表情",
};

#[derive(Deserialize)]
struct Snapshot {
    /// 开放平台目录里的表情的中文名（按接口类型）。
    names: std::collections::HashMap<String, String>,
    /// 目录以外的表情，按飞书前端里的顺序。
    extras: Vec<SnapshotEmoji>,
}

#[derive(Deserialize)]
struct SnapshotEmoji {
    #[serde(rename = "type")]
    emoji_type: String,
    image: String,
    name: Option<String>,
    panel: bool,
    tone: Option<String>,
}

/// 读随包快照；快照随源码一起检查过（`the_bundled_snapshot_parses_and_names_every_emoji`），读不懂时按没有快照。
fn snapshot() -> &'static Snapshot {
    static SNAPSHOT: std::sync::OnceLock<Snapshot> = std::sync::OnceLock::new();
    SNAPSHOT.get_or_init(|| {
        serde_json::from_str(SNAPSHOT_JSON).unwrap_or_else(|error| {
            tracing::error!(%error, "随包的表情快照读不懂，只用开放平台目录");
            Snapshot {
                names: Default::default(),
                extras: Vec::new(),
            }
        })
    })
}

/// 表情的中文名（随包快照）：发富文本的 `md` 段时表情写成 `[名字]`（协议 provider-protocol v40）。
pub(crate) fn name_of(emoji_type: &str) -> Option<String> {
    let snapshot = snapshot();
    snapshot.names.get(emoji_type).cloned().or_else(|| {
        snapshot
            .extras
            .iter()
            .find(|emoji| emoji.emoji_type == emoji_type)
            .and_then(|emoji| emoji.name.clone())
    })
}

/// 把开放平台目录补全（契约 client v15 §2）：官方表情补上中文名，后面接上快照里目录没有的表情
/// （飞书以后把它们写进文档时以文档为准，不重复）。本机缓存仍只存开放平台目录，这一步在读目录时做。
pub(crate) fn complete_catalog(mut catalog: EmojiCatalog) -> EmojiCatalog {
    let snapshot = snapshot();
    for emoji in &mut catalog.emojis {
        if emoji.name.is_none() {
            // 快照生成后飞书才写进文档的表情，中文名在快照的目录外表情里。
            emoji.name = snapshot.names.get(&emoji.emoji_type).cloned().or_else(|| {
                snapshot
                    .extras
                    .iter()
                    .find(|extra| extra.emoji_type == emoji.emoji_type)
                    .and_then(|extra| extra.name.clone())
            });
        }
    }
    let known: std::collections::HashSet<String> = catalog
        .emojis
        .iter()
        .map(|e| e.emoji_type.clone())
        .collect();
    catalog.emojis.extend(
        snapshot
            .extras
            .iter()
            .filter(|extra| !known.contains(&extra.emoji_type))
            .map(|extra| EmojiInfo {
                emoji_type: extra.emoji_type.clone(),
                image_url: format!("{REACTION_IMAGE_BASE}{}", extra.image),
                name: extra.name.clone(),
                panel: extra.panel,
                tone: extra.tone.clone(),
                unicode: None,
                category: None,
                aliases: Vec::new(),
                tone_of: None,
            }),
    );
    catalog
}

/// 目录里没有的表情类型按飞书的命名规律猜图（契约 client v15 §2）：依次返回要试的地址。
/// 企业表情（纯数字编号）与格式不对的类型不猜，返回空。
pub(crate) fn guessed_images(emoji_type: &str) -> Vec<EmojiInfo> {
    let plausible = !emoji_type.is_empty()
        && emoji_type.len() <= GUESS_TYPE_MAX_BYTES
        && !emoji_type.bytes().all(|b| b.is_ascii_digit())
        && emoji_type
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if !plausible {
        return Vec::new();
    }
    let stem = emoji_type.to_ascii_lowercase();
    GUESS_SUFFIXES
        .iter()
        .map(|suffix| {
            EmojiInfo::new(
                emoji_type,
                format!("{REACTION_IMAGE_BASE}emoji_{stem}{suffix}.png"),
            )
        })
        .collect()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// 图片文件头：PNG、GIF、JPEG、WebP。
pub(crate) fn is_image(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a])
        || bytes.starts_with(b"GIF8")
        || bytes.starts_with(&[0xff, 0xd8, 0xff])
        || (bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP")
}

/// 表情图地址：`//` 开头补 https；只接受飞书 CDN（本机测试地址时也接受同一测试服务）。
fn image_url(raw: &str, api_base: &str) -> ImResult<Url> {
    let full = if raw.starts_with("//") {
        format!("https:{raw}")
    } else {
        raw.to_owned()
    };
    let url = Url::parse(&full).map_err(|_| invalid_catalog("表情图地址格式不正确。"))?;
    validate_url(&url)?;
    let host = url.host_str().unwrap_or_default();
    let test_host = Url::parse(api_base)
        .ok()
        .and_then(|base| base.host_str().map(str::to_owned));
    let local = url.scheme() == "http" && test_host.as_deref() == Some(host);
    if !local
        && !IMAGE_HOST_SUFFIXES
            .iter()
            .any(|suffix| host.ends_with(suffix))
    {
        return Err(invalid_catalog("表情图地址不在飞书 CDN 上。"));
    }
    Ok(url)
}

/// 从文档正文里解析表情表：一个单元格是 Markdown 图片 `![类型](地址?height=…)`，下一个单元格是类型名。
pub(crate) fn parse_catalog(content: &str, api_base: &str) -> ImResult<Vec<EmojiInfo>> {
    let mut cells = Vec::new();
    let mut rest = content;
    while let Some(start) = rest.find("<md-td") {
        let after = &rest[start..];
        let Some(open_end) = after.find('>') else {
            break;
        };
        let Some(close) = after.find("</md-td>") else {
            break;
        };
        if close < open_end {
            break;
        }
        cells.push(after[open_end + 1..close].trim());
        rest = &after[close + "</md-td>".len()..];
    }
    let mut emojis: Vec<EmojiInfo> = Vec::new();
    let mut index = 0;
    while index + 1 < cells.len() {
        let Some(src) = markdown_image(cells[index]) else {
            index += 1;
            continue;
        };
        let emoji_type = cells[index + 1].trim();
        if markdown_image(emoji_type).is_some() {
            index += 1;
            continue;
        }
        reactions::validate_emoji_type(emoji_type)
            .map_err(|_| invalid_catalog("表情文档里有看不懂的表情类型。"))?;
        let clean = src.split('?').next().unwrap_or(src);
        let url = image_url(clean, api_base)?;
        if emojis.iter().any(|e| e.emoji_type == emoji_type) {
            return Err(invalid_catalog("表情文档里有重复的表情类型。"));
        }
        emojis.push(EmojiInfo::new(emoji_type, url.to_string()));
        if emojis.len() > CATALOG_MAX {
            return Err(invalid_catalog("表情文档里的表情多得不正常。"));
        }
        index += 2;
    }
    if emojis.is_empty() {
        return Err(invalid_catalog("表情文档里没有找到表情表，格式可能变了。"));
    }
    Ok(emojis)
}

/// 单元格里的 Markdown 图片地址。
fn markdown_image(cell: &str) -> Option<&str> {
    let start = cell.find("![")?;
    let open = cell[start..].find("](")? + start + 2;
    let close = cell[open..].find(')')? + open;
    let src = cell[open..close].trim();
    (!src.is_empty() && !src.contains(char::is_whitespace)).then_some(src)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://open.feishu.cn";

    #[test]
    fn the_bundled_snapshot_parses_and_names_every_emoji() {
        let snapshot: Snapshot = serde_json::from_str(SNAPSHOT_JSON).expect("随包快照必须读得懂");
        // 2026-09-30 生成：开放平台目录的 182 个都有中文名；目录外 25 个进面板、65 个肤色版本只显示。
        assert!(snapshot.names.len() >= 180);
        assert!(
            snapshot
                .extras
                .iter()
                .all(|e| e.name.as_deref().is_some_and(|n| !n.is_empty()))
        );
        assert!(
            snapshot
                .extras
                .iter()
                .all(|e| e.image.starts_with("emoji_") && e.image.ends_with(".png"))
        );
        // 2026-09-30 测试群逐个点过：25 个里 15 个点得上，其余 10 个飞书回 231001（不让点成表情回复）。
        assert!(snapshot.extras.iter().filter(|e| e.panel).count() >= 15);
        assert!(
            snapshot
                .extras
                .iter()
                .filter(|e| e.tone.is_some())
                .all(|e| !e.panel),
            "肤色版本不进面板"
        );
        // 取样核对过的：显示的图对上名字（BLUSH 的图是呲牙、XBLUSH 是脸红）、内部叫法不同的别名、客户端专有表情。
        for (emoji_type, name) in [
            ("THUMBSUP", "赞"),
            ("SMIRK", "坏笑"),
            ("BLUSH", "呲牙"),
            ("XBLUSH", "脸红"),
            ("JIAYI", "+1"),
            ("MUSCLE", "加油"),
        ] {
            assert_eq!(
                snapshot.names.get(emoji_type).map(String::as_str),
                Some(name),
                "{emoji_type}"
            );
        }
        let extra = |t: &str| snapshot.extras.iter().find(|e| e.emoji_type == t).unwrap();
        assert_eq!(extra("BLACKFACE").name.as_deref(), Some("新月脸"));
        assert_eq!(extra("BeAtTheForefront").name.as_deref(), Some("一马当先"));
        assert_eq!(extra("DarkThumbsup").tone.as_deref(), Some("深色"));
        // 快照里的类型不与目录的类型重复，按类型去重。
        let mut types: Vec<&str> = snapshot
            .extras
            .iter()
            .map(|e| e.emoji_type.as_str())
            .collect();
        types.sort_unstable();
        types.dedup();
        assert_eq!(types.len(), snapshot.extras.len());
    }

    #[test]
    fn completing_the_catalog_names_official_emojis_and_appends_the_rest_once() {
        let official = EmojiCatalog {
            emojis: vec![
                EmojiInfo::new(
                    "THUMBSUP",
                    "https://sf3-ttcdn-tos.pstatp.com/obj/lark-reaction-cn/emoji_thumbsup_v2.png",
                ),
                // 飞书以后把客户端专有的写进文档：以文档为准，不重复。
                EmojiInfo::new(
                    "BLACKFACE",
                    "https://sf3-ttcdn-tos.pstatp.com/obj/lark-reaction-cn/emoji_blackface_doc.png",
                ),
            ],
            source_updated_ms: 1,
            fetched_at_ms: 1,
            categories: Vec::new(),
            tones: Vec::new(),
        };
        let completed = complete_catalog(official);
        assert_eq!(completed.emojis[0].name.as_deref(), Some("赞"));
        assert!(completed.emojis[0].panel);
        let blackface: Vec<_> = completed
            .emojis
            .iter()
            .filter(|e| e.emoji_type == "BLACKFACE")
            .collect();
        assert_eq!(blackface.len(), 1);
        assert!(blackface[0].image_url.ends_with("emoji_blackface_doc.png"));
        assert_eq!(
            blackface[0].name.as_deref(),
            Some("新月脸"),
            "中文名仍从快照来"
        );
        let awesome = completed
            .emojis
            .iter()
            .find(|e| e.emoji_type == "AWESOME")
            .unwrap();
        assert_eq!(
            awesome.image_url,
            format!("{REACTION_IMAGE_BASE}emoji_awesome.png")
        );
        // 666 飞书不让点成表情回复（测试群实测 231001）：照样显示，不进面板。
        assert_eq!(
            (awesome.name.as_deref(), awesome.panel),
            (Some("666"), false)
        );
        assert!(blackface[0].panel, "新月脸在测试群点得上，进面板");
        let dark = completed
            .emojis
            .iter()
            .find(|e| e.emoji_type == "DarkThumbsup")
            .unwrap();
        assert!(!dark.panel);
        assert_eq!(completed.source_updated_ms, 1);
    }

    #[test]
    fn unknown_types_are_guessed_by_feishus_naming_but_enterprise_ids_are_not() {
        let urls: Vec<String> = guessed_images("Horse2027")
            .into_iter()
            .map(|e| e.image_url)
            .collect();
        assert_eq!(
            urls,
            ["", "_v1", "_v2", "_v3", "_v4"]
                .map(|s| format!("{REACTION_IMAGE_BASE}emoji_horse2027{s}.png"))
        );
        assert!(
            guessed_images("Horse2027")
                .iter()
                .all(|e| e.emoji_type == "Horse2027")
        );
        // 企业表情是纯数字编号，飞书没有公开的图；格式不对、过长的也不猜。
        for bad in ["7600319979925490618", "", "a/b", "a.b", &"A".repeat(60)] {
            assert!(guessed_images(bad).is_empty(), "{bad}");
        }
    }

    /// 界面拿到的规则与这里猜图、认企业表情的做法一致（契约 client v17 §7）；Friend 前端用同一夹具检查
    /// 这两条正则（`im-emoji-images.test.ts`），两边都对上才算规则没写岔。
    #[test]
    fn guess_pattern_agrees_with_guessed_images() {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Rules {
            guess_pattern: String,
            placeholder_pattern: String,
            placeholder_label: String,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Case {
            emoji_type: String,
            guess: bool,
            placeholder: bool,
        }
        #[derive(Deserialize)]
        struct Fixture {
            rules: Rules,
            cases: Vec<Case>,
        }
        let fixture: Fixture = serde_json::from_str(include_str!(
            "../../tests/fixtures/emoji/unlisted-feishu.json"
        ))
        .unwrap();
        assert_eq!(
            UNLISTED_EMOJI.guess_pattern,
            Some(fixture.rules.guess_pattern.as_str())
        );
        assert_eq!(
            UNLISTED_EMOJI.placeholder_pattern,
            Some(fixture.rules.placeholder_pattern.as_str())
        );
        assert_eq!(
            UNLISTED_EMOJI.placeholder_label,
            fixture.rules.placeholder_label
        );
        assert!(
            fixture
                .rules
                .guess_pattern
                .contains(&format!("{{1,{GUESS_TYPE_MAX_BYTES}}}")),
            "长度上限与猜图一致"
        );
        for case in &fixture.cases {
            let kind = &case.emoji_type;
            assert_eq!(
                !guessed_images(kind).is_empty(),
                case.guess,
                "猜图：{kind:?}"
            );
            let enterprise = !kind.is_empty() && kind.bytes().all(|b| b.is_ascii_digit());
            assert_eq!(enterprise, case.placeholder, "企业表情：{kind:?}");
        }
    }

    #[test]
    fn catalog_keeps_the_document_order_and_normalizes_image_addresses() {
        let content = "<md-table><md-tbody><md-tr>\
            <md-td>![OK](https://sf3-ttcdn-tos.pstatp.com/obj/lark-reaction-cn/emoji_ok_v2.png?height=96&lazyload=true&width=104)</md-td><md-td>OK</md-td>\
            <md-td>![THUMBSUP](https://sf3-ttcdn-tos.pstatp.com/obj/lark-reaction-cn/emoji_thumbsup_v2.png?height=96)</md-td><md-td>THUMBSUP</md-td>\
            <md-td>![](//sf3-cn.feishucdn.com/obj/open-platform-opendoc/abc_HAMMER.png?height=96)</md-td><md-td>HAMMER</md-td>\
            </md-tr><md-tr><md-td>![EatingFood](https://sf3-ttcdn-tos.pstatp.com/obj/lark-reaction-cn/emoji_eat.png)</md-td><md-td>EatingFood</md-td></md-tr></md-tbody></md-table>";
        let emojis = parse_catalog(content, BASE).unwrap();
        let types: Vec<&str> = emojis.iter().map(|e| e.emoji_type.as_str()).collect();
        assert_eq!(types, ["OK", "THUMBSUP", "HAMMER", "EatingFood"]);
        assert_eq!(
            emojis[0].image_url,
            "https://sf3-ttcdn-tos.pstatp.com/obj/lark-reaction-cn/emoji_ok_v2.png"
        );
        assert_eq!(
            emojis[2].image_url,
            "https://sf3-cn.feishucdn.com/obj/open-platform-opendoc/abc_HAMMER.png"
        );
    }

    #[test]
    fn catalog_rejects_changed_formats_duplicates_and_foreign_image_hosts() {
        assert!(parse_catalog("<p>没有表格</p>", BASE).is_err());
        let dup = "<md-td>![A](https://a.pstatp.com/1.png)</md-td><md-td>OK</md-td><md-td>![B](https://a.pstatp.com/2.png)</md-td><md-td>OK</md-td>";
        assert!(parse_catalog(dup, BASE).is_err());
        let foreign = "<md-td>![A](https://example.com/1.png)</md-td><md-td>OK</md-td>";
        assert!(parse_catalog(foreign, BASE).is_err());
        let bad_type = "<md-td>![A](https://a.pstatp.com/1.png)</md-td><md-td>OK THEN</md-td>";
        assert!(parse_catalog(bad_type, BASE).is_err());
    }

    #[test]
    fn only_real_image_bytes_count_as_emoji_images() {
        assert!(is_image(&[
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0
        ]));
        assert!(is_image(b"GIF89a..."));
        assert!(!is_image(b"<html>not found</html>"));
    }
}
