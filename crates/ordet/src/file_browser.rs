use std::fs;
use std::path::{Path, PathBuf};

use ordet_tui_framework::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileBrowserMode {
    Browse,
    Search,
    Path,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileBrowserEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

#[derive(Clone, Debug)]
pub struct FileBrowserState {
    pub current_dir: PathBuf,
    pub selected: usize,
    pub search: String,
    pub manual_path: String,
    pub mode: FileBrowserMode,
    pub message: String,
    all_entries: Vec<FileBrowserEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileBrowserAction {
    None,
    Open(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileBrowserLayout {
    pub modal: Rect,
    pub inner: Rect,
    pub list_area: Rect,
}

impl FileBrowserState {
    pub fn open(initial_dir: PathBuf) -> Self {
        let current_dir = resolve_initial_dir(initial_dir);
        let mut state = Self {
            manual_path: current_dir.display().to_string(),
            current_dir,
            selected: 0,
            search: String::new(),
            mode: FileBrowserMode::Browse,
            message: browser_mode_message(FileBrowserMode::Browse),
            all_entries: Vec::new(),
        };
        if let Err(error) = state.refresh() {
            state.message = error;
        }
        state
    }

    pub fn refresh(&mut self) -> Result<(), String> {
        let entries = read_entries(&self.current_dir)?;
        self.all_entries = entries;
        self.selected = self
            .selected
            .min(self.filtered_entries().len().saturating_sub(1));
        self.manual_path = self.current_dir.display().to_string();
        Ok(())
    }

    pub fn filtered_entries(&self) -> Vec<&FileBrowserEntry> {
        let needle = self.search.trim().to_ascii_lowercase();
        if needle.is_empty() {
            return self.all_entries.iter().collect();
        }

        self.all_entries
            .iter()
            .filter(|entry| entry.name.to_ascii_lowercase().contains(&needle))
            .collect()
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        self.selected = (self.selected + 1).min(self.filtered_entries().len().saturating_sub(1));
    }

    pub fn move_home(&mut self) {
        self.selected = 0;
    }

    pub fn move_end(&mut self) {
        self.selected = self.filtered_entries().len().saturating_sub(1);
    }

    pub fn page_up(&mut self, amount: usize) {
        self.selected = self.selected.saturating_sub(amount.max(1));
    }

    pub fn page_down(&mut self, amount: usize) {
        self.selected =
            (self.selected + amount.max(1)).min(self.filtered_entries().len().saturating_sub(1));
    }

    pub fn cycle_mode(&mut self) {
        self.mode = match self.mode {
            FileBrowserMode::Browse => FileBrowserMode::Search,
            FileBrowserMode::Search => FileBrowserMode::Path,
            FileBrowserMode::Path => FileBrowserMode::Browse,
        };
        self.message = browser_mode_message(self.mode);
    }

    pub fn push_char(&mut self, ch: char) {
        match self.mode {
            FileBrowserMode::Browse => {
                self.mode = FileBrowserMode::Search;
                self.search.push(ch);
            }
            FileBrowserMode::Search => self.search.push(ch),
            FileBrowserMode::Path => self.manual_path.push(ch),
        }
        self.selected = 0;
    }

    pub fn backspace(&mut self) -> Result<FileBrowserAction, String> {
        match self.mode {
            FileBrowserMode::Browse => self.go_parent(),
            FileBrowserMode::Search => {
                self.search.pop();
                self.selected = 0;
                Ok(FileBrowserAction::None)
            }
            FileBrowserMode::Path => {
                self.manual_path.pop();
                Ok(FileBrowserAction::None)
            }
        }
    }

    pub fn activate_selected(&mut self) -> Result<FileBrowserAction, String> {
        match self.mode {
            FileBrowserMode::Path => self.confirm_manual_path(),
            FileBrowserMode::Browse | FileBrowserMode::Search => {
                let Some(entry) = self.selected_entry() else {
                    return Ok(FileBrowserAction::None);
                };
                if entry.is_dir {
                    self.current_dir = entry.path;
                    self.search.clear();
                    self.selected = 0;
                    self.refresh()?;
                    // The path row already says where we are; don't repeat it
                    // in the message.
                    self.message = browser_mode_message(self.mode);
                    Ok(FileBrowserAction::None)
                } else {
                    Ok(FileBrowserAction::Open(entry.path))
                }
            }
        }
    }

    pub fn confirm_manual_path(&mut self) -> Result<FileBrowserAction, String> {
        let path = resolve_input_path(self.manual_path.trim(), &self.current_dir)?;
        if path.is_dir() {
            self.current_dir = path;
            self.search.clear();
            self.selected = 0;
            self.mode = FileBrowserMode::Browse;
            self.refresh()?;
            self.message = browser_mode_message(self.mode);
            Ok(FileBrowserAction::None)
        } else if path.exists() {
            Ok(FileBrowserAction::Open(path))
        } else {
            Err("That file does not exist.".to_string())
        }
    }

    pub fn go_parent(&mut self) -> Result<FileBrowserAction, String> {
        if let Some(parent) = self.current_dir.parent() {
            self.current_dir = parent.to_path_buf();
            self.search.clear();
            self.selected = 0;
            self.refresh()?;
            // No "Moved to …" report; the path row is the report.
            self.message = browser_mode_message(self.mode);
        }
        Ok(FileBrowserAction::None)
    }

    pub fn selected_entry(&self) -> Option<FileBrowserEntry> {
        self.filtered_entries()
            .get(self.selected)
            .map(|entry| (*entry).clone())
    }

    pub fn select_path(&mut self, path: &Path) {
        if let Some(index) = self
            .filtered_entries()
            .iter()
            .position(|entry| entry.path == path)
        {
            self.selected = index;
        }
    }
}

/// The browser is a fixed card: browsing between short and long folders must
/// not resize the box under the cursor. Small terminals clamp it, but the
/// normal case is always the same rectangle.
pub const BROWSER_WIDTH: u16 = 56;
pub const BROWSER_HEIGHT: u16 = 24;

pub fn browser_layout(area: Rect) -> FileBrowserLayout {
    let width = BROWSER_WIDTH.min(area.width);
    let height = BROWSER_HEIGHT.min(area.height);

    // Dialog corner: flush with the top-left of the body, under the menu
    // bar, not floating in the middle of the page.
    let modal = Rect::new(area.x, area.y, width, height);
    let inner = modal.inset(1, 1);
    FileBrowserLayout {
        modal,
        inner,
        list_area: inner,
    }
}

fn browser_mode_message(mode: FileBrowserMode) -> String {
    match mode {
        FileBrowserMode::Browse => {
            "Browse mode. Type to search. Enter opens. Backspace goes up. ⌃R renames, ⌃C copies, Tab switches mode."
                .to_string()
        }
        FileBrowserMode::Search => {
            "Search mode. Type to filter entries, Up/Down selects, Enter opens, ⌃R renames, ⌃C copies."
                .to_string()
        }
        FileBrowserMode::Path => {
            "Path mode. Type a full or relative path, then Enter. Tab switches mode.".to_string()
        }
    }
}

/// Whether `message` is just the stock mode hint rather than an error or
/// action report worth showing.
impl FileBrowserState {
    pub fn is_mode_hint(&self) -> bool {
        self.message == browser_mode_message(self.mode)
    }
}

fn read_entries(dir: &Path) -> Result<Vec<FileBrowserEntry>, String> {
    let mut entries = fs::read_dir(dir)
        .map_err(|error| format!("Failed to read {}: {error}", dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| {
            let path = entry.path();
            let is_dir = path.is_dir();
            let mut name = entry.file_name().to_string_lossy().to_string();
            if is_dir {
                name.push('/');
            }
            FileBrowserEntry { path, name, is_dir }
        })
        .collect::<Vec<_>>();

    entries.sort_by(|left, right| {
        (!left.is_dir, left.name.to_ascii_lowercase())
            .cmp(&(!right.is_dir, right.name.to_ascii_lowercase()))
    });
    Ok(entries)
}

fn resolve_initial_dir(path: PathBuf) -> PathBuf {
    let path = if path.as_os_str().is_empty() {
        fallback_current_dir()
    } else {
        path
    };

    if path.is_dir() {
        path
    } else {
        path.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(fallback_current_dir)
    }
}

fn fallback_current_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn resolve_input_path(input: &str, current_dir: &Path) -> Result<PathBuf, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Enter a file or folder path.".to_string());
    }

    let raw = expand_path(PathBuf::from(trimmed));
    Ok(if raw.is_absolute() {
        raw
    } else {
        current_dir.join(raw)
    })
}

fn expand_path(path: PathBuf) -> PathBuf {
    if let Ok(stripped) = path.strip_prefix("~") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    }
    path
}

#[cfg(test)]
mod tests {
    use super::{browser_layout, resolve_initial_dir, FileBrowserMode, FileBrowserState};
    use ordet_tui_framework::Rect;
    use std::path::PathBuf;

    #[test]
    fn typing_in_browse_mode_switches_to_search() {
        let mut browser = FileBrowserState::open(std::env::temp_dir());
        browser.push_char('n');
        assert_eq!(browser.mode, FileBrowserMode::Search);
        assert_eq!(browser.search, "n");
    }

    #[test]
    fn relative_file_paths_open_from_current_directory() {
        let expected = std::env::current_dir().expect("cwd");
        assert_eq!(resolve_initial_dir(PathBuf::from("Kayra.ord")), expected);
    }

    #[test]
    fn directory_changes_do_not_repeat_the_path_in_the_message() {
        let root = PathBuf::from("target/ordet-browser-navigation-test");
        let child = root.join("child");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&child).expect("seed dir");
        let mut browser = FileBrowserState::open(root.clone());

        browser.select_path(&child);
        browser.activate_selected().expect("enter child");
        assert_eq!(browser.current_dir, child);
        assert!(browser.is_mode_hint(), "message: {}", browser.message);
        assert!(!browser.message.contains("Entered"), "{}", browser.message);

        browser.go_parent().expect("go parent");
        assert_eq!(browser.current_dir, root);
        assert!(browser.is_mode_hint(), "message: {}", browser.message);
        assert!(!browser.message.contains("Moved to"), "{}", browser.message);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn browser_layout_is_fixed_at_the_top_left() {
        let area = Rect::new(3, 7, 120, 40);
        let layout = browser_layout(area);
        assert_eq!((layout.modal.x, layout.modal.y), (area.x, area.y));
        assert_eq!(layout.modal.width, super::BROWSER_WIDTH);
        assert_eq!(layout.modal.height, super::BROWSER_HEIGHT);

        // A small body clamps the card instead of spilling off-screen.
        let small = browser_layout(Rect::new(0, 0, 40, 20));
        assert_eq!((small.modal.width, small.modal.height), (40, 20));
    }
}
