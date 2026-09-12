use std::ops::Range;

pub fn line_starts(content: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    starts
}

pub fn line_end(starts: &[usize], line: usize, content_len: usize) -> usize {
    starts
        .get(line + 1)
        .map(|start| start.saturating_sub(1))
        .unwrap_or(content_len)
}

pub fn line_index(starts: &[usize], offset: usize) -> usize {
    match starts.binary_search(&offset) {
        Ok(index) => index,
        Err(index) => index.saturating_sub(1),
    }
}

fn selected_line_span(text: &str, range: Range<usize>) -> (usize, usize) {
    let starts = line_starts(text);
    let start = range.start.min(text.len());
    let mut end = range.end.min(text.len());
    if end > start && text.as_bytes().get(end - 1) == Some(&b'\n') {
        end -= 1;
    }
    let start_line = line_index(&starts, start);
    let end_line = line_index(&starts, end);
    (start_line, end_line.max(start_line))
}

/// Wrap the selected range, or unwrap when the same markers already surround it.
/// Returns the new document and the next UTF-8 cursor range.
pub fn wrap_or_unwrap(
    text: &str,
    range: Range<usize>,
    prefix: &str,
    suffix: &str,
) -> (String, Range<usize>) {
    let start = range.start.min(text.len());
    let end = range.end.min(text.len()).max(start);
    if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return (text.to_owned(), start..end);
    }

    if start == end {
        if start >= prefix.len()
            && text.is_char_boundary(start - prefix.len())
            && &text[start - prefix.len()..start] == prefix
            && text[start..].starts_with(suffix)
        {
            let mut next = String::with_capacity(text.len() - prefix.len() - suffix.len());
            next.push_str(&text[..start - prefix.len()]);
            next.push_str(&text[start + suffix.len()..]);
            let cursor = start - prefix.len();
            return (next, cursor..cursor);
        }
        let mut next = String::with_capacity(text.len() + prefix.len() + suffix.len());
        next.push_str(&text[..start]);
        next.push_str(prefix);
        next.push_str(suffix);
        next.push_str(&text[start..]);
        let cursor = start + prefix.len();
        return (next, cursor..cursor);
    }

    let selected = &text[start..end];
    if selected.starts_with(prefix)
        && selected.ends_with(suffix)
        && selected.len() >= prefix.len() + suffix.len()
    {
        let inner = &selected[prefix.len()..selected.len() - suffix.len()];
        let mut next = String::with_capacity(text.len() - prefix.len() - suffix.len());
        next.push_str(&text[..start]);
        next.push_str(inner);
        next.push_str(&text[end..]);
        return (next, start..start + inner.len());
    }

    if start >= prefix.len()
        && text.is_char_boundary(start - prefix.len())
        && &text[start - prefix.len()..start] == prefix
        && text[end..].starts_with(suffix)
    {
        let mut next = String::with_capacity(text.len() - prefix.len() - suffix.len());
        next.push_str(&text[..start - prefix.len()]);
        next.push_str(selected);
        next.push_str(&text[end + suffix.len()..]);
        let next_start = start - prefix.len();
        return (next, next_start..next_start + selected.len());
    }

    let mut next = String::with_capacity(text.len() + prefix.len() + suffix.len());
    next.push_str(&text[..start]);
    next.push_str(prefix);
    next.push_str(selected);
    next.push_str(suffix);
    next.push_str(&text[end..]);
    (
        next,
        start..start + prefix.len() + selected.len() + suffix.len(),
    )
}

pub fn heading_level(line: &str) -> u8 {
    let bytes = line.as_bytes();
    let mut count = 0;
    while count < bytes.len() && count < 6 && bytes[count] == b'#' {
        count += 1;
    }
    if count == 0 {
        return 0;
    }
    if bytes.get(count) == Some(&b' ') || count == bytes.len() {
        count as u8
    } else {
        0
    }
}

fn heading_body(line: &str) -> &str {
    let level = heading_level(line);
    if level == 0 {
        line
    } else {
        line[level as usize..]
            .strip_prefix(' ')
            .unwrap_or(&line[level as usize..])
    }
}

