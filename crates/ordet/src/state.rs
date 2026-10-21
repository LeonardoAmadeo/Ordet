use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use ordet_format::default_save_file_name;
use ordet_tui_framework::Rect;

use unicode_width::UnicodeWidthStr;

use crate::clipboard::Clipboard;
use crate::editor_buffer::{EditorBuffer, SearchOptions};
use crate::file_browser::FileBrowserState;
use crate::history::HistoryFile;
use crate::i18n::UiLanguage;
use crate::prompt::PromptState;
use crate::recovery::RecoveredDraft;
use crate::stats::WordStats;
use crate::theme::{ThemeContext, ThemePalette, ThemeSelection};
use crate::workspace::{WorkspaceFolder, WorkspaceScanCache};

/// Bottom status row: one line.
pub const STATUS_HEIGHT: u16 = 1;
/// How long a status message lives before falling back to the focus hint.
pub const STATUS_LIFETIME: Duration = Duration::from_secs(3);
pub const MAX_RECENT_FILES: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WelcomeOption {
    ResumeRecent,
    NewDraft,
    OpenFile,
    Help,
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WelcomeFocus {
    Menu,
    HelpContent,
}

pub struct AppState {
    pub drafts: Vec<DraftState>,
    pub active_draft: usize,
    pub recent_files: Vec<PathBuf>,
    pub panels: PanelsState,
    /// Open windows (splits) over the editor area.
    pub windows: WindowState,
    pub ui: UiState,
    /// Shared kill ring bridged to the system clipboard. One per app, not per
    /// draft, so copying in one page and pasting in another works.
    pub clipboard: Clipboard,
    /// Folders tracked in the workspace panel, in the order they were added.
    pub workspace_folders: Vec<WorkspaceFolder>,
    /// Cached directory scans behind a `RefCell` because `workspace_rows`
    /// only gets `&self`. Never serialized; rebuilt from the folders above.
    pub(crate) workspace_scans: RefCell<WorkspaceScanCache>,
    /// Snapshots of saved text per file. Loaded once at startup, appended on
    /// every save, persisted off the main loop.
    pub history: HistoryFile,
}

/// `.ord` document metadata carried alongside the text. The editor preserves
/// what it loaded so a save never silently resets status, title mode, or
/// label color.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentMetadata {
    pub status: String,
    pub auto_title: bool,
    pub label_color: String,
}

impl Default for DocumentMetadata {
    fn default() -> Self {
        Self {
            status: String::from("Draft"),
            auto_title: true,
            label_color: String::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrefixMode {
    CtrlK,
    /// After `Ctrl-O`: line alignment.
    CtrlO,
    /// After `Ctrl-K Q`: the quick jump. A digit jumps to a numbered
    /// bookmark, `P` jumps to the previous position.
    CtrlKQuick,
}

pub struct UiState {
    pub status: String,
    /// When the current status message expires and falls back to the focus
    /// hint. `None` means it stays (focus hints, live recording cues).
    pub status_expires_at: Option<Instant>,
    pub should_quit: bool,
    pub layout: RootLayout,
    pub quit_after_save: bool,
    pub theme: ThemeSelection,
    pub word_target: Option<usize>,
    pub prompt: Option<PromptState>,
    pub file_browser: Option<FileBrowserState>,
    pub local_hour: u8,
    pub system_is_dark: Option<bool>,
    /// Housekeeping ticks since launch. Only used to slow down the system
    /// theme probe; never serialized.
    pub tick_count: u64,
    pub pending_open: bool,
    pub welcome_selected: WelcomeOption,
    pub welcome_focus: WelcomeFocus,
    pub welcome_help_scroll: usize,
    pub language: UiLanguage,
    pub prefix: Option<PrefixMode>,
    pub insert_mode: bool,
    pub show_line_numbers: bool,
    pub autosave_enabled: bool,
    pub indent_width: usize,
    /// Ruler row above the text, one per window. Persisted; every launch
    /// starts on.
    pub show_ruler: bool,
    /// Top bar with the menu titles, and the centred page name while one
    /// window is open. Persisted; hiding it hands the row back to the text.
    pub show_menu_bar: bool,
    /// Live word/char counters in the status cluster. Off hides them.
    pub show_live_counts: bool,
    /// Fullscreen-editor zoom. Never serialized: every launch starts unzoomed.
    /// While zoomed the ruler is hidden; the header, text and status row stay.
    pub zoomed: bool,
    /// File backups: master switch, back-up-on-close, newest-N to keep, and
    /// a custom directory (empty means the default beside settings).
    pub backup_enabled: bool,
    pub backup_on_close: bool,
    pub backup_keep: usize,
    pub backup_dir: String,
    pub last_search: Option<String>,
    pub search_options: SearchOptions,
    pub mouse_down_cursor: Option<usize>,
    pub quit_confirmation: Option<QuitConfirmation>,
    /// Ctrl+R arms instead of firing: the target waits here until a second
    /// Ctrl+R confirms it. Any other action disarms. Never serialized.
    pub pending_trash: Option<PathBuf>,
    /// Same for closing a dirty unsaved page: the draft index waits for a
    /// second Delete/Ctrl+R. Never serialized.
    pub pending_close_draft: Option<usize>,
    /// About sheet opened from Help. It owns the keyboard and closes on
    /// Esc/Enter/Space, so no key leaks into the editor behind it.
    pub about_open: bool,
    /// Raw terminal sequences (OSC 52 clipboard writes) queued for the next
    /// frame. Components cannot touch the terminal, so they queue here and
    /// `draw_frame` flushes through `Terminal::overlay_sequence`.
    pub pending_overlays: Vec<String>,
    /// Open top menu (index into `MENU_BAR`), if any. Never serialized.
    pub menu_open: Option<usize>,
    /// Highlighted item of the open menu. Never serialized.
    pub menu_selected: usize,
    /// First visible item of a dropdown that is taller than the terminal.
    /// Never serialized.
    pub menu_scroll: usize,
    /// A menu item press is waiting for its release to run (press -
    /// drag - release). Never serialized.
    pub menu_armed: bool,
}

/// Set while Ordet is waiting for the user to confirm discarding unsaved work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuitConfirmation {
    pub dirty_titles: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct RootLayout {
    pub header: Rect,
    pub editor: Rect,
    pub status: Rect,
    /// One rect per visible window, in `visible_window_drafts` order. A
    /// single window reports just the editor rect, so the unsplit layout is
    /// exactly the old one.
    pub window_rects: Vec<Rect>,
}

/// Which overlay panel is open. One at a time; Esc closes the topmost thing
/// (settings picker first, then prompt, then the panel).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelKind {
    Notes,
    Stats,
    Settings,
    Revisions,
    Help,
    Pages,
    Workspace,
}

impl PanelKind {
    pub fn title(self) -> &'static str {
        match self {
            PanelKind::Notes => "Notes",
            PanelKind::Stats => "Word Stats",
            PanelKind::Settings => "Settings",
            PanelKind::Revisions => "Revisions",
            PanelKind::Help => "Help",
            PanelKind::Pages => "Pages",
            PanelKind::Workspace => "Workspace",
        }
    }
}

/// The five Help sections: one short page at a time instead of one long
/// scroll of every shortcut the editor knows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HelpTab {
    #[default]
    Movement,
    Writing,
    Workspace,
    Tools,
    Blocks,
}

impl HelpTab {
    pub const ALL: [Self; 5] = [
        Self::Movement,
        Self::Writing,
        Self::Workspace,
        Self::Tools,
        Self::Blocks,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Movement => "Movement",
            Self::Writing => "Writing",
            Self::Workspace => "Workspace",
            Self::Tools => "Tools",
            Self::Blocks => "Blocks",
        }
    }

    pub fn next(self) -> Self {
        let index = Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }

    pub fn previous(self) -> Self {
        let index = Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0);
        Self::ALL[(index + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

/// Open-panel state: which panel (if any) covers the body plus its per-panel
/// highlights and scrolls. `panel: None` means the bare editor.
#[derive(Default)]
pub struct PanelsState {
    pub panel: Option<PanelKind>,
    pub scroll: usize,
    pub help_tab: HelpTab,
    pub settings_selected: usize,
    /// Option picker open over the settings panel, if any. Lists every
    /// choice at once so multi-option rows (theme, language, …) never need
    /// blind one-by-one cycling.
    pub settings_picker: Option<SettingsPickerState>,
    pub revision_selected: usize,
    /// Highlight into the Pages panel's visible-draft list.
    pub pages_selected: usize,
    /// Highlight into the Workspace panel's row list.
    pub workspace_selected: usize,
}

impl PanelsState {
    pub fn is_open(&self) -> bool {
        self.panel.is_some()
    }

    pub fn is(&self, kind: PanelKind) -> bool {
        self.panel == Some(kind)
    }

    /// Show one panel, resetting scroll and the Help tab so every open
    /// starts at the top of its first page.
    pub fn open(&mut self, kind: PanelKind) {
        self.panel = Some(kind);
        self.scroll = 0;
        if kind == PanelKind::Help {
            self.help_tab = HelpTab::default();
        }
    }

    /// Hide the open panel. Panel highlights survive; scrolls reset.
    pub fn close(&mut self) {
        self.panel = None;
        self.scroll = 0;
    }

    pub fn toggle(&mut self, kind: PanelKind) {
        if self.is(kind) {
            self.close();
        } else {
            self.open(kind);
        }
    }
}

/// A small window desktop: four tiles fill a terminal and a keyboard cycle
/// without hiding text behind a stack.
pub const MAX_WINDOWS: usize = 4;

/// Open windows: the drafts shown side by side in the editor area plus which
/// one owns the keyboard. Empty `panes` means the plain single window on the
/// active draft, so the unsplit editor never pays for window bookkeeping.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowState {
    /// One entry per visible window, in layout order (left/top first).
    pub panes: Vec<DraftId>,
    /// Index into `panes` that owns the keyboard.
    pub active: usize,
}

/// Tile the editor area for `count` open windows: one full-height pane, two
/// columns, three as a full-height left pane beside a stacked pair, four
/// quadrants. One row/column between tiles is left as a gutter for the plain
/// separator the renderer draws. Areas too small to split fall back to the
/// single full pane.
pub fn split_rects(area: Rect, count: usize) -> Vec<Rect> {
    let count = count.clamp(1, MAX_WINDOWS);
    if count == 1 || area.width < 7 || area.height < 5 {
        return vec![area];
    }
    let half_w = area.width / 2;
    let right_x = area.x + half_w + 1;
    let right_w = area.width.saturating_sub(half_w + 1);
    let top_h = area.height / 2;
    let bottom_y = area.y + top_h + 1;
    let bottom_h = area.height.saturating_sub(top_h + 1);
    match count {
        2 => vec![
            Rect::new(area.x, area.y, half_w, area.height),
            Rect::new(right_x, area.y, right_w, area.height),
        ],
        3 => vec![
            Rect::new(area.x, area.y, half_w, area.height),
            Rect::new(right_x, area.y, right_w, top_h),
            Rect::new(right_x, bottom_y, right_w, bottom_h),
        ],
        _ => vec![
            Rect::new(area.x, area.y, half_w, top_h),
            Rect::new(right_x, area.y, right_w, top_h),
            Rect::new(area.x, bottom_y, half_w, bottom_h),
            Rect::new(right_x, bottom_y, right_w, bottom_h),
        ],
    }
}

/// A row of the Workspace panel: a tracked folder header or one of its files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceRowKind {
    Folder(PathBuf),
    File(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceRow {
    pub kind: WorkspaceRowKind,
    pub label: String,
    /// Unsaved count beside a folder header (`n/m` when non-zero).
    pub unsaved: usize,
    pub file_count: usize,
}

/// A row of the Pages panel: one listed draft.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageRow {
    /// Index into `AppState::drafts`.
    pub index: usize,
    /// 1-based page number, matching the header's `Pg n/m`.
    pub number: usize,
    pub label: String,
    pub active: bool,
    pub unsaved: bool,
}

/// Which settings row an option picker is choosing for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsPickerKind {
    Theme,
    Language,
    IndentWidth,
}

