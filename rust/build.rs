fn main() {
    println!("cargo:rerun-if-changed=assets/Writer.ico");
    println!("cargo:rerun-if-changed=resources/windows/open_live_writer.rc");

    #[cfg(windows)]
    embed_resource::compile(
        "resources/windows/open_live_writer.rc",
        embed_resource::NONE,
    )
    .manifest_optional()
    .expect("failed to embed Open Live Writer icon");
}
