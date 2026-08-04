mod markdown;
mod publishing;
mod storage;

use std::{
    borrow::Cow,
    fs,
    ops::Range,
    path::{Path, PathBuf},
};

use gpui::{
    App, Application, AssetSource, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, FocusHandle, Focusable, FontWeight,
    GlobalElementId, KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PaintQuad, PathPromptOptions, Pixels, Point, PromptLevel, ShapedLine,
    SharedString, Style, TextRun, UTF16Selection, Window, WindowBounds, WindowOptions, actions,
    div, fill, hsla, img, point, prelude::*, px, relative, rgb, size, white,
};
use rust_embed::Embed;
use unicode_segmentation::UnicodeSegmentation;

use crate::markdown::{Block, parse_blocks};
use crate::publishing::{
    CREDENTIALS_URL, CREDENTIALS_USERNAME, NotionConfig, StoredPublishSettings, TypechoConfig,
    document_title, publish_to_notion as publish_notion_request,
    publish_to_typecho as publish_typecho_request,
};

const DEFAULT_MARKDOWN: &str = "# 欢迎回来，Open Live Writer\n\n这是一个保持怀旧外观的 Markdown 编辑器。你可以直接编辑左侧内容，然后切换到预览。\n\n- 使用工具栏快速插入 Markdown\n- 点击“复制到 Notion”复制可粘贴的 Markdown\n- 文件使用 UTF-8 的 md 格式保存\n\n> 先写作，再发布。\n";

const BLUE: u32 = 0x3b78b4;
const DARK_BLUE: u32 = 0x2b5f95;
const RIBBON_BLUE: u32 = 0xe6eff9;
const WORKSPACE: u32 = 0xe9edf2;
const BORDER: u32 = 0xc6ced8;
const TEXT: u32 = 0x263746;
const MUTED: u32 = 0x617285;
const INLINE_CODE_MARKER: &str = "\x60";

actions!(
    open_live_writer,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectAll,
        Home,
        End,
        PasteText,
        CutText,
        CopyText,
        NewDocument,
        OpenDocument,
        OpenDraft,
        SaveDocument,
        SaveDraft,
        TogglePreview,
        BoldText,
        ItalicText,
        HeadingText,
        BulletsText,
        QuoteText,
        LinkText,
        StrikeText,
        CodeText,
        ImageText,
        TableText,
        VideoText,
        DividerText,
        UndoText,
        RedoText,
        ToggleSettings,
        PublishToNotion,
        PublishToTypecho,
        QuitApplication,
    ]
);

#[derive(Embed)]
#[folder = "assets/"]
struct EmbeddedAssets;

struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(EmbeddedAssets::get(path).map(|asset| Cow::Owned(asset.data.into_owned())))
    }

    fn list(&self, _path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(EmbeddedAssets::iter().map(SharedString::from).collect())
    }
}

#[derive(Clone)]
struct EditorLayout {
    lines: Vec<ShapedLine>,
    starts: Vec<usize>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
}

#[derive(Clone)]
struct EditSnapshot {
    content: String,
    selected_range: Range<usize>,
    selection_reversed: bool,
}

struct MarkdownInput {
    focus_handle: FocusHandle,
    pub content: SharedString,
    masked: bool,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<EditorLayout>,
    is_selecting: bool,
    // ponytail: full-text snapshots keep the first slice simple; switch to edit deltas for very long documents.
    undo_stack: Vec<EditSnapshot>,
    redo_stack: Vec<EditSnapshot>,
}

impl MarkdownInput {
    fn set_content(&mut self, content: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.content = content.into();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.last_layout = None;
        self.is_selecting = false;
        self.undo_stack.clear();
        self.redo_stack.clear();
        cx.notify();
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn replace_range(&mut self, range: Range<usize>, replacement: &str, cx: &mut Context<Self>) {
        self.replace_range_with_history(range, replacement, true, cx);
    }

    fn replace_range_with_history(
        &mut self,
        range: Range<usize>,
        replacement: &str,
        record_undo: bool,
        cx: &mut Context<Self>,
    ) {
        if record_undo {
            self.undo_stack.push(self.snapshot());
            self.redo_stack.clear();
        }
        let mut content = self.content.to_string();
        content.replace_range(range.clone(), replacement);
        let cursor = range.start + replacement.len();
        self.content = content.into();
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
        self.last_layout = None;
        cx.notify();
    }

    fn snapshot(&self) -> EditSnapshot {
        EditSnapshot {
            content: self.content.to_string(),
            selected_range: self.selected_range.clone(),
            selection_reversed: self.selection_reversed,
        }
    }

    fn restore(&mut self, snapshot: EditSnapshot) {
        self.content = snapshot.content.into();
        self.selected_range = snapshot.selected_range;
        self.selection_reversed = snapshot.selection_reversed;
        self.marked_range = None;
        self.last_layout = None;
    }

    fn undo(&mut self, _: &UndoText, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(previous) = self.undo_stack.pop() else {
            return;
        };
        self.redo_stack.push(self.snapshot());
        self.restore(previous);
        cx.notify();
    }

    fn redo(&mut self, _: &RedoText, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(next) = self.redo_stack.pop() else {
            return;
        };
        self.undo_stack.push(self.snapshot());
        self.restore(next);
        cx.notify();
    }

    fn replace_selection(&mut self, replacement: &str, cx: &mut Context<Self>) {
        self.replace_range(self.selected_range.clone(), replacement, cx);
    }

    fn selected_text(&self) -> String {
        self.content[self.selected_range.clone()].to_owned()
    }

    fn wrap_selection(&mut self, prefix: &str, suffix: &str, cx: &mut Context<Self>) {
        let selected = self.selected_text();
        let replacement = format!("{prefix}{selected}{suffix}");
        let start = self.selected_range.start;
        self.replace_range(self.selected_range.clone(), &replacement, cx);
        self.selected_range = start..start + replacement.len();
        self.selection_reversed = false;
        cx.notify();
    }

    fn prefix_current_line(&mut self, prefix: &str, cx: &mut Context<Self>) {
        let (line_start, _) = self.line_range(self.cursor_offset());
        self.replace_range(line_start..line_start, prefix, cx);
    }

    fn line_starts(&self) -> Vec<usize> {
        line_starts(self.content.as_ref())
    }

    fn line_range(&self, offset: usize) -> (usize, usize) {
        let starts = self.line_starts();
        let line = line_for_offset(&starts, offset);
        let start = starts[line];
        let end = line_end(&starts, line, self.content.len());
        (start, end.max(start))
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = offset.min(self.content.len());
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = offset.min(self.content.len());
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify();
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(index, _)| (index < offset).then_some(index))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(index, _)| (index > offset).then_some(index))
            .unwrap_or(self.content.len())
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        self.content
            .char_indices()
            .take_while(|(index, _)| *index < offset)
            .map(|(_, character)| character.len_utf16())
            .sum()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        utf8_offset_from_utf16(self.content.as_ref(), offset)
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    fn move_vertical(&mut self, delta: isize, select: bool, cx: &mut Context<Self>) {
        let starts = self.line_starts();
        let cursor = self.cursor_offset();
        let line = line_for_offset(&starts, cursor);
        let column = cursor.saturating_sub(starts[line]);
        let target = (line as isize + delta).clamp(0, starts.len() as isize - 1) as usize;
        let target_end = line_end(&starts, target, self.content.len());
        let target_offset = (starts[target] + column).min(target_end);
        if select {
            self.select_to(target_offset, cx);
        } else {
            self.move_to(target_offset, cx);
        }
    }