/// Centered option list for one settings row: all choices visible,
/// arrows/Home/End move, Enter confirms, Esc closes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsPickerState {
    pub kind: SettingsPickerKind,
    pub title: String,
    pub options: Vec<String>,
    /// Highlighted row. Moves with the keyboard and the mouse.
    pub selected: usize,
    /// First visible row. Follows the highlight on short screens.
    pub scroll: usize,
    /// Row holding the current value when opened. Stays put while the
    /// highlight moves, so the ● marker never lies.
    pub active: usize,
    /// Theme in force when a theme picker opened. Moving the highlight
    /// previews live; Esc restores this. Only meaningful for themes.
    pub preview_from: Option<ThemeSelection>,
}

impl SettingsPickerState {
    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
    }

    pub fn move_down(&mut self) {
        if !self.options.is_empty() && self.selected + 1 < self.options.len() {
            self.selected += 1;
        }
        self.follow();
    }

    pub fn move_home(&mut self) {
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn move_end(&mut self) {
        if !self.options.is_empty() {
            self.selected = self.options.len() - 1;
        }
        self.follow();
    }

    /// Keeps the highlight inside a `height`-tall window.
    pub fn follow(&mut self) {
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
    }

    pub fn clamp_to(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        if self.selected >= self.scroll + height {
            self.scroll = self.selected.saturating_sub(height - 1);
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
    }
}

/// Cell rects of the menu bar titles, left to right. The row carries a
/// symmetric 1-cell margin: the first title starts at `header.x + 1` and
/// the last ends one cell before the frame, so neither edge touches it.
/// Shared by the renderer and the mouse code so clicks hit the painted
/// titles. Titles that do not fit are dropped.
pub fn menu_title_cells(header: Rect) -> Vec<Rect> {
    let mut cells = Vec::new();
    let mut x = header.x.saturating_add(1);
    for menu in crate::commands::MENU_BAR {
        let width = menu.title.chars().count() as u16;
        if width == 0 || header.height == 0 {
            break;
        }
        if x.saturating_add(width) > header.right().saturating_sub(1) {
            break;
        }
        cells.push(Rect::new(x, header.y, width, 1));
        x = x.saturating_add(width).saturating_add(2);
    }
    cells
}

/// Width of an open menu's dropdown content (the list area inside the
/// border): longest `label + 2 spaces + hint` row plus 2 trailing breathing
/// cells plus 2 horizontal padding cells (1 left + 1 right, always menu bg
/// so the selection never touches the border).
///
/// Uses display width (not char count) so the renderer's padding math and
/// this agree exactly. The outer rect adds 2 more cells for the border.
pub fn menu_drop_width(menu_index: usize) -> usize {
    use unicode_width::UnicodeWidthStr;
    crate::commands::MENU_BAR
        .get(menu_index)
        .map(|menu| {
            menu.items
                .iter()
                .map(|item| {
                    UnicodeWidthStr::width(item.label) + UnicodeWidthStr::width(item.hint) + 6
                })
                .max()
                .unwrap_or(10)
        })
        .unwrap_or(0)
}

/// Outer rect of the open menu's dropdown, border included.
///
/// Alignment contract (see `render_menu_dropdown`):
/// - `outer.x` == the menu title's `x` (left border sits directly under the
///   title's first cell, e.g. `Edit`'s `E`), clamped only when it would
///   overflow the right edge. With the top bar hidden there are no titles
///   to hang from, so the dropdown anchors at the header rect's left edge.
/// - `outer.y` == `header.bottom()` (top border touches the header row, no
///   1-row editor gap in between; with the bar hidden that is the
///   terminal's first row).
/// - `body_bottom` is the first row it may not cover (the status bar top).
/// - `item_count` is the number of rows the menu currently offers, so a
///   context-sensitive menu does not leave blank rows.
///
/// The bordered opaque box is what separates the menu from the editor text
/// behind it. Callers that need rows must go through `menu_drop_inner_rect`,
/// which is the list area inside the border, the renderer, hover, and
/// click code all share it.
pub fn menu_drop_rect(
    header: Rect,
    body_bottom: u16,
    menu_index: usize,
    item_count: usize,
) -> Rect {
    let cells = menu_title_cells(header);
    let mut x = cells.get(menu_index).map(|cell| cell.x).unwrap_or(header.x);
    let max_width = header.width.saturating_sub(2).max(10);
    let content_width = menu_drop_width(menu_index) as u16;
    // +2 for the left/right border.
    let width = content_width.saturating_add(2).min(max_width);
    if x.saturating_add(width) > header.right() {
        x = header.right().saturating_sub(width);
    }
    let available = body_bottom.saturating_sub(header.bottom());
    // +2 for the top/bottom border.
    let height = (item_count as u16).saturating_add(2).min(available);
    Rect::new(x, header.bottom(), width, height)
}

/// List area inside the dropdown border: the rows the renderer paints and
/// the mouse maps to. Empty when the outer rect is too small to hold a
/// border plus at least one row. Mirrors `components::render_menu_dropdown`.
pub fn menu_drop_inner_rect(
    header: Rect,
    body_bottom: u16,
    menu_index: usize,
    item_count: usize,
) -> Rect {
    let outer = menu_drop_rect(header, body_bottom, menu_index, item_count);
    if outer.width < 3 || outer.height < 3 {
        return Rect::new(0, 0, 0, 0);
    }
    outer.inset(1, 1)
}

/// Centered rect with the same math as the modal renderer, so hover and
/// clicks map to the painted rows. Mirrors `components::centered_rect`.
pub fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x.saturating_add(area.width.saturating_sub(width) / 2),
        area.y
            .saturating_add(area.height.saturating_sub(height) / 2),
        width,
        height,
    )
}

/// Outer rect of the open panel (border included), or an empty rect when no
/// panel is open. The renderer and the mouse code share this so a click
/// lands on the row the user sees.
///
/// The size and the corner both live beside the panel renderer
/// (`components::panel_rect`): only that code knows how many rows help,
/// settings or stats actually paint, and panels are cards
/// that hug their content, hung off the top-right of the page, under the
/// menu bar, rather than sheets that take a share of the screen.
pub fn panel_rect(model: &AppState) -> Rect {
    crate::components::panel_rect(model)
}

/// The panel's inside area, below the border and above the bottom border.
pub fn panel_inner_rect(model: &AppState) -> Rect {
    let outer = panel_rect(model);
    if outer.width < 2 || outer.height < 2 {
        return Rect::new(0, 0, 0, 0);
    }
    outer.inset(1, 1)
}

/// The panel's content area: everything inside the border. The name rides in
/// the top border, so no inner header row is reserved.
pub fn panel_content_rect(model: &AppState) -> Rect {
    panel_inner_rect(model)
}

/// Modal rect for the settings option picker. Tall enough to show every
/// choice at once (that is the point), clamped to the screen; short
/// terminals fall back to scrolling. Mirrors the renderer below.
pub fn settings_picker_modal_rect(full: Rect, option_count: usize) -> Rect {
    if full.width < 16 || full.height < 6 {
        return Rect::new(0, 0, 0, 0);
    }
    let modal_width = 46.min(full.width.saturating_sub(4)).max(24);
    // Title + hint + blank header rows, then one row per choice.
    let modal_height = ((option_count + 5) as u16)
        .min(full.height.saturating_sub(2))
        .max(6);
    let modal_rect = centered_rect(full, modal_width, modal_height);
    if modal_rect.width < 3 || modal_rect.height < 3 {
        return Rect::new(0, 0, 0, 0);
    }
    modal_rect
}

/// List rows area inside the settings picker modal: options start after
/// the same 3 header rows the renderer paints.
pub fn settings_picker_list_rect(full: Rect, option_count: usize) -> Rect {
    let modal_rect = settings_picker_modal_rect(full, option_count);
    if modal_rect.width < 3 || modal_rect.height < 3 {
        return Rect::new(0, 0, 0, 0);
    }
    let inner = modal_rect.inset(1, 1);
    Rect::new(
        inner.x,
        inner.y + 3,
        inner.width,
        inner.height.saturating_sub(3),
    )
}

pub(crate) fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();

    for raw_line in text.lines() {
        if raw_line.trim().is_empty() {
            lines.push(String::new());
            continue;
        }

        let mut current = String::new();
        for word in raw_line.split_whitespace() {
            let candidate = if current.is_empty() {
                word.to_string()
            } else {
                format!("{current} {word}")
            };

            if UnicodeWidthStr::width(candidate.as_str()) <= width {
                current = candidate;
                continue;
            }

            if !current.is_empty() {
                lines.push(current);
            }

            if UnicodeWidthStr::width(word) <= width {
                current = word.to_string();
            } else {
                let mut segment = String::new();
                for ch in word.chars() {
                    let candidate = format!("{segment}{ch}");
                    if !segment.is_empty() && UnicodeWidthStr::width(candidate.as_str()) > width {
                        lines.push(segment);
                        segment = String::new();
                    }
                    segment.push(ch);
                }
                current = segment;
            }
        }

        if !current.is_empty() {
            lines.push(current);
        }
    }

    if lines.is_empty() {
        lines.push(String::new());
    }

    lines
}

