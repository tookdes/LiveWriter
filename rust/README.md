# Open Live Writer · Rust + GPUI Markdown

这是 Open Live Writer 的新 Rust + GPUI 编辑器入口，旧的 .NET 实现仍保留为外观和发布协议参考。

## 运行

    cargo run --manifest-path rust/Cargo.toml

当前纵向切片：

- 怀旧风格的标题栏、Ribbon 工具栏和双栏工作区；
- UTF-8 Markdown 编辑、中文输入、鼠标选择、剪贴板、撤销/重做；
- 打开/保存 Markdown 文件；
- Markdown 预览：标题、段落、列表、引用、代码块、分隔线、图片、视频和表格；
- 常用行内 Markdown（粗体、斜体、删除线、行内代码、链接）会保留视觉样式；
- 配置环境变量后可直接创建 Notion 页面或提交 Typecho XML-RPC；未配置时安全降级为复制 Markdown。
- 中文输入法使用 GPUI 的 marked-text/UTF-16 接口；尺寸使用 GPUI 逻辑像素，跟随 macOS/Windows 缩放比例。

发布配置（不要提交到仓库）：

    OPEN_LIVE_WRITER_NOTION_TOKEN
    OPEN_LIVE_WRITER_NOTION_PARENT_PAGE_ID
    OPEN_LIVE_WRITER_TYPECHO_XMLRPC_URL
    OPEN_LIVE_WRITER_TYPECHO_USERNAME
    OPEN_LIVE_WRITER_TYPECHO_PASSWORD

设置页和系统凭据存储已接入；旧版账户、草稿、媒体库等大型功能不属于这个最小 Rust 编辑器切片。

跨平台构建：

    cargo test --manifest-path rust/Cargo.toml
    cargo build --manifest-path rust/Cargo.toml --release

Windows 和 macOS 的 Rust 编辑器构建由 GitHub Actions 的 `build-rust-editor` job 验证。
