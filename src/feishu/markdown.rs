//! 飞书写法里的 Markdown 解析成格式正文节点：富文本的 `md` 节点（契约 client v6 §3）与卡片 2.0 的
//! `markdown`、`lark_md`（契约 client v16 §3–§4）共用这一份 GFM 解析与节点转换。
//!
//! 调用方之间的不同放在 [`Options`] 里；平台自己的写法（卡片里的 `<font>`、`<text_tag>` 等，标签名带下划线的
//! CommonMark 根本认不出）由调用方在解析**之前**换成 [`Placeholders`] 的占位，这里解析完再按位置换回节点。

use crate::*;

/// 单段 Markdown 原文的上限；超过按读取预算报错。
pub(super) const MARKDOWN_TEXT_MAX_BYTES: usize = 256 * 1024;

/// 占位用的私用区字符：叶子节点 `E000 下标 E001`、彩色开 `E002 下标 E003`、彩色闭 `E004`。
const LEAF_OPEN: char = '\u{E000}';
const LEAF_CLOSE: char = '\u{E001}';
const COLOR_OPEN: char = '\u{E002}';
const COLOR_OPEN_END: char = '\u{E003}';
const COLOR_CLOSE: char = '\u{E004}';

/// 各调用方不同的地方；默认值就是富文本 `md` 节点的规则。
#[derive(Default)]
pub(super) struct Options<'a> {
    /// Markdown 图片属于哪条消息：给了就成这条消息的图片资源（卡片，取样核实），
    /// 没给按暂不支持（富文本里的写法没取样）。
    pub images: Option<&'a MessageRef>,
    /// 表格每页几行（卡片照飞书分页）；不给不分页。
    pub table_page_size: Option<u16>,
    /// `:OK:` 这类表情码成表情节点（卡片）。
    pub emoji_codes: bool,
    /// 单独一行的 `<hr>` 是分割线（卡片，取样核实）；富文本里仍当文字。
    pub hr_lines: bool,
}

/// 调用方在解析前换出去的节点：叶子（提及、人员、时间、标签、链接）与彩色字的开闭。
#[derive(Default)]
pub(super) struct Placeholders {
    leaves: Vec<RichNode>,
    tones: Vec<Option<CardTone>>,
}

impl Placeholders {
    /// 一个叶子节点的占位。
    pub(super) fn leaf(&mut self, node: RichNode) -> String {
        self.leaves.push(node);
        format!("{LEAF_OPEN}{}{LEAF_CLOSE}", self.leaves.len() - 1)
    }

    /// 彩色字开头的占位；色调认不出时为空，闭合后只留里面的文字。
    pub(super) fn open_color(&mut self, tone: Option<CardTone>) -> String {
        self.tones.push(tone);
        format!("{COLOR_OPEN}{}{COLOR_OPEN_END}", self.tones.len() - 1)
    }

    /// 彩色字结尾的占位。
    pub(super) fn close_color() -> String {
        COLOR_CLOSE.to_string()
    }
}

/// 去掉原文里本来就有的占位字符，免得被当成占位（平台文字里不该有私用区字符）。
pub(super) fn strip_placeholder_chars(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(*c, LEAF_OPEN..=COLOR_CLOSE))
        .collect()
}

/// 一段 Markdown（占位已换好）解析成块：GFM；飞书的 `<at user_id="…">名字</at>` 转成提及，
/// 其它嵌入的 HTML 当文字；每个换行都换行。
///
/// # Errors
/// 原文超过上限时返回 [`ErrorKind::LimitExceeded`]；解析失败时返回 [`ErrorKind::ProtocolError`]。
pub(super) fn blocks(
    text: &str,
    options: &Options,
    placeholders: &Placeholders,
) -> ImResult<Vec<RichNode>> {
    if text.len() > MARKDOWN_TEXT_MAX_BYTES {
        return Err(ImError::new(
            ErrorKind::LimitExceeded,
            "Markdown 内容超过读取预算。",
        ));
    }
    let tree = markdown::to_mdast(text, &markdown::ParseOptions::gfm()).map_err(|error| {
        tracing::warn!(%error, "飞书内容里的 Markdown 解析失败");
        ImError::new(ErrorKind::ProtocolError, "Markdown 内容读不懂。")
    })?;
    let converter = Converter {
        options,
        placeholders,
    };
    Ok(converter.children(tree.children().map(Vec::as_slice).unwrap_or_default()))
}