pub struct EditorPaneState {
    pub buffer: EditorBuffer,
    pub file_path: Option<PathBuf>,
    pub scroll: usize,
    pub revision: u64,
    pub last_saved_revision: u64,
    pub pending_save: bool,
    /// `.ord` document metadata. Loaded from disk when present, preserved on
    /// save, editable through the status prompt. Plain-text files start with
    /// these defaults, which is what a first save as `.ord` writes.
    pub doc_status: String,
    pub doc_auto_title: bool,
    pub doc_label_color: String,
    /// Numbered bookmarks (`Ctrl-K 0` to `Ctrl-K 9`): char offsets to jump
    /// back to. Ten fingers in the manuscript. Never serialized.
    pub bookmarks: [Option<usize>; 10],
    /// Most recently dropped bookmark: what a bare jump returns to, so no
    /// slot number has to be memorized. Never serialized.
    pub last_bookmark: Option<usize>,
    /// Cursor position when the wheel/trackpad last scrolled freely. While
    /// it matches the cursor, `ensure_cursor_visible` only clamps the view
    /// into range instead of yanking it back, any cursor move clears it.
    pub free_scroll_at: Option<usize>,
}

#[derive(Default)]
pub struct NotesPaneState {
    pub buffer: EditorBuffer,
    pub scroll: usize,
    pub revision: u64,
    /// Same numbered bookmarks as the editor, for the notes pane.
    pub bookmarks: [Option<usize>; 10],
    /// Most recently dropped bookmark here, too.
    pub last_bookmark: Option<usize>,
    /// Same free-scroll tracking as the editor pane.
    pub free_scroll_at: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WordTargetProgress {
    pub target: usize,
    pub current: usize,
    pub remaining: usize,
    pub overflow: usize,
    pub ratio: f32,
    pub percent: usize,
    pub met: bool,
}

/// Stable identity for a draft. Row indices shift when pages are reordered
/// or closed, so background work (saves, reads) captures this instead and
/// resolves the row again when it lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DraftId(pub u64);

pub struct DraftState {
    pub id: DraftId,
    pub editor: EditorPaneState,
    pub notes: NotesPaneState,
    /// Whether this draft is a listed page. The startup scratch and empty
    /// throwaway drafts stay unlisted until they hold something.
    pub listed: bool,
}

impl AppState {
    pub fn new(
        drafts: Vec<(EditorBuffer, Option<PathBuf>)>,
        recent_files: Vec<PathBuf>,
        theme: ThemeSelection,
        word_target: Option<usize>,
        local_hour: u8,
        system_is_dark: Option<bool>,
    ) -> Self {
        let recent_files = normalize_recent_files(recent_files);
        let initial_welcome_selection = if recent_files.is_empty() {
            WelcomeOption::NewDraft
        } else {
            WelcomeOption::ResumeRecent
        };
        let drafts = if drafts.is_empty() {
            vec![DraftState::auto_scratch()]
        } else {
            drafts
                .into_iter()
                .map(|(buffer, file_path)| DraftState::new(buffer, file_path))
                .collect()
        };

        Self {
            drafts,
            active_draft: 0,
            recent_files,
            panels: PanelsState::default(),
            windows: WindowState::default(),
            clipboard: Clipboard::new(),
            workspace_folders: Vec::new(),
            workspace_scans: RefCell::new(WorkspaceScanCache::new()),
            history: HistoryFile::default(),
            ui: UiState {
                // The bar stays quiet at rest: the status cluster on the right
                // carries mode, page and position, and the left only lights up
                // for prompts, panel help and confirmations.
                status: String::new(),
                status_expires_at: None,
                should_quit: false,
                layout: RootLayout::default(),
                quit_after_save: false,
                theme,
                word_target,
                prompt: None,
                file_browser: None,
                local_hour,
                system_is_dark,
                tick_count: 0,
                pending_open: false,
                welcome_selected: initial_welcome_selection,
                welcome_focus: WelcomeFocus::Menu,
                welcome_help_scroll: 0,
                language: UiLanguage::default(),
                prefix: None,
                insert_mode: true,
                show_line_numbers: false,
                autosave_enabled: true,
                indent_width: 4,
                show_ruler: true,
                show_menu_bar: true,
                show_live_counts: true,
                zoomed: false,
                backup_enabled: true,
                backup_on_close: true,
                backup_keep: 5,
                backup_dir: String::new(),
                last_search: None,
                search_options: SearchOptions::default(),
                mouse_down_cursor: None,
                quit_confirmation: None,
                pending_overlays: Vec::new(),
                pending_trash: None,
                pending_close_draft: None,
                about_open: false,
                menu_open: None,
                menu_selected: 0,
                menu_scroll: 0,
                menu_armed: false,
            },
        }
    }

    pub fn update_layout(&mut self, area: Rect) {
        // The top bar is one row when shown and no rows when hidden: the
        // ruler and text then start on the terminal's first line. The rect
        // keeps its origin either way, so dropdown menus and the welcome
        // screen still anchor at the top.
        let header_height = if self.ui.show_menu_bar { 1 } else { 0 };
        let header = Rect::new(area.x, area.y, area.width, header_height.min(area.height));
        // One status row, always: message on the left, state cluster on the
        // right. Zoom hides the ruler, never the bar.
        let status_height = STATUS_HEIGHT.min(area.height.saturating_sub(1));
        let status = Rect::new(
            area.x,
            area.bottom().saturating_sub(status_height),
            area.width,
            status_height,
        );
        let body_height = area
            .height
            .saturating_sub(header.height)
            .saturating_sub(status.height);
        let body = Rect::new(
            area.x,
            area.y.saturating_add(header.height),
            area.width,
            body_height,
        );

        // The editor is the whole body: panels are overlays drawn on top of
        // it, never reserved columns, so the text width never changes when
        // one opens or closes. Windows tile the same area.
        let window_count = if self.ui.zoomed {
            1
        } else {
            self.window_count()
        };
        let window_rects = split_rects(body, window_count);
        self.ui.layout = RootLayout {
            header,
            editor: body,
            status,
            window_rects,
        };
    }

    pub fn theme_context(&self) -> ThemeContext {
        ThemeContext {
            hour: self.ui.local_hour,
            system_is_dark: self.ui.system_is_dark,
        }
    }

    pub fn theme_palette(&self) -> ThemePalette {
        self.ui.theme.palette(self.theme_context())
    }

    pub fn theme_name(&self) -> String {
        self.ui.theme.display_name(self.theme_context())
    }

    pub fn word_target_progress(&self) -> Option<WordTargetProgress> {
        let target = self.ui.word_target?;
        if target == 0 {
            return None;
        }

        let current = self.editor().word_stats().word_count;
        let met = current >= target;
        let remaining = target.saturating_sub(current);
        let overflow = current.saturating_sub(target);
        let ratio = if target == 0 {
            0.0
        } else {
            current as f32 / target as f32
        };
        let percent = if target == 0 {
            0
        } else {
            ((ratio * 100.0).round() as usize).min(999)
        };

        Some(WordTargetProgress {
            target,
            current,
            remaining,
            overflow,
            ratio,
            percent,
            met,
        })
    }

    /// Important transient note: warnings, failures, and modal hints. Routine
    /// success feedback uses `set_status_quiet` instead.
    pub fn set_status(&mut self, status: impl Into<String>) {
        self.ui.status = status.into();
        self.ui.status_expires_at = Some(Instant::now() + STATUS_LIFETIME);
    }

    /// Routine action feedback: the bottom-left is reserved for prompts,
    /// confirmations, warnings, and failures, so ordinary success messages
    /// stay silent. A stale transient note is cleared; persistent hints and
    /// confirmations survive.
    pub fn set_status_quiet(&mut self, _status: impl Into<String>) {
        if self.ui.status_expires_at.is_some() {
            self.ui.status.clear();
            self.ui.status_expires_at = None;
        }
    }

    /// Status that stays until replaced: focus hints, live recording cues,
    /// armed-trash prompts. Important transient notes expire via `set_status`;
    /// routine ones use `set_status_quiet`.
    pub fn set_status_persistent(&mut self, status: impl Into<String>) {
        self.ui.status = status.into();
        self.ui.status_expires_at = None;
    }

    pub fn show_welcome_screen(&self) -> bool {
        self.has_only_auto_scratch()
    }

    pub fn welcome_options(&self) -> Vec<WelcomeOption> {
        let mut options = Vec::new();
        if self.primary_recent_path().is_some() {
            options.push(WelcomeOption::ResumeRecent);
        }
        options.push(WelcomeOption::NewDraft);
        options.push(WelcomeOption::OpenFile);
        options.push(WelcomeOption::Help);
        options.push(WelcomeOption::Exit);
        options
    }

    pub fn primary_recent_path(&self) -> Option<PathBuf> {
        self.recent_candidates().into_iter().next()
    }

    pub fn welcome_recent_paths(&self) -> Vec<PathBuf> {
        self.recent_candidates()
    }

    pub fn body_area(&self) -> Rect {
        if self.show_welcome_screen() {
            Rect::new(
                self.ui.layout.header.x,
                self.ui.layout.header.y,
                self.ui.layout.header.width,
                self.ui
                    .layout
                    .status
                    .y
                    .saturating_sub(self.ui.layout.header.y),
            )
        } else {
            Rect::new(
                self.ui.layout.header.x,
                self.ui.layout.header.bottom(),
                self.ui.layout.header.width,
                self.ui
                    .layout
                    .status
                    .y
                    .saturating_sub(self.ui.layout.header.bottom()),
            )
        }
    }

    pub fn cycle_welcome_selection(&mut self, backwards: bool) {
        let options = self.welcome_options();
        if options.is_empty() {
            return;
        }

        let current = options
            .iter()
            .position(|option| *option == self.ui.welcome_selected)
            .unwrap_or(0);
        let next = if backwards {
            current
                .checked_sub(1)
                .unwrap_or(options.len().saturating_sub(1))
        } else {
            (current + 1) % options.len()
        };
        self.set_welcome_selection(options[next]);
    }

    pub fn set_welcome_selection(&mut self, option: WelcomeOption) {
        self.ui.welcome_selected = option;
        if option != WelcomeOption::Help {
            self.ui.welcome_focus = WelcomeFocus::Menu;
        }
    }

    pub fn ensure_welcome_selection(&mut self) {
        let options = self.welcome_options();
        if options.is_empty() {
            self.set_welcome_selection(WelcomeOption::NewDraft);
            return;
        }

        if !options.contains(&self.ui.welcome_selected) {
            self.set_welcome_selection(options[0]);
        }
    }

    pub fn welcome_help_active(&self) -> bool {
        self.ui.welcome_selected == WelcomeOption::Help
            && self.ui.welcome_focus == WelcomeFocus::HelpContent
    }

    /// Whether the welcome screen shows its right-hand side panel. Merely
    /// highlighting a menu entry never shows it: Open file reveals the
    /// browser rail only after Enter opens it, and Help reveals the shortcut
    /// rail only after H, Enter, or Right moves focus into it.
    pub fn welcome_side_panel_visible(&self) -> bool {
        if self.ui.file_browser.is_some() {
            return true;
        }
        self.welcome_help_active()
    }