    fn backspace(&mut self, _: &Backspace, _window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let end = self.cursor_offset();
            self.selected_range = self.previous_boundary(end)..end;
        }
        self.replace_selection("", cx);
    }

    fn delete(&mut self, _: &Delete, _window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let start = self.cursor_offset();
            self.selected_range = start..self.next_boundary(start);
        }
        self.replace_selection("", cx);
    }

    fn left(&mut self, _: &Left, _window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    fn up(&mut self, _: &Up, _window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertical(-1, false, cx);
    }

    fn down(&mut self, _: &Down, _window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertical(1, false, cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_up(&mut self, _: &SelectUp, _window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertical(-1, true, cx);
    }

    fn select_down(&mut self, _: &SelectDown, _window: &mut Window, cx: &mut Context<Self>) {
        self.move_vertical(1, true, cx);
    }

    fn select_all(&mut self, _: &SelectAll, _window: &mut Window, cx: &mut Context<Self>) {
        self.selected_range = 0..self.content.len();
        self.selection_reversed = false;
        cx.notify();
    }

    fn home(&mut self, _: &Home, _window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_range(self.cursor_offset()).0, cx);
    }

    fn end(&mut self, _: &End, _window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_range(self.cursor_offset()).1, cx);
    }

    fn paste(&mut self, _: &PasteText, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_selection(&text, cx);
        }
    }

    fn copy(&mut self, _: &CopyText, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.selected_text()));
        }
    }

    fn cut(&mut self, _: &CutText, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.selected_text()));
            self.replace_selection("", cx);
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.is_selecting = true;
        let offset = self.index_for_mouse_position(event.position);
        if event.modifiers.shift {
            self.select_to(offset, cx);
        } else {
            self.move_to(offset, cx);
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        let Some(layout) = &self.last_layout else {
            return 0;
        };
        let relative_y = position.y - layout.bounds.top();
        let line = if relative_y <= px(0.) {
            0
        } else {
            ((relative_y / layout.line_height).floor() as usize).min(layout.lines.len() - 1)
        };
        let x = (position.x - layout.bounds.left()).max(px(0.));
        let byte_offset = layout.lines[line].closest_index_for_x(x);
        let line_end = line_end(&layout.starts, line, self.content.len());
        (layout.starts[line] + byte_offset).min(line_end)
    }
}

