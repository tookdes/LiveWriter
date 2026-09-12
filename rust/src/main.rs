#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod editing;
mod markdown;
mod publishing;
mod storage;

use std::{
    backtrace::Backtrace,
    borrow::Cow,
    collections::HashMap,
    fs,
    io::Write,
    ops::Range,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use gpui::{
    AnyElement, App, Application, AssetSource, Bounds, ClipboardEntry, ClipboardItem, Context,
    CursorStyle, Entity, EntityInputHandler, ExternalPaths, FocusHandle, Focusable, FontStyle,
    FontWeight, HighlightStyle, Image, ImageFormat, ImageSource, Img, KeyBinding, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathPromptOptions, Pixels, PromptLevel,
    ScrollHandle, SharedString, StrikethroughStyle, StyledText, UnderlineStyle, Window,
    WindowBounds, WindowOptions, actions, div, hsla, img, linear_color_stop, linear_gradient,
    point, prelude::*, px, relative, rgb, size, white,
};
use gpui_component::{
    RopeExt, Theme, ThemeMode, WindowExt,
    input::{Input, InputEvent, InputState},
    notification::Notification,
    tooltip::Tooltip,
};
use rust_embed::Embed;

use crate::editing::{
    character_count, document_outline, is_clipboard_url, looks_like_url, markdown_code_block,
    markdown_link, markdown_table, normalize_url, selection_has_wrap, set_heading_level,
    toggle_prefixes, wrap_or_unwrap,
};
use crate::markdown::{Block, InlineStyle, parse_blocks, parse_inline};
use crate::publishing::{
    CREDENTIALS_URL, CREDENTIALS_USERNAME, NotionConfig, StoredPublishSettings, TypechoConfig,
    default_http_client, document_title, local_image_count, local_images,
    publish_or_update_typecho, publish_to_notion as publish_notion_request, test_notion_connection,
    test_typecho_connection,
};
use crate::storage::{DocumentMeta, DraftEntry, EditorPrefs, WorkspaceMode, crash_log_path};

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
        SaveAsDocument,
        SaveDraft,
        TogglePreview,
        CycleWorkspace,
        ToggleFocus,
        ToggleOutline,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        Heading2Text,
        Heading3Text,
        CodeBlockText,
        PasteImage,
        ShowDrafts,
        ShowRecent,
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
    pub font_size: f32,
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
            font_size: if multi_line { 16. } else { 14. },
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

    fn current_range(&self, window: &mut Window, cx: &mut Context<Self>) -> std::ops::Range<usize> {
        self.state.update(cx, |state, cx| {
            let Some(selection) = state.selected_text_range(false, window, cx) else {
                let cursor = state.cursor();
                return cursor..cursor;
            };
            state.text().offset_utf16_to_offset(selection.range.start)
                ..state.text().offset_utf16_to_offset(selection.range.end)
        })
    }

    fn apply_document_edit(
        &mut self,
        next: String,
        cursor: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.state.clone();
        state.update(cx, |state, cx| {
            let full = state.text().to_string();
            if full == next {
                return;
            }
            let range_utf16 = utf16_range_from_utf8(&full, 0..full.len());
            <InputState as EntityInputHandler>::replace_text_in_range(
                state,
                Some(range_utf16),
                &next,
                window,
                cx,
            );
            let cursor = cursor.min(state.text().len());
            let position = state.text().offset_to_position(cursor);
            state.set_cursor_position(position, window, cx);
        });
    }

    fn wrap_selection(
        &mut self,
        prefix: &str,
        suffix: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.current_range(window, cx);
        let full = self.state.read(cx).text().to_string();
        let (next, next_range) = wrap_or_unwrap(&full, range, prefix, suffix);
        self.apply_document_edit(next, next_range.end, window, cx);
    }

    #[allow(dead_code)]
    fn has_wrap(
        &self,
        prefix: &str,
        suffix: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let range = self.current_range(window, cx);
        let full = self.state.read(cx).text().to_string();
        selection_has_wrap(&full, range, prefix, suffix)
    }

    fn toggle_line_prefix(&mut self, prefix: &str, window: &mut Window, cx: &mut Context<Self>) {
        let range = self.current_range(window, cx);
        let full = self.state.read(cx).text().to_string();
        let (next, cursor) = toggle_prefixes(&full, range, prefix);
        self.apply_document_edit(next, cursor, window, cx);
    }

    fn set_heading(&mut self, level: u8, window: &mut Window, cx: &mut Context<Self>) {
        let range = self.current_range(window, cx);
        let full = self.state.read(cx).text().to_string();
        let (next, cursor) = set_heading_level(&full, range, level);
        self.apply_document_edit(next, cursor, window, cx);
    }

    #[allow(dead_code)]
    fn cycle_heading(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_line_prefix("# ", window, cx);
    }

    #[allow(dead_code)]
    fn current_line(&self, cx: &App) -> String {
        let full = self.state.read(cx).text().to_string();
        let cursor = self.state.read(cx).cursor();
        let starts = crate::editing::line_starts(&full);
        let line = crate::editing::line_index(&starts, cursor);
        let start = starts[line];
        let end = crate::editing::line_end(&starts, line, full.len());
        full[start..end].to_owned()
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
            .text_size(px(self.font_size));
        if self.multi_line {
            input = input
                .h_full()
                .line_height(px((self.font_size * 1.75).round()));
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

impl Focusable for MarkdownInput {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.state.read(cx).focus_handle(cx)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum EditorDialog {
    None,
    Link,
    Table,
    #[allow(dead_code)]
    Heading,
    PublishNotion,
    PublishTypecho,
    Drafts,
    Recent,
    #[allow(dead_code)]
    Lightbox {
        url: String,
        alt: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PublishKind {
    Notion,
    Typecho,
}

struct MarkdownEditor {
    text_input: Entity<MarkdownInput>,
    notion_token_input: Entity<MarkdownInput>,
    notion_parent_input: Entity<MarkdownInput>,
    typecho_url_input: Entity<MarkdownInput>,
    typecho_username_input: Entity<MarkdownInput>,
    typecho_password_input: Entity<MarkdownInput>,
    link_text_input: Entity<MarkdownInput>,
    link_url_input: Entity<MarkdownInput>,
    publish_title_input: Entity<MarkdownInput>,
    path: Option<PathBuf>,
    preview: bool,
    workspace_mode: WorkspaceMode,
    focus_mode: bool,
    outline_visible: bool,
    font_size: f32,
    active_tab: usize,
    split_ratio: f32,
    splitter_dragging: bool,
    settings_visible: bool,
    dialog: EditorDialog,
    dirty: bool,
    status: SharedString,
    publish_settings: StoredPublishSettings,
    last_observed_content: String,
    suppress_observer: bool,
    close_confirmed: bool,
    line_ending: String,
    file_encoding: FileEncoding,
    last_draft_content: Option<String>,
    last_window_title: String,
    last_backup_at: Option<u64>,
    recent_files: Vec<PathBuf>,
    drafts: Vec<DraftEntry>,
    publishing: bool,
    publish_update_existing: bool,
    publish_public: bool,
    document_meta: DocumentMeta,
    pending_media: Vec<PathBuf>,
    preview_source: SharedString,
    preview_generation: u64,
    editor_scroll: ScrollHandle,
    preview_scroll: ScrollHandle,
    last_editor_scroll: f32,
    last_preview_scroll: f32,
    table_rows: usize,
    table_cols: usize,
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
        let link_text_input = Self::new_settings_input(window, cx, String::new(), false);
        let link_url_input = Self::new_settings_input(window, cx, String::new(), false);
        let publish_title_input = Self::new_settings_input(window, cx, String::new(), false);
        let prefs = storage::load_prefs();
        text_input.update(cx, |input, cx| {
            input.font_size = prefs.font_size;
            cx.notify();
        });
        let subscription = cx.observe(&text_input, |editor, input, cx| {
            if editor.suppress_observer {
                return;
            }
            let content = input.read(cx).content.to_string();
            if content != editor.last_observed_content {
                editor.last_observed_content = content;
                editor.dirty = true;
                editor.status = "正在编辑 · 尚未保存".into();
                editor.schedule_preview(cx);
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
            link_text_input,
            link_url_input,
            publish_title_input,
            path: None,
            preview: prefs.workspace_mode == WorkspaceMode::Preview,
            workspace_mode: prefs.workspace_mode,
            focus_mode: prefs.focus_mode,
            outline_visible: prefs.outline_visible,
            font_size: prefs.font_size,
            active_tab: 0,
            split_ratio: prefs.split_ratio,
            splitter_dragging: false,
            settings_visible: false,
            dialog: EditorDialog::None,
            dirty: false,
            status: "就绪 · Markdown 模式".into(),
            last_observed_content: initial.clone(),
            suppress_observer: false,
            close_confirmed: false,
            line_ending: "\n".to_owned(),
            file_encoding: FileEncoding::Utf8,
            last_draft_content: None,
            last_window_title: "Open Live Writer".to_owned(),
            last_backup_at: storage::autosave_modified_secs(),
            recent_files: storage::load_recent_files(),
            drafts: storage::list_drafts(),
            publishing: false,
            publish_update_existing: false,
            publish_public: true,
            document_meta: DocumentMeta::default(),
            pending_media: Vec::new(),
            preview_source: initial.clone().into(),
            preview_generation: 0,
            editor_scroll: ScrollHandle::new(),
            preview_scroll: ScrollHandle::new(),
            last_editor_scroll: 0.,
            last_preview_scroll: 0.,
            table_rows: 2,
            table_cols: 2,
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
                        // 内容与最近一次草稿一致时无需重复备份。
                        if editor.last_draft_content.as_deref() == Some(content.as_str()) {
                            return;
                        }
                        match storage::save_autosave(&content) {
                            Ok(()) => {
                                editor.last_draft_content = Some(content);
                                editor.last_backup_at = storage::autosave_modified_secs();
                            }
                            Err(error) => eprintln!(
                                "\u{81ea}\u{52a8}\u{5907}\u{4efd}\u{5931}\u{8d25}\u{ff1a}{error}"
                            ),
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

    fn persist_prefs(&self) {
        let prefs = EditorPrefs {
            split_ratio: self.split_ratio,
            workspace_mode: self.workspace_mode,
            font_size: self.font_size,
            last_directory: self
                .path
                .as_ref()
                .and_then(|path| path.parent().map(Path::to_path_buf))
                .or_else(|| storage::load_prefs().last_directory),
            outline_visible: self.outline_visible,
            focus_mode: self.focus_mode,
        };
        let _ = storage::save_prefs(&prefs);
    }

    fn notify_window(
        window: &mut Window,
        cx: &mut App,
        success: bool,
        title: impl Into<SharedString>,
        message: impl Into<SharedString>,
    ) {
        let notification = if success {
            Notification::success(message).title(title)
        } else {
            Notification::error(message).title(title).autohide(false)
        };
        window.push_notification(notification, cx);
    }

    fn set_status(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.status = message.into();
        cx.notify();
    }

    fn close_dialog(&mut self, cx: &mut Context<Self>) {
        self.dialog = EditorDialog::None;
        cx.notify();
    }

    fn apply_font_size(&mut self, cx: &mut Context<Self>) {
        let size = self.font_size;
        self.text_input.update(cx, |input, cx| {
            input.font_size = size;
            cx.notify();
        });
        self.persist_prefs();
        cx.notify();
    }

    fn cycle_workspace(
        &mut self,
        _: &CycleWorkspace,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_visible = false;
        self.workspace_mode = self.workspace_mode.cycle();
        self.preview = self.workspace_mode == WorkspaceMode::Preview;
        self.persist_prefs();
        self.set_status(
            format!("{} \u{6a21}\u{5f0f}", self.workspace_mode.label()),
            cx,
        );
    }

    #[allow(dead_code)]
    fn set_workspace_mode(&mut self, mode: WorkspaceMode, cx: &mut Context<Self>) {
        self.settings_visible = false;
        self.workspace_mode = mode;
        self.preview = mode == WorkspaceMode::Preview;
        self.persist_prefs();
        cx.notify();
    }

    fn toggle_focus(&mut self, _: &ToggleFocus, _window: &mut Window, cx: &mut Context<Self>) {
        self.focus_mode = !self.focus_mode;
        self.persist_prefs();
        self.set_status(
            if self.focus_mode {
                "\u{4e13}\u{6ce8}\u{6a21}\u{5f0f}"
            } else {
                "\u{5df2}\u{9000}\u{51fa}\u{4e13}\u{6ce8}\u{6a21}\u{5f0f}"
            },
            cx,
        );
    }

    fn toggle_outline(&mut self, _: &ToggleOutline, _window: &mut Window, cx: &mut Context<Self>) {
        self.outline_visible = !self.outline_visible;
        self.persist_prefs();
        cx.notify();
    }

    fn zoom_in(&mut self, _: &ZoomIn, _window: &mut Window, cx: &mut Context<Self>) {
        self.font_size = (self.font_size + 1.).min(28.);
        self.apply_font_size(cx);
    }

    fn zoom_out(&mut self, _: &ZoomOut, _window: &mut Window, cx: &mut Context<Self>) {
        self.font_size = (self.font_size - 1.).max(12.);
        self.apply_font_size(cx);
    }

    fn zoom_reset(&mut self, _: &ZoomReset, _window: &mut Window, cx: &mut Context<Self>) {
        self.font_size = 16.;
        self.apply_font_size(cx);
    }

    fn schedule_preview(&mut self, cx: &mut Context<Self>) {
        let content = self.content(cx);
        self.preview_generation = self.preview_generation.wrapping_add(1);
        let generation = self.preview_generation;
        cx.spawn(async move |editor, cx| {
            gpui::Timer::after(std::time::Duration::from_millis(80)).await;
            let _ = editor.update(cx, |editor, cx| {
                if editor.preview_generation == generation {
                    editor.preview_source = content.into();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn sync_scroll_offsets(&mut self) {
        let editor_y: f32 = self.editor_scroll.offset().y.into();
        let preview_y: f32 = self.preview_scroll.offset().y.into();
        let editor_max: f32 = self.editor_scroll.max_offset().height.into();
        let preview_max: f32 = self.preview_scroll.max_offset().height.into();
        if (editor_y - self.last_editor_scroll).abs() > 1. && editor_max > 1. && preview_max > 1. {
            let ratio = (editor_y / editor_max).clamp(0., 1.);
            self.preview_scroll
                .set_offset(point(px(0.), px(preview_max * ratio)));
            self.last_editor_scroll = editor_y;
            self.last_preview_scroll = preview_max * ratio;
        } else if (preview_y - self.last_preview_scroll).abs() > 1.
            && editor_max > 1.
            && preview_max > 1.
        {
            let ratio = (preview_y / preview_max).clamp(0., 1.);
            self.editor_scroll
                .set_offset(point(px(0.), px(editor_max * ratio)));
            self.last_preview_scroll = preview_y;
            self.last_editor_scroll = editor_max * ratio;
        }
    }

    fn jump_to_offset(&mut self, offset: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.text_input.update(cx, |input, cx| {
            input.state.update(cx, |state, cx| {
                let offset = offset.min(state.text().len());
                let position = state.text().offset_to_position(offset);
                state.set_cursor_position(position, window, cx);
            });
        });
        self.workspace_mode = WorkspaceMode::Split;
        self.preview = false;
        cx.notify();
    }

    fn word_status(&self, cx: &App) -> String {
        let content = self.content(cx);
        let chars = character_count(&content);
        format!("{chars} \u{5b57}")
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

    fn close_settings(
        &mut self,
        _: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_visible {
            self.toggle_settings(&ToggleSettings, window, cx);
        }
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
        self.path = path.clone();
        self.dirty = false;
        self.close_confirmed = false;
        self.last_draft_content = None;
        self.document_meta = path
            .as_ref()
            .map(|path| storage::load_document_meta(path))
            .unwrap_or_default();
        if let Some(path) = path.as_ref() {
            if let Ok(recent) = storage::remember_recent_file(path) {
                self.recent_files = recent;
            }
            self.persist_prefs();
        }
        self.pending_media.clear();
        self.preview_source = self.last_observed_content.clone().into();
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
        self.settings_visible = false;
        if self.dirty {
            self.confirm_discard(PendingOperation::New, window, cx);
            return;
        }
        self.new_document_now(cx);
    }

    fn new_document_now(&mut self, cx: &mut Context<Self>) {
        self.replace_document(
            None,
            LoadedDocument::new(String::new(), FileEncoding::Utf8),
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
        self.settings_visible = false;
        let content = self.content(cx);
        match storage::save_draft(&content) {
            Ok(path) => {
                let _ = storage::clear_autosave();
                self.last_draft_content = Some(content);
                self.drafts = storage::list_drafts();
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
        self.settings_visible = false;
        if self.dirty {
            self.confirm_discard(PendingOperation::OpenDraft, window, cx);
            return;
        }
        self.open_draft_now(cx);
    }

    fn open_draft_now(&mut self, cx: &mut Context<Self>) {
        self.drafts = storage::list_drafts();
        if self.drafts.is_empty() {
            self.status = "\u{6682}\u{65e0}\u{672c}\u{5730}\u{8349}\u{7a3f}".into();
            cx.notify();
            return;
        }
        self.dialog = EditorDialog::Drafts;
        cx.notify();
    }

    fn load_named_draft(&mut self, id: &str, cx: &mut Context<Self>) {
        match storage::load_named_draft(id) {
            Ok(Some((entry, content))) => {
                self.replace_document(None, LoadedDocument::new(content, FileEncoding::Utf8), cx);
                self.dialog = EditorDialog::None;
                self.status = format!(
                    "\u{5df2}\u{6253}\u{5f00}\u{8349}\u{7a3f} \u{00b7} {}",
                    entry.title
                )
                .into();
            }
            Ok(None) => self.status = "\u{8349}\u{7a3f}\u{4e0d}\u{5b58}\u{5728}".into(),
            Err(error) => self.status = format!("draft error: {error}").into(),
        }
        cx.notify();
    }

    fn show_recent(&mut self, _: &ShowRecent, _window: &mut Window, cx: &mut Context<Self>) {
        self.recent_files = storage::load_recent_files();
        self.dialog = EditorDialog::Recent;
        cx.notify();
    }

    fn show_drafts(&mut self, _: &ShowDrafts, window: &mut Window, cx: &mut Context<Self>) {
        self.open_draft(&OpenDraft, window, cx);
    }

    fn open_recent_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        match fs::read(&path) {
            Ok(bytes) => match decode_markdown(&bytes) {
                Ok(document) => {
                    self.dialog = EditorDialog::None;
                    self.replace_document(Some(path), document, cx);
                }
                Err(message) => self.status = message.into(),
            },
            Err(error) => {
                self.status = format!("\u{6253}\u{5f00}\u{5931}\u{8d25}\u{ff1a}{error}").into()
            }
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
        let suggested_name = if current_path.is_none() {
            Some(default_markdown_filename(&self.content(cx)))
        } else {
            None
        };
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
                    app.prompt_for_new_path(
                        &default_save_directory(),
                        Some(suggested_name.as_deref().unwrap_or("未命名文章.md")),
                    )
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
        self.settings_visible = false;
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
        let mut editor_content = self.content(cx);
        if !self.pending_media.is_empty() {
            match storage::relocate_pending_media(&editor_content, &path, &self.pending_media) {
                Ok((rewritten, relocated)) => {
                    editor_content = rewritten;
                    self.pending_media.clear();
                    let _ = relocated;
                    self.suppress_observer = true;
                    self.text_input.update(cx, |input, cx| {
                        input.set_document_content(editor_content.clone(), cx)
                    });
                    self.suppress_observer = false;
                    self.last_observed_content = editor_content.clone();
                }
                Err(error) => {
                    self.status = format!("\u{56fe}\u{7247}\u{8d44}\u{6e90}\u{5b89}\u{7f6e}\u{5931}\u{8d25}\u{ff1a}{error}").into();
                    cx.notify();
                    return false;
                }
            }
        }
        let disk_content = editor_content.replace('\n', &self.line_ending);
        let bytes = match encode_markdown(&disk_content, self.file_encoding) {
            Ok(bytes) => bytes,
            Err(message) => {
                self.status = message.into();
                cx.notify();
                return false;
            }
        };
        match storage::atomic_write(&path, &bytes) {
            Ok(()) => {
                self.path = Some(path.clone());
                self.dirty = false;
                self.last_observed_content = editor_content;
                if let Ok(recent) = storage::remember_recent_file(&path) {
                    self.recent_files = recent;
                }
                let _ = storage::save_document_meta(&path, &self.document_meta);
                self.persist_prefs();
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
        self.settings_visible = false;
        if let Some(path) = self.path.clone() {
            self.save_to(path, cx);
            return;
        }
        let suggested_name = default_markdown_filename(&self.content(cx));
        let receiver = cx.prompt_for_new_path(&default_save_directory(), Some(&suggested_name));
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

    fn save_as_document(
        &mut self,
        _: &SaveAsDocument,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_visible = false;
        let suggested_name = self
            .path
            .as_ref()
            .and_then(|path| path.file_name().and_then(|name| name.to_str()))
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| default_markdown_filename(&self.content(cx)));
        let directory = self
            .path
            .as_ref()
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .unwrap_or_else(default_save_directory);
        let receiver = cx.prompt_for_new_path(&directory, Some(&suggested_name));
        cx.spawn(async move |editor, cx| {
            let Ok(Ok(Some(path))) = receiver.await else {
                return;
            };
            let _ = editor.update(cx, |editor, cx| editor.save_to(path, cx));
        })
        .detach();
    }

    fn toggle_preview(&mut self, _: &TogglePreview, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_workspace(&CycleWorkspace, window, cx);
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
        let suggested_name = if current_path.is_none() {
            Some(default_markdown_filename(&self.content(cx)))
        } else {
            None
        };
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
                    app.prompt_for_new_path(
                        &default_save_directory(),
                        Some(suggested_name.as_deref().unwrap_or("未命名文章.md")),
                    )
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = window;
        self.open_publish_dialog(PublishKind::Notion, cx);
    }

    fn open_publish_dialog(&mut self, kind: PublishKind, cx: &mut Context<Self>) {
        if self.publishing {
            self.status = "\u{6b63}\u{5728}\u{53d1}\u{5e03}\u{ff0c}\u{8bf7}\u{7a0d}\u{5019}".into();
            cx.notify();
            return;
        }
        self.settings_visible = false;
        let markdown = self.content(cx);
        self.publish_title_input.update(cx, |input, cx| {
            input.set_content(document_title(&markdown), cx)
        });
        self.publish_update_existing = match kind {
            PublishKind::Notion => self.document_meta.notion_page_id.is_some(),
            PublishKind::Typecho => self.document_meta.typecho_post_id.is_some(),
        };
        self.publish_public = true;
        self.dialog = match kind {
            PublishKind::Notion => EditorDialog::PublishNotion,
            PublishKind::Typecho => EditorDialog::PublishTypecho,
        };
        cx.notify();
    }

    fn confirm_publish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dialog = self.dialog.clone();
        self.dialog = EditorDialog::None;
        match dialog {
            EditorDialog::PublishNotion => self.execute_notion_publish(window, cx),
            EditorDialog::PublishTypecho => self.execute_typecho_publish(window, cx),
            _ => {}
        }
    }

    fn copy_markdown_only(&mut self, cx: &mut Context<Self>) {
        let markdown = self.content(cx);
        cx.write_to_clipboard(ClipboardItem::new_string(markdown));
        self.dialog = EditorDialog::None;
        self.status = "Markdown \u{5df2}\u{590d}\u{5236}".into();
        cx.notify();
    }

    fn execute_notion_publish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.publishing {
            return;
        }
        self.settings_visible = false;
        let markdown = self.content(cx);
        let title = {
            let value = self.publish_title_input.read(cx).content.trim().to_owned();
            if value.is_empty() {
                document_title(&markdown)
            } else {
                value
            }
        };
        let warning = local_image_warning(&markdown);
        if let Some(config) =
            NotionConfig::from_env().or_else(|| self.publish_settings.notion_config())
        {
            let http = cx.http_client();
            let window_handle = window.window_handle();
            self.publishing = true;
            self.status = format!("正在发布到 Notion…{warning}").into();
            cx.notify();
            cx.spawn(async move |editor, cx| {
                match publish_notion_request(http, config, &title, &markdown).await {
                    Ok(page) => {
                        let url = page.url.clone();
                        let page_id = page.id.clone();
                        let _ = editor.update(cx, |editor, cx| {
                            editor.publishing = false;
                            editor.document_meta.notion_page_id = Some(page_id);
                            editor.document_meta.notion_url = Some(url.clone());
                            if let Some(path) = editor.path.as_ref() {
                                let _ = storage::save_document_meta(path, &editor.document_meta);
                            }
                            editor.status = format!("已发布到 Notion{warning} · {url}").into();
                            cx.notify();
                        });
                        let _ = window_handle.update(cx, |_, window, cx| {
                            MarkdownEditor::notify_window(
                                window,
                                cx,
                                true,
                                "Notion",
                                format!("发布成功，点击通知打开：{url}"),
                            );
                        });
                    }
                    Err(error) => {
                        let message = format!("Notion 发布失败：{error}");
                        let _ = window_handle.update(cx, |_, window, cx| {
                            MarkdownEditor::notify_window(
                                window,
                                cx,
                                false,
                                "Notion",
                                message.clone(),
                            );
                        });
                        let _ = editor.update(cx, |editor, cx| {
                            editor.publishing = false;
                            editor.status = message.into();
                            cx.notify();
                        });
                    }
                }
            })
            .detach();
        } else {
            self.publishing = false;
            self.settings_visible = true;
            self.status = "未配置 Notion，请先填写发布设置；也可复制 Markdown".into();
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = window;
        self.open_publish_dialog(PublishKind::Typecho, cx);
    }

    fn execute_typecho_publish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.publishing {
            return;
        }
        self.settings_visible = false;
        let markdown = self.content(cx);
        let title = {
            let value = self.publish_title_input.read(cx).content.trim().to_owned();
            if value.is_empty() {
                document_title(&markdown)
            } else {
                value
            }
        };
        let warning = local_image_warning(&markdown);
        let existing_id = if self.publish_update_existing {
            self.document_meta.typecho_post_id.clone()
        } else {
            None
        };
        let publish_public = self.publish_public;
        if let Some(config) =
            TypechoConfig::from_env().or_else(|| self.publish_settings.typecho_config())
        {
            let http = cx.http_client();
            let window_handle = window.window_handle();
            self.publishing = true;
            self.status = format!("正在发布到 Typecho…{warning}").into();
            cx.notify();
            cx.spawn(async move |editor, cx| {
                match publish_or_update_typecho(
                    http,
                    config,
                    &title,
                    &markdown,
                    existing_id.as_deref(),
                    publish_public,
                )
                .await
                {
                    Ok(post_id) => {
                        let _ = editor.update(cx, |editor, cx| {
                            editor.publishing = false;
                            editor.document_meta.typecho_post_id = Some(post_id.clone());
                            if let Some(path) = editor.path.as_ref() {
                                let _ = storage::save_document_meta(path, &editor.document_meta);
                            }
                            editor.status =
                                format!("已提交到 Typecho{warning} · 文章 ID：{post_id}").into();
                            cx.notify();
                        });
                    }
                    Err(error) => {
                        let message = format!("Typecho 发布失败：{error}");
                        let _ = window_handle.update(cx, |_, window, cx| {
                            MarkdownEditor::notify_window(
                                window,
                                cx,
                                false,
                                "Typecho",
                                message.clone(),
                            );
                        });
                        let _ = editor.update(cx, |editor, cx| {
                            editor.publishing = false;
                            editor.status = message.into();
                            cx.notify();
                        });
                    }
                }
            })
            .detach();
        } else {
            cx.write_to_clipboard(ClipboardItem::new_string(markdown));
            self.settings_visible = true;
            self.status = "未配置 Typecho，请先填写发布设置".into();
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
        self.settings_visible = false;
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
        self.settings_visible = false;
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
        self.settings_visible = false;
        let selected = self.text_input.update(cx, |input, cx| {
            let range = input.current_range(window, cx);
            let full = input.state.read(cx).text().to_string();
            if range.start < range.end && range.end <= full.len() {
                full[range.start..range.end].to_owned()
            } else {
                String::new()
            }
        });
        let clipboard = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default();
        let url = if is_clipboard_url(&clipboard) {
            normalize_url(&clipboard)
        } else if looks_like_url(&selected) {
            normalize_url(&selected)
        } else {
            String::new()
        };
        let label = if looks_like_url(&selected) {
            String::new()
        } else {
            selected
        };
        self.link_text_input
            .update(cx, |input, cx| input.set_content(label, cx));
        self.link_url_input
            .update(cx, |input, cx| input.set_content(url, cx));
        self.dialog = EditorDialog::Link;
        cx.notify();
    }

    fn confirm_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let label = self.link_text_input.read(cx).content.to_string();
        let url = self.link_url_input.read(cx).content.to_string();
        let snippet = markdown_link(&label, &url);
        self.dialog = EditorDialog::None;
        self.text_input.update(cx, |input, cx| {
            input.queue_insert(snippet, cx);
        });
        self.status = "\u{5df2}\u{63d2}\u{5165}\u{94fe}\u{63a5}".into();
        let _ = window;
        cx.notify();
    }

    fn heading_menu(&mut self, cx: &mut Context<Self>) {
        self.settings_visible = false;
        self.dialog = EditorDialog::Heading;
        cx.notify();
    }

    fn apply_heading_level(&mut self, level: u8, window: &mut Window, cx: &mut Context<Self>) {
        self.dialog = EditorDialog::None;
        self.text_input
            .update(cx, |input, cx| input.set_heading(level, window, cx));
        self.status = "\u{5df2}\u{66f4}\u{65b0}\u{6807}\u{9898}".into();
        cx.notify();
    }

    fn code_block_action(
        &mut self,
        _: &CodeBlockText,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_snippet(
            markdown_code_block(""),
            "\u{5df2}\u{63d2}\u{5165}\u{4ee3}\u{7801}\u{5757}",
            cx,
        );
    }

    fn insert_snippet(&mut self, snippet: String, message: &'static str, cx: &mut Context<Self>) {
        self.settings_visible = false;
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

    fn insert_local_image(&mut self, source: PathBuf, cx: &mut Context<Self>) {
        if !is_image_path(&source) {
            self.status = "\u{8bf7}\u{9009}\u{62e9} PNG\u{3001}JPEG\u{3001}GIF\u{3001}WebP\u{3001}SVG \u{6216} BMP \u{56fe}\u{7247}".into();
            cx.notify();
            return;
        }
        let bytes = match fs::read(&source) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.status =
                    format!("\u{8bfb}\u{53d6}\u{56fe}\u{7247}\u{5931}\u{8d25}\u{ff1a}{error}")
                        .into();
                cx.notify();
                return;
            }
        };
        let name = source
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("image.png");
        self.insert_image_bytes(name, &bytes, cx);
    }

    fn insert_image_bytes(&mut self, file_name: &str, bytes: &[u8], cx: &mut Context<Self>) {
        let dest_dir = if let Some(path) = self.path.as_ref() {
            storage::media_dir_for_document(path)
        } else {
            match storage::ensure_pending_media_dir() {
                Ok(dir) => dir,
                Err(error) => {
                    self.status = format!("\u{65e0}\u{6cd5}\u{521b}\u{5efa}\u{56fe}\u{7247}\u{76ee}\u{5f55}\u{ff1a}{error}").into();
                    cx.notify();
                    return;
                }
            }
        };
        match storage::save_media_bytes(&dest_dir, file_name, bytes) {
            Ok(saved) => {
                if self.path.is_none() {
                    self.pending_media.push(saved.clone());
                }
                let url = storage::relative_media_url(self.path.as_deref(), &saved);
                let snippet = format!("![\u{56fe}\u{7247}]({url})");
                self.insert_snippet(snippet, "\u{5df2}\u{63d2}\u{5165}\u{56fe}\u{7247}", cx);
            }
            Err(error) => {
                self.status =
                    format!("\u{56fe}\u{7247}\u{4fdd}\u{5b58}\u{5931}\u{8d25}\u{ff1a}{error}")
                        .into();
                cx.notify();
            }
        }
    }

    fn image_action(&mut self, _: &ImageText, _window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("\u{9009}\u{62e9}\u{56fe}\u{7247}\u{6587}\u{4ef6}".into()),
        });
        cx.spawn(async move |editor, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let _ = editor.update(cx, |editor, cx| {
                for path in paths {
                    editor.insert_local_image(path, cx);
                }
            });
        })
        .detach();
    }

    fn paste_image(&mut self, _: &PasteImage, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(item) = cx.read_from_clipboard() {
            for entry in item.entries() {
                if let ClipboardEntry::Image(image) = entry {
                    let ext = match image.format {
                        ImageFormat::Png => "png",
                        ImageFormat::Jpeg => "jpg",
                        ImageFormat::Gif => "gif",
                        ImageFormat::Webp => "webp",
                        ImageFormat::Bmp => "bmp",
                        ImageFormat::Tiff => "tiff",
                        ImageFormat::Svg => "svg",
                    };
                    let stamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let name = format!("paste-{stamp}.{ext}");
                    self.insert_image_bytes(&name, &image.bytes, cx);
                    return;
                }
            }
        }
        window.dispatch_action(Box::new(gpui_component::input::Paste), cx);
    }

    fn drop_images(&mut self, paths: &ExternalPaths, cx: &mut Context<Self>) {
        for path in paths.paths() {
            if is_image_path(path) {
                self.insert_local_image(path.clone(), cx);
            } else if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
            {
                self.status = "\u{62d6}\u{5165}\u{7684} Markdown \u{8bf7}\u{7528}\u{6253}\u{5f00}\u{6253}\u{5f00}".into();
                cx.notify();
            }
        }
    }

    fn table_action(&mut self, _: &TableText, _window: &mut Window, cx: &mut Context<Self>) {
        self.settings_visible = false;
        self.dialog = EditorDialog::Table;
        cx.notify();
    }

    fn insert_chosen_table(&mut self, cx: &mut Context<Self>) {
        let snippet = markdown_table(self.table_rows, self.table_cols);
        self.dialog = EditorDialog::None;
        self.insert_snippet(snippet, "\u{5df2}\u{63d2}\u{5165}\u{8868}\u{683c}", cx);
    }

    fn video_action(&mut self, _: &VideoText, _window: &mut Window, cx: &mut Context<Self>) {
        self.settings_visible = false;
        self.link_text_input
            .update(cx, |input, cx| input.set_content("\u{89c6}\u{9891}", cx));
        let clip = String::new();
        self.link_url_input
            .update(cx, |input, cx| input.set_content(clip, cx));
        self.dialog = EditorDialog::Link;
        self.status = "\u{63d2}\u{5165}\u{89c6}\u{9891}\u{94fe}\u{63a5}".into();
        cx.notify();
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
            self.persist_prefs();
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
                    if self.settings_visible {
                        "返回编辑"
                    } else {
                        "发布设置"
                    },
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
                                cx.listener(|editor, _event, _window, cx| {
                                    editor.heading_menu(cx)
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
                            cx.listener(|editor, _event, _window, cx| { editor.heading_menu(cx) }),
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
                            if self.settings_visible {
                                "返回编辑"
                            } else {
                                "设置"
                            },
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
                        if self.settings_visible {
                            "返回编辑"
                        } else {
                            "发布设置"
                        },
                        cx.listener(|editor, _event, window, cx| {
                            editor.toggle_settings(&ToggleSettings, window, cx)
                        }),
                    ),),
                ))
                .into_any_element(),
        }
    }

    fn heading2_action(&mut self, _: &Heading2Text, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_heading_level(2, window, cx);
    }

    fn heading3_action(&mut self, _: &Heading3Text, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_heading_level(3, window, cx);
    }

    fn test_notion_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        let Some(config) = NotionConfig::from_env().or_else(|| settings.notion_config()) else {
            self.status = "\u{5c1a}\u{672a}\u{914d}\u{7f6e} Notion".into();
            cx.notify();
            return;
        };
        let from_env = NotionConfig::from_env().is_some();
        let http = cx.http_client();
        let window_handle = window.window_handle();
        self.status = "\u{6b63}\u{5728}\u{6d4b}\u{8bd5} Notion\u{2026}".into();
        cx.notify();
        cx.spawn(
            async move |editor, cx| match test_notion_connection(http, config).await {
                Ok(name) => {
                    let message = if from_env {
                        format!("Notion OK (env): {name}")
                    } else {
                        format!("Notion OK: {name}")
                    };
                    let _ = editor.update(cx, |editor, cx| {
                        editor.status = message.clone().into();
                        cx.notify();
                    });
                    let _ = window_handle.update(cx, |_, window, cx| {
                        MarkdownEditor::notify_window(window, cx, true, "Notion", message);
                    });
                }
                Err(error) => {
                    let message = format!("Notion test failed: {error}");
                    let _ = editor.update(cx, |editor, cx| {
                        editor.status = message.clone().into();
                        cx.notify();
                    });
                    let _ = window_handle.update(cx, |_, window, cx| {
                        MarkdownEditor::notify_window(window, cx, false, "Notion", message);
                    });
                }
            },
        )
        .detach();
    }

    fn test_typecho_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        let Some(config) = TypechoConfig::from_env().or_else(|| settings.typecho_config()) else {
            self.status = "\u{5c1a}\u{672a}\u{914d}\u{7f6e} Typecho".into();
            cx.notify();
            return;
        };
        let from_env = TypechoConfig::from_env().is_some();
        let http = cx.http_client();
        let window_handle = window.window_handle();
        self.status = "\u{6b63}\u{5728}\u{6d4b}\u{8bd5} Typecho\u{2026}".into();
        cx.notify();
        cx.spawn(
            async move |editor, cx| match test_typecho_connection(http, config).await {
                Ok(name) => {
                    let message = if from_env {
                        format!("Typecho OK (env): {name}")
                    } else {
                        format!("Typecho OK: {name}")
                    };
                    let _ = editor.update(cx, |editor, cx| {
                        editor.status = message.clone().into();
                        cx.notify();
                    });
                    let _ = window_handle.update(cx, |_, window, cx| {
                        MarkdownEditor::notify_window(window, cx, true, "Typecho", message);
                    });
                }
                Err(error) => {
                    let message = format!("Typecho test failed: {error}");
                    let _ = editor.update(cx, |editor, cx| {
                        editor.status = message.clone().into();
                        cx.notify();
                    });
                    let _ = window_handle.update(cx, |_, window, cx| {
                        MarkdownEditor::notify_window(window, cx, false, "Typecho", message);
                    });
                }
            },
        )
        .detach();
    }

    fn outline_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let content = self.content(cx);
        let items = document_outline(&content);
        let mut list = div()
            .id("document-outline")
            .flex()
            .flex_col()
            .flex_grow()
            .min_h_0()
            .overflow_y_scroll()
            .gap_1()
            .p_2();
        if items.is_empty() {
            list = list.child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("\u{6682}\u{65e0}\u{6807}\u{9898}"),
            );
        }
        for (index, item) in items.into_iter().enumerate() {
            let offset = item.offset;
            let title = if item.title.is_empty() {
                format!("H{}", item.level)
            } else {
                item.title
            };
            list = list.child(
                div()
                    .id(("outline-item", index))
                    .pl(px(8. + (item.level.saturating_sub(1) as f32) * 10.))
                    .py_1()
                    .rounded_sm()
                    .text_xs()
                    .hover(|style| style.bg(rgb(0xe7f0f9)).cursor_pointer())
                    .on_click(cx.listener(move |editor, _, window, cx| {
                        editor.jump_to_offset(offset, window, cx);
                    }))
                    .child(title),
            );
        }
        div()
            .w(px(196.))
            .h_full()
            .flex()
            .flex_none()
            .flex_col()
            .bg(white())
            .border_1()
            .border_color(rgb(BORDER))
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
                    .child("\u{5927}\u{7eb2}"),
            )
            .child(list)
            .into_any_element()
    }

    fn dialog_layer(&self, cx: &mut Context<Self>) -> AnyElement {
        let card = match &self.dialog {
            EditorDialog::None => return div().into_any_element(),
            EditorDialog::Link => self.link_dialog(cx),
            EditorDialog::Table => self.table_dialog(cx),
            EditorDialog::Heading => self.heading_dialog(cx),
            EditorDialog::PublishNotion => self.publish_dialog(PublishKind::Notion, cx),
            EditorDialog::PublishTypecho => self.publish_dialog(PublishKind::Typecho, cx),
            EditorDialog::Drafts => self.drafts_dialog(cx),
            EditorDialog::Recent => self.recent_dialog(cx),
            EditorDialog::Lightbox { url, alt } => self.lightbox_dialog(url, alt),
        };
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(hsla(0., 0., 0., 0.32))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|editor, _, _, cx| editor.close_dialog(cx)),
            )
            .child(
                div()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(card),
            )
            .into_any_element()
    }

    fn dialog_card(title: &'static str, body: impl IntoElement) -> impl IntoElement {
        div()
            .w(px(460.))
            .max_w(px(560.))
            .bg(white())
            .border_1()
            .border_color(rgb(BORDER))
            .shadow_sm()
            .p_5()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight(700.))
                    .text_color(rgb(DARK_BLUE))
                    .child(title),
            )
            .child(body)
    }

    fn dialog_button(
        id: &'static str,
        label: &'static str,
        primary: bool,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        div()
            .id(id)
            .px_3()
            .py_2()
            .rounded_sm()
            .bg(rgb(if primary { BLUE } else { 0xd7e2ec }))
            .text_color(rgb(if primary { 0xffffff } else { TEXT }))
            .hover(|style| {
                style
                    .bg(rgb(if primary { DARK_BLUE } else { 0xc8d7e5 }))
                    .cursor_pointer()
            })
            .on_click(on_click)
            .child(label)
    }

    fn link_dialog(&self, cx: &mut Context<Self>) -> AnyElement {
        Self::dialog_card(
            "\u{63d2}\u{5165}\u{94fe}\u{63a5}",
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(settings_field(
                    "\u{6587}\u{5b57}",
                    "\u{7559}\u{7a7a}\u{5219}\u{4f7f}\u{7528}\u{5730}\u{5740}",
                    self.link_text_input.clone(),
                ))
                .child(settings_field(
                    "\u{5730}\u{5740}",
                    "https://...",
                    self.link_url_input.clone(),
                ))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(Self::dialog_button(
                            "cancel-link",
                            "\u{53d6}\u{6d88}",
                            false,
                            cx.listener(|editor, _, _, cx| editor.close_dialog(cx)),
                        ))
                        .child(Self::dialog_button(
                            "confirm-link",
                            "\u{63d2}\u{5165}",
                            true,
                            cx.listener(|editor, _, window, cx| editor.confirm_link(window, cx)),
                        )),
                ),
        )
        .into_any_element()
    }

    fn table_dialog(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut grid = div().flex().flex_col().gap_1();
        for rows in 1..=5 {
            let mut row = div().flex().gap_1();
            for cols in 1..=5 {
                let active = self.table_rows == rows && self.table_cols == cols;
                row = row.child(
                    div()
                        .id(("table-size", rows * 10 + cols))
                        .w(px(36.))
                        .h(px(28.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .border_1()
                        .border_color(rgb(if active { BLUE } else { BORDER }))
                        .bg(rgb(if active { 0xd9eaf8 } else { 0xffffff }))
                        .text_xs()
                        .hover(|style| style.bg(rgb(0xe7f0f9)).cursor_pointer())
                        .on_click(cx.listener(move |editor, _, _, cx| {
                            editor.table_rows = rows;
                            editor.table_cols = cols;
                            cx.notify();
                        }))
                        .child(format!("{rows}x{cols}")),
                );
            }
            grid = grid.child(row);
        }
        Self::dialog_card(
            "\u{63d2}\u{5165}\u{8868}\u{683c}",
            div().flex().flex_col().gap_3().child(grid).child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::dialog_button(
                        "cancel-table",
                        "\u{53d6}\u{6d88}",
                        false,
                        cx.listener(|editor, _, _, cx| editor.close_dialog(cx)),
                    ))
                    .child(Self::dialog_button(
                        "confirm-table",
                        "\u{63d2}\u{5165}",
                        true,
                        cx.listener(|editor, _, _, cx| editor.insert_chosen_table(cx)),
                    )),
            ),
        )
        .into_any_element()
    }

    fn heading_dialog(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut buttons = div().flex().flex_wrap().gap_2();
        for level in 0..=6 {
            let label: SharedString = if level == 0 {
                "\u{6b63}\u{6587}".into()
            } else {
                format!("H{level}").into()
            };
            buttons = buttons.child(
                div()
                    .id(("heading-level", level as usize))
                    .px_3()
                    .py_2()
                    .rounded_sm()
                    .bg(rgb(0xe7f0f9))
                    .hover(|style| style.bg(rgb(0xd9eaf8)).cursor_pointer())
                    .on_click(cx.listener(move |editor, _, window, cx| {
                        editor.apply_heading_level(level, window, cx);
                    }))
                    .child(label),
            );
        }
        Self::dialog_card(
            "\u{6807}\u{9898}",
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(buttons)
                .child(Self::dialog_button(
                    "cancel-heading",
                    "\u{5173}\u{95ed}",
                    false,
                    cx.listener(|editor, _, _, cx| editor.close_dialog(cx)),
                )),
        )
        .into_any_element()
    }

    fn publish_dialog(&self, kind: PublishKind, cx: &mut Context<Self>) -> AnyElement {
        let markdown = self.content(cx);
        let images = local_images(&markdown);
        let configured = match kind {
            PublishKind::Notion => {
                NotionConfig::from_env().is_some()
                    || self.publish_settings.notion_config().is_some()
            }
            PublishKind::Typecho => {
                TypechoConfig::from_env().is_some()
                    || self.publish_settings.typecho_config().is_some()
            }
        };
        let existing = match kind {
            PublishKind::Notion => self.document_meta.notion_page_id.clone(),
            PublishKind::Typecho => self.document_meta.typecho_post_id.clone(),
        };
        let title = match kind {
            PublishKind::Notion => "\u{53d1}\u{5e03}\u{5230} Notion",
            PublishKind::Typecho => "\u{53d1}\u{5e03}\u{5230} Typecho",
        };
        let mut body = div().flex().flex_col().gap_3().child(settings_field(
            "\u{6807}\u{9898}",
            "\u{53d1}\u{5e03}\u{540e}\u{7684}\u{6587}\u{7ae0}\u{6807}\u{9898}",
            self.publish_title_input.clone(),
        ));
        if existing.is_some() {
            body = body.child(
                div()
                    .id("toggle-update-existing")
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .hover(|style| style.bg(rgb(0xe7f0f9)).cursor_pointer())
                    .on_click(cx.listener(|editor, _, _, cx| {
                        editor.publish_update_existing = !editor.publish_update_existing;
                        cx.notify();
                    }))
                    .child(format!(
                        "{} \u{66f4}\u{65b0}\u{5df2}\u{6709}\u{6587}\u{7ae0} ({})",
                        if self.publish_update_existing {
                            "[x]"
                        } else {
                            "[ ]"
                        },
                        existing.clone().unwrap_or_default()
                    )),
            );
        }
        if kind == PublishKind::Typecho {
            body = body.child(
                div()
                    .id("toggle-publish-public")
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .hover(|style| style.bg(rgb(0xe7f0f9)).cursor_pointer())
                    .on_click(cx.listener(|editor, _, _, cx| {
                        editor.publish_public = !editor.publish_public;
                        cx.notify();
                    }))
                    .child(if self.publish_public {
                        "[x] \u{516c}\u{5f00}\u{53d1}\u{5e03}"
                    } else {
                        "[ ] \u{4fdd}\u{5b58}\u{4e3a}\u{8349}\u{7a3f}"
                    }),
            );
        }
        if !images.is_empty() {
            body = body.child(div().text_xs().text_color(rgb(0xa15c12)).child(format!(
                "\u{542b} {} \u{5904}\u{672c}\u{5730}/\u{76f8}\u{5bf9}\u{56fe}\u{7247}\u{ff0c}\u{53d1}\u{5e03}\u{540e}\u{53ef}\u{80fd}\u{65e0}\u{6cd5}\u{76f4}\u{63a5}\u{663e}\u{793a}\u{3002}",
                images.len()
            )));
        }
        if !configured {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(rgb(0xa15c12))
                    .child("\u{5c1a}\u{672a}\u{914d}\u{7f6e}\u{3002}\u{53ef}\u{4ee5}\u{590d}\u{5236} Markdown\u{ff0c}\u{6216}\u{6253}\u{5f00}\u{8bbe}\u{7f6e}\u{3002}"),
            );
        }
        let mut actions = div()
            .flex()
            .justify_end()
            .gap_2()
            .child(Self::dialog_button(
                "cancel-publish",
                "\u{53d6}\u{6d88}",
                false,
                cx.listener(|editor, _, _, cx| editor.close_dialog(cx)),
            ));
        actions = actions.child(Self::dialog_button(
            "copy-publish",
            "\u{590d}\u{5236} Markdown",
            false,
            cx.listener(|editor, _, _, cx| editor.copy_markdown_only(cx)),
        ));
        if !configured {
            actions = actions.child(Self::dialog_button(
                "open-settings-publish",
                "\u{53d1}\u{5e03}\u{8bbe}\u{7f6e}",
                false,
                cx.listener(|editor, _, _, cx| {
                    editor.dialog = EditorDialog::None;
                    editor.settings_visible = true;
                    cx.notify();
                }),
            ));
        } else {
            actions = actions.child(Self::dialog_button(
                "confirm-publish",
                if self.publishing {
                    "\u{6b63}\u{5728}\u{53d1}\u{5e03}\u{2026}"
                } else {
                    "\u{53d1}\u{5e03}"
                },
                true,
                cx.listener(|editor, _, window, cx| editor.confirm_publish(window, cx)),
            ));
        }
        Self::dialog_card(title, body.child(actions)).into_any_element()
    }

    fn drafts_dialog(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut list = div()
            .id("dialog-list")
            .flex()
            .flex_col()
            .gap_1()
            .max_h(px(360.))
            .overflow_y_scroll();
        for (index, draft) in self.drafts.iter().enumerate() {
            let id = draft.id.clone();
            list = list.child(
                div()
                    .id(("draft-item", index))
                    .p_2()
                    .rounded_sm()
                    .border_1()
                    .border_color(rgb(BORDER))
                    .hover(|style| style.bg(rgb(0xe7f0f9)).cursor_pointer())
                    .on_click(cx.listener(move |editor, _, _, cx| {
                        editor.load_named_draft(&id, cx);
                    }))
                    .child(
                        div()
                            .font_weight(FontWeight(600.))
                            .child(draft.title.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(draft.excerpt.clone()),
                    ),
            );
        }
        Self::dialog_card(
            "\u{8349}\u{7a3f}\u{7bb1}",
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(list)
                .child(Self::dialog_button(
                    "close-drafts",
                    "\u{5173}\u{95ed}",
                    false,
                    cx.listener(|editor, _, _, cx| editor.close_dialog(cx)),
                )),
        )
        .into_any_element()
    }

    fn recent_dialog(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut list = div()
            .id("dialog-list")
            .flex()
            .flex_col()
            .gap_1()
            .max_h(px(360.))
            .overflow_y_scroll();
        if self.recent_files.is_empty() {
            list = list.child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("\u{6682}\u{65e0}\u{6700}\u{8fd1}\u{6587}\u{4ef6}"),
            );
        }
        for (index, path) in self.recent_files.iter().enumerate() {
            let path_buf = path.clone();
            let label = display_path(path);
            list = list.child(
                div()
                    .id(("recent-item", index))
                    .p_2()
                    .rounded_sm()
                    .hover(|style| style.bg(rgb(0xe7f0f9)).cursor_pointer())
                    .on_click(cx.listener(move |editor, _, _, cx| {
                        editor.open_recent_path(path_buf.clone(), cx);
                    }))
                    .child(label),
            );
        }
        Self::dialog_card(
            "\u{6700}\u{8fd1}\u{6587}\u{4ef6}",
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(list)
                .child(Self::dialog_button(
                    "close-recent",
                    "\u{5173}\u{95ed}",
                    false,
                    cx.listener(|editor, _, _, cx| editor.close_dialog(cx)),
                )),
        )
        .into_any_element()
    }

    fn lightbox_dialog(&self, url: &str, alt: &str) -> AnyElement {
        let source = preview_image_source(url, self.path.as_ref().and_then(|path| path.parent()));
        Self::dialog_card(
            "\u{56fe}\u{7247}",
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(img(source).max_w(px(520.)).max_h(px(420.)))
                .child(div().text_xs().text_color(rgb(MUTED)).child(alt.to_owned()))
                .child(div().text_xs().text_color(rgb(BLUE)).child(url.to_owned())),
        )
        .into_any_element()
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
        self.sync_scroll_offsets();
        let preview_content = if self.preview_source.is_empty() {
            self.text_input.read(cx).content.clone()
        } else {
            self.preview_source.clone()
        };
        let base = self
            .path
            .as_ref()
            .and_then(|path| path.parent().map(Path::to_path_buf));
        let editor_scroll = self.editor_scroll.clone();
        let preview_scroll = self.preview_scroll.clone();
        let workspace = if self.settings_visible {
            div()
                    .flex()
                    .flex_col()
                    .flex_grow()
                    .min_h_0()
                    .w_full()
                    .p_3()
                    .gap_2()
                    .child(settings_panel(
                        self.notion_token_input.clone(),
                        self.notion_parent_input.clone(),
                        self.typecho_url_input.clone(),
                        self.typecho_username_input.clone(),
                        self.typecho_password_input.clone(),
                        cx.listener(Self::close_settings),
                        cx.listener(Self::save_publish_settings),
                    ))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .px_6()
                            .child(Self::dialog_button(
                                "test-notion",
                                "\u{6d4b}\u{8bd5} Notion",
                                false,
                                cx.listener(|editor, _, window, cx| {
                                    editor.test_notion_settings(window, cx)
                                }),
                            ))
                            .child(Self::dialog_button(
                                "test-typecho",
                                "\u{6d4b}\u{8bd5} Typecho",
                                false,
                                cx.listener(|editor, _, window, cx| {
                                    editor.test_typecho_settings(window, cx)
                                }),
                            ))
                            .child(div().text_xs().text_color(rgb(MUTED)).child(
                                if NotionConfig::from_env().is_some()
                                    || TypechoConfig::from_env().is_some()
                                {
                                    "\u{73af}\u{5883}\u{53d8}\u{91cf}\u{4f18}\u{5148}\u{4e8e}\u{8fd9}\u{91cc}\u{4fdd}\u{5b58}\u{7684}\u{51ed}\u{636e}\u{3002}"
                                } else {
                                    "\u{51ed}\u{636e}\u{4ec5}\u{4fdd}\u{5b58}\u{5230}\u{7cfb}\u{7edf}\u{5b89}\u{5168}\u{5b58}\u{50a8}\u{3002}"
                                },
                            )),
                    )
        } else {
            let mut row = div()
                .flex()
                .flex_row()
                .flex_grow()
                .min_h_0()
                .w_full()
                .p_3()
                .gap_3()
                .on_mouse_move(cx.listener(Self::splitter_mouse_move))
                .on_mouse_up(MouseButton::Left, cx.listener(Self::splitter_mouse_up))
                .on_mouse_up_out(MouseButton::Left, cx.listener(Self::splitter_mouse_up));
            if self.outline_visible {
                row = row.child(self.outline_panel(cx));
            }
            match self.workspace_mode {
                WorkspaceMode::Edit => {
                    row = row.child(editor_panel(self.text_input.clone(), None, editor_scroll));
                }
                WorkspaceMode::Preview => {
                    row = row.child(preview_panel(
                        preview_content,
                        "Markdown \u{9884}\u{89c8}",
                        preview_scroll,
                        base,
                    ));
                }
                WorkspaceMode::Split => {
                    row = row
                        .child(editor_panel(
                            self.text_input.clone(),
                            Some(self.split_ratio),
                            editor_scroll,
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
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(Self::splitter_mouse_down),
                                )
                                .hover(|style| {
                                    style.bg(rgb(0xd5e5f2)).cursor(CursorStyle::ResizeLeftRight)
                                })
                                .child(
                                    div().w(px(1.)).h_full().bg(rgb(if self.splitter_dragging {
                                        BLUE
                                    } else {
                                        BORDER
                                    })),
                                ),
                        )
                        .child(preview_panel(
                            preview_content,
                            "Markdown \u{9884}\u{89c8}",
                            preview_scroll,
                            base,
                        ));
                }
            }
            row
        };
        let ribbon = if self.focus_mode {
            div()
                .h(px(36.))
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .px_3()
                .bg(rgb(RIBBON_BLUE))
                .border_b_1()
                .border_color(rgb(BORDER))
                .child("\u{4e13}\u{6ce8}\u{6a21}\u{5f0f}")
                .child(Self::dialog_button(
                    "exit-focus",
                    "\u{9000}\u{51fa}\u{4e13}\u{6ce8}",
                    false,
                    cx.listener(|editor, _, window, cx| {
                        editor.toggle_focus(&ToggleFocus, window, cx)
                    }),
                ))
                .into_any_element()
        } else {
            self.ribbon(cx)
        };

        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(WORKSPACE))
            .text_color(rgb(TEXT))
            .key_context("MarkdownEditor")
            .on_action(cx.listener(Self::new_document))
            .on_action(cx.listener(Self::open_document))
            .on_action(cx.listener(Self::open_draft))
            .on_action(cx.listener(Self::save_document))
            .on_action(cx.listener(Self::save_as_document))
            .on_action(cx.listener(Self::save_draft))
            .on_action(cx.listener(Self::toggle_preview))
            .on_action(cx.listener(Self::cycle_workspace))
            .on_action(cx.listener(Self::toggle_focus))
            .on_action(cx.listener(Self::toggle_outline))
            .on_action(cx.listener(Self::zoom_in))
            .on_action(cx.listener(Self::zoom_out))
            .on_action(cx.listener(Self::zoom_reset))
            .on_action(cx.listener(Self::toggle_settings))
            .on_action(cx.listener(Self::bold_action))
            .on_action(cx.listener(Self::italic_action))
            .on_action(cx.listener(Self::heading_action))
            .on_action(cx.listener(Self::heading2_action))
            .on_action(cx.listener(Self::heading3_action))
            .on_action(cx.listener(Self::bullets_action))
            .on_action(cx.listener(Self::quote_action))
            .on_action(cx.listener(Self::link_action))
            .on_action(cx.listener(Self::strike_action))
            .on_action(cx.listener(Self::code_action))
            .on_action(cx.listener(Self::code_block_action))
            .on_action(cx.listener(Self::image_action))
            .on_action(cx.listener(Self::paste_image))
            .on_action(cx.listener(Self::table_action))
            .on_action(cx.listener(Self::video_action))
            .on_action(cx.listener(Self::divider_action))
            .on_action(cx.listener(Self::undo_action))
            .on_action(cx.listener(Self::redo_action))
            .on_action(cx.listener(Self::quit_application))
            .on_action(cx.listener(Self::publish_to_notion))
            .on_action(cx.listener(Self::publish_to_typecho))
            .on_action(cx.listener(Self::show_drafts))
            .on_action(cx.listener(Self::show_recent))
            .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(rgb(0xd9eaf8)))
            .on_drop(cx.listener(|editor, paths: &ExternalPaths, _, cx| {
                editor.drop_images(paths, cx);
            }))
            .capture_action(
                cx.listener(|editor, _: &gpui_component::input::Paste, window, cx| {
                    if let Some(item) = cx.read_from_clipboard()
                        && item
                            .entries()
                            .iter()
                            .any(|entry| matches!(entry, ClipboardEntry::Image(_)))
                    {
                        editor.paste_image(&PasteImage, window, cx);
                        cx.stop_propagation();
                    }
                }),
            )
            .when(!self.focus_mode, |this| {
                this.child(
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
            })
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
                        "{} · {} · {}{}",
                        title,
                        self.workspace_mode.label(),
                        self.word_status(cx),
                        self.last_backup_at
                            .map(|secs| format!(" · backup {secs}"))
                            .unwrap_or_default()
                    )),
            )
            .when(self.dialog != EditorDialog::None, |this| {
                this.child(self.dialog_layer(cx))
            })
    }
}