    pub fn focus_welcome_help(&mut self) {
        if self.ui.welcome_selected == WelcomeOption::Help {
            self.ui.welcome_focus = WelcomeFocus::HelpContent;
        }
    }

    pub fn blur_welcome_help(&mut self) {
        self.ui.welcome_focus = WelcomeFocus::Menu;
    }

    pub fn scroll_welcome_help(&mut self, forward: bool, amount: usize, max_scroll: usize) {
        let amount = amount.max(1);
        if forward {
            self.ui.welcome_help_scroll = self
                .ui
                .welcome_help_scroll
                .saturating_add(amount)
                .min(max_scroll);
        } else {
            self.ui.welcome_help_scroll = self.ui.welcome_help_scroll.saturating_sub(amount);
        }
    }

    pub fn reset_welcome_help_scroll(&mut self) {
        self.ui.welcome_help_scroll = 0;
    }

    pub fn clamp_welcome_help_scroll(&mut self, max_scroll: usize) {
        self.ui.welcome_help_scroll = self.ui.welcome_help_scroll.min(max_scroll);
    }

    pub fn set_welcome_help_scroll(&mut self, scroll: usize) {
        self.ui.welcome_help_scroll = scroll;
    }

    pub fn editor(&self) -> &EditorPaneState {
        &self.drafts[self.active_draft].editor
    }

    pub fn editor_mut(&mut self) -> &mut EditorPaneState {
        &mut self.drafts[self.active_draft].editor
    }

    pub fn notes(&self) -> &NotesPaneState {
        &self.drafts[self.active_draft].notes
    }

    pub fn notes_mut(&mut self) -> &mut NotesPaneState {
        &mut self.drafts[self.active_draft].notes
    }

    /// 0-based index of the open draft inside the visible Pages list. Zero
    /// when the open draft is a hidden scratch (no Pages row yet).
    pub fn active_visible_draft_index(&self) -> usize {
        self.visible_drafts()
            .iter()
            .position(|index| *index == self.active_draft)
            .unwrap_or(0)
    }

    pub fn switch_to_draft(&mut self, index: usize) {
        if index < self.drafts.len() {
            self.active_draft = index;
            self.sync_active_window();
        }
    }

    pub fn add_draft(&mut self, buffer: EditorBuffer, file_path: Option<PathBuf>) -> usize {
        self.drafts.push(DraftState::new(
            buffer,
            file_path.map(normalize_document_path),
        ));
        let index = self.drafts.len().saturating_sub(1);
        self.switch_to_draft(index);
        index
    }

    pub fn replace_active_draft(&mut self, buffer: EditorBuffer, file_path: Option<PathBuf>) {
        let draft = DraftState::new(buffer, file_path.map(normalize_document_path));
        if self.drafts.is_empty() {
            self.drafts.push(draft);
            self.active_draft = 0;
        } else {
            self.drafts[self.active_draft] = draft;
            // A new identity means a new pane entry: the replaced page is
            // gone and its window now shows the fresh draft.
            self.sync_active_window();
        }
    }

    pub fn has_only_auto_scratch(&self) -> bool {
        self.drafts.len() == 1 && self.drafts[0].is_auto_scratch()
    }

    pub fn visible_draft_count(&self) -> usize {
        self.visible_drafts().len()
    }

    /// Indices into `drafts` for every listed page, in list order. The hidden
    /// scratch draft leaves a gap that row indices must not see.
    pub fn visible_drafts(&self) -> Vec<usize> {
        self.drafts
            .iter()
            .enumerate()
            .filter(|(_, draft)| draft.is_listed())
            .map(|(index, _)| index)
            .collect()
    }

    /// 1-based page number of the open draft among the visible pages.
    /// `None` while the open draft is a hidden scratch (no pages yet).
    pub fn active_page_number(&self) -> Option<usize> {
        let mut page = 0;
        for (index, draft) in self.drafts.iter().enumerate() {
            if !draft.is_listed() {
                continue;
            }
            page += 1;
            if index == self.active_draft {
                return Some(page);
            }
        }
        None
    }

    /// Index of the draft with this id (`None` once it has been closed).
    /// Row indices move when pages are reordered; ids never do.
    pub fn draft_index_by_id(&self, id: DraftId) -> Option<usize> {
        self.drafts.iter().position(|draft| draft.id == id)
    }

    /// Id of the open draft. Drafts are never empty while the app runs, so
    /// this mirrors `editor()`'s invariant.
    pub fn active_draft_id(&self) -> DraftId {
        self.drafts[self.active_draft].id
    }

    /// Windows currently open. The plain editor counts as one window.
    pub fn window_count(&self) -> usize {
        self.windows.panes.len().max(1)
    }

    /// True when the desktop is actually tiled: split panes each paint a
    /// title row above their ruler. Zoom and a too-small terminal both fall
    /// back to one rect, so they keep the single header title instead.
    pub fn window_titles_visible(&self) -> bool {
        self.ui.layout.window_rects.len() > 1
    }

    /// Draft index shown in each open window, in layout order. The implicit
    /// single window just reports the active draft.
    pub fn window_drafts(&self) -> Vec<usize> {
        if self.windows.panes.is_empty() || self.drafts.is_empty() {
            return vec![self.active_draft.min(self.drafts.len().saturating_sub(1))];
        }
        self.windows
            .panes
            .iter()
            .map(|id| self.draft_index_by_id(*id).unwrap_or(self.active_draft))
            .collect()
    }

    /// Drafts the editor paints right now: zoom collapses the desktop to the
    /// window that owns the keyboard.
    pub fn visible_window_drafts(&self) -> Vec<usize> {
        if self.ui.zoomed {
            return vec![self.active_window_draft()];
        }
        self.window_drafts()
    }

    /// Draft index shown in the window that owns the keyboard.
    pub fn active_window_draft(&self) -> usize {
        if self.windows.panes.is_empty() {
            return self.active_draft;
        }
        self.windows
            .panes
            .get(self.windows.active)
            .and_then(|id| self.draft_index_by_id(*id))
            .unwrap_or(self.active_draft)
    }

    /// Where the keyboard window sits in layout order.
    pub fn active_window_position(&self) -> usize {
        if self.windows.panes.is_empty() {
            return 0;
        }
        self.windows
            .active
            .min(self.windows.panes.len().saturating_sub(1))
    }

    /// The rect the keyboard window paints in. Falls back to the whole editor
    /// before a split layout has been computed.
    pub fn active_editor_area(&self) -> Rect {
        self.ui
            .layout
            .window_rects
            .get(self.active_window_position())
            .copied()
            .filter(|rect| rect.width > 0 && rect.height > 0)
            .unwrap_or(self.ui.layout.editor)
    }

    /// Open a window on `index` (or focus it when it is already open) and
    /// give it the keyboard. False once the desktop is full.
    pub fn open_window(&mut self, index: usize) -> bool {
        if index >= self.drafts.len() {
            return false;
        }
        let id = self.drafts[index].id;
        if let Some(position) = self.windows.panes.iter().position(|pane| *pane == id) {
            self.windows.active = position;
            self.active_draft = index;
            return true;
        }
        if self.windows.panes.is_empty() && id == self.active_draft_id() {
            // Two views of one draft would share one cursor, so a split
            // waits for a second page instead.
            return false;
        }
        if self.windows.panes.len() >= MAX_WINDOWS {
            return false;
        }
        if self.windows.panes.is_empty() {
            // The implicit window becomes explicit before the second opens.
            self.windows.panes.push(self.active_draft_id());
        }
        self.windows.panes.push(id);
        self.windows.active = self.windows.panes.len() - 1;
        self.active_draft = index;
        true
    }

    /// Bring the window holding `id` to the keyboard.
    pub fn focus_window_id(&mut self, id: DraftId) -> bool {
        let Some(position) = self.windows.panes.iter().position(|pane| *pane == id) else {
            return false;
        };
        self.windows.active = position;
        if let Some(index) = self.draft_index_by_id(id) {
            self.active_draft = index;
        }
        true
    }

    /// Move the window that owns the keyboard to `slot` (zero-based tile
    /// position), swapping with the window already there. False when the
    /// desktop is unsplit, the slot is out of range, or it is already home.
    pub fn move_active_window_to(&mut self, slot: usize) -> bool {
        if self.windows.panes.len() < 2 || slot >= self.windows.panes.len() {
            return false;
        }
        let active = self.active_window_position();
        if active == slot {
            return false;
        }
        self.windows.panes.swap(active, slot);
        self.windows.active = slot;
        true
    }

    /// Move the keyboard to the next (`delta` ±1) window. False when the
    /// plain editor is all there is.
    pub fn cycle_window(&mut self, delta: isize) -> bool {
        if self.windows.panes.len() < 2 {
            return false;
        }
        let count = self.windows.panes.len() as isize;
        let next = (self.windows.active as isize + delta).rem_euclid(count) as usize;
        self.windows.active = next;
        if let Some(index) = self.draft_index_by_id(self.windows.panes[next]) {
            self.active_draft = index;
        }
        true
    }

    /// Close the window that owns the keyboard. The next window takes over;
    /// dropping to a single pane falls back to the plain editor.
    pub fn close_active_window(&mut self) -> bool {
        if self.windows.panes.is_empty() {
            return false;
        }
        let position = self.active_window_position();
        self.windows.panes.remove(position);
        match self.windows.panes.first().copied() {
            // Down to the plain window: the survivor owns the keyboard.
            Some(survivor) if self.windows.panes.len() < 2 => {
                self.windows.panes.clear();
                self.windows.active = 0;
                if let Some(index) = self.draft_index_by_id(survivor) {
                    self.active_draft = index;
                }
            }
            _ => {
                self.windows.active = position.min(self.windows.panes.len().saturating_sub(1));
                self.active_draft = self.active_window_draft();
            }
        }
        true
    }

    /// Point the keyboard window at the active draft. Switching pages swaps
    /// the focused window's content; a page already on screen trades places
    /// with it instead of appearing twice.
    pub fn sync_active_window(&mut self) {
        if self.windows.panes.is_empty() || self.drafts.is_empty() {
            return;
        }
        let index = self.active_draft.min(self.drafts.len() - 1);
        let id = self.drafts[index].id;
        let position = self.active_window_position();
        match self.windows.panes.iter().position(|pane| *pane == id) {
            Some(other) if other != position => {
                let previous = self.windows.panes[position];
                self.windows.panes[position] = id;
                self.windows.panes[other] = previous;
            }
            _ => self.windows.panes[position] = id,
        }
    }