impl EntityInputHandler for MarkdownInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or_else(|| self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.replace_range_with_history(range, new_text, self.marked_range.is_none(), cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or_else(|| self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let start = range.start;
        let record_undo = self.marked_range.is_none();
        self.replace_range_with_history(range, new_text, record_undo, cx);
        self.marked_range = (!new_text.is_empty()).then_some(start..start + new_text.len());
        if let Some(selected) = new_selected_range_utf16 {
            let selected = utf16_range_to_utf8(new_text, &selected);
            self.selected_range = start + selected.start..start + selected.end;
        }
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        let line = line_for_offset(&layout.starts, range.start);
        let start = layout.starts[line];
        let x_start = layout.lines[line].x_for_index(range.start.saturating_sub(start));
        let x_end = layout.lines[line].x_for_index(range.end.saturating_sub(start));
        Some(Bounds::from_corners(
            point(
                layout.bounds.left() + x_start,
                layout.bounds.top() + layout.line_height * line,
            ),
            point(
                layout.bounds.left() + x_end,
                layout.bounds.top() + layout.line_height * (line + 1),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.offset_to_utf16(self.index_for_mouse_position(point)))
    }
}

struct MarkdownTextElement {
    input: Entity<MarkdownInput>,
}

struct MarkdownPrepaint {
    lines: Vec<ShapedLine>,
    starts: Vec<usize>,
    selections: Vec<PaintQuad>,
    cursor: Option<PaintQuad>,
}

impl IntoElement for MarkdownTextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for MarkdownTextElement {
    type RequestLayoutState = ();
    type PrepaintState = MarkdownPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let line_count = self.input.read(cx).content.split('\n').count().max(1);
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = (line_count * window.line_height()).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let starts = line_starts(content.as_ref());
        let line_height = window.line_height();
        let style = window.text_style();
        let color = style.color;
        let font_size = style.font_size.to_pixels(window.rem_size());
        let display_lines: Vec<SharedString> = if content.is_empty() {
            vec!["开始输入 Markdown…".into()]
        } else {
            content
                .split('\n')
                .map(|line| {
                    if input.masked {
                        SharedString::from("*".repeat(line.len()))
                    } else {
                        SharedString::from(line.to_owned())
                    }
                })
                .collect()
        };
        let placeholder = content.is_empty();
        let lines: Vec<ShapedLine> = display_lines
            .into_iter()
            .map(|line| {
                let text_color = if placeholder {
                    color.opacity(0.45)
                } else {
                    color
                };
                window.text_system().shape_line(
                    line.clone(),
                    font_size,
                    &[TextRun {
                        len: line.len(),
                        font: style.font(),
                        color: text_color,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }],
                    None,
                )
            })
            .collect();

        let mut selections = Vec::new();
        if !placeholder && !input.selected_range.is_empty() {
            for (line_index, line) in lines.iter().enumerate() {
                let start = starts[line_index];
                let end = line_end(&starts, line_index, content.len());
                let selected_start = input.selected_range.start.max(start);
                let selected_end = input.selected_range.end.min(end);
                if selected_start < selected_end {
                    selections.push(fill(
                        Bounds::from_corners(
                            point(
                                bounds.left() + line.x_for_index(selected_start - start),
                                bounds.top() + line_height * line_index,
                            ),
                            point(
                                bounds.left() + line.x_for_index(selected_end - start),
                                bounds.top() + line_height * (line_index + 1),
                            ),
                        ),
                        hsla(0.58, 0.65, 0.75, 0.35),
                    ));
                }
            }
        }

        let cursor = if input.selected_range.is_empty() {
            let cursor = input.cursor_offset();
            let line_index = line_for_offset(&starts, cursor).min(lines.len() - 1);
            let start = starts.get(line_index).copied().unwrap_or(0);
            Some(fill(
                Bounds::new(
                    point(
                        bounds.left() + lines[line_index].x_for_index(cursor.saturating_sub(start)),
                        bounds.top() + line_height * line_index,
                    ),
                    size(px(1.5), line_height),
                ),
                rgb(BLUE),
            ))
        } else {
            None
        };

        MarkdownPrepaint {
            lines,
            starts,
            selections,
            cursor,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        for selection in prepaint.selections.drain(..) {
            window.paint_quad(selection);
        }
        let line_height = window.line_height();
        for (line_index, line) in prepaint.lines.iter().enumerate() {
            let origin = point(bounds.left(), bounds.top() + line_height * line_index);
            let _ = line.paint(origin, line_height, window, cx);
        }
        if focus_handle.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }
        self.input.update(cx, |input, _| {
            input.last_layout = Some(EditorLayout {
                lines: prepaint.lines.clone(),
                starts: prepaint.starts.clone(),
                bounds,
                line_height,
            });
        });
    }
}

impl Render for MarkdownInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .w_full()
            .key_context("MarkdownInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .text_size(px(16.))
            .line_height(px(28.))
            .text_color(rgb(TEXT))
            .child(MarkdownTextElement { input: cx.entity() })
    }
}

impl Focusable for MarkdownInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

struct MarkdownEditor {
    text_input: Entity<MarkdownInput>,
    notion_token_input: Entity<MarkdownInput>,
    notion_parent_input: Entity<MarkdownInput>,
    typecho_url_input: Entity<MarkdownInput>,
    typecho_username_input: Entity<MarkdownInput>,
    typecho_password_input: Entity<MarkdownInput>,
    path: Option<PathBuf>,
    preview: bool,
    active_tab: usize,
    settings_visible: bool,
    dirty: bool,
    status: SharedString,
    publish_settings: StoredPublishSettings,
    last_observed_content: String,
    suppress_observer: bool,
    close_confirmed: bool,
    _content_subscription: gpui::Subscription,
}

#[derive(Copy, Clone)]
enum PendingOperation {
    New,
    Open,
    OpenDraft,
}

impl MarkdownEditor {
    fn new_settings_input(
        cx: &mut Context<Self>,
        content: String,
        masked: bool,
    ) -> Entity<MarkdownInput> {
        cx.new(|cx| MarkdownInput {
            focus_handle: cx.focus_handle(),
            content: content.into(),
            masked,
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            is_selecting: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        })
    }

    fn new(cx: &mut Context<Self>) -> Self {
        let initial = DEFAULT_MARKDOWN.to_owned();
        let text_input = cx.new(|cx| MarkdownInput {
            focus_handle: cx.focus_handle(),
            content: initial.clone().into(),
            masked: false,
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            is_selecting: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        });
        let publish_settings = StoredPublishSettings {
            notion_token: std::env::var("OPEN_LIVE_WRITER_NOTION_TOKEN").unwrap_or_default(),
            notion_parent_page_id: std::env::var("OPEN_LIVE_WRITER_NOTION_PARENT_PAGE_ID")
                .unwrap_or_default(),
            typecho_xmlrpc_url: std::env::var("OPEN_LIVE_WRITER_TYPECHO_XMLRPC_URL")
                .unwrap_or_default(),
            typecho_username: std::env::var("OPEN_LIVE_WRITER_TYPECHO_USERNAME")
                .unwrap_or_default(),
            typecho_password: std::env::var("OPEN_LIVE_WRITER_TYPECHO_PASSWORD")
                .unwrap_or_default(),
        };
        let notion_token_input =
            Self::new_settings_input(cx, publish_settings.notion_token.clone(), true);
        let notion_parent_input =
            Self::new_settings_input(cx, publish_settings.notion_parent_page_id.clone(), false);
        let typecho_url_input =
            Self::new_settings_input(cx, publish_settings.typecho_xmlrpc_url.clone(), false);
        let typecho_username_input =
            Self::new_settings_input(cx, publish_settings.typecho_username.clone(), false);
        let typecho_password_input =
            Self::new_settings_input(cx, publish_settings.typecho_password.clone(), true);
        let subscription = cx.observe(&text_input, |editor, input, cx| {
            if editor.suppress_observer {
                return;
            }
            let content = input.read(cx).content.to_string();
            if content != editor.last_observed_content {
                editor.last_observed_content = content;
                editor.dirty = true;
                editor.status = "正在编辑 · 尚未保存".into();
                cx.notify();
            }
        });
        let editor = Self {
            text_input,
            notion_token_input,
            notion_parent_input,
            typecho_url_input,
            typecho_username_input,
            typecho_password_input,
            path: None,
            preview: false,
            active_tab: 0,
            settings_visible: false,
            dirty: false,
            status: "就绪 · Markdown 模式".into(),
            last_observed_content: initial,
            suppress_observer: false,
            close_confirmed: false,
            publish_settings,
            _content_subscription: subscription,
        };
        let credentials = cx.read_credentials(CREDENTIALS_URL);
        cx.spawn(async move |view, cx| {
            if let Ok(Some((_, bytes))) = credentials.await
                && let Ok(settings) = serde_json::from_slice::<StoredPublishSettings>(&bytes)
            {
                let _ = view.update(cx, |editor, cx| editor.load_publish_settings(settings, cx));
            }
        })
        .detach();
        editor
    }

    fn load_publish_settings(&mut self, settings: StoredPublishSettings, cx: &mut Context<Self>) {
        self.publish_settings = settings.clone();
        let values = [
            (&self.notion_token_input, settings.notion_token),
            (&self.notion_parent_input, settings.notion_parent_page_id),
            (&self.typecho_url_input, settings.typecho_xmlrpc_url),
            (&self.typecho_username_input, settings.typecho_username),
            (&self.typecho_password_input, settings.typecho_password),
        ];
        for (input, value) in values {
            input.update(cx, |input, cx| input.set_content(value, cx));
        }
        cx.notify();
    }

    fn content(&self, cx: &App) -> String {
        self.text_input.read(cx).content.to_string()
    }

    fn toggle_settings(
        &mut self,
        _: &ToggleSettings,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_visible = !self.settings_visible;
        self.status = if self.settings_visible {
            "发布设置 · 凭据使用系统安全存储".into()
        } else {
            "就绪 · Markdown 模式".into()
        };
        cx.notify();
    }

    fn save_publish_settings(
        &mut self,
        _: &gpui::ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let settings = StoredPublishSettings {
            notion_token: self.notion_token_input.read(cx).content.to_string(),
            notion_parent_page_id: self.notion_parent_input.read(cx).content.to_string(),
            typecho_xmlrpc_url: self.typecho_url_input.read(cx).content.to_string(),
            typecho_username: self.typecho_username_input.read(cx).content.to_string(),
            typecho_password: self.typecho_password_input.read(cx).content.to_string(),
        };
        let Ok(bytes) = serde_json::to_vec(&settings) else {
            self.status = "发布设置序列化失败".into();
            cx.notify();
            return;
        };
        self.publish_settings = settings;
        let task = cx.write_credentials(CREDENTIALS_URL, CREDENTIALS_USERNAME, &bytes);
        self.status = "正在保存发布设置…".into();
        cx.notify();
        cx.spawn(async move |editor, cx| match task.await {
            Ok(()) => {
                let _ = editor.update(cx, |editor, cx| {
                    editor.status = "发布设置已保存到系统凭据存储".into();
                    cx.notify();
                });
            }
            Err(error) => {
                let _ = editor.update(cx, |editor, cx| {
                    editor.status = format!("发布设置保存失败：{error}").into();
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn replace_document(&mut self, path: Option<PathBuf>, content: String, cx: &mut Context<Self>) {
        self.suppress_observer = true;
        self.text_input
            .update(cx, |input, cx| input.set_content(content.clone(), cx));
        self.suppress_observer = false;
        self.last_observed_content = content;
        self.path = path;
        self.dirty = false;
        self.close_confirmed = false;
        self.status = "已加载 · UTF-8 Markdown".into();
        cx.notify();
    }

    fn new_document(&mut self, _: &NewDocument, window: &mut Window, cx: &mut Context<Self>) {
        if self.dirty {
            self.confirm_discard(PendingOperation::New, window, cx);
            return;
        }
        self.new_document_now(cx);
    }

    fn new_document_now(&mut self, cx: &mut Context<Self>) {
        self.replace_document(None, "# 未命名文章\n\n".to_owned(), cx);
        self.status = "新文章 · 尚未保存".into();
    }

    fn new_document_click(
        &mut self,
        _: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.new_document(&NewDocument, window, cx);
    }

    fn save_draft(&mut self, _: &SaveDraft, _window: &mut Window, cx: &mut Context<Self>) {
        match storage::save_draft(&self.content(cx)) {
            Ok(path) => self.status = format!("草稿已保存 · {}", display_path(&path)).into(),
            Err(error) => self.status = format!("草稿保存失败：{error}").into(),
        }
        cx.notify();
    }

    fn save_draft_click(
        &mut self,
        _: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.save_draft(&SaveDraft, window, cx);
    }

    fn open_draft(&mut self, _: &OpenDraft, window: &mut Window, cx: &mut Context<Self>) {
        if self.dirty {
            self.confirm_discard(PendingOperation::OpenDraft, window, cx);
            return;
        }
        self.open_draft_now(cx);
    }

    fn open_draft_now(&mut self, cx: &mut Context<Self>) {
        match storage::load_draft() {
            Ok(Some((path, content))) => {
                self.replace_document(None, normalize_newlines(content), cx);
                self.status = format!("已打开草稿 · {}", display_path(&path)).into();
            }
            Ok(None) => self.status = "暂无本地草稿".into(),
            Err(error) => self.status = format!("草稿打开失败：{error}").into(),
        }
        cx.notify();
    }

    fn open_draft_click(
        &mut self,
        _: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_draft(&OpenDraft, window, cx);
    }

    fn confirm_discard(
        &mut self,
        operation: PendingOperation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let answer = window.prompt(
            PromptLevel::Warning,
            "当前文章有未保存的修改",
            Some("请选择“放弃修改并继续”，或先保存后取消。"),
            &["放弃修改并继续", "取消"],
            cx,
        );
        cx.spawn(async move |editor, cx| {
            if answer.await.ok() != Some(0) {
                return;
            }
            let _ = editor.update(cx, |editor, cx| match operation {
                PendingOperation::New => editor.new_document_now(cx),
                PendingOperation::Open => editor.begin_open(cx),
                PendingOperation::OpenDraft => editor.open_draft_now(cx),
            });
        })
        .detach();
    }

    fn open_document(&mut self, _: &OpenDocument, window: &mut Window, cx: &mut Context<Self>) {
        if self.dirty {
            self.confirm_discard(PendingOperation::Open, window, cx);
            return;
        }
        self.begin_open(cx);
    }

    fn begin_open(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("打开 Markdown 文件".into()),
        });
        cx.spawn(async move |editor, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            match fs::read_to_string(&path) {
                Ok(content) => {
                    let _ = editor.update(cx, |editor, cx| {
                        editor.replace_document(Some(path), normalize_newlines(content), cx);
                    });
                }
                Err(error) => {
                    let _ = editor.update(cx, |editor, cx| {
                        editor.status = format!("打开失败：{error}").into();
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    fn open_document_click(
        &mut self,
        _: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_document(&OpenDocument, window, cx);
    }

    fn save_to(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let content = self.content(cx);
        match fs::write(&path, content) {
            Ok(()) => {
                self.path = Some(path.clone());
                self.dirty = false;
                self.last_observed_content = self.content(cx);
                self.status = format!("已保存 · {}", display_path(&path)).into();
            }
            Err(error) => self.status = format!("保存失败：{error}").into(),
        }
        cx.notify();
    }

    fn save_document(&mut self, _: &SaveDocument, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.path.clone() {
            self.save_to(path, cx);
            return;
        }
        let directory = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let receiver = cx.prompt_for_new_path(&directory, Some("未命名文章.md"));
        cx.spawn(async move |editor, cx| {
            let Ok(Ok(Some(path))) = receiver.await else {
                return;
            };
            let _ = editor.update(cx, |editor, cx| editor.save_to(path, cx));
        })
        .detach();
    }

    fn save_document_click(
        &mut self,
        _: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.save_document(&SaveDocument, window, cx);
    }

    fn toggle_preview(&mut self, _: &TogglePreview, _window: &mut Window, cx: &mut Context<Self>) {
        self.preview = !self.preview;
        self.status = if self.preview {
            "预览模式 · Markdown 已渲染".into()
        } else {
            "编辑模式 · Markdown 源文".into()
        };
        cx.notify();
    }

    fn toggle_preview_click(
        &mut self,
        _: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_preview(&TogglePreview, window, cx);
    }

    fn quit_application(
        &mut self,
        _: &QuitApplication,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.dirty {
            cx.quit();
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            "当前文章有未保存的修改",
            Some("退出后这些修改将丢失。"),
            &["放弃并退出", "取消"],
            cx,
        );
        cx.spawn(async move |editor, cx| {
            if answer.await.ok() == Some(0) {
                let _ = editor.update(cx, |editor, cx| {
                    editor.close_confirmed = true;
                    cx.notify();
                });
                let _ = cx.update(|app| app.quit());
            }
        })
        .detach();
    }

    fn publish_to_notion(
        &mut self,
        _: &PublishToNotion,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let markdown = self.content(cx);
        let title = document_title(&markdown);
        if let Some(config) =
            NotionConfig::from_env().or_else(|| self.publish_settings.notion_config())
        {
            let http = cx.http_client();
            self.status = "正在发布到 Notion…".into();
            cx.notify();
            cx.spawn(async move |editor, cx| {
                let result = publish_notion_request(http, config, &title, &markdown).await;
                let _ = editor.update(cx, |editor, cx| {
                    editor.status = match result {
                        Ok(_) => "已发布到 Notion".into(),
                        Err(error) => format!("Notion 发布失败：{error}").into(),
                    };
                    cx.notify();
                });
            })
            .detach();
        } else {
            cx.write_to_clipboard(ClipboardItem::new_string(markdown));
            self.status = "未配置 Notion，Markdown 已复制 · 粘贴即可发布".into();
            cx.notify();
        }
    }

    fn publish_to_notion_click(
        &mut self,
        _: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.publish_to_notion(&PublishToNotion, window, cx);
    }

    fn publish_to_typecho(
        &mut self,
        _: &PublishToTypecho,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let markdown = self.content(cx);
        let title = document_title(&markdown);
        if let Some(config) =
            TypechoConfig::from_env().or_else(|| self.publish_settings.typecho_config())
        {
            let http = cx.http_client();
            self.status = "正在发布到 Typecho…".into();
            cx.notify();
            cx.spawn(async move |editor, cx| {
                let result = publish_typecho_request(http, config, &title, &markdown).await;
                let _ = editor.update(cx, |editor, cx| {
                    editor.status = match result {
                        Ok(_) => "已提交到 Typecho".into(),
                        Err(error) => format!("Typecho 发布失败：{error}").into(),
                    };
                    cx.notify();
                });
            })
            .detach();
        } else {
            cx.write_to_clipboard(ClipboardItem::new_string(markdown));
            self.status = "未配置 Typecho，Markdown 已复制".into();
            cx.notify();
        }
    }

    fn publish_to_typecho_click(
        &mut self,
        _: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.publish_to_typecho(&PublishToTypecho, window, cx);
    }

    fn apply_format(&mut self, prefix: &str, suffix: &str, message: &str, cx: &mut Context<Self>) {
        self.text_input
            .update(cx, |input, cx| input.wrap_selection(prefix, suffix, cx));
        self.status = message.to_owned().into();
        cx.notify();
    }

    fn apply_prefix(&mut self, prefix: &str, message: &str, cx: &mut Context<Self>) {
        self.text_input
            .update(cx, |input, cx| input.prefix_current_line(prefix, cx));
        self.status = message.to_owned().into();
        cx.notify();
    }

    fn bold_action(&mut self, _: &BoldText, _window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format("**", "**", "已插入粗体 Markdown", cx);
    }

    fn italic_action(&mut self, _: &ItalicText, _window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format("*", "*", "已插入斜体 Markdown", cx);
    }

    fn heading_action(&mut self, _: &HeadingText, _window: &mut Window, cx: &mut Context<Self>) {
        self.apply_prefix("# ", "已插入一级标题", cx);
    }

    fn bullets_action(&mut self, _: &BulletsText, _window: &mut Window, cx: &mut Context<Self>) {
        self.apply_prefix("- ", "已插入无序列表", cx);
    }

    fn quote_action(&mut self, _: &QuoteText, _window: &mut Window, cx: &mut Context<Self>) {
        self.apply_prefix("> ", "已插入引用", cx);
    }

    fn link_action(&mut self, _: &LinkText, _window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format("[", "](https://example.com)", "已插入链接 Markdown", cx);
    }

    fn insert_snippet(&mut self, snippet: String, message: &'static str, cx: &mut Context<Self>) {
        self.text_input
            .update(cx, |input, cx| input.replace_selection(&snippet, cx));
        self.status = message.into();
        cx.notify();
    }

    fn strike_action(&mut self, _: &StrikeText, _window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format("~~", "~~", "已插入删除线 Markdown", cx);
    }

    fn code_action(&mut self, _: &CodeText, _window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format(
            INLINE_CODE_MARKER,
            INLINE_CODE_MARKER,
            "已插入行内代码 Markdown",
            cx,
        );
    }

    fn image_action(&mut self, _: &ImageText, _window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择图片文件".into()),
        });
        cx.spawn(async move |editor, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            if !is_image_path(&path) {
                let _ = editor.update(cx, |editor, cx| {
                    editor.status = "请选择 PNG、JPEG、GIF、WebP、SVG 或 BMP 图片".into();
                    cx.notify();
                });
                return;
            }
            let snippet = format!("![图片]({})", markdown_image_url(&path));
            let _ = editor.update(cx, |editor, cx| {
                editor.insert_snippet(snippet, "已插入本地图片 Markdown", cx);
            });
        })
        .detach();
    }

    fn table_action(&mut self, _: &TableText, _window: &mut Window, cx: &mut Context<Self>) {
        self.insert_snippet(
            "| 列 1 | 列 2 |\n| --- | --- |\n| 内容 | 内容 |".to_owned(),
            "已插入 Markdown 表格",
            cx,
        );
    }

    fn video_action(&mut self, _: &VideoText, _window: &mut Window, cx: &mut Context<Self>) {
        self.insert_snippet(
            "[视频](https://example.com/video)".to_owned(),
            "已插入视频链接 Markdown",
            cx,
        );
    }

    fn divider_action(&mut self, _: &DividerText, _window: &mut Window, cx: &mut Context<Self>) {
        self.insert_snippet("\n---\n".to_owned(), "已插入分隔线 Markdown", cx);
    }

    fn undo_action(&mut self, _: &UndoText, window: &mut Window, cx: &mut Context<Self>) {
        self.text_input
            .update(cx, |input, cx| input.undo(&UndoText, window, cx));
        self.status = "已撤销".into();
        cx.notify();
    }

    fn redo_action(&mut self, _: &RedoText, window: &mut Window, cx: &mut Context<Self>) {
        self.text_input
            .update(cx, |input, cx| input.redo(&RedoText, window, cx));
        self.status = "已重做".into();
        cx.notify();
    }

    fn format_button(
        &mut self,
        prefix: &'static str,
        suffix: &'static str,
        message: &'static str,
        _: &gpui::ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_format(prefix, suffix, message, cx);
    }

    fn prefix_button(
        &mut self,
        prefix: &'static str,
        message: &'static str,
        _: &gpui::ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_prefix(prefix, message, cx);
    }

    fn undo_click(&mut self, _: &gpui::ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.undo_action(&UndoText, window, cx);
    }

    fn redo_click(&mut self, _: &gpui::ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.redo_action(&RedoText, window, cx);
    }
}

impl Render for MarkdownEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self
            .path
            .as_ref()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or("未命名文章.md");
        let title = if self.dirty {
            format!("{title} *")
        } else {
            title.to_owned()
        };
        let content = self.text_input.read(cx).content.clone();
        let workspace = if self.settings_visible {
            div()
                .flex()
                .flex_grow()
                .min_h_0()
                .w_full()
                .p_3()
                .child(settings_panel(
                    self.notion_token_input.clone(),
                    self.notion_parent_input.clone(),
                    self.typecho_url_input.clone(),
                    self.typecho_username_input.clone(),
                    self.typecho_password_input.clone(),
                    cx.listener(Self::save_publish_settings),
                ))
        } else if self.preview {
            div()
                .flex()
                .flex_grow()
                .min_h_0()
                .w_full()
                .p_3()
                .gap_3()
                .child(preview_panel(content.clone(), "预览 · 发布效果"))
        } else {
            div()
                .flex()
                .flex_grow()
                .min_h_0()
                .w_full()
                .p_3()
                .gap_3()
                .child(editor_panel(self.text_input.clone()))
                .child(preview_panel(content.clone(), "预览 · 实时 Markdown"))
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(WORKSPACE))
            .text_color(rgb(TEXT))
            .key_context("MarkdownEditor")
            .on_action(cx.listener(Self::new_document))
            .on_action(cx.listener(Self::open_document))
            .on_action(cx.listener(Self::open_draft))
            .on_action(cx.listener(Self::save_document))
            .on_action(cx.listener(Self::save_draft))
            .on_action(cx.listener(Self::toggle_preview))
            .on_action(cx.listener(Self::toggle_settings))
            .on_action(cx.listener(Self::bold_action))
            .on_action(cx.listener(Self::italic_action))
            .on_action(cx.listener(Self::heading_action))
            .on_action(cx.listener(Self::bullets_action))
            .on_action(cx.listener(Self::quote_action))
            .on_action(cx.listener(Self::link_action))
            .on_action(cx.listener(Self::strike_action))
            .on_action(cx.listener(Self::code_action))
            .on_action(cx.listener(Self::image_action))
            .on_action(cx.listener(Self::table_action))
            .on_action(cx.listener(Self::video_action))
            .on_action(cx.listener(Self::divider_action))
            .on_action(cx.listener(Self::undo_action))
            .on_action(cx.listener(Self::redo_action))
            .on_action(cx.listener(Self::quit_application))
            .on_action(cx.listener(Self::publish_to_notion))
            .on_action(cx.listener(Self::publish_to_typecho))
            .child(title_bar(&title))
            .child(
                div()
                    .h(px(28.))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_1()
                    .bg(rgb(BLUE))
                    .px_3()
                    .child(tab(
                        "主页",
                        self.active_tab == 0,
                        cx.listener(|editor, _, _, cx| {
                            editor.active_tab = 0;
                            cx.notify();
                        }),
                    ))
                    .child(tab(
                        "插入",
                        self.active_tab == 1,
                        cx.listener(|editor, _, _, cx| {
                            editor.active_tab = 1;
                            cx.notify();
                        }),
                    ))
                    .child(tab(
                        "博客文章",
                        self.active_tab == 2,
                        cx.listener(|editor, _, _, cx| {
                            editor.active_tab = 2;
                            cx.notify();
                        }),
                    )),
            )
            .child(
                div()
                    .h(px(92.))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .bg(rgb(RIBBON_BLUE))
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .px_3()
                    .child(ribbon_button(
                        "icons/new.png",
                        "新建",
                        cx.listener(Self::new_document_click),
                    ))
                    .child(ribbon_button(
                        "icons/open.png",
                        "打开",
                        cx.listener(Self::open_document_click),
                    ))
                    .child(ribbon_button(
                        "icons/save.png",
                        "保存",
                        cx.listener(Self::save_document_click),
                    ))
                    .child(ribbon_button(
                        "icons/open.png",
                        "打开草稿",
                        cx.listener(Self::open_draft_click),
                    ))
                    .child(ribbon_button(
                        "icons/save.png",
                        "保存草稿",
                        cx.listener(Self::save_draft_click),
                    ))
                    .child(separator())
                    .child(ribbon_button(
                        "icons/bold.png",
                        "粗体",
                        cx.listener(|editor, event, window, cx| {
                            editor.format_button(
                                "**",
                                "**",
                                "已插入粗体 Markdown",
                                event,
                                window,
                                cx,
                            )
                        }),
                    ))
                    .child(ribbon_button(
                        "icons/italic.png",
                        "斜体",
                        cx.listener(|editor, event, window, cx| {
                            editor.format_button("*", "*", "已插入斜体 Markdown", event, window, cx)
                        }),
                    ))
                    .child(ribbon_button(
                        "icons/bullets.png",
                        "列表",
                        cx.listener(|editor, event, window, cx| {
                            editor.prefix_button("- ", "已插入无序列表", event, window, cx)
                        }),
                    ))
                    .child(ribbon_button(
                        "icons/blockquote.png",
                        "引用",
                        cx.listener(|editor, event, window, cx| {
                            editor.prefix_button("> ", "已插入引用", event, window, cx)
                        }),
                    ))
                    .child(ribbon_button(
                        "icons/heading.png",
                        "标题",
                        cx.listener(|editor, event, window, cx| {
                            editor.prefix_button("# ", "已插入一级标题", event, window, cx)
                        }),
                    ))
                    .child(ribbon_button(
                        "icons/link.png",
                        "链接",
                        cx.listener(|editor, _event, window, cx| {
                            editor.link_action(&LinkText, window, cx)
                        }),
                    ))
                    .child(separator())
                    .child(ribbon_button(
                        "icons/undo.png",
                        "撤销",
                        cx.listener(Self::undo_click),
                    ))
                    .child(ribbon_button(
                        "icons/redo.png",
                        "重做",
                        cx.listener(Self::redo_click),
                    ))
                    .child(div().flex_grow())
                    .child(ribbon_button(
                        "icons/settings.png",
                        "设置",
                        cx.listener(|editor, _event, window, cx| {
                            editor.toggle_settings(&ToggleSettings, window, cx)
                        }),
                    ))
                    .child(ribbon_button(
                        "icons/notion.png",
                        "复制到 Notion",
                        cx.listener(Self::publish_to_notion_click),
                    ))
                    .child(ribbon_button(
                        "icons/typecho.png",
                        "发布 Typecho",
                        cx.listener(Self::publish_to_typecho_click),
                    ))
                    .child(ribbon_button(
                        "icons/preview.png",
                        if self.preview { "编辑" } else { "预览" },
                        cx.listener(Self::toggle_preview_click),
                    )),
            )
            .child(
                div()
                    .h(px(76.))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .bg(rgb(0xf1f6fb))
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .px_3()
                    .child(
                        div()
                            .w(px(92.))
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child("Markdown 插入"),
                    )
                    .child(ribbon_button(
                        "icons/strike.png",
                        "删除线",
                        cx.listener(|editor, _event, window, cx| {
                            editor.strike_action(&StrikeText, window, cx)
                        }),
                    ))
                    .child(ribbon_button(
                        "icons/code.png",
                        "代码",
                        cx.listener(|editor, _event, window, cx| {
                            editor.code_action(&CodeText, window, cx)
                        }),
                    ))
                    .child(ribbon_button(
                        "icons/image.png",
                        "图片",
                        cx.listener(|editor, _event, window, cx| {
                            editor.image_action(&ImageText, window, cx)
                        }),
                    ))
                    .child(ribbon_button(
                        "icons/table.png",
                        "表格",
                        cx.listener(|editor, _event, window, cx| {
                            editor.table_action(&TableText, window, cx)
                        }),
                    ))
                    .child(ribbon_button(
                        "icons/video.png",
                        "视频",
                        cx.listener(|editor, _event, window, cx| {
                            editor.video_action(&VideoText, window, cx)
                        }),
                    ))
                    .child(ribbon_button(
                        "icons/preview.png",
                        "分隔线",
                        cx.listener(|editor, _event, window, cx| {
                            editor.divider_action(&DividerText, window, cx)
                        }),
                    )),
            )
            .child(workspace)
            .child(
                div()
                    .h(px(24.))
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .bg(rgb(0xd9e3ee))
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(self.status.clone())
                    .child(format!(
                        "{} · {}",
                        title,
                        if self.preview { "预览" } else { "编辑" }
                    )),
            )
    }
}

fn line_starts(content: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    starts
}

fn normalize_newlines(content: String) -> String {
    content.replace("\r\n", "\n").replace('\r', "\n")
}

fn utf8_offset_from_utf16(text: &str, offset: usize) -> usize {
    let mut utf16 = 0;
    for (index, character) in text.char_indices() {
        if utf16 >= offset {
            return index;
        }
        utf16 += character.len_utf16();
    }
    text.len()
}

fn utf16_range_to_utf8(text: &str, range: &Range<usize>) -> Range<usize> {
    utf8_offset_from_utf16(text, range.start)..utf8_offset_from_utf16(text, range.end)
}

fn line_for_offset(starts: &[usize], offset: usize) -> usize {
    starts
        .partition_point(|start| *start <= offset)
        .saturating_sub(1)
}

fn line_end(starts: &[usize], line: usize, content_len: usize) -> usize {
    starts
        .get(line + 1)
        .copied()
        .map(|start| start.saturating_sub(1))
        .unwrap_or(content_len)
}

fn display_path(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

fn is_image_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp")
    )
}

fn markdown_image_url(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{path}")
    }
}

fn title_bar(title: &str) -> impl IntoElement {
    div()
        .h(px(42.))
        .w_full()
        .flex()
        .items_center()
        .gap_2()
        .bg(rgb(DARK_BLUE))
        .px_3()
        .text_color(white())
        .child(img("title-bar-logo.png").size_4())
        .child(
            div()
                .font_weight(FontWeight(600.))
                .child("Open Live Writer"),
        )
        .child(div().text_color(hsla(0., 0., 1., 0.7)).child("· Markdown"))
        .child(div().flex_grow())
        .child(
            div()
                .text_sm()
                .text_color(hsla(0., 0., 1., 0.85))
                .child(title.to_owned()),
        )
}

fn editor_panel(input: Entity<MarkdownInput>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .flex_grow()
        .min_w_0()
        .min_h_0()
        .bg(white())
        .border_1()
        .border_color(rgb(BORDER))
        .shadow_sm()
        .child(
            div()
                .h(px(32.))
                .flex()
                .items_center()
                .px_3()
                .bg(rgb(0xf4f7fa))
                .border_b_1()
                .border_color(rgb(BORDER))
                .text_sm()
                .text_color(rgb(MUTED))
                .child("编辑 · Markdown 源文"),
        )
        .child(
            div()
                .id("markdown-editor-scroll")
                .flex_grow()
                .min_h_0()
                .overflow_scroll()
                .p_5()
                .child(input),
        )
}

fn preview_panel(content: SharedString, label: &'static str) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .flex_grow()
        .min_w_0()
        .min_h_0()
        .bg(white())
        .border_1()
        .border_color(rgb(BORDER))
        .shadow_sm()
        .child(
            div()
                .h(px(32.))
                .flex()
                .items_center()
                .px_3()
                .bg(rgb(0xf4f7fa))
                .border_b_1()
                .border_color(rgb(BORDER))
                .text_sm()
                .text_color(rgb(MUTED))
                .child(label),
        )
        .child(
            div()
                .id("markdown-preview-scroll")
                .flex_grow()
                .min_h_0()
                .overflow_y_scroll()
                .p_5()
                .children(markdown_preview(content.as_ref())),
        )
}

fn settings_field(
    label: &'static str,
    hint: &'static str,
    input: Entity<MarkdownInput>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_sm().font_weight(FontWeight(600.)).child(label))
        .child(div().text_xs().text_color(rgb(MUTED)).child(hint))
        .child(
            div()
                .h(px(38.))
                .w_full()
                .flex()
                .items_center()
                .border_1()
                .border_color(rgb(BORDER))
                .bg(white())
                .px_2()
                .child(input),
        )
}

fn settings_panel(
    notion_token: Entity<MarkdownInput>,
    notion_parent: Entity<MarkdownInput>,
    typecho_url: Entity<MarkdownInput>,
    typecho_username: Entity<MarkdownInput>,
    typecho_password: Entity<MarkdownInput>,
    on_save: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .flex_grow()
        .min_h_0()
        .w_full()
        .id("publish-settings-scroll")
        .overflow_scroll()
        .bg(rgb(WORKSPACE))
        .p_6()
        .gap_4()
        .child(
            div()
                .text_xl()
                .font_weight(FontWeight(700.))
                .text_color(rgb(DARK_BLUE))
                .child("发布设置"),
        )
        .child(div().text_sm().text_color(rgb(MUTED)).child(
            "配置后，发布按钮会直接调用服务；凭据仅保存到系统安全存储。未配置时仍可复制 Markdown。",
        ))
        .child(
            div()
                .flex()
                .gap_4()
                .w_full()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_grow()
                        .gap_3()
                        .p_4()
                        .bg(white())
                        .border_1()
                        .border_color(rgb(BORDER))
                        .child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight(700.))
                                .text_color(rgb(0x203c59))
                                .child("Notion"),
                        )
                        .child(settings_field(
                            "集成 Token",
                            "在 Notion 集成页面创建的内部集成 Token",
                            notion_token,
                        ))
                        .child(settings_field(
                            "父页面 ID",
                            "新文章会作为子页面创建在这里",
                            notion_parent,
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_grow()
                        .gap_3()
                        .p_4()
                        .bg(white())
                        .border_1()
                        .border_color(rgb(BORDER))
                        .child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight(700.))
                                .text_color(rgb(0x203c59))
                                .child("Typecho / MetaWeblog"),
                        )
                        .child(settings_field(
                            "XML-RPC 地址",
                            "通常是站点地址加 /action/xmlrpc",
                            typecho_url,
                        ))
                        .child(settings_field(
                            "用户名",
                            "Typecho 登录用户名",
                            typecho_username,
                        ))
                        .child(settings_field(
                            "密码",
                            "只写入系统凭据存储",
                            typecho_password,
                        )),
                ),
        )
        .child(
            div().flex().justify_end().child(
                div()
                    .id("save-publish-settings")
                    .px_4()
                    .py_2()
                    .rounded_sm()
                    .bg(rgb(BLUE))
                    .text_color(white())
                    .hover(|style| style.bg(rgb(DARK_BLUE)).cursor_pointer())
                    .on_click(on_save)
                    .child("保存发布设置"),
            ),
        )
}

