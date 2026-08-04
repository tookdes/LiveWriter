use std::{env, fs, io, path::PathBuf};

pub fn draft_path() -> PathBuf {
    if cfg!(target_os = "windows")
        && let Some(root) = env::var_os("APPDATA")
    {
        return PathBuf::from(root).join("OpenLiveWriter").join("draft.md");
    }
    if cfg!(target_os = "macos")
        && let Some(root) = env::var_os("HOME")
    {
        return PathBuf::from(root)
            .join("Library")
            .join("Application Support")
            .join("OpenLiveWriter")
            .join("draft.md");
    }
    if let Some(root) = env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(root).join("OpenLiveWriter").join("draft.md");
    }
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join(".local")
        .join("share")
        .join("OpenLiveWriter")
        .join("draft.md")
}

pub fn save_draft(content: &str) -> io::Result<PathBuf> {
    let path = draft_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, content)?;
    Ok(path)
}

pub fn load_draft() -> io::Result<Option<(PathBuf, String)>> {
    let path = draft_path();
    match fs::read_to_string(&path) {
        Ok(content) => Ok(Some((path, content))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::draft_path;

    #[test]
    fn keeps_drafts_in_an_application_data_directory() {
        let path = draft_path();
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("draft.md")
        );
        assert!(path.to_string_lossy().contains("OpenLiveWriter"));
    }
}