    /// Moves a page one visible step up (`delta < 0`) or down (`delta > 0`)
    /// hopping over hidden scratch drafts. The open page follows the moved
    /// row. Returns false at the edges.
    pub fn move_draft(&mut self, draft_index: usize, delta: isize) -> bool {
        if delta == 0 || draft_index >= self.drafts.len() {
            return false;
        }
        let visible = self.visible_drafts();
        let Some(position) = visible.iter().position(|&index| index == draft_index) else {
            return false;
        };
        let Some(target) = position
            .checked_add_signed(delta)
            .filter(|&target| target < visible.len())
        else {
            return false;
        };
        let other = visible[target];
        self.drafts.swap(draft_index, other);
        // `other` is the moved page's new home; whoever lived there took its
        // old slot, so the open page follows too.
        if self.active_draft == draft_index {
            self.active_draft = other;
        } else if self.active_draft == other {
            self.active_draft = draft_index;
        }
        true
    }

    pub fn find_draft_by_path(&self, path: &Path) -> Option<usize> {
        self.drafts.iter().position(|draft| {
            draft
                .editor
                .file_path
                .as_deref()
                .map(|candidate| paths_match(candidate, path))
                .unwrap_or(false)
        })
    }

    /// Rows for the Pages panel: one per listed page, in list order.
    pub fn page_rows(&self) -> Vec<PageRow> {
        self.visible_drafts()
            .into_iter()
            .enumerate()
            .map(|(position, index)| {
                let draft = &self.drafts[index];
                PageRow {
                    index,
                    number: position + 1,
                    label: draft.page_label(),
                    active: index == self.active_draft,
                    unsaved: draft.editor.is_dirty(),
                }
            })
            .collect()
    }

    /// Rows for the Workspace panel: folder headers (with their expander)
    /// followed by the files of every expanded folder.
    pub fn workspace_rows(&self) -> Vec<WorkspaceRow> {
        let mut rows = Vec::new();
        for folder in self.workspace_folders.clone() {
            let marker = if folder.expanded { "▾" } else { "▸" };
            let (file_count, unsaved) = self.workspace_summary(&folder.path);
            rows.push(WorkspaceRow {
                kind: WorkspaceRowKind::Folder(folder.path.clone()),
                label: format!("{marker} {}", folder.name()),
                unsaved,
                file_count,
            });
            if folder.expanded {
                for file in self.workspace_files(&folder.path) {
                    rows.push(WorkspaceRow {
                        kind: WorkspaceRowKind::File(file.path),
                        label: format!("  {}", file.relative),
                        unsaved: 0,
                        file_count: 0,
                    });
                }
            }
        }
        rows
    }

    pub fn remember_recent_path(&mut self, path: PathBuf) {
        let normalized = normalize_document_path(path);
        self.recent_files
            .retain(|candidate| !paths_match(candidate, &normalized));
        self.recent_files.insert(0, normalized);
        self.recent_files.truncate(MAX_RECENT_FILES);
        self.ensure_welcome_selection();
    }

    pub fn remove_recent_path(&mut self, path: &Path) {
        self.recent_files
            .retain(|candidate| !paths_match(candidate, path));
        self.ensure_welcome_selection();
    }

    /// Register a folder in the workspace panel. Returns the normalized path,
    /// or a reason the folder cannot be tracked.
    pub fn add_workspace(&mut self, path: PathBuf) -> Result<PathBuf, String> {
        let normalized = normalize_document_path(path);
        if !normalized.is_dir() {
            return Err("That folder does not exist.".to_string());
        }
        if self
            .workspace_folders
            .iter()
            .any(|folder| paths_match(&folder.path, &normalized))
        {
            return Err("That folder is already tracked.".to_string());
        }
        self.workspace_folders
            .push(WorkspaceFolder::new(normalized.clone()));
        Ok(normalized)
    }

    /// Drop a folder registration. The folder itself is untouched.
    pub fn remove_workspace(&mut self, path: &Path) -> bool {
        let before = self.workspace_folders.len();
        self.workspace_folders
            .retain(|folder| !paths_match(&folder.path, path));
        self.workspace_folders.len() != before
    }

    pub fn toggle_workspace_expanded(&mut self, path: &Path) {
        if let Some(folder) = self
            .workspace_folders
            .iter_mut()
            .find(|folder| paths_match(&folder.path, path))
        {
            folder.expanded = !folder.expanded;
        }
    }

    /// Cached document listing for a workspace root.
    pub fn workspace_files(&self, root: &Path) -> Vec<crate::workspace::WorkspaceFile> {
        self.workspace_scans.borrow_mut().files_for(root)
    }

    /// True when `path` is open in a draft with unsaved edits.
    pub fn is_path_dirty(&self, path: &Path) -> bool {
        self.find_draft_by_path(path)
            .and_then(|index| self.drafts.get(index))
            .is_some_and(|draft| draft.editor.is_dirty())
    }

    /// `(documents, unsaved)` for a workspace folder header.
    pub fn workspace_summary(&self, root: &Path) -> (usize, usize) {
        let files = self.workspace_files(root);
        let unsaved = files
            .iter()
            .filter(|file| self.is_path_dirty(&file.path))
            .count();
        (files.len(), unsaved)
    }

    pub fn retarget_path_references(&mut self, source: &Path, target: &Path, source_is_dir: bool) {
        for draft in &mut self.drafts {
            if let Some(path) = &mut draft.editor.file_path {
                if let Some(updated) = retarget_path(path, source, target, source_is_dir) {
                    *path = updated;
                }
            }
        }

        for path in &mut self.recent_files {
            if let Some(updated) = retarget_path(path, source, target, source_is_dir) {
                *path = updated;
            }
        }

        self.recent_files = normalize_recent_files(self.recent_files.clone());
        self.ensure_welcome_selection();
    }

    /// Close one draft by index. The list never empties: closing the last
    /// page leaves a fresh scratch behind.
    pub fn remove_draft_at(&mut self, index: usize) -> bool {
        if index >= self.drafts.len() {
            return false;
        }
        let removed = self.drafts[index].id;
        self.drafts.remove(index);
        if self.drafts.is_empty() {
            self.drafts.push(DraftState::auto_scratch());
        }
        self.active_draft = self.active_draft.min(self.drafts.len().saturating_sub(1));
        self.windows.panes.retain(|pane| *pane != removed);
        if self.windows.panes.len() < 2 {
            self.windows.panes.clear();
            self.windows.active = 0;
        } else {
            self.windows.active = self
                .windows
                .active
                .min(self.windows.panes.len().saturating_sub(1));
            self.active_draft = self.active_window_draft();
        }
        self.ensure_welcome_selection();
        true
    }

    pub fn remove_drafts_for_path(&mut self, path: &Path) -> usize {
        let before = self.drafts.len();
        self.drafts.retain(|draft| {
            !draft
                .editor
                .file_path
                .as_deref()
                .map(|candidate| paths_match(candidate, path))
                .unwrap_or(false)
        });
        let removed = before.saturating_sub(self.drafts.len());
        if self.drafts.is_empty() {
            self.drafts.push(DraftState::auto_scratch());
        }
        self.active_draft = self.active_draft.min(self.drafts.len().saturating_sub(1));
        self.ensure_welcome_selection();
        removed
    }

    pub fn close_draft(&mut self, index: usize) -> bool {
        if index >= self.drafts.len() || self.drafts.len() <= 1 {
            return false;
        }
        self.drafts.remove(index);
        self.active_draft = self.active_draft.min(self.drafts.len().saturating_sub(1));
        self.ensure_welcome_selection();
        true
    }

    pub fn close_active_draft(&mut self) -> bool {
        self.close_draft(self.active_draft)
    }

    pub fn mark_draft_dirty(&mut self, index: usize) {
        if let Some(draft) = self.drafts.get_mut(index) {
            draft.editor.mark_dirty();
        }
    }

    /// Snapshots of every draft with unsaved edits, for crash recovery.
    pub fn unsaved_drafts(&self) -> Vec<RecoveredDraft> {
        self.drafts
            .iter()
            .filter(|draft| draft.editor.is_dirty())
            .map(|draft| RecoveredDraft {
                path: draft.editor.file_path.clone(),
                title: draft.editor.title(),
                text: draft.editor.buffer.plain_text(),
                content_html: draft.editor.buffer.to_ord_html(),
                status: Some(draft.editor.doc_status.clone()),
                auto_title: Some(draft.editor.doc_auto_title),
                label_color: Some(draft.editor.doc_label_color.clone()),
            })
            .collect()
    }

    fn recent_candidates(&self) -> Vec<PathBuf> {
        self.recent_files
            .iter()
            .filter(|path| self.find_draft_by_path(path.as_path()).is_none())
            .take(MAX_RECENT_FILES)
            .cloned()
            .collect()
    }

    pub fn set_draft_metadata(&mut self, index: usize, metadata: DocumentMetadata) {
        if let Some(draft) = self.drafts.get_mut(index) {
            draft.editor.doc_status = metadata.status;
            draft.editor.doc_auto_title = metadata.auto_title;
            draft.editor.doc_label_color = metadata.label_color;
        }
    }
}

/// Next stable draft id. Process-wide, so every draft in a session is unique
/// no matter how many `AppState`s tests build.
fn next_draft_id() -> DraftId {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    DraftId(NEXT.fetch_add(1, Ordering::Relaxed))
}

impl DraftState {
    pub fn new(buffer: EditorBuffer, file_path: Option<PathBuf>) -> Self {
        Self::new_with_visibility(buffer, file_path, true)
    }

    pub fn auto_scratch() -> Self {
        Self::new_with_visibility(EditorBuffer::new(), None, false)
    }

    pub fn new_with_visibility(
        buffer: EditorBuffer,
        file_path: Option<PathBuf>,
        listed: bool,
    ) -> Self {
        Self {
            id: next_draft_id(),
            editor: EditorPaneState::new(buffer, file_path),
            notes: NotesPaneState::default(),
            listed,
        }
    }

    pub fn page_label(&self) -> String {
        self.editor
            .file_path
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.editor.title())
    }

    pub fn is_auto_scratch(&self) -> bool {
        !self.listed
            && self.editor.file_path.is_none()
            && !self.editor.is_dirty()
            && !self.editor.pending_save
            && self.editor.buffer.plain_text().is_empty()
            && self.notes.revision == 0
            && self.notes.buffer.plain_text().is_empty()
    }

    /// Whether this draft shows up as a page.
    pub fn is_listed(&self) -> bool {
        self.listed
            || self.editor.file_path.is_some()
            || self.editor.is_dirty()
            || !self.editor.buffer.plain_text().is_empty()
            || self.notes.revision > 0
            || !self.notes.buffer.plain_text().is_empty()
    }
}

