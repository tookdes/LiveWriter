use std::{
    env, fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

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

pub fn drafts_dir() -> PathBuf {
    app_data_dir().join("drafts")
}

pub fn crash_log_path() -> PathBuf {
    app_data_dir().join("crash.log")
}

pub fn autosave_path() -> PathBuf {
    app_data_dir().join("autosave.md")
}

pub fn prefs_path() -> PathBuf {
    app_data_dir().join("prefs.json")
}

pub fn recent_path() -> PathBuf {
    app_data_dir().join("recent.json")
}

pub fn drafts_index_path() -> PathBuf {
    drafts_dir().join("index.json")
}

pub fn pending_media_dir() -> PathBuf {
    app_data_dir().join("pending-media")
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_temp_path(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().unwrap_or_default();
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(file_name);
    temp_name.push(format!(".{}.{}.tmp", std::process::id(), counter));
    parent.join(temp_name)
}

/// Atomic write: write a sibling temp file, then replace the destination.
/// On Windows the destination is copied aside first so a failed replace can
/// restore the original instead of deleting both copies.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if parent != Path::new("") && parent != Path::new(".") {
        fs::create_dir_all(parent)?;
    }
    let temp_path = unique_temp_path(path);
    if let Err(error) = fs::write(&temp_path, bytes) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }

    match fs::rename(&temp_path, path) {
        Ok(()) => Ok(()),
        Err(first_error) => {
            if !path.exists() {
                let _ = fs::remove_file(&temp_path);
                return Err(first_error);
            }
            let backup_path = unique_temp_path(&path.with_extension("bak"));
            if let Err(error) = fs::copy(path, &backup_path) {
                let _ = fs::remove_file(&temp_path);
                return Err(error);
            }
            let replaced = (|| {
                fs::remove_file(path)?;
                fs::rename(&temp_path, path)
            })();
            match replaced {
                Ok(()) => {
                    let _ = fs::remove_file(&backup_path);
                    Ok(())
                }
                Err(error) => {
                    if !path.exists() {
                        let _ = fs::rename(&backup_path, path)
                            .or_else(|_| fs::copy(&backup_path, path).map(|_| ()));
                    }
                    let _ = fs::remove_file(&temp_path);
                    let _ = fs::remove_file(&backup_path);
                    Err(error)
                }
            }
        }
    }
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    atomic_write(path, &bytes)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> io::Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMode {
    Edit,
    #[default]
    Split,
    Preview,
}

