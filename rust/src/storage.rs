use std::{env, fs, io, path::PathBuf};

fn app_data_dir() -> PathBuf {
    if cfg!(target_os = "windows")
        && let Some(root) = env::var_os("APPDATA")
    {
        return PathBuf::from(root).join("OpenLiveWriter");
    }
    if cfg!(target_os = "macos")
        && let Some(root) = env::var_os("HOME")
    {
        return PathBuf::from(root)
            .join("Library")
            .join("Application Support")
            .join("OpenLiveWriter");
    }
    if let Some(root) = env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(root).join("OpenLiveWriter");
    }
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join(".local")
        .join("share")
        .join("OpenLiveWriter")
}

pub fn draft_path() -> PathBuf {
    app_data_dir().join("draft.md")
}

pub fn autosave_path() -> PathBuf {
    app_data_dir().join("autosave.md")
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

pub fn save_autosave(content: &str) -> io::Result<()> {
    let path = autosave_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, content)
}

pub fn load_autosave() -> io::Result<Option<String>> {
    match fs::read_to_string(autosave_path()) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn clear_autosave() -> io::Result<()> {
    match fs::remove_file(autosave_path()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::{autosave_path, draft_path};

    #[test]
    fn keeps_drafts_in_an_application_data_directory() {
        let path = draft_path();
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("draft.md")
        );
        assert!(path.to_string_lossy().contains("OpenLiveWriter"));
        assert_eq!(
            autosave_path().file_name().and_then(|name| name.to_str()),
            Some("autosave.md")
        );
    }
}
