//! Revision history: a snapshot of the text on every save.
//!
//! Each save appends the saved text under the file's path, newest last,
//! capped per file. The revisions panel lists them with word-count deltas
//! and a compact diff against the current buffer; restoring replaces the
//! buffer through the normal edit path, so it stays undoable. Storage is a
//! single JSON file next to `settings.json`, written off the main loop.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ordet_format::write_bytes_atomically;

use crate::editor_buffer::{AttrSpan, ParagraphAlignment};

/// Snapshots kept per file, newest last.
pub const REVISIONS_PER_FILE: usize = 20;
/// Larger texts are not snapshotted; the cap keeps the file small.
pub const REVISION_TEXT_LIMIT: usize = 200 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Revision {
    /// Unix seconds.
    pub timestamp: u64,
    pub words: usize,
    pub chars: usize,
    pub text: String,
    #[serde(default)]
    pub alignments: Vec<ParagraphAlignment>,
    #[serde(default)]
    pub attrs: Vec<AttrSpan>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HistoryFile {
    #[serde(default)]
    pub revisions: HashMap<String, Vec<Revision>>,
}

impl HistoryFile {
    /// Record `text` for `key` (a display path). Identical consecutive saves
    /// collapse into one entry, so holding Ctrl-P does not flood the history.
    pub fn record(
        &mut self,
        key: &str,
        text: &str,
        alignments: &[ParagraphAlignment],
        attrs: &[AttrSpan],
        timestamp: u64,
    ) {
        if text.len() > REVISION_TEXT_LIMIT {
            return;
        }
        let entries = self.revisions.entry(key.to_string()).or_default();
        if entries.last().is_some_and(|last| {
            last.text == text && last.alignments == alignments && last.attrs == attrs
        }) {
            return;
        }
        let (words, chars) = count_words_chars(text);
        entries.push(Revision {
            timestamp,
            words,
            chars,
            text: text.to_string(),
            alignments: alignments.to_vec(),
            attrs: attrs.to_vec(),
        });
        while entries.len() > REVISIONS_PER_FILE {
            entries.remove(0);
        }
    }

    pub fn revisions_for(&self, key: &str) -> &[Revision] {
        self.revisions.get(key).map(Vec::as_slice).unwrap_or(&[])
    }
}

pub fn count_words_chars(text: &str) -> (usize, usize) {
    (text.split_whitespace().count(), text.chars().count())
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|age| age.as_secs())
        .unwrap_or(0)
}

/// "just now", "5m ago", "3h ago", "9d ago", "4mo ago", "2y ago".
pub fn relative_time(now: u64, then: u64) -> String {
    let delta = now.saturating_sub(then);
    if delta < 60 {
        "just now".to_string()
    } else if delta < 3_600 {
        format!("{}m ago", delta / 60)
    } else if delta < 86_400 {
        format!("{}h ago", delta / 3_600)
    } else if delta < 2_592_000 {
        format!("{}d ago", delta / 86_400)
    } else if delta < 31_536_000 {
        format!("{}mo ago", delta / 2_592_000)
    } else {
        format!("{}y ago", delta / 31_536_000)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    /// ' ' context, '-' removed, '+' added.
    pub tag: char,
    pub text: String,
}

/// Compact line diff: common prefix/suffix trimmed, the changed middle shown
/// as `-` then `+`, with up to `context` unchanged lines on each side.
pub fn diff_lines(old: &str, new: &str, context: usize) -> Vec<DiffLine> {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let mut prefix = 0;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old_lines.len() - prefix
        && suffix < new_lines.len() - prefix
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }

    if prefix == old_lines.len() && prefix == new_lines.len() {
        return vec![DiffLine {
            tag: ' ',
            text: "(no changes)".to_string(),
        }];
    }

    let context = context.min(prefix).min(suffix);
    let mut out = Vec::new();
    for line in &old_lines[prefix - context..prefix] {
        out.push(DiffLine {
            tag: ' ',
            text: line.to_string(),
        });
    }
    for line in &old_lines[prefix..old_lines.len() - suffix] {
        out.push(DiffLine {
            tag: '-',
            text: line.to_string(),
        });
    }
    for line in &new_lines[prefix..new_lines.len() - suffix] {
        out.push(DiffLine {
            tag: '+',
            text: line.to_string(),
        });
    }
    for line in &new_lines[new_lines.len() - suffix..new_lines.len() - suffix + context] {
        out.push(DiffLine {
            tag: ' ',
            text: line.to_string(),
        });
    }
    out
}

