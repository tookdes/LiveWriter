##### Q: What is this?

A: Open Live Writer is a Rust + GPUI Markdown editor for writing, previewing,
and publishing blog posts.

##### Q: Which platforms are supported?

A: The application targets macOS and Windows. Both use the same Rust workspace
and GPUI UI.

##### Q: How do I build it?

A: Install stable Rust and run `cargo run`, `cargo test`, or
`cargo build --release` from the repository root.

##### Q: How do I publish to Notion or Typecho?

A: Set the environment variables documented in [`rust/README.md`](rust/README.md)
before launching the application. Without them, the editor remains local.

##### Q: Is this free?

A: Yes. Open Live Writer is released under the [MIT license](license.txt).

##### Q: I found a bug. What should I do?

A: Open an issue with the platform, reproduction steps, and relevant logs or
screenshots.
