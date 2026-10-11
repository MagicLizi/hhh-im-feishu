//! 普通消息的格式正文（契约 client v6，Friend im-watch v51）：飞书纯文字与富文本整理成
//! 平台无关的 mdast 子集（[`ImRich`]）。
//!
//! 三件事都在这里：
//! - 纯文字里的网址：用 `linkify` 找，关掉 IRI（遇到中文就停），不带协议的只认 `www.`；
//!   `[文字](地址)` 是飞书纯文字自己的链接写法，库不认，手写。与飞书客户端的识别一致（真实取样 3）。
//! - 富文本（`post`）逐节点翻译：样式、链接、@、图片、表情、分割线、代码块、空行，
//!   以及“行首单独成节点的编号或圆点”合成的嵌套列表（真实取样 1、2、7、8、11）。
//! - 平台明确标为 Markdown 的内容（`content_v2` 里的 `md` 节点，机器人与集成常用）：
//!   交给 `markdown` 按 GFM 解析成 mdast，再转成本库自己的节点；飞书的 `<at>` 写法转成提及。
//!
//! 飞书特有的规则（列表标记、接口发的代码块换行翻倍）只能手写；
//! 各家（含飞书官方 SDK）都是逐标签手写映射（调研 `agent-learn/compare/im-rich-text-normalization.md`）。

use super::markdown;
use super::optional;
use super::translate::{post_mention, resource, unsupported};
use crate::*;
use linkify::{LinkFinder, LinkKind};
use serde_json::Value;

/// 格式正文的节点预算，与消息内容块预算一致，避免单条消息产生无界小对象。
const RICH_NODES_MAX: usize = 2048;
/// 语法树的嵌套深度上限（列表、引用、样式互相嵌套），防止恶意构造的深树撑爆栈。
const RICH_DEPTH_MAX: usize = 24;

/// 纯文字消息拆出的块（文字、提及）里有网址时，整条改成一段格式正文；没有网址时原样返回。
/// 文字里的换行成为行内换行。图片、文件、暂不支持的块也能放进段落；卡片或已经是格式正文的不动。
pub(crate) fn linkify_blocks(blocks: Vec<ImContent>) -> Vec<ImContent> {
    let has_link = blocks
        .iter()
        .any(|b| matches!(b, ImContent::Text { text } if contains_link(text)));
    let embeddable = blocks.iter().all(|b| {
        matches!(
            b,
            ImContent::Text { .. }
                | ImContent::Mention { .. }
                | ImContent::Resource { video: None, .. }
                | ImContent::Unsupported { .. }
        )
    });
    if !has_link || !embeddable {
        return blocks;
    }
    let mut children = Vec::new();
    for block in blocks {
        match block {
            ImContent::Text { text } => children.extend(text_nodes(&text)),
            other => children.push(inline_of(other)),
        }
    }
    vec![ImContent::Rich(ImRich::new(vec![RichNode::Paragraph {
        children,
    }]))]
}

/// 本机一次性修正（契约 client v6 §5）：升级前存下的消息里，文字块中的网址变成链接。
/// 不需要改时返回 None。
pub(crate) fn repair_links(content: &[ImContent]) -> Option<Vec<ImContent>> {
    let changed = linkify_blocks(content.to_vec());
    (changed != content).then_some(changed)
}

/// 升级前存下的富文本里，表情、分割线、代码块只记成了暂不支持：出现在视野里时重读一次。
/// 其中带网址的，本机修正（[`repair_links`]）先把整条包成了格式正文，暂不支持的节点落在树里，同样要算。
pub(crate) fn is_unsupported_rich_post(message: &ImMessage) -> bool {
    let rereadable = |kind: &str| matches!(kind, "emotion" | "hr" | "code_block");
    !message.deleted
        && message.content.iter().any(|c| match c {
            ImContent::Unsupported { kind, .. } => rereadable(kind),
            ImContent::Rich(rich) => {
                let mut found = false;
                rich.walk(&mut |node| {
                    found |= matches!(node, RichNode::Unsupported { kind, .. } if rereadable(kind));
                });
                found
            }
            _ => false,
        })
}