struct Converter<'a> {
    options: &'a Options<'a>,
    placeholders: &'a Placeholders,
}

/// 一层兄弟节点在收彩色之前的样子：节点，或彩色的开 / 闭。
enum Piece {
    Node(RichNode),
    Open(Option<CardTone>),
    Close,
}

impl Converter<'_> {
    fn children(&self, nodes: &[markdown::mdast::Node]) -> Vec<RichNode> {
        let mut pieces = Vec::new();
        let mut index = 0;
        while index < nodes.len() {
            // `<at user_id="…">` 名字 `</at>`：解析器把开闭标签各当一个 HTML 节点，名字在中间的文字里。
            if let markdown::mdast::Node::Html(open) = &nodes[index]
                && let Some(id) = at_user_id(&open.value)
            {
                let mut name = String::new();
                let mut end = index + 1;
                while end < nodes.len() {
                    match &nodes[end] {
                        markdown::mdast::Node::Html(close) if close.value.trim() == "</at>" => {
                            break;
                        }
                        other => name.push_str(&other.to_string()),
                    }
                    end += 1;
                }
                pieces.push(Piece::Node(if id == "all" {
                    RichNode::Mention {
                        actor: None,
                        everyone: true,
                    }
                } else {
                    RichNode::Mention {
                        actor: Some(ActorRef {
                            id,
                            id_type: "open_id".into(),
                            kind: "user".into(),
                            name: (!name.trim().is_empty()).then(|| name.trim().to_owned()),
                        }),
                        everyone: false,
                    }
                }));
                index = (end + 1).min(nodes.len());
                continue;
            }
            match &nodes[index] {
                markdown::mdast::Node::Text(text) => self.text(&text.value, &mut pieces),
                other => pieces.push(Piece::Node(self.convert(other))),
            }
            index += 1;
        }
        fold_colors(pieces)
    }

    /// 文字：占位换回节点；聊天里每个换行都换行（与卡片正文同一规则）；卡片里的表情码成表情。
    fn text(&self, value: &str, out: &mut Vec<Piece>) {
        let mut rest = value;
        while let Some(at) = rest.find([LEAF_OPEN, COLOR_OPEN, COLOR_CLOSE]) {
            self.plain(&rest[..at], out);
            let marker = rest[at..].chars().next().unwrap_or_default();
            let after = &rest[at + marker.len_utf8()..];
            if marker == COLOR_CLOSE {
                out.push(Piece::Close);
                rest = after;
                continue;
            }
            let end_marker = if marker == LEAF_OPEN {
                LEAF_CLOSE
            } else {
                COLOR_OPEN_END
            };
            let Some(end) = after.find(end_marker) else {
                // 占位被截断（不该发生）：剩下的当文字。
                self.plain(after, out);
                return;
            };
            let slot = after[..end].parse::<usize>().ok();
            match (marker, slot) {
                (LEAF_OPEN, Some(i)) => {
                    if let Some(node) = self.placeholders.leaves.get(i) {
                        out.push(Piece::Node(node.clone()));
                    }
                }
                (_, Some(i)) => out.push(Piece::Open(
                    self.placeholders.tones.get(i).copied().flatten(),
                )),
                _ => {}
            }
            rest = &after[end + end_marker.len_utf8()..];
        }
        self.plain(rest, out);
    }

    fn plain(&self, value: &str, out: &mut Vec<Piece>) {
        for (i, line) in value.split('\n').enumerate() {
            if i > 0 {
                out.push(Piece::Node(RichNode::Break));
            }
            if self.options.emoji_codes {
                emoji_codes(line, out);
            } else if !line.is_empty() {
                out.push(Piece::Node(RichNode::Text { value: line.into() }));
            }
        }
    }

    fn convert(&self, node: &markdown::mdast::Node) -> RichNode {
        use markdown::mdast::Node;
        let kids = |n: &Node| self.children(n.children().map(Vec::as_slice).unwrap_or_default());
        match node {
            Node::Paragraph(_) => RichNode::Paragraph {
                children: kids(node),
            },
            Node::Heading(h) => RichNode::Heading {
                depth: h.depth,
                children: kids(node),
            },
            Node::ThematicBreak(_) => RichNode::ThematicBreak,
            Node::Blockquote(_) => RichNode::Blockquote {
                children: kids(node),
            },
            Node::List(list) => RichNode::List {
                ordered: list.ordered,
                start: list.start.filter(|s| list.ordered && *s != 1),
                children: kids(node),
            },
            Node::ListItem(_) => RichNode::ListItem {
                children: kids(node),
            },
            Node::Code(code) => RichNode::Code {
                lang: code.lang.clone().filter(|l| !l.is_empty()),
                value: code.value.clone(),
            },
            Node::Table(table) => RichNode::Table {
                align: table
                    .align
                    .iter()
                    .map(|a| match a {
                        markdown::mdast::AlignKind::Left => Some(RichAlign::Left),
                        markdown::mdast::AlignKind::Center => Some(RichAlign::Center),
                        markdown::mdast::AlignKind::Right => Some(RichAlign::Right),
                        markdown::mdast::AlignKind::None => None,
                    })
                    .collect(),
                page_size: self.options.table_page_size,
                children: kids(node),
            },
            Node::TableRow(_) => RichNode::TableRow {
                children: kids(node),
            },
            Node::TableCell(_) => RichNode::TableCell {
                children: kids(node),
            },
            Node::Text(text) => RichNode::Text {
                value: text.value.clone(),
            },
            Node::Strong(_) => RichNode::Strong {
                children: kids(node),
            },
            Node::Emphasis(_) => RichNode::Emphasis {
                children: kids(node),
            },
            Node::Delete(_) => RichNode::Delete {
                children: kids(node),
            },
            Node::InlineCode(code) => RichNode::InlineCode {
                value: code.value.clone(),
            },
            Node::Break(_) => RichNode::Break,
            Node::Link(link) => RichNode::Link {
                url: link.url.clone(),
                children: kids(node),
            },
            // 嵌入的 HTML 不执行，照原样当文字显示；卡片里单独一行的 `<hr>` 是分割线（取样核实）。
            Node::Html(html) if self.options.hr_lines && is_hr(&html.value) => {
                RichNode::ThematicBreak
            }
            Node::Html(html) => RichNode::Text {
                value: html.value.clone(),
            },
            // 卡片里的 `![说明](img_key)` 是这条消息的图片（取样核实能按消息资源读到）；富文本里的写法没取样。
            Node::Image(image) => match self.options.images {
                Some(message) if !image.url.trim().is_empty() => RichNode::Resource {
                    resource: ResourceRef {
                        message: message.clone(),
                        key: image.url.trim().to_owned(),
                        kind: ResourceKind::Image,
                    },
                    name: (!image.alt.trim().is_empty()).then(|| image.alt.trim().to_owned()),
                    size: None,
                },
                _ => RichNode::Unsupported {
                    kind: "md_image".into(),
                    label: Some("图片".into()),
                },
            },
            Node::ImageReference(_) => RichNode::Unsupported {
                kind: "md_image".into(),
                label: Some("图片".into()),
            },
            // 其它（脚注、定义、数学公式等）没取样：保留文字内容，不丢。
            other => RichNode::Text {
                value: other.to_string(),
            },
        }
    }
}