#[allow(dead_code)]
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
    if !bytes.len().is_multiple_of(2) {
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
    if let Some(path) = storage::load_prefs().last_directory
        && path.is_dir()
    {
        return path;
    }
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

fn default_markdown_filename(markdown: &str) -> String {
    let mut name = document_title(markdown)
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
            {
                '_'
            } else {
                character
            }
        })
        .collect::<String>();
    name = name.trim().trim_end_matches(['.', ' ']).to_owned();
    if name.is_empty() {
        name = "未命名文章".to_owned();
    }
    if !name.to_ascii_lowercase().ends_with(".md") {
        name.push_str(".md");
    }
    name
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

#[allow(dead_code)]
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

#[allow(dead_code)]
fn markdown_image_url(path: &Path) -> String {
    let path = percent_encode_path(&path.to_string_lossy().replace('\\', "/"));
    if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{path}")
    }
}

#[allow(dead_code)]
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

fn preview_image_source(url: &str, base: Option<&Path>) -> ImageSource {
    let decoded = percent_decode(url);
    if let Some(path) = decoded.strip_prefix("file:///") {
        ImageSource::from(PathBuf::from(path))
    } else if let Some(path) = decoded.strip_prefix("file://") {
        ImageSource::from(PathBuf::from(path))
    } else if looks_like_url(&decoded) {
        ImageSource::from(decoded)
    } else if let Some(base) = base {
        ImageSource::from(base.join(decoded.replace('/', std::path::MAIN_SEPARATOR_STR)))
    } else {
        ImageSource::from(PathBuf::from(decoded))
    }
}

