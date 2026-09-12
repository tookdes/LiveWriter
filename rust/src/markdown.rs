use markdown::{ParseOptions, mdast::Node, to_mdast};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Heading {
        level: u8,
        text: String,
    },
    Paragraph(String),
    Bullet {
        text: String,
        depth: usize,
    },
    Numbered {
        marker: String,
        text: String,
        depth: usize,
    },
    Task {
        checked: bool,
        text: String,
        depth: usize,
    },
    Quote(String),
    Aside(String),
    Code {
        text: String,
        language: Option<String>,
    },
    Image {
        alt: String,
        url: String,
    },
    Video {
        url: String,
    },
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Divider,
    Html(String),
}

/// Parse CommonMark with the full GitHub Flavored Markdown extension set.
///
/// `markdown-rs` handles the grammar (including task lists, tables,
/// strikethrough, autolinks, and footnotes); this adapter maps its AST to the
/// native preview blocks used by the application.
pub fn parse_blocks(markdown: &str) -> Vec<Block> {
    let Ok(tree) = to_mdast(markdown, &ParseOptions::gfm()) else {
        return vec![Block::Paragraph(markdown.to_owned())];
    };

    let mut blocks = Vec::new();
    append_node(&tree, &mut blocks);
    blocks
}

fn append_node(node: &Node, blocks: &mut Vec<Block>) {
    match node {
        Node::Root(root) => append_nodes(&root.children, blocks),
        Node::Heading(heading) => blocks.push(Block::Heading {
            level: heading.depth,
            text: inline_markdown(&heading.children),
        }),
        Node::Paragraph(paragraph) => append_paragraph(&paragraph.children, blocks),
        Node::List(list) => append_list(list, blocks, 0),
        Node::Blockquote(quote) => {
            let text = block_text(&quote.children);
            if !text.trim().is_empty() {
                blocks.push(Block::Quote(text));
            }
        }
        Node::Code(code) => blocks.push(Block::Code {
            text: code.value.clone(),
            language: code.lang.clone(),
        }),
        Node::ThematicBreak(_) => blocks.push(Block::Divider),
        Node::Table(table) => append_table(table, blocks),
        Node::Html(html) => append_html(&html.value, blocks),
        Node::Image(image) => blocks.push(Block::Image {
            alt: image.alt.clone(),
            url: image.url.clone(),
        }),
        _ => {
            let text = inline_markdown(std::slice::from_ref(node));
            if !text.trim().is_empty() {
                blocks.push(Block::Paragraph(text));
            }
        }
    }
}

fn append_nodes(nodes: &[Node], blocks: &mut Vec<Block>) {
    for node in nodes {
        append_node(node, blocks);
    }
}

fn append_paragraph(children: &[Node], blocks: &mut Vec<Block>) {
    if let [Node::Image(image)] = children {
        blocks.push(Block::Image {
            alt: image.alt.clone(),
            url: image.url.clone(),
        });
        return;
    }

    if let [Node::Link(link)] = children
        && is_video_label(&inline_markdown(&link.children))
    {
        blocks.push(Block::Video {
            url: link.url.clone(),
        });
        return;
    }

    if let [Node::Html(html)] = children
        && let Some(content) = aside_content(&html.value)
    {
        blocks.push(Block::Aside(content));
        return;
    }

    let text = inline_markdown(children);
    if !text.trim().is_empty() {
        blocks.push(Block::Paragraph(text));
    }
}

fn append_list(list: &markdown::mdast::List, blocks: &mut Vec<Block>, depth: usize) {
    for (offset, node) in list.children.iter().enumerate() {
        let Node::ListItem(item) = node else {
            continue;
        };

        let text = list_item_text(&item.children);
        if !text.trim().is_empty() {
            if let Some(checked) = item.checked {
                blocks.push(Block::Task {
                    checked,
                    text,
                    depth,
                });
            } else if list.ordered {
                let marker = list.start.unwrap_or(1) + offset as u32;
                blocks.push(Block::Numbered {
                    marker: marker.to_string(),
                    text,
                    depth,
                });
            } else {
                blocks.push(Block::Bullet { text, depth });
            }
        }

        for child in &item.children {
            if let Node::List(nested) = child {
                append_list(nested, blocks, depth.saturating_add(1));
            }
        }
    }
}