fn tab(
    label: &'static str,
    active: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let background = if active { rgb(0xffffff) } else { rgb(BLUE) };
    let foreground = if active {
        hsla(0.58, 0.52, 0.47, 1.)
    } else {
        white()
    };
    div()
        .id(label)
        .h(px(24.))
        .px_3()
        .flex()
        .items_center()
        .rounded_sm()
        .bg(background)
        .text_sm()
        .text_color(foreground)
        .hover(|style| style.bg(rgb(0xd4e3f2)).cursor_pointer())
        .on_click(on_click)
        .child(label)
}

fn separator() -> impl IntoElement {
    div().h(px(54.)).w(px(1.)).bg(rgb(BORDER)).mx_1()
}

fn ribbon_button(
    icon: &'static str,
    label: &'static str,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(label)
        .w(px(64.))
        .h(px(70.))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_1()
        .rounded_sm()
        .text_xs()
        .text_color(rgb(TEXT))
        .hover(|style| style.bg(rgb(0xd4e3f2)).cursor_pointer())
        .on_click(on_click)
        .child(img(icon).size_5())
        .child(label)
}

fn markdown_preview(markdown: &str) -> Vec<gpui::AnyElement> {
    parse_blocks(markdown)
        .into_iter()
        .enumerate()
        .map(|(index, block)| match block {
            Block::Heading { level, text } => {
                let size = match level {
                    1 => px(28.),
                    2 => px(23.),
                    3 => px(20.),
                    _ => px(17.),
                };
                div()
                    .id(("preview-heading", index))
                    .w_full()
                    .mt_3()
                    .mb_2()
                    .font_weight(FontWeight(700.))
                    .text_size(size)
                    .text_color(rgb(0x203c59))
                    .child(inline_preview(&text))
                    .into_any_element()
            }
            Block::Paragraph(text) => div()
                .id(("preview-paragraph", index))
                .w_full()
                .mb_3()
                .text_base()
                .line_height(px(26.))
                .child(inline_preview(&text))
                .into_any_element(),
            Block::Bullet(text) => div()
                .id(("preview-bullet", index))
                .w_full()
                .mb_1()
                .pl_3()
                .flex()
                .text_base()
                .child("• ")
                .child(inline_preview(&text))
                .into_any_element(),
            Block::Numbered { marker, text } => div()
                .id(("preview-number", index))
                .w_full()
                .mb_1()
                .pl_3()
                .flex()
                .text_base()
                .child(format!("{}. ", marker))
                .child(inline_preview(&text))
                .into_any_element(),
            Block::Quote(text) => div()
                .id(("preview-quote", index))
                .w_full()
                .mb_3()
                .pl_3()
                .border_l_2()
                .border_color(rgb(BLUE))
                .italic()
                .text_color(rgb(MUTED))
                .child(inline_preview(&text))
                .into_any_element(),
            Block::Code(text) => div()
                .id(("preview-code", index))
                .w_full()
                .mb_3()
                .p_3()
                .rounded_sm()
                .bg(rgb(0xf1f3f5))
                .border_1()
                .border_color(rgb(0xdfe3e8))
                .text_color(rgb(0x38434d))
                .child(text)
                .into_any_element(),
            Block::Image { alt, url } => div()
                .id(("preview-image", index))
                .w_full()
                .mb_3()
                .flex()
                .flex_col()
                .gap_1()
                .child(img(url).max_w_full())
                .child(div().text_xs().text_color(rgb(MUTED)).child(alt))
                .into_any_element(),
            Block::Video { url } => div()
                .id(("preview-video", index))
                .w_full()
                .mb_3()
                .p_3()
                .rounded_sm()
                .border_1()
                .border_color(rgb(BORDER))
                .bg(rgb(0xf1f6fb))
                .child(div().font_weight(FontWeight(600.)).child("视频"))
                .child(div().text_xs().text_color(rgb(BLUE)).child(url))
                .into_any_element(),
            Block::Table { headers, rows } => {
                let mut table = div()
                    .id(("preview-table", index))
                    .w_full()
                    .mb_3()
                    .flex()
                    .flex_col()
                    .border_1()
                    .border_color(rgb(BORDER));
                table = table.child(preview_table_row(headers, true, index * 1000));
                for (row_index, row) in rows.into_iter().enumerate() {
                    table =
                        table.child(preview_table_row(row, false, index * 1000 + row_index + 1));
                }
                table.into_any_element()
            }
            Block::Divider => div()
                .id(("preview-divider", index))
                .w_full()
                .h(px(1.))
                .my_3()
                .bg(rgb(BORDER))
                .into_any_element(),
        })
        .collect()
}

