# Open Live Writer

Open Live Writer is a cross-platform Markdown editor written in **Rust** with
[GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui). The Rust
binary is the only application entry point; the old managed and native
implementations are no longer part of the tree.

## Build and run

```sh
cargo run
cargo test
cargo build --release
```

PowerShell users can run `./build.ps1` for a release build.

## Features

- Ribbon-style editor with Markdown editing, preview, undo/redo, and clipboard;
- Chinese IME marked text, UTF-16 text ranges, and logical-pixel HiDPI layout;
- UTF-8 Markdown open/save and local draft recovery;
- Markdown preview for headings, lists, quotes, code, images, video, tables,
  inline formatting, and dividers;
- optional Notion page creation and Typecho MetaWeblog publishing;
- OS application-data storage for drafts and publishing credentials.

Publishing configuration is read from environment variables. See
[`rust/README.md`](rust/README.md); credentials must never be committed.

## Contributing

Run `cargo fmt --all`, `cargo test`, and
`cargo clippy --workspace --all-targets -- -D warnings` before opening a pull
request. See [`CONTRIBUTING.md`](CONTRIBUTING.md).

## License

Open Live Writer is released under the [MIT License](license.txt).
