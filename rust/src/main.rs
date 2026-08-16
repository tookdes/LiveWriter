#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod markdown;
mod publishing;
mod storage;

use std::{
    borrow::Cow,
    collections::HashMap,
    fs,
    ops::Range,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use gpui::{
    AnyElement, App, Application, AssetSource, Bounds, ClipboardItem, Context, CursorStyle, Entity,
    EntityInputHandler, FocusHandle, Focusable, FontWeight, Image, ImageFormat, ImageSource, Img,
    KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathPromptOptions,
    Pixels, PromptLevel, SharedString, Window, WindowBounds, WindowOptions, actions, div, hsla,
    img, linear_color_stop, linear_gradient, prelude::*, px, relative, rgb, size, white,
};
use gpui_component::{
    RopeExt, Theme, ThemeMode,
    input::{Input, InputEvent, InputState},
};
use rust_embed::Embed;

use crate::markdown::{Block, InlineStyle, RichTextPiece, parse_blocks, parse_inline};
use crate::publishing::{
    CREDENTIALS_URL, CREDENTIALS_USERNAME, NotionConfig, StoredPublishSettings, TypechoConfig,
    document_title, local_image_count, publish_to_notion as publish_notion_request,
    publish_to_typecho as publish_typecho_request,
};

const DEFAULT_MARKDOWN: &str = "# 欢迎回来，Open Live Writer\n\n这是一个保持怀旧外观的 Markdown 编辑器。你可以直接编辑左侧内容，然后切换到预览。\n\n- 使用工具栏快速插入 Markdown\n- 点击“复制到 Notion”复制可粘贴的 Markdown\n- 文件使用 UTF-8 的 md 格式保存\n- [ ] GitHub 待办清单\n- [x] 已完成的待办\n\n<aside>💡 Notion 旁注块</aside>\n\n> 先写作，再发布。\n";

const BLUE: u32 = 0x3b78b4;
const DARK_BLUE: u32 = 0x2b5f95;
const RIBBON_BLUE: u32 = 0xf3f6fa;
const RIBBON_SEPARATOR: u32 = 0xc8d5e2;
const RIBBON_TAB_HEIGHT: Pixels = px(30.);
const RIBBON_HEIGHT: Pixels = px(94.);
const RIBBON_LARGE_BUTTON_WIDTH: Pixels = px(62.);
const RIBBON_LARGE_BUTTON_HEIGHT: Pixels = px(70.);
const RIBBON_SMALL_BUTTON_HEIGHT: Pixels = px(22.);
const WORKSPACE: u32 = 0xe9edf2;
const BORDER: u32 = 0xc6ced8;
const TEXT: u32 = 0x263746;
const MUTED: u32 = 0x617285;
const INLINE_CODE_MARKER: &str = "\x60";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileEncoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    Gbk,
}

#[derive(Clone, Debug)]
struct LoadedDocument {
    content: String,
    line_ending: String,
    encoding: FileEncoding,
}

impl LoadedDocument {
    fn new(content: String, encoding: FileEncoding) -> Self {
        let (content, line_ending) = prepare_content(content);
        Self {
            content,
            line_ending,
            encoding,
        }
    }
}

fn typecho_is_primary_publish_target(notion_configured: bool, typecho_configured: bool) -> bool {
    typecho_configured && !notion_configured
}

macro_rules! ribbon_controls {
    ($first:expr $(, $rest:expr)* $(,)?) => {{
        let controls = div().h_full().flex().items_center().gap_0p5().child($first);
        $(let controls = controls.child($rest);)*
        controls
    }};
}

macro_rules! ribbon_stack {
    ($first:expr $(, $rest:expr)* $(,)?) => {{
        let controls = div().flex().flex_col().justify_center().gap_0p5().child($first);
        $(let controls = controls.child($rest);)*
        controls
    }};
}

actions!(
    open_live_writer,
    [
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

struct MarkdownInput {
    state: Entity<InputState>,
    pub content: SharedString,
    multi_line: bool,
    masked: bool,
    pending_content: Option<SharedString>,
    pending_insert: Option<SharedString>,
    refocus_on_recreate: bool,
    _subscription: gpui::Subscription,
}

impl MarkdownInput {
    fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        content: String,
        masked: bool,
        multi_line: bool,
    ) -> Self {
        let content: SharedString = content.into();
        let state = cx.new(|cx| {
            let mut state = InputState::new(window, cx).default_value(content.clone());
            if multi_line {
                state = state.multi_line().soft_wrap(true);
            }
            state.masked(masked)
        });
        let subscription = subscribe_input_state(&state, cx);
        Self {
            state: state.clone(),
            content,
            multi_line,
            masked,
            pending_content: None,
            pending_insert: None,
            refocus_on_recreate: false,
            _subscription: subscription,
        }
    }

    fn set_document_content(&mut self, content: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.refocus_on_recreate = true;
        self.set_content(content, cx);
    }

    fn set_content(&mut self, content: impl Into<SharedString>, cx: &mut Context<Self>) {
        let content = content.into();
        self.content = content.clone();
        self.pending_content = Some(content);
        self.pending_insert = None;
        cx.notify();
    }

    fn queue_insert(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        let text: SharedString = text.into();
        let mut pending = self
            .pending_insert
            .take()
            .map(|text| text.to_string())
            .unwrap_or_default();
        pending.push_str(text.as_ref());
        self.pending_insert = Some(pending.into());
        cx.notify();
    }

    fn sync_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(content) = self.pending_content.take() {
            // Recreate the InputState so the native undo/redo history cannot
            // leak between documents (a fresh state always starts with an
            // empty history).
            let multi_line = self.multi_line;
            let masked = self.masked;
            let refocus = self.refocus_on_recreate;
            let state = cx.new(|cx| {
                let mut state = InputState::new(window, cx).default_value(content.clone());
                if multi_line {
                    state = state.multi_line().soft_wrap(true);
                }
                state.masked(masked)
            });
            self._subscription = subscribe_input_state(&state, cx);
            self.state = state;
            self.refocus_on_recreate = false;
            if refocus {
                let handle = self.state.read(cx).focus_handle(cx);
                window.on_next_frame(move |window, _| {
                    window.focus(&handle);
                });
            }
        }
        if let Some(text) = self.pending_insert.take() {
            let state = self.state.clone();
            state.update(cx, |state, cx| state.insert(text, window, cx));
        }
    }

    fn wrap_selection(
        &mut self,
        prefix: &str,
        suffix: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prefix = prefix.to_owned();
        let suffix = suffix.to_owned();
        let state = self.state.clone();
        state.update(cx, |state, cx| {
            let Some(selection) = state.selected_text_range(false, window, cx) else {
                return;
            };
            let range = state.text().offset_utf16_to_offset(selection.range.start)
                ..state.text().offset_utf16_to_offset(selection.range.end);
            let cursor_inside = range.start + prefix.len();
            let move_cursor_inside = range.is_empty();
            let selected = state.text().slice(range).to_string();
            state.replace(format!("{}{}{}", prefix, selected, suffix), window, cx);
            if move_cursor_inside {
                let position = state.text().offset_to_position(cursor_inside);
                state.set_cursor_position(position, window, cx);
            }
        });
    }

    fn toggle_line_prefix(&mut self, prefix: &str, window: &mut Window, cx: &mut Context<Self>) {
        let prefix = prefix.to_owned();
        let state = self.state.clone();
        state.update(cx, |state, cx| {
            let full = state.text().to_string();
            let starts = line_starts(&full);
            let cursor = state.cursor();
            let line = state.text().offset_to_position(state.cursor()).line as usize;
            let line = line.min(starts.len().saturating_sub(1));
            let line_start = starts[line];
            let line_end = line_end(&starts, line, full.len());
            let line_text = &full[line_start..line_end];

            let (new_line, cursor_delta) = if prefix == "# " {
                toggle_heading_line(line_text, cursor.saturating_sub(line_start))
            } else if let Some(rest) = line_text.strip_prefix(&prefix) {
                (
                    rest.to_owned(),
                    cursor
                        .saturating_sub(line_start)
                        .saturating_sub(prefix.len()),
                )
            } else {
                (
                    format!("{prefix}{line_text}"),
                    cursor.saturating_sub(line_start) + prefix.len(),
                )
            };

            let range_utf16 = utf16_range_from_utf8(&full, line_start..line_end);
            <InputState as EntityInputHandler>::replace_text_in_range(
                state,
                Some(range_utf16),
                &new_line,
                window,
                cx,
            );
            let new_cursor = (line_start + cursor_delta).min(state.text().len());
            let position = state.text().offset_to_position(new_cursor);
            state.set_cursor_position(position, window, cx);
        });
    }
}