/// 富文本要不要整理成格式正文：有标题、样式、链接、表情、分割线、代码块、`md`，
/// 有列表行，或文字里有网址时要；只有文字、@、图片的照旧（翻译结果与升级前相同）。
pub(crate) fn post_needs_rich(title: Option<&str>, rows: &[Value]) -> bool {
    if title.is_some_and(|t| !t.is_empty()) {
        return true;
    }
    rows.iter().filter_map(Value::as_array).any(|row| {
        list_marker(row).is_some()
            || row.iter().any(|node| {
                match node.get("tag").and_then(Value::as_str).unwrap_or_default() {
                    "text" => {
                        has_style(node)
                            || node
                                .get("text")
                                .and_then(Value::as_str)
                                .is_some_and(contains_link)
                    }
                    "a" | "emotion" | "hr" | "code_block" | "md" => true,
                    _ => false,
                }
            })
    })
}

/// 飞书富文本的行（`content` 或 `content_v2`）整理成格式正文。标题作为第一行、加粗。
pub(crate) fn post_rich(
    reference: &MessageRef,
    title: Option<&str>,
    rows: &[Value],
    mentions: Option<&Vec<Value>>,
) -> ImResult<ImRich> {
    let mut blocks = Vec::new();
    if let Some(title) = title.filter(|t| !t.is_empty()) {
        blocks.push(RichNode::Paragraph {
            children: vec![RichNode::Strong {
                children: vec![RichNode::Text {
                    value: title.into(),
                }],
            }],
        });
    }
    let mut list = ListBuilder::default();
    for row in rows {
        let nodes = row
            .as_array()
            .ok_or_else(|| ImError::new(ErrorKind::ProtocolError, "富文本行格式无效。"))?;
        if nodes.is_empty() {
            list.flush(&mut blocks);
            blocks.push(RichNode::Paragraph {
                children: Vec::new(),
            });
            continue;
        }
        if let Some(marker) = list_marker(nodes) {
            let children = inline_nodes(reference, &nodes[1..], mentions)?;
            list.push(marker, children);
            continue;
        }
        list.flush(&mut blocks);
        let mut line = Vec::new();
        for node in nodes {
            match node.get("tag").and_then(Value::as_str).unwrap_or_default() {
                "hr" => {
                    flush_line(&mut line, &mut blocks);
                    blocks.push(RichNode::ThematicBreak);
                }
                "code_block" => {
                    flush_line(&mut line, &mut blocks);
                    blocks.push(RichNode::Code {
                        // 飞书用 PLAIN_TEXT 表示“没有语言”（取样 7），不当语言名。
                        lang: optional(node, "language")
                            .filter(|l| !l.is_empty() && !l.eq_ignore_ascii_case("plain_text"))
                            .map(|l| l.to_lowercase()),
                        value: restore_code(
                            node.get("text").and_then(Value::as_str).unwrap_or_default(),
                        ),
                    });
                }
                "md" => {
                    flush_line(&mut line, &mut blocks);
                    blocks.extend(markdown_blocks(
                        node.get("text").and_then(Value::as_str).unwrap_or_default(),
                    )?);
                }
                _ => line.extend(inline_nodes(
                    reference,
                    std::slice::from_ref(node),
                    mentions,
                )?),
            }
        }
        flush_line(&mut line, &mut blocks);
    }
    list.flush(&mut blocks);
    let rich = ImRich::new(blocks);
    check_budget(&rich)?;
    Ok(rich)
}

fn flush_line(line: &mut Vec<RichNode>, blocks: &mut Vec<RichNode>) {
    if !line.is_empty() {
        blocks.push(RichNode::Paragraph {
            children: std::mem::take(line),
        });
    }
}

fn check_budget(rich: &ImRich) -> ImResult<()> {
    if rich.node_count() > RICH_NODES_MAX {
        return Err(ImError::new(
            ErrorKind::LimitExceeded,
            "富文本消息超过内容块预算。",
        ));
    }
    fn depth(nodes: &[RichNode], level: usize) -> ImResult<()> {
        if level > RICH_DEPTH_MAX {
            return Err(ImError::new(
                ErrorKind::LimitExceeded,
                "富文本消息的嵌套超过读取预算。",
            ));
        }
        for node in nodes {
            depth(node.children(), level + 1)?;
        }
        Ok(())
    }
    depth(&rich.root.children, 0)
}

