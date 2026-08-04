# Contributing to Open Live Writer

Open Live Writer is a Rust + GPUI application. Keep changes small, platform
portable, and free of checked-in credentials.

## Development

Install the stable Rust toolchain, then run:

```sh
cargo fmt --all
cargo test
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release
```

For UI changes, also launch `cargo run` on the target platform and check text
input, clipboard, file dialogs, drafts, and HiDPI scaling.

## Pull requests

Describe the user-visible change and the commands used to test it. Keep the
Rust workspace as the only build/runtime path; do not add managed or native
project files back to the repository.