pub fn history_path() -> Result<PathBuf, String> {
    if let Some(override_path) = std::env::var_os("ORDET_TUI_HISTORY_PATH") {
        let override_path = PathBuf::from(override_path);
        if !override_path.as_os_str().is_empty() {
            return Ok(override_path);
        }
    }
    let settings = crate::settings::settings_path()?;
    Ok(settings.with_file_name("history.json"))
}

pub fn load_history() -> Result<HistoryFile, String> {
    let path = history_path()?;
    if !path.exists() {
        return Ok(HistoryFile::default());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("Failed to read history {}: {error}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|error| format!("Failed to parse history {}: {error}", path.display()))
}

pub fn save_history(history: &HistoryFile) -> Result<(), String> {
    let path = history_path()?;
    let json = serde_json::to_string(history)
        .map_err(|error| format!("Failed to encode history: {error}"))?;
    // Atomic like recovery: a kill mid-write cannot corrupt the file.
    write_bytes_atomically(&path, json.as_bytes())
        .map_err(|error| format!("Failed to store history {}: {error}", path.display()))
}

pub fn history_key_for(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::{diff_lines, relative_time, HistoryFile};

    #[test]
    fn identical_saves_collapse_and_old_ones_fall_off() {
        let mut history = HistoryFile::default();
        history.record("a.ord", "one", &[], &[], 100);
        history.record("a.ord", "one", &[], &[], 200);
        assert_eq!(history.revisions_for("a.ord").len(), 1);
        for index in 0..super::REVISIONS_PER_FILE + 5 {
            history.record(
                "a.ord",
                &format!("text {index}"),
                &[],
                &[],
                300 + index as u64,
            );
        }
        let revisions = history.revisions_for("a.ord");
        assert_eq!(revisions.len(), super::REVISIONS_PER_FILE);
        assert_eq!(revisions.last().map(|r| r.text.as_str()), Some("text 24"));
    }

    #[test]
    fn oversized_texts_are_not_snapshotted() {
        let mut history = HistoryFile::default();
        history.record(
            "big.ord",
            &"x".repeat(super::REVISION_TEXT_LIMIT + 1),
            &[],
            &[],
            1,
        );
        assert!(history.revisions_for("big.ord").is_empty());
    }

    #[test]
    fn relative_time_reads_naturally() {
        assert_eq!(relative_time(1_000, 999), "just now");
        assert_eq!(relative_time(1_000, 941), "just now");
        assert_eq!(relative_time(1_000, 940), "1m ago");
        assert_eq!(relative_time(10_000, 9_400), "10m ago");
        assert_eq!(relative_time(100_000, 90_000), "2h ago");
        assert_eq!(relative_time(1_000_000, 100_000), "10d ago");
    }

    #[test]
    fn diff_shows_changed_middle_with_context() {
        let old = "keep\nbefore\nchange me\nafter";
        let new = "keep\nbefore\nchanged!\nafter";
        let diff = diff_lines(old, new, 1);
        let rendered: Vec<String> = diff
            .iter()
            .map(|line| format!("{} {}", line.tag, line.text))
            .collect();
        // One shared line of context on each side of the change.
        assert!(rendered.contains(&"  before".to_string()));
        assert!(rendered.contains(&"- change me".to_string()));
        assert!(rendered.contains(&"+ changed!".to_string()));
    }

    #[test]
    fn diff_of_identical_text_says_so() {
        let diff = diff_lines("same\ntext", "same\ntext", 2);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].tag, ' ');
    }

    #[test]
    fn diff_keeps_every_changed_line() {
        let old = (0..150)
            .map(|index| format!("old {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let new = (0..150)
            .map(|index| format!("new {index}"))
            .collect::<Vec<_>>()
            .join("\n");

        let diff = diff_lines(&old, &new, 0);

        assert_eq!(diff.len(), 300);
        assert_eq!(diff.last().map(|line| line.text.as_str()), Some("new 149"));
    }

    #[test]
    fn revisions_keep_rich_formatting() {
        use crate::editor_buffer::{AttrKind, AttrSpan, ParagraphAlignment};

        let mut history = HistoryFile::default();
        let alignments = vec![ParagraphAlignment::Center, ParagraphAlignment::Right];
        let attrs = vec![AttrSpan {
            start: 0,
            end: 3,
            kind: AttrKind::Bold,
        }];
        history.record("draft.ord", "one\ntwo", &alignments, &attrs, 1);
        let revision = &history.revisions_for("draft.ord")[0];
        assert_eq!(revision.alignments, alignments);
        assert_eq!(revision.attrs, attrs);
    }
}