impl Render for MarkdownInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_pending(window, cx);
        let mut input = Input::new(&self.state)
            .appearance(false)
            .bordered(false)
            .focus_bordered(false)
            .text_color(rgb(TEXT))
            .text_size(if self.multi_line { px(16.) } else { px(14.) });
        if self.multi_line {
            input = input.h_full().line_height(px(28.));
        }
        div()
            .w_full()
            .when(self.multi_line, |this| this.h_full())
            .child(input)
    }
}

fn subscribe_input_state(
    state: &Entity<InputState>,
    cx: &mut Context<MarkdownInput>,
) -> gpui::Subscription {
    cx.subscribe(
        state,
        |input: &mut MarkdownInput, state, event: &InputEvent, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            let next = state.read(cx).value();
            if next == input.content {
                return;
            }
            input.content = next;
            cx.notify();
        },
    )
}

fn toggle_heading_line(line: &str, cursor_offset: usize) -> (String, usize) {
    if let Some(rest) = line.strip_prefix("# ") {
        let removed = line.len().saturating_sub(rest.len());
        (rest.to_owned(), cursor_offset.saturating_sub(removed))
    } else if line.starts_with('#') {
        let rest = line.trim_start_matches('#').trim_start();
        let title_offset = line.len().saturating_sub(rest.len());
        let new_line = format!("# {rest}");
        let cursor = if cursor_offset <= title_offset {
            2.min(new_line.len())
        } else {
            2 + cursor_offset.saturating_sub(title_offset)
        };
        (new_line, cursor)
    } else {
        (format!("# {line}"), cursor_offset + 2)
    }
}

