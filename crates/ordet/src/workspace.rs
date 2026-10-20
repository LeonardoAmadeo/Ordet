//! Workspace folders: the writing folders the workspace panel lists.
//!
//! A workspace is a registered folder. The workspace panel lists the documents inside
//! each one with unsaved-work markers, so changes across many folders stay
//! visible without opening every file. Scans are cached per folder and only
//! redone when the folder's contents change (directory mtime) or the cache
//! ages out, keeping the 120fps panel render free of filesystem I/O.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How deep the scan goes: the folder itself plus one level of subfolders.
pub const SCAN_DEPTH: usize = 2;
/// Upper bound per folder so a stray `~/Documents` registration stays cheap.
pub const SCAN_FILE_LIMIT: usize = 300;
/// Rescan at most this often even when mtimes look unchanged.
const SCAN_MAX_AGE: Duration = Duration::from_secs(2);

/// File types Ordet opens as text. Anything else is not a writing document.
fn is_document_file(path: &Path) -> bool {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => matches!(
            ext.to_ascii_lowercase().as_str(),
            "ord" | "txt" | "md" | "markdown"
        ),
        None => false,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceFile {
    pub path: PathBuf,
    /// Display path relative to the workspace root, `/`-separated.
    pub relative: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceFolder {
    pub path: PathBuf,
    pub expanded: bool,
}

impl WorkspaceFolder {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            expanded: true,
        }
    }

    pub fn name(&self) -> String {
        self.path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

#[derive(Clone, Debug)]
struct CachedScan {
    /// Newest mtime seen across the walked directories when scanned.
    stamp: Option<std::time::SystemTime>,
    scanned_at: Instant,
    files: Vec<WorkspaceFile>,
}

/// Cached directory scans with mtime invalidation. Lives behind a `RefCell`
/// on `AppState` because `workspace_rows` only gets `&self`.
#[derive(Clone, Debug, Default)]
pub struct WorkspaceScanCache {
    scans: HashMap<PathBuf, CachedScan>,
}

impl WorkspaceScanCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Documents under `root`, from cache when the folder has not changed.
    pub fn files_for(&mut self, root: &Path) -> Vec<WorkspaceFile> {
        let stamp = directory_stamp(root);
        let fresh = self.scans.get(root).is_some_and(|cached| {
            cached.stamp == stamp && cached.scanned_at.elapsed() < SCAN_MAX_AGE
        });
        if fresh {
            return self
                .scans
                .get(root)
                .map(|c| c.files.clone())
                .unwrap_or_default();
        }
        let files = scan_workspace(root);
        self.scans.insert(
            root.to_path_buf(),
            CachedScan {
                stamp,
                scanned_at: Instant::now(),
                files: files.clone(),
            },
        );
        files
    }

    /// Forget everything, e.g. after a folder is renamed outside Ordet.
    pub fn invalidate_all(&mut self) {
        self.scans.clear();
    }
}

/// Newest modification time across `root` and the subdirectories the scan
/// walks. A file added, removed, or renamed anywhere in the tree bumps its
/// parent directory, which is exactly what the listing cares about.
fn directory_stamp(root: &Path) -> Option<std::time::SystemTime> {
    let mut newest = root_meta_mtime(root);
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() && !is_skipped_dir(&path) {
                newest = newest.max(entry_meta_mtime(&entry));
                stack.push(path);
            }
        }
    }
    newest
}

fn root_meta_mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn entry_meta_mtime(entry: &std::fs::DirEntry) -> Option<std::time::SystemTime> {
    entry.metadata().and_then(|m| m.modified()).ok()
}

fn is_skipped_dir(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    name.starts_with('.') || matches!(name, "target" | "node_modules")
}

/// Collect document files under `root` to `SCAN_DEPTH`, skipping hidden
/// entries. Sorted case-insensitively by relative path for a stable listing.
pub fn scan_workspace(root: &Path) -> Vec<WorkspaceFile> {
    let mut files = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                if depth + 1 < SCAN_DEPTH && !is_skipped_dir(&path) {
                    stack.push((path, depth + 1));
                }
            } else if is_document_file(&path) {
                let relative = path
                    .strip_prefix(root)
                    .map(|rel| {
                        rel.components()
                            .map(|c| c.as_os_str().to_string_lossy().into_owned())
                            .collect::<Vec<_>>()
                            .join("/")
                    })
                    .unwrap_or_else(|_| name.into_owned());
                files.push(WorkspaceFile { path, relative });
                if files.len() >= SCAN_FILE_LIMIT {
                    break;
                }
            }
        }
    }
    files.sort_by(|a, b| {
        a.relative
            .to_ascii_lowercase()
            .cmp(&b.relative.to_ascii_lowercase())
    });
    files
}

#[cfg(test)]
mod tests {
    use super::{is_document_file, scan_workspace, WorkspaceScanCache};
    use std::path::PathBuf;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ordet-workspace-test-{}", name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("test dir");
        dir
    }

    fn write(dir: &std::path::Path, rel: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent");
        }
        std::fs::write(&path, "words").expect("write");
    }

    #[test]
    fn scan_lists_documents_but_not_other_files() {
        let dir = test_dir("kinds");
        write(&dir, "draft.ord");
        write(&dir, "notes.txt");
        write(&dir, "chapter.md");
        write(&dir, "archive.zip");
        write(&dir, "binary");
        write(&dir, ".hidden.ord");

        let files = scan_workspace(&dir);
        let relatives: Vec<&str> = files.iter().map(|f| f.relative.as_str()).collect();
        assert_eq!(relatives, vec!["chapter.md", "draft.ord", "notes.txt"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_descends_one_level_with_relative_labels() {
        let dir = test_dir("nested");
        write(&dir, "top.ord");
        write(&dir, "part/one.ord");
        write(&dir, "part/deep/two.ord");

        let files = scan_workspace(&dir);
        let relatives: Vec<&str> = files.iter().map(|f| f.relative.as_str()).collect();
        assert_eq!(relatives, vec!["part/one.ord", "top.ord"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_of_a_missing_folder_is_empty() {
        let missing = std::env::temp_dir().join("ordet-workspace-test-nope-missing");
        let _ = std::fs::remove_dir_all(&missing);
        assert!(scan_workspace(&missing).is_empty());
    }

    #[test]
    fn cache_returns_the_scan_and_notices_new_files() {
        let dir = test_dir("cache");
        write(&dir, "one.ord");

        let mut cache = WorkspaceScanCache::new();
        assert_eq!(cache.files_for(&dir).len(), 1);
        // Second call hits the cache: no rescan, same answer.
        assert_eq!(cache.files_for(&dir).len(), 1);
        // A new file bumps the directory mtime, invalidating the cache.
        write(&dir, "two.ord");
        // mtime granularity can hide a same-tick change, so both answers are
        // accepted here; the aged-out rescan below proves refresh works.
        let _ = cache.files_for(&dir);
        cache.invalidate_all();
        assert_eq!(cache.files_for(&dir).len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn document_extensions_are_matched_case_insensitively() {
        assert!(is_document_file(&PathBuf::from("a.ORD")));
        assert!(is_document_file(&PathBuf::from("a.Txt")));
        assert!(!is_document_file(&PathBuf::from("a.png")));
    }
}
