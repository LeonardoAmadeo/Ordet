use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyModifiers};
use ordet_tui_framework::{KeyChord, Keymap};

use crate::editor_buffer::EditorBuffer;
use crate::editor_buffer::ParagraphAlignment;
use crate::state::{DraftId, PanelKind, SettingsPickerKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    Quit,
    NewDraft,
    Save,
    TrashFile,
    OpenSavePathPrompt,
    OpenFilePrompt,
    CycleTheme,
    OpenThemePrompt,
    OpenCustomColorPrompt,
    OpenWordTargetPrompt,
    OpenAlignmentPrompt,
    AlignLeft,
    AlignCenter,
    AlignRight,
    AlignJustify,
    ToggleHelp,
    ToggleNotes,
    ToggleStats,
    ToggleSettings,
    ToggleRevisions,
    NextPage,
    PreviousPage,
    ToggleRuler,
    /// Alt-M: show or hide the top bar (menu titles + page name).
    ToggleMenuBar,
    /// Window menu: open the next listed page in a new window until four are
    /// tiled, then walk the desktop.
    NextWindow,
    /// Window menu: close the window that owns the keyboard.
    CloseWindow,
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    MoveWordLeft,
    MoveWordRight,
    SelectLeft,
    SelectRight,
    SelectUp,
    SelectDown,
    MoveHome,
    MoveEnd,
    PageUp,
    PageDown,
    MovePageUp,
    MovePageDown,
    Backspace,
    DeleteForward,
    Submit,
    InsertTab,
    Outdent,
    CycleCase,
    ToggleBold,
    ToggleItalic,
    BlockTitle,
    BlockHeading,
    BlockQuote,
    BlockBody,
    InsertTimestamp,
    Undo,
    Redo,
    SelectAll,
    DeleteWordForward,
    DeleteWordLeft,
    DeleteLine,
    OpenLine,
    ToggleInsertMode,
    OpenFindPrompt,
    OpenReplacePrompt,
    FindNext,
    OpenGoToLinePrompt,
    MoveDocumentTop,
    MoveDocumentBottom,
    MoveWindowTop,
    MoveWindowBottom,
    DeleteToEndOfLine,
    JumpPreviousPosition,
    MarkBlockBegin,
    MarkBlockEnd,
    MoveToBlockStart,
    MoveToBlockEnd,
    CopyBlock,
    MoveBlock,
    DeleteBlock,
    HideBlock,
    OpenWriteBlockPrompt,
    OpenReadFilePrompt,
    SaveAndClose,
    SaveAndExit,
    ToggleLineNumbers,
    CopySelection,
    CutSelection,
    PasteClipboard,
    ToggleZoom,
    OpenWorkspacePrompt,
    OpenStatusPrompt,
}

#[derive(Clone, Debug)]
pub enum Action {
    Quit,
    NewDraft,
    SaveRequested,
    OpenSavePathPrompt,
    OpenFilePrompt,
    OpenPath(PathBuf),
    CycleTheme,
    CycleThemePrevious,
    /// F6: cycle to the next draft, wrapping at the end.
    NextPage,
    /// Shift-F6: cycle to the previous draft, wrapping at the start.
    PreviousPage,
    SwitchDraft(usize),
    /// Alt-Up / Alt-Down: reorder the Pages list one step. The open page
    /// follows the moved row.
    MoveDraft {
        index: usize,
        delta: isize,
    },
    /// Delete asks, Ctrl+R confirms: `confirm` is true only for the Ctrl+R
    /// key. Anything armed fires solely on a confirming second press.
    TrashRequested {
        confirm: bool,
    },
    /// Open the named panel, or close it when it is already open.
    TogglePanel(PanelKind),
    /// Alt-R / View menu: show or hide the ruler row above the text.
    ToggleRuler,
    /// Alt-M / View menu: show or hide the top bar with the menus.
    ToggleMenuBar,
    /// Window menu: open the next listed page in a new window until four are
    /// tiled, then walk the desktop.
    NextWindow,
    /// Window menu: close the window that owns the keyboard.
    CloseWindow,
    /// Window menu: move the focused window to a tile slot, swapping with
    /// the window already there. Zero-based.
    MoveWindowToSlot(usize),
    /// Help menu About row: prints the version and where to find help.
    About,
    OpenThemePrompt,
    OpenCustomColorPrompt,
    OpenWordTargetPrompt,
    OpenAlignmentPrompt,
    AlignParagraph(ParagraphAlignment),
    /// Settings marker for the dormant language row. The welcome/browser
    /// translations remain in the binary; the row returns when the English-
    /// only beta ends.
    #[allow(dead_code)]
    CycleLanguage,
    ToggleLineNumbers,
    ToggleAutosave,
    CycleIndentWidth,
    ToggleLiveCounts,
    ToggleZoom,
    ToggleBackup,
    ToggleBackupOnClose,
    CycleBackupKeep,
    BackupLocationPrompt,
    BackupNow,
    SetStatus(String),
    FindRequested(Option<String>),
    ReplaceRequested(Option<String>),
    FindNextRequested,
    GoToLineRequested,
    ToggleInsertMode,
    WriteBlockRequested,
    ReadFileRequested,
    SaveAndCloseRequested,
    SaveAndExitRequested,
    CopyRequested,
    CutRequested,
    PasteRequested,
    SortSelectedLines,
    DropQuickBookmark,
    JumpLastBookmark,
    OpenWorkspacePrompt,
    OpenStatusPrompt,
    RemoveWorkspace(PathBuf),
    RestoreSelectedRevision,
    OpenSettingsPicker(SettingsPickerKind),
}

