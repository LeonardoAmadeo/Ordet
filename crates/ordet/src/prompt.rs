use std::path::{Path, PathBuf};

use crate::editor_buffer::{ParagraphAlignment, SearchOptions};

#[derive(Clone, Debug)]
pub enum PromptKind {
    Theme,
    CustomColor,
    WordTarget,
    ParagraphAlignment,
    SavePath,
    OpenPath,
    FindText,
    FindReplaceQuery,
    FindReplaceValue { query: String },
    GoToLine,
    WriteBlock,
    ReadFile,
    WorkspacePath,
    DocumentStatus,
    RenameBrowserEntry { source: PathBuf, is_dir: bool },
    CopyBrowserEntry { source: PathBuf, is_dir: bool },
    BackupDirectory,
}

#[derive(Clone, Debug)]
pub struct PromptState {
    pub kind: PromptKind,
    pub label: String,
    pub hint: String,
    pub value: String,
}

impl PromptState {
    pub fn theme() -> Self {
        Self {
            kind: PromptKind::Theme,
            label: "Theme".to_string(),
            hint: "Type a preset name, auto, or system".to_string(),
            value: String::new(),
        }
    }

    pub fn custom_color(initial: Option<&str>) -> Self {
        Self {
            kind: PromptKind::CustomColor,
            label: "Accent".to_string(),
            hint: "Enter a color tag like #7fb3ff".to_string(),
            value: initial.unwrap_or_default().to_string(),
        }
    }

    pub fn word_target(initial: Option<usize>) -> Self {
        Self {
            kind: PromptKind::WordTarget,
            label: "Word Target".to_string(),
            hint: "Enter a number, or 0/off/none to clear".to_string(),
            value: initial.map(|value| value.to_string()).unwrap_or_default(),
        }
    }

    pub fn document_status(initial: &str) -> Self {
        Self {
            kind: PromptKind::DocumentStatus,
            label: "Status".to_string(),
            hint: "Draft, In Progress, Done, or your own label".to_string(),
            value: initial.to_string(),
        }
    }

    pub fn paragraph_alignment(initial: ParagraphAlignment) -> Self {
        Self {
            kind: PromptKind::ParagraphAlignment,
            label: "Alignment".to_string(),
            hint: "Enter left, center, right, or justify".to_string(),
            value: initial.display_name().to_string(),
        }
    }

    pub fn save_path(initial: Option<&str>) -> Self {
        Self {
            kind: PromptKind::SavePath,
            label: "Save Path".to_string(),
            hint: "Enter a path like ./draft.ord, ./draft.txt, or ./draft.md".to_string(),
            value: initial.unwrap_or_default().to_string(),
        }
    }

    pub fn open_path(initial: Option<&str>) -> Self {
        Self {
            kind: PromptKind::OpenPath,
            label: "Open".to_string(),
            hint: "Enter a file path like ./draft.ord, ./draft.txt, or ./draft.md".to_string(),
            value: initial.unwrap_or_default().to_string(),
        }
    }

    pub fn workspace_path(initial: Option<&str>) -> Self {
        Self {
            kind: PromptKind::WorkspacePath,
            label: "Workspace Folder".to_string(),
            hint: "Enter a folder to track, like ./writing or ~/Documents/novel".to_string(),
            value: initial.unwrap_or_default().to_string(),
        }
    }

    pub fn find(initial: Option<&str>) -> Self {
        Self {
            kind: PromptKind::FindText,
            label: "Find".to_string(),
            hint: search_hint(&PromptKind::FindText, SearchOptions::default()),
            value: initial.unwrap_or_default().to_string(),
        }
    }

    pub fn find_replace_query(initial: Option<&str>) -> Self {
        Self {
            kind: PromptKind::FindReplaceQuery,
            label: "Replace: Find".to_string(),
            hint: search_hint(&PromptKind::FindReplaceQuery, SearchOptions::default()),
            value: initial.unwrap_or_default().to_string(),
        }
    }

    pub fn find_replace_value(query: &str, initial: Option<&str>) -> Self {
        let kind = PromptKind::FindReplaceValue {
            query: query.to_string(),
        };
        Self {
            label: format!("Replace '{}' with", query),
            hint: search_hint(&kind, SearchOptions::default()),
            kind,
            value: initial.unwrap_or_default().to_string(),
        }
    }

    pub fn go_to_line() -> Self {
        Self {
            kind: PromptKind::GoToLine,
            label: "Go to Line".to_string(),
            hint: "Enter line number".to_string(),
            value: String::new(),
        }
    }

    pub fn write_block(initial: Option<&str>) -> Self {
        Self {
            kind: PromptKind::WriteBlock,
            label: "Write Block to".to_string(),
            hint: "Enter file path to write marked block".to_string(),
            value: initial.unwrap_or_default().to_string(),
        }
    }

    pub fn read_file(initial: Option<&str>) -> Self {
        Self {
            kind: PromptKind::ReadFile,
            label: "Read File into Cursor".to_string(),
            hint: "Enter file path to insert at cursor".to_string(),
            value: initial.unwrap_or_default().to_string(),
        }
    }

    pub fn rename_browser_entry(source: &Path, is_dir: bool) -> Self {
        Self {
            kind: PromptKind::RenameBrowserEntry {
                source: source.to_path_buf(),
                is_dir,
            },
            label: if is_dir {
                "Rename Folder".to_string()
            } else {
                "Rename File".to_string()
            },
            hint: "Enter a new name or destination path".to_string(),
            value: source
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default(),
        }
    }

    pub fn copy_browser_entry(source: &Path, is_dir: bool, initial: &str) -> Self {
        Self {
            kind: PromptKind::CopyBrowserEntry {
                source: source.to_path_buf(),
                is_dir,
            },
            label: if is_dir {
                "Copy Folder".to_string()
            } else {
                "Copy File".to_string()
            },
            hint: "Enter a new copy path".to_string(),
            value: initial.to_string(),
        }
    }

    pub fn backup_directory(current: &str) -> Self {
        Self {
            kind: PromptKind::BackupDirectory,
            label: "Backup Location".to_string(),
            hint: "Empty resets to the default backup folder.".to_string(),
            value: current.to_string(),
        }
    }

    pub fn push(&mut self, ch: char) {
        self.value.push(ch);
    }

    pub fn backspace(&mut self) {
        self.value.pop();
    }

    pub fn render_status(&self) -> String {
        format!("{} > {}  [{}]", self.label, self.value, self.hint)
    }
}

/// Hint line for the find/replace prompts, naming the active flags and the
/// chords that flip them while the prompt is open.
pub fn search_hint(kind: &PromptKind, options: SearchOptions) -> String {
    let base = match kind {
        PromptKind::FindText => "Enter to search, ←/↑ previous, →/↓ next, Esc to cancel",
        PromptKind::FindReplaceQuery => "Enter text to replace, Enter to continue",
        PromptKind::FindReplaceValue { .. } => "Enter to replace & find next, Tab to replace all",
        _ => return String::new(),
    };
    format!(
        "{base} [{}] (⌥R regex, ⌥W word, ⌥C case)",
        options.describe()
    )
}
