use std::{
    env, fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

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

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 原子写入：先写同目录临时文件，再 rename 覆盖目标，
/// 避免写入途中崩溃把原文件写坏。
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "目标路径无效"))?;
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(file_name);
    temp_name.push(format!(".{}.{}.tmp", std::process::id(), counter));
    let temp_path = parent.join(temp_name);

    if let Err(error) = fs::write(&temp_path, bytes) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }

    match fs::rename(&temp_path, path) {
        Ok(()) => Ok(()),
        Err(first_error) => {
            // Windows 上目标已存在时 rename 可能失败；
            // 确认是“已存在”类错误后再替换。
            let can_replace = cfg!(windows)
                && matches!(
                    first_error.kind(),
                    io::ErrorKind::AlreadyExists | io::ErrorKind::PermissionDenied
                );
            let result = if can_replace {
                let removed = match fs::remove_file(path) {
                    Ok(()) => true,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => true,
                    Err(_) => false,
                };
                if removed {
                    fs::rename(&temp_path, path)
                } else {
                    Err(first_error)
                }
            } else {
                Err(first_error)
            };
            if result.is_err() {
                let _ = fs::remove_file(&temp_path);
            }
            result
        }
    }
}

pub fn save_draft(content: &str) -> io::Result<PathBuf> {
    let path = draft_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    atomic_write(&path, content.as_bytes())?;
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
    atomic_write(&path, content.as_bytes())
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