impl EditorPaneState {
    pub fn new(buffer: EditorBuffer, file_path: Option<PathBuf>) -> Self {
        Self {
            buffer,
            file_path: file_path.map(normalize_document_path),
            scroll: 0,
            revision: 0,
            last_saved_revision: 0,
            pending_save: false,
            doc_status: String::from("Draft"),
            doc_auto_title: true,
            doc_label_color: String::new(),
            bookmarks: [None; 10],
            last_bookmark: None,
            free_scroll_at: None,
        }
    }

    pub fn title(&self) -> String {
        self.file_path
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.buffer.title())
    }

    pub fn is_dirty(&self) -> bool {
        self.revision != self.last_saved_revision
    }

    pub fn mark_dirty(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }

    pub fn mark_saved(&mut self, revision: u64, path: PathBuf) {
        self.file_path = Some(normalize_document_path(path));
        if revision > self.last_saved_revision {
            self.last_saved_revision = revision;
        }
        self.pending_save = false;
    }

    /// Runs on every keystroke and every frame, so it asks the buffer for the
    /// two numbers it needs instead of building a layout for the whole document.
    pub fn ensure_cursor_visible(&mut self, width: usize, height: usize) {
        let width = width.max(1);
        let height = height.max(1);
        if self.free_scroll_at == Some(self.buffer.cursor()) {
            // Free scroll (wheel/trackpad): only clamp into range, never
            // yank the view back to the cursor. Any cursor move clears the
            // flag by changing the cursor first.
            let max_scroll = self.buffer.total_visual_lines(width).saturating_sub(height);
            self.scroll = self.scroll.min(max_scroll);
            return;
        }
        self.free_scroll_at = None;
        let cursor_line = self
            .buffer
            .visual_line_for_cursor(width, self.buffer.cursor());

        if cursor_line < self.scroll {
            self.scroll = cursor_line;
        } else if cursor_line >= self.scroll.saturating_add(height) {
            self.scroll = cursor_line.saturating_sub(height.saturating_sub(1));
        }

        let max_scroll = self.buffer.total_visual_lines(width).saturating_sub(height);
        self.scroll = self.scroll.min(max_scroll);
    }

    pub fn default_save_path(&self) -> PathBuf {
        self.file_path.clone().unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(default_save_file_name(Some(&self.title())))
        })
    }

    /// Word/character/sentence/paragraph totals, folded once per edit inside
    /// the buffer. Reading them never scans the document.
    pub fn word_stats(&self) -> WordStats {
        self.buffer.stats()
    }
}

impl NotesPaneState {
    pub fn mark_dirty(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }

    /// Runs on every keystroke and every frame, so it asks the buffer for the
    /// two numbers it needs instead of building a layout for the whole document.
    pub fn ensure_cursor_visible(&mut self, width: usize, height: usize) {
        let width = width.max(1);
        let height = height.max(1);
        if self.free_scroll_at == Some(self.buffer.cursor()) {
            // Free scroll (wheel/trackpad): only clamp into range, never
            // yank the view back to the cursor. Any cursor move clears the
            // flag by changing the cursor first.
            let max_scroll = self.buffer.total_visual_lines(width).saturating_sub(height);
            self.scroll = self.scroll.min(max_scroll);
            return;
        }
        self.free_scroll_at = None;
        let cursor_line = self
            .buffer
            .visual_line_for_cursor(width, self.buffer.cursor());

        if cursor_line < self.scroll {
            self.scroll = cursor_line;
        } else if cursor_line >= self.scroll.saturating_add(height) {
            self.scroll = cursor_line.saturating_sub(height.saturating_sub(1));
        }

        let max_scroll = self.buffer.total_visual_lines(width).saturating_sub(height);
        self.scroll = self.scroll.min(max_scroll);
    }
}

fn normalize_recent_files(recent_files: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut normalized: Vec<PathBuf> = Vec::new();
    for path in recent_files {
        if !is_recent_path_visible(&path) {
            continue;
        }
        let path = normalize_document_path(path);
        if normalized
            .iter()
            .any(|candidate| paths_match(candidate, &path))
        {
            continue;
        }
        normalized.push(path);
        if normalized.len() >= MAX_RECENT_FILES {
            break;
        }
    }
    normalized
}

fn is_recent_path_visible(path: &std::path::Path) -> bool {
    if !path.is_file() {
        return false;
    }

    if is_startup_cursor_artifact(path) {
        return false;
    }

    if is_temp_test_artifact(path) {
        return false;
    }

    true
}

fn is_startup_cursor_artifact(path: &std::path::Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.starts_with("ordet-startup-cursor-"))
        .unwrap_or(false)
}

fn is_temp_test_artifact(path: &std::path::Path) -> bool {
    let mut prefixes = vec![std::env::temp_dir(), PathBuf::from("/tmp")];
    if let Ok(canonical_temp_dir) = std::env::temp_dir().canonicalize() {
        prefixes.push(canonical_temp_dir);
    }

    let canonical_path = path.canonicalize().ok();

    prefixes.into_iter().any(|prefix| {
        if path_is_temp_test_artifact(path, &prefix) {
            return true;
        }

        match (&canonical_path, prefix.canonicalize()) {
            (Some(canonical_path), Ok(canonical_prefix)) => {
                path_is_temp_test_artifact(canonical_path, &canonical_prefix)
            }
            _ => false,
        }
    })
}

fn path_is_temp_test_artifact(path: &std::path::Path, prefix: &std::path::Path) -> bool {
    path.strip_prefix(prefix)
        .ok()
        .and_then(|relative| relative.components().next())
        .and_then(|component| component.as_os_str().to_str())
        .map(|name| {
            (name.starts_with("ordet-") && name.ends_with("-test"))
                || name.starts_with("ordet-startup-cursor-")
        })
        .unwrap_or(false)
}

fn retarget_path(
    path: &Path,
    source: &Path,
    target: &Path,
    source_is_dir: bool,
) -> Option<PathBuf> {
    if paths_match(path, source) {
        return Some(normalize_document_path(target));
    }

    if source_is_dir {
        let normalized_source = normalize_document_path(source);
        let normalized_target = normalize_document_path(target);
        let normalized_path = normalize_document_path(path);
        if let Ok(relative) = normalized_path.strip_prefix(&normalized_source) {
            return Some(normalized_target.join(relative));
        }
    }

    None
}

pub(crate) fn normalize_document_path(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };

    if absolute.exists() {
        // `dunce` removes Windows' `\\?\` verbatim prefix so paths in the
        // title bar, recent list, and status line stay readable.
        dunce::canonicalize(&absolute).unwrap_or(absolute)
    } else {
        absolute
    }
}