fn editor_panel(
    input: Entity<MarkdownInput>,
    width: Option<f32>,
    scroll: ScrollHandle,
) -> impl IntoElement {
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
                .track_scroll(&scroll)
                .p_5()
                .child(input),
        )
}

fn preview_panel(
    content: SharedString,
    label: &'static str,
    scroll: ScrollHandle,
    base: Option<PathBuf>,
) -> impl IntoElement {
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
                .track_scroll(&scroll)
                .whitespace_normal()
                .p_5()
                .children(markdown_preview(content.as_ref(), base.as_deref())),
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
    on_close: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
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
                            "父页面或数据库 ID",
                            "支持普通页面 ID、数据库 ID 或完整 Notion 链接",
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
            div().flex().justify_end().gap_2().child(
                div()
                    .id("close-publish-settings")
                    .px_4()
                    .py_2()
                    .rounded_sm()
                    .bg(rgb(0xd7e2ec))
                    .text_color(rgb(TEXT))
                    .hover(|style| style.bg(rgb(0xc8d7e5)).cursor_pointer())
                    .on_click(on_close)
                    .child("返回编辑"),
                )
                .child(
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
        .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
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
        .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
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
        .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
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

fn markdown_preview(markdown: &str, base: Option<&Path>) -> Vec<gpui::AnyElement> {
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
                    .child(inline_preview(&text, base))
                    .into_any_element()
            }
            Block::Paragraph(text) => div()
                .id(("preview-paragraph", index))
                .w_full()
                .mb_3()
                .text_base()
                .line_height(px(26.))
                .child(inline_preview(&text, base))
                .into_any_element(),
            Block::Bullet { text, depth } => div()
                .id(("preview-bullet", index))
                .w_full()
                .mb_1()
                .pl(px(12. + depth as f32 * 18.))
                .flex()
                .text_base()
                .child("• ")
                .child(inline_preview(&text, base))
                .into_any_element(),
            Block::Numbered {
                marker,
                text,
                depth,
            } => div()
                .id(("preview-number", index))
                .w_full()
                .mb_1()
                .pl(px(12. + depth as f32 * 18.))
                .flex()
                .text_base()
                .child(format!("{}. ", marker))
                .child(inline_preview(&text, base))
                .into_any_element(),
            Block::Task {
                checked,
                text,
                depth,
            } => {
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
                    .pl(px(12. + depth as f32 * 18.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_base()
                    .child(checkbox)
                    .child(
                        div()
                            .when(checked, |this| this.text_color(rgb(MUTED)).line_through())
                            .child(inline_preview(&text, base)),
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
                .child(inline_preview(&text, base))
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
                            .child(inline_preview(&content, base)),
                    )
                    .into_any_element()
            }
            Block::Code { text, language } => {
                let copy_text = text.clone();
                div()
                    .id(("preview-code", index))
                    .w_full()
                    .mb_3()
                    .p_3()
                    .rounded_sm()
                    .bg(rgb(0xf1f3f5))
                    .border_1()
                    .border_color(rgb(0xdfe3e8))
                    .text_color(rgb(0x38434d))
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .justify_between()
                            .mb_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .child(language.clone().unwrap_or_else(|| "code".into())),
                            )
                            .child(
                                div()
                                    .id(("copy-code", index))
                                    .text_xs()
                                    .text_color(rgb(BLUE))
                                    .cursor_pointer()
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            copy_text.clone(),
                                        ));
                                    })
                                    .child("\u{590d}\u{5236}"),
                            ),
                    )
                    .child(
                        div()
                            .id(("preview-code-scroll", index))
                            .w_full()
                            .overflow_x_scroll()
                            .font_family("Menlo, Monaco, Consolas, monospace")
                            .child(text),
                    )
                    .into_any_element()
            }
            Block::Image { alt, url } => div()
                .id(("preview-image", index))
                .w_full()
                .mb_3()
                .flex()
                .flex_col()
                .gap_1()
                .child(img(preview_image_source(&url, base)).max_w_full())
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
                    .flex()
                    .flex_col()
                    .border_1()
                    .border_color(rgb(BORDER));
                table = table.child(preview_table_row(headers, true, index * 1000, base));
                for (row_index, row) in rows.into_iter().enumerate() {
                    table = table.child(preview_table_row(
                        row,
                        false,
                        index * 1000 + row_index + 1,
                        base,
                    ));
                }
                div()
                    .id(("preview-table-scroll", index))
                    .w_full()
                    .mb_3()
                    .overflow_x_scroll()
                    .child(table)
                    .into_any_element()
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