pub fn set_heading_line(line: &str, cursor_offset: usize, level: u8) -> (String, usize) {
    let body = heading_body(line);
    let old_prefix = line.len() - body.len();
    let new_line = if level == 0 {
        body.to_owned()
    } else {
        format!("{} {body}", "#".repeat(level as usize))
    };
    let new_prefix = new_line.len() - body.len();
    let cursor = if cursor_offset <= old_prefix {
        new_prefix.min(new_line.len())
    } else {
        new_prefix + cursor_offset.saturating_sub(old_prefix)
    };
    (new_line.clone(), cursor.min(new_line.len()))
}

pub fn cycle_heading_line(line: &str, cursor_offset: usize) -> (String, usize) {
    let current = heading_level(line);
    let next = if current >= 6 { 0 } else { current + 1 };
    set_heading_line(line, cursor_offset, next)
}

/// Toggle list / quote prefixes. Matching prefix removes it; any other block
/// prefix is replaced so markers never stack.
pub fn toggle_block_prefix(line: &str, prefix: &str, cursor_offset: usize) -> (String, usize) {
    if prefix == "# " {
        return cycle_heading_line(line, cursor_offset);
    }
    if let Some(rest) = exact_prefix_rest(line, prefix) {
        return (rest.to_owned(), cursor_offset.saturating_sub(prefix.len()));
    }

    let (rest, removed) = strip_block_prefix(line);
    let new_line = format!("{prefix}{rest}");
    let cursor = if cursor_offset <= removed {
        prefix.len().min(new_line.len())
    } else {
        prefix.len() + (cursor_offset - removed)
    };
    (new_line, cursor)
}

pub fn exact_prefix_rest<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(prefix)?;
    if prefix == "- "
        && (rest.starts_with("[ ] ") || rest.starts_with("[x] ") || rest.starts_with("[X] "))
    {
        return None;
    }
    Some(rest)
}

pub fn strip_block_prefix(line: &str) -> (&str, usize) {
    if heading_level(line) > 0 {
        let body = heading_body(line);
        return (body, line.len() - body.len());
    }
    for prefix in ["- [ ] ", "- [x] ", "- [X] ", "- ", "* ", "+ ", "> "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return (rest, prefix.len());
        }
    }
    let bytes = line.as_bytes();
    let mut digits = 0;
    while digits < bytes.len() && bytes[digits].is_ascii_digit() {
        digits += 1;
    }
    if digits > 0
        && let Some(rest) = line[digits..]
            .strip_prefix(". ")
            .or_else(|| line[digits..].strip_prefix(") "))
    {
        return (rest, digits + 2);
    }
    (line, 0)
}

pub fn toggle_prefixes(text: &str, range: Range<usize>, prefix: &str) -> (String, usize) {
    let starts = line_starts(text);
    let (start_line, end_line) = selected_line_span(text, range.clone());
    if start_line == end_line {
        let line_start = starts[start_line];
        let line_stop = line_end(&starts, start_line, text.len());
        let line_text = &text[line_start..line_stop];
        let cursor = range.start.max(range.end).saturating_sub(line_start);
        let (new_line, cursor_delta) = toggle_block_prefix(line_text, prefix, cursor);
        let mut next = String::with_capacity(text.len() + new_line.len());
        next.push_str(&text[..line_start]);
        next.push_str(&new_line);
        next.push_str(&text[line_stop..]);
        return (next, line_start + cursor_delta);
    }

    let mut lines = Vec::new();
    let mut nonempty = 0;
    let mut matching = 0;
    for line in start_line..=end_line {
        let line_start = starts[line];
        let line_stop = line_end(&starts, line, text.len());
        let line_text = &text[line_start..line_stop];
        lines.push(line_text);
        if !line_text.trim().is_empty() {
            nonempty += 1;
            if exact_prefix_rest(line_text, prefix).is_some() {
                matching += 1;
            }
        }
    }
    let strip_all = nonempty > 0 && matching == nonempty;

    let replace_start = starts[start_line];
    let replace_end = line_end(&starts, end_line, text.len());
    let mut replacement = String::new();
    for (index, line_text) in lines.into_iter().enumerate() {
        if index > 0 {
            replacement.push('\n');
        }
        if line_text.trim().is_empty() {
            replacement.push_str(line_text);
            continue;
        }
        let new_line = if strip_all {
            exact_prefix_rest(line_text, prefix)
                .unwrap_or(line_text)
                .to_owned()
        } else if exact_prefix_rest(line_text, prefix).is_some() {
            line_text.to_owned()
        } else {
            toggle_block_prefix(line_text, prefix, prefix.len()).0
        };
        replacement.push_str(&new_line);
    }

    let mut next = String::with_capacity(text.len() + replacement.len());
    next.push_str(&text[..replace_start]);
    next.push_str(&replacement);
    next.push_str(&text[replace_end..]);
    (next, replace_start + replacement.len())
}

