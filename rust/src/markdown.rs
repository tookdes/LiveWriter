#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Heading {
        level: u8,
        text: String,
    },
    Paragraph(String),
    Bullet(String),
    Numbered {
        marker: String,
        text: String,
    },
    Quote(String),
    Code(String),
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
}

pub fn parse_blocks(markdown: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut paragraph = Vec::new();
    let mut code = None::<Vec<String>>;
    let lines: Vec<&str> = markdown.lines().collect();
    let mut index = 0;

    let flush_paragraph = |blocks: &mut Vec<Block>, paragraph: &mut Vec<String>| {
        if !paragraph.is_empty() {
            blocks.push(Block::Paragraph(paragraph.join(" ")));
            paragraph.clear();
        }
    };

    while index < lines.len() {
        let line = lines[index];
        if let Some(code_lines) = code.as_mut() {
            if line.trim_start().starts_with("```") {
                blocks.push(Block::Code(std::mem::take(code_lines).join("\n")));
                code = None;
            } else {
                code_lines.push(line.to_owned());
            }
            index += 1;
            continue;
        }

        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            flush_paragraph(&mut blocks, &mut paragraph);
            code = Some(Vec::new());
        } else if let Some(headers) = table_row(trimmed)
            && index + 1 < lines.len()
            && is_table_separator(lines[index + 1])
        {
            flush_paragraph(&mut blocks, &mut paragraph);
            let width = headers.len();
            let mut rows = Vec::new();
            index += 2;
            while index < lines.len() {
                let Some(row) = table_row(lines[index].trim()) else {
                    break;
                };
                rows.push(normalize_row(row, width));
                index += 1;
            }
            blocks.push(Block::Table {
                headers: normalize_row(headers, width),
                rows,
            });
            continue;
        } else if let Some((alt, url)) = image(trimmed) {
            flush_paragraph(&mut blocks, &mut paragraph);
            blocks.push(Block::Image { alt, url });
        } else if let Some(url) = video(trimmed) {
            flush_paragraph(&mut blocks, &mut paragraph);
            blocks.push(Block::Video { url });
        } else if trimmed.is_empty() {
            flush_paragraph(&mut blocks, &mut paragraph);
        } else if trimmed == "---" || trimmed == "***" {
            flush_paragraph(&mut blocks, &mut paragraph);
            blocks.push(Block::Divider);
        } else if let Some((level, text)) = heading(trimmed) {
            flush_paragraph(&mut blocks, &mut paragraph);
            blocks.push(Block::Heading { level, text });
        } else if let Some(text) = trimmed
            .strip_prefix("> ")
            .or_else(|| trimmed.strip_prefix(">"))
        {
            flush_paragraph(&mut blocks, &mut paragraph);
            blocks.push(Block::Quote(text.trim().to_owned()));
        } else if let Some(text) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            flush_paragraph(&mut blocks, &mut paragraph);
            blocks.push(Block::Bullet(text.to_owned()));
        } else if let Some((marker, text)) = trimmed.split_once(". ").filter(|(prefix, _)| {
            !prefix.is_empty() && prefix.chars().all(|character| character.is_ascii_digit())
        }) {
            flush_paragraph(&mut blocks, &mut paragraph);
            blocks.push(Block::Numbered {
                marker: marker.to_owned(),
                text: text.to_owned(),
            });
        } else {
            paragraph.push(trimmed.to_owned());
        }
        index += 1;
    }

    if let Some(code_lines) = code {
        blocks.push(Block::Code(code_lines.join("\n")));
    }
    flush_paragraph(&mut blocks, &mut paragraph);
    blocks
}

pub fn strip_inline(markdown: &str) -> String {
    let mut text = markdown.to_owned();
    while let Some(start) = text.find('[') {
        let Some(end) = text[start..].find("](").map(|offset| start + offset) else {
            break;
        };
        let Some(close) = text[end + 2..].find(')').map(|offset| end + 2 + offset) else {
            break;
        };
        let label = text[start + 1..end].to_owned();
        let replace_start = if start > 0 && text.as_bytes()[start - 1] == b'!' {
            start - 1
        } else {
            start
        };
        text.replace_range(replace_start..=close, &label);
    }
    for marker in ["**", "__", "~~", "*", "_", "`"] {
        text = text.replace(marker, "");
    }
    text
}

fn image(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("![")?;
    let label_end = rest.find("](")?;
    let url = rest[label_end + 2..].strip_suffix(')')?.trim();
    (!url.is_empty()).then(|| (rest[..label_end].to_owned(), url.to_owned()))
}

fn video(line: &str) -> Option<String> {
    let rest = line.strip_prefix('[')?;
    let label_end = rest.find("](")?;
    let label = rest[..label_end].trim();
    if !label.eq_ignore_ascii_case("视频") && !label.eq_ignore_ascii_case("video") {
        return None;
    }
    let url = rest[label_end + 2..].strip_suffix(')')?.trim();
    (!url.is_empty()).then(|| url.to_owned())
}

fn table_row(line: &str) -> Option<Vec<String>> {
    if !line.contains('|') {
        return None;
    }
    let line = line.strip_prefix('|').unwrap_or(line);
    let line = line.strip_suffix('|').unwrap_or(line);
    let cells: Vec<String> = line.split('|').map(|cell| cell.trim().to_owned()).collect();
    (!cells.is_empty() && cells.iter().any(|cell| !cell.is_empty())).then_some(cells)
}

fn is_table_separator(line: &str) -> bool {
    let Some(cells) = table_row(line.trim()) else {
        return false;
    };
    cells.iter().all(|cell| {
        let cell = cell.trim_matches(':').trim();
        cell.len() >= 3 && cell.chars().all(|character| character == '-')
    })
}

fn normalize_row(mut row: Vec<String>, width: usize) -> Vec<String> {
    row.truncate(width);
    row.resize(width, String::new());
    row
}

fn heading(line: &str) -> Option<(u8, String)> {
    let marker_len = line
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if (1..=6).contains(&marker_len) && line.chars().nth(marker_len) == Some(' ') {
        Some((marker_len as u8, line[marker_len + 1..].trim().to_owned()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{Block, parse_blocks, strip_inline};

    #[test]
    fn parses_the_blocks_used_by_the_preview() {
        assert_eq!(
            parse_blocks("# 标题\n\n- 一\n1. 二\n> 三\n\n```\nlet x = 1;\n```"),
            vec![
                Block::Heading {
                    level: 1,
                    text: "标题".into()
                },
                Block::Bullet("一".into()),
                Block::Numbered {
                    marker: "1".into(),
                    text: "二".into(),
                },
                Block::Quote("三".into()),
                Block::Code("let x = 1;".into()),
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
    fn strips_common_inline_markdown() {
        assert_eq!(
            strip_inline("**粗体** [链接](https://example.com)"),
            "粗体 链接"
        );
        assert_eq!(strip_inline("![图片](https://example.com/a.png)"), "图片");
    }
}