fn list_item_text(children: &[Node]) -> String {
    children
        .iter()
        .filter_map(|child| match child {
            Node::Paragraph(paragraph) => Some(inline_markdown(&paragraph.children)),
            Node::Code(code) => Some(format!("`{}`", code.value)),
            _ => None,
        })
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn append_table(table: &markdown::mdast::Table, blocks: &mut Vec<Block>) {
    let mut rows = table.children.iter().filter_map(|node| match node {
        Node::TableRow(row) => Some(
            row.children
                .iter()
                .filter_map(|cell| match cell {
                    Node::TableCell(cell) => Some(inline_markdown(&cell.children)),
                    _ => None,
                })
                .collect::<Vec<_>>(),
        ),
        _ => None,
    });

    let Some(headers) = rows.next() else {
        return;
    };
    blocks.push(Block::Table {
        headers,
        rows: rows.collect(),
    });
}

fn append_html(value: &str, blocks: &mut Vec<Block>) {
    if let Some(content) = aside_content(value) {
        blocks.push(Block::Aside(content));
    } else if !value.trim().is_empty() {
        // Raw HTML is intentionally shown as source instead of being executed.
        blocks.push(Block::Html(value.trim().to_owned()));
    }
}

fn aside_content(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let lowercase = trimmed.to_ascii_lowercase();
    if !lowercase.starts_with("<aside") {
        return None;
    }

    let opening_end = trimmed.find('>')?;
    let closing_start = lowercase.rfind("</aside>")?;
    (closing_start >= opening_end)
        .then(|| trimmed[opening_end + 1..closing_start].trim().to_owned())
}

fn is_video_label(label: &str) -> bool {
    let label = label.trim();
    label.eq_ignore_ascii_case("video") || label == "视频"
}

fn block_text(nodes: &[Node]) -> String {
    nodes
        .iter()
        .filter_map(|node| match node {
            Node::Paragraph(paragraph) => Some(inline_markdown(&paragraph.children)),
            Node::Code(code) => Some(format!("`{}`", code.value)),
            Node::List(list) => Some(
                list.children
                    .iter()
                    .filter_map(|node| match node {
                        Node::ListItem(item) => Some(list_item_text(&item.children)),
                        _ => None,
                    })
                    .filter(|text| !text.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        })
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn inline_markdown(nodes: &[Node]) -> String {
    nodes.iter().map(inline_markdown_node).collect::<String>()
}

fn inline_markdown_node(node: &Node) -> String {
    match node {
        Node::Text(text) => text.value.clone(),
        Node::Strong(strong) => format!("**{}**", inline_markdown(&strong.children)),
        Node::Emphasis(emphasis) => format!("*{}*", inline_markdown(&emphasis.children)),
        Node::Delete(delete) => format!("~~{}~~", inline_markdown(&delete.children)),
        Node::InlineCode(code) => format!("`{}`", code.value),
        Node::InlineMath(math) => format!("${}$", math.value),
        Node::Link(link) => format!("[{}]({})", inline_markdown(&link.children), link.url),
        Node::Image(image) => format!("![{}]({})", image.alt, image.url),
        Node::Break(_) => "\n".to_owned(),
        Node::Html(html) => html.value.clone(),
        Node::Paragraph(paragraph) => inline_markdown(&paragraph.children),
        _ => String::new(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InlineStyle {
    Bold,
    Italic,
    Strike,
    Code,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RichTextPiece {
    pub text: String,
    pub styles: Vec<InlineStyle>,
    pub link: Option<String>,
    pub image: Option<String>,
}

/// Parse inline Markdown (paragraph / heading / list item text) into annotated
/// text pieces. Both the live preview and the publishing export use this one
/// renderer, so the two views stay consistent.
pub fn parse_inline(markdown: &str) -> Vec<RichTextPiece> {
    let Ok(tree) = to_mdast(markdown, &ParseOptions::gfm()) else {
        return vec![RichTextPiece {
            text: markdown.to_owned(),
            styles: Vec::new(),
            link: None,
            image: None,
        }];
    };
    let children = match &tree {
        Node::Root(root) => root.children.as_slice(),
        _ => std::slice::from_ref(&tree),
    };
    let mut pieces = Vec::new();
    append_inline(children, &mut pieces, &[], None);
    pieces
}

fn append_inline(
    nodes: &[Node],
    pieces: &mut Vec<RichTextPiece>,
    styles: &[InlineStyle],
    link: Option<&str>,
) {
    for node in nodes {
        match node {
            Node::Text(text) => push_inline_piece(pieces, text.value.clone(), styles, link, None),
            Node::Strong(strong) => {
                let mut nested = styles.to_vec();
                nested.push(InlineStyle::Bold);
                append_inline(&strong.children, pieces, &nested, link);
            }
            Node::Emphasis(emphasis) => {
                let mut nested = styles.to_vec();
                nested.push(InlineStyle::Italic);
                append_inline(&emphasis.children, pieces, &nested, link);
            }
            Node::Delete(delete) => {
                let mut nested = styles.to_vec();
                nested.push(InlineStyle::Strike);
                append_inline(&delete.children, pieces, &nested, link);
            }
            Node::InlineCode(code) => {
                let mut nested = styles.to_vec();
                nested.push(InlineStyle::Code);
                push_inline_piece(pieces, code.value.clone(), &nested, link, None);
            }
            Node::Link(link_node) => {
                append_inline(&link_node.children, pieces, styles, Some(&link_node.url));
            }
            Node::Image(image) => {
                push_inline_piece(pieces, image.alt.clone(), styles, link, Some(&image.url));
            }
            Node::Break(_) => push_inline_piece(pieces, "\n".to_owned(), styles, link, None),
            Node::Html(html) => push_inline_piece(pieces, html.value.clone(), styles, link, None),
            Node::Paragraph(paragraph) => append_inline(&paragraph.children, pieces, styles, link),
            _ => {}
        }
    }
}

fn push_inline_piece(
    pieces: &mut Vec<RichTextPiece>,
    text: String,
    styles: &[InlineStyle],
    link: Option<&str>,
    image: Option<&str>,
) {
    if text.is_empty() {
        return;
    }
    pieces.push(RichTextPiece {
        text,
        styles: styles.to_vec(),
        link: link.map(ToOwned::to_owned),
        image: image.map(ToOwned::to_owned),
    });
}

pub fn strip_inline(markdown: &str) -> String {
    parse_inline(markdown)
        .into_iter()
        .map(|piece| piece.text)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Block, parse_blocks, strip_inline};

    #[test]
    fn parses_the_blocks_used_by_the_preview() {
        assert_eq!(
            parse_blocks("# 标题\n\n- 一\n\n1. 二\n\n> 三\n\n```\nlet x = 1;\n```"),
            vec![
                Block::Heading {
                    level: 1,
                    text: "标题".into()
                },
                Block::Bullet {
                    text: "一".into(),
                    depth: 0
                },
                Block::Numbered {
                    marker: "1".into(),
                    text: "二".into(),
                    depth: 0,
                },
                Block::Quote("三".into()),
                Block::Code {
                    text: "let x = 1;".into(),
                    language: None,
                },
            ]
        );
    }

    #[test]
    fn parses_gfm_tasks_and_notion_asides() {
        assert_eq!(
            parse_blocks(
                "- [ ] 待完成\n- [x] 已完成\n\n<aside>💡 **提示**</aside>\n\n<aside></aside>"
            ),
            vec![
                Block::Task {
                    checked: false,
                    text: "待完成".into(),
                    depth: 0,
                },
                Block::Task {
                    checked: true,
                    text: "已完成".into(),
                    depth: 0,
                },
                Block::Aside("💡 **提示**".into()),
                Block::Aside(String::new()),
            ]
        );
    }

    #[test]
    fn parses_images_and_tables_without_swallowing_following_text() {
        assert_eq!(
            parse_blocks(
                "![封面](https://example.com/a.png)\n\n| 名称 | 值 |\n| --- | --- |\n| 一 | 二 |\n\n正文"
            ),
            vec![
                Block::Image {
                    alt: "封面".into(),
                    url: "https://example.com/a.png".into(),
                },
                Block::Table {
                    headers: vec!["名称".into(), "值".into()],
                    rows: vec![vec!["一".into(), "二".into()]],
                },
                Block::Paragraph("正文".into()),
            ]
        );
        assert_eq!(
            parse_blocks("[视频](https://example.com/video)"),
            vec![Block::Video {
                url: "https://example.com/video".into(),
            }]
        );
    }

    #[test]
    fn parses_inline_styles_links_and_images() {
        use super::{InlineStyle, parse_inline};

        let pieces =
            parse_inline("**粗体** *斜体* `代码` [链接](https://example.com) ![图](a.png)");
        assert_eq!(pieces[0].text, "粗体");
        assert_eq!(pieces[0].styles, vec![InlineStyle::Bold]);
        assert_eq!(pieces[2].styles, vec![InlineStyle::Italic]);
        assert_eq!(pieces[4].styles, vec![InlineStyle::Code]);
        assert_eq!(pieces[6].link.as_deref(), Some("https://example.com"));
        assert_eq!(pieces[8].text, "图");
        assert_eq!(pieces[8].image.as_deref(), Some("a.png"));

        // Nested link inside bold keeps both annotations.
        let pieces = parse_inline("**[链接](https://x)**");
        assert_eq!(pieces[0].styles, vec![InlineStyle::Bold]);
        assert_eq!(pieces[0].link.as_deref(), Some("https://x"));

        // GFM does not treat intraword underscores as emphasis.
        let pieces = parse_inline("snake_case");
        assert_eq!(pieces.len(), 1);
        assert!(pieces[0].styles.is_empty());
        assert_eq!(pieces[0].text, "snake_case");
    }

    #[test]
    fn strips_common_inline_markdown() {
        assert_eq!(
            strip_inline("**粗体** [链接](https://example.com)"),
            "粗体 链接"
        );
        assert_eq!(strip_inline("![图片](https://example.com/a.png)"), "图片");
    }

    #[test]
    fn preserves_nested_list_depth() {
        assert_eq!(
            parse_blocks("- a\n  - b\n    - c"),
            vec![
                Block::Bullet {
                    text: "a".into(),
                    depth: 0
                },
                Block::Bullet {
                    text: "b".into(),
                    depth: 1
                },
                Block::Bullet {
                    text: "c".into(),
                    depth: 2
                },
            ]
        );
    }
}