/// What picking a menu item does: either an `Action` applied straight to
/// the app, or a `Command` routed through the normal dispatch so it lands
/// on the editor exactly like its key chord would.
#[derive(Clone, Debug)]
pub enum MenuEffect {
    Action(Action),
    Command(Command),
}

#[derive(Clone, Debug)]
pub struct MenuItem {
    pub label: &'static str,
    pub hint: &'static str,
    pub effect: MenuEffect,
}

#[derive(Clone, Debug)]
pub struct MenuDef {
    pub title: &'static str,
    pub items: &'static [MenuItem],
}

/// The menu rows that apply to the current desktop. The Window menu does
/// not advertise commands for windows that do not exist: a lone window gets
/// only "Next window", and slot rows appear one per open window once the
/// desktop is actually split.
pub fn menu_items(menu_index: usize, window_count: usize) -> Vec<MenuItem> {
    let Some(menu) = MENU_BAR.get(menu_index) else {
        return Vec::new();
    };
    if menu.title != "Window" {
        return menu.items.to_vec();
    }

    let mut items = vec![menu.items[0].clone()];
    if window_count > 1 {
        items.extend(menu.items.iter().skip(1).take(1).cloned());
        let slot_count = window_count.min(menu.items.len().saturating_sub(2));
        items.extend(menu.items.iter().skip(2).take(slot_count).cloned());
    }
    items
}