fn preview_table_row(cells: Vec<String>, header: bool, index: usize) -> gpui::AnyElement {
    let mut row = div().id(("preview-table-row", index)).flex().w_full();
    for (cell_index, cell) in cells.into_iter().enumerate() {
        let mut element = div()
            .id(("preview-table-cell", index * 100 + cell_index))
            .flex_grow()
            .min_w_0()
            .p_2()
            .border_color(rgb(BORDER))
            .border_r_1()
            .border_b_1()
            .child(inline_preview(&cell));
        if header {
            element = element.font_weight(FontWeight(600.)).bg(rgb(0xf1f6fb));
        }
        row = row.child(element);
    }
    row.into_any_element()
}

fn inline_preview(markdown: &str) -> gpui::AnyElement {
    let mut children = Vec::new();
    let mut offset = 0;
    while offset < markdown.len() {
        let rest = &markdown[offset..];
        let Some((marker_offset, marker)) = next_preview_marker(rest) else {
            children.push(preview_inline_piece(rest, None));
            break;
        };
        if marker_offset > 0 {
            children.push(preview_inline_piece(&rest[..marker_offset], None));
            offset += marker_offset;
            continue;
        }
        if marker == "["
            && let Some(label_end) = rest.find("](")
            && let Some(url_end) = rest[label_end + 2..].find(')')
        {
            let label = &rest[1..label_end];
            children.push(preview_inline_piece(label, Some("link")));
            offset += label_end + 2 + url_end + 1;
            continue;
        }
        if let Some(close_offset) = rest[marker.len()..].find(marker) {
            let start = marker.len();
            let end = start + close_offset;
            let style = match marker {
                "**" | "__" => Some("bold"),
                "*" | "_" => Some("italic"),
                "~~" => Some("strike"),
                INLINE_CODE_MARKER => Some("code"),
                _ => None,
            };
            if style.is_some() {
                children.push(preview_inline_piece(&rest[start..end], style));
                offset += end + marker.len();
                continue;
            }
        }
        children.push(preview_inline_piece(marker, None));
        offset += marker.len();
    }
    div()
        .flex()
        .flex_wrap()
        .children(children)
        .into_any_element()
}

