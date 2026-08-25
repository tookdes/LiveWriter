# Open Live Writer · Rust + GPUI

这是 Open Live Writer 的唯一应用实现：Rust + GPUI Markdown 编辑器。

## 运行

```sh
cargo run --manifest-path rust/Cargo.toml
```

功能包括：

- 怀旧风格标题栏、Ribbon 工具栏和双栏工作区；
- UTF-8 Markdown 编辑、中文输入法、鼠标选择、剪贴板、撤销/重做；
- 打开 UTF-8 / UTF-16 / GBK 文件（保持原有换行与 BOM），保存 Markdown 文件；
- 跨平台本地草稿、每 30 秒自动备份与启动恢复；
- 标题、段落、列表、引用、代码块、分隔线、图片、视频、表格预览；
- 粗体、斜体、删除线、行内代码和链接等常用行内 Markdown；
- 可选的 Notion 页面创建和 Typecho XML-RPC 发布。

中文输入法使用 GPUI 的 marked-text/UTF-16 接口；尺寸使用 GPUI 逻辑像素，
跟随 macOS/Windows 缩放比例。

发布配置（不要提交到仓库）：

```text
OPEN_LIVE_WRITER_NOTION_TOKEN
OPEN_LIVE_WRITER_NOTION_PARENT_PAGE_ID
OPEN_LIVE_WRITER_TYPECHO_XMLRPC_URL
OPEN_LIVE_WRITER_TYPECHO_USERNAME
OPEN_LIVE_WRITER_TYPECHO_PASSWORD
```

`OPEN_LIVE_WRITER_NOTION_PARENT_PAGE_ID` 可填写普通页面 ID、数据库 ID 或完整 Notion 链接。

```sh
cargo fmt --all
cargo test --manifest-path rust/Cargo.toml
cargo clippy --manifest-path rust/Cargo.toml --all-targets -- -D warnings
cargo build --manifest-path rust/Cargo.toml --release
```
