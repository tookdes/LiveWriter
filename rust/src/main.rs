mod markdown;
mod publishing;
mod storage;

use std::{
    borrow::Cow,
    fs,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

use gpui::{
    AnyElement, App, Application, AssetSource, Bounds, ClipboardItem, Context, CursorStyle, Entity,
    EntityInputHandler, FocusHandle, Focusable, FontWeight, Image, ImageFormat, Img, KeyBinding,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathPromptOptions, Pixels,
    PromptLevel, SharedString, Window, WindowBounds, WindowOptions, actions, div, hsla, img,
    linear_color_stop, linear_gradient, prelude::*, px, relative, rgb, size, white,
};
use gpui_component::{
    RopeExt,
    input::{Input, InputEvent, InputState},
};
use rust_embed::Embed;

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
const RIBBON_SEPARATOR: u32 = 0xc8d5e2;
const RIBBON_TAB_HEIGHT: Pixels = px(30.);
const RIBBON_HEIGHT: Pixels = px(84.);
const RIBBON_BUTTON_WIDTH: Pixels = px(56.);
const RIBBON_BUTTON_HEIGHT: Pixels = px(56.);
const WORKSPACE: u32 = 0xe9edf2;
const BORDER: u32 = 0xc6ced8;
const TEXT: u32 = 0x263746;
const MUTED: u32 = 0x617285;
const INLINE_CODE_MARKER: &str = "\x60";

macro_rules! ribbon_controls {
    ($first:expr $(, $rest:expr)* $(,)?) => {{
        let controls = div().flex().items_center().child($first);
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
    pending_content: Option<SharedString>,
    pending_insert: Option<SharedString>,
    suppress_history: bool,
    undo_stack: Vec<SharedString>,
    redo_stack: Vec<SharedString>,
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
                state = state.multi_line().soft_wrap(false);
            }
            state.masked(masked)
        });
        let subscription = cx.subscribe(
            &state,
            |input: &mut MarkdownInput, state, event: &InputEvent, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                let next = state.read(cx).value();
                if next == input.content {
                    return;
                }
                if !input.suppress_history {
                    input.undo_stack.push(input.content.clone());
                    input.redo_stack.clear();
                }
                input.content = next;
                cx.notify();
            },
        );
        Self {
            state: state.clone(),
            content,
            multi_line,
            pending_content: None,
            pending_insert: None,
            suppress_history: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            _subscription: subscription,
        }
    }

    fn set_content(&mut self, content: impl Into<SharedString>, cx: &mut Context<Self>) {
        let content = content.into();
        self.content = content.clone();
        self.pending_content = Some(content);
        self.pending_insert = None;
        self.suppress_history = true;
        self.undo_stack.clear();
        self.redo_stack.clear();
        cx.notify();
    }

    fn restore_content(&mut self, content: SharedString, cx: &mut Context<Self>) {
        self.content = content.clone();
        self.pending_content = Some(content);
        self.pending_insert = None;
        self.suppress_history = true;
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
            let state = self.state.clone();
            state.update(cx, |state, cx| state.set_value(content, window, cx));
            self.suppress_history = false;
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
            let selected = state.text().slice(range).to_string();
            state.replace(format!("{}{}{}", prefix, selected, suffix), window, cx);
        });
    }

    fn prefix_current_line(&mut self, prefix: &str, window: &mut Window, cx: &mut Context<Self>) {
        let prefix = prefix.to_owned();
        let state = self.state.clone();
        state.update(cx, |state, cx| {
            let position = state.text().offset_to_position(state.cursor());
            let line_start = state.text().line_start_offset(position.line as usize);
            let line_start = state.text().offset_to_position(line_start);
            state.set_cursor_position(line_start, window, cx);
            state.insert(prefix, window, cx);
        });
    }

    fn undo(&mut self, cx: &mut Context<Self>) {
        let Some(previous) = self.undo_stack.pop() else {
            return;
        };
        self.redo_stack.push(self.content.clone());
        self.restore_content(previous, cx);
    }

    fn redo(&mut self, cx: &mut Context<Self>) {
        let Some(next) = self.redo_stack.pop() else {
            return;
        };
        self.undo_stack.push(self.content.clone());
        self.restore_content(next, cx);
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
        self.text_input.update(cx, |input, cx| {
            input.prefix_current_line(prefix, window, cx)
        });
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
        self.apply_prefix("# ", "已插入一级标题", window, cx);
    }

    fn bullets_action(&mut self, _: &BulletsText, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_prefix("- ", "已插入无序列表", window, cx);
    }

    fn quote_action(&mut self, _: &QuoteText, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_prefix("> ", "已插入引用", window, cx);
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

    fn undo_action(&mut self, _: &UndoText, _window: &mut Window, cx: &mut Context<Self>) {
        self.text_input.update(cx, |input, cx| input.undo(cx));
        self.status = "已撤销".into();
        cx.notify();
    }

    fn redo_action(&mut self, _: &RedoText, _window: &mut Window, cx: &mut Context<Self>) {
        self.text_input.update(cx, |input, cx| input.redo(cx));
        self.status = "已重做".into();
        cx.notify();
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
                .px_2()
                .child(ribbon_group(
                    "文档",
                    ribbon_controls!(
                        ribbon_button(
                            "icons/new.png",
                            "新建",
                            cx.listener(Self::new_document_click),
                        ),
                        ribbon_button(
                            "icons/open.png",
                            "打开",
                            cx.listener(Self::open_document_click),
                        ),
                        ribbon_button(
                            "icons/save.png",
                            "保存",
                            cx.listener(Self::save_document_click),
                        ),
                        ribbon_button(
                            "icons/open-draft.png",
                            "打开草稿",
                            cx.listener(Self::open_draft_click),
                        ),
                        ribbon_button(
                            "icons/save-draft.png",
                            "保存草稿",
                            cx.listener(Self::save_draft_click),
                        ),
                    ),
                ))
                .child(ribbon_group(
                    "文本",
                    ribbon_controls!(
                        ribbon_button(
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
                        ),
                        ribbon_button(
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
                        ),
                        ribbon_button(
                            "icons/strike.png",
                            "删除线",
                            cx.listener(|editor, _event, window, cx| {
                                editor.strike_action(&StrikeText, window, cx)
                            }),
                        ),
                        ribbon_button(
                            "icons/code.png",
                            "代码",
                            cx.listener(|editor, _event, window, cx| {
                                editor.code_action(&CodeText, window, cx)
                            }),
                        ),
                    ),
                ))
                .child(ribbon_group(
                    "段落",
                    ribbon_controls!(
                        ribbon_button(
                            "icons/bullets.png",
                            "列表",
                            cx.listener(|editor, event, window, cx| {
                                editor.prefix_button("- ", "已插入无序列表", event, window, cx)
                            }),
                        ),
                        ribbon_button(
                            "icons/blockquote.png",
                            "引用",
                            cx.listener(|editor, event, window, cx| {
                                editor.prefix_button("> ", "已插入引用", event, window, cx)
                            }),
                        ),
                        ribbon_button(
                            "icons/heading.png",
                            "标题",
                            cx.listener(|editor, event, window, cx| {
                                editor.prefix_button("# ", "已插入一级标题", event, window, cx)
                            }),
                        ),
                        ribbon_button(
                            "icons/link.png",
                            "链接",
                            cx.listener(|editor, _event, window, cx| {
                                editor.link_action(&LinkText, window, cx)
                            }),
                        ),
                    ),
                ))
                .child(ribbon_group(
                    "历史记录",
                    ribbon_controls!(
                        ribbon_button("icons/undo.png", "撤销", cx.listener(Self::undo_click)),
                        ribbon_button("icons/redo.png", "重做", cx.listener(Self::redo_click)),
                    ),
                ))
                .child(ribbon_group(
                    "文章",
                    ribbon_controls!(
                        ribbon_button(
                            "icons/settings.png",
                            "设置",
                            cx.listener(|editor, _event, window, cx| {
                                editor.toggle_settings(&ToggleSettings, window, cx)
                            }),
                        ),
                        ribbon_button(
                            "icons/notion.png",
                            "Notion",
                            cx.listener(Self::publish_to_notion_click),
                        ),
                        ribbon_button(
                            "icons/typecho.png",
                            "Typecho",
                            cx.listener(Self::publish_to_typecho_click),
                        ),
                        ribbon_button(
                            "icons/preview.png",
                            if self.preview { "编辑" } else { "预览" },
                            cx.listener(Self::toggle_preview_click),
                        ),
                    ),
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
                .px_2()
                .child(ribbon_group(
                    "Markdown",
                    ribbon_controls!(
                        ribbon_button(
                            "icons/heading.png",
                            "标题",
                            cx.listener(|editor, event, window, cx| {
                                editor.prefix_button("# ", "已插入一级标题", event, window, cx)
                            }),
                        ),
                        ribbon_button(
                            "icons/bullets.png",
                            "列表",
                            cx.listener(|editor, event, window, cx| {
                                editor.prefix_button("- ", "已插入无序列表", event, window, cx)
                            }),
                        ),
                        ribbon_button(
                            "icons/blockquote.png",
                            "引用",
                            cx.listener(|editor, event, window, cx| {
                                editor.prefix_button("> ", "已插入引用", event, window, cx)
                            }),
                        ),
                        ribbon_button(
                            "icons/link.png",
                            "链接",
                            cx.listener(|editor, _event, window, cx| {
                                editor.link_action(&LinkText, window, cx)
                            }),
                        ),
                        ribbon_button(
                            "icons/strike.png",
                            "删除线",
                            cx.listener(|editor, _event, window, cx| {
                                editor.strike_action(&StrikeText, window, cx)
                            }),
                        ),
                        ribbon_button(
                            "icons/code.png",
                            "代码",
                            cx.listener(|editor, _event, window, cx| {
                                editor.code_action(&CodeText, window, cx)
                            }),
                        ),
                    ),
                ))
                .child(ribbon_group(
                    "媒体",
                    ribbon_controls!(
                        ribbon_button(
                            "icons/image.png",
                            "图片",
                            cx.listener(|editor, _event, window, cx| {
                                editor.image_action(&ImageText, window, cx)
                            }),
                        ),
                        ribbon_button(
                            "icons/video.png",
                            "视频",
                            cx.listener(|editor, _event, window, cx| {
                                editor.video_action(&VideoText, window, cx)
                            }),
                        ),
                    ),
                ))
                .child(ribbon_group(
                    "结构",
                    ribbon_controls!(
                        ribbon_button(
                            "icons/table.png",
                            "表格",
                            cx.listener(|editor, _event, window, cx| {
                                editor.table_action(&TableText, window, cx)
                            }),
                        ),
                        ribbon_button(
                            "icons/divider.png",
                            "分隔线",
                            cx.listener(|editor, _event, window, cx| {
                                editor.divider_action(&DividerText, window, cx)
                            }),
                        ),
                    ),
                ))
                .into_any_element(),
            _ => div()
                .h(RIBBON_HEIGHT)
                .w_full()
                .flex()
                .id("ribbon-blog-scroll")
                .overflow_x_scroll()
                .bg(rgb(RIBBON_BLUE))
                .border_b_1()
                .border_color(rgb(BORDER))
                .px_2()
                .child(ribbon_group(
                    "草稿",
                    ribbon_controls!(
                        ribbon_button(
                            "icons/new.png",
                            "新建",
                            cx.listener(Self::new_document_click),
                        ),
                        ribbon_button(
                            "icons/open-draft.png",
                            "打开草稿",
                            cx.listener(Self::open_draft_click),
                        ),
                        ribbon_button(
                            "icons/save-draft.png",
                            "保存草稿",
                            cx.listener(Self::save_draft_click),
                        ),
                    ),
                ))
                .child(ribbon_group(
                    "发布",
                    ribbon_controls!(
                        ribbon_button(
                            "icons/notion.png",
                            "Notion",
                            cx.listener(Self::publish_to_notion_click),
                        ),
                        ribbon_button(
                            "icons/typecho.png",
                            "Typecho",
                            cx.listener(Self::publish_to_typecho_click),
                        ),
                    ),
                ))
                .child(ribbon_group(
                    "查看",
                    ribbon_controls!(
                        ribbon_button(
                            "icons/preview.png",
                            if self.preview { "编辑" } else { "预览" },
                            cx.listener(Self::toggle_preview_click),
                        ),
                        ribbon_button(
                            "icons/settings.png",
                            "设置",
                            cx.listener(|editor, _event, window, cx| {
                                editor.toggle_settings(&ToggleSettings, window, cx)
                            }),
                        ),
                    ),
                ))
                .into_any_element(),
        }
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
        .h(RIBBON_TAB_HEIGHT)
        .px_3()
        .flex()
        .items_center()
        .rounded_t_sm()
        .bg(background)
        .text_size(px(14.))
        .text_color(foreground)
        .hover(|style| {
            style
                .bg(if active { rgb(0xffffff) } else { rgb(0x568ac0) })
                .cursor_pointer()
        })
        .active(|style| style.bg(rgb(0xd7e8f6)))
        .on_click(on_click)
        .child(label)
}

fn ribbon_group(label: &'static str, controls: impl IntoElement) -> impl IntoElement {
    div()
        .h_full()
        .flex()
        .flex_col()
        .flex_none()
        .px_1()
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
                .text_color(rgb(MUTED))
                .child(label),
        )
}

fn ribbon_button(
    icon: &'static str,
    label: &'static str,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(label)
        .w(RIBBON_BUTTON_WIDTH)
        .h(RIBBON_BUTTON_HEIGHT)
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
                .w(px(20.))
                .h(px(20.))
                .flex()
                .items_center()
                .justify_center()
                .child(ribbon_icon(icon).size_4()),
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

fn ribbon_icon(path: &'static str) -> Img {
    let asset = EmbeddedAssets::get(path).expect("missing embedded ribbon icon");
    img(Arc::new(Image::from_bytes(
        ImageFormat::Png,
        asset.data.into_owned(),
    )))
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
        gpui_component::init(cx);
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
        let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
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
        EmbeddedAssets, is_image_path, line_end, line_starts, markdown_image_url,
        normalize_newlines, utf16_range_to_utf8,
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

    #[test]
    fn embeds_ribbon_icons() {
        assert!(EmbeddedAssets::get("icons/new.png").is_some());
        assert!(EmbeddedAssets::get("icons/open-draft.png").is_some());
        assert!(EmbeddedAssets::get("icons/save-draft.png").is_some());
        assert!(EmbeddedAssets::get("icons/divider.png").is_some());
        assert!(EmbeddedAssets::get("icons/notion.png").is_some());
        assert!(EmbeddedAssets::get("icons/typecho.png").is_some());
        assert!(EmbeddedAssets::get("Writer.ico").is_some());
    }
}