/// 富文本一行里的行内节点。
fn inline_nodes(
    reference: &MessageRef,
    nodes: &[Value],
    mentions: Option<&Vec<Value>>,
) -> ImResult<Vec<RichNode>> {
    let mut out = Vec::new();
    for node in nodes {
        let tag = node
            .get("tag")
            .and_then(Value::as_str)
            .ok_or_else(|| ImError::new(ErrorKind::ProtocolError, "富文本节点缺少类型。"))?;
        match tag {
            "text" => {
                // 富文本编辑器会保留空文字节点；空字符串与字段缺失不同（沿用原有规则）。
                let text = node.get("text").and_then(Value::as_str).ok_or_else(|| {
                    ImError::new(ErrorKind::ProtocolError, "富文本节点缺少文字字段。")
                })?;
                if !text.is_empty() {
                    out.extend(styled(node, text_nodes(text)));
                }
            }
            "a" => {
                let href = super::required(node, "href")?;
                let text = optional(node, "text")
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| href.clone());
                out.extend(styled(
                    node,
                    vec![RichNode::Link {
                        url: href,
                        children: vec![RichNode::Text { value: text }],
                    }],
                ));
            }
            "at" => out.push(inline_of(post_mention(node, mentions)?)),
            "img" => out.push(inline_of(resource(reference, node, ResourceKind::Image)?)),
            "emotion" => out.push(
                match optional(node, "emoji_type").filter(|t| !t.is_empty()) {
                    Some(emoji_type) => RichNode::Emoji { emoji_type },
                    None => inline_of(unsupported("emotion")),
                },
            ),
            other => out.push(inline_of(unsupported(other))),
        }
    }
    Ok(out)
}

/// 统一内容块放进格式正文：文字、提及、资源、暂不支持各有对应节点。
fn inline_of(block: ImContent) -> RichNode {
    match block {
        ImContent::Text { text } => RichNode::Text { value: text },
        ImContent::Mention { actor, everyone } => RichNode::Mention { actor, everyone },
        ImContent::Resource {
            resource,
            name,
            size,
            ..
        } => RichNode::Resource {
            resource,
            name,
            size,
        },
        ImContent::Unsupported { kind, label } => RichNode::Unsupported { kind, label },
        // 卡片与格式正文不会出现在一行里；万一出现按暂不支持显示，不丢。
        ImContent::Card(_) | ImContent::Rich(_) => RichNode::Unsupported {
            kind: "nested".into(),
            label: Some("消息".into()),
        },
    }
}

fn has_style(node: &Value) -> bool {
    node.get("style")
        .and_then(Value::as_array)
        .is_some_and(|s| !s.is_empty())
}

/// 把一组行内节点包进一种样式节点。
type Wrap = fn(Vec<RichNode>) -> RichNode;

/// 按飞书的 `style` 数组给节点包上样式：加粗在最外，其次斜体、下划线、删除线。
/// 认不出的样式忽略；没有样式时原样返回这几个节点。
fn styled(node: &Value, children: Vec<RichNode>) -> Vec<RichNode> {
    let styles: Vec<&str> = node
        .get("style")
        .and_then(Value::as_array)
        .map(|s| s.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let wraps: [(&str, Wrap); 4] = [
        ("lineThrough", |children| RichNode::Delete { children }),
        ("underline", |children| RichNode::Underline { children }),
        ("italic", |children| RichNode::Emphasis { children }),
        ("bold", |children| RichNode::Strong { children }),
    ];
    let mut wrapped = children;
    for (style, wrap) in wraps {
        if styles.contains(&style) {
            wrapped = vec![wrap(wrapped)];
        }
    }
    wrapped
}

/// 一行文字拆成文字、链接与行内换行。
fn text_nodes(text: &str) -> Vec<RichNode> {
    let mut out = Vec::new();
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            out.push(RichNode::Break);
        }
        push_links(line, &mut out);
    }
    out
}

/// 一行里有没有链接（网址或 `[文字](地址)`）。
fn contains_link(text: &str) -> bool {
    let mut probe = Vec::new();
    push_links(text, &mut probe);
    probe.iter().any(|n| matches!(n, RichNode::Link { .. }))
}

/// 飞书纯文字识别网址的规则（真实取样 3，与飞书客户端显示一致）。
fn finder() -> LinkFinder {
    let mut finder = LinkFinder::new();
    finder
        .kinds(&[LinkKind::Url])
        .url_must_have_scheme(false)
        .url_can_be_iri(false);
    finder
}

/// 不带协议的只认 `www.` 开头：`README.md`、`v1.2` 这类不当网址（飞书取样只见 `www.`）。
fn accepted(url: &str) -> bool {
    url.contains("://") || url.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("www."))
}

fn push_text(value: &str, out: &mut Vec<RichNode>) {
    if value.is_empty() {
        return;
    }
    if let Some(RichNode::Text { value: last }) = out.last_mut() {
        last.push_str(value);
    } else {
        out.push(RichNode::Text {
            value: value.into(),
        });
    }
}

