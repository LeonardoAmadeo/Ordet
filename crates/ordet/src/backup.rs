//! File backups, Scrivener Backup-tab style (minus zip): on demand and on
//! quit, each tracked file copied beside a millisecond timestamp, keeping
//! the newest N per file. Untitled drafts have no path to copy, so they
//! never back up, the quit prompt already guards those.

use std::path::{Path, PathBuf};

/// Suffix marker separating the original stem from the backup timestamp.
const MARKER: &str = ".backup-";

/// Where backups land when no custom directory is set: next to settings.json.
pub fn default_backup_dir(settings_file: &Path) -> PathBuf {
    settings_file
        .parent()
        .map(|parent| parent.join("Backups"))
        .unwrap_or_else(|| PathBuf::from("Backups"))
}

/// Copy `source` into `backup_dir` as `{stem}.backup-{millis}{ext}`.
/// Returns the backup path.
pub fn backup_file(source: &Path, backup_dir: &Path, now_millis: u64) -> Result<PathBuf, String> {
    let name = source
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| format!("Cannot back up {}", source.display()))?;
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    };
    std::fs::create_dir_all(backup_dir)
        .map_err(|error| format!("Cannot create {}: {error}", backup_dir.display()))?;
    let mut candidate = backup_dir.join(format!("{stem}{MARKER}{now_millis}{ext}"));
    // Same-millisecond reruns must not clobber: bump a counter suffix.
    let mut counter = 2u32;
    while candidate.exists() {
        candidate = backup_dir.join(format!("{stem}{MARKER}{now_millis}-{counter}{ext}"));
        counter += 1;
    }
    std::fs::copy(source, &candidate)
        .map_err(|error| format!("Cannot back up {}: {error}", source.display()))?;
    Ok(candidate)
}

/// Drop all but the newest `keep` backups of the file named `file_name` in
/// `backup_dir`. Returns how many were removed.
pub fn prune_backups(backup_dir: &Path, file_name: &str, keep: usize) -> usize {
    let (stem, ext) = match file_name.rfind('.') {
        Some(dot) if dot > 0 => (&file_name[..dot], &file_name[dot..]),
        _ => (file_name, ""),
    };
    let Ok(entries) = std::fs::read_dir(backup_dir) else {
        return 0;
    };
    let mut matches: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                return false;
            };
            name.starts_with(&format!("{stem}{MARKER}")) && name.ends_with(ext)
        })
        .collect();
    // Names sort chronologically: fixed marker, numeric timestamp, same stem.
    matches.sort();
    let excess = matches.len().saturating_sub(keep.max(1));
    matches
        .into_iter()
        .take(excess)
        .filter(|path| std::fs::remove_file(path).is_ok())
        .count()
}

/// Current time in milliseconds since the Unix epoch. Falls back to zero
/// rather than failing when the clock misbehaves.
pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|age| age.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{backup_file, now_millis, prune_backups};

    fn unique_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ordet-backup-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn backup_copies_and_prunes_to_keep() {
        let root = unique_dir("prune");
        let source = root.join("draft.ord");
        std::fs::write(&source, "v1").expect("seed");
        let backups = root.join("Backups");

        for version in 1..=4u64 {
            std::fs::write(&source, format!("v{version}")).expect("seed");
            backup_file(&source, &backups, 1_700_000_000_000 + version).expect("backup");
        }
        let removed = prune_backups(&backups, "draft.ord", 2);
        assert_eq!(removed, 2);
        let remaining: Vec<_> = std::fs::read_dir(&backups)
            .expect("read")
            .filter_map(|entry| {
                entry.ok().and_then(|entry| {
                    entry
                        .path()
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_string)
                })
            })
            .collect();
        assert_eq!(remaining.len(), 2);
        // Newest two survive: contents v3 and v4 in some order of names.
        let mut bodies: Vec<String> = remaining
            .iter()
            .map(|name| std::fs::read_to_string(backups.join(name)).expect("read"))
            .collect();
        bodies.sort();
        assert_eq!(bodies, vec!["v3".to_string(), "v4".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn backup_never_clobbers_same_millisecond() {
        let root = unique_dir("collision");
        let source = root.join("note.txt");
        std::fs::write(&source, "x").expect("seed");
        let backups = root.join("Backups");
        let first = backup_file(&source, &backups, now_millis()).expect("first");
        let second = backup_file(&source, &backups, now_millis()).expect("second");
        assert_ne!(first, second);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prune_ignores_other_files() {
        let root = unique_dir("others");
        let backups = root.join("Backups");
        std::fs::create_dir_all(&backups).expect("dir");
        std::fs::write(backups.join("unrelated.txt"), "keep me").expect("seed");
        assert_eq!(prune_backups(&backups, "draft.ord", 1), 0);
        assert!(backups.join("unrelated.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