impl Focusable for MarkdownInput {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.state.read(cx).focus_handle(cx)
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
    split_ratio: f32,
    splitter_dragging: bool,
    settings_visible: bool,
    dirty: bool,
    status: SharedString,
    publish_settings: StoredPublishSettings,
    last_observed_content: String,
    suppress_observer: bool,
    close_confirmed: bool,
    line_ending: String,
    file_encoding: FileEncoding,
    last_window_title: String,
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
        window: &mut Window,
        cx: &mut Context<Self>,
        content: String,
        masked: bool,
    ) -> Entity<MarkdownInput> {
        cx.new(|cx| MarkdownInput::new(window, cx, content, masked, false))
    }

    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let initial = DEFAULT_MARKDOWN.to_owned();
        let text_input = cx.new(|cx| MarkdownInput::new(window, cx, initial.clone(), false, true));
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
            Self::new_settings_input(window, cx, publish_settings.notion_token.clone(), true);
        let notion_parent_input = Self::new_settings_input(
            window,
            cx,
            publish_settings.notion_parent_page_id.clone(),
            false,
        );
        let typecho_url_input = Self::new_settings_input(
            window,
            cx,
            publish_settings.typecho_xmlrpc_url.clone(),
            false,
        );
        let typecho_username_input =
            Self::new_settings_input(window, cx, publish_settings.typecho_username.clone(), false);
        let typecho_password_input =
            Self::new_settings_input(window, cx, publish_settings.typecho_password.clone(), true);
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
            split_ratio: 0.38,
            splitter_dragging: false,
            settings_visible: false,
            dirty: false,
            status: "就绪 · Markdown 模式".into(),
            last_observed_content: initial,
            suppress_observer: false,
            close_confirmed: false,
            line_ending: "\n".to_owned(),
            file_encoding: FileEncoding::Utf8,
            last_window_title: "Open Live Writer".to_owned(),
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
        cx.spawn(async move |editor, cx| {
            loop {
                gpui::Timer::after(std::time::Duration::from_secs(30)).await;
                let _ = editor.update(cx, |editor, cx| {
                    if editor.dirty {
                        let content = editor.content(cx);
                        if let Err(error) = storage::save_autosave(&content) {
                            eprintln!("自动备份失败：{error}");
                        }
                    }
                });
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
            let is_empty = input.read(cx).content.is_empty();
            if is_empty && !value.is_empty() {
                input.update(cx, |input, cx| input.set_content(value, cx));
            }
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
            "发布设置 · 环境变量优先，凭据保存在系统安全存储".into()
        } else if self.preview {
            if self.dirty {
                "预览模式 · 尚未保存".into()
            } else {
                "预览模式 · Markdown 已渲染".into()
            }
        } else if self.dirty {
            "正在编辑 · 尚未保存".into()
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
            notion_token: self.notion_token_input.read(cx).content.trim().to_owned(),
            notion_parent_page_id: self.notion_parent_input.read(cx).content.trim().to_owned(),
            typecho_xmlrpc_url: self.typecho_url_input.read(cx).content.trim().to_owned(),
            typecho_username: self
                .typecho_username_input
                .read(cx)
                .content
                .trim()
                .to_owned(),
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

    fn replace_document(
        &mut self,
        path: Option<PathBuf>,
        document: LoadedDocument,
        cx: &mut Context<Self>,
    ) {
        let LoadedDocument {
            content,
            line_ending,
            encoding,
        } = document;
        self.suppress_observer = true;
        self.text_input.update(cx, |input, cx| {
            input.set_document_content(content.clone(), cx)
        });
        self.suppress_observer = false;
        self.last_observed_content = content;
        self.line_ending = line_ending;
        self.file_encoding = encoding;
        self.path = path;
        self.dirty = false;
        self.close_confirmed = false;
        self.status = "已加载 · Markdown".into();
        let _ = storage::clear_autosave();
        cx.notify();
    }

    fn restore_autosave(&mut self, content: String, cx: &mut Context<Self>) {
        self.replace_document(None, LoadedDocument::new(content, FileEncoding::Utf8), cx);
        self.dirty = true;
        self.status = "已恢复自动备份 · 尚未保存".into();
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
        self.replace_document(
            None,
            LoadedDocument::new("# 未命名文章\n\n".to_owned(), FileEncoding::Utf8),
            cx,
        );
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
            Ok(path) => {
                let _ = storage::clear_autosave();
                self.status =
                    format!("草稿已保存 · {} · 尚未保存为文件", display_path(&path)).into()
            }
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
                self.replace_document(None, LoadedDocument::new(content, FileEncoding::Utf8), cx);
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
            Some("请选择保存并继续、放弃修改并继续，或取消。"),
            &["保存并继续", "放弃修改并继续", "取消"],
            cx,
        );
        let current_path = self.path.clone();
        cx.spawn(async move |editor, cx| match answer.await.ok() {
            Some(0) => {
                if let Some(path) = current_path {
                    let _ = editor.update(cx, |editor, cx| {
                        if editor.save_to(path, cx) {
                            editor.continue_pending(operation, cx);
                        }
                    });
                    return;
                }
                let Ok(receiver) = cx.update(|app| {
                    app.prompt_for_new_path(&default_save_directory(), Some("未命名文章.md"))
                }) else {
                    return;
                };
                let Ok(Ok(Some(path))) = receiver.await else {
                    return;
                };
                let _ = editor.update(cx, |editor, cx| {
                    if editor.save_to(path, cx) {
                        editor.continue_pending(operation, cx);
                    }
                });
            }
            Some(1) => {
                let _ = editor.update(cx, |editor, cx| editor.continue_pending(operation, cx));
            }
            _ => {}
        })
        .detach();
    }

    fn continue_pending(&mut self, operation: PendingOperation, cx: &mut Context<Self>) {
        match operation {
            PendingOperation::New => self.new_document_now(cx),
            PendingOperation::Open => self.begin_open(cx),
            PendingOperation::OpenDraft => self.open_draft_now(cx),
        }
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
            match fs::read(&path) {
                Ok(bytes) => match decode_markdown(&bytes) {
                    Ok(document) => {
                        let _ = editor.update(cx, |editor, cx| {
                            editor.replace_document(Some(path), document, cx);
                        });
                    }
                    Err(message) => {
                        let _ = editor.update(cx, |editor, cx| {
                            editor.status = message.into();
                            cx.notify();
                        });
                    }
                },
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

    fn save_to(&mut self, path: PathBuf, cx: &mut Context<Self>) -> bool {
        let editor_content = self.content(cx);
        let disk_content = editor_content.replace('\n', &self.line_ending);
        let bytes = match encode_markdown(&disk_content, self.file_encoding) {
            Ok(bytes) => bytes,
            Err(message) => {
                self.status = message.into();
                cx.notify();
                return false;
            }
        };
        match fs::write(&path, bytes) {
            Ok(()) => {
                self.path = Some(path.clone());
                self.dirty = false;
                self.last_observed_content = editor_content;
                self.status = format!("已保存 · {}", display_path(&path)).into();
                let _ = storage::clear_autosave();
                cx.notify();
                true
            }
            Err(error) => {
                self.status = format!("保存失败：{error}").into();
                cx.notify();
                false
            }
        }
    }

    fn save_document(&mut self, _: &SaveDocument, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.path.clone() {
            self.save_to(path, cx);
            return;
        }
        let receiver = cx.prompt_for_new_path(&default_save_directory(), Some("未命名文章.md"));
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
            if self.dirty {
                "预览模式 · 尚未保存".into()
            } else {
                "预览模式 · Markdown 已渲染".into()
            }
        } else {
            if self.dirty {
                "编辑模式 · 尚未保存".into()
            } else {
                "编辑模式 · Markdown 源文".into()
            }
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
            Some("请选择保存并退出、放弃修改并退出，或取消。"),
            &["保存并退出", "放弃并退出", "取消"],
            cx,
        );
        let current_path = self.path.clone();
        cx.spawn(async move |editor, cx| match answer.await.ok() {
            Some(0) => {
                if let Some(path) = current_path {
                    let saved = editor.update(cx, |editor, cx| {
                        if editor.save_to(path, cx) {
                            editor.close_confirmed = true;
                            cx.notify();
                            true
                        } else {
                            false
                        }
                    });
                    if saved.unwrap_or(false) {
                        let _ = cx.update(|app| app.quit());
                    }
                    return;
                }
                let Ok(receiver) = cx.update(|app| {
                    app.prompt_for_new_path(&default_save_directory(), Some("未命名文章.md"))
                }) else {
                    return;
                };
                let Ok(Ok(Some(path))) = receiver.await else {
                    return;
                };
                let saved = editor.update(cx, |editor, cx| {
                    if editor.save_to(path, cx) {
                        editor.close_confirmed = true;
                        cx.notify();
                        true
                    } else {
                        false
                    }
                });
                if saved.unwrap_or(false) {
                    let _ = cx.update(|app| app.quit());
                }
            }
            Some(1) => {
                let _ = editor.update(cx, |editor, cx| {
                    editor.close_confirmed = true;
                    let _ = storage::clear_autosave();
                    cx.notify();
                });
                let _ = cx.update(|app| app.quit());
            }
            _ => {}
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
        let warning = local_image_warning(&markdown);
        if let Some(config) =
            NotionConfig::from_env().or_else(|| self.publish_settings.notion_config())
        {
            let http = cx.http_client();
            self.status = format!("正在发布到 Notion…{warning}").into();
            cx.notify();
            cx.spawn(async move |editor, cx| {
                let result = publish_notion_request(http, config, &title, &markdown).await;
                let _ = editor.update(cx, |editor, cx| {
                    editor.status = match result {
                        Ok(_) => format!("已发布到 Notion{warning}").into(),
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
        let warning = local_image_warning(&markdown);
        if let Some(config) =
            TypechoConfig::from_env().or_else(|| self.publish_settings.typecho_config())
        {
            let http = cx.http_client();
            self.status = format!("正在发布到 Typecho…{warning}").into();
            cx.notify();
            cx.spawn(async move |editor, cx| {
                let result = publish_typecho_request(http, config, &title, &markdown).await;
                let _ = editor.update(cx, |editor, cx| {
                    editor.status = match result {
                        Ok(_) => format!("已提交到 Typecho{warning}").into(),
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

    fn apply_format(
        &mut self,
        prefix: &str,
        suffix: &str,
        message: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.text_input.update(cx, |input, cx| {
            input.wrap_selection(prefix, suffix, window, cx)
        });
        self.status = message.to_owned().into();
        cx.notify();
    }

    fn apply_prefix(
        &mut self,
        prefix: &str,
        message: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.text_input
            .update(cx, |input, cx| input.toggle_line_prefix(prefix, window, cx));
        self.status = message.to_owned().into();
        cx.notify();
    }

    fn bold_action(&mut self, _: &BoldText, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format("**", "**", "已插入粗体 Markdown", window, cx);
    }

    fn italic_action(&mut self, _: &ItalicText, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format("*", "*", "已插入斜体 Markdown", window, cx);
    }

    fn heading_action(&mut self, _: &HeadingText, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_prefix("# ", "已切换标题", window, cx);
    }

    fn bullets_action(&mut self, _: &BulletsText, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_prefix("- ", "已切换无序列表", window, cx);
    }

    fn quote_action(&mut self, _: &QuoteText, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_prefix("> ", "已切换引用", window, cx);
    }

    fn link_action(&mut self, _: &LinkText, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format(
            "[",
            "](https://example.com)",
            "已插入链接 Markdown",
            window,
            cx,
        );
    }

    fn insert_snippet(&mut self, snippet: String, message: &'static str, cx: &mut Context<Self>) {
        self.text_input
            .update(cx, |input, cx| input.queue_insert(snippet, cx));
        self.status = message.into();
        cx.notify();
    }

    fn strike_action(&mut self, _: &StrikeText, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format("~~", "~~", "已插入删除线 Markdown", window, cx);
    }

    fn code_action(&mut self, _: &CodeText, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format(
            INLINE_CODE_MARKER,
            INLINE_CODE_MARKER,
            "已插入行内代码 Markdown",
            window,
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
        window.dispatch_action(Box::new(gpui_component::input::Undo), cx);
    }

    fn redo_action(&mut self, _: &RedoText, window: &mut Window, cx: &mut Context<Self>) {
        window.dispatch_action(Box::new(gpui_component::input::Redo), cx);
    }

    fn format_button(
        &mut self,
        prefix: &'static str,
        suffix: &'static str,
        message: &'static str,
        _: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_format(prefix, suffix, message, window, cx);
    }

    fn prefix_button(
        &mut self,
        prefix: &'static str,
        message: &'static str,
        _: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_prefix(prefix, message, window, cx);
    }

    fn undo_click(&mut self, _: &gpui::ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.undo_action(&UndoText, window, cx);
    }

    fn redo_click(&mut self, _: &gpui::ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.redo_action(&RedoText, window, cx);
    }

    fn splitter_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button == MouseButton::Left {
            self.splitter_dragging = true;
            window.set_window_cursor_style(CursorStyle::ResizeLeftRight);
            cx.notify();
        }
    }

    fn splitter_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.splitter_dragging || !event.dragging() {
            return;
        }
        let window_width: f32 = window.bounds().size.width.into();
        if window_width <= 0. {
            return;
        }
        let ratio = (f32::from(event.position.x) / window_width).clamp(0.22, 0.74);
        if (ratio - self.split_ratio).abs() > 0.002 {
            self.split_ratio = ratio;
            cx.notify();
        }
    }

    fn splitter_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button == MouseButton::Left && self.splitter_dragging {
            self.splitter_dragging = false;
            window.set_window_cursor_style(CursorStyle::Arrow);
            cx.notify();
        }
    }

    fn home_publish_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let notion_configured =
            NotionConfig::from_env().is_some() || self.publish_settings.notion_config().is_some();
        let typecho_configured =
            TypechoConfig::from_env().is_some() || self.publish_settings.typecho_config().is_some();
        let typecho_primary =
            typecho_is_primary_publish_target(notion_configured, typecho_configured);

        type Click =
            fn(&mut MarkdownEditor, &gpui::ClickEvent, &mut Window, &mut Context<MarkdownEditor>);
        let (large_label, large_badged, large_click, small_icon, small_label, small_click) =
            if typecho_primary {
                (
                    "Typecho",
                    !typecho_configured,
                    Self::publish_to_typecho_click as Click,
                    "icons/notion.png",
                    "Notion",
                    Self::publish_to_notion_click as Click,
                )
            } else {
                (
                    "Notion",
                    !notion_configured,
                    Self::publish_to_notion_click as Click,
                    "icons/typecho.png",
                    "Typecho",
                    Self::publish_to_typecho_click as Click,
                )
            };

        ribbon_controls!(
            ribbon_large_button_badged(
                "icons/publish-large.png",
                large_label,
                large_badged,
                cx.listener(large_click),
            ),
            ribbon_stack!(
                ribbon_small_button(small_icon, small_label, cx.listener(small_click)),
                ribbon_small_button(
                    "icons/settings.png",
                    "发布设置",
                    cx.listener(|editor, _event, window, cx| {
                        editor.toggle_settings(&ToggleSettings, window, cx)
                    }),
                ),
                ribbon_small_button(
                    "icons/preview.png",
                    if self.preview {
                        "返回编辑"
                    } else {
                        "文章预览"
                    },
                    cx.listener(Self::toggle_preview_click),
                ),
            ),
        )
        .into_any_element()
    }

    fn ribbon(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.active_tab {
            0 => div()
                .h(RIBBON_HEIGHT)
                .w_full()
                .flex()
                .id("ribbon-home-scroll")
                .overflow_x_scroll()
                .bg(rgb(RIBBON_BLUE))
                .border_b_1()
                .border_color(rgb(BORDER))
                .px_1()
                .child(ribbon_group(
                    "文档",
                    ribbon_controls!(
                        ribbon_large_button(
                            "icons/new-large.png",
                            "新建",
                            cx.listener(Self::new_document_click),
                        ),
                        ribbon_stack!(
                            ribbon_small_button(
                                "icons/open.png",
                                "打开",
                                cx.listener(Self::open_document_click),
                            ),
                            ribbon_small_button(
                                "icons/save.png",
                                "保存",
                                cx.listener(Self::save_document_click),
                            ),
                        ),
                        ribbon_stack!(
                            ribbon_small_button(
                                "icons/open-draft.png",
                                "打开草稿",
                                cx.listener(Self::open_draft_click),
                            ),
                            ribbon_small_button(
                                "icons/save-draft.png",
                                "保存草稿",
                                cx.listener(Self::save_draft_click),
                            ),
                        ),
                    ),
                ))
                .child(ribbon_group("发布", self.home_publish_controls(cx)))
                .child(ribbon_group(
                    "字体",
                    div()
                        .h_full()
                        .flex()
                        .flex_col()
                        .justify_center()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(ribbon_compact_button(
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
                                .child(ribbon_compact_button(
                                    "icons/italic.png",
                                    "斜体",
                                    cx.listener(|editor, event, window, cx| {
                                        editor.format_button(
                                            "*",
                                            "*",
                                            "已插入斜体 Markdown",
                                            event,
                                            window,
                                            cx,
                                        )
                                    }),
                                ))
                                .child(ribbon_compact_button(
                                    "icons/strike.png",
                                    "删除线",
                                    cx.listener(|editor, _event, window, cx| {
                                        editor.strike_action(&StrikeText, window, cx)
                                    }),
                                ))
                                .child(ribbon_compact_button(
                                    "icons/code.png",
                                    "行内代码",
                                    cx.listener(|editor, _event, window, cx| {
                                        editor.code_action(&CodeText, window, cx)
                                    }),
                                ))
                                .child(ribbon_compact_button(
                                    "icons/undo.png",
                                    "撤销",
                                    cx.listener(Self::undo_click),
                                ))
                                .child(ribbon_compact_button(
                                    "icons/redo.png",
                                    "重做",
                                    cx.listener(Self::redo_click),
                                )),
                        ),
                ))
                .child(ribbon_group(
                    "段落",
                    ribbon_controls!(
                        ribbon_large_button(
                            "icons/paragraph-large.png",
                            "列表",
                            cx.listener(|editor, event, window, cx| {
                                editor.prefix_button("- ", "已切换无序列表", event, window, cx)
                            }),
                        ),
                        ribbon_stack!(
                            ribbon_small_button(
                                "icons/heading.png",
                                "标题",
                                cx.listener(|editor, event, window, cx| {
                                    editor.prefix_button("# ", "已切换标题", event, window, cx)
                                }),
                            ),
                            ribbon_small_button(
                                "icons/bullets.png",
                                "待办清单",
                                cx.listener(|editor, event, window, cx| {
                                    editor.prefix_button(
                                        "- [ ] ",
                                        "已切换待办事项",
                                        event,
                                        window,
                                        cx,
                                    )
                                }),
                            ),
                            ribbon_small_button(
                                "icons/blockquote.png",
                                "引用",
                                cx.listener(|editor, event, window, cx| {
                                    editor.prefix_button("> ", "已切换引用", event, window, cx)
                                }),
                            ),
                        ),
                    ),
                ))
                .child(ribbon_group(
                    "插入",
                    ribbon_controls!(
                        ribbon_stack!(
                            ribbon_small_button(
                                "icons/link.png",
                                "超链接",
                                cx.listener(|editor, _event, window, cx| {
                                    editor.link_action(&LinkText, window, cx)
                                }),
                            ),
                            ribbon_small_button(
                                "icons/image.png",
                                "图片",
                                cx.listener(|editor, _event, window, cx| {
                                    editor.image_action(&ImageText, window, cx)
                                }),
                            ),
                            ribbon_small_button(
                                "icons/video.png",
                                "视频",
                                cx.listener(|editor, _event, window, cx| {
                                    editor.video_action(&VideoText, window, cx)
                                }),
                            ),
                        ),
                        ribbon_stack!(
                            ribbon_small_button(
                                "icons/table.png",
                                "表格",
                                cx.listener(|editor, _event, window, cx| {
                                    editor.table_action(&TableText, window, cx)
                                }),
                            ),
                            ribbon_small_button(
                                "icons/divider.png",
                                "分隔线",
                                cx.listener(|editor, _event, window, cx| {
                                    editor.divider_action(&DividerText, window, cx)
                                }),
                            ),
                            ribbon_small_button(
                                "icons/blockquote.png",
                                "Notion 旁注",
                                cx.listener(|editor, _event, _window, cx| {
                                    editor.insert_snippet(
                                        "\n<aside>💡 提示</aside>\n".to_owned(),
                                        "已插入 Notion 旁注",
                                        cx,
                                    )
                                }),
                            ),
                        ),
                    ),
                ))
                .child(ribbon_group(
                    "编辑",
                    ribbon_controls!(ribbon_large_button(
                        "icons/preview-large.png",
                        if self.preview { "编辑" } else { "预览" },
                        cx.listener(Self::toggle_preview_click),
                    ),),
                ))
                .into_any_element(),
            1 => div()
                .h(RIBBON_HEIGHT)
                .w_full()
                .flex()
                .id("ribbon-insert-scroll")
                .overflow_x_scroll()
                .bg(rgb(RIBBON_BLUE))
                .border_b_1()
                .border_color(rgb(BORDER))
                .px_1()
                .child(ribbon_group(
                    "Markdown 块",
                    ribbon_controls!(
                        ribbon_large_button(
                            "icons/html-large.png",
                            "标题",
                            cx.listener(|editor, event, window, cx| {
                                editor.prefix_button("# ", "已切换标题", event, window, cx)
                            }),
                        ),
                        ribbon_stack!(
                            ribbon_small_button(
                                "icons/bullets.png",
                                "项目列表",
                                cx.listener(|editor, event, window, cx| {
                                    editor.prefix_button("- ", "已切换无序列表", event, window, cx)
                                }),
                            ),
                            ribbon_small_button(
                                "icons/bullets.png",
                                "待办清单",
                                cx.listener(|editor, event, window, cx| {
                                    editor.prefix_button(
                                        "- [ ] ",
                                        "已切换待办事项",
                                        event,
                                        window,
                                        cx,
                                    )
                                }),
                            ),
                            ribbon_small_button(
                                "icons/blockquote.png",
                                "引用",
                                cx.listener(|editor, event, window, cx| {
                                    editor.prefix_button("> ", "已切换引用", event, window, cx)
                                }),
                            ),
                        ),
                        ribbon_stack!(
                            ribbon_small_button(
                                "icons/code.png",
                                "代码",
                                cx.listener(|editor, _event, window, cx| {
                                    editor.code_action(&CodeText, window, cx)
                                }),
                            ),
                            ribbon_small_button(
                                "icons/strike.png",
                                "删除线",
                                cx.listener(|editor, _event, window, cx| {
                                    editor.strike_action(&StrikeText, window, cx)
                                }),
                            ),
                            ribbon_small_button(
                                "icons/blockquote.png",
                                "Notion 旁注",
                                cx.listener(|editor, _event, _window, cx| {
                                    editor.insert_snippet(
                                        "\n<aside>💡 提示</aside>\n".to_owned(),
                                        "已插入 Notion 旁注",
                                        cx,
                                    )
                                }),
                            ),
                        ),
                    ),
                ))
                .child(ribbon_group(
                    "媒体",
                    ribbon_controls!(
                        ribbon_large_button(
                            "icons/image-large.png",
                            "图片",
                            cx.listener(|editor, _event, window, cx| {
                                editor.image_action(&ImageText, window, cx)
                            }),
                        ),
                        ribbon_stack!(
                            ribbon_small_button(
                                "icons/link.png",
                                "超链接",
                                cx.listener(|editor, _event, window, cx| {
                                    editor.link_action(&LinkText, window, cx)
                                }),
                            ),
                            ribbon_small_button(
                                "icons/video.png",
                                "视频",
                                cx.listener(|editor, _event, window, cx| {
                                    editor.video_action(&VideoText, window, cx)
                                }),
                            ),
                        ),
                    ),
                ))
                .child(ribbon_group(
                    "结构",
                    ribbon_controls!(
                        ribbon_large_button(
                            "icons/table-large.png",
                            "表格",
                            cx.listener(|editor, _event, window, cx| {
                                editor.table_action(&TableText, window, cx)
                            }),
                        ),
                        ribbon_stack!(
                            ribbon_small_button(
                                "icons/divider.png",
                                "分隔线",
                                cx.listener(|editor, _event, window, cx| {
                                    editor.divider_action(&DividerText, window, cx)
                                }),
                            ),
                            ribbon_small_button(
                                "icons/preview.png",
                                if self.preview {
                                    "返回编辑"
                                } else {
                                    "实时预览"
                                },
                                cx.listener(Self::toggle_preview_click),
                            ),
                        ),
                    ),
                ))
                .into_any_element(),
            2 => div()
                .h(RIBBON_HEIGHT)
                .w_full()
                .flex()
                .id("ribbon-blog-scroll")
                .overflow_x_scroll()
                .bg(rgb(RIBBON_BLUE))
                .border_b_1()
                .border_color(rgb(BORDER))
                .px_1()
                .child(ribbon_group(
                    "发布",
                    ribbon_controls!(
                        ribbon_large_button_badged(
                            "icons/publish-large.png",
                            "Notion",
                            !(NotionConfig::from_env().is_some()
                                || self.publish_settings.notion_config().is_some()),
                            cx.listener(Self::publish_to_notion_click),
                        ),
                        ribbon_large_button_badged(
                            "icons/publish-large.png",
                            "Typecho",
                            !(TypechoConfig::from_env().is_some()
                                || self.publish_settings.typecho_config().is_some()),
                            cx.listener(Self::publish_to_typecho_click),
                        ),
                    ),
                ))
                .child(ribbon_group(
                    "草稿",
                    ribbon_controls!(
                        ribbon_large_button(
                            "icons/open-large.png",
                            "打开草稿",
                            cx.listener(Self::open_draft_click),
                        ),
                        ribbon_large_button(
                            "icons/save-large.png",
                            "保存草稿",
                            cx.listener(Self::save_draft_click),
                        ),
                    ),
                ))
                .child(ribbon_group(
                    "文章选项",
                    ribbon_controls!(
                        ribbon_large_button(
                            "icons/settings-large.png",
                            "设置",
                            cx.listener(|editor, _event, window, cx| {
                                editor.toggle_settings(&ToggleSettings, window, cx)
                            }),
                        ),
                        ribbon_large_button(
                            "icons/preview-large.png",
                            if self.preview { "编辑" } else { "预览" },
                            cx.listener(Self::toggle_preview_click),
                        ),
                    ),
                ))
                .into_any_element(),
            _ => div()
                .h(RIBBON_HEIGHT)
                .w_full()
                .flex()
                .id("ribbon-file-scroll")
                .overflow_x_scroll()
                .bg(rgb(RIBBON_BLUE))
                .border_b_1()
                .border_color(rgb(BORDER))
                .px_1()
                .child(ribbon_group(
                    "文件",
                    ribbon_controls!(
                        ribbon_large_button(
                            "icons/new-large.png",
                            "新建",
                            cx.listener(Self::new_document_click),
                        ),
                        ribbon_large_button(
                            "icons/open-large.png",
                            "打开",
                            cx.listener(Self::open_document_click),
                        ),
                        ribbon_large_button(
                            "icons/save-large.png",
                            "保存",
                            cx.listener(Self::save_document_click),
                        ),
                    ),
                ))
                .child(ribbon_group(
                    "草稿",
                    ribbon_controls!(
                        ribbon_large_button(
                            "icons/open-large.png",
                            "打开草稿",
                            cx.listener(Self::open_draft_click),
                        ),
                        ribbon_large_button(
                            "icons/save-large.png",
                            "保存草稿",
                            cx.listener(Self::save_draft_click),
                        ),
                    ),
                ))
                .child(ribbon_group(
                    "选项",
                    ribbon_controls!(ribbon_large_button(
                        "icons/settings-large.png",
                        "发布设置",
                        cx.listener(|editor, _event, window, cx| {
                            editor.toggle_settings(&ToggleSettings, window, cx)
                        }),
                    ),),
                ))
                .into_any_element(),
        }
    }
}

impl Render for MarkdownEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let window_title = format!("{title} — Open Live Writer");
        if self.last_window_title != window_title {
            self.last_window_title = window_title.clone();
            window.set_window_title(&window_title);
        }
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
                .flex_row()
                .flex_grow()
                .min_h_0()
                .w_full()
                .p_3()
                .gap_3()
                .on_mouse_move(cx.listener(Self::splitter_mouse_move))
                .on_mouse_up(MouseButton::Left, cx.listener(Self::splitter_mouse_up))
                .on_mouse_up_out(MouseButton::Left, cx.listener(Self::splitter_mouse_up))
                .child(editor_panel(
                    self.text_input.clone(),
                    Some(self.split_ratio),
                ))
                .child(
                    div()
                        .id("editor-preview-splitter")
                        .w(px(6.))
                        .h_full()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .cursor(CursorStyle::ResizeLeftRight)
                        .on_mouse_down(MouseButton::Left, cx.listener(Self::splitter_mouse_down))
                        .hover(|style| style.bg(rgb(0xd5e5f2)).cursor(CursorStyle::ResizeLeftRight))
                        .child(div().w(px(1.)).h_full().bg(rgb(if self.splitter_dragging {
                            BLUE
                        } else {
                            BORDER
                        }))),
                )
                .child(preview_panel(content.clone(), "预览 · 实时 Markdown"))
        };
        let ribbon = self.ribbon(cx);

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
            .child(
                div()
                    .h(RIBBON_TAB_HEIGHT)
                    .w_full()
                    .flex()
                    .items_center()
                    .bg(rgb(0xf5f6f8))
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .px_2()
                    .child(file_tab(
                        self.active_tab == 3,
                        cx.listener(|editor, _, _, cx| {
                            editor.active_tab = 3;
                            cx.notify();
                        }),
                    ))
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
            .child(ribbon)
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

fn prepare_content(content: String) -> (String, String) {
    let line_ending = if content.contains("\r\n") {
        "\r\n".to_owned()
    } else if content.contains('\r') {
        "\r".to_owned()
    } else {
        "\n".to_owned()
    };
    (normalize_newlines(content), line_ending)
}

fn decode_markdown(bytes: &[u8]) -> Result<LoadedDocument, String> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        let content = String::from_utf8(bytes[3..].to_vec())
            .map_err(|error| format!("UTF-8 解码失败：{error}"))?;
        return Ok(LoadedDocument::new(content, FileEncoding::Utf8Bom));
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let content = decode_utf16_bytes(&bytes[2..], true)?;
        return Ok(LoadedDocument::new(content, FileEncoding::Utf16Le));
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let content = decode_utf16_bytes(&bytes[2..], false)?;
        return Ok(LoadedDocument::new(content, FileEncoding::Utf16Be));
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(LoadedDocument::new(text.to_owned(), FileEncoding::Utf8));
    }
    let (decoded, _, _) = encoding_rs::GBK.decode(bytes);
    Ok(LoadedDocument::new(decoded.into_owned(), FileEncoding::Gbk))
}

fn decode_utf16_bytes(bytes: &[u8], little_endian: bool) -> Result<String, String> {
    if bytes.len() % 2 != 0 {
        return Err("UTF-16 文件损坏：字节长度不是偶数".to_owned());
    }
    let units = bytes
        .chunks_exact(2)
        .map(|chunk| {
            if little_endian {
                u16::from_le_bytes([chunk[0], chunk[1]])
            } else {
                u16::from_be_bytes([chunk[0], chunk[1]])
            }
        })
        .collect::<Vec<_>>();
    String::from_utf16(&units).map_err(|error| format!("UTF-16 解码失败：{error}"))
}

fn encode_markdown(content: &str, encoding: FileEncoding) -> Result<Vec<u8>, String> {
    match encoding {
        FileEncoding::Utf8 => Ok(content.as_bytes().to_vec()),
        FileEncoding::Utf8Bom => {
            let mut bytes = vec![0xEF, 0xBB, 0xBF];
            bytes.extend_from_slice(content.as_bytes());
            Ok(bytes)
        }
        FileEncoding::Utf16Le => {
            let mut bytes = vec![0xFF, 0xFE];
            for unit in content.encode_utf16() {
                bytes.extend_from_slice(&unit.to_le_bytes());
            }
            Ok(bytes)
        }
        FileEncoding::Utf16Be => {
            let mut bytes = vec![0xFE, 0xFF];
            for unit in content.encode_utf16() {
                bytes.extend_from_slice(&unit.to_be_bytes());
            }
            Ok(bytes)
        }
        FileEncoding::Gbk => {
            let (encoded, _, had_errors) = encoding_rs::GBK.encode(content);
            if had_errors {
                Err("保存失败：当前内容包含 GBK 无法表示的字符".to_owned())
            } else {
                Ok(encoded.into_owned())
            }
        }
    }
}

fn default_save_directory() -> PathBuf {
    if cfg!(target_os = "windows")
        && let Some(profile) = std::env::var_os("USERPROFILE")
    {
        let documents = PathBuf::from(profile).join("Documents");
        if documents.is_dir() {
            return documents;
        }
    }
    if cfg!(target_os = "macos")
        && let Some(home) = std::env::var_os("HOME")
    {
        let documents = PathBuf::from(home).join("Documents");
        if documents.is_dir() {
            return documents;
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn local_image_warning(markdown: &str) -> String {
    let count = local_image_count(markdown);
    if count > 0 {
        format!("（含 {count} 处本地/相对图片，发布后可能无法直接显示）")
    } else {
        String::new()
    }
}

fn utf16_offset_from_utf8(text: &str, byte_offset: usize) -> usize {
    let mut utf16 = 0;
    for (index, character) in text.char_indices() {
        if index >= byte_offset {
            break;
        }
        utf16 += character.len_utf16();
    }
    utf16
}

fn utf16_range_from_utf8(text: &str, range: Range<usize>) -> Range<usize> {
    utf16_offset_from_utf8(text, range.start)..utf16_offset_from_utf8(text, range.end)
}

#[cfg(test)]
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

#[cfg(test)]
fn utf16_range_to_utf8(text: &str, range: &Range<usize>) -> Range<usize> {
    utf8_offset_from_utf16(text, range.start)..utf8_offset_from_utf16(text, range.end)
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
    let path = percent_encode_path(&path.to_string_lossy().replace('\\', "/"));
    if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{path}")
    }
}

fn percent_encode_path(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b':' | b'/' | b'.' | b'-' | b'_' | b'~' => {
                encoded.push(byte as char)
            }
            _ => {
                encoded.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    encoded
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
        {
            decoded.push(high * 16 + low);
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn preview_image_source(url: &str) -> ImageSource {
    let decoded = percent_decode(url);
    if let Some(path) = decoded.strip_prefix("file:///") {
        ImageSource::from(PathBuf::from(path))
    } else if let Some(path) = decoded.strip_prefix("file://") {
        ImageSource::from(PathBuf::from(path))
    } else {
        ImageSource::from(decoded)
    }
}

fn editor_panel(input: Entity<MarkdownInput>, width: Option<f32>) -> impl IntoElement {
    let mut panel = div()
        .flex()
        .flex_col()
        .flex_grow()
        .min_w_0()
        .min_h_0()
        .bg(white())
        .border_1()
        .border_color(rgb(BORDER))
        .shadow_sm();
    if let Some(width) = width {
        panel = panel.w(relative(width)).flex_none();
    }
    panel
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
                .w_full()
                .min_h_0()
                .min_w_0()
                .overflow_y_scroll()
                .overflow_x_hidden()
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
                .w_full()
                .min_h_0()
                .min_w_0()
                .overflow_y_scroll()
                .overflow_x_hidden()
                .whitespace_normal()
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
            "配置后，发布按钮会直接调用服务；环境变量优先于这里保存的设置，凭据仅保存到系统安全存储。未配置时仍可复制 Markdown。",
        ))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_4()
                .w_full()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_grow()
                        .min_w(px(320.))
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
                        .min_w(px(320.))
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
    let background = if active { rgb(0xffffff) } else { rgb(0xf5f6f8) };
    let foreground = if active { rgb(DARK_BLUE) } else { rgb(TEXT) };
    div()
        .id(label)
        .h(RIBBON_TAB_HEIGHT)
        .px_4()
        .flex()
        .items_center()
        .bg(background)
        .border_1()
        .border_color(if active { rgb(BORDER) } else { rgb(0xf5f6f8) })
        .text_size(px(14.))
        .text_color(foreground)
        .hover(|style| {
            style
                .bg(if active { rgb(0xffffff) } else { rgb(0xe7f0f9) })
                .cursor_pointer()
        })
        .active(|style| style.bg(rgb(0xd7e8f6)))
        .on_click(on_click)
        .child(label)
}

fn file_tab(
    active: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id("文件")
        .h(RIBBON_TAB_HEIGHT)
        .px_5()
        .flex()
        .items_center()
        .bg(if active { rgb(DARK_BLUE) } else { rgb(BLUE) })
        .text_size(px(14.))
        .text_color(white())
        .hover(|style| style.bg(rgb(DARK_BLUE)).cursor_pointer())
        .active(|style| style.bg(rgb(0x244f7c)))
        .on_click(on_click)
        .child("文件")
}

fn ribbon_group(label: &'static str, controls: impl IntoElement) -> impl IntoElement {
    div()
        .h_full()
        .flex()
        .flex_col()
        .flex_none()
        .px_1()
        .pt(px(2.))
        .border_r_1()
        .border_color(rgb(RIBBON_SEPARATOR))
        .child(
            div()
                .flex()
                .flex_grow()
                .items_center()
                .justify_center()
                .child(controls),
        )
        .child(
            div()
                .h(px(16.))
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(rgb(0x75879a))
                .child(label),
        )
}

fn ribbon_large_button(
    icon: &'static str,
    label: &'static str,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    ribbon_large_button_badged(icon, label, false, on_click)
}

fn ribbon_large_button_badged(
    icon: &'static str,
    label: &'static str,
    badged: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(label)
        .w(RIBBON_LARGE_BUTTON_WIDTH)
        .h(RIBBON_LARGE_BUTTON_HEIGHT)
        .flex()
        .flex_col()
        .flex_none()
        .items_center()
        .justify_center()
        .gap_1()
        .px_1()
        .rounded_sm()
        .border_1()
        .border_color(hsla(0., 0., 0., 0.))
        .text_xs()
        .text_color(rgb(TEXT))
        .hover(|style| {
            style
                .bg(linear_gradient(
                    0.,
                    linear_color_stop(rgb(0xffffff), 0.),
                    linear_color_stop(rgb(0xd9eaf8), 1.),
                ))
                .border_color(rgb(0x8db6d9))
                .shadow_sm()
                .cursor_pointer()
        })
        .active(|style| {
            style
                .bg(linear_gradient(
                    0.,
                    linear_color_stop(rgb(0xb8d3eb), 0.),
                    linear_color_stop(rgb(0xe7f3fc), 1.),
                ))
                .border_color(rgb(0x5b91c2))
                .shadow_none()
        })
        .on_click(on_click)
        .child(
            div()
                .w(px(34.))
                .h(px(34.))
                .relative()
                .flex()
                .items_center()
                .justify_center()
                .child(ribbon_icon(icon).w(px(32.)).h(px(32.)))
                .when(badged, |this| {
                    this.child(
                        div()
                            .absolute()
                            .top(px(0.))
                            .right(px(0.))
                            .w(px(9.))
                            .h(px(9.))
                            .rounded_full()
                            .bg(rgb(0xd64545)),
                    )
                }),
        )
        .child(
            div()
                .h(px(18.))
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .text_center()
                .child(label),
        )
}

fn ribbon_small_button(
    icon: &'static str,
    label: &'static str,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(label)
        .min_w(px(84.))
        .h(RIBBON_SMALL_BUTTON_HEIGHT)
        .flex()
        .flex_none()
        .items_center()
        .gap_1()
        .px_1()
        .rounded_sm()
        .border_1()
        .border_color(hsla(0., 0., 0., 0.))
        .text_size(px(12.))
        .text_color(rgb(TEXT))
        .hover(|style| {
            style
                .bg(linear_gradient(
                    0.,
                    linear_color_stop(rgb(0xffffff), 0.),
                    linear_color_stop(rgb(0xdcecf9), 1.),
                ))
                .border_color(rgb(0x8db6d9))
                .cursor_pointer()
        })
        .active(|style| style.bg(rgb(0xc8dff2)).border_color(rgb(0x5b91c2)))
        .on_click(on_click)
        .child(
            div()
                .w(px(18.))
                .h(px(18.))
                .flex()
                .items_center()
                .justify_center()
                .child(ribbon_icon(icon).size_4()),
        )
        .child(label)
}

fn ribbon_compact_button(
    icon: &'static str,
    label: &'static str,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(label)
        .w(px(28.))
        .h(px(23.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded_sm()
        .border_1()
        .border_color(hsla(0., 0., 0., 0.))
        .hover(|style| {
            style
                .bg(linear_gradient(
                    0.,
                    linear_color_stop(rgb(0xffffff), 0.),
                    linear_color_stop(rgb(0xdcecf9), 1.),
                ))
                .border_color(rgb(0x8db6d9))
                .cursor_pointer()
        })
        .active(|style| style.bg(rgb(0xc8dff2)).border_color(rgb(0x5b91c2)))
        .on_click(on_click)
        .child(ribbon_icon(icon).size_4())
}

fn ribbon_icon(path: &'static str) -> Img {
    static CACHE: OnceLock<Mutex<HashMap<&'static str, Arc<Image>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let image = cache
        .lock()
        .unwrap()
        .entry(path)
        .or_insert_with(|| {
            let asset = EmbeddedAssets::get(path).expect("missing embedded ribbon icon");
            Arc::new(Image::from_bytes(ImageFormat::Png, asset.data.into_owned()))
        })
        .clone();
    img(image)
}

fn markdown_preview(markdown: &str) -> Vec<gpui::AnyElement> {
    let blocks = parse_blocks(markdown);
    if blocks.is_empty() {
        return vec![
            div()
                .w_full()
                .p_4()
                .text_color(rgb(MUTED))
                .child("开始输入 Markdown，这里会显示预览。")
                .into_any_element(),
        ];
    }
    blocks
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
            Block::Task { checked, text } => {
                let checkbox = div()
                    .w(px(18.))
                    .h(px(18.))
                    .mt(px(3.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .border_1()
                    .border_color(rgb(BLUE))
                    .when(checked, |this| {
                        this.bg(rgb(BLUE)).text_color(white()).child("✓")
                    })
                    .when(!checked, |this| this.bg(white()));
                div()
                    .id(("preview-task", index))
                    .w_full()
                    .mb_1()
                    .pl_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_base()
                    .child(checkbox)
                    .child(
                        div()
                            .when(checked, |this| this.text_color(rgb(MUTED)).line_through())
                            .child(inline_preview(&text)),
                    )
                    .into_any_element()
            }
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
            Block::Aside(text) => {
                let content = if text.trim().is_empty() {
                    "旁注".to_owned()
                } else {
                    text
                };
                div()
                    .id(("preview-aside", index))
                    .w_full()
                    .mb_3()
                    .p_3()
                    .flex()
                    .gap_2()
                    .rounded_sm()
                    .border_l_4()
                    .border_color(rgb(0x72a7d8))
                    .bg(rgb(0xeaf3fc))
                    .child(div().text_lg().child("💡"))
                    .child(
                        div()
                            .flex_grow()
                            .min_w_0()
                            .line_height(px(24.))
                            .child(inline_preview(&content)),
                    )
                    .into_any_element()
            }
            Block::Code { text, language } => div()
                .id(("preview-code", index))
                .w_full()
                .mb_3()
                .p_3()
                .rounded_sm()
                .bg(rgb(0xf1f3f5))
                .border_1()
                .border_color(rgb(0xdfe3e8))
                .text_color(rgb(0x38434d))
                .when(language.is_some(), |this| {
                    this.child(
                        div()
                            .mb_1()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(language.clone().unwrap_or_default()),
                    )
                })
                .child(
                    div()
                        .w_full()
                        .font_family("Menlo, Monaco, Consolas, monospace")
                        .child(text),
                )
                .into_any_element(),
            Block::Image { alt, url } => div()
                .id(("preview-image", index))
                .w_full()
                .mb_3()
                .flex()
                .flex_col()
                .gap_1()
                .child(img(preview_image_source(&url)).max_w_full())
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
            Block::Html(text) => div()
                .id(("preview-html", index))
                .w_full()
                .mb_3()
                .p_3()
                .rounded_sm()
                .border_1()
                .border_color(rgb(BORDER))
                .bg(rgb(0xf7f9fb))
                .text_color(rgb(0x38434d))
                .child(div().text_xs().text_color(rgb(MUTED)).child("HTML"))
                .child(text)
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
    let children = parse_inline(markdown)
        .iter()
        .map(preview_inline_piece)
        .collect::<Vec<_>>();
    div()
        .w_full()
        .max_w_full()
        .min_w_0()
        .flex()
        .flex_wrap()
        .whitespace_normal()
        .overflow_x_hidden()
        .children(children)
        .into_any_element()
}

fn preview_inline_piece(piece: &RichTextPiece) -> gpui::AnyElement {
    if let Some(url) = &piece.image {
        return img(preview_image_source(url))
            .max_h(px(160.))
            .max_w_full()
            .into_any_element();
    }
    let mut element = div()
        .min_w_0()
        .max_w_full()
        .flex_shrink()
        .whitespace_normal()
        .child(piece.text.clone());
    for style in &piece.styles {
        element = match style {
            InlineStyle::Bold => element.font_weight(FontWeight(700.)),
            InlineStyle::Italic => element.italic(),
            InlineStyle::Strike => element.line_through(),
            InlineStyle::Code => element.bg(rgb(0xf1f3f5)).px_1(),
        };
    }
    if piece.link.is_some() {
        element = element.text_color(rgb(BLUE)).underline();
    }
    element.into_any_element()
}

fn main() {
    Application::new().with_assets(Assets).run(|cx: &mut App| {
        gpui_component::init(cx);
        // The application uses a fixed light Windows Live Writer palette. Keep the component
        // editor in the matching light theme instead of inheriting the system dark theme.
        Theme::change(ThemeMode::Light, None, cx);
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
            KeyBinding::new("secondary-shift-p", TogglePreview, None),
            KeyBinding::new("secondary-comma", ToggleSettings, None),
            KeyBinding::new("secondary-shift-enter", PublishToNotion, None),
            KeyBinding::new("secondary-shift-t", PublishToTypecho, None),
            KeyBinding::new("secondary-q", QuitApplication, None),
        ]);
        // GPUI uses logical pixels. At 200% Windows scaling this opens at roughly
        // 2260 x 1500 physical pixels, wide enough to show the complete Home ribbon.
        let bounds = Bounds::centered(None, size(px(1130.), px(750.)), cx);
        let mut editor_entity = None;
        let mut editor_focus_handle = None;
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("Open Live Writer".into()),
                        ..Default::default()
                    }),
                    window_min_size: Some(size(px(900.), px(600.))),
                    ..Default::default()
                },
                |window, cx| {
                    let editor = cx.new(|cx| MarkdownEditor::new(window, cx));
                    editor_focus_handle =
                        Some(editor.read(cx).text_input.read(cx).focus_handle(cx));
                    editor_entity = Some(editor.clone());
                    cx.new(|cx| gpui_component::Root::new(editor, window, cx))
                },
            )
            .expect("failed to open the editor window");
        let editor_entity = editor_entity.expect("editor view was not created");
        let editor_focus_handle = editor_focus_handle.expect("editor focus handle was not created");
        window
            .update(cx, |_, window, cx| {
                window.focus(&editor_focus_handle);
                let editor = editor_entity.downgrade();
                if let Ok(Some(content)) = storage::load_autosave() {
                    let autosave_editor = editor_entity.downgrade();
                    window
                        .spawn(cx, async move |cx| {
                            let answer = cx.update(|window, cx| {
                                window.prompt(
                                    gpui::PromptLevel::Warning,
                                    "检测到未保存的自动备份",
                                    Some("上次退出时存在未保存的修改，是否恢复？"),
                                    &["恢复备份", "放弃备份"],
                                    cx,
                                )
                            });
                            let Some(answer) = answer.ok() else {
                                return;
                            };
                            let choice = answer.await.ok();
                            let _ = autosave_editor.update(cx, |editor, cx| {
                                if choice == Some(0) {
                                    editor.restore_autosave(content.clone(), cx);
                                }
                                let _ = storage::clear_autosave();
                            });
                        })
                        .detach();
                }
                window.on_window_should_close(cx, move |window, cx| {
                    let (dirty, close_confirmed, path) = editor
                        .read_with(cx, |editor, _| {
                            (editor.dirty, editor.close_confirmed, editor.path.clone())
                        })
                        .unwrap_or((false, false, None));
                    if !dirty || close_confirmed {
                        return true;
                    }
                    let answer = window.prompt(
                        gpui::PromptLevel::Warning,
                        "当前文章有未保存的修改",
                        Some("请选择保存并关闭、放弃修改并关闭，或取消。"),
                        &["保存并关闭", "放弃并关闭", "取消"],
                        cx,
                    );
                    let editor_for_prompt = editor.clone();
                    window
                        .spawn(cx, async move |cx| match answer.await.ok() {
                            Some(0) => {
                                if let Some(path) = path.clone() {
                                    let saved = editor_for_prompt.update(cx, |editor, cx| {
                                        if editor.save_to(path, cx) {
                                            editor.close_confirmed = true;
                                            cx.notify();
                                            true
                                        } else {
                                            false
                                        }
                                    });
                                    if saved.unwrap_or(false) {
                                        let _ = cx.update(|window, _| window.remove_window());
                                    }
                                    return;
                                }
                                let Ok(receiver) = cx.update(|_, cx| {
                                    cx.prompt_for_new_path(
                                        &default_save_directory(),
                                        Some("未命名文章.md"),
                                    )
                                }) else {
                                    return;
                                };
                                let Ok(Ok(Some(path))) = receiver.await else {
                                    return;
                                };
                                let saved = editor_for_prompt.update(cx, |editor, cx| {
                                    if editor.save_to(path, cx) {
                                        editor.close_confirmed = true;
                                        cx.notify();
                                        true
                                    } else {
                                        false
                                    }
                                });
                                if saved.unwrap_or(false) {
                                    let _ = cx.update(|window, _| window.remove_window());
                                }
                            }
                            Some(1) => {
                                let _ = editor_for_prompt.update(cx, |editor, cx| {
                                    editor.close_confirmed = true;
                                    let _ = storage::clear_autosave();
                                    cx.notify();
                                });
                                let _ = cx.update(|window, _| window.remove_window());
                            }
                            _ => {}
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
        EmbeddedAssets, is_image_path, line_end, line_starts, markdown_image_url,
        normalize_newlines, percent_decode, typecho_is_primary_publish_target, utf16_range_to_utf8,
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
        assert_eq!(
            markdown_image_url(path),
            "file:///tmp/%E5%B0%81%E9%9D%A2.png"
        );
        assert_eq!(percent_decode("%E5%B0%81%E9%9D%A2"), "封面");
        assert_eq!(percent_decode("a%20b.png"), "a b.png");
        assert!(!is_image_path(Path::new("/tmp/article.md")));
    }

    #[test]
    fn promotes_the_only_configured_publish_target() {
        assert!(typecho_is_primary_publish_target(false, true));
        assert!(!typecho_is_primary_publish_target(true, false));
        assert!(!typecho_is_primary_publish_target(true, true));
        assert!(!typecho_is_primary_publish_target(false, false));
    }

    #[test]
    fn embeds_ribbon_icons() {
        assert!(EmbeddedAssets::get("icons/new.png").is_some());
        assert!(EmbeddedAssets::get("icons/new-large.png").is_some());
        assert!(EmbeddedAssets::get("icons/publish-large.png").is_some());
        assert!(EmbeddedAssets::get("icons/paragraph-large.png").is_some());
        assert!(EmbeddedAssets::get("icons/preview-large.png").is_some());
        assert!(EmbeddedAssets::get("icons/open-draft.png").is_some());
        assert!(EmbeddedAssets::get("icons/save-draft.png").is_some());
        assert!(EmbeddedAssets::get("icons/divider.png").is_some());
        assert!(EmbeddedAssets::get("icons/notion.png").is_some());
        assert!(EmbeddedAssets::get("icons/typecho.png").is_some());
        assert!(EmbeddedAssets::get("Writer.ico").is_some());
    }
}