fn next_preview_marker(text: &str) -> Option<(usize, &'static str)> {
    ["**", "__", "~~", "*", "_", INLINE_CODE_MARKER, "["]
        .into_iter()
        .filter_map(|marker| text.find(marker).map(|offset| (offset, marker)))
        .min_by_key(|(offset, _)| *offset)
}

fn preview_inline_piece(text: &str, style: Option<&str>) -> gpui::AnyElement {
    let mut piece = div().child(text.to_owned());
    match style {
        Some("bold") => piece = piece.font_weight(FontWeight(700.)),
        Some("italic") => piece = piece.italic(),
        Some("strike") => piece = piece.line_through(),
        Some("code") => piece = piece.bg(rgb(0xf1f3f5)).px_1(),
        Some("link") => piece = piece.text_color(rgb(BLUE)).underline(),
        _ => {}
    }
    piece.into_any_element()
}

fn main() {
    Application::new().with_assets(Assets).run(|cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("secondary-n", NewDocument, None),
            KeyBinding::new("secondary-o", OpenDocument, None),
            KeyBinding::new("secondary-shift-o", OpenDraft, None),
            KeyBinding::new("secondary-s", SaveDocument, None),
            KeyBinding::new("secondary-shift-s", SaveDraft, None),
            KeyBinding::new("secondary-b", BoldText, None),
            KeyBinding::new("secondary-i", ItalicText, None),
            KeyBinding::new("secondary-1", HeadingText, None),
            KeyBinding::new("secondary-shift-8", BulletsText, None),
            KeyBinding::new("secondary-shift-.", QuoteText, None),
            KeyBinding::new("secondary-k", LinkText, None),
            KeyBinding::new("secondary-z", UndoText, None),
            KeyBinding::new("secondary-shift-z", RedoText, None),
            KeyBinding::new("secondary-shift-p", TogglePreview, None),
            KeyBinding::new("secondary-comma", ToggleSettings, None),
            KeyBinding::new("secondary-shift-enter", PublishToNotion, None),
            KeyBinding::new("secondary-shift-t", PublishToTypecho, None),
            KeyBinding::new("secondary-q", QuitApplication, None),
            KeyBinding::new("backspace", Backspace, Some("MarkdownInput")),
            KeyBinding::new("delete", Delete, Some("MarkdownInput")),
            KeyBinding::new("left", Left, Some("MarkdownInput")),
            KeyBinding::new("right", Right, Some("MarkdownInput")),
            KeyBinding::new("up", Up, Some("MarkdownInput")),
            KeyBinding::new("down", Down, Some("MarkdownInput")),
            KeyBinding::new("shift-left", SelectLeft, Some("MarkdownInput")),
            KeyBinding::new("shift-right", SelectRight, Some("MarkdownInput")),
            KeyBinding::new("shift-up", SelectUp, Some("MarkdownInput")),
            KeyBinding::new("shift-down", SelectDown, Some("MarkdownInput")),
            KeyBinding::new("secondary-a", SelectAll, Some("MarkdownInput")),
            KeyBinding::new("secondary-v", PasteText, Some("MarkdownInput")),
            KeyBinding::new("secondary-c", CopyText, Some("MarkdownInput")),
            KeyBinding::new("secondary-x", CutText, Some("MarkdownInput")),
            KeyBinding::new("home", Home, Some("MarkdownInput")),
            KeyBinding::new("end", End, Some("MarkdownInput")),
        ]);
        let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("Open Live Writer · Markdown".into()),
                        ..Default::default()
                    }),
                    window_min_size: Some(size(px(900.), px(600.))),
                    ..Default::default()
                },
                |_, cx| cx.new(MarkdownEditor::new),
            )
            .expect("failed to open the editor window");
        window
            .update(cx, |editor, window, cx| {
                window.focus(&editor.text_input.read(cx).focus_handle.clone());
                let editor = cx.entity().downgrade();
                window.on_window_should_close(cx, move |window, cx| {
                    let (dirty, close_confirmed) = editor
                        .read_with(cx, |editor, _| (editor.dirty, editor.close_confirmed))
                        .unwrap_or((false, false));
                    if !dirty || close_confirmed {
                        return true;
                    }
                    let answer = window.prompt(
                        gpui::PromptLevel::Warning,
                        "当前文章有未保存的修改",
                        Some("关闭后这些修改将丢失。"),
                        &["放弃并关闭", "取消"],
                        cx,
                    );
                    let editor_for_prompt = editor.clone();
                    window
                        .spawn(cx, async move |cx| {
                            if answer.await.ok() == Some(0) {
                                let _ = editor_for_prompt.update(cx, |editor, cx| {
                                    editor.close_confirmed = true;
                                    cx.notify();
                                });
                                let _ = cx.update(|window, _| window.remove_window());
                            }
                        })
                        .detach();
                    false
                });
                cx.activate(true);
            })
            .expect("failed to focus the editor");
    });
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        is_image_path, line_end, line_starts, markdown_image_url, normalize_newlines,
        utf16_range_to_utf8,
    };

    #[test]
    fn keeps_the_cursor_after_the_last_character() {
        assert_eq!(line_end(&line_starts("hello"), 0, 5), 5);
        assert_eq!(line_end(&line_starts("hello\nworld"), 1, 11), 11);
    }

    #[test]
    fn normalizes_windows_newlines_and_local_ime_ranges() {
        assert_eq!(normalize_newlines("a\r\nb\rc".into()), "a\nb\nc");
        assert_eq!(utf16_range_to_utf8("中文", &(0..2)), 0..6);
    }

    #[test]
    fn accepts_common_image_files_and_builds_file_urls() {
        let path = Path::new("/tmp/封面.png");
        assert!(is_image_path(path));
        assert_eq!(markdown_image_url(path), "file:///tmp/封面.png");
        assert!(!is_image_path(Path::new("/tmp/article.md")));
    }
}