/// The top menu bar. Every entry maps to a real action or command, no
/// decorative items. Hints name the key chords that do the same thing.
pub const MENU_BAR: &[MenuDef] = &[
    MenuDef {
        title: "File",
        items: &[
            MenuItem {
                label: "New draft",
                hint: "⌃N",
                effect: MenuEffect::Action(Action::NewDraft),
            },
            MenuItem {
                label: "Open file…",
                hint: "⌃B",
                effect: MenuEffect::Action(Action::OpenFilePrompt),
            },
            MenuItem {
                label: "Save",
                hint: "⌃S",
                effect: MenuEffect::Action(Action::SaveRequested),
            },
            MenuItem {
                label: "Save as…",
                hint: "⌃⇧S",
                effect: MenuEffect::Action(Action::OpenSavePathPrompt),
            },
            MenuItem {
                label: "Save and close",
                hint: "⌃K H",
                effect: MenuEffect::Action(Action::SaveAndCloseRequested),
            },
            MenuItem {
                label: "Save and exit",
                hint: "⌃K X",
                effect: MenuEffect::Action(Action::SaveAndExitRequested),
            },
            MenuItem {
                label: "Move current file to Trash",
                hint: "⌃K R",
                effect: MenuEffect::Action(Action::TrashRequested { confirm: true }),
            },
            MenuItem {
                label: "Track workspace folder…",
                hint: "⌃⇧W",
                effect: MenuEffect::Action(Action::OpenWorkspacePrompt),
            },
            MenuItem {
                label: "Quit",
                hint: "⌃Q",
                effect: MenuEffect::Action(Action::Quit),
            },
        ],
    },
    MenuDef {
        title: "Edit",
        items: &[
            MenuItem {
                label: "Undo",
                hint: "⌃Z",
                effect: MenuEffect::Command(Command::Undo),
            },
            MenuItem {
                label: "Redo",
                hint: "⌃⇧Z",
                effect: MenuEffect::Command(Command::Redo),
            },
            MenuItem {
                label: "Cut",
                hint: "⌃X",
                effect: MenuEffect::Action(Action::CutRequested),
            },
            MenuItem {
                label: "Copy",
                hint: "⌃C",
                effect: MenuEffect::Action(Action::CopyRequested),
            },
            MenuItem {
                label: "Paste",
                hint: "⌃V",
                effect: MenuEffect::Action(Action::PasteRequested),
            },
            MenuItem {
                label: "Select all",
                hint: "⌃A",
                effect: MenuEffect::Command(Command::SelectAll),
            },
            MenuItem {
                label: "Delete to line end",
                hint: "⌃⌥K",
                effect: MenuEffect::Command(Command::DeleteToEndOfLine),
            },
            MenuItem {
                label: "Insert / overwrite",
                hint: "Insert",
                effect: MenuEffect::Action(Action::ToggleInsertMode),
            },
            MenuItem {
                label: "Sort lines",
                hint: "",
                effect: MenuEffect::Action(Action::SortSelectedLines),
            },
            MenuItem {
                label: "Drop bookmark",
                hint: "",
                effect: MenuEffect::Action(Action::DropQuickBookmark),
            },
            MenuItem {
                label: "Jump to last mark",
                hint: "",
                effect: MenuEffect::Action(Action::JumpLastBookmark),
            },
            MenuItem {
                label: "Find…",
                hint: "⌃F",
                effect: MenuEffect::Action(Action::FindRequested(None)),
            },
            MenuItem {
                label: "Replace…",
                hint: "⌃K E",
                effect: MenuEffect::Action(Action::ReplaceRequested(None)),
            },
            MenuItem {
                label: "Go to line…",
                hint: "⌃K L",
                effect: MenuEffect::Action(Action::GoToLineRequested),
            },
            MenuItem {
                label: "Find next",
                hint: "⌃K Y",
                effect: MenuEffect::Action(Action::FindNextRequested),
            },
        ],
    },
    MenuDef {
        title: "View",
        items: &[
            MenuItem {
                label: "Pages…",
                hint: "",
                effect: MenuEffect::Action(Action::TogglePanel(PanelKind::Pages)),
            },
            MenuItem {
                label: "Workspace…",
                hint: "⌃⇧W",
                effect: MenuEffect::Action(Action::TogglePanel(PanelKind::Workspace)),
            },
            MenuItem {
                label: "Notes",
                hint: "F4",
                effect: MenuEffect::Action(Action::TogglePanel(PanelKind::Notes)),
            },
            MenuItem {
                label: "Word stats",
                hint: "F9",
                effect: MenuEffect::Action(Action::TogglePanel(PanelKind::Stats)),
            },
            MenuItem {
                label: "Revisions",
                hint: "F5",
                effect: MenuEffect::Action(Action::TogglePanel(PanelKind::Revisions)),
            },
            MenuItem {
                label: "Settings",
                hint: "F2",
                effect: MenuEffect::Action(Action::TogglePanel(PanelKind::Settings)),
            },
            MenuItem {
                label: "Line numbers",
                hint: "⌃K J",
                effect: MenuEffect::Action(Action::ToggleLineNumbers),
            },
            MenuItem {
                label: "Ruler",
                hint: "⌥R",
                effect: MenuEffect::Action(Action::ToggleRuler),
            },
            MenuItem {
                label: "Menu bar",
                hint: "⌥M",
                effect: MenuEffect::Action(Action::ToggleMenuBar),
            },
            MenuItem {
                label: "Zoom document",
                hint: "F11",
                effect: MenuEffect::Command(Command::ToggleZoom),
            },
            MenuItem {
                label: "Next theme",
                hint: "⌃T",
                effect: MenuEffect::Action(Action::CycleTheme),
            },
            MenuItem {
                label: "Theme picker…",
                hint: "⌃⇧T",
                effect: MenuEffect::Action(Action::OpenThemePrompt),
            },
        ],
    },
    MenuDef {
        title: "Insert",
        items: &[
            MenuItem {
                label: "Timestamp",
                hint: "⌥T",
                effect: MenuEffect::Command(Command::InsertTimestamp),
            },
            MenuItem {
                label: "Write block to file…",
                hint: "⌃K I",
                effect: MenuEffect::Action(Action::WriteBlockRequested),
            },
            MenuItem {
                label: "Insert file…",
                hint: "⌃K O",
                effect: MenuEffect::Action(Action::ReadFileRequested),
            },
        ],
    },
    MenuDef {
        title: "Format",
        items: &[
            MenuItem {
                label: "Align paragraph…",
                hint: "⌃L",
                effect: MenuEffect::Action(Action::OpenAlignmentPrompt),
            },
            MenuItem {
                label: "Center line",
                hint: "⌃O C",
                effect: MenuEffect::Command(Command::AlignCenter),
            },
            MenuItem {
                label: "Right line",
                hint: "⌃O R",
                effect: MenuEffect::Command(Command::AlignRight),
            },
            MenuItem {
                label: "Left line",
                hint: "⌃O L",
                effect: MenuEffect::Command(Command::AlignLeft),
            },
            MenuItem {
                label: "Justify",
                hint: "⌃O J",
                effect: MenuEffect::Command(Command::AlignJustify),
            },
            MenuItem {
                label: "Bold",
                hint: "⌥B",
                effect: MenuEffect::Command(Command::ToggleBold),
            },
            MenuItem {
                label: "Italic",
                hint: "⌥E",
                effect: MenuEffect::Command(Command::ToggleItalic),
            },
            MenuItem {
                label: "Title",
                hint: "",
                effect: MenuEffect::Command(Command::BlockTitle),
            },
            MenuItem {
                label: "Heading",
                hint: "",
                effect: MenuEffect::Command(Command::BlockHeading),
            },
            MenuItem {
                label: "Block Quote",
                hint: "",
                effect: MenuEffect::Command(Command::BlockQuote),
            },
            MenuItem {
                label: "Body",
                hint: "",
                effect: MenuEffect::Command(Command::BlockBody),
            },
            MenuItem {
                label: "Indent",
                hint: "Tab",
                effect: MenuEffect::Command(Command::InsertTab),
            },
            MenuItem {
                label: "Outdent",
                hint: "⇧Tab",
                effect: MenuEffect::Command(Command::Outdent),
            },
            MenuItem {
                label: "Change case",
                hint: "⌥U",
                effect: MenuEffect::Command(Command::CycleCase),
            },
            MenuItem {
                label: "Word target…",
                hint: "⌃G",
                effect: MenuEffect::Action(Action::OpenWordTargetPrompt),
            },
            MenuItem {
                label: "Set document status…",
                hint: "⌥S",
                effect: MenuEffect::Action(Action::OpenStatusPrompt),
            },
            MenuItem {
                label: "Accent color…",
                hint: "⌃K C",
                effect: MenuEffect::Action(Action::OpenCustomColorPrompt),
            },
        ],
    },
    MenuDef {
        title: "Page",
        items: &[
            MenuItem {
                label: "Next page",
                hint: "F6",
                effect: MenuEffect::Action(Action::NextPage),
            },
            MenuItem {
                label: "Previous page",
                hint: "⇧F6",
                effect: MenuEffect::Action(Action::PreviousPage),
            },
            MenuItem {
                label: "Move page up",
                hint: "⌥↑",
                effect: MenuEffect::Command(Command::MovePageUp),
            },
            MenuItem {
                label: "Move page down",
                hint: "⌥↓",
                effect: MenuEffect::Command(Command::MovePageDown),
            },
            MenuItem {
                label: "Close page",
                hint: "Pages ⌦",
                effect: MenuEffect::Action(Action::TrashRequested { confirm: false }),
            },
        ],
    },
    MenuDef {
        title: "Window",
        items: &[
            MenuItem {
                label: "Next window",
                hint: "⌥N",
                effect: MenuEffect::Command(Command::NextWindow),
            },
            MenuItem {
                label: "Close window",
                hint: "⌥W",
                effect: MenuEffect::Command(Command::CloseWindow),
            },
            MenuItem {
                label: "Move to slot 1",
                hint: "",
                effect: MenuEffect::Action(Action::MoveWindowToSlot(0)),
            },
            MenuItem {
                label: "Move to slot 2",
                hint: "",
                effect: MenuEffect::Action(Action::MoveWindowToSlot(1)),
            },
            MenuItem {
                label: "Move to slot 3",
                hint: "",
                effect: MenuEffect::Action(Action::MoveWindowToSlot(2)),
            },
            MenuItem {
                label: "Move to slot 4",
                hint: "",
                effect: MenuEffect::Action(Action::MoveWindowToSlot(3)),
            },
        ],
    },
    MenuDef {
        title: "Help",
        items: &[
            MenuItem {
                label: "Keyboard help",
                hint: "F1",
                effect: MenuEffect::Action(Action::TogglePanel(PanelKind::Help)),
            },
            MenuItem {
                label: concat!("About Ordet v", env!("CARGO_PKG_VERSION")),
                hint: "",
                effect: MenuEffect::Action(Action::About),
            },
        ],
    },
];