/// 把一层兄弟节点里彩色字的开闭之间收成一个彩色节点；色调认不出、没闭合或多出来的闭合都只留文字。
fn fold_colors(pieces: Vec<Piece>) -> Vec<RichNode> {
    let mut stack: Vec<(Option<CardTone>, Vec<RichNode>)> = vec![(None, Vec::new())];
    for piece in pieces {
        match piece {
            Piece::Node(node) => stack.last_mut().expect("栈底总在").1.push(node),
            Piece::Open(tone) => stack.push((tone, Vec::new())),
            Piece::Close if stack.len() > 1 => {
                let (tone, children) = stack.pop().expect("栈里不止一层");
                let into = &mut stack.last_mut().expect("栈底总在").1;
                match tone {
                    Some(tone) if !children.is_empty() => {
                        into.push(RichNode::Color { tone, children })
                    }
                    _ => into.extend(children),
                }
            }
            Piece::Close => {}
        }
    }
    while stack.len() > 1 {
        let (_, children) = stack.pop().expect("栈里不止一层");
        stack.last_mut().expect("栈底总在").1.extend(children);
    }
    stack.pop().map(|(_, nodes)| nodes).unwrap_or_default()
}

/// 一行文字里的 `:OK:` 表情码：冒号前后不紧贴字母数字、码以字母开头，免得把 `10:30:45`、`a:b:c` 当成表情。
/// 目录里认不认得由上层判断（认不出的照原文显示）。
fn emoji_codes(line: &str, out: &mut Vec<Piece>) {
    let bytes = line.as_bytes();
    let mut text_start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':'
            && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b':')
            && bytes.get(i + 1).is_some_and(u8::is_ascii_alphabetic)
        {
            let name_end = bytes[i + 1..]
                .iter()
                .position(|b| !(b.is_ascii_alphanumeric() || *b == b'_'))
                .map_or(bytes.len(), |p| i + 1 + p);
            let closes = bytes.get(name_end) == Some(&b':');
            let after_ok = bytes
                .get(name_end + 1)
                .is_none_or(|b| !b.is_ascii_alphanumeric() || *b == b':');
            if closes && after_ok && name_end - (i + 1) <= EMOJI_CODE_MAX {
                if text_start < i {
                    out.push(Piece::Node(RichNode::Text {
                        value: line[text_start..i].into(),
                    }));
                }
                out.push(Piece::Node(RichNode::Emoji {
                    emoji_type: line[i + 1..name_end].into(),
                }));
                i = name_end + 1;
                text_start = i;
                continue;
            }
        }
        i += 1;
    }
    if text_start < line.len() {
        out.push(Piece::Node(RichNode::Text {
            value: line[text_start..].into(),
        }));
    }
}