/// 一行文字：先认 `[文字](地址)`，其余部分再找裸网址。
fn push_links(line: &str, out: &mut Vec<RichNode>) {
    let mut rest = line;
    while let Some((before, text, url, after)) = markdown_link(rest) {
        push_bare_links(before, out);
        out.push(RichNode::Link {
            url: url.into(),
            children: vec![RichNode::Text { value: text.into() }],
        });
        rest = after;
    }
    push_bare_links(rest, out);
}

/// 找第一个 `[文字](地址)`：文字非空、不含 `]`，地址整段是一个能被识别的网址。
fn markdown_link(text: &str) -> Option<(&str, &str, &str, &str)> {
    let mut from = 0;
    while let Some(offset) = text[from..].find("](") {
        let mid = from + offset;
        let open = text[..mid].rfind('[');
        let close = text[mid + 2..].find(')').map(|c| mid + 2 + c);
        if let (Some(open), Some(close)) = (open, close) {
            let label = &text[open + 1..mid];
            let url = &text[mid + 2..close];
            let whole = finder()
                .links(url)
                .next()
                .is_some_and(|l| l.start() == 0 && l.end() == url.len() && accepted(url));
            if !label.is_empty() && !label.contains(']') && whole {
                return Some((&text[..open], label, url, &text[close + 1..]));
            }
        }
        from = mid + 2;
    }
    None
}

fn push_bare_links(text: &str, out: &mut Vec<RichNode>) {
    let mut last = 0;
    for link in finder().links(text) {
        if !accepted(link.as_str()) {
            continue;
        }
        push_text(&text[last..link.start()], out);
        out.push(RichNode::Link {
            url: link.as_str().into(),
            children: vec![RichNode::Text {
                value: link.as_str().into(),
            }],
        });
        last = link.end();
    }
    push_text(&text[last..], out);
}

/// 行首的列表标记：这一行第一个节点是没有样式的文字，整个节点只有标记本身
/// （“- ”“1. ”“a. ”，前面每层两个空格）。夹在文字中间或和正文连在同一节点里的不算（取样 1）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Marker {
    depth: usize,
    ordered: bool,
    number: u32,
}

fn list_marker(row: &[Value]) -> Option<Marker> {
    let first = row.first()?;
    if first.get("tag").and_then(Value::as_str) != Some("text") || has_style(first) {
        return None;
    }
    let text = first.get("text").and_then(Value::as_str)?;
    let body = text.strip_suffix(' ')?;
    let indent = body.len() - body.trim_start_matches(' ').len();
    if indent % 2 != 0 {
        return None;
    }
    let mark = &body[indent..];
    let depth = indent / 2 + 1;
    if mark == "-" {
        return Some(Marker {
            depth,
            ordered: false,
            number: 1,
        });
    }
    let label = mark.strip_suffix('.')?;
    let number = if !label.is_empty()
        && label.len() <= 9
        && label.bytes().all(|b| b.is_ascii_digit())
    {
        label.parse().ok()?
    } else if !label.is_empty() && label.len() <= 3 && label.bytes().all(|b| b.is_ascii_lowercase())
    {
        label
            .bytes()
            .fold(0u32, |n, b| n * 26 + u32::from(b - b'a' + 1))
    } else {
        return None;
    };
    Some(Marker {
        depth,
        ordered: true,
        number,
    })
}

/// 连续的列表行合成嵌套列表：同层同类接着放，层级变深就挂到上一项里，
/// 同层换了有序 / 无序就另起一个列表（取样 8：无序列表后接有序列表）。
#[derive(Default)]
struct ListBuilder {
    /// 从外到内正在搭的各层列表。
    open: Vec<OpenList>,
    /// 已经关掉的最外层列表，按顺序等 `flush` 放进正文。
    done: Vec<RichNode>,
}

/// 正在搭的一层列表。
struct OpenList {
    depth: usize,
    ordered: bool,
    start: u32,
    items: Vec<RichNode>,
}

impl ListBuilder {
    fn push(&mut self, marker: Marker, inline: Vec<RichNode>) {
        while let Some(top) = self.open.last() {
            if top.depth > marker.depth
                || (top.depth == marker.depth && top.ordered != marker.ordered)
            {
                if let Some(list) = self.close_top() {
                    self.done.push(list);
                }
            } else {
                break;
            }
        }
        let item = RichNode::ListItem {
            children: vec![RichNode::Paragraph { children: inline }],
        };
        match self.open.last_mut() {
            Some(top) if top.depth == marker.depth => top.items.push(item),
            _ => self.open.push(OpenList {
                depth: marker.depth,
                ordered: marker.ordered,
                start: marker.number,
                items: vec![item],
            }),
        }
    }

