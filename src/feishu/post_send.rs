//! 发出的格式正文 → 飞书富文本（`post`）（协议 provider-protocol v40，HHH friend/composer-rich-text v2）。
//!
//! 飞书接口发富文本有两种写法：原生标签（`text` 带样式、`a`、`at`、`emotion`、`code_block`、`hr`）与 `md` 段。
//! 真实取样（2026-10-26，测试群，用户在飞书 App 里截图）：`md` 段里的 `[微笑]`、`:SMILE:` 都显示成字，只有原生
//! `emotion` 是图；原生标签又没有行内代码、引用和列表。所以：
//! - 段落（文字、提及、表情、粗体、斜体、下划线、删除线、链接）用原生标签，表情是图；段落里有行内代码而没有表情时
//!   这一段改用 `md`（保住行内代码；下划线在 `md` 里没有写法，只留字）；两样都有时表情优先，行内代码只留字。
//! - 引用、列表用 `md` 段：里面的表情写成 `[名字]`（飞书 App 里是字，宿主照样认成图）。代码块用原生 `code_block`。
//! - 段落里的换行另起一行（飞书富文本一行就是一段）；每个 `md` 段单独占一行。
//!
//! 提及只认飞书的用户编号（字母、数字、下划线）；`md` 里的文字把 Markdown 符号转义，免得用户打的字变成格式或提及。

use crate::*;
use hhh_im_protocol::content::RichNode as Node;
use serde_json::{Value, json};

/// 一条格式正文转成的富文本最多多少字节（飞书限 30 KB 的请求体，留出余量）。
const POST_MAX_BYTES: usize = 28 * 1024;
/// 提及与表情编号的长度上限。
const ID_MAX_BYTES: usize = 128;
/// 嵌套列表每层缩进（飞书文档的写法是 4 个空格）。
const LIST_INDENT: &str = "    ";

/// 格式正文的块 → 富文本内容 JSON（`msg_type` 是 `post`）。`name_of` 给表情的中文名（写进 `md` 段时用）。
///
/// # Errors
/// 内容为空、提及或表情编号不对、超过大小时返回 [`ErrorKind::InvalidRequest`] 或 [`ErrorKind::LimitExceeded`]。
pub(crate) fn post_content(
    nodes: &[Node],
    name_of: &dyn Fn(&str) -> Option<String>,
) -> ImResult<String> {
    let writer = Writer { name_of };
    let mut rows: Vec<Vec<Value>> = Vec::new();
    for node in nodes {
        writer.block(node, &mut rows)?;
    }
    while rows.first().is_some_and(Vec::is_empty) {
        rows.remove(0);
    }
    while rows.last().is_some_and(Vec::is_empty) {
        rows.pop();
    }
    if rows.is_empty() {
        return Err(ImError::new(
            ErrorKind::InvalidRequest,
            "不能发送空白消息。",
        ));
    }
    let content = serde_json::to_string(&json!({"zh_cn": {"title": "", "content": rows}}))
        .map_err(|_| ImError::new(ErrorKind::InvalidRequest, "无法编码发送内容。"))?;
    if content.len() > POST_MAX_BYTES {
        return Err(ImError::new(
            ErrorKind::LimitExceeded,
            "发送内容超过长度预算。",
        ));
    }
    Ok(content)
}

struct Writer<'a> {
    name_of: &'a dyn Fn(&str) -> Option<String>,
}

fn contains(nodes: &[Node], wanted: &dyn Fn(&Node) -> bool) -> bool {
    nodes
        .iter()
        .any(|node| wanted(node) || children(node).is_some_and(|inner| contains(inner, wanted)))
}

fn children(node: &Node) -> Option<&[Node]> {
    match node {
        Node::Paragraph { children }
        | Node::Heading { children, .. }
        | Node::Blockquote { children }
        | Node::List { children, .. }
        | Node::ListItem { children }
        | Node::Table { children, .. }
        | Node::TableRow { children }
        | Node::TableCell { children }
        | Node::Strong { children }
        | Node::Emphasis { children }
        | Node::Delete { children }
        | Node::Underline { children }
        | Node::Link { children, .. }
        | Node::Color { children, .. } => Some(children),
        _ => None,
    }
}