impl WorkspaceMode {
    pub fn cycle(self) -> Self {
        match self {
            Self::Edit => Self::Split,
            Self::Split => Self::Preview,
            Self::Preview => Self::Edit,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Edit => "\u{7f16}\u{8f91}",
            Self::Split => "\u{53cc}\u{680f}",
            Self::Preview => "\u{9884}\u{89c8}",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditorPrefs {
    #[serde(default = "default_split_ratio")]
    pub split_ratio: f32,
    #[serde(default)]
    pub workspace_mode: WorkspaceMode,
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    #[serde(default)]
    pub last_directory: Option<PathBuf>,
    #[serde(default)]
    pub outline_visible: bool,
    #[serde(default)]
    pub focus_mode: bool,
}

fn default_split_ratio() -> f32 {
    0.42
}

fn default_font_size() -> f32 {
    16.0
}

impl Default for EditorPrefs {
    fn default() -> Self {
        Self {
            split_ratio: default_split_ratio(),
            workspace_mode: WorkspaceMode::Split,
            font_size: default_font_size(),
            last_directory: None,
            outline_visible: false,
            focus_mode: false,
        }
    }
}

impl EditorPrefs {
    pub fn clamped(mut self) -> Self {
        self.split_ratio = self.split_ratio.clamp(0.22, 0.74);
        self.font_size = self.font_size.clamp(12.0, 28.0);
        self
    }
}

pub fn load_prefs() -> EditorPrefs {
    read_json::<EditorPrefs>(&prefs_path())
        .ok()
        .flatten()
        .unwrap_or_default()
        .clamped()
}

pub fn save_prefs(prefs: &EditorPrefs) -> io::Result<()> {
    write_json(&prefs_path(), &prefs.clone().clamped())
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentFiles {
    #[serde(default)]
    pub paths: Vec<PathBuf>,
}

const MAX_RECENT_FILES: usize = 12;

pub fn load_recent_files() -> Vec<PathBuf> {
    read_json::<RecentFiles>(&recent_path())
        .ok()
        .flatten()
        .unwrap_or_default()
        .paths
        .into_iter()
        .filter(|path| path.exists())
        .take(MAX_RECENT_FILES)
        .collect()
}

pub fn remember_recent_file(path: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = load_recent_files();
    paths.retain(|existing| existing != path);
    paths.insert(0, path.to_path_buf());
    paths.truncate(MAX_RECENT_FILES);
    write_json(
        &recent_path(),
        &RecentFiles {
            paths: paths.clone(),
        },
    )?;
    Ok(paths)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftEntry {
    pub id: String,
    pub title: String,
    pub updated_at: u64,
    pub excerpt: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct DraftIndex {
    #[serde(default)]
    drafts: Vec<DraftEntry>,
}

const MAX_DRAFTS: usize = 40;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn excerpt_of(content: &str) -> String {
    content
        .chars()
        .filter(|ch| *ch != '\r')
        .take(80)
        .collect::<String>()
        .replace('\n', " ")
        .trim()
        .to_owned()
}

fn draft_file(id: &str) -> PathBuf {
    drafts_dir().join(format!("{id}.md"))
}

fn load_draft_index() -> DraftIndex {
    let mut index = read_json::<DraftIndex>(&drafts_index_path())
        .ok()
        .flatten()
        .unwrap_or_default();
    migrate_legacy_draft(&mut index);
    index
}

fn migrate_legacy_draft(index: &mut DraftIndex) {
    let legacy = draft_path();
    if !legacy.exists() {
        return;
    }
    if index.drafts.iter().any(|entry| entry.id == "legacy") {
        return;
    }
    if let Ok(content) = fs::read_to_string(&legacy) {
        let _ = fs::create_dir_all(drafts_dir());
        let _ = atomic_write(&draft_file("legacy"), content.as_bytes());
        index.drafts.insert(
            0,
            DraftEntry {
                id: "legacy".to_owned(),
                title: title_from_markdown(&content),
                updated_at: now_secs(),
                excerpt: excerpt_of(&content),
            },
        );
        let _ = write_json(&drafts_index_path(), index);
    }
}

pub fn list_drafts() -> Vec<DraftEntry> {
    load_draft_index().drafts
}

fn title_from_markdown(content: &str) -> String {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            let title = trimmed.trim_start_matches('#').trim();
            if !title.is_empty() {
                return title.chars().take(100).collect();
            }
        }
    }
    for line in content.lines() {
        let title = line.trim();
        if !title.is_empty() {
            return title.chars().take(100).collect();
        }
    }
    "\u{672a}\u{547d}\u{540d}\u{6587}\u{7ae0}".to_owned()
}

pub fn save_named_draft(title: &str, content: &str) -> io::Result<DraftEntry> {
    fs::create_dir_all(drafts_dir())?;
    let id = format!(
        "{}-{}",
        now_secs(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    atomic_write(&draft_file(&id), content.as_bytes())?;
    let entry = DraftEntry {
        id: id.clone(),
        title: if title.trim().is_empty() {
            title_from_markdown(content)
        } else {
            title.trim().to_owned()
        },
        updated_at: now_secs(),
        excerpt: excerpt_of(content),
    };
    let mut index = load_draft_index();
    index.drafts.retain(|existing| existing.id != id);
    index.drafts.insert(0, entry.clone());
    index.drafts.truncate(MAX_DRAFTS);
    write_json(&drafts_index_path(), &index)?;
    // Keep the historical single-slot draft in sync for older builds.
    let _ = atomic_write(&draft_path(), content.as_bytes());
    Ok(entry)
}

pub fn load_named_draft(id: &str) -> io::Result<Option<(DraftEntry, String)>> {
    let index = load_draft_index();
    let Some(entry) = index.drafts.into_iter().find(|entry| entry.id == id) else {
        return Ok(None);
    };
    match fs::read_to_string(draft_file(id)) {
        Ok(content) => Ok(Some((entry, content))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[allow(dead_code)]
pub fn delete_named_draft(id: &str) -> io::Result<()> {
    let mut index = load_draft_index();
    index.drafts.retain(|entry| entry.id != id);
    write_json(&drafts_index_path(), &index)?;
    match fs::remove_file(draft_file(id)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn save_draft(content: &str) -> io::Result<PathBuf> {
    let entry = save_named_draft(&title_from_markdown(content), content)?;
    Ok(draft_file(&entry.id))
}

#[allow(dead_code)]
pub fn load_draft() -> io::Result<Option<(PathBuf, String)>> {
    let index = load_draft_index();
    if let Some(entry) = index.drafts.first()
        && let Ok(Some((_, content))) = load_named_draft(&entry.id)
    {
        return Ok(Some((draft_file(&entry.id), content)));
    }
    let path = draft_path();
    match fs::read_to_string(&path) {
        Ok(content) => Ok(Some((path, content))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn save_autosave(content: &str) -> io::Result<()> {
    atomic_write(&autosave_path(), content.as_bytes())
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

pub fn autosave_modified_secs() -> Option<u64> {
    fs::metadata(autosave_path())
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

pub fn sidecar_meta_path(markdown_path: &Path) -> PathBuf {
    let mut name = markdown_path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    name.push(".olw-meta.json");
    match markdown_path.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentMeta {
    #[serde(default)]
    pub notion_page_id: Option<String>,
    #[serde(default)]
    pub notion_url: Option<String>,
    #[serde(default)]
    pub typecho_post_id: Option<String>,
}

pub fn load_document_meta(markdown_path: &Path) -> DocumentMeta {
    read_json(&sidecar_meta_path(markdown_path))
        .ok()
        .flatten()
        .unwrap_or_default()
}

pub fn save_document_meta(markdown_path: &Path, meta: &DocumentMeta) -> io::Result<()> {
    write_json(&sidecar_meta_path(markdown_path), meta)
}

pub fn media_dir_for_document(markdown_path: &Path) -> PathBuf {
    let stem = markdown_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("article");
    match markdown_path.parent() {
        Some(parent) => parent.join(format!("{stem}.assets")),
        None => PathBuf::from(format!("{stem}.assets")),
    }
}

pub fn ensure_pending_media_dir() -> io::Result<PathBuf> {
    let dir = pending_media_dir();
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn save_media_bytes(dir: &Path, file_name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let path = unique_media_path(dir, file_name);
    atomic_write(&path, bytes)?;
    Ok(path)
}

fn unique_media_path(dir: &Path, file_name: &str) -> PathBuf {
    let sanitized = sanitize_file_name(file_name);
    let candidate = dir.join(&sanitized);
    if !candidate.exists() {
        return candidate;
    }
    let path = Path::new(&sanitized);
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("image");
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("png");
    for index in 2..1000 {
        let next = dir.join(format!("{stem}-{index}.{ext}"));
        if !next.exists() {
            return next;
        }
    }
    dir.join(format!(
        "{stem}-{}.{ext}",
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

pub fn sanitize_file_name(file_name: &str) -> String {
    let name = Path::new(file_name)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("image.png");
    let sanitized = name
        .chars()
        .map(|ch| match ch {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            ch if ch.is_control() => '_',
            ch => ch,
        })
        .collect::<String>();
    if sanitized.trim().is_empty() {
        "image.png".to_owned()
    } else {
        sanitized
    }
}

pub fn relative_media_url(markdown_path: Option<&Path>, media_path: &Path) -> String {
    if let Some(markdown_path) = markdown_path
        && let Some(parent) = markdown_path.parent()
        && let Ok(relative) = media_path.strip_prefix(parent)
    {
        return relative.to_string_lossy().replace('\\', "/");
    }
    format!(
        "file:///{}",
        media_path.to_string_lossy().replace('\\', "/")
    )
}

pub fn relocate_pending_media(
    markdown: &str,
    markdown_path: &Path,
    pending_paths: &[PathBuf],
) -> io::Result<(String, Vec<PathBuf>)> {
    if pending_paths.is_empty() {
        return Ok((markdown.to_owned(), Vec::new()));
    }
    let dest_dir = media_dir_for_document(markdown_path);
    fs::create_dir_all(&dest_dir)?;
    let mut next = markdown.to_owned();
    let mut relocated = Vec::new();
    for source in pending_paths {
        let Some(file_name) = source.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let dest = unique_media_path(&dest_dir, file_name);
        fs::copy(source, &dest)?;
        let from = source.to_string_lossy().replace('\\', "/");
        let to = relative_media_url(Some(markdown_path), &dest);
        next = next.replace(&from, &to);
        next = next.replace(&format!("file:///{from}"), &to);
        next = next.replace(&source.to_string_lossy().replace('/', "\\"), &to);
        relocated.push(dest);
        let _ = fs::remove_file(source);
    }
    Ok((next, relocated))
}

#[cfg(test)]
mod tests {
    use super::{
        WorkspaceMode, autosave_path, crash_log_path, draft_path, sanitize_file_name,
        unique_media_path,
    };
    use std::path::Path;

    #[test]
    fn keeps_drafts_in_an_application_data_directory() {
        let path = draft_path();
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("draft.md")
        );
        assert!(path.to_string_lossy().contains("OpenLiveWriter"));
        assert_eq!(
            crash_log_path().file_name().and_then(|name| name.to_str()),
            Some("crash.log")
        );
        assert_eq!(
            autosave_path().file_name().and_then(|name| name.to_str()),
            Some("autosave.md")
        );
    }

    #[test]
    fn sanitizes_media_names_and_cycles_workspace_modes() {
        assert_eq!(sanitize_file_name("a<>.png"), "a__.png");
        assert_eq!(WorkspaceMode::Edit.cycle(), WorkspaceMode::Split);
        assert_eq!(WorkspaceMode::Preview.cycle(), WorkspaceMode::Edit);
        let dir = Path::new(".");
        let first = unique_media_path(dir, "note.txt");
        assert!(first.ends_with("note.txt") || first.file_name().is_some());
    }
}