pub(crate) fn paths_match(left: &Path, right: &Path) -> bool {
    normalize_document_path(left) == normalize_document_path(right)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        panel_inner_rect, panel_rect, split_rects, AppState, PanelKind, PanelsState, WelcomeOption,
        WorkspaceRowKind,
    };
    use crate::editor_buffer::EditorBuffer;
    use crate::theme::ThemeSelection;
    use ordet_tui_framework::Rect;

    #[test]
    fn pages_are_numbered_among_the_visible_drafts() {
        let mut state = blank_state();
        assert_eq!(
            state.active_page_number(),
            None,
            "a hidden scratch is not a page yet"
        );
        assert_eq!(state.visible_draft_count(), 0);

        state.add_draft(EditorBuffer::from_text("one"), None);
        state.add_draft(EditorBuffer::from_text("two"), None);
        assert_eq!(state.visible_draft_count(), 2);
        assert_eq!(state.active_page_number(), Some(2), "two is open");

        state.switch_to_draft(1);
        assert_eq!(state.active_page_number(), Some(1));
    }

    #[test]
    fn page_rows_number_the_listed_pages_and_mark_unsaved_ones() {
        let mut state = blank_state();
        state.add_draft(EditorBuffer::from_text("one"), None);
        state.add_draft(EditorBuffer::from_text("two"), None);
        state.mark_draft_dirty(2);

        let rows = state.page_rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].number, 1);
        assert_eq!(rows[0].label, "one");
        assert!(!rows[0].active);
        assert!(!rows[0].unsaved);
        assert_eq!(rows[1].number, 2);
        assert_eq!(rows[1].label, "two");
        assert!(rows[1].active);
        assert!(rows[1].unsaved);
        assert_eq!(state.active_visible_draft_index(), 1);
    }

    #[test]
    fn split_rects_tile_the_editor_with_gutters() {
        let area = Rect::new(0, 0, 100, 30);
        assert_eq!(split_rects(area, 1), vec![area]);

        let two = split_rects(area, 2);
        assert_eq!(two.len(), 2);
        assert_eq!(two[0].width + two[1].width + 1, 100, "{two:?}");
        assert_eq!(two[0].height, 30);

        let three = split_rects(area, 3);
        assert_eq!(three.len(), 3);
        assert_eq!(three[1].height + three[2].height + 1, 30, "{three:?}");

        let four = split_rects(area, 4);
        assert_eq!(four.len(), 4);
        assert_eq!(four[0].height + four[2].height + 1, 30, "{four:?}");

        // Too small to split: the single pane survives.
        assert_eq!(split_rects(Rect::new(0, 0, 4, 3), 4).len(), 1);
    }

    #[test]
    fn windows_open_cycle_and_close() {
        let mut state = blank_state();
        state.add_draft(EditorBuffer::from_text("one"), None);
        state.add_draft(EditorBuffer::from_text("two"), None);
        let rows = state.page_rows();
        let (one, two) = (rows[0].index, rows[1].index);

        assert!(state.open_window(one));
        assert_eq!(state.window_count(), 2);
        assert_eq!(state.window_drafts(), vec![two, one]);
        assert_eq!(state.active_window_draft(), one);

        assert!(state.cycle_window(1));
        assert_eq!(state.active_window_draft(), two);
        assert!(state.cycle_window(-1));
        assert_eq!(state.active_window_draft(), one);

        assert!(state.close_active_window());
        assert_eq!(state.window_count(), 1);
        assert_eq!(state.active_window_draft(), two);
    }

    #[test]
    fn a_window_moves_to_a_slot_by_swapping_tiles() {
        let mut state = blank_state();
        for label in ["one", "two", "three", "four"] {
            state.add_draft(EditorBuffer::from_text(label), None);
        }
        let rows = state.page_rows();
        // The active page is "four"; opening one, two, three fills the rest
        // in that order: [four, one, two, three].
        for row in rows.iter().take(3) {
            assert!(state.open_window(row.index));
        }
        let labels = |state: &AppState| -> Vec<String> {
            state
                .window_drafts()
                .iter()
                .map(|index| state.drafts[*index].page_label())
                .collect()
        };
        assert_eq!(labels(&state), vec!["four", "one", "two", "three"]);
        assert_eq!(state.active_window_position(), 3);

        // Move window 4 into slot 1: the old slot 1 trades places.
        assert!(state.move_active_window_to(0));
        assert_eq!(labels(&state), vec!["three", "one", "two", "four"]);
        assert_eq!(state.active_window_position(), 0);

        // Focus "one" (slot 2) and move it to slot 3.
        let one = state.drafts[rows[0].index].id;
        assert!(state.focus_window_id(one));
        assert_eq!(state.active_window_position(), 1);
        assert!(state.move_active_window_to(2));
        assert_eq!(labels(&state), vec!["three", "two", "one", "four"]);
        assert_eq!(state.active_window_position(), 2);

        // Same slot and off-desktop slots are no-ops.
        assert!(!state.move_active_window_to(2));
        assert!(!state.move_active_window_to(4));

        // The plain editor has no slots to move to.
        let mut single = blank_state();
        single.add_draft(EditorBuffer::from_text("solo"), None);
        assert!(!single.move_active_window_to(0));
    }

    #[test]
    fn switching_pages_trades_places_instead_of_duplicating() {
        let mut state = blank_state();
        state.add_draft(EditorBuffer::from_text("one"), None);
        state.add_draft(EditorBuffer::from_text("two"), None);
        let rows = state.page_rows();
        let (one, two) = (rows[0].index, rows[1].index);
        state.open_window(one);
        assert_eq!(state.window_drafts(), vec![two, one]);

        // Page two is already on screen: the panes trade contents so it can
        // take the keyboard without appearing twice.
        state.switch_to_draft(two);
        assert_eq!(state.window_drafts(), vec![one, two]);
        assert_eq!(state.active_window_draft(), two);
    }

    #[test]
    fn closing_a_page_closes_its_window() {
        let mut state = blank_state();
        state.add_draft(EditorBuffer::from_text("one"), None);
        state.add_draft(EditorBuffer::from_text("two"), None);
        state.add_draft(EditorBuffer::from_text("three"), None);
        let rows = state.page_rows();
        let (one, two) = (rows[0].index, rows[1].index);
        state.open_window(one);
        state.open_window(two);
        assert_eq!(state.window_count(), 3);

        // Close the page shown in the left window: its window goes with it
        // and the keyboard lands on a surviving pane.
        assert!(state.remove_draft_at(one));
        assert_eq!(state.window_count(), 2);
        let active = state.active_window_draft();
        assert_eq!(state.drafts[active].page_label(), "two");
        assert!(!state
            .window_drafts()
            .iter()
            .any(|index| state.drafts[*index].page_label() == "one"));
    }

    #[test]
    fn moving_a_page_reorders_the_list_past_the_hidden_scratch() {
        let mut state = blank_state();
        state.add_draft(EditorBuffer::from_text("one"), None);
        state.add_draft(EditorBuffer::from_text("two"), None);
        let two = 2;
        assert_eq!(state.active_page_number(), Some(2));

        assert!(state.move_draft(two, -1));

        // The hidden scratch stays put; the two pages swap around it, and the
        // open page follows the moved row.
        assert_eq!(state.editor().title(), "two");
        assert_eq!(state.active_page_number(), Some(1));
        let pages: Vec<String> = state
            .page_rows()
            .iter()
            .map(|row| row.label.clone())
            .collect();
        assert_eq!(pages, vec!["two".to_string(), "one".to_string()]);

        // Two leads the list now; one trails it. Neither can move further.
        assert!(!state.move_draft(1, -1));
        assert!(!state.move_draft(2, 1));
        // Moving the untouched page from under the open one swaps them again.
        assert!(state.move_draft(2, -1));
        assert_eq!(state.editor().title(), "two");

        // Degenerate deltas are no-ops: zero, past the end, off the list.
        assert!(!state.move_draft(1, 0));
        assert!(!state.move_draft(1, 5));
        assert!(!state.move_draft(99, -1));
        let pages: Vec<String> = state
            .page_rows()
            .iter()
            .map(|row| row.label.clone())
            .collect();
        assert_eq!(pages, vec!["one".to_string(), "two".to_string()]);
    }

    fn workspace_test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ordet-workspace-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("workspace test dir");
        dir
    }

    fn blank_state() -> AppState {
        AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        )
    }

    #[test]
    fn workspaces_register_dedupe_and_remove() {
        let mut state = blank_state();
        let dir = workspace_test_dir("register");
        std::fs::write(dir.join("a.ord"), "words").expect("write");

        let added = state.add_workspace(dir.clone()).expect("add");
        assert!(added.is_dir());
        assert!(state.add_workspace(dir.clone()).is_err());
        assert_eq!(state.workspace_folders.len(), 1);

        let missing = dir.join("nope");
        assert!(state.add_workspace(missing).is_err());

        assert!(state.remove_workspace(&dir));
        assert!(!state.remove_workspace(&dir));
        assert!(state.workspace_folders.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn workspace_rows_mark_unsaved_files() {
        let dir = workspace_test_dir("rows");
        let saved = dir.join("saved.ord");
        let dirty = dir.join("dirty.ord");
        std::fs::write(&saved, "saved words").expect("write");
        std::fs::write(&dirty, "dirty words").expect("write");

        let mut state = AppState::new(
            vec![
                (
                    EditorBuffer::from_text("dirty words plus more"),
                    Some(dirty.clone()),
                ),
                (EditorBuffer::from_text("saved words"), Some(saved.clone())),
            ],
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        // The saved draft matches disk; the dirty one has unsaved edits.
        state.mark_draft_dirty(0);
        state.add_workspace(dir.clone()).expect("add");

        assert!(state.is_path_dirty(&dirty));
        assert!(!state.is_path_dirty(&saved));
        let (files, unsaved) = state.workspace_summary(&dir);
        assert_eq!((files, unsaved), (2, 1));

        let rows = state.workspace_rows();
        assert_eq!(rows.len(), 3);
        assert!(matches!(rows[0].kind, WorkspaceRowKind::Folder(_)));
        assert_eq!(rows[0].unsaved, 1);
        assert_eq!(rows[0].file_count, 2);
        assert!(matches!(rows[1].kind, WorkspaceRowKind::File(_)));

        state.toggle_workspace_expanded(&dir);
        let collapsed = state.workspace_rows();
        assert_eq!(collapsed.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_startup_uses_a_hidden_scratch_draft() {
        let state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );

        assert_eq!(state.drafts.len(), 1);
        assert!(state.has_only_auto_scratch());
        assert_eq!(state.visible_draft_count(), 0);
        assert!(state.visible_drafts().is_empty());
        assert!(state.page_rows().is_empty());
    }

    #[test]
    fn replacing_hidden_scratch_promotes_the_real_draft() {
        let mut state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );

        state.replace_active_draft(
            EditorBuffer::from_text("hello"),
            Some(PathBuf::from("/tmp/Kayra.ord")),
        );

        assert_eq!(state.drafts.len(), 1);
        assert_eq!(state.visible_draft_count(), 1);
        assert_eq!(state.editor().title(), "Kayra.ord");
        assert_eq!(state.visible_drafts(), vec![0]);
    }

    #[test]
    fn switching_drafts_changes_the_active_editor() {
        let mut state = AppState::new(
            vec![
                (EditorBuffer::from_text("Alpha"), None),
                (EditorBuffer::from_text("Beta"), None),
            ],
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );

        state.switch_to_draft(1);

        assert_eq!(state.editor().title(), "Beta");
        assert_eq!(state.active_draft, 1);
    }

    #[test]
    fn saved_draft_title_prefers_the_file_name_over_the_first_line() {
        let path = PathBuf::from("/tmp/Kayra.ord");
        let state = AppState::new(
            vec![(
                EditorBuffer::from_text("Time is the enemy once again."),
                Some(path),
            )],
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );

        assert_eq!(state.editor().title(), "Kayra.ord");
    }

    #[test]
    fn pages_panel_hides_recent_files() {
        let base = unique_temp_dir("ordet-recent-state");
        fs::create_dir_all(&base).expect("temp dir");
        let alpha = base.join("alpha.txt");
        let beta = base.join("beta.txt");
        fs::write(&alpha, "alpha").expect("alpha");
        fs::write(&beta, "beta").expect("beta");
        let state = AppState::new(
            vec![(EditorBuffer::from_text("Alpha"), Some(alpha.clone()))],
            vec![alpha, beta],
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );

        // Pages lists drafts only; recents still feed the welcome screen via
        // `recent_candidates`.
        assert_eq!(state.visible_drafts(), vec![0]);
        assert!(!state
            .page_rows()
            .iter()
            .any(|row| row.label.contains("beta")));
        assert_eq!(state.recent_candidates().len(), 1);
    }

    #[test]
    fn welcome_options_promote_resume_when_recent_work_exists() {
        let base = unique_temp_dir("ordet-welcome-recent");
        fs::create_dir_all(&base).expect("temp dir");
        let recent = base.join("chapter-07.ord");
        fs::write(&recent, "alpha").expect("recent");

        let state = AppState::new(
            Vec::new(),
            vec![recent.clone()],
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );

        assert_eq!(
            state.welcome_options(),
            vec![
                WelcomeOption::ResumeRecent,
                WelcomeOption::NewDraft,
                WelcomeOption::OpenFile,
                WelcomeOption::Help,
                WelcomeOption::Exit,
            ]
        );
        assert_eq!(
            state.primary_recent_path(),
            Some(super::normalize_document_path(recent))
        );
        assert_eq!(state.ui.welcome_selected, WelcomeOption::ResumeRecent);
    }

    #[test]
    fn welcome_selection_skips_resume_when_no_recent_work_exists() {
        let mut state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );

        assert_eq!(state.ui.welcome_selected, WelcomeOption::NewDraft);
        state.cycle_welcome_selection(false);
        assert_eq!(state.ui.welcome_selected, WelcomeOption::OpenFile);
        state.cycle_welcome_selection(false);
        assert_eq!(state.ui.welcome_selected, WelcomeOption::Help);
        state.cycle_welcome_selection(false);
        assert_eq!(state.ui.welcome_selected, WelcomeOption::Exit);
        state.cycle_welcome_selection(false);
        assert_eq!(state.ui.welcome_selected, WelcomeOption::NewDraft);
    }

    #[test]
    fn normalize_recent_files_hides_test_artifacts() {
        let temp_root = std::env::temp_dir().join("ordet-recent-test");
        let _ = fs::remove_dir_all(&temp_root);
        fs::create_dir_all(&temp_root).expect("temp root");
        let fake_recent = temp_root.join("one.txt");
        fs::write(&fake_recent, "test").expect("fake recent");

        let real_root = unique_temp_dir("ordet-real-recent");
        fs::create_dir_all(&real_root).expect("real root");
        let real_recent = real_root.join("draft.txt");
        fs::write(&real_recent, "draft").expect("real recent");

        let state = AppState::new(
            vec![(EditorBuffer::new(), None)],
            vec![fake_recent, real_recent.clone()],
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );

        assert_eq!(
            state.recent_files,
            vec![super::normalize_document_path(real_recent)]
        );

        let _ = fs::remove_dir_all(&temp_root);
    }

    #[test]
    fn normalize_recent_files_hides_startup_cursor_artifacts() {
        let startup_recent = unique_temp_dir("ordet-startup-cursor").with_extension("txt");
        fs::write(&startup_recent, "test").expect("startup artifact");

        let real_root = unique_temp_dir("ordet-real-startup-recent");
        fs::create_dir_all(&real_root).expect("real root");
        let real_recent = real_root.join("draft.txt");
        fs::write(&real_recent, "draft").expect("real recent");

        let state = AppState::new(
            vec![(EditorBuffer::new(), None)],
            vec![startup_recent.clone(), real_recent.clone()],
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );

        assert_eq!(
            state.recent_files,
            vec![super::normalize_document_path(real_recent)]
        );

        let _ = fs::remove_file(startup_recent);
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn case_only_path_differences_do_not_duplicate_drafts_or_recents() {
        let base = unique_temp_dir("ordet-case-draft");
        fs::create_dir_all(&base).expect("temp dir");
        let upper = base.join("Kayra.ord");
        fs::write(&upper, "alpha").expect("draft");
        let lower = base.join("kayra.ord");

        let state = AppState::new(
            vec![(EditorBuffer::from_text("Alpha"), Some(upper.clone()))],
            vec![lower.clone()],
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );

        assert_eq!(state.find_draft_by_path(&lower), Some(0));
        assert!(!state
            .page_rows()
            .iter()
            .any(|row| row.label.contains("kayra")));
    }

    #[test]
    fn removing_a_saved_draft_closes_it_instead_of_detaching_it() {
        let base = unique_temp_dir("ordet-remove-draft");
        fs::create_dir_all(&base).expect("temp dir");
        let path = base.join("draft.ord");
        fs::write(&path, "alpha").expect("draft");

        let mut state = AppState::new(
            vec![
                (EditorBuffer::from_text("Alpha"), Some(path.clone())),
                (EditorBuffer::from_text("Beta"), None),
            ],
            vec![path.clone()],
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );

        let removed = state.remove_drafts_for_path(&path);

        assert_eq!(removed, 1);
        assert_eq!(state.drafts.len(), 1);
        assert!(state.find_draft_by_path(&path).is_none());
    }

    #[test]
    fn panels_toggle_one_at_a_time() {
        let mut panels = PanelsState::default();
        assert!(!panels.is_open());

        panels.open(PanelKind::Notes);
        assert!(panels.is_open());
        assert!(panels.is(PanelKind::Notes));

        // Opening another replaces the first; toggling the same one closes.
        panels.open(PanelKind::Help);
        assert!(panels.is(PanelKind::Help));
        panels.toggle(PanelKind::Help);
        assert!(!panels.is_open());

        panels.open(PanelKind::Pages);
        panels.scroll = 4;
        panels.close();
        assert_eq!(panels.scroll, 0);
        assert!(!panels.is_open());
    }

    #[test]
    fn editor_fills_the_whole_body() {
        let mut state = blank_state();
        state.update_layout(Rect::new(0, 0, 120, 30));

        let layout = state.ui.layout;
        assert_eq!(layout.header, Rect::new(0, 0, 120, 1));
        assert_eq!(layout.editor, Rect::new(0, 1, 120, 28));
        assert_eq!(layout.status, Rect::new(0, 29, 120, 1));
        // Text area inset: 4 columns either side, ruler row, one row of air.
        assert_eq!(
            crate::components::editor_text_area(layout.editor).width,
            112
        );
        assert_eq!(
            crate::components::editor_text_area(layout.editor).height,
            26
        );
        assert_eq!(layout.window_rects, vec![layout.editor]);
    }

    #[test]
    fn hiding_the_menu_bar_hands_its_row_to_the_text() {
        let mut state = blank_state();
        state.update_layout(Rect::new(0, 0, 120, 30));
        assert_eq!(state.ui.layout.header, Rect::new(0, 0, 120, 1));
        assert_eq!(state.ui.layout.editor, Rect::new(0, 1, 120, 28));

        // Alt-M: the bar keeps its origin but loses its row, so the editor
        // (ruler included) starts on the terminal's first line and the
        // status row never moves.
        state.ui.show_menu_bar = false;
        state.update_layout(Rect::new(0, 0, 120, 30));
        assert_eq!(state.ui.layout.header, Rect::new(0, 0, 120, 0));
        assert_eq!(state.ui.layout.editor, Rect::new(0, 0, 120, 29));
        assert_eq!(state.ui.layout.status, Rect::new(0, 29, 120, 1));
        // No titles hang from a bar that is not there...
        assert!(super::menu_title_cells(state.ui.layout.header).is_empty());
        // ...but F10 still drops the menus over the first text line.
        let drop = super::menu_drop_rect(
            state.ui.layout.header,
            state.ui.layout.status.y,
            0,
            crate::commands::MENU_BAR[0].items.len(),
        );
        assert_eq!(drop.y, 0);
        assert!(drop.width > 3 && drop.height > 2, "dropdown: {drop:?}");
    }

    #[test]
    fn zoom_keeps_the_status_row_and_collapses_the_desktop() {
        let mut state = blank_state();
        state.update_layout(Rect::new(0, 0, 120, 30));
        let unzoomed_editor = state.ui.layout.editor.height;
        assert_eq!(state.ui.layout.status.height, 1);
        state.ui.zoomed = true;
        state.update_layout(Rect::new(0, 0, 120, 30));

        // Zoom hides the ruler at paint time; the bar keeps its row, and
        // the desktop collapses to the one window it shows.
        assert_eq!(state.ui.layout.status.height, 1);
        assert_eq!(state.ui.layout.editor.height, unzoomed_editor);
        assert_eq!(state.ui.layout.window_rects, vec![state.ui.layout.editor]);
    }

    #[test]
    fn panel_rect_is_empty_while_closed() {
        let mut state = blank_state();
        state.update_layout(Rect::new(0, 0, 120, 30));
        assert_eq!(panel_rect(&state), Rect::new(0, 0, 0, 0));
        assert_eq!(panel_inner_rect(&state), Rect::new(0, 0, 0, 0));
    }

    #[test]
    fn panels_are_cards_sized_to_their_content() {
        let mut state = blank_state();
        state.update_layout(Rect::new(0, 0, 120, 30));

        // Stats with no target: eight rows of counters in a 10-row box, hung
        // off the top-right of the page (row 1 is the first text row, under
        // the menu bar), not a sheet in the middle of the screen.
        state.panels.open(PanelKind::Stats);
        let rect = panel_rect(&state);
        assert_eq!((rect.y, rect.right()), (1, 120));
        assert_eq!((rect.width, rect.height), (32, 10));

        // Settings: 14 rows plus 3 section headings, capped at 22 rows, in a
        // fixed-width card so changing a value never resizes it.
        state.panels.close();
        state.panels.open(PanelKind::Settings);
        let rect = panel_rect(&state);
        assert_eq!(rect.width, 46);
        assert_eq!(rect.height, 17 + 2);

        // Help is now five short tabbed pages. The card is as wide as the
        // longest action plus the widest chord on that page, and tall enough
        // for the longest page, the footer, and the borders: no scrolling.
        state.panels.close();
        state.panels.open(PanelKind::Help);
        let rect = panel_rect(&state);
        assert_eq!(rect.width, 67);
        assert_eq!(rect.height, 18);
        assert!(rect.width < 120 && rect.height < 28);
    }

    #[test]
    fn settings_panel_keeps_its_width_and_position_across_themes() {
        let mut state = blank_state();
        state.update_layout(Rect::new(0, 0, 120, 30));
        state.panels.open(PanelKind::Settings);
        state.ui.theme = ThemeSelection::Preset {
            preset: crate::theme::ThemePreset::Midnight,
        };
        let midnight = panel_rect(&state);
        state.ui.theme = ThemeSelection::Preset {
            preset: crate::theme::ThemePreset::SilverGlobe,
        };
        let silver_globe = panel_rect(&state);
        assert_eq!(midnight, silver_globe);
        assert_eq!(silver_globe.width, 46);
        assert_eq!(silver_globe.right(), 120);
    }

    #[test]
    fn prose_cards_are_as_wide_as_the_copy_they_print() {
        let mut state = blank_state();
        state.update_layout(Rect::new(0, 0, 120, 30));

        // Revisions with no snapshots is two sentences wide, and no wider.
        state.panels.open(PanelKind::Revisions);
        let rect = panel_rect(&state);
        assert_eq!(rect.width, 46);

        // Notes opens a writing column, not a sheet: the floor is 38
        // content columns, and the card grows with the longest line.
        state.panels.open(PanelKind::Notes);
        let rect = panel_rect(&state);
        assert_eq!(rect.width, 40);
        state.notes_mut().buffer.insert_text(&"x".repeat(50));
        assert_eq!(panel_rect(&state).width, 52);
    }

    #[test]
    fn a_word_target_grows_the_stats_card_by_its_three_lines() {
        let mut state = blank_state();
        state.update_layout(Rect::new(0, 0, 120, 30));
        state.ui.word_target = Some(500);
        state.panels.open(PanelKind::Stats);
        assert_eq!(panel_rect(&state).height, 11 + 2);
    }

    #[test]
    fn list_panels_hug_their_rows_and_labels() {
        let mut state = blank_state();
        state.update_layout(Rect::new(0, 0, 120, 40));
        state.add_draft(EditorBuffer::from_text("one"), None);
        state.add_draft(EditorBuffer::from_text("two"), None);
        state.add_draft(EditorBuffer::from_text("three"), None);
        state.panels.open(PanelKind::Pages);

        // Three short page names: three rows + border, and the 30-column
        // floor rather than the old fixed 56.
        let rect = panel_rect(&state);
        assert_eq!((rect.y, rect.right()), (1, 120));
        assert_eq!(rect.width, 30);
        assert_eq!(rect.height, 5);
        // The margin the width cap keeps is the body's left-hand one now
        // that the card hangs off the right edge.
        assert!(rect.x >= 4);
    }

    #[test]
    fn panel_cards_stay_inside_a_narrow_body() {
        let mut state = blank_state();
        state.update_layout(Rect::new(0, 0, 40, 10));
        state.panels.open(PanelKind::Workspace);
        let rect = panel_rect(&state);
        // No folders tracked: the placeholder row, and the card still fits.
        assert_eq!(rect.width, 36);
        assert_eq!(rect.height, 1 + 2);
        assert!(rect.right() <= 40);

        // Tall content is clamped to the body, never spilling over it.
        state.panels.open(PanelKind::Help);
        let rect = panel_rect(&state);
        assert!(rect.right() <= 40);
        assert!(rect.bottom() <= 9, "help card inside the 8-row body");
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{stamp}"))
    }
}