/// 只留下的字（链接文字、退化成文字的内容）。
fn plain(nodes: &[Node]) -> String {
    let mut out = String::new();
    for node in nodes {
        match node {
            Node::Text { value } | Node::InlineCode { value } | Node::Code { value, .. } => {
                out.push_str(value)
            }
            Node::Tag { text, .. } => out.push_str(text),
            Node::Break => out.push('\n'),
            other => {
                if let Some(inner) = children(other) {
                    out.push_str(&plain(inner));
                }
            }
        }
    }
    out
}

fn user_id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= ID_MAX_BYTES
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn bad(reason: &str) -> ImError {
    ImError::new(ErrorKind::InvalidRequest, reason)
}

impl Writer<'_> {
    fn block(&self, node: &Node, rows: &mut Vec<Vec<Value>>) -> ImResult<()> {
        match node {
            Node::Paragraph { children } | Node::Heading { children, .. } => {
                let emoji = contains(children, &|n| matches!(n, Node::Emoji { .. }));
                let code = contains(children, &|n| matches!(n, Node::InlineCode { .. }));
                if code && !emoji {
                    rows.push(vec![md(self.inline_md(children, "")?)]);
                } else {
                    let mut lines = vec![Vec::new()];
                    self.native(children, &[], &mut lines)?;
                    rows.extend(lines);
                }
            }
            Node::Blockquote { children } => {
                let text = self.blocks_md(children, "")?;
                let quoted = text
                    .split('\n')
                    .map(|line| format!("> {line}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                rows.push(vec![md(quoted)]);
            }
            Node::List { .. } => {
                let mut lines = Vec::new();
                self.list_md(node, "", &mut lines)?;
                rows.push(vec![md(lines.join("\n"))]);
            }
            Node::Code { lang, value } => {
                let mut tag = json!({"tag": "code_block", "text": value});
                if let Some(lang) = lang.as_deref().filter(|l| !l.is_empty()) {
                    tag["language"] = Value::from(lang.to_uppercase());
                }
                rows.push(vec![tag]);
            }
            Node::ThematicBreak => rows.push(vec![json!({"tag": "hr"})]),
            // 输入框不会交出的块（表格等）：按文字发，不丢字。
            other => {
                for line in plain(std::slice::from_ref(other)).split('\n') {
                    rows.push(if line.is_empty() {
                        Vec::new()
                    } else {
                        vec![json!({"tag": "text", "text": line})]
                    });
                }
            }
        }
        Ok(())
    }

    /// 原生标签：`lines` 的最后一行是当前行，换行另起一行。
    fn native(
        &self,
        nodes: &[Node],
        style: &[&'static str],
        lines: &mut Vec<Vec<Value>>,
    ) -> ImResult<()> {
        let with = |extra: &'static str| {
            let mut next = style.to_vec();
            if !next.contains(&extra) {
                next.push(extra);
            }
            next
        };
        fn push(lines: &mut Vec<Vec<Value>>, tag: Value) {
            match lines.last_mut() {
                Some(line) => line.push(tag),
                None => lines.push(vec![tag]),
            }
        }
        for node in nodes {
            match node {
                Node::Text { value } | Node::InlineCode { value } => {
                    for (i, part) in value.split('\n').enumerate() {
                        if i > 0 {
                            lines.push(Vec::new());
                        }
                        if !part.is_empty() {
                            push(lines, json!({"tag": "text", "text": part, "style": style}));
                        }
                    }
                }
                Node::Break => lines.push(Vec::new()),
                Node::Strong { children } => self.native(children, &with("bold"), lines)?,
                Node::Emphasis { children } => self.native(children, &with("italic"), lines)?,
                Node::Underline { children } => self.native(children, &with("underline"), lines)?,
                Node::Delete { children } => self.native(children, &with("lineThrough"), lines)?,
                Node::Link { url, children } => {
                    let text = plain(children);
                    push(
                        lines,
                        json!({"tag": "a", "href": url, "text": if text.is_empty() { url.clone() } else { text }, "style": style}),
                    );
                }
                Node::Mention { everyone: true, .. } => {
                    push(lines, json!({"tag": "at", "user_id": "all"}))
                }
                Node::Mention {
                    person: Some(person),
                    ..
                } => {
                    if !user_id_ok(&person.user_id) {
                        return Err(bad("提及对象编号格式不正确。"));
                    }
                    push(lines, json!({"tag": "at", "user_id": person.user_id}));
                }
                Node::Mention { .. } => return Err(bad("提及缺少对象。")),
                Node::Emoji { emoji_type } => {
                    if emoji_type.is_empty() || emoji_type.len() > ID_MAX_BYTES {
                        return Err(bad("表情编号格式不正确。"));
                    }
                    push(lines, json!({"tag": "emotion", "emoji_type": emoji_type}));
                }
                other => match children(other) {
                    Some(inner) => self.native(inner, style, lines)?,
                    None => {
                        let text = plain(std::slice::from_ref(other));
                        if !text.is_empty() {
                            push(lines, json!({"tag": "text", "text": text, "style": style}));
                        }
                    }
                },
            }
        }
        Ok(())
    }

    /// `md` 里的行内内容；换行接上 `indent`（列表项里续行要缩进）。
    fn inline_md(&self, nodes: &[Node], indent: &str) -> ImResult<String> {
        let mut out = String::new();
        let mut line_start = true;
        for node in nodes {
            let piece = match node {
                Node::Text { value } => {
                    let mut text = String::new();
                    for (i, part) in value.split('\n').enumerate() {
                        if i > 0 {
                            text.push('\n');
                            text.push_str(indent);
                        }
                        text.push_str(&escape_md(part, line_start || i > 0));
                    }
                    text
                }
                Node::InlineCode { value } => {
                    if value.contains('`') {
                        format!("`` {value} ``")
                    } else {
                        format!("`{value}`")
                    }
                }
                Node::Break => format!("\n{indent}"),
                Node::Strong { children } => wrap("**", &self.inline_md(children, indent)?),
                Node::Emphasis { children } => wrap("*", &self.inline_md(children, indent)?),
                Node::Delete { children } => wrap("~~", &self.inline_md(children, indent)?),
                Node::Link { url, children } => {
                    let text = self.inline_md(children, indent)?;
                    let url = url.replace(' ', "%20").replace(')', "%29");
                    format!(
                        "[{}]({url})",
                        if text.is_empty() {
                            escape_md(&url, false)
                        } else {
                            text
                        }
                    )
                }
                Node::Mention { everyone: true, .. } => "<at user_id=\"all\"></at>".into(),
                Node::Mention {
                    person: Some(person),
                    ..
                } => {
                    if !user_id_ok(&person.user_id) {
                        return Err(bad("提及对象编号格式不正确。"));
                    }
                    format!("<at user_id=\"{}\"></at>", person.user_id)
                }
                Node::Mention { .. } => return Err(bad("提及缺少对象。")),
                Node::Emoji { emoji_type } => format!(
                    "[{}]",
                    (self.name_of)(emoji_type).unwrap_or_else(|| emoji_type.clone())
                ),
                other => match children(other) {
                    // 下划线、彩色字等 `md` 里没有写法的：只留里面的内容。
                    Some(inner) => self.inline_md(inner, indent)?,
                    None => escape_md(&plain(std::slice::from_ref(other)), line_start),
                },
            };
            line_start = piece.ends_with('\n') || (line_start && piece.is_empty());
            out.push_str(&piece);
        }
        Ok(out)
    }

    /// `md` 里的一串块（引用里的段落、列表项里的几段），块与块之间换行。
    fn blocks_md(&self, nodes: &[Node], indent: &str) -> ImResult<String> {
        let mut parts = Vec::new();
        for node in nodes {
            match node {
                Node::List { .. } => {
                    let mut lines = Vec::new();
                    self.list_md(node, indent, &mut lines)?;
                    parts.push(lines.join("\n"));
                }
                Node::Code { value, .. } => parts.push(format!("```\n{value}\n```")),
                Node::Blockquote { children } => {
                    let inner = self.blocks_md(children, "")?;
                    parts.push(
                        inner
                            .split('\n')
                            .map(|line| format!("> {line}"))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    );
                }
                other => parts.push(self.inline_md(
                    children(other).unwrap_or(std::slice::from_ref(other)),
                    indent,
                )?),
            }
        }
        Ok(parts.join("\n"))
    }

    fn list_md(&self, list: &Node, indent: &str, lines: &mut Vec<String>) -> ImResult<()> {
        let Node::List {
            ordered,
            start,
            children: items,
        } = list
        else {
            return Ok(());
        };
        let first = start.unwrap_or(1);
        for (i, item) in items.iter().enumerate() {
            let marker = if *ordered {
                format!("{}. ", first as usize + i)
            } else {
                "- ".to_string()
            };
            let hang = format!("{indent}{}", " ".repeat(marker.chars().count()));
            let mut wrote = false;
            for block in children_of_item(item) {
                match block {
                    Node::List { .. } => {
                        // 只装着子列表的一项：先写下这一项的标记。
                        if !wrote {
                            lines.push(format!("{indent}{}", marker.trim_end()));
                            wrote = true;
                        }
                        self.list_md(block, &format!("{indent}{LIST_INDENT}"), lines)?;
                    }
                    other => {
                        let text = self.inline_md(
                            children(other).unwrap_or(std::slice::from_ref(other)),
                            &hang,
                        )?;
                        if wrote {
                            lines.push(format!("{hang}{text}"));
                        } else {
                            lines.push(format!("{indent}{marker}{text}"));
                            wrote = true;
                        }
                    }
                }
            }
            if !wrote {
                lines.push(format!("{indent}{}", marker.trim_end()));
            }
        }
        Ok(())
    }
}

fn children_of_item(item: &Node) -> &[Node] {
    match item {
        Node::ListItem { children } => children,
        other => std::slice::from_ref(other),
    }
}

fn md(text: String) -> Value {
    json!({"tag": "md", "text": text})
}

/// 样式标记包住里面的字：首尾的空白放在标记外面（`** 粗体**` 不算粗体）。
fn wrap(marker: &str, inner: &str) -> String {
    let core = inner.trim();
    if core.is_empty() {
        return inner.to_string();
    }
    let lead = &inner[..inner.len() - inner.trim_start().len()];
    let trail = &inner[inner.trim_end().len()..];
    format!("{lead}{marker}{core}{marker}{trail}")
}

/// `md` 里的文字：Markdown 符号与 `<` 前面加反斜杠；行首会变成标题、列表、引用的写法也转义。
fn escape_md(text: &str, line_start: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(
            c,
            '\\' | '`' | '*' | '_' | '~' | '[' | ']' | '<' | '>' | '|'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    if line_start {
        let trimmed = out.trim_start();
        let lead = out.len() - trimmed.len();
        if trimmed.starts_with(['#', '-', '+']) {
            out.insert(lead, '\\');
        } else if let Some(digits) = trimmed.find(|c: char| !c.is_ascii_digit())
            && digits > 0
            && trimmed[digits..].starts_with(['.', ')'])
        {
            out.insert(lead + digits, '\\');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use hhh_im_protocol::content::PersonRef;

    fn t(value: &str) -> Node {
        Node::Text {
            value: value.into(),
        }
    }
    fn p(children: Vec<Node>) -> Node {
        Node::Paragraph { children }
    }
    fn names(t: &str) -> Option<String> {
        match t {
            "SMILE" => Some("微笑".into()),
            "GLANCE" => Some("看".into()),
            _ => None,
        }
    }
    fn rows(nodes: &[Node]) -> Value {
        let content: Value = serde_json::from_str(&post_content(nodes, &names).unwrap()).unwrap();
        content["zh_cn"]["content"].clone()
    }

    #[test]
    fn paragraphs_with_emoji_use_native_tags_and_emoji_stays_a_picture() {
        let content = rows(&[p(vec![
            Node::Strong {
                children: vec![t("明早 9 点")],
            },
            t(" 集合"),
            Node::Emoji {
                emoji_type: "SMILE".into(),
            },
            Node::Break,
            Node::Underline {
                children: vec![Node::Delete {
                    children: vec![t("不去")],
                }],
            },
            Node::Mention {
                person: Some(PersonRef::user("ou_a1", Some("陈言"))),
                everyone: false,
            },
            Node::Mention {
                person: None,
                everyone: true,
            },
            Node::Link {
                url: "https://example.com".into(),
                children: vec![Node::Emphasis {
                    children: vec![t("路线")],
                }],
            },
            Node::InlineCode {
                value: "登山杖".into(),
            },
        ])]);
        assert_eq!(
            content,
            json!([
                [
                    {"tag": "text", "text": "明早 9 点", "style": ["bold"]},
                    {"tag": "text", "text": " 集合", "style": []},
                    {"tag": "emotion", "emoji_type": "SMILE"}
                ],
                [
                    {"tag": "text", "text": "不去", "style": ["underline", "lineThrough"]},
                    {"tag": "at", "user_id": "ou_a1"},
                    {"tag": "at", "user_id": "all"},
                    {"tag": "a", "href": "https://example.com", "text": "路线", "style": []},
                    {"tag": "text", "text": "登山杖", "style": []}
                ]
            ])
        );
    }

    #[test]
    fn a_paragraph_with_inline_code_and_no_emoji_goes_as_markdown() {
        let content = rows(&[p(vec![
            t("带上 "),
            Node::InlineCode {
                value: "登山杖".into(),
            },
            t(" 和 "),
            Node::Strong {
                children: vec![t(" 水")],
            },
            Node::Mention {
                person: Some(PersonRef::user("ou_a1", None)),
                everyone: false,
            },
        ])]);
        assert_eq!(
            content,
            json!([[{"tag": "md", "text": "带上 `登山杖` 和  **水**<at user_id=\"ou_a1\"></at>"}]])
        );
    }

    #[test]
    fn lists_quotes_and_code_blocks() {
        let item = |children: Vec<Node>| Node::ListItem { children };
        let content = rows(&[
            Node::List {
                ordered: true,
                start: Some(3),
                children: vec![
                    item(vec![
                        p(vec![
                            t("水"),
                            Node::Emoji {
                                emoji_type: "GLANCE".into(),
                            },
                        ]),
                        Node::List {
                            ordered: false,
                            start: None,
                            children: vec![item(vec![p(vec![
                                t("矿泉水"),
                                Node::Break,
                                t("两瓶"),
                            ])])],
                        },
                    ]),
                    item(vec![p(vec![Node::Strong {
                        children: vec![t("零食")],
                    }])]),
                ],
            },
            Node::Blockquote {
                children: vec![p(vec![t("下雨"), Node::Break, t("改周日 *真的*")])],
            },
            Node::Code {
                lang: Some("rust".into()),
                value: "let a = 1;\nlet b = 2;".into(),
            },
            Node::Code {
                lang: None,
                value: "plain".into(),
            },
        ]);
        assert_eq!(
            content,
            json!([
                [{"tag": "md", "text": "3. 水[看]\n    - 矿泉水\n      两瓶\n4. **零食**"}],
                [{"tag": "md", "text": "> 下雨\n> 改周日 \\*真的\\*"}],
                [{"tag": "code_block", "text": "let a = 1;\nlet b = 2;", "language": "RUST"}],
                [{"tag": "code_block", "text": "plain"}]
            ])
        );
    }

    #[test]
    fn typed_markdown_symbols_and_at_tags_are_escaped_in_markdown() {
        let content = rows(&[Node::Blockquote {
            children: vec![p(vec![t(
                "1. 不是列表 <at user_id=\"all\"></at> # [x](y) a_b",
            )])],
        }]);
        assert_eq!(
            content[0][0]["text"],
            "> 1\\. 不是列表 \\<at user\\_id=\"all\"\\>\\</at\\> # \\[x\\](y) a\\_b"
        );
        let heading = rows(&[Node::List {
            ordered: false,
            start: None,
            children: vec![Node::ListItem {
                children: vec![p(vec![t("# 标题")])],
            }],
        }]);
        assert_eq!(heading[0][0]["text"], "- \\# 标题");
    }

    #[test]
    fn empty_bad_or_too_large_bodies_are_refused() {
        assert!(post_content(&[p(vec![])], &names).is_err());
        let bad = [p(vec![Node::Mention {
            person: Some(PersonRef::user("ou_<x>", None)),
            everyone: false,
        }])];
        assert_eq!(
            post_content(&bad, &names).unwrap_err().kind,
            ErrorKind::InvalidRequest
        );
        let huge = [p(vec![t(&"字".repeat(12 * 1024))])];
        assert_eq!(
            post_content(&huge, &names).unwrap_err().kind,
            ErrorKind::LimitExceeded
        );
    }

    #[test]
    fn leading_and_trailing_empty_lines_are_dropped_but_middle_ones_stay() {
        let content = rows(&[
            p(vec![]),
            p(vec![t("甲")]),
            p(vec![]),
            p(vec![t("乙")]),
            p(vec![]),
        ]);
        assert_eq!(
            content,
            json!([[{"tag": "text", "text": "甲", "style": []}], [], [{"tag": "text", "text": "乙", "style": []}]])
        );
    }
}