    /// 关掉最里层的列表：挂到外一层最后一项里；没有外层时留给 `flush` 取走。
    fn close_top(&mut self) -> Option<RichNode> {
        let top = self.open.pop()?;
        let list = RichNode::List {
            ordered: top.ordered,
            start: (top.ordered && top.start != 1).then_some(top.start),
            children: top.items,
        };
        match self
            .open
            .last_mut()
            .and_then(|outer| outer.items.last_mut())
        {
            Some(RichNode::ListItem { children }) => {
                children.push(list);
                None
            }
            _ => Some(list),
        }
    }

    fn flush(&mut self, blocks: &mut Vec<RichNode>) {
        while !self.open.is_empty() {
            if let Some(list) = self.close_top() {
                self.done.push(list);
            }
        }
        blocks.append(&mut self.done);
    }
}

/// 代码块原文的换行（取样 1、2、7）：接口发的代码块每个换行都被飞书翻倍、末尾多两个换行；
/// 编辑器写的正常、只以一个换行结尾。所有换行都成对出现并且以两个换行结尾时，去掉多出的那两个、
/// 其余每对还原成一个；否则原样。最后去掉结尾的一个换行：那是最后一行的结束（mdast 的代码原文末尾不带换行）。
pub(crate) fn restore_code(text: &str) -> String {
    let runs_even = text
        .split(|c| c != '\n')
        .filter(|run| !run.is_empty())
        .all(|run| run.len() % 2 == 0);
    let mut value = if text.contains('\n') && runs_even && text.ends_with("\n\n") {
        let doubled = &text[..text.len() - 2];
        let mut out = String::with_capacity(doubled.len());
        let mut pending = 0usize;
        for c in doubled.chars() {
            if c == '\n' {
                pending += 1;
                if pending == 2 {
                    out.push('\n');
                    pending = 0;
                }
            } else {
                out.push(c);
            }
        }
        out
    } else {
        text.to_owned()
    };
    if value.ends_with('\n') {
        value.pop();
    }
    value
}