#[derive(Clone, Debug)]
pub enum AppMessage {
    SaveCompleted {
        /// The id, not the row index: pages can be reordered or closed
        /// between the spawn and this completion.
        draft_id: DraftId,
        path: PathBuf,
        revision: u64,
    },
    /// A quiet autosave landed. Handled like SaveCompleted minus the
    /// status chatter and quit/exit intents.
    AutosaveCompleted {
        draft_id: DraftId,
        path: PathBuf,
        revision: u64,
    },
    TrashCompleted {
        path: PathBuf,
        trashed_to: PathBuf,
    },
    OpenCompleted {
        path: PathBuf,
        buffer: Box<EditorBuffer>,
        metadata: crate::state::DocumentMetadata,
    },
    WriteBlockCompleted {
        path: PathBuf,
        chars: usize,
        block_only: bool,
    },
    RecoveryCompleted,
    HistoryPersisted,
    ReadFileCompleted {
        draft_id: DraftId,
        revision: u64,
        path: PathBuf,
        text: String,
        chars: usize,
    },
}

pub fn build_keymap() -> Keymap<Command> {
    let mut keymap = Keymap::new();
    keymap.bind(
        KeyChord::new(KeyCode::F(6), KeyModifiers::empty()),
        Command::NextPage,
    );
    keymap.bind(
        KeyChord::new(KeyCode::F(6), KeyModifiers::SHIFT),
        Command::PreviousPage,
    );
    keymap.bind(KeyChord::ctrl('t'), Command::CycleTheme);
    keymap.bind(
        KeyChord::new(
            KeyCode::Char('t'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        Command::OpenThemePrompt,
    );
    keymap.bind(
        KeyChord::new(KeyCode::Char('T'), KeyModifiers::CONTROL),
        Command::OpenThemePrompt,
    );
    // Save owns the universal chord (Ctrl-P stays as an alias). Movement
    // keeps arrows plus Ctrl-E; Down owns down, and Ctrl-X belongs to Cut.
    keymap.bind(KeyChord::ctrl('s'), Command::Save);
    // Ctrl-Shift-S is Save As; document status moves to Option-S so Ctrl-U
    // can stay with the terminal's line-kill convention.
    keymap.bind(
        KeyChord::new(
            KeyCode::Char('s'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        Command::OpenSavePathPrompt,
    );
    keymap.bind(
        KeyChord::new(KeyCode::Char('s'), KeyModifiers::ALT),
        Command::OpenStatusPrompt,
    );
    keymap.bind(KeyChord::ctrl('e'), Command::MoveUp);
    // Select-all owns Ctrl-A. Word navigation is the Option-Left/Right pair.
    keymap.bind(KeyChord::ctrl('a'), Command::SelectAll);
    // Ctrl-F is the universal Find; Option-Left/Right move by word.
    // Option-Up/Down stay on page reordering.
    keymap.bind(KeyChord::ctrl('f'), Command::OpenFindPrompt);
    keymap.bind(
        KeyChord::new(KeyCode::Left, KeyModifiers::ALT),
        Command::MoveWordLeft,
    );
    keymap.bind(
        KeyChord::new(KeyCode::Right, KeyModifiers::ALT),
        Command::MoveWordRight,
    );
    keymap.bind(KeyChord::ctrl('q'), Command::Quit);
    keymap.bind(KeyChord::ctrl('n'), Command::NewDraft);
    keymap.bind(KeyChord::ctrl('b'), Command::OpenFilePrompt);
    keymap.bind(KeyChord::ctrl('p'), Command::Save);
    keymap.bind(
        KeyChord::new(KeyCode::F(4), KeyModifiers::empty()),
        Command::ToggleNotes,
    );
    // No Ctrl chord moves left/right: arrows own movement.
    keymap.bind(
        KeyChord::new(KeyCode::Char('r'), KeyModifiers::ALT),
        Command::ToggleRuler,
    );
    // Option-M hides the top bar itself, for writers who want nothing but
    // ruler, text and status row on screen. F10 still reaches the menus.
    keymap.bind(
        KeyChord::new(KeyCode::Char('m'), KeyModifiers::ALT),
        Command::ToggleMenuBar,
    );
    // Windows: Option-N opens the next page until four windows are tiled,
    // then walks the desktop. Option-W closes the window that owns the
    // keyboard.
    keymap.bind(
        KeyChord::new(KeyCode::Char('n'), KeyModifiers::ALT),
        Command::NextWindow,
    );
    keymap.bind(
        KeyChord::new(KeyCode::Char('w'), KeyModifiers::ALT),
        Command::CloseWindow,
    );
    keymap.bind(KeyChord::ctrl('h'), Command::ToggleHelp);
    keymap.bind(
        KeyChord::new(KeyCode::F(1), KeyModifiers::empty()),
        Command::ToggleHelp,
    );
    keymap.bind(
        KeyChord::new(KeyCode::F(9), KeyModifiers::empty()),
        Command::ToggleStats,
    );
    keymap.bind(
        KeyChord::new(KeyCode::F(11), KeyModifiers::empty()),
        Command::ToggleZoom,
    );
    // F2 is terminal-safe; Ctrl punctuation is not consistently delivered by
    // terminal emulators or international keyboard layouts.
    keymap.bind(
        KeyChord::new(KeyCode::F(2), KeyModifiers::empty()),
        Command::ToggleSettings,
    );
    // Terminals differ on whether Ctrl-plus arrives as `+` with Shift or as
    // Ctrl-`=`. Treat each form as the same user-facing shortcut.
    // F5 is the terminal-safe primary for revisions.
    keymap.bind(
        KeyChord::new(KeyCode::F(5), KeyModifiers::empty()),
        Command::ToggleRevisions,
    );
    keymap.bind(
        KeyChord::new(KeyCode::Char('+'), KeyModifiers::CONTROL),
        Command::ToggleRevisions,
    );
    keymap.bind(
        KeyChord::new(
            KeyCode::Char('+'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        Command::ToggleRevisions,
    );
    keymap.bind(KeyChord::ctrl('='), Command::ToggleRevisions);
    keymap.bind(KeyChord::ctrl('g'), Command::OpenWordTargetPrompt);
    keymap.bind(KeyChord::ctrl('l'), Command::OpenAlignmentPrompt);
    // Copy/paste own the universal chords (the accent prompt keeps Ctrl-K
    // C). Cut owns Ctrl-X, with Alt-X kept as the writing-hand alias; Down
    // arrow is the only down-step now. Alt-V owns every case/Shift encoding
    // so the first press pastes no matter how the terminal reports it.
    keymap.bind(KeyChord::ctrl('c'), Command::CopySelection);
    keymap.bind(KeyChord::ctrl('x'), Command::CutSelection);
    keymap.bind(KeyChord::ctrl('v'), Command::PasteClipboard);
    keymap.bind(
        KeyChord::new(KeyCode::Char('V'), KeyModifiers::ALT),
        Command::PasteClipboard,
    );
    keymap.bind(
        KeyChord::new(KeyCode::Char('v'), KeyModifiers::ALT | KeyModifiers::SHIFT),
        Command::PasteClipboard,
    );
    keymap.bind(
        KeyChord::new(KeyCode::Char('V'), KeyModifiers::ALT | KeyModifiers::SHIFT),
        Command::PasteClipboard,
    );
    keymap.bind(KeyChord::plain(KeyCode::Left), Command::MoveLeft);
    keymap.bind(KeyChord::plain(KeyCode::Right), Command::MoveRight);
    keymap.bind(KeyChord::plain(KeyCode::Up), Command::MoveUp);
    keymap.bind(KeyChord::plain(KeyCode::Down), Command::MoveDown);
    keymap.bind(
        KeyChord::new(KeyCode::Left, KeyModifiers::SHIFT),
        Command::SelectLeft,
    );
    keymap.bind(
        KeyChord::new(KeyCode::Right, KeyModifiers::SHIFT),
        Command::SelectRight,
    );
    keymap.bind(
        KeyChord::new(KeyCode::Up, KeyModifiers::SHIFT),
        Command::SelectUp,
    );
    keymap.bind(
        KeyChord::new(KeyCode::Down, KeyModifiers::SHIFT),
        Command::SelectDown,
    );
    // Reorder pages: Option-arrows. The Pages panel's highlight wins when
    // that panel is open; otherwise the page being edited moves.
    keymap.bind(
        KeyChord::new(KeyCode::Up, KeyModifiers::ALT),
        Command::MovePageUp,
    );
    keymap.bind(
        KeyChord::new(KeyCode::Down, KeyModifiers::ALT),
        Command::MovePageDown,
    );
    keymap.bind(KeyChord::plain(KeyCode::Home), Command::MoveHome);
    keymap.bind(KeyChord::plain(KeyCode::End), Command::MoveEnd);
    keymap.bind(KeyChord::plain(KeyCode::PageUp), Command::PageUp);
    keymap.bind(KeyChord::plain(KeyCode::PageDown), Command::PageDown);
    keymap.bind(KeyChord::plain(KeyCode::Backspace), Command::Backspace);
    keymap.bind(KeyChord::plain(KeyCode::Delete), Command::DeleteForward);
    keymap.bind(KeyChord::plain(KeyCode::Enter), Command::Submit);
    keymap.bind(KeyChord::plain(KeyCode::Tab), Command::InsertTab);
    keymap.bind(
        KeyChord::new(KeyCode::Tab, KeyModifiers::SHIFT),
        Command::Outdent,
    );
    keymap.bind(
        KeyChord::new(KeyCode::Char('u'), KeyModifiers::ALT),
        Command::CycleCase,
    );
    keymap.bind(
        KeyChord::new(KeyCode::Char('b'), KeyModifiers::ALT),
        Command::ToggleBold,
    );
    keymap.bind(
        KeyChord::new(KeyCode::Char('e'), KeyModifiers::ALT),
        Command::ToggleItalic,
    );
    keymap.bind(
        KeyChord::new(KeyCode::Char('t'), KeyModifiers::ALT),
        Command::InsertTimestamp,
    );
    keymap.bind(
        KeyChord::new(KeyCode::Char('c'), KeyModifiers::ALT),
        Command::CopySelection,
    );
    keymap.bind(
        KeyChord::new(KeyCode::Char('x'), KeyModifiers::ALT),
        Command::CutSelection,
    );
    keymap.bind(
        KeyChord::new(KeyCode::Char('v'), KeyModifiers::ALT),
        Command::PasteClipboard,
    );
    keymap.bind(KeyChord::ctrl('z'), Command::Undo);
    keymap.bind(
        KeyChord::new(
            KeyCode::Char('z'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        Command::Redo,
    );
    keymap.bind(
        KeyChord::new(KeyCode::Char('Z'), KeyModifiers::CONTROL),
        Command::Redo,
    );
    keymap.bind(KeyChord::ctrl('y'), Command::DeleteLine);
    keymap.bind(KeyChord::plain(KeyCode::Insert), Command::ToggleInsertMode);
    keymap.bind(
        KeyChord::new(
            KeyCode::Char('o'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        Command::OpenLine,
    );
    keymap.bind(
        KeyChord::new(
            KeyCode::Char('k'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ),
        Command::DeleteToEndOfLine,
    );
    keymap.bind(
        KeyChord::new(
            KeyCode::Char('j'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        Command::JumpPreviousPosition,
    );
    keymap.bind(
        KeyChord::new(KeyCode::PageUp, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
        Command::MoveWindowTop,
    );
    keymap.bind(
        KeyChord::new(
            KeyCode::PageDown,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        Command::MoveWindowBottom,
    );
    keymap.bind(
        KeyChord::new(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        Command::MoveToBlockStart,
    );
    keymap.bind(
        KeyChord::new(
            KeyCode::Char('k'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        Command::MoveToBlockEnd,
    );
    keymap.bind(
        KeyChord::new(
            KeyCode::Char('w'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        Command::OpenWorkspacePrompt,
    );
    keymap.bind(
        KeyChord::new(KeyCode::Delete, KeyModifiers::CONTROL),
        Command::DeleteWordForward,
    );
    keymap.bind(
        KeyChord::new(KeyCode::Delete, KeyModifiers::ALT),
        Command::DeleteWordForward,
    );
    keymap.bind(
        KeyChord::new(KeyCode::Backspace, KeyModifiers::CONTROL),
        Command::DeleteWordLeft,
    );
    keymap.bind(
        KeyChord::new(KeyCode::Backspace, KeyModifiers::ALT),
        Command::DeleteWordLeft,
    );
    keymap.bind(
        KeyChord::new(KeyCode::Home, KeyModifiers::CONTROL),
        Command::MoveDocumentTop,
    );
    keymap.bind(
        KeyChord::new(KeyCode::End, KeyModifiers::CONTROL),
        Command::MoveDocumentBottom,
    );
    keymap
}

/// The chord that opens the command prefix.
pub fn prefix_leader() -> KeyChord {
    KeyChord::new(KeyCode::Char('k'), KeyModifiers::CONTROL)
}

/// Second-stroke commands reached through the `Ctrl-K` prefix.
///
/// The prefix keeps the rest of the command surface off single chords.
/// Commands that already own a comfortable single chord stay there, so this
/// layer is reserved for the remainder.
pub fn build_prefix_keymap() -> Keymap<Command> {
    let mut keymap = Keymap::new();
    for (letter, command) in PREFIX_COMMANDS {
        keymap.bind(KeyChord::plain(KeyCode::Char(*letter)), *command);
    }
    keymap
}

/// Every `Ctrl-K` stroke paired with the command it runs.
pub const PREFIX_COMMANDS: &[(char, Command)] = &[
    ('a', Command::SelectAll),
    ('b', Command::MarkBlockBegin),
    ('c', Command::OpenCustomColorPrompt),
    ('e', Command::OpenReplacePrompt),
    ('f', Command::OpenFindPrompt),
    ('g', Command::OpenWordTargetPrompt),
    ('h', Command::SaveAndClose),
    ('i', Command::OpenWriteBlockPrompt),
    ('j', Command::ToggleLineNumbers),
    ('k', Command::MarkBlockEnd),
    ('l', Command::OpenGoToLinePrompt),
    ('m', Command::MoveBlock),
    ('n', Command::ToggleNotes),
    ('o', Command::OpenReadFilePrompt),
    ('p', Command::OpenSavePathPrompt),
    ('q', Command::Quit),
    ('r', Command::TrashFile),
    ('s', Command::Save),
    ('t', Command::OpenThemePrompt),
    ('u', Command::HideBlock),
    ('v', Command::CopyBlock),
    ('w', Command::ToggleStats),
    ('x', Command::SaveAndExit),
    ('y', Command::FindNext),
    ('z', Command::DeleteBlock),
];

/// What each `Ctrl-K` stroke does, kept beside [`PREFIX_COMMANDS`] for the
/// keymap parity test. The Help panel intentionally shows only common
/// commands instead of dumping this A–Z list.
#[cfg(test)]
pub const PREFIX_COMMAND_HELP: &[(&str, &str)] = &[
    ("⌃K A", "select all"),
    ("⌃K B", "mark block start"),
    ("⌃K C", "accent color"),
    ("⌃K E", "find and replace"),
    ("⌃K F", "find"),
    ("⌃K G", "word target"),
    ("⌃K H", "save and close draft"),
    ("⌃K I", "write block to file"),
    ("⌃K J", "toggle line numbers"),
    ("⌃K K", "mark block end"),
    ("⌃K L", "go to line"),
    ("⌃K M", "move block"),
    ("⌃K N", "toggle notes"),
    ("⌃K O", "read file at cursor"),
    ("⌃K P", "save as"),
    ("⌃K Q", "quick jump"),
    ("⌃K R", "move file to Trash"),
    ("⌃K S", "save"),
    ("⌃K T", "theme picker"),
    ("⌃K U", "clear block"),
    ("⌃K V", "copy block"),
    ("⌃K W", "word stats"),
    ("⌃K X", "save and exit"),
    ("⌃K Y", "find next"),
    ("⌃K Z", "delete block"),
];

/// `Ctrl-O`, the formatting prefix: quick line alignment without a prompt.
pub fn o_prefix_leader() -> KeyChord {
    KeyChord::new(KeyCode::Char('o'), KeyModifiers::CONTROL)
}

/// Second-stroke commands reached through the `Ctrl-O` prefix.
pub fn build_o_prefix_keymap() -> Keymap<Command> {
    let mut keymap = Keymap::new();
    for (letter, command) in O_PREFIX_COMMANDS {
        keymap.bind(KeyChord::plain(KeyCode::Char(*letter)), *command);
    }
    keymap
}

/// Every `Ctrl-O` stroke paired with the command it runs.
pub const O_PREFIX_COMMANDS: &[(char, Command)] = &[
    ('c', Command::AlignCenter),
    ('j', Command::AlignJustify),
    ('l', Command::AlignLeft),
    ('r', Command::AlignRight),
];

/// What each `Ctrl-O` stroke does, kept beside [`O_PREFIX_COMMANDS`] for the
/// keymap parity test. The Help panel consolidates alignment onto one row.
#[cfg(test)]
pub const O_PREFIX_COMMAND_HELP: &[(&str, &str)] = &[
    ("⌃O C", "center line"),
    ("⌃O J", "justify paragraph"),
    ("⌃O L", "left-align line"),
    ("⌃O R", "right-align line"),
];

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ordet_tui_framework::KeyChord;

    use super::{
        build_keymap, build_o_prefix_keymap, build_prefix_keymap, o_prefix_leader, prefix_leader,
        Command, MENU_BAR, O_PREFIX_COMMANDS, O_PREFIX_COMMAND_HELP, PREFIX_COMMANDS,
        PREFIX_COMMAND_HELP,
    };
    use std::collections::HashSet;

    #[test]
    fn movement_and_direct_shortcuts_resolve() {
        let keymap = build_keymap();

        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            Some(Command::Save)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            Some(Command::Save)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL)),
            Some(Command::MoveUp)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)),
            Some(Command::CutSelection)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Down, KeyModifiers::empty())),
            Some(Command::MoveDown)
        );
        // Option-Left/Right repeat the word motions; Option-Up/Down reorder
        // pages from anywhere.
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT)),
            Some(Command::MoveWordLeft)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT)),
            Some(Command::MoveWordRight)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT)),
            Some(Command::MovePageUp)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Down, KeyModifiers::ALT)),
            Some(Command::MovePageDown)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            Some(Command::SelectAll)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL)),
            Some(Command::OpenFindPrompt)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL)),
            Some(Command::CycleTheme)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)),
            None,
            "⌃O reaches the on-screen formatting prefix, not the main keymap"
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::F(6), KeyModifiers::empty())),
            Some(Command::NextPage)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::F(6), KeyModifiers::SHIFT)),
            Some(Command::PreviousPage)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::ALT)),
            Some(Command::ToggleRuler)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT)),
            Some(Command::ToggleMenuBar)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT)),
            Some(Command::NextWindow)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::ALT)),
            Some(Command::CloseWindow)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)),
            Some(Command::Quit)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            Some(Command::NewDraft)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)),
            Some(Command::OpenFilePrompt)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            Some(Command::Save)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
            None,
            "Ctrl-U is left to the terminal"
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(
                KeyCode::Char('s'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )),
            Some(Command::OpenSavePathPrompt)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::ALT)),
            Some(Command::OpenStatusPrompt)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)),
            None,
            "Ctrl-R is left to the file browser's rename"
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL)),
            Some(Command::PasteClipboard)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::F(4), KeyModifiers::empty())),
            Some(Command::ToggleNotes)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT)),
            None
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL)),
            Some(Command::ToggleHelp)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL)),
            None,
            "Ctrl-W is left to delete-word/close-window conventions"
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::F(9), KeyModifiers::empty())),
            Some(Command::ToggleStats)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL)),
            Some(Command::OpenWordTargetPrompt)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL)),
            Some(Command::OpenAlignmentPrompt)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Command::CopySelection)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::CONTROL)),
            None,
            "Ctrl-Shift-A is gone; Option-Left/Right are the word pair"
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(
                KeyCode::Char('t'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )),
            Some(Command::OpenThemePrompt)
        );
    }

    #[test]
    fn terminal_safe_undo_and_redo_bindings_resolve() {
        let keymap = build_keymap();

        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL)),
            Some(Command::Undo)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(
                KeyCode::Char('z'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )),
            Some(Command::Redo)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::SUPER)),
            None
        );
    }

    #[test]
    fn indentation_and_writing_shortcuts_resolve() {
        let keymap = build_keymap();

        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::ALT)),
            Some(Command::CopySelection)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT)),
            Some(Command::CutSelection)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::ALT)),
            Some(Command::PasteClipboard)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::F(1), KeyModifiers::empty())),
            Some(Command::ToggleHelp)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::F(2), KeyModifiers::empty())),
            Some(Command::ToggleSettings)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('+'), KeyModifiers::CONTROL)),
            Some(Command::ToggleRevisions)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(
                KeyCode::Char('+'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )),
            Some(Command::ToggleRevisions)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('='), KeyModifiers::CONTROL)),
            Some(Command::ToggleRevisions)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
            Some(Command::InsertTab)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT)),
            Some(Command::Outdent)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::ALT)),
            Some(Command::CycleCase)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT)),
            Some(Command::ToggleBold)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::ALT)),
            Some(Command::ToggleItalic)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::ALT)),
            Some(Command::InsertTimestamp)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(
                KeyCode::Char('o'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )),
            Some(Command::OpenLine)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(
                KeyCode::Char('b'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )),
            Some(Command::MoveToBlockStart)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(
                KeyCode::Char('k'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )),
            Some(Command::MoveToBlockEnd)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(
                KeyCode::Char('k'),
                KeyModifiers::CONTROL | KeyModifiers::ALT
            )),
            Some(Command::DeleteToEndOfLine)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(
                KeyCode::PageUp,
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )),
            Some(Command::MoveWindowTop)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(
                KeyCode::PageDown,
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )),
            Some(Command::MoveWindowBottom)
        );
    }

    #[test]
    fn classic_shortcuts_resolve() {
        let keymap = build_keymap();

        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL)),
            Some(Command::DeleteLine)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Insert, KeyModifiers::NONE)),
            Some(Command::ToggleInsertMode)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Delete, KeyModifiers::CONTROL)),
            Some(Command::DeleteWordForward)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Delete, KeyModifiers::ALT)),
            Some(Command::DeleteWordForward)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL)),
            Some(Command::DeleteWordLeft)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT)),
            Some(Command::DeleteWordLeft)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL)),
            Some(Command::MoveDocumentTop)
        );
        assert_eq!(
            keymap.resolve(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL)),
            Some(Command::MoveDocumentBottom)
        );
    }

    /// Every stroke in the prefix reference must actually resolve, and every
    /// prefix command must be documented, so the dormant table cannot drift.
    #[test]
    fn prefix_help_matches_keymap() {
        let keymap = build_prefix_keymap();

        let mut documented = HashSet::new();
        for (keys, _) in PREFIX_COMMAND_HELP {
            let letter = keys
                .rsplit(' ')
                .next()
                .expect("each help row ends with a key")
                .chars()
                .next()
                .expect("each help row ends with a letter")
                .to_ascii_lowercase();
            assert!(documented.insert(letter), "⌃K {letter} is documented twice");
            assert_eq!(
                keymap.resolve(KeyEvent::new(
                    KeyCode::Char(letter.to_ascii_lowercase()),
                    KeyModifiers::NONE
                )),
                Some(
                    PREFIX_COMMANDS
                        .iter()
                        .find(|(candidate, _)| *candidate == letter)
                        .map(|(_, command)| *command)
                        .expect("documented stroke is bound")
                ),
                "⌃K {letter} is documented but resolves elsewhere"
            );
        }

        for (letter, _) in PREFIX_COMMANDS {
            assert!(
                documented.contains(letter),
                "⌃K {letter} is bound but not documented"
            );
        }
    }

    #[test]
    fn o_prefix_has_the_alignment_strokes() {
        let keymap = build_o_prefix_keymap();
        assert_eq!(
            o_prefix_leader(),
            KeyChord::new(KeyCode::Char('o'), KeyModifiers::CONTROL)
        );
        assert_eq!(O_PREFIX_COMMAND_HELP.len(), O_PREFIX_COMMANDS.len());
        for ((keys, description), (letter, command)) in
            O_PREFIX_COMMAND_HELP.iter().zip(O_PREFIX_COMMANDS)
        {
            assert!(!description.is_empty());
            let hinted = keys.rsplit(' ').next().expect("each row ends with a key");
            assert_eq!(
                hinted.to_ascii_lowercase(),
                letter.to_string(),
                "⌃O {letter} is documented as {keys:?}"
            );
            assert_eq!(
                keymap.resolve(KeyEvent::new(KeyCode::Char(*letter), KeyModifiers::NONE)),
                Some(*command),
                "⌃O {letter} is documented but resolves elsewhere"
            );
        }
    }

    #[test]
    fn menus_do_not_repeat_actions() {
        let menu = |title: &str| {
            MENU_BAR
                .iter()
                .find(|menu| menu.title == title)
                .expect("menu exists")
        };
        let page = menu("Page");
        let labels: Vec<&str> = page.items.iter().map(|item| item.label).collect();
        assert_eq!(
            labels,
            vec![
                "Next page",
                "Previous page",
                "Move page up",
                "Move page down",
                "Close page"
            ]
        );
        let help = menu("Help");
        assert_eq!(help.items[0].label, "Keyboard help");
        assert!(
            help.items
                .iter()
                .any(|item| item.label.starts_with("About Ordet v")),
            "Help needs an About row"
        );

        let mut seen: Vec<(&str, &str, String)> = Vec::new();
        for menu in MENU_BAR {
            for item in menu.items {
                let effect = format!("{:?}", item.effect);
                if let Some((other_menu, other_label, _)) =
                    seen.iter().find(|(_, _, other)| *other == effect)
                {
                    panic!(
                        "{} > {} repeats {} > {}",
                        menu.title, item.label, other_menu, other_label
                    );
                }
                seen.push((menu.title, item.label, effect));
            }
        }
    }

    #[test]
    fn prefix_leader_is_not_also_a_plain_binding() {
        assert_eq!(
            prefix_leader(),
            KeyChord::new(KeyCode::Char('k'), KeyModifiers::CONTROL)
        );
        assert_eq!(
            build_keymap().resolve(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL)),
            None,
            "⌃K must reach the prefix handler, not the main keymap"
        );
    }
}