pub fn set_heading_level(text: &str, range: Range<usize>, level: u8) -> (String, usize) {
    let starts = line_starts(text);
    let (start_line, end_line) = selected_line_span(text, range);
    let replace_start = starts[start_line];
    let replace_end = line_end(&starts, end_line, text.len());
    let mut replacement = String::new();
    for line in start_line..=end_line {
        if line > start_line {
            replacement.push('\n');
        }
        let line_text = &text[starts[line]..line_end(&starts, line, text.len())];
        if line_text.trim().is_empty() {
            replacement.push_str(line_text);
            continue;
        }
        replacement.push_str(&set_heading_line(line_text, 0, level).0);
    }
    let mut next = String::with_capacity(text.len() + replacement.len());
    next.push_str(&text[..replace_start]);
    next.push_str(&replacement);
    next.push_str(&text[replace_end..]);
    (next, replace_start + replacement.len())
}

pub fn markdown_table(rows: usize, cols: usize) -> String {
    let rows = rows.clamp(1, 8);
    let cols = cols.clamp(1, 8);
    let header = (1..=cols)
        .map(|index| format!("列 {index}"))
        .collect::<Vec<_>>()
        .join(" | ");
    let divider = (0..cols).map(|_| "---").collect::<Vec<_>>().join(" | ");
    let mut lines = vec![format!("| {header} |"), format!("| {divider} |")];
    for _ in 0..rows {
        let row = (0..cols).map(|_| "内容").collect::<Vec<_>>().join(" | ");
        lines.push(format!("| {row} |"));
    }
    lines.join("\n")
}

pub fn markdown_link(text: &str, url: &str) -> String {
    let label = if text.trim().is_empty() {
        "链接"
    } else {
        text.trim()
    };
    let href = if url.trim().is_empty() {
        "https://example.com"
    } else {
        url.trim()
    };
    format!("[{label}]({href})")
}

pub fn markdown_code_block(language: &str) -> String {
    let language = language.trim();
    if language.is_empty() {
        "```\n\n```".to_owned()
    } else {
        format!("```{language}\n\n```")
    }
}

pub fn looks_like_url(value: &str) -> bool {
    let value = value.trim();
    value.starts_with("https://")
        || value.starts_with("http://")
        || value.starts_with("mailto:")
        || value.starts_with("www.")
}