/// 表情码的长度上限，与表情回复的表情类型同一限制。
const EMOJI_CODE_MAX: usize = 64;

fn is_hr(html: &str) -> bool {
    let tag = html.trim().to_ascii_lowercase();
    tag == "<hr>" || tag == "<hr/>" || tag == "<hr />"
}

fn at_user_id(html: &str) -> Option<String> {
    let rest = html.trim().strip_prefix("<at ")?;
    let start = rest.find("user_id=\"")? + "user_id=\"".len();
    let end = rest[start..].find('"')? + start;
    Some(rest[start..end].to_owned()).filter(|id| !id.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(value: &str) -> RichNode {
        RichNode::Text {
            value: value.into(),
        }
    }

    fn card(text: &str, placeholders: &Placeholders) -> Vec<RichNode> {
        let message = MessageRef {
            conversation: ConversationRef::new("test", "oc_test"),
            id: "om_test".into(),
        };
        blocks(
            text,
            &Options {
                images: Some(&message),
                table_page_size: Some(5),
                emoji_codes: true,
                hr_lines: true,
            },
            placeholders,
        )
        .unwrap()
    }

    #[test]
    fn colors_fold_across_inline_nodes_and_unknown_colors_keep_text() {
        let mut p = Placeholders::default();
        let text = format!(
            "相当于 {}**0.9折**{}（只要 8.51%）{}灰字{}",
            p.open_color(Some(CardTone::Green)),
            Placeholders::close_color(),
            p.open_color(None),
            Placeholders::close_color()
        );
        assert_eq!(
            card(&text, &p),
            vec![RichNode::Paragraph {
                children: vec![
                    t("相当于 "),
                    RichNode::Color {
                        tone: CardTone::Green,
                        children: vec![RichNode::Strong {
                            children: vec![t("0.9折")]
                        }],
                    },
                    t("（只要 8.51%）"),
                    t("灰字"),
                ],
            }]
        );
    }

    #[test]
    fn leaves_come_back_in_place_and_stray_markers_are_harmless() {
        let mut p = Placeholders::default();
        let text = format!(
            "标签：{} 多出来的闭合{}",
            p.leaf(RichNode::Tag {
                text: "紫标签".into(),
                tone: CardTone::Purple
            }),
            Placeholders::close_color()
        );
        assert_eq!(
            card(&text, &p),
            vec![RichNode::Paragraph {
                children: vec![
                    t("标签："),
                    RichNode::Tag {
                        text: "紫标签".into(),
                        tone: CardTone::Purple
                    },
                    t(" 多出来的闭合"),
                ],
            }]
        );
        // 原文里本来就有的私用区字符先去掉，伪造不了占位。
        assert_eq!(strip_placeholder_chars("a\u{E000}0\u{E001}b"), "a0b");
    }

    #[test]
    fn emoji_codes_are_strict_about_their_neighbours() {
        let nodes = card(
            "表情：:OK::THUMBSUP: 时间 10:30:45 key:value: a:b:c",
            &Placeholders::default(),
        );
        let RichNode::Paragraph { children } = &nodes[0] else {
            panic!("{nodes:?}")
        };
        let emojis: Vec<_> = children
            .iter()
            .filter_map(|n| match n {
                RichNode::Emoji { emoji_type } => Some(emoji_type.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(emojis, ["OK", "THUMBSUP"]);
        let text: String = children
            .iter()
            .filter_map(|n| match n {
                RichNode::Text { value } => Some(value.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "表情： 时间 10:30:45 key:value: a:b:c");
    }

    #[test]
    fn card_tables_are_paged_images_belong_to_the_message_and_hr_lines_divide() {
        let nodes = card(
            "| 指标 | 数值 |\n| -------- | -------- |\n| 请求完成 | 97.66% |\n\n<hr>\n\n![悬停文字](img_v3_x)",
            &Placeholders::default(),
        );
        assert!(matches!(
            &nodes[0],
            RichNode::Table {
                page_size: Some(5),
                ..
            }
        ));
        assert_eq!(nodes[1], RichNode::ThematicBreak);
        assert!(matches!(
            &nodes[2],
            RichNode::Paragraph { children } if matches!(&children[0],
                RichNode::Resource { resource, name: Some(alt), .. }
                    if resource.key == "img_v3_x" && resource.message.id == "om_test" && alt == "悬停文字")
        ));
        // 富文本（默认规则）：图片仍暂不支持、表格不分页、表情码不转。
        let post = blocks(
            "| a |\n| - |\n| 1 |\n\n![x](img) :OK:",
            &Options::default(),
            &Placeholders::default(),
        )
        .unwrap();
        assert!(matches!(
            &post[0],
            RichNode::Table {
                page_size: None,
                ..
            }
        ));
        let RichNode::Paragraph { children } = &post[1] else {
            panic!("{post:?}")
        };
        assert!(matches!(&children[0], RichNode::Unsupported { kind, .. } if kind == "md_image"));
        assert_eq!(children[1], t(" :OK:"));
    }
}