/// 平台明确标为 Markdown 的原文（`md` 节点）解析成块：规则见 `feishu/markdown.rs`（富文本用默认规则：
/// Markdown 图片按暂不支持、表格不分页、表情码不转；契约 client v6 §3）。
fn markdown_blocks(text: &str) -> ImResult<Vec<RichNode>> {
    markdown::blocks(
        text,
        &markdown::Options::default(),
        &markdown::Placeholders::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reference() -> MessageRef {
        MessageRef {
            conversation: ConversationRef::new("test", "oc_test"),
            id: "om_test".into(),
        }
    }
    fn t(value: &str) -> RichNode {
        RichNode::Text {
            value: value.into(),
        }
    }
    fn link(url: &str, text: &str) -> RichNode {
        RichNode::Link {
            url: url.into(),
            children: vec![t(text)],
        }
    }
    fn p(children: Vec<RichNode>) -> RichNode {
        RichNode::Paragraph { children }
    }
    fn item(children: Vec<RichNode>) -> RichNode {
        RichNode::ListItem { children }
    }

    #[test]
    fn plain_text_links_break_where_feishu_breaks_them() {
        // 真实取样 3 原文（2026-09-28，示例测试）：飞书客户端在“。”“）”前截断，识别不带协议的 www.，
        // [飞书](地址) 显示成“飞书”，表情码原样。
        let text = "裸网址 https://x.com/ExampleDevs/status/1000000000000000001?s=20 紧跟中文句号https://www.feishu.cn。括号（https://open.feishu.cn）不带协议 www.feishu.cn 链接写法 [飞书](https://www.feishu.cn) 表情码[看][微笑] http 的 http://example.com 带端口 https://example.com:8443/a_b-c?x=1&y=2#frag";
        let nodes = text_nodes(text);
        let links: Vec<_> = nodes
            .iter()
            .filter_map(|n| match n {
                RichNode::Link { url, children } => Some((url.as_str(), children.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            links.iter().map(|(u, _)| *u).collect::<Vec<_>>(),
            vec![
                "https://x.com/ExampleDevs/status/1000000000000000001?s=20",
                "https://www.feishu.cn",
                "https://open.feishu.cn",
                "www.feishu.cn",
                "https://www.feishu.cn",
                "http://example.com",
                "https://example.com:8443/a_b-c?x=1&y=2#frag",
            ]
        );
        assert_eq!(links[4].1, vec![t("飞书")]);
        assert!(nodes.contains(&t("。括号（")));
        assert!(
            nodes.iter().any(
                |n| matches!(n, RichNode::Text { value } if value.contains("表情码[看][微笑]"))
            )
        );
    }

    #[test]
    fn trailing_punctuation_bare_domains_and_newlines() {
        assert_eq!(
            text_nodes("见 https://example.com/a. 下一行\n(https://example.com/b)"),
            vec![
                t("见 "),
                link("https://example.com/a", "https://example.com/a"),
                t(". 下一行"),
                RichNode::Break,
                t("("),
                link("https://example.com/b", "https://example.com/b"),
                t(")"),
            ]
        );
        // 不带协议又不是 www. 开头的不当网址。
        assert_eq!(
            text_nodes("看 README.md 与 v1.2.3"),
            vec![t("看 README.md 与 v1.2.3")]
        );
        assert!(!contains_link("[看][微笑]"));
        assert!(!contains_link("[文字](不是网址)"));
    }

    #[test]
    fn text_messages_without_links_stay_as_they_were() {
        let blocks = vec![
            ImContent::Mention {
                actor: None,
                everyone: true,
            },
            ImContent::Text {
                text: " 今晚发版".into(),
            },
        ];
        assert_eq!(linkify_blocks(blocks.clone()), blocks);
        assert_eq!(repair_links(&blocks), None);
        let with_link = vec![ImContent::Text {
            text: "看 https://open.feishu.cn".into(),
        }];
        let repaired = repair_links(&with_link).unwrap();
        assert!(matches!(&repaired[..], [ImContent::Rich(_)]));
        // 已经修正过的再修正一次不再变化。
        assert_eq!(repair_links(&repaired), None);
    }

    #[test]
    fn list_markers_must_be_their_own_leading_node() {
        let row = |nodes: Value| nodes.as_array().unwrap().clone();
        assert_eq!(
            list_marker(&row(
                json!([{"tag":"text","text":"1. ","style":[]},{"tag":"text","text":"一"}])
            )),
            Some(Marker {
                depth: 1,
                ordered: true,
                number: 1
            })
        );
        assert_eq!(
            list_marker(&row(
                json!([{"tag":"text","text":"  a. ","style":[]},{"tag":"text","text":"一"}])
            )),
            Some(Marker {
                depth: 2,
                ordered: true,
                number: 1
            })
        );
        assert_eq!(
            list_marker(&row(
                json!([{"tag":"text","text":"  - ","style":[]},{"tag":"text","text":"一"}])
            )),
            Some(Marker {
                depth: 2,
                ordered: false,
                number: 1
            })
        );
        // 取样 1：夹在一段文字里的“1.”不算；带样式的不算；奇数空格不算。
        assert_eq!(
            list_marker(&row(
                json!([{"tag":"text","text":"手打符号 1. 不是列表","style":[]}])
            )),
            None
        );
        assert_eq!(
            list_marker(&row(json!([{"tag":"text","text":"1. ","style":["bold"]}]))),
            None
        );
        assert_eq!(
            list_marker(&row(json!([{"tag":"text","text":" - ","style":[]}]))),
            None
        );
    }

    #[test]
    fn nested_lists_follow_feishu_rows() {
        // 真实取样 8：编辑器写的无序列表（带第二层）后接有序列表（第二层是 a.），再接一行正文。
        let rows = json!([
            [{"tag":"text","text":"无序列表与嵌套","style":[]}],
            [{"tag":"text","text":"- ","style":[]},{"tag":"text","text":"无序一","style":[]}],
            [{"tag":"text","text":"  - ","style":[]},{"tag":"text","text":"嵌套一","style":[]}],
            [{"tag":"text","text":"- ","style":[]},{"tag":"text","text":"无序二","style":[]}],
            [{"tag":"text","text":"1. ","style":[]},{"tag":"text","text":"有序一","style":[]}],
            [{"tag":"text","text":"  a. ","style":[]},{"tag":"text","text":"有序下的嵌套","style":[]}],
            [{"tag":"text","text":"2. ","style":[]},{"tag":"text","text":"有序二","style":[]}],
            [{"tag":"text","text":"列表之后的正文","style":[]}]
        ]);
        let rich = post_rich(&reference(), None, rows.as_array().unwrap(), None).unwrap();
        let li = |text: &str, nested: Option<RichNode>| {
            let mut children = vec![p(vec![t(text)])];
            children.extend(nested);
            item(children)
        };
        assert_eq!(
            rich.root.children,
            vec![
                p(vec![t("无序列表与嵌套")]),
                RichNode::List {
                    ordered: false,
                    start: None,
                    children: vec![
                        li(
                            "无序一",
                            Some(RichNode::List {
                                ordered: false,
                                start: None,
                                children: vec![li("嵌套一", None)]
                            })
                        ),
                        li("无序二", None),
                    ],
                },
                RichNode::List {
                    ordered: true,
                    start: None,
                    children: vec![
                        li(
                            "有序一",
                            Some(RichNode::List {
                                ordered: true,
                                start: None,
                                children: vec![li("有序下的嵌套", None)]
                            })
                        ),
                        li("有序二", None),
                    ],
                },
                p(vec![t("列表之后的正文")]),
            ]
        );
    }

    #[test]
    fn code_block_newlines_are_restored_only_when_all_doubled() {
        // 取样 1、2：接口发的每个换行都翻倍；取样 7：编辑器写的正常、中间有真的空行。
        assert_eq!(
            restore_code("fn main() {\n\n    println!(\"hi\");\n\n}\n\n"),
            "fn main() {\n    println!(\"hi\");\n}"
        );
        assert_eq!(restore_code("print('hi')\n\n"), "print('hi')");
        assert_eq!(
            restore_code("let a = 1;\n\nlet b = 2;\n"),
            "let a = 1;\n\nlet b = 2;"
        );
        assert_eq!(restore_code("一行"), "一行");
        // 接口发的原文自己以换行结尾（2026-09-28 真实环境验收时发的“验收 1”）：翻倍后末尾是 4 个换行，
        // 去掉飞书多出的两个、还原翻倍后，结尾那个换行是这一行的结束，不是多一行空行。
        assert_eq!(
            restore_code("fn main() {\n\n    println!(\"hi\");\n\n}\n\n\n\n"),
            "fn main() {\n    println!(\"hi\");\n}"
        );
    }

    #[test]
    fn styles_links_mentions_emoji_and_blocks() {
        // 取样 1 的形状（接口发送、读回）：样式数组、链接、占位 @、表情、分割线、代码块、手打符号原样。
        let mentions =
            vec![json!({"id":"ou_me","id_type":"open_id","key":"@_user_1","name":"张三"})];
        let rows = json!([
            [{"tag":"text","text":"加粗斜体","style":["bold","italic"]},{"tag":"text","text":"删除线","style":["lineThrough"]}],
            [{"tag":"a","href":"https://open.feishu.cn","text":"加粗链接","style":["bold"]}],
            [{"tag":"at","user_id":"@_user_1","user_name":"张三","style":[]}],
            [{"tag":"emotion","emoji_type":"SMILE"}],
            [{"tag":"hr"}],
            [{"tag":"code_block","language":"RUST","text":"a\n\nb\n\n"}],
            [{"tag":"text","text":"*星号* <b>尖括号</b>","style":[]}]
        ]);
        let rich = post_rich(
            &reference(),
            Some("富文本标签"),
            rows.as_array().unwrap(),
            Some(&mentions),
        )
        .unwrap();
        let c = &rich.root.children;
        assert_eq!(
            c[0],
            p(vec![RichNode::Strong {
                children: vec![t("富文本标签")]
            }])
        );
        assert_eq!(
            c[1],
            p(vec![
                RichNode::Strong {
                    children: vec![RichNode::Emphasis {
                        children: vec![t("加粗斜体")]
                    }]
                },
                RichNode::Delete {
                    children: vec![t("删除线")]
                },
            ])
        );
        assert_eq!(
            c[2],
            p(vec![RichNode::Strong {
                children: vec![link("https://open.feishu.cn", "加粗链接")]
            }])
        );
        assert!(
            matches!(&c[3], RichNode::Paragraph { children } if matches!(&children[..], [RichNode::Mention { actor: Some(a), everyone: false }] if a.id == "ou_me"))
        );
        assert_eq!(
            c[4],
            p(vec![RichNode::Emoji {
                emoji_type: "SMILE".into()
            }])
        );
        assert_eq!(c[5], RichNode::ThematicBreak);
        assert_eq!(
            c[6],
            RichNode::Code {
                lang: Some("rust".into()),
                value: "a\nb".into()
            }
        );
        assert_eq!(c[7], p(vec![t("*星号* <b>尖括号</b>")]));
    }

    #[test]
    fn markdown_nodes_keep_quote_inline_code_and_feishu_mentions() {
        // 真实取样 2 的 content_v2（接口用 md 发送）：引用与行内代码只在这里有；@ 写成 <at user_id>。
        let md = "**加粗** *斜体* ~~删除线~~ `行内代码` [链接](https://www.feishu.cn)\n\n1. 第一项\n2. 第二项\n   - 嵌套无序一\n\n> 引用一行\n> 引用第二行\n\n```python\nprint('hi')\n```\n\n---\n\n<at user_id=\"ou_me\">张三</at> 提及";
        let blocks = markdown_blocks(md).unwrap();
        let has = |pred: &dyn Fn(&RichNode) -> bool| {
            let rich = ImRich::new(blocks.clone());
            let mut found = false;
            rich.walk(&mut |n| found |= pred(n));
            found
        };
        assert!(has(
            &|n| matches!(n, RichNode::InlineCode { value } if value == "行内代码")
        ));
        assert!(has(&|n| matches!(n, RichNode::Blockquote { .. })));
        assert!(has(
            &|n| matches!(n, RichNode::Code { lang: Some(l), value } if l == "python" && value == "print('hi')")
        ));
        assert!(has(&|n| matches!(n, RichNode::List { ordered: true, .. })));
        assert!(has(
            &|n| matches!(n, RichNode::Mention { actor: Some(a), .. } if a.id == "ou_me" && a.name.as_deref() == Some("张三"))
        ));
        assert!(has(&|n| matches!(n, RichNode::ThematicBreak)));
        // 其它 HTML 当文字，不执行。
        assert_eq!(
            markdown_blocks("<script>x</script>").unwrap(),
            vec![t("<script>x</script>")]
        );
    }

    #[test]
    fn only_old_posts_with_now_supported_nodes_are_reread() {
        let with = |kind: &str| ImMessage {
            reference: reference(),
            sender: ActorRef {
                id: "ou_a".into(),
                id_type: "open_id".into(),
                kind: "user".into(),
                name: None,
            },
            created_at_ms: 1,
            updated_at_ms: None,
            content: vec![unsupported(kind)],
            reply_to: None,
            root_message: None,
            thread_id: None,
            in_thread: None,
            deleted: false,
            incomplete: true,
            edited_at_ms: None,
            broadcast: false,
            reactions: None,
            thread_copy: None,
            thread_activity: None,
            edit_source: None,
            raw: Default::default(),
        };
        for kind in ["emotion", "hr", "code_block"] {
            assert!(is_unsupported_rich_post(&with(kind)), "{kind}");
        }
        // 投票、视频等仍不支持的不重读；撤回的不重读。
        assert!(!is_unsupported_rich_post(&with("vote")));
        assert!(!is_unsupported_rich_post(&with("media")));
        let mut recalled = with("hr");
        recalled.deleted = true;
        assert!(!is_unsupported_rich_post(&recalled));
        // 升级前压平的富文本里有网址时，本机修正先把它包成格式正文，暂不支持的表情落进树里：仍要重读
        // （真实数据：示例测试 群的取样 1，2026-09-28 核对开发版的库时发现）。
        let mut repaired = with("emotion");
        repaired.content = vec![
            ImContent::Text {
                text: "链接文字 (https://www.feishu.cn) ".into(),
            },
            unsupported("emotion"),
        ];
        repaired.content = repair_links(&repaired.content).expect("有网址，要修正");
        assert!(matches!(repaired.content.as_slice(), [ImContent::Rich(_)]));
        assert!(is_unsupported_rich_post(&repaired));
        let mut repaired_vote = with("vote");
        repaired_vote.content = vec![
            ImContent::Text {
                text: "https://www.feishu.cn ".into(),
            },
            unsupported("vote"),
        ];
        repaired_vote.content = repair_links(&repaired_vote.content).expect("有网址，要修正");
        assert!(!is_unsupported_rich_post(&repaired_vote));
    }

    #[test]
    fn plain_posts_do_not_need_rich_and_budgets_are_bounded() {
        let plain =
            json!([[{"tag":"text","text":"只有字","style":[]},{"tag":"at","user_id":"all"}]]);
        assert!(!post_needs_rich(None, plain.as_array().unwrap()));
        assert!(post_needs_rich(Some("标题"), plain.as_array().unwrap()));
        let deep = (0..40).fold("x".to_owned(), |inner, _| format!("> {inner}"));
        assert_eq!(
            markdown_blocks(&deep)
                .map(|b| check_budget(&ImRich::new(b)))
                .unwrap()
                .unwrap_err()
                .kind,
            ErrorKind::LimitExceeded
        );
        let long = "a".repeat(markdown::MARKDOWN_TEXT_MAX_BYTES + 1);
        assert_eq!(
            markdown_blocks(&long).unwrap_err().kind,
            ErrorKind::LimitExceeded
        );
    }
}