fn preview_table_row(
    cells: Vec<String>,
    header: bool,
    index: usize,
    base: Option<&Path>,
) -> gpui::AnyElement {
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
            .child(inline_preview(&cell, base));
        if header {
            element = element.font_weight(FontWeight(600.)).bg(rgb(0xf1f6fb));
        }
        row = row.child(element);
    }
    row.into_any_element()
}

fn inline_preview(markdown: &str, base: Option<&Path>) -> gpui::AnyElement {
    let pieces = parse_inline(markdown);

    // Render styled runs as one block-level wrapped text element per segment.
    // Wrapping them as flex items broke both behaviors: flex measures items at
    // max-content width, so the text never saw a wrap width, while the older
    // per-piece boxes could only wrap at style boundaries (right after closing
    // `**`). Block segments keep natural word/CJK wrapping intact.
    let mut column = div().w_full().max_w_full().min_w_0().flex().flex_col();
    let mut text = String::new();
    let mut highlights: Vec<(Range<usize>, HighlightStyle)> = Vec::new();

    for piece in pieces {
        if let Some(url) = piece.image {
            column = flush_inline_text(
                column,
                std::mem::take(&mut text),
                std::mem::take(&mut highlights),
            );
            let click_url = url.clone();
            column = column.child(
                div()
                    .w_full()
                    .max_w_full()
                    .min_w_0()
                    .my_1()
                    .id("preview-inline-image")
                    .cursor_pointer()
                    .on_click(move |_, _, cx| {
                        if looks_like_url(&click_url) {
                            cx.open_url(&click_url);
                        }
                    })
                    .child(
                        img(preview_image_source(&url, base))
                            .max_h(px(160.))
                            .max_w_full(),
                    ),
            );
            continue;
        }
        if let Some(url) = piece.link.clone() {
            column = flush_inline_text(
                column,
                std::mem::take(&mut text),
                std::mem::take(&mut highlights),
            );
            let click_url = url.clone();
            let tip = url.clone();
            column = column.child(
                div()
                    .id(SharedString::from(format!("preview-link-{url}")))
                    .text_color(rgb(BLUE))
                    .underline()
                    .cursor_pointer()
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .on_click(move |_, _, cx| {
                        cx.open_url(&click_url);
                    })
                    .child(piece.text),
            );
            continue;
        }

        let start = text.len();
        text.push_str(&piece.text);
        let end = text.len();

        let mut style = HighlightStyle::default();
        for inline in &piece.styles {
            style = match inline {
                InlineStyle::Bold => HighlightStyle {
                    font_weight: Some(FontWeight(700.)),
                    ..style
                },
                InlineStyle::Italic => HighlightStyle {
                    font_style: Some(FontStyle::Italic),
                    ..style
                },
                InlineStyle::Strike => HighlightStyle {
                    strikethrough: Some(StrikethroughStyle {
                        thickness: px(1.),
                        color: None,
                    }),
                    ..style
                },
                InlineStyle::Code => HighlightStyle {
                    background_color: Some(rgb(0xf1f3f5).into()),
                    ..style
                },
            };
        }
        if piece.link.is_some() {
            style = HighlightStyle {
                color: Some(rgb(BLUE).into()),
                underline: Some(UnderlineStyle {
                    thickness: px(1.),
                    color: None,
                    wavy: false,
                }),
                ..style
            };
        }
        highlights.push((start..end, style));
    }

    flush_inline_text(column, text, highlights).into_any_element()
}

