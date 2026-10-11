//! 飞书卡片（interactive）翻译成通用卡片 [`ImCard`]。
//!
//! 读消息时带 `card_msg_content_type=user_card_content`，拿到的是发送时的原始卡片 JSON（卡片 1.0 或 2.0，
//! 2.0 以 `schema: "2.0"` 区分，内容是飞书规整过的读回形状）；不带这个参数时是飞书简化过的内容：一个标题，
//! 下面按行排列的元素（2.0 卡片的简化内容只有升级提示）。三种形状都翻成通用卡片。
//! 只翻真实取样核实过的元素：1.0 取样“开发通知”最近 50 条（2026-09-27）——标题栏、`div` 文字、
//! 多栏 `column_set`、分割线 `hr`；2.0 取样日报卡与 示例测试 群的 4 张测试卡（2026-09-30，契约 client v16）；
//! 简化内容的 `text`、`a`、`hr`。其它元素不猜它们的意思——跳过，并把消息标成不完整，
//! 界面据此提示“部分内容在平台里查看”。

use super::markdown::{self, Placeholders};
use super::optional;
use crate::*;
use serde_json::Value;

/// 一张卡片的块数上限（多栏里的块也算）。飞书卡片 JSON 本身不超过 30 KB，真实卡片远用不到。
const CARD_BLOCKS_MAX: usize = 512;
/// 一张卡片文字的总字节上限。
const CARD_TEXT_MAX_BYTES: usize = 64 * 1024;
/// 容器（多栏、折叠面板、交互容器、表单）里再套容器的层数上限：飞书容器最多嵌套五层。
const CARD_DEPTH_MAX: usize = 5;
/// 一张卡片里格式正文节点的总数上限（与富文本消息的节点预算一致）；日报卡约 1000 个。
const CARD_RICH_NODES_MAX: usize = 4096;
/// 格式正文的嵌套深度上限（列表、引用、样式互相嵌套），防止深树撑爆栈。
const CARD_RICH_DEPTH_MAX: usize = 24;
/// 表格组件的列数上限：飞书规定最多 50 列。
const CARD_TABLE_COLUMNS_MAX: usize = 50;
/// 一张卡片里表格行数的总上限；飞书卡片 JSON 不超过 30 KB，真实卡片远用不到。
const CARD_TABLE_ROWS_MAX: usize = 1000;
/// Markdown 表格每页几行：飞书客户端实测与内部结构的 `pageSize` 都是 10（契约 client v19 §1；
/// 飞书文档写“表头以外每页最多 5 行”，与客户端不符）。卡片照飞书分页（契约 im-watch v60）。
const MARKDOWN_TABLE_PAGE_SIZE: u16 = 10;
/// 从飞书内部结构里至多取几张表格的排法（与块数上限同一量级，真实卡片远用不到）。
const TABLE_LAYOUTS_MAX: usize = CARD_BLOCKS_MAX;
/// 表格组件没给 `page_size` 时每页几行（飞书默认 5，可设 1–10）。
const TABLE_PAGE_SIZE_DEFAULT: u16 = 5;
/// 标题栏标签至多几个：飞书规定最多 3 个。
const CARD_TAGS_MAX: usize = 3;
/// 飞书给卡片 2.0 的简化内容里只有这句升级提示（低版本客户端的兜底，取样核实），不翻进卡片。
const UPGRADE_TIP: &str = "请升级至最新版本客户端，以查看内容";

/// 翻好的卡片，以及里面有没有认不出的部分。
pub(crate) struct Translated {
    pub card: ImCard,
    pub incomplete: bool,
}

/// 卡片内容是哪种形状。
#[derive(Debug, PartialEq, Eq)]
enum Shape {
    /// 发送时的原始卡片 JSON（卡片 1.0）：`header` + 元素对象的列表。
    Raw,
    /// 发送时的原始卡片 JSON（卡片 2.0）：`schema: "2.0"`、`header` + `body.elements`。
    V2,
    /// 飞书简化过的内容：`title` + 行的列表（每行是元素的列表）。
    Simplified,
    /// 认不出：别的 `schema`、模板卡片或别的还没取样过的结构。
    Unknown,
}

fn shape(body: &Value) -> Shape {
    let Some(object) = body.as_object() else {
        return Shape::Unknown;
    };
    // 以 schema 区分版本：只认取样过的 2.0，别的不按 1.0 硬读。
    if let Some(schema) = object.get("schema") {
        let elements_ok = match body.pointer("/body/elements") {
            None | Some(Value::Null) => true,
            Some(Value::Array(items)) => items.iter().all(Value::is_object),
            Some(_) => false,
        };
        return if schema.as_str() == Some("2.0") && elements_ok {
            Shape::V2
        } else {
            Shape::Unknown
        };
    }
    match object.get("elements") {
        Some(Value::Array(items)) if !items.is_empty() => {
            if items.iter().all(Value::is_array) {
                Shape::Simplified
            } else if items.iter().all(Value::is_object) {
                Shape::Raw
            } else {
                Shape::Unknown
            }
        }
        // 没有元素的卡片：原始结构只剩 header，简化内容只剩 title（都是真实取样见过的）。
        None | Some(Value::Null) | Some(Value::Array(_)) => {
            if object.get("header").is_some_and(Value::is_object) {
                Shape::Raw
            } else if object.get("title").is_some_and(Value::is_string) {
                Shape::Simplified
            } else {
                Shape::Unknown
            }
        }
        Some(_) => Shape::Unknown,
    }
}

/// 这段卡片内容认不出结构：调用方可以不带原始卡片参数再读一次，拿简化内容。
pub(crate) fn needs_fallback(body: &Value) -> bool {
    shape(body) == Shape::Unknown
}

/// 把卡片内容翻成通用卡片；结构认不出、或标题和内容都没有时返回 None（调用方按暂不支持处理）。
/// `message` 是这张卡片所在的消息：卡片里的图片属于它（平台只按所在消息给图，取样核实）。
///
/// # Errors
/// 块数、嵌套、表格或文字超过读取预算时返回 [`ErrorKind::LimitExceeded`]。
pub(crate) fn translate(body: &Value, message: &MessageRef) -> ImResult<Option<Translated>> {
    let mut budget = Budget::default();
    let translated = match shape(body) {
        Shape::Raw => raw(body, message, &mut budget)?,
        Shape::V2 => v2(body, message, &mut budget)?,
        // 简化内容丢了多栏等排版：一律标不完整，界面提示去平台里看原样。
        Shape::Simplified => {
            let mut translated = simplified(body, &mut budget)?;
            translated.incomplete = true;
            translated
        }
        Shape::Unknown => return Ok(None),
    };
    let card = &translated.card;
    if card.title.is_none() && card.subtitle.is_none() && card.blocks.is_empty() {
        return Ok(None);
    }
    Ok(Some(translated))
}

/// 飞书内部结构里一张表格的排法：表头文字、每列对齐、每页行数（契约 client v19 §2）。
///
/// 读消息时的 `user_card_content` 把 Markdown 表格的对齐标记规整成了 `--------`（取样核实，Friend 回溯 v63 §2.2），
/// 带 `card_msg_content_type=raw_card_content` 读到的 `json_card` 是飞书客户端自己用的结构，Markdown 表格在里面
/// 已经是表格：`{"tag": "table", "property": {"columns": [{"displayName", "horizontalAlign"?}], "pageSize"}}`。
/// 这是飞书没写进文档的结构，只取这几个字段，对不上就不用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TableLayout {
    headers: Vec<String>,
    align: Vec<Option<RichAlign>>,
    page_size: Option<u16>,
}