pub fn normalize_url(value: &str) -> String {
    let value = value.trim();
    if value.starts_with("www.") {
        format!("https://{value}")
    } else {
        value.to_owned()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineItem {
    pub level: u8,
    pub title: String,
    pub offset: usize,
}

pub fn document_outline(text: &str) -> Vec<OutlineItem> {
    let mut items = Vec::new();
    let mut offset = 0;
    for line in text.split('\n') {
        let level = heading_level(line);
        if level > 0 {
            items.push(OutlineItem {
                level,
                title: heading_body(line).trim().to_owned(),
                offset,
            });
        }
        offset += line.len() + 1;
    }
    items
}

pub fn character_count(text: &str) -> usize {
    text.chars().filter(|ch| !ch.is_whitespace()).count()
}

#[allow(dead_code)]
pub fn selected_count(text: &str, range: Range<usize>) -> usize {
    let start = range.start.min(text.len());
    let end = range.end.min(text.len()).max(start);
    if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return 0;
    }
    character_count(&text[start..end])
}

pub fn is_clipboard_url(value: &str) -> bool {
    looks_like_url(value) && !value.contains(['\n', ' ', '\t'])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ActiveBlock {
    Paragraph,
    Heading(u8),
    Bullet,
    Task,
    Quote,
    Numbered,
}

#[allow(dead_code)]
pub fn active_block(line: &str) -> ActiveBlock {
    if let Some(level) = Some(heading_level(line)).filter(|level| *level > 0) {
        return ActiveBlock::Heading(level);
    }
    if line.starts_with("- [ ] ") || line.starts_with("- [x] ") || line.starts_with("- [X] ") {
        return ActiveBlock::Task;
    }
    if line.starts_with("- ") || line.starts_with("* ") || line.starts_with("+ ") {
        return ActiveBlock::Bullet;
    }
    if line.starts_with("> ") {
        return ActiveBlock::Quote;
    }
    let bytes = line.as_bytes();
    let mut digits = 0;
    while digits < bytes.len() && bytes[digits].is_ascii_digit() {
        digits += 1;
    }
    if digits > 0 && (line[digits..].starts_with(". ") || line[digits..].starts_with(") ")) {
        return ActiveBlock::Numbered;
    }
    ActiveBlock::Paragraph
}

#[allow(dead_code)]
pub fn selection_has_wrap(text: &str, range: Range<usize>, prefix: &str, suffix: &str) -> bool {
    let start = range.start.min(text.len());
    let end = range.end.min(text.len()).max(start);
    if start == end {
        return start >= prefix.len()
            && text.is_char_boundary(start - prefix.len())
            && &text[start - prefix.len()..start] == prefix
            && text[start..].starts_with(suffix);
    }
    if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return false;
    }
    let selected = &text[start..end];
    (selected.starts_with(prefix) && selected.ends_with(suffix))
        || (start >= prefix.len()
            && text.is_char_boundary(start - prefix.len())
            && &text[start - prefix.len()..start] == prefix
            && text[end..].starts_with(suffix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_and_unwraps_inline_markers() {
        let (wrapped, range) = wrap_or_unwrap("hello", 0..5, "**", "**");
        assert_eq!(wrapped, "**hello**");
        assert_eq!(range, 0..9);
        let (unwrapped, range) = wrap_or_unwrap(&wrapped, 0..9, "**", "**");
        assert_eq!(unwrapped, "hello");
        assert_eq!(range, 0..5);
        let (inside, range) = wrap_or_unwrap("text", 4..4, "**", "**");
        assert_eq!(inside, "text****");
        assert_eq!(range, 6..6);
        let (cleared, range) = wrap_or_unwrap("text****", 6..6, "**", "**");
        assert_eq!(cleared, "text");
        assert_eq!(range, 4..4);
    }

    #[test]
    fn cycles_heading_levels() {
        let (line, _) = cycle_heading_line("Title", 0);
        assert_eq!(line, "# Title");
        let (line, _) = cycle_heading_line("# Title", 0);
        assert_eq!(line, "## Title");
        let (line, _) = cycle_heading_line("###### Title", 0);
        assert_eq!(line, "Title");
    }

    #[test]
    fn toggles_prefixes_across_selected_lines() {
        let source = "one\ntwo\nthree";
        let (next, _) = toggle_prefixes(source, 0..source.len(), "- ");
        assert_eq!(next, "- one\n- two\n- three");
        let (next, _) = toggle_prefixes(&next, 0..next.len(), "- ");
        assert_eq!(next, "one\ntwo\nthree");
        let mixed = "- one\ntwo";
        let (next, _) = toggle_prefixes(mixed, 0..mixed.len(), "- ");
        assert_eq!(next, "- one\n- two");
    }

    #[test]
    fn does_not_stack_task_and_bullet_prefixes() {
        let (line, _) = toggle_block_prefix("- [ ] task", "- ", 0);
        assert_eq!(line, "- task");
        let (line, _) = toggle_block_prefix("> quote", "- ", 0);
        assert_eq!(line, "- quote");
    }

    #[test]
    fn builds_tables_and_counts_characters() {
        let table = markdown_table(2, 3);
        assert!(table.contains("| 列 1 | 列 2 | 列 3 |"));
        assert_eq!(table.lines().count(), 4);
        assert_eq!(character_count("hello 世界"), 7);
        assert!(looks_like_url("https://example.com"));
        assert_eq!(normalize_url("www.example.com"), "https://www.example.com");
        let outline = document_outline("# A\n\n## B\ntext");
        assert_eq!(outline.len(), 2);
        assert_eq!(outline[1].level, 2);
    }
}