fn flush_inline_text(
    column: gpui::Div,
    text: String,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
) -> gpui::Div {
    if text.is_empty() {
        return column;
    }
    column.child(
        div()
            .w_full()
            .max_w_full()
            .min_w_0()
            .whitespace_normal()
            .child(StyledText::new(text).with_highlights(highlights)),
    )
}

/// Windowed apps have no stderr, so persist panics to a log file instead of
/// vanishing silently when the process dies.
fn install_panic_logger() {
    std::panic::set_hook(Box::new(|info| {
        let message = if let Some(text) = info.payload().downcast_ref::<&str>() {
            (*text).to_owned()
        } else if let Some(text) = info.payload().downcast_ref::<String>() {
            text.clone()
        } else {
            "unknown panic payload".to_owned()
        };
        let location = info
            .location()
            .map(|location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            })
            .unwrap_or_else(|| "unknown".to_owned());
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default();

        let entry = format!(
            "==== panic (unix {} s)\nmessage: {}\nlocation: {}\nbacktrace:\n{}\n",
            timestamp,
            message,
            location,
            Backtrace::force_capture(),
        );
        let path = crash_log_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = file.write_all(entry.as_bytes());
        }
    }));
}

fn main() {
    install_panic_logger();
    let application = Application::new()
        .with_assets(Assets)
        .with_http_client(default_http_client());
    application.run(|cx: &mut App| {
        gpui_component::init(cx);
        // The application uses a fixed light Windows Live Writer palette. Keep the component
        // editor in the matching light theme instead of inheriting the system dark theme.
        Theme::change(ThemeMode::Light, None, cx);
        cx.bind_keys([
            KeyBinding::new("secondary-n", NewDocument, None),
            KeyBinding::new("secondary-o", OpenDocument, None),
            KeyBinding::new("secondary-shift-o", OpenDraft, None),
            KeyBinding::new("secondary-s", SaveDocument, None),
            KeyBinding::new("secondary-shift-s", SaveAsDocument, None),
            KeyBinding::new("secondary-shift-d", SaveDraft, None),
            KeyBinding::new("secondary-b", BoldText, None),
            KeyBinding::new("secondary-i", ItalicText, None),
            KeyBinding::new("secondary-1", HeadingText, None),
            KeyBinding::new("secondary-2", Heading2Text, None),
            KeyBinding::new("secondary-3", Heading3Text, None),
            KeyBinding::new("secondary-shift-8", BulletsText, None),
            KeyBinding::new("secondary-shift-.", QuoteText, None),
            KeyBinding::new("secondary-k", LinkText, None),
            KeyBinding::new("secondary-shift-c", CodeBlockText, None),
            KeyBinding::new("secondary-shift-p", CycleWorkspace, None),
            KeyBinding::new("secondary-shift-f", ToggleFocus, None),
            KeyBinding::new("f11", ToggleFocus, None),
            KeyBinding::new("secondary-shift-l", ToggleOutline, None),
            KeyBinding::new("secondary-equal", ZoomIn, None),
            KeyBinding::new("secondary-minus", ZoomOut, None),
            KeyBinding::new("secondary-0", ZoomReset, None),
            KeyBinding::new("secondary-comma", ToggleSettings, None),
            KeyBinding::new("secondary-shift-enter", PublishToNotion, None),
            KeyBinding::new("secondary-shift-t", PublishToTypecho, None),
            KeyBinding::new("secondary-shift-r", ShowRecent, None),
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
                    let (dirty, close_confirmed, path, content) = editor
                        .read_with(cx, |editor, cx| {
                            (
                                editor.dirty,
                                editor.close_confirmed,
                                editor.path.clone(),
                                editor.content(cx),
                            )
                        })
                        .unwrap_or((false, false, None, String::new()));
                    let suggested_name = if path.is_none() {
                        Some(default_markdown_filename(&content))
                    } else {
                        None
                    };
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
                                        Some(suggested_name.as_deref().unwrap_or("未命名文章.md")),
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
        EmbeddedAssets, default_markdown_filename, is_image_path, line_end, line_starts,
        markdown_image_url, normalize_newlines, percent_decode, typecho_is_primary_publish_target,
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
        assert_eq!(
            markdown_image_url(path),
            "file:///tmp/%E5%B0%81%E9%9D%A2.png"
        );
        assert_eq!(percent_decode("%E5%B0%81%E9%9D%A2"), "封面");
        assert_eq!(percent_decode("a%20b.png"), "a b.png");
        assert!(!is_image_path(Path::new("/tmp/article.md")));
    }

    #[test]
    fn suggests_markdown_title_as_filename() {
        assert_eq!(
            default_markdown_filename("# Endless Fight: Babel?\n\n正文"),
            "Endless Fight_ Babel_.md"
        );
        assert_eq!(
            default_markdown_filename(""),
            "\u{672a}\u{547d}\u{540d}\u{6587}\u{7ae0}.md"
        );
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
