use std::fs;
use std::path::{Path, PathBuf};

use ordet_format::write_bytes_atomically;
use serde::{Deserialize, Serialize};

/// One unsaved draft as it was last snapshotted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveredDraft {
    /// `None` for a scratch buffer that has never been saved anywhere.
    #[serde(default)]
    pub path: Option<PathBuf>,
    pub title: String,
    pub text: String,
}

/// The on-disk recovery snapshot.
///
/// Only drafts with unsaved edits are recorded, so an empty `drafts` list means
/// there is nothing to recover and the file can be removed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryFile {
    #[serde(default)]
    pub drafts: Vec<RecoveredDraft>,
}

impl RecoveryFile {
    pub fn is_empty(&self) -> bool {
        self.drafts.is_empty()
    }
}

pub fn recovery_path() -> Result<PathBuf, String> {
    if let Some(override_path) = std::env::var_os("ORDET_TUI_RECOVERY_PATH") {
        let override_path = PathBuf::from(override_path);
        if !override_path.as_os_str().is_empty() {
            return Ok(override_path);
        }
    }

    let settings = crate::settings::settings_path()?;
    let parent = settings
        .parent()
        .ok_or_else(|| format!("Settings path {} has no parent.", settings.display()))?;
    Ok(parent.join("recovery.json"))
}

pub fn load_recovery() -> Result<RecoveryFile, String> {
    let path = recovery_path()?;
    read_recovery_from(&path)
}

pub fn read_recovery_from(path: &Path) -> Result<RecoveryFile, String> {
    if !path.exists() {
        return Ok(RecoveryFile::default());
    }

    let text = fs::read_to_string(path)
        .map_err(|error| format!("Failed to read recovery file {}: {error}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(RecoveryFile::default());
    }
    serde_json::from_str(&text)
        .map_err(|error| format!("Failed to parse recovery file {}: {error}", path.display()))
}

pub fn save_recovery(file: &RecoveryFile) -> Result<(), String> {
    let path = recovery_path()?;
    write_recovery_to(&path, file)
}

/// Writes the snapshot atomically, so a crash mid-write cannot leave a
/// half-written file that fails to parse on the next launch.
pub fn write_recovery_to(path: &Path, file: &RecoveryFile) -> Result<(), String> {
    let json = serde_json::to_string(file)
        .map_err(|error| format!("Failed to encode recovery snapshot: {error}"))?;
    write_bytes_atomically(path, json.as_bytes()).map_err(|error| {
        format!(
            "Failed to replace recovery file {}: {error}",
            path.display()
        )
    })
}

pub fn clear_recovery() -> Result<(), String> {
    let path = recovery_path()?;
    clear_recovery_at(&path)
}

pub fn clear_recovery_at(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "Failed to remove recovery file {}: {error}",
            path.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        clear_recovery_at, read_recovery_from, write_recovery_to, RecoveredDraft, RecoveryFile,
    };

    fn temp_path(name: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ordet-recovery-test-{}-{name}.json",
            std::process::id()
        ));
        path
    }

    #[test]
    fn a_missing_recovery_file_reads_as_empty() {
        let path = temp_path("missing");
        let _ = clear_recovery_at(&path);

        let loaded = read_recovery_from(&path).expect("missing file is not an error");

        assert!(loaded.is_empty());
    }

    #[test]
    fn drafts_survive_a_round_trip() {
        let path = temp_path("roundtrip");
        let file = RecoveryFile {
            drafts: vec![
                RecoveredDraft {
                    path: Some(std::path::PathBuf::from("/tmp/Alpha.ord")),
                    title: "Alpha.ord".to_string(),
                    text: "alpha body".to_string(),
                },
                RecoveredDraft {
                    path: None,
                    title: "Untitled".to_string(),
                    text: "scratch".to_string(),
                },
            ],
        };

        write_recovery_to(&path, &file).expect("write");
        let loaded = read_recovery_from(&path).expect("read");
        let _ = clear_recovery_at(&path);

        assert_eq!(loaded, file);
    }

    #[test]
    fn a_corrupt_recovery_file_reports_an_error_instead_of_panicking() {
        let path = temp_path("corrupt");
        std::fs::write(&path, "{ not json").expect("write");

        let result = read_recovery_from(&path);
        let _ = clear_recovery_at(&path);

        assert!(result.is_err());
    }

    #[test]
    fn an_empty_file_reads_as_nothing_to_recover() {
        let path = temp_path("blank");
        std::fs::write(&path, "   \n").expect("write");

        let loaded = read_recovery_from(&path).expect("blank file is not an error");
        let _ = clear_recovery_at(&path);

        assert!(loaded.is_empty());
    }

    #[test]
    fn clearing_a_recovery_file_that_is_already_gone_succeeds() {
        let path = temp_path("gone");
        let _ = clear_recovery_at(&path);

        clear_recovery_at(&path).expect("clearing twice is not an error");
    }
}