/// 按文档顺序取出 `json_card` 里每张表格的排法；认不出的表格跳过。只看 `body`（没有时看整张）：
/// 取样里另有 `newBody`，为空，不再重复取一遍。
pub(crate) fn table_layouts(json_card: &Value) -> Vec<TableLayout> {
    fn visit(value: &Value, out: &mut Vec<TableLayout>) {
        if out.len() == TABLE_LAYOUTS_MAX {
            return;
        }
        match value {
            Value::Array(items) => items.iter().for_each(|item| visit(item, out)),
            Value::Object(object) => {
                let property = object.get("property");
                if object.get("tag").and_then(Value::as_str) == Some("table")
                    && let Some(columns) = property
                        .and_then(|p| p.get("columns"))
                        .and_then(Value::as_array)
                {
                    out.push(TableLayout {
                        headers: columns
                            .iter()
                            .map(|c| {
                                c.get("displayName")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .trim()
                                    .to_owned()
                            })
                            .collect(),
                        align: columns
                            .iter()
                            .map(|c| match c.get("horizontalAlign").and_then(Value::as_str) {
                                Some("left") => Some(RichAlign::Left),
                                Some("center") => Some(RichAlign::Center),
                                Some("right") => Some(RichAlign::Right),
                                _ => None,
                            })
                            .collect(),
                        page_size: property
                            .and_then(|p| p.get("pageSize"))
                            .and_then(Value::as_u64)
                            .filter(|n| (1..=10).contains(n))
                            .map(|n| n as u16),
                    });
                    return;
                }
                object.values().for_each(|item| visit(item, out));
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    visit(json_card.get("body").unwrap_or(json_card), &mut out);
    out
}

/// 表格还没有任何对齐信息：卡片 2.0 的 Markdown 表格读回时就是这样（表格组件读回时每列都带对齐）。
fn unaligned(align: &[Option<RichAlign>]) -> bool {
    align.iter().all(Option::is_none)
}

/// 卡片里还有没有对齐信息的表格：要按飞书内部结构补读一次（契约 client v19 §2、§3）。
pub(crate) fn has_unaligned_tables(card: &ImCard) -> bool {
    let mut found = false;
    card.walk_rich(&mut |node| {
        found |= matches!(node, RichNode::Table { align, .. } if unaligned(align));
    });
    found
}

/// 表格第一行（表头）每格的文字。
fn header_texts(rows: &[RichNode]) -> Vec<String> {
    fn text(node: &RichNode, out: &mut String) {
        match node {
            RichNode::Text { value } | RichNode::InlineCode { value } => out.push_str(value),
            _ => node.children().iter().for_each(|child| text(child, out)),
        }
    }
    rows.first().map_or_else(Vec::new, |row| {
        row.children()
            .iter()
            .map(|cell| {
                let mut out = String::new();
                text(cell, &mut out);
                out.trim().to_owned()
            })
            .collect()
    })
}

/// 没有对齐信息的表格表头：同一张卡片的这组表头变了（发送方更新了卡片），之前取到的排法就不再适用。
pub(crate) fn unaligned_table_headers(card: &ImCard) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    card.walk_rich(&mut |node| {
        if let RichNode::Table {
            align, children, ..
        } = node
            && unaligned(align)
        {
            out.push(header_texts(children));
        }
    });
    out
}

/// 按飞书内部结构给卡片里没有对齐信息的表格补上对齐与每页行数，返回补上了几张。
///
/// 按出现顺序往后找列数与表头文字都相同的一张（中间夹着的表格组件、对不上的都跳过）；对上的每列对齐照它，
/// 没标对齐的列记成靠左——与飞书的显示一致，也表示这张表补读过（重读规则按“还没有对齐信息”找）。
/// 对不上的表格保持原样。
pub(crate) fn apply_table_layouts(card: &mut ImCard, layouts: &[TableLayout]) -> usize {
    let mut next = 0;
    let mut applied = 0;
    card.walk_blocks_mut(&mut |block| {
        let CardBlock::Rich { children } = block else {
            return;
        };
        RichNode::walk_all_mut(children, &mut |node| {
            let RichNode::Table {
                align,
                page_size,
                children,
            } = node
            else {
                return;
            };
            if !unaligned(align) {
                return;
            }
            let headers = header_texts(children);
            let Some(found) = layouts[next..].iter().position(|l| l.headers == headers) else {
                return;
            };
            let layout = &layouts[next + found];
            *align = layout
                .align
                .iter()
                .map(|a| Some(a.unwrap_or(RichAlign::Left)))
                .collect();
            if let Some(size) = layout.page_size {
                *page_size = Some(size);
            }
            next += found + 1;
            applied += 1;
        });
    });
    applied
}

/// 翻一张卡片时累计的块数与文字字节，超过上限就停。
#[derive(Default)]
struct Budget {
    blocks: usize,
    bytes: usize,
}

impl Budget {
    fn take(&mut self, bytes: usize) -> ImResult<()> {
        self.blocks += 1;
        self.bytes += bytes;
        if self.blocks > CARD_BLOCKS_MAX || self.bytes > CARD_TEXT_MAX_BYTES {
            return Err(ImError::new(
                ErrorKind::LimitExceeded,
                "卡片内容超过读取预算。",
            ));
        }
        Ok(())
    }
}

fn paragraph(text: String, budget: &mut Budget) -> ImResult<CardBlock> {
    budget.take(text.len())?;
    Ok(CardBlock::Paragraph {
        parts: vec![CardInline::Markdown { text }],
    })
}

/// 平台的标题颜色归到几种色调；取样里见过的只有 blue，其余按飞书文档里的颜色名归类，不认识的不上色。
fn tone(template: &str) -> CardTone {
    match template {
        "blue" | "wathet" | "indigo" => CardTone::Blue,
        "green" | "turquoise" => CardTone::Green,
        "yellow" => CardTone::Yellow,
        "orange" => CardTone::Orange,
        "red" | "carmine" => CardTone::Red,
        "violet" | "purple" => CardTone::Purple,
        _ => CardTone::Neutral,
    }
}

/// 纯文字放进 Markdown 段落前转义：每个 ASCII 标点前加反斜杠，保证原样显示、不被当成格式或链接。
pub(crate) fn escape_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    for c in text.chars() {
        if c.is_ascii_punctuation() {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// 一个 https 链接写成 Markdown：地址用尖括号包住，括号、空格都不会截断地址。
/// 地址不是 https、或含尖括号与换行时只留显示文字。
fn link(text: &str, href: &str) -> String {
    let label = escape_markdown(text);
    let safe = href.starts_with("https://") && !href.contains(['<', '>', '\n', '\r', ' ']);
    if safe {
        format!("[{label}](<{href}>)")
    } else {
        label
    }
}

fn raw(body: &Value, message: &MessageRef, budget: &mut Budget) -> ImResult<Translated> {
    let header = body.get("header");
    let title = header
        .and_then(|h| h.pointer("/title/content"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned);
    if let Some(title) = &title {
        budget.take(title.len())?;
    }
    let tone = header
        .and_then(|h| h.get("template"))
        .and_then(Value::as_str)
        .map_or(CardTone::Neutral, tone);
    let mut incomplete = false;
    let blocks = match body.get("elements") {
        Some(Value::Array(items)) => raw_blocks(items, 0, message, budget, &mut incomplete)?,
        _ => Vec::new(),
    };
    let card = ImCard {
        title,
        tone,
        blocks,
        ..ImCard::default()
    };
    check_rich_budget(&card)?;
    Ok(Translated { card, incomplete })
}

/// 原始卡片的一串元素：`div` 的文字、`hr`、`column_set`；别的跳过并记下不完整。
fn raw_blocks(
    items: &[Value],
    depth: usize,
    message: &MessageRef,
    budget: &mut Budget,
    incomplete: &mut bool,
) -> ImResult<Vec<CardBlock>> {
    let mut blocks = Vec::new();
    for item in items {
        match item.get("tag").and_then(Value::as_str) {
            Some("div") => {
                // 取样里的 div 只带 text；fields、extra 等还没见过，有就算看不全。
                if item.get("fields").is_some() || item.get("extra").is_some() {
                    *incomplete = true;
                }
                let text = item.get("text");
                let content = text.and_then(|t| t.get("content")).and_then(Value::as_str);
                match (
                    text.and_then(|t| t.get("tag")).and_then(Value::as_str),
                    content,
                ) {
                    // 带飞书 @ 标签的 lark_md 按 2.0 的 Markdown 同一套翻成格式正文，@ 成为提及（契约 client v31 §2）。
                    (Some("lark_md"), Some(content)) if has_feishu_at(content) => {
                        budget.take(content.len())?;
                        let children = markdown_nodes(content, message)?;
                        if !children.is_empty() {
                            blocks.push(CardBlock::Rich { children });
                        }
                    }
                    // 别的 lark_md 就是 Markdown（取样里用到加粗、链接与换行），原样交给界面。
                    (Some("lark_md"), Some(content)) if !content.is_empty() => {
                        blocks.push(paragraph(content.to_owned(), budget)?);
                    }
                    (Some("plain_text"), Some(content)) if !content.is_empty() => {
                        blocks.push(paragraph(escape_markdown(content), budget)?);
                    }
                    (Some("lark_md" | "plain_text"), Some(_)) => {}
                    _ => *incomplete = true,
                }
            }
            Some("hr") => {
                budget.take(0)?;
                blocks.push(CardBlock::Divider);
            }
            Some("column_set") if depth < CARD_DEPTH_MAX => {
                let mut columns = Vec::new();
                for column in item
                    .get("columns")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    match (
                        column.get("tag").and_then(Value::as_str),
                        column.get("elements").and_then(Value::as_array),
                    ) {
                        (Some("column"), Some(children)) => {
                            columns.push(raw_blocks(
                                children,
                                depth + 1,
                                message,
                                budget,
                                incomplete,
                            )?);
                        }
                        (Some("column"), None) => columns.push(Vec::new()),
                        _ => *incomplete = true,
                    }
                }
                if columns.iter().any(|c| !c.is_empty()) {
                    budget.take(0)?;
                    blocks.push(CardBlock::Columns { columns });
                }
            }
            _ => *incomplete = true,
        }
    }
    Ok(blocks)
}

/// 文字、标签的颜色归到色调（契约 client v16 §2）：与标题色不同，灰要保留成灰；
/// `default`、RGBA 与认不出的返回空（不上色，文字照留）。
fn text_tone(color: &str) -> Option<CardTone> {
    match color {
        "grey" | "neutral" => Some(CardTone::Grey),
        "lime" => Some(CardTone::Green),
        _ => match tone(color) {
            CardTone::Neutral => None,
            known => Some(known),
        },
    }
}

/// 翻一张 2.0 卡片时的上下文：卡片所在的消息、预算与“有认不出的部分”。
struct V2<'a> {
    message: &'a MessageRef,
    budget: &'a mut Budget,
    incomplete: bool,
    table_rows: usize,
}

fn v2(body: &Value, message: &MessageRef, budget: &mut Budget) -> ImResult<Translated> {
    let header = body.get("header");
    let text = |key: &str| {
        header
            .and_then(|h| h.pointer(&format!("/{key}/content")))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
    };
    let (title, subtitle) = (text("title"), text("subtitle"));
    for part in title.iter().chain(&subtitle) {
        budget.take(part.len())?;
    }
    let tone = header
        .and_then(|h| h.get("template"))
        .and_then(Value::as_str)
        .map_or(CardTone::Neutral, tone);
    let tags = header
        .and_then(|h| h.get("text_tag_list"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tag| {
            let text = tag.pointer("/text/content").and_then(Value::as_str)?.trim();
            (!text.is_empty()).then(|| CardTag {
                text: text.to_owned(),
                tone: text_tone(tag.get("color").and_then(Value::as_str).unwrap_or_default())
                    .unwrap_or(CardTone::Neutral),
            })
        })
        .take(CARD_TAGS_MAX)
        .collect::<Vec<_>>();
    for tag in &tags {
        budget.take(tag.text.len())?;
    }
    let wide = body.pointer("/config/width_mode").and_then(Value::as_str) == Some("fill");
    let mut cx = V2 {
        message,
        budget,
        incomplete: false,
        table_rows: 0,
    };
    let blocks = match body.pointer("/body/elements") {
        Some(Value::Array(items)) => cx.blocks(items, 0)?,
        _ => Vec::new(),
    };
    let card = ImCard {
        title,
        subtitle,
        tags,
        tone,
        wide,
        blocks,
    };
    check_rich_budget(&card)?;
    Ok(Translated {
        card,
        incomplete: cx.incomplete,
    })
}

/// 一张卡片里格式正文的总节点数与嵌套深度（预算，防止无界小对象与深树）。
fn check_rich_budget(card: &ImCard) -> ImResult<()> {
    let mut count = 0;
    card.walk_rich(&mut |_| count += 1);
    fn deep(nodes: &[RichNode], level: usize) -> bool {
        level > CARD_RICH_DEPTH_MAX || nodes.iter().any(|n| deep(n.children(), level + 1))
    }
    let mut too_deep = false;
    card.walk_blocks(&mut |block| {
        if let CardBlock::Rich { children }
        | CardBlock::Panel {
            title: children, ..
        } = block
        {
            too_deep |= deep(children, 0);
        }
    });
    if count > CARD_RICH_NODES_MAX || too_deep {
        return Err(ImError::new(
            ErrorKind::LimitExceeded,
            "卡片内容超过读取预算。",
        ));
    }
    Ok(())
}

impl V2<'_> {
    /// 一串 2.0 组件翻成块；认不出的跳过并记下不完整（契约 client v16 §2）。
    fn blocks(&mut self, items: &[Value], depth: usize) -> ImResult<Vec<CardBlock>> {
        let mut blocks = Vec::new();
        for item in items {
            let tag = item.get("tag").and_then(Value::as_str).unwrap_or_default();
            match tag {
                "div" => {
                    // 取样里的 div 只带 text；fields、icon、extra 等还没见过，有就算看不全。
                    if item.get("fields").is_some()
                        || item.get("extra").is_some()
                        || item.get("icon").is_some()
                    {
                        self.incomplete = true;
                    }
                    let text = item.get("text");
                    let content = text
                        .and_then(|t| t.get("content"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    match text.and_then(|t| t.get("tag")).and_then(Value::as_str) {
                        Some("plain_text") => self.push_plain(content, &mut blocks)?,
                        Some("lark_md") => self.push_markdown(content, &mut blocks)?,
                        _ => self.incomplete = true,
                    }
                }
                "markdown" => self.push_markdown(
                    item.get("content")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    &mut blocks,
                )?,
                "hr" => {
                    self.budget.take(0)?;
                    blocks.push(CardBlock::Divider);
                }
                "column_set" if depth < CARD_DEPTH_MAX => {
                    let mut columns = Vec::new();
                    for column in item
                        .get("columns")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        match column.get("tag").and_then(Value::as_str) {
                            Some("column") => columns.push(
                                match column.get("elements").and_then(Value::as_array) {
                                    Some(children) => self.blocks(children, depth + 1)?,
                                    None => Vec::new(),
                                },
                            ),
                            _ => self.incomplete = true,
                        }
                    }
                    if columns.iter().any(|c| !c.is_empty()) {
                        self.budget.take(0)?;
                        blocks.push(CardBlock::Columns { columns });
                    }
                }
                "table" => match self.table(item)? {
                    Some(table) => {
                        self.budget.take(0)?;
                        blocks.push(CardBlock::Rich {
                            children: vec![table],
                        });
                    }
                    None => self.incomplete = true,
                },
                "person" | "person_list" => self.push_people(item, &mut blocks)?,
                "collapsible_panel" if depth < CARD_DEPTH_MAX => {
                    let title = self.panel_title(item.pointer("/header/title"))?;
                    let children = match item.get("elements").and_then(Value::as_array) {
                        Some(children) => self.blocks(children, depth + 1)?,
                        None => Vec::new(),
                    };
                    if !title.is_empty() || !children.is_empty() {
                        self.budget.take(0)?;
                        blocks.push(CardBlock::Panel {
                            title,
                            expanded: item.get("expanded").and_then(Value::as_bool) == Some(true),
                            border: item.get("border").is_some_and(Value::is_object),
                            blocks: children,
                        });
                    }
                }
                "img" | "img_combination" => self.push_images(item, &mut blocks)?,
                // 交互容器与表单：里面能显示的照常放在原位；容器本身的交互（读回时链接已丢）与表单控件
                // 在这里都不能用，标不完整让界面提示去平台里操作。
                "interactive_container" | "form" if depth < CARD_DEPTH_MAX => {
                    self.incomplete = true;
                    if let Some(children) = item.get("elements").and_then(Value::as_array) {
                        blocks.extend(self.blocks(children, depth + 1)?);
                    }
                }
                // 按钮、选择、日期、输入、图表等交互与图表组件不画（契约 im-watch v60 §11 第 1、4 项）。
                _ => self.incomplete = true,
            }
        }
        Ok(blocks)
    }

    /// `div` 的纯文字：不当格式，每个换行都换行。
    fn push_plain(&mut self, content: &str, blocks: &mut Vec<CardBlock>) -> ImResult<()> {
        if content.trim().is_empty() {
            return Ok(());
        }
        self.budget.take(content.len())?;
        blocks.push(CardBlock::Rich {
            children: vec![RichNode::Paragraph {
                children: plain_nodes(content),
            }],
        });
        Ok(())
    }

    fn push_markdown(&mut self, content: &str, blocks: &mut Vec<CardBlock>) -> ImResult<()> {
        if content.trim().is_empty() {
            return Ok(());
        }
        self.budget.take(content.len())?;
        let children = self.markdown(content)?;
        if !children.is_empty() {
            blocks.push(CardBlock::Rich { children });
        }
        Ok(())
    }

    fn markdown(&mut self, content: &str) -> ImResult<Vec<RichNode>> {
        markdown_nodes(content, self.message)
    }

    /// 折叠面板的标题：`plain_text` 当文字，`markdown` 取第一段的行内节点。
    fn panel_title(&mut self, title: Option<&Value>) -> ImResult<Vec<RichNode>> {
        let content = title
            .and_then(|t| t.get("content"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if content.trim().is_empty() {
            return Ok(Vec::new());
        }
        self.budget.take(content.len())?;
        Ok(
            match title.and_then(|t| t.get("tag")).and_then(Value::as_str) {
                Some("markdown" | "lark_md") => self
                    .markdown(content)?
                    .into_iter()
                    .flat_map(|node| match node {
                        RichNode::Paragraph { children } => children,
                        other => vec![other],
                    })
                    .collect(),
                _ => plain_nodes(content),
            },
        )
    }

    fn push_people(&mut self, item: &Value, blocks: &mut Vec<CardBlock>) -> ImResult<()> {
        let ids: Vec<&str> = match item.get("user_id").and_then(Value::as_str) {
            Some(id) => vec![id],
            None => item
                .get("persons")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|p| p.get("id").and_then(Value::as_str))
                .collect(),
        };
        let people: Vec<ActorRef> = ids
            .into_iter()
            .filter(|id| !id.trim().is_empty())
            .map(user)
            .collect();
        if people.is_empty() {
            self.incomplete = true;
            return Ok(());
        }
        self.budget.take(0)?;
        blocks.push(CardBlock::People {
            people,
            size: match item.get("size").and_then(Value::as_str) {
                Some("medium") => PersonSize::Medium,
                Some("large" | "extra_large") => PersonSize::Large,
                _ => PersonSize::Small,
            },
            // 读回时显示与否没写就是飞书的默认：都显示（取样里 person_list 的 show_name 被省掉）。
            avatar: item.get("show_avatar").and_then(Value::as_bool) != Some(false),
            name: item.get("show_name").and_then(Value::as_bool) != Some(false),
        });
        Ok(())
    }

    fn push_images(&mut self, item: &Value, blocks: &mut Vec<CardBlock>) -> ImResult<()> {
        let image = |key: Option<&str>, alt: Option<&str>| {
            let key = key.map(str::trim).filter(|k| !k.is_empty())?;
            Some(CardImage {
                resource: ResourceRef {
                    message: self.message.clone(),
                    key: key.to_owned(),
                    kind: ResourceKind::Image,
                },
                alt: alt
                    .map(str::trim)
                    .filter(|a| !a.is_empty())
                    .map(str::to_owned),
                size: None,
            })
        };
        let images: Vec<CardImage> = match item.get("img_list").and_then(Value::as_array) {
            Some(list) => list
                .iter()
                .filter_map(|i| image(i.get("img_key").and_then(Value::as_str), None))
                .collect(),
            None => image(
                item.get("img_key").and_then(Value::as_str),
                item.pointer("/alt/content").and_then(Value::as_str),
            )
            .into_iter()
            .collect(),
        };
        if images.is_empty() {
            self.incomplete = true;
            return Ok(());
        }
        // 多图混排：取样核实的只有 double（两张一行）；triple 按名字三张一行，其余按图片数一行至多三张。
        let columns = match item.get("combination_mode").and_then(Value::as_str) {
            None => 1,
            Some("double" | "bisect") => 2,
            Some("triple" | "trisect") => 3,
            Some(_) => images.len().clamp(1, 3) as u8,
        };
        self.budget.take(0)?;
        blocks.push(CardBlock::Images { columns, images });
        Ok(())
    }

    /// 表格组件（读回形状，契约 client v16 §4）翻成一张表格：第一行是表头。
    fn table(&mut self, item: &Value) -> ImResult<Option<RichNode>> {
        let Some(columns) = item.get("columns").and_then(Value::as_array) else {
            return Ok(None);
        };
        let rows = item
            .get("rows")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        self.table_rows += rows.len() + 1;
        if columns.len() > CARD_TABLE_COLUMNS_MAX || self.table_rows > CARD_TABLE_ROWS_MAX {
            return Err(ImError::new(
                ErrorKind::LimitExceeded,
                "卡片里的表格超过读取预算。",
            ));
        }
        let cell = |children: Vec<RichNode>| RichNode::TableCell { children };
        let mut out = vec![RichNode::TableRow {
            children: columns
                .iter()
                .map(|c| {
                    let name = c
                        .get("display_name")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    cell(if name.is_empty() {
                        Vec::new()
                    } else {
                        vec![text_node(name)]
                    })
                })
                .collect(),
        }];
        for row in rows {
            let mut cells = Vec::with_capacity(columns.len());
            for column in columns {
                let value = column
                    .get("name")
                    .and_then(Value::as_str)
                    .and_then(|name| row.get(name));
                cells.push(cell(match value {
                    Some(value) if !value.is_null() => self.table_cell(value, column)?,
                    _ => Vec::new(),
                }));
            }
            out.push(RichNode::TableRow { children: cells });
        }
        let align = columns
            .iter()
            .map(
                |c| match c.get("horizontal_align").and_then(Value::as_str) {
                    Some("left") => Some(RichAlign::Left),
                    Some("center") => Some(RichAlign::Center),
                    Some("right") => Some(RichAlign::Right),
                    // 飞书默认：数字列靠右，其余靠左。
                    _ if c.get("data_type").and_then(Value::as_str) == Some("number") => {
                        Some(RichAlign::Right)
                    }
                    _ => None,
                },
            )
            .collect();
        let page_size = item
            .get("page_size")
            .and_then(Value::as_u64)
            .map_or(TABLE_PAGE_SIZE_DEFAULT, |n| n.clamp(1, 10) as u16);
        Ok(Some(RichNode::Table {
            align,
            page_size: Some(page_size),
            children: out,
        }))
    }

    fn table_cell(&mut self, value: &Value, column: &Value) -> ImResult<Vec<RichNode>> {
        let kind = column
            .get("data_type")
            .and_then(Value::as_str)
            .unwrap_or("text");
        Ok(match kind {
            "number" => match value.as_f64() {
                Some(n) => vec![text_node(&format_number(n, column.get("format")))],
                None => vec![text_node(&scalar_text(value))],
            },
            // 选项：读回是 [{text, color}]；没带颜色的是飞书默认的蓝（飞书文档）。
            "options" => items(value)
                .filter_map(|o| {
                    let text = o
                        .get("text")
                        .and_then(Value::as_str)
                        .or_else(|| o.as_str())?;
                    Some(RichNode::Tag {
                        text: text.to_owned(),
                        tone: o
                            .get("color")
                            .and_then(Value::as_str)
                            .and_then(text_tone)
                            .unwrap_or(CardTone::Blue),
                    })
                })
                .collect(),
            "persons" => items(value)
                .filter_map(|p| p.get("id").and_then(Value::as_str).or_else(|| p.as_str()))
                .filter(|id| !id.trim().is_empty())
                .map(|id| RichNode::Person { actor: user(id) })
                .collect(),
            // 日期：读回是 {type: "timestamp", value: "<毫秒>", format}；没规整的是毫秒数。
            "date" => {
                let ms = value
                    .get("value")
                    .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
                    .or_else(|| value.as_i64());
                let format = value
                    .get("format")
                    .or_else(|| column.get("date_format"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                match ms {
                    Some(ms) => vec![RichNode::Time {
                        ms,
                        format: format.to_owned(),
                    }],
                    None => vec![text_node(&scalar_text(value))],
                }
            }
            "lark_md" | "markdown" => match value {
                Value::String(text) => self
                    .markdown(text)?
                    .into_iter()
                    .flat_map(|node| match node {
                        RichNode::Paragraph { children } => children,
                        other => vec![other],
                    })
                    .collect(),
                _ => self.internal_elements(value),
            },
            _ => vec![text_node(&scalar_text(value))],
        })
    }

    /// 表格里 `lark_md` / `markdown` 单元格读回的飞书内部结构：只翻取样见过的 `plain_text`（加粗）与 `link`，
    /// 其它元素留下可得的文字并标不完整。
    fn internal_elements(&mut self, value: &Value) -> Vec<RichNode> {
        let mut out = Vec::new();
        for element in value
            .pointer("/property/elements")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let property = element.get("property");
            let content = property
                .and_then(|p| p.get("content"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            match element.get("tag").and_then(Value::as_str) {
                Some("plain_text") => {
                    let bold = property
                        .and_then(|p| p.pointer("/textStyle/attributes"))
                        .and_then(Value::as_array)
                        .is_some_and(|a| a.iter().any(|x| x.as_str() == Some("bold")));
                    if !content.is_empty() {
                        let node = text_node(content);
                        out.push(if bold {
                            RichNode::Strong {
                                children: vec![node],
                            }
                        } else {
                            node
                        });
                    }
                }
                Some("link") => {
                    let url = property
                        .and_then(|p| p.pointer("/url/url"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    out.push(RichNode::Link {
                        url: url.to_owned(),
                        children: vec![text_node(content)],
                    });
                }
                _ => {
                    self.incomplete = true;
                    if !content.is_empty() {
                        out.push(text_node(content));
                    }
                }
            }
        }
        out
    }
}

fn text_node(value: &str) -> RichNode {
    RichNode::Text {
        value: value.to_owned(),
    }
}

/// 纯文字的行内节点：每个换行都换行。
fn plain_nodes(content: &str) -> Vec<RichNode> {
    let mut out = Vec::new();
    for (i, line) in content.split('\n').enumerate() {
        if i > 0 {
            out.push(RichNode::Break);
        }
        if !line.is_empty() {
            out.push(text_node(line));
        }
    }
    out
}

/// 卡片里的一段 Markdown：先把飞书写法换成占位，再按 GFM 解析（契约 client v16 §3–§4）。卡片 2.0 的 Markdown 与
/// 带飞书 @ 标签的卡片 1.0 `lark_md` 共用。
fn markdown_nodes(content: &str, message: &MessageRef) -> ImResult<Vec<RichNode>> {
    let mut placeholders = Placeholders::default();
    let text = feishu_tags(
        &markdown::strip_placeholder_chars(content),
        &mut placeholders,
    );
    markdown::blocks(
        &text,
        &markdown::Options {
            images: Some(message),
            table_page_size: Some(MARKDOWN_TABLE_PAGE_SIZE),
            emoji_codes: true,
            hr_lines: true,
        },
        &placeholders,
    )
}

/// 这段卡片文字里有飞书的 @ 标签（`<at id=…></at>`、`<at user_id="…">`）。
pub(crate) fn has_feishu_at(text: &str) -> bool {
    text.contains("<at ")
}

/// 卡片里每个提及本人以外某个人的节点（富文本块与折叠面板标题里），可改写。@所有人不算。
pub(crate) fn for_each_person_mention(card: &mut ImCard, f: &mut impl FnMut(&mut ActorRef)) {
    card.walk_blocks_mut(&mut |block| match block {
        CardBlock::Rich { children }
        | CardBlock::Panel {
            title: children, ..
        } => RichNode::walk_all_mut(children, &mut |node| {
            if let RichNode::Mention {
                actor: Some(actor),
                everyone: false,
            } = node
            {
                f(actor);
            }
        }),
        _ => {}
    });
}

/// 卡片里提及的人的编号（@所有人不算），按出现先后。
pub(crate) fn person_mention_ids(card: &ImCard) -> Vec<String> {
    let mut ids = Vec::new();
    card.walk_rich(&mut |node| {
        if let RichNode::Mention {
            actor: Some(actor),
            everyone: false,
        } = node
        {
            ids.push(actor.id.clone());
        }
    });
    ids
}

/// 卡片 1.0 段落里还留着飞书 @ 标签原文（升级前存下的，契约 client v31 §2 第 6 条）。
pub(crate) fn has_raw_at_text(card: &ImCard) -> bool {
    let mut found = false;
    card.walk_blocks(&mut |block| {
        if let CardBlock::Paragraph { parts } = block {
            found |= parts.iter().any(|part| match part {
                CardInline::Markdown { text } => has_feishu_at(text),
            });
        }
    });
    found
}

fn user(id: &str) -> ActorRef {
    ActorRef {
        id: id.trim().to_owned(),
        id_type: "open_id".into(),
        kind: "user".into(),
        name: None,
    }
}

/// 数组逐项，单个值当一项。
fn items(value: &Value) -> Box<dyn Iterator<Item = &Value> + '_> {
    match value {
        Value::Array(list) => Box::new(list.iter()),
        other => Box::new(std::iter::once(other)),
    }
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// 表格数字按列上的格式排成文字：小数位 `precision`（0–10）、千分位 `separator`、货币符号 `symbol`。
fn format_number(n: f64, format: Option<&Value>) -> String {
    let precision = format
        .and_then(|f| f.get("precision"))
        .and_then(Value::as_u64)
        .map(|p| p.min(10) as usize);
    let separator = format
        .and_then(|f| f.get("separator"))
        .and_then(Value::as_bool)
        == Some(true);
    let symbol = format
        .and_then(|f| f.get("symbol"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let body = match precision {
        Some(p) => format!("{:.*}", p, n.abs()),
        None => format!("{}", n.abs()),
    };
    let (int, frac) = body
        .split_once('.')
        .map_or((body.as_str(), None), |(i, f)| (i, Some(f)));
    let int = if separator {
        let digits: Vec<char> = int.chars().collect();
        let mut grouped = String::with_capacity(int.len() + int.len() / 3);
        for (i, d) in digits.iter().enumerate() {
            if i > 0 && (digits.len() - i).is_multiple_of(3) {
                grouped.push(',');
            }
            grouped.push(*d);
        }
        grouped
    } else {
        int.to_owned()
    };
    let sign = if n < 0.0 { "-" } else { "" };
    match frac {
        Some(frac) => format!("{sign}{symbol}{int}.{frac}"),
        None => format!("{sign}{symbol}{int}"),
    }
}

/// 卡片 Markdown 里飞书自己的写法换成占位（契约 client v16 §3）；认不出的标签原样留着，由解析器当文字。
fn feishu_tags(text: &str, placeholders: &mut Placeholders) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let tail = &rest[lt..];
        match feishu_tag(tail, placeholders) {
            Some((replacement, used)) => {
                out.push_str(&replacement);
                rest = &tail[used..];
            }
            None => {
                out.push('<');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// 从 `<` 开始认一个飞书标签，返回换成的占位与用掉的字节数；认不出返回 None。
fn feishu_tag(tail: &str, placeholders: &mut Placeholders) -> Option<(String, usize)> {
    if tail.starts_with("</font>") {
        return Some((Placeholders::close_color(), "</font>".len()));
    }
    let open_end = tail.find('>')?;
    let open = &tail[1..open_end];
    let name_len = open
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(open.len());
    let (name, attrs) = open.split_at(name_len);
    let after_open = open_end + 1;
    // 成对的标签：取到 `</名字>` 为止的正文。
    let paired = || {
        let close = format!("</{name}>");
        let end = tail[after_open..].find(&close)?;
        Some((
            &tail[after_open..after_open + end],
            after_open + end + close.len(),
        ))
    };
    let (node, used) = match name {
        "font" => {
            let tone = attr(attrs, "color").and_then(text_tone);
            return Some((placeholders.open_color(tone), after_open));
        }
        "at" => {
            let (body, used) = paired()?;
            let id = attr(attrs, "id")?;
            if !body.trim().is_empty() {
                return None;
            }
            let node = if id == "all" {
                RichNode::Mention {
                    actor: None,
                    everyone: true,
                }
            } else {
                RichNode::Mention {
                    actor: Some(user(id)),
                    everyone: false,
                }
            };
            (node, used)
        }
        "person" => {
            let (_, used) = paired()?;
            (
                RichNode::Person {
                    actor: user(attr(attrs, "id")?),
                },
                used,
            )
        }
        "local_datetime" => {
            let (_, used) = paired()?;
            (
                RichNode::Time {
                    ms: attr(attrs, "millisecond")?.parse().ok()?,
                    format: attr(attrs, "format_type").unwrap_or_default().to_owned(),
                },
                used,
            )
        }
        "text_tag" | "number_tag" => {
            let (body, used) = paired()?;
            let tone = attr(attrs, "color").and_then(text_tone);
            (
                RichNode::Tag {
                    text: body.trim().to_owned(),
                    tone: tone.unwrap_or(CardTone::Neutral),
                },
                used,
            )
        }
        "link" => {
            let (body, used) = paired()?;
            (
                RichNode::Link {
                    url: attr(attrs, "url")?.to_owned(),
                    children: vec![text_node(body.trim())],
                },
                used,
            )
        }
        _ => return None,
    };
    Some((placeholders.leaf(node), used))
}

/// 标签属性的值：`名字 = '值'`、`名字="值"` 或不带引号的 `名字=值`（等号两边可有空格，取样里都有）。
fn attr<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let mut offset = 0;
    while let Some(found) = attrs[offset..].find(name) {
        let at = offset + found;
        let boundary = attrs[..at]
            .chars()
            .next_back()
            .is_none_or(char::is_whitespace);
        let after = attrs[at + name.len()..].trim_start();
        if boundary && let Some(value) = after.strip_prefix('=') {
            let value = value.trim_start();
            return if let Some(quoted) = value.strip_prefix('\'') {
                quoted.split_once('\'').map(|(v, _)| v)
            } else if let Some(quoted) = value.strip_prefix('"') {
                quoted.split_once('"').map(|(v, _)| v)
            } else {
                Some(&value[..value.find(char::is_whitespace).unwrap_or(value.len())])
            };
        }
        offset = at + name.len();
    }
    None
}

/// 飞书简化过的卡片内容：每行拼成一段；`hr` 单独成一条分割线；别的元素跳过并记下不完整。
fn simplified(body: &Value, budget: &mut Budget) -> ImResult<Translated> {
    // 2.0 卡片的简化内容把副标题用换行并进标题（取样核实）：拆回来。
    let heading = optional(body, "title").unwrap_or_default();
    let (title, subtitle) = heading.split_once('\n').unwrap_or((&heading, ""));
    let [title, subtitle] =
        [title, subtitle].map(|t| Some(t.trim().to_owned()).filter(|t| !t.is_empty()));
    for part in title.iter().chain(&subtitle) {
        budget.take(part.len())?;
    }
    let mut incomplete = false;
    let mut blocks = Vec::new();
    for row in body
        .get("elements")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        // 低版本客户端的升级兜底（一张升级提示图 + 一句升级提示）不是卡片内容，整行不要（契约 im-watch v60）。
        let upgrade_tip = row.as_array().into_iter().flatten().any(|node| {
            node.get("tag").and_then(Value::as_str) == Some("text")
                && node.get("text").and_then(Value::as_str).map(str::trim) == Some(UPGRADE_TIP)
        });
        if upgrade_tip {
            incomplete = true;
            continue;
        }
        let mut text = String::new();
        for node in row.as_array().into_iter().flatten() {
            match node.get("tag").and_then(Value::as_str) {
                Some("text") => {
                    text.push_str(&escape_markdown(
                        node.get("text").and_then(Value::as_str).unwrap_or_default(),
                    ));
                }
                Some("a") => text.push_str(&link(
                    node.get("text").and_then(Value::as_str).unwrap_or_default(),
                    node.get("href").and_then(Value::as_str).unwrap_or_default(),
                )),
                Some("hr") => {
                    if !text.trim().is_empty() {
                        blocks.push(paragraph(std::mem::take(&mut text), budget)?);
                    }
                    text.clear();
                    budget.take(0)?;
                    blocks.push(CardBlock::Divider);
                }
                _ => incomplete = true,
            }
        }
        if !text.trim().is_empty() {
            blocks.push(paragraph(text, budget)?);
        }
    }
    Ok(Translated {
        card: ImCard {
            title,
            subtitle,
            tone: CardTone::Neutral,
            blocks,
            ..ImCard::default()
        },
        incomplete,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn message() -> MessageRef {
        MessageRef {
            conversation: ConversationRef::new("test", "oc_test"),
            id: "om_test".into(),
        }
    }

    fn tr(body: &Value) -> ImResult<Option<Translated>> {
        translate(body, &message())
    }

    fn md(text: &str) -> CardBlock {
        CardBlock::Paragraph {
            parts: vec![CardInline::Markdown { text: text.into() }],
        }
    }

    #[test]
    fn raw_cards_keep_title_tone_columns_dividers_and_markdown() {
        // 真实取样“PR Review 汇总”的结构（数字与链接照取样）。
        let body = json!({
            "config": {},
            "header": {"template": "blue", "title": {"content": "PR Review 汇总", "i18n": {}, "tag": "plain_text"}},
            "elements": [
                {"tag": "div", "text": {"content": "**acme/rocket**  ·  auto", "tag": "lark_md"}},
                {"background_style": "default", "tag": "column_set", "columns": [
                    {"tag": "column", "elements": [{"tag": "div", "text": {"content": "**238**\n当前 PR", "tag": "lark_md"}}]},
                    {"tag": "column", "elements": [{"tag": "div", "text": {"content": "**13**\n本轮有变化", "tag": "lark_md"}}]}
                ]},
                {"tag": "hr"},
                {"tag": "div", "text": {"content": "[#5149 · feat(mobile): 设置页对齐新版样式](https://github.com/acme/rocket/pull/5149)", "tag": "lark_md"}}
            ]
        });
        let translated = tr(&body).unwrap().unwrap();
        assert!(!translated.incomplete);
        assert_eq!(
            translated.card,
            ImCard {
                title: Some("PR Review 汇总".into()),
                tone: CardTone::Blue,
                blocks: vec![
                    md("**acme/rocket**  ·  auto"),
                    CardBlock::Columns {
                        columns: vec![vec![md("**238**\n当前 PR")], vec![md("**13**\n本轮有变化")]]
                    },
                    CardBlock::Divider,
                    md(
                        "[#5149 · feat(mobile): 设置页对齐新版样式](https://github.com/acme/rocket/pull/5149)"
                    ),
                ],
                ..ImCard::default()
            }
        );
    }

    #[test]
    fn raw_card_markdown_with_a_feishu_at_becomes_a_mention_and_the_rest_stays_markdown() {
        // 真实取样（契约 client v31，2026-10-04）：发布机器人的卡片 1.0，div 的 lark_md 里写 `<at id=…></at>`；
        // 编号、链接换成假的。别的 lark_md 段落照旧原样交给界面。
        let body = json!({
            "config": {},
            "header": {"template": "red", "title": {"content": "发布失败", "tag": "plain_text"}},
            "elements": [
                {"tag": "div", "text": {"content": "渠道：**正式版** · 触发人：<at id=ou_card_app_trigger></at> · 流水线：[#333](https://git.example.com/p/1)", "tag": "lark_md"}},
                {"tag": "div", "text": {"content": "**准备环境**：失败", "tag": "lark_md"}}
            ]
        });
        let translated = tr(&body).unwrap().unwrap();
        let [CardBlock::Rich { children }, second] = translated.card.blocks.as_slice() else {
            panic!("{:?}", translated.card.blocks)
        };
        assert_eq!(*second, md("**准备环境**：失败"), "没有 @ 的段落样子不变");
        let [RichNode::Paragraph { children: line }] = children.as_slice() else {
            panic!("{children:?}")
        };
        assert_eq!(
            line.iter()
                .filter(|n| matches!(n, RichNode::Mention { actor: Some(a), everyone: false } if a.id == "ou_card_app_trigger"))
                .count(),
            1,
            "{line:?}"
        );
        assert!(
            line.iter().any(|n| matches!(n, RichNode::Strong { .. })),
            "加粗还在：{line:?}"
        );
        assert!(
            line.iter().any(
                |n| matches!(n, RichNode::Link { url, .. } if url == "https://git.example.com/p/1")
            ),
            "链接还在：{line:?}"
        );
        assert_eq!(
            person_mention_ids(&translated.card),
            ["ou_card_app_trigger"]
        );
        assert!(!has_raw_at_text(&translated.card));
        // 升级前存下的样子：@ 还在 Markdown 段落里。
        let stored = ImCard {
            blocks: vec![md("触发人：<at id=ou_card_app_trigger></at>")],
            ..ImCard::default()
        };
        assert!(has_raw_at_text(&stored));
        assert!(person_mention_ids(&stored).is_empty());
    }

    #[test]
    fn at_all_in_a_raw_card_is_everyone_and_not_a_person() {
        let body = json!({
            "header": {"title": {"content": "通知", "tag": "plain_text"}},
            "elements": [{"tag": "div", "text": {"content": "<at id=all></at> 今晚发版", "tag": "lark_md"}}]
        });
        let translated = tr(&body).unwrap().unwrap();
        let mut everyone = 0;
        translated.card.walk_rich(&mut |n| {
            if matches!(
                n,
                RichNode::Mention {
                    actor: None,
                    everyone: true
                }
            ) {
                everyone += 1;
            }
        });
        assert_eq!(everyone, 1);
        assert!(
            person_mention_ids(&translated.card).is_empty(),
            "@所有人不用对人"
        );
    }

    #[test]
    fn a_card_with_only_a_title_is_valid_and_an_empty_one_is_not() {
        // 真实取样：原始结构只有 header、简化内容 elements 为 null 的卡片。
        let raw = json!({"config": {}, "header": {"template": "blue", "title": {"content": "PR Review 汇总", "tag": "plain_text"}}});
        let simplified = json!({"title": "PR Review 汇总", "elements": null});
        for body in [raw, simplified] {
            let card = tr(&body).unwrap().unwrap().card;
            assert_eq!(card.title.as_deref(), Some("PR Review 汇总"));
            assert!(card.blocks.is_empty());
        }
        for body in [
            json!({}),
            json!({"header": {}}),
            json!({"title": "  ", "elements": []}),
        ] {
            assert!(tr(&body).unwrap().is_none(), "{body}");
        }
    }

    #[test]
    fn unknown_elements_are_skipped_and_mark_the_card_incomplete() {
        let body = json!({
            "header": {"title": {"content": "审批", "tag": "plain_text"}},
            "elements": [
                {"tag": "div", "text": {"content": "请审批", "tag": "lark_md"}},
                {"tag": "action", "actions": [{"tag": "button", "text": {"content": "同意", "tag": "plain_text"}}]},
                {"tag": "img", "img_key": "img_x"},
                {"tag": "div", "fields": [{"is_short": true, "text": {"content": "x", "tag": "lark_md"}}]}
            ]
        });
        let translated = tr(&body).unwrap().unwrap();
        assert!(translated.incomplete);
        assert_eq!(translated.card.blocks, vec![md("请审批")]);
        assert_eq!(translated.card.tone, CardTone::Neutral);
    }

    #[test]
    fn plain_text_is_escaped_so_it_is_never_read_as_markdown() {
        assert_eq!(
            escape_markdown("#1 *a* [b](c) 中文"),
            "\\#1 \\*a\\* \\[b\\]\\(c\\) 中文"
        );
        let body = json!({"header": {"title": {"content": "t", "tag": "plain_text"}},
            "elements": [{"tag": "div", "text": {"content": "# 不是标题", "tag": "plain_text"}}]});
        assert_eq!(
            tr(&body).unwrap().unwrap().card.blocks,
            vec![md("\\# 不是标题")]
        );
    }

    #[test]
    fn simplified_cards_keep_rows_links_and_dividers_and_are_always_incomplete() {
        // 真实取样的简化内容：数字栏被压成同一行的几段文字，链接之间以换行分隔。
        let body = json!({"title": "PR Review 汇总", "elements": [
            [{"tag": "text", "text": "acme/rocket"}, {"tag": "text", "text": "  ·  auto"}],
            [{"tag": "text", "text": "252"}, {"tag": "text", "text": "\n当前 PR"}],
            [{"tag": "hr"}],
            [{"tag": "a", "href": "https://github.com/acme/rocket/pull/4891", "text": "#4891 · fix(mobile)"}, {"tag": "text", "text": "\n"}, {"tag": "a", "href": "http://example.test", "text": "不安全"}],
            [{"tag": "button", "text": "同意"}]
        ]});
        let translated = tr(&body).unwrap().unwrap();
        assert!(translated.incomplete);
        assert_eq!(
            translated.card.blocks,
            vec![
                // 简化内容是纯文字：每个 ASCII 标点都转义，显示时原样。
                md("acme\\/rocket  ·  auto"),
                md("252\n当前 PR"),
                CardBlock::Divider,
                md(
                    "[\\#4891 · fix\\(mobile\\)](<https://github.com/acme/rocket/pull/4891>)\n不安全"
                ),
            ]
        );
    }

    #[test]
    fn unknown_shapes_ask_for_the_simplified_content() {
        for body in [
            json!({"schema": "3.0", "body": {"elements": []}}),
            json!({"schema": "2.0", "body": {"elements": [1]}}),
            json!({"type": "template", "data": {"template_id": "x"}}),
            json!({"elements": [1, 2]}),
            json!("not an object"),
        ] {
            assert!(needs_fallback(&body), "{body}");
            assert!(tr(&body).unwrap().is_none());
        }
        assert!(!needs_fallback(&json!({"title": "t", "elements": [[]]})));
        // 卡片 2.0 按原始结构读，不再退回简化内容（契约 client v16 §1）。
        assert!(!needs_fallback(
            &json!({"schema": "2.0", "body": {"elements": []}})
        ));
        assert!(!needs_fallback(
            &json!({"header": {}, "elements": [{"tag": "hr"}]})
        ));
    }

    #[test]
    fn tones_follow_the_platform_title_color() {
        assert_eq!(tone("blue"), CardTone::Blue);
        assert_eq!(tone("wathet"), CardTone::Blue);
        assert_eq!(tone("carmine"), CardTone::Red);
        assert_eq!(tone("grey"), CardTone::Neutral);
        assert_eq!(tone("future"), CardTone::Neutral);
    }

    fn text(v: &str) -> RichNode {
        RichNode::Text { value: v.into() }
    }

    fn para(children: Vec<RichNode>) -> RichNode {
        RichNode::Paragraph { children }
    }

    /// 示例测试 群测试卡 1 的读回（2026-09-30 取样，人员编号换成 ou_me）：标题栏全字段与 Markdown 的各种写法。
    fn test_card_one() -> Value {
        json!({"body": {"elements": [
            {"element_id": "_2", "tag": "div", "text": {"content": "普通文本 div：**这里不是加粗**", "tag": "plain_text", "text_align": "left"}, "width": "fill"},
            {"element_id": "_4", "tag": "div", "text": {"content": "div 的 lark_md：**加粗** [链接](https://open.feishu.cn)", "tag": "lark_md"}, "width": "fill"},
            {"content": "\n# 一级标题\n\n## 二级标题\n\n### 三级标题\n\n#### 四级标题\n正文 *斜体* ~~删除线~~ `行内代码`\n用 br 换的行> 引用一段\n", "element_id": "_6", "tag": "markdown"},
            {"content": "- 无序 1\n    - 无序 1.1\n- 无序 2\n1. 有序 1\n2. 有序 2\n```JSON\n{\"a\": 1}\n\n```\n", "element_id": "_7", "tag": "markdown"},
            {"content": "彩色：<font color='red'>红</font> <font color='green'>绿</font> <font color='grey'>灰</font> <font color='orange'>**橙色加粗**</font>\n提及：<at id=ou_me></at>\n标签：<text_tag color='violet'>紫标签</text_tag> 角标：<number_tag>7</number_tag>\n表情：:OK::THUMBSUP: 😀\n时间：<local_datetime millisecond='1790726449832' format_type='date_num'></local_datetime>\n超链接：[开放平台](https://open.feishu.cn) 图标链接：<link icon='chat_outlined' url='https://open.feishu.cn' >战略研讨会</link>\n人员：<person id = 'ou_me' show_name = true show_avatar = true style = 'capsule'></person>", "element_id": "_8", "tag": "markdown"},
            {"element_id": "_9", "tag": "hr"},
            {"content": "\n| 指标 | 数值 |\n| -------- | -------- |\n| 第一行 | 1 |\n| 第二行 | 2 |\n| <hr> |  |\n| 分割线下面 |  |\n", "element_id": "_10", "tag": "markdown"}
        ]},
        "config": {"enable_forward_interaction": false, "streaming_mode": false, "width_mode": "fill"},
        "header": {"subtitle": {"content": "副标题：取样用，稍后撤回", "tag": "plain_text"}, "template": "green",
            "text_tag_list": [{"color": "blue", "tag": "text_tag", "text": {"content": "测试", "tag": "plain_text"}}, {"color": "orange", "tag": "text_tag", "text": {"content": "卡片 2.0", "tag": "plain_text"}}],
            "title": {"content": "HHH 测试卡 1：标题栏与文字", "tag": "plain_text"}},
        "schema": "2.0"})
    }

    #[test]
    fn v2_header_keeps_title_subtitle_tags_tone_and_width() {
        let translated = tr(&test_card_one()).unwrap().unwrap();
        let card = &translated.card;
        assert!(!translated.incomplete, "测试卡 1 的写法都认得");
        assert_eq!(card.title.as_deref(), Some("HHH 测试卡 1：标题栏与文字"));
        assert_eq!(card.subtitle.as_deref(), Some("副标题：取样用，稍后撤回"));
        assert_eq!(card.tone, CardTone::Green);
        assert!(card.wide);
        assert_eq!(
            card.tags,
            vec![
                CardTag {
                    text: "测试".into(),
                    tone: CardTone::Blue
                },
                CardTag {
                    text: "卡片 2.0".into(),
                    tone: CardTone::Orange
                },
            ]
        );
    }

    #[test]
    fn v2_markdown_keeps_feishu_tags_as_structure() {
        let card = tr(&test_card_one()).unwrap().unwrap().card;
        let rich = |i: usize| match &card.blocks[i] {
            CardBlock::Rich { children } => children.clone(),
            other => panic!("第 {i} 块不是富文本：{other:?}"),
        };
        // div 的纯文字不当格式；lark_md 按 Markdown。
        assert_eq!(
            rich(0),
            vec![para(vec![text("普通文本 div：**这里不是加粗**")])]
        );
        assert!(
            matches!(&rich(1)[0], RichNode::Paragraph { children } if matches!(&children[1], RichNode::Strong { .. }))
        );
        // 标题一到四级；飞书读回把引用前的换行弄丢了（取样如此），照读回显示。
        let headings: Vec<u8> = rich(2)
            .iter()
            .filter_map(|n| match n {
                RichNode::Heading { depth, .. } => Some(*depth),
                _ => None,
            })
            .collect();
        assert_eq!(headings, [1, 2, 3, 4]);
        // 列表（缩进 4 个空格是嵌套）与代码块（语言名照读回）。
        let lists = rich(3);
        assert!(
            matches!(&lists[0], RichNode::List { ordered: false, children, .. } if matches!(&children[0], RichNode::ListItem { children } if matches!(children.last(), Some(RichNode::List { .. }))))
        );
        assert!(matches!(&lists[1], RichNode::List { ordered: true, .. }));
        assert!(matches!(&lists[2], RichNode::Code { lang: Some(l), .. } if l == "JSON"));
        // 飞书自己的写法：彩色字（灰保留成灰）、@（读回不带名字）、标签、角标、表情码、时间、图标链接、人员。
        let RichNode::Paragraph { children } = &rich(4)[0] else {
            panic!()
        };
        let tones: Vec<CardTone> = children
            .iter()
            .filter_map(|n| match n {
                RichNode::Color { tone, .. } => Some(*tone),
                _ => None,
            })
            .collect();
        assert_eq!(
            tones,
            [
                CardTone::Red,
                CardTone::Green,
                CardTone::Grey,
                CardTone::Orange
            ]
        );
        assert!(children.iter().any(|n| matches!(n, RichNode::Color { tone: CardTone::Orange, children } if matches!(&children[0], RichNode::Strong { .. }))));
        assert!(children.contains(&RichNode::Mention {
            actor: Some(user("ou_me")),
            everyone: false
        }));
        assert!(children.contains(&RichNode::Tag {
            text: "紫标签".into(),
            tone: CardTone::Purple
        }));
        assert!(children.contains(&RichNode::Tag {
            text: "7".into(),
            tone: CardTone::Neutral
        }));
        assert!(children.contains(&RichNode::Emoji {
            emoji_type: "OK".into()
        }));
        assert!(children.contains(&RichNode::Emoji {
            emoji_type: "THUMBSUP".into()
        }));
        assert!(children.contains(&RichNode::Time {
            ms: 1_790_726_449_832,
            format: "date_num".into()
        }));
        assert!(children.contains(&RichNode::Link {
            url: "https://open.feishu.cn".into(),
            children: vec![text("战略研讨会")]
        }));
        assert!(children.contains(&RichNode::Person {
            actor: user("ou_me")
        }));
        assert!(
            !children
                .iter()
                .any(|n| matches!(n, RichNode::Text { value } if value.contains('<'))),
            "认得的标签都换成了结构：{children:?}"
        );
        // hr 组件是分割线；表格照飞书客户端每页 10 行（client v19 §1；读回里表格吞进去的 <hr> 照读回留在格子里）。
        assert_eq!(card.blocks[5], CardBlock::Divider);
        assert!(
            matches!(&rich(6)[0], RichNode::Table { page_size: Some(10), children, .. } if children.len() == 5)
        );
    }

    #[test]
    fn v2_simplified_content_never_shows_the_upgrade_tip() {
        let body = json!({"title": "日报\n日期：2026-09-29", "elements": [[
            {"tag": "img", "image_key": "img_v3_upgrade"},
            {"tag": "text", "text": "请升级至最新版本客户端，以查看内容"},
            {"tag": "text", "text": ""}
        ]]});
        let translated = tr(&body).unwrap().unwrap();
        assert!(translated.incomplete);
        assert_eq!(translated.card.title.as_deref(), Some("日报"));
        assert_eq!(
            translated.card.subtitle.as_deref(),
            Some("日期：2026-09-29")
        );
        assert!(translated.card.blocks.is_empty());
    }

    #[test]
    fn text_tones_keep_grey_and_drop_unknown_colors() {
        assert_eq!(text_tone("grey"), Some(CardTone::Grey));
        assert_eq!(text_tone("neutral"), Some(CardTone::Grey));
        assert_eq!(text_tone("lime"), Some(CardTone::Green));
        assert_eq!(text_tone("violet"), Some(CardTone::Purple));
        for unknown in ["default", "rgba(1,2,3,0.5)", "", "future"] {
            assert_eq!(text_tone(unknown), None, "{unknown}");
        }
        // 认不出颜色的彩色字只留文字。
        let mut p = Placeholders::default();
        assert_eq!(
            markdown::blocks(
                &feishu_tags("<font color='rgba(1,2,3,1)'>字</font>", &mut p),
                &markdown::Options::default(),
                &p
            )
            .unwrap(),
            vec![para(vec![text("字")])]
        );
    }

    #[test]
    fn table_numbers_follow_the_column_format() {
        let f = |v: Value| Some(v);
        assert_eq!(
            format_number(
                12345.7,
                f(json!({"precision": 1, "separator": true, "symbol": "$"})).as_ref()
            ),
            "$12,345.7"
        );
        assert_eq!(
            format_number(800.0, f(json!({"precision": 1, "symbol": "$"})).as_ref()),
            "$800.0"
        );
        assert_eq!(
            format_number(1234567.0, f(json!({"separator": true})).as_ref()),
            "1,234,567"
        );
        assert_eq!(
            format_number(-5.25, f(json!({"precision": 1, "symbol": "¥"})).as_ref()),
            "-¥5.2"
        );
        assert_eq!(format_number(0.5, None), "0.5");
    }

    #[test]
    fn feishu_tag_attributes_accept_quotes_and_spaces() {
        assert_eq!(attr(" id = 'ou_me' show_name = true", "id"), Some("ou_me"));
        assert_eq!(attr(" id=ou_x", "id"), Some("ou_x"));
        assert_eq!(attr(" user_id=\"ou_y\"", "id"), None, "user_id 不是 id");
        assert_eq!(
            attr(" millisecond='1' format_type='date_num'", "format_type"),
            Some("date_num")
        );
        // 认不出的标签、没闭合的成对标签原样留给解析器（当文字）。
        let mut p = Placeholders::default();
        assert_eq!(
            feishu_tags("<audio file_key='x'></audio> <text_tag>没闭合", &mut p),
            "<audio file_key='x'></audio> <text_tag>没闭合"
        );
    }

    #[test]
    fn v2_unknown_components_are_skipped_and_containers_are_bounded() {
        let body = json!({"schema": "2.0", "header": {"title": {"content": "t", "tag": "plain_text"}}, "body": {"elements": [
            {"tag": "button", "text": {"content": "同意", "tag": "plain_text"}},
            {"tag": "chart", "chart_spec": {}},
            {"tag": "markdown", "content": "留下的字"}
        ]}});
        let translated = tr(&body).unwrap().unwrap();
        assert!(translated.incomplete);
        assert_eq!(
            translated.card.blocks,
            vec![CardBlock::Rich {
                children: vec![para(vec![text("留下的字")])]
            }]
        );
        // 折叠面板套得太深：最里层不读，标不完整。
        let mut nested = json!({"tag": "markdown", "content": "深处"});
        for _ in 0..=CARD_DEPTH_MAX {
            nested = json!({"tag": "collapsible_panel", "header": {"title": {"content": "p", "tag": "plain_text"}}, "elements": [nested]});
        }
        let deep = tr(&json!({"schema": "2.0", "body": {"elements": [nested]}}))
            .unwrap()
            .unwrap();
        assert!(deep.incomplete);
        // 表格列太多、行太多按读取预算报错。
        let columns: Vec<Value> = (0..=CARD_TABLE_COLUMNS_MAX)
            .map(|i| json!({"name": format!("c{i}"), "data_type": "text"}))
            .collect();
        let wide = json!({"schema": "2.0", "body": {"elements": [{"tag": "table", "columns": columns, "rows": []}]}});
        assert_eq!(
            tr(&wide).err().map(|e| e.kind),
            Some(ErrorKind::LimitExceeded)
        );
        let rows: Vec<Value> = (0..=CARD_TABLE_ROWS_MAX)
            .map(|i| json!({"a": i.to_string()}))
            .collect();
        let long = json!({"schema": "2.0", "body": {"elements": [{"tag": "table", "columns": [{"name": "a"}], "rows": rows}]}});
        assert_eq!(
            tr(&long).err().map(|e| e.kind),
            Some(ErrorKind::LimitExceeded)
        );
        // 格式正文节点太多也按读取预算报错。
        let many = "a\n".repeat(CARD_RICH_NODES_MAX);
        let heavy =
            json!({"schema": "2.0", "body": {"elements": [{"tag": "markdown", "content": many}]}});
        assert_eq!(
            tr(&heavy).err().map(|e| e.kind),
            Some(ErrorKind::LimitExceeded)
        );
    }

    #[test]
    fn card_budgets_and_nesting_are_bounded() {
        let many: Vec<Value> = (0..=CARD_BLOCKS_MAX)
            .map(|_| json!({"tag": "hr"}))
            .collect();
        let body = json!({"header": {}, "elements": many});
        assert_eq!(
            tr(&body).err().map(|e| e.kind),
            Some(ErrorKind::LimitExceeded)
        );
        let long = "x".repeat(CARD_TEXT_MAX_BYTES + 1);
        let body = json!({"header": {}, "elements": [{"tag": "div", "text": {"content": long, "tag": "lark_md"}}]});
        assert_eq!(
            tr(&body).err().map(|e| e.kind),
            Some(ErrorKind::LimitExceeded)
        );
        // 多栏套得太深：最里层不读，标不完整。
        let mut nested = json!({"tag": "div", "text": {"content": "深处", "tag": "lark_md"}});
        for _ in 0..=CARD_DEPTH_MAX {
            nested =
                json!({"tag": "column_set", "columns": [{"tag": "column", "elements": [nested]}]});
        }
        let translated = tr(&json!({"header": {"title": {"content": "t"}}, "elements": [nested]}))
            .unwrap()
            .unwrap();
        assert!(translated.incomplete);
    }
}
