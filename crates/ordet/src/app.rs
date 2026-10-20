use std::fs;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command as ProcessCommand;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ordet_format::{
    read_from_path, write_bytes_atomically, write_to_path, FormatError, OrdetDocument,
};
use ordet_tui_framework::{
    Component, EventCx, KeyChord, Keymap, Point, Rect, TaskError, TaskKey, TaskPool, TaskResult,
    Terminal, UiEvent,
};

use crate::commands::{
    build_keymap, build_o_prefix_keymap, build_prefix_keymap, o_prefix_leader, prefix_leader,
    Action, AppMessage, Command,
};
use crate::components::{
    help_jump_target, help_panel_navigation, panel_row_at_point, window_text_area, wrap_and_gutter,
    RootView,
};
use crate::editor_buffer::DraftSnapshot;
use crate::editor_buffer::{EditorBuffer, ParagraphAlignment, SearchOptions};
use crate::file_browser::{FileBrowserAction, FileBrowserMode, FileBrowserState};
use crate::history::{history_key_for, load_history, relative_time, save_history, unix_now};
use crate::i18n::UiLanguage;
use crate::prompt::{PromptKind, PromptState};
use crate::recovery::{clear_recovery, load_recovery, save_recovery, RecoveryFile};
use crate::settings::{load_settings, save_settings, settings_path, AppSettings};
use crate::state::{
    normalize_document_path, panel_content_rect, paths_match, AppState, DocumentMetadata, DraftId,
    PanelKind, PrefixMode, QuitConfirmation, SettingsPickerKind, SettingsPickerState,
};
use crate::theme::{ThemePreset, ThemeSelection};

/// Housekeeping cadence for time-derived UI (clock, status expiry) and the
/// periodic disk work (recovery, autosave). This is not a redraw timer: a
/// tick only asks for a frame when one of those visible values changed.
const TICK_INTERVAL: Duration = Duration::from_millis(250);

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match parse_cli_args(&args) {
        CliAction::Run {
            files,
            config,
            latency_log,
        } => {
            crate::latency::enable_from_flag(latency_log);
            // Startup is single-threaded; no task has spawned yet.
            apply_config_override(&config);
            run_with_files(files)
        }
        CliAction::Help => {
            print!("{CLI_HELP}");
            Ok(())
        }
        CliAction::Version => {
            println!("ordet {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        CliAction::PrintConfig { config } => print_config(config),
        CliAction::ResetConfig { config } => reset_config(config),
        CliAction::UsageError(message) => {
            eprintln!("{message}\n\n{CLI_HELP}");
            std::process::exit(2);
        }
    }
}

const CLI_HELP: &str = "\
Ordet - a terminal-first word processor.

Usage:
  ordet [options] [--] [file ...]

Options:
  --config <path>     Use this settings file instead of the default.
  --latency-log       Print keystroke latency percentiles (stderr) on exit.
  --print-config      Print the effective settings as JSON and exit.
  --reset-config      Write default settings (to --config, or the default
                      path) and exit. Overwrites without asking.
  -h, --help          Show this help and exit.
  -V, --version       Show the version and exit.

Anything else opens as a draft. Prefix keys with `--` to open a file whose
name starts with a dash. Inside the app, F1 opens the tabbed shortcut
guide, and Ctrl-Shift-W tracks a folder in the workspace.
";

#[derive(Clone, Debug, PartialEq, Eq)]
enum CliAction {
    Run {
        files: Vec<String>,
        config: Option<String>,
        latency_log: bool,
    },
    Help,
    Version,
    PrintConfig {
        config: Option<String>,
    },
    ResetConfig {
        config: Option<String>,
    },
    UsageError(String),
}

fn parse_cli_args(args: &[String]) -> CliAction {
    let mut files = Vec::new();
    let mut config = None;
    let mut latency_log = false;
    let mut only_files = false;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if only_files {
            files.push(arg.clone());
        } else if arg == "--" {
            only_files = true;
        } else if arg == "--help" || arg == "-h" {
            return CliAction::Help;
        } else if arg == "--version" || arg == "-V" {
            return CliAction::Version;
        } else if arg == "--latency-log" {
            latency_log = true;
        } else if arg == "--print-config" {
            return CliAction::PrintConfig { config };
        } else if arg == "--reset-config" {
            return CliAction::ResetConfig { config };
        } else if arg == "--config" {
            index += 1;
            match args.get(index) {
                Some(path) => config = Some(path.clone()),
                None => return CliAction::UsageError("--config needs a path.".to_string()),
            }
        } else if let Some(path) = arg.strip_prefix("--config=") {
            if path.is_empty() {
                return CliAction::UsageError("--config needs a path.".to_string());
            }
            config = Some(path.to_string());
        } else if arg.starts_with("--") {
            return CliAction::UsageError(format!("Unknown option {arg}."));
        } else {
            files.push(arg.clone());
        }
        index += 1;
    }
    CliAction::Run {
        files,
        config,
        latency_log,
    }
}

/// Point the settings lookup at `config` for this process, so `--config`
/// applies to the management commands too.
fn apply_config_override(config: &Option<String>) {
    if let Some(path) = config {
        unsafe {
            std::env::set_var("ORDET_TUI_SETTINGS_PATH", path);
        }
    }
}

fn print_config(config: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    apply_config_override(&config);
    let path = settings_path().map_err(std::io::Error::other)?;
    let settings = load_settings().unwrap_or_default();
    println!("{}", serde_json::to_string_pretty(&settings)?);
    println!("\nConfig file: {}", path.display());
    Ok(())
}

fn reset_config(config: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    apply_config_override(&config);
    let path = settings_path().map_err(std::io::Error::other)?;
    save_settings(&AppSettings::default()).map_err(std::io::Error::other)?;
    println!("Wrote default settings to {}.", path.display());
    Ok(())
}

fn run_with_files(files: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    install_signal_restore_hook();
    install_terminal_panic_hook();
    let mut app = OrdetApp::from_args(files)?;
    let mut terminal = Terminal::enter()?;
    let result = app.run(&mut terminal);
    let clear_result = terminal.restore();
    clear_result?;
    result
}

/// Restore the terminal before a panic unwinds. Background task panics are
/// already caught and reported by the task pool, so their hook stays silent
/// instead of painting a panic over the TUI.
fn install_terminal_panic_hook() {
    let main_thread = std::thread::current().id();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        if thread
            .name()
            .map(|name| name.starts_with("ordet-task-"))
            .unwrap_or(false)
        {
            return;
        }
        if thread.id() == main_thread {
            Terminal::emergency_restore();
        }
        default_hook(info);
    }));
}

/// Raw mode turns Ctrl-C into Copy, but an external SIGINT/SIGTERM/SIGHUP
/// still terminates the process without unwinding. Restore the screen and
/// mouse mode from the signal thread before exiting.
#[cfg(unix)]
fn install_signal_restore_hook() {
    use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;

    let Ok(mut signals) = Signals::new([SIGINT, SIGTERM, SIGHUP]) else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("ordet-signal".to_string())
        .spawn(move || {
            if let Some(signal) = signals.forever().next() {
                Terminal::emergency_restore();
                std::process::exit(128 + signal);
            }
        });
}

#[cfg(target_os = "windows")]
fn install_signal_restore_hook() {
    let _ = ctrlc::set_handler(|| {
        Terminal::emergency_restore();
        std::process::exit(130);
    });
}

#[cfg(not(any(unix, target_os = "windows")))]
fn install_signal_restore_hook() {}

/// How long to wait after an edit before rewriting the recovery snapshot.
const RECOVERY_INTERVAL: Duration = Duration::from_secs(3);

/// Idle delay between autosave sweeps. Every word lands on disk shortly
/// after typing stops; the recovery snapshot covers the gap.
const AUTOSAVE_DELAY: Duration = Duration::from_secs(2);

struct OrdetApp {
    state: AppState,
    keymap: Keymap<Command>,
    prefix_keymap: Keymap<Command>,
    o_prefix_keymap: Keymap<Command>,
    root: RootView,
    tasks: TaskPool<AppMessage>,
    next_tick: Instant,
    /// Rendering is event-driven: input, completed work, a resize, or a
    /// visible clock/status change asks for a frame. There is no frame cap;
    /// a requested frame draws immediately.
    redraw_requested: bool,
    pending_after_save: CloseIntent,
    next_recovery_write: Instant,
    next_autosave_at: Instant,
    recovered_at_startup: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseIntent {
    None,
    CloseDraft,
    Exit,
}

#[derive(Debug)]
struct LoadedDraft {
    buffer: EditorBuffer,
    metadata: DocumentMetadata,
}

impl OrdetApp {
    fn from_args(args: Vec<String>) -> Result<Self, Box<dyn std::error::Error>> {
        let settings_file_exists = settings_path().map(|path| path.exists()).unwrap_or(false);
        let settings = load_settings();
        let (
            (
                theme,
                word_target,
                recent_files,
                ui_language,
                workspaces,
                show_line_numbers,
                show_ruler,
                show_menu_bar,
                autosave_enabled,
                indent_width,
                show_live_counts,
                backup_enabled,
                backup_on_close,
                backup_keep,
                backup_dir,
            ),
            settings_status,
        ) = match settings {
            Ok(settings) => (
                (
                    settings.theme.sanitize(),
                    settings.word_target,
                    settings.recent_files,
                    settings.ui_language,
                    settings.workspaces,
                    settings.show_line_numbers,
                    settings.show_ruler,
                    settings.show_menu_bar,
                    settings.autosave_enabled,
                    sanitize_indent_width(settings.indent_width),
                    settings.show_live_counts,
                    settings.backup_enabled,
                    settings.backup_on_close,
                    settings.backup_keep.max(1),
                    settings.backup_dir,
                ),
                None,
            ),
            Err(error) => (
                (
                    ThemeSelection::default(),
                    None,
                    Vec::new(),
                    UiLanguage::English,
                    Vec::new(),
                    false,
                    true,
                    true,
                    true,
                    4,
                    true,
                    true,
                    true,
                    5,
                    String::new(),
                ),
                Some(format!("Settings fallback: {error}")),
            ),
        };
        let mut startup_status = settings_status;

        let mut drafts = Vec::new();
        let mut draft_metadata = Vec::new();
        if !args.is_empty() {
            for raw_path in args {
                let path = normalize_document_path(expand_path(PathBuf::from(raw_path)));
                if path.exists() {
                    let loaded = load_buffer_from_path(&path)?;
                    draft_metadata.push(loaded.metadata.clone());
                    drafts.push((loaded.buffer, Some(path)));
                } else {
                    if startup_status.is_none() {
                        startup_status = Some(format!(
                            "{} did not exist. Opened a new unsaved draft instead.",
                            path.display()
                        ));
                    }
                    draft_metadata.push(DocumentMetadata::default());
                    drafts.push((EditorBuffer::new(), None));
                }
            }
        }

        // Anything the last session left unsaved comes back in place of a lone
        // empty scratch draft, otherwise after the files this launch opened, so
        // a crash costs at most the edits since the last snapshot.
        let recovered = match load_recovery() {
            Ok(file) => file.drafts,
            Err(error) => {
                if startup_status.is_none() {
                    startup_status = Some(format!("Recovery snapshot ignored: {error}"));
                }
                Vec::new()
            }
        };
        let recovered_count = recovered.len();
        let mut recovered_start = 0usize;
        if recovered_count > 0 {
            let mut restored = Vec::new();
            for draft in recovered {
                let mut buffer = EditorBuffer::from_text(&draft.text);
                buffer.move_cursor_to_end();
                restored.push((buffer, draft.path));
            }
            if drafts.len() == 1 && is_empty_scratch(&drafts) {
                drafts.clear();
            }
            recovered_start = drafts.len();
            drafts.extend(restored);
            draft_metadata.resize(drafts.len(), DocumentMetadata::default());
        }

        let loaded_recent_files = recent_files;
        let merged_recent_files = merge_recent_paths(
            loaded_recent_files.clone(),
            drafts
                .iter()
                .filter_map(|(_, path)| path.clone())
                .filter(|path| path.exists()),
        );

        let mut state = AppState::new(
            drafts,
            merged_recent_files,
            theme,
            word_target,
            current_local_hour(),
            detect_system_dark_mode(),
        );
        for (index, metadata) in draft_metadata.into_iter().enumerate() {
            state.set_draft_metadata(index, metadata);
        }

        for index in recovered_start..recovered_start + recovered_count {
            state.mark_draft_dirty(index);
        }
        if recovered_count > 0 {
            let count = recovered_count;
            startup_status = Some(match startup_status {
                Some(existing) => format!("{existing} Recovered {count} unsaved draft(s)."),
                None => format!("Recovered {count} unsaved draft(s) from the last session."),
            });
        }
        state.ui.language = ui_language;
        state.ui.show_line_numbers = show_line_numbers;
        state.ui.show_ruler = show_ruler;
        state.ui.show_menu_bar = show_menu_bar;
        state.ui.autosave_enabled = autosave_enabled;
        state.ui.indent_width = indent_width;
        state.ui.show_live_counts = show_live_counts;
        state.ui.backup_enabled = backup_enabled;
        state.ui.backup_on_close = backup_on_close;
        state.ui.backup_keep = backup_keep.max(1);
        state.ui.backup_dir = backup_dir;
        for editor in state.drafts.iter_mut().map(|d| &mut d.editor) {
            editor.buffer.set_indent_width(indent_width);
        }
        // History is best-effort: a corrupt file starts empty and is replaced
        // on the next save rather than nagging every launch.
        state.history = load_history().unwrap_or_default();
        // Folders that no longer exist fall off the workspace quietly.
        state.workspace_folders = workspaces
            .into_iter()
            .map(crate::state::normalize_document_path)
            .filter(|path| path.is_dir())
            .map(crate::workspace::WorkspaceFolder::new)
            .collect();
        state.ensure_welcome_selection();
        if let Some(status) = startup_status {
            state.set_status(status);
        }
        let recent_changed = state.recent_files != loaded_recent_files;
        if recent_changed || !settings_file_exists {
            if let Err(error) = save_settings(&AppSettings {
                theme: state.ui.theme.clone(),
                word_target: state.ui.word_target,
                recent_files: state.recent_files.clone(),
                ui_language: state.ui.language,
                show_line_numbers: state.ui.show_line_numbers,
                show_ruler: state.ui.show_ruler,
                show_menu_bar: state.ui.show_menu_bar,
                autosave_enabled: state.ui.autosave_enabled,
                indent_width: state.ui.indent_width,
                show_live_counts: state.ui.show_live_counts,
                backup_enabled: state.ui.backup_enabled,
                backup_on_close: state.ui.backup_on_close,
                backup_keep: state.ui.backup_keep,
                backup_dir: state.ui.backup_dir.clone(),
                workspaces: state
                    .workspace_folders
                    .iter()
                    .map(|folder| folder.path.clone())
                    .collect(),
            }) {
                if state.ui.status.is_empty() {
                    state.set_status(format!("Settings fallback: {error}"));
                }
            }
        }

        let now = Instant::now();
        Ok(Self {
            state,
            keymap: build_keymap(),
            prefix_keymap: build_prefix_keymap(),
            o_prefix_keymap: build_o_prefix_keymap(),
            root: RootView::new(),
            tasks: TaskPool::new(),
            next_tick: now + TICK_INTERVAL,
            redraw_requested: true,
            pending_after_save: CloseIntent::None,
            next_recovery_write: now + RECOVERY_INTERVAL,
            next_autosave_at: now + AUTOSAVE_DELAY,
            recovered_at_startup: recovered_count,
        })
    }

    fn run(&mut self, terminal: &mut Terminal) -> Result<(), Box<dyn std::error::Error>> {
        // Latency-log anchor: the moment input woke the loop. `None` means
        // this frame was caused by housekeeping, not by a keystroke.
        let mut input_at: Option<Instant> = None;
        // Time the last input batch spent in `event::read` + dispatch, so the
        // latency report can show the keystroke's two halves. Zero when the
        // frame is not answering an input batch.
        let mut handle_split = Duration::ZERO;
        // Time in `event::read` itself (transport) and how many queued events
        // one wakeup swallowed, so the report can separate transport from app.
        let mut read_split = Duration::ZERO;
        let mut batch_count = 1u32;

        loop {
            // A frame is never delayed, only coalesced: every event that
            // arrived while the last frame was being painted is handled
            // before this draw, and nothing here sleeps toward a frame cap.
            if self.redraw_requested {
                let draw_started = Instant::now();
                self.draw_frame(terminal)?;
                let draw_split = draw_started.elapsed();
                self.redraw_requested = false;
                if let Some(at) = input_at.take() {
                    // Whole span for the keystroke: wakeup through render,
                    // ANSI write, and the flush that `draw_frame` ends with.
                    crate::latency::record_parts(
                        at.elapsed(),
                        read_split,
                        handle_split,
                        draw_split,
                        batch_count,
                    );
                }

                if self.state.ui.should_quit {
                    break;
                }
            }

            let (deadlines, count) = self.next_deadlines();
            self.service_due_deadlines();
            if self.redraw_requested {
                continue;
            }

            let wait = deadlines
                .iter()
                .take(count)
                .map(|at| at.saturating_duration_since(Instant::now()))
                .min()
                .unwrap_or(Duration::ZERO);

            if event::poll(wait)? {
                // Drain the batch before deciding to draw: a burst of keys
                // becomes one frame. The timestamp is what the latency log
                // measures from, so it is taken the moment input wakes us.
                // (When another deadline was the wakeup, the timestamp is
                // overwritten by the next input before it is read.)
                input_at = Some(Instant::now());
                let read_started = Instant::now();
                let first_event = event::read()?;
                read_split = read_started.elapsed();
                let handle_started = Instant::now();
                let resized = self.handle_terminal_event(first_event);
                if let Some(area) = resized {
                    terminal.update_size(area);
                }
                self.redraw_requested = true;
                let mut batch = 1u32;
                while event::poll(Duration::ZERO)? {
                    batch += 1;
                    let resized = self.handle_terminal_event(event::read()?);
                    if let Some(area) = resized {
                        terminal.update_size(area);
                    }
                    self.redraw_requested = true;
                }
                handle_split = handle_started.elapsed();
                batch_count = batch;
            }
        }

        // Reaching here means the user chose to quit, so whatever was still
        // unsaved was discarded on purpose. Leaving the snapshot behind would
        // resurrect it on the next launch. Nothing can be reported to the user
        // at this point, so a failure here is left to the next launch to clear.
        let _ = clear_recovery();
        let _ = self.persist_settings();
        crate::latency::report_final();

        Ok(())
    }

    fn draw_frame(&mut self, terminal: &mut Terminal) -> Result<(), Box<dyn std::error::Error>> {
        let area = terminal.size()?;
        self.state.update_layout(area);
        self.refresh_window_views();
        // The Notes panel lives on the same buffer model as the editor; a
        // resize while it is open must keep its cursor in view.
        if self.state.panels.is(PanelKind::Notes) {
            let content = panel_content_rect(&self.state);
            let (notes_width, _) = wrap_and_gutter(
                content.width as usize,
                &self.state.notes().buffer,
                self.state.ui.show_line_numbers,
            );
            self.state
                .notes_mut()
                .ensure_cursor_visible(notes_width, content.height.max(1) as usize);
        }

        terminal.draw(|frame| {
            self.root.render(&self.state, frame, area, false);
            // A dropdown, picker, browser, or a panel that does not own the
            // text cursor would otherwise let it blink through the overlay.
            let cursor_owned_by_panel = self.state.panels.is(PanelKind::Notes);
            if self.state.ui.menu_open.is_some()
                || self.state.ui.file_browser.is_some()
                || (self.state.panels.is_open() && !cursor_owned_by_panel)
            {
                frame.clear_cursor();
            }
        })?;
        self.flush_pending_overlays(terminal)?;
        Ok(())
    }

    /// Emit raw terminal sequences queued by components, currently OSC 52
    /// clipboard writes. Kept separate from the buffer diff because overlays
    /// bypass the cell grid entirely.
    fn flush_pending_overlays(
        &mut self,
        terminal: &mut Terminal,
    ) -> Result<(), Box<dyn std::error::Error>> {
        for sequence in self.state.ui.pending_overlays.drain(..) {
            terminal.overlay_sequence(&sequence)?;
        }
        Ok(())
    }

    /// Every pending activation time, in a small fixed batch: the 250ms
    /// housekeeping tick and the periodic disk work (recovery, autosave).
    /// The event loop learns its next wakeup from this list; there is no
    /// frame timer among them.
    fn next_deadlines(&self) -> ([Instant; 4], usize) {
        let mut deadlines = [Instant::now(); 4];
        let mut count = 0;
        for deadline in [
            Some(self.next_tick),
            Some(self.next_recovery_write),
            Some(self.next_autosave_at),
        ]
        .into_iter()
        .flatten()
        {
            if count < deadlines.len() {
                deadlines[count] = deadline;
                count += 1;
            }
        }
        (deadlines, count)
    }

    /// Run everything that came due while the loop was asleep, then update the
    /// deadlines that just fired. A frame is requested only when one of these
    /// changed something visible.
    fn service_due_deadlines(&mut self) {
        let (deadlines, count) = self.next_deadlines();
        let now = Instant::now();
        let mut highest_due = None;
        for deadline in deadlines.iter().take(count) {
            if *deadline <= now {
                highest_due = Some(
                    highest_due
                        .map(|current: Instant| current.max(*deadline))
                        .unwrap_or(*deadline),
                );
            }
        }
        let Some(due_at) = highest_due else {
            return;
        };

        // Completed work is the freshest state; the channel holds every
        // result finished since the last pass.
        while let Some(task) = self.tasks.try_recv() {
            self.handle_task_result(task);
            self.redraw_requested = true;
        }

        if self.next_tick <= due_at {
            self.handle_tick();
            self.next_tick = due_at + TICK_INTERVAL;
            while self.next_tick <= Instant::now() {
                self.next_tick += TICK_INTERVAL;
            }
        }
        if self.next_recovery_write <= due_at {
            self.maybe_write_recovery();
        }
        if self.next_autosave_at <= due_at {
            self.maybe_autosave();
        }
    }

    /// One housekeeping pass for time-derived UI. The clock only repaints when
    /// the hour actually changed, and the system-theme probe (a `defaults`
    /// process on macOS) runs at a slower cadence than the tick itself.
    fn handle_tick(&mut self) {
        let before = (
            self.state.ui.local_hour,
            self.state.ui.system_is_dark,
            self.state.ui.status.clone(),
            self.state.ui.status_expires_at,
        );
        self.dispatch(UiEvent::Tick);
        self.state.ui.tick_count = self.state.ui.tick_count.wrapping_add(1);
        let after = (
            self.state.ui.local_hour,
            self.state.ui.system_is_dark,
            self.state.ui.status.clone(),
            self.state.ui.status_expires_at,
        );
        self.redraw_requested |= before != after;
    }

    /// Returns a resize the caller must forward to the terminal backend, if
    /// the event carried one. Everything else is applied in place.
    fn handle_terminal_event(&mut self, event: Event) -> Option<ordet_tui_framework::Rect> {
        let mut resized = None;
        match event {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                self.handle_key_event(key);
            }
            Event::Paste(pasted) => {
                self.handle_paste_event(pasted);
            }
            Event::Resize(width, height) => {
                let area = ordet_tui_framework::Rect::new(0, 0, width, height);
                resized = Some(area);
                self.dispatch(UiEvent::Resize(area));
            }
            Event::Mouse(mouse) => {
                self.handle_mouse_event(mouse);
            }
            _ => {}
        }
        resized
    }

    fn handle_mouse_event(&mut self, mouse: MouseEvent) {
        if self.state.ui.about_open {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                self.close_about();
            }
            return;
        }
        let point = Point::new(mouse.column, mouse.row);
        match mouse.kind {
            // Passive motion traces dropdown highlights. Handled first so
            // pickers (which swallow clicks) still highlight on hover.
            MouseEventKind::Moved => self.mouse_hover(point),
            MouseEventKind::Down(MouseButton::Left) => {
                // Prompts, the browser, quit confirmation and the welcome
                // screen own their input; clicks behind them do nothing.
                // Pickers stay reachable: their own press handler decides.
                if self.state.ui.prompt.is_some()
                    || self.state.ui.file_browser.is_some()
                    || self.state.ui.quit_confirmation.is_some()
                    || self.state.show_welcome_screen()
                {
                    return;
                }
                self.mouse_press(point);
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.state.ui.prompt.is_some()
                    || self.state.ui.file_browser.is_some()
                    || self.state.ui.quit_confirmation.is_some()
                    || self.state.show_welcome_screen()
                {
                    return;
                }
                self.mouse_drag(point);
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.mouse_release(point);
            }
            MouseEventKind::ScrollUp => {
                self.mouse_scroll(point, true);
            }
            MouseEventKind::ScrollDown => {
                self.mouse_scroll(point, false);
            }
            _ => {}
        }
    }

    /// The full-screen rect the overlay modals center in, rebuilt from the
    /// layout so hover and clicks map to the painted rows.
    fn full_area(&self) -> Rect {
        let header = self.state.ui.layout.header;
        let status = self.state.ui.layout.status;
        Rect::new(
            header.x,
            header.y,
            header.width,
            status.bottom().saturating_sub(header.y),
        )
    }

    /// Passive cursor motion: dropdown and picker highlights trace the
    /// cursor. Never runs anything and never steals clicks; purely visual.
    fn mouse_hover(&mut self, point: Point) {
        if self.state.show_welcome_screen()
            || self.state.ui.prompt.is_some()
            || self.state.ui.file_browser.is_some()
            || self.state.ui.quit_confirmation.is_some()
            || self.state.ui.about_open
        {
            return;
        }
        // Open menu: titles switch it, rows retarget it.
        if self.state.ui.menu_open.is_some() {
            let header = self.state.ui.layout.header;
            if header.contains(point) {
                let cells = crate::state::menu_title_cells(header);
                for (index, cell) in cells.iter().enumerate() {
                    if cell.contains(point) && self.state.ui.menu_open != Some(index) {
                        self.state.ui.menu_open = Some(index);
                        self.state.ui.menu_selected = 0;
                        return;
                    }
                }
                return;
            }
            if let Some(open) = self.state.ui.menu_open {
                let (outer, inner, count) = self.menu_geometry(open);
                // Tiny screens fall back to a borderless box (see renderer):
                // rows then live directly in the outer rect.
                let list = if inner.width > 0 && inner.height > 0 {
                    inner
                } else {
                    outer
                };
                if list.width > 0 && list.height > 0 && list.contains(point) {
                    let row = (point.y - list.y) as usize;
                    if row < count.min(list.height as usize) {
                        self.state.ui.menu_selected = row;
                    }
                }
            }
            return;
        }
        if self.state.panels.settings_picker.is_some() {
            let option_count = self
                .state
                .panels
                .settings_picker
                .as_ref()
                .map(|picker| picker.options.len())
                .unwrap_or(0);
            let list = crate::state::settings_picker_list_rect(self.full_area(), option_count);
            if list.width > 0 && list.height > 0 && list.contains(point) {
                let Some(picker) = self.state.panels.settings_picker.as_ref() else {
                    return;
                };
                let start = picker
                    .scroll
                    .min(option_count.saturating_sub(list.height as usize));
                let row = start + (point.y - list.y) as usize;
                if row < option_count {
                    if let Some(p) = self.state.panels.settings_picker.as_mut() {
                        p.selected = row;
                    }
                    self.preview_settings_picker_theme();
                }
            }
            return;
        }
        // Hovering a Pages or Workspace row moves the highlight, like the
        // menus: it never activates the row or steals the keyboard.
        if let Some(row) = panel_row_at_point(&self.state, point) {
            match self.state.panels.panel {
                Some(PanelKind::Pages) => self.state.panels.pages_selected = row,
                Some(PanelKind::Workspace) => self.state.panels.workspace_selected = row,
                _ => {}
            }
        }
    }

    /// Clicks on the settings option picker: a row confirms it (hover or
    /// click position wins over the keyboard selection), anywhere else
    /// closes the picker. Returns true when the picker owned the click.
    fn handle_picker_mouse_press(&mut self, point: Point) -> bool {
        if self.state.panels.settings_picker.is_some() {
            let option_count = self
                .state
                .panels
                .settings_picker
                .as_ref()
                .map(|picker| picker.options.len())
                .unwrap_or(0);
            let list = crate::state::settings_picker_list_rect(self.full_area(), option_count);
            let (count, start) = self
                .state
                .panels
                .settings_picker
                .as_ref()
                .map(|picker| {
                    let count = picker.options.len();
                    let start = picker
                        .scroll
                        .min(count.saturating_sub(list.height as usize));
                    (count, start)
                })
                .unwrap_or((0, 0));
            if list.width > 0 && list.height > 0 && list.contains(point) {
                let row = start + (point.y - list.y) as usize;
                if row < count {
                    if let Some(picker) = self.state.panels.settings_picker.as_mut() {
                        picker.selected = row;
                    }
                    self.confirm_settings_picker();
                }
            } else {
                self.cancel_settings_picker();
            }
            return true;
        }
        false
    }

    /// Clicks on the menu bar titles and the open dropdown. Title presses
    /// toggle or switch; item presses only arm (the highlight follows) and
    /// the release runs, so press-drag-release works like native menus. A
    /// press anywhere else closes an open menu. Returns true when the press
    /// belonged to the menu.
    fn handle_menu_mouse_press(&mut self, point: Point) -> bool {
        if self.state.show_welcome_screen()
            || self.state.ui.prompt.is_some()
            || self.state.ui.file_browser.is_some()
            || self.state.ui.quit_confirmation.is_some()
        {
            self.state.ui.menu_open = None;
            return false;
        }
        let header = self.state.ui.layout.header;
        if header.contains(point) {
            let cells = crate::state::menu_title_cells(header);
            for (index, cell) in cells.iter().enumerate() {
                if cell.contains(point) {
                    self.state.ui.menu_armed = false;
                    if self.state.ui.menu_open == Some(index) {
                        self.state.ui.menu_open = None;
                        self.restore_focus_status();
                    } else {
                        self.state.ui.menu_open = Some(index);
                        self.state.ui.menu_selected = 0;
                        self.clear_status();
                    }
                    return true;
                }
            }
            // The draft name side of the header: closes, claims nothing else.
            if self.state.ui.menu_open.is_some() {
                self.state.ui.menu_open = None;
                self.state.ui.menu_armed = false;
                self.restore_focus_status();
                return true;
            }
            return false;
        }
        let Some(open) = self.state.ui.menu_open else {
            return false;
        };
        let (outer, inner, count) = self.menu_geometry(open);
        // Tiny screens fall back to a borderless box: rows live in `outer`.
        let list = if inner.width > 0 && inner.height > 0 {
            inner
        } else {
            outer
        };
        if list.width > 0 && list.height > 0 && list.contains(point) {
            let row = (point.y - list.y) as usize;
            if row < count.min(list.height as usize) {
                self.state.ui.menu_selected = row;
                // Armed: the release runs the row under the cursor.
                self.state.ui.menu_armed = true;
            }
            return true;
        }
        // Border clicks belong to the menu: swallow them so the menu stays
        // open instead of closing on its own chrome.
        if outer.width > 0 && outer.height > 0 && outer.contains(point) {
            return true;
        }
        self.state.ui.menu_open = None;
        self.state.ui.menu_armed = false;
        self.restore_focus_status();
        true
    }

    /// Release after a menu item press runs the row under the cursor (or
    /// dismisses outside it). Anything else falls through to the normal
    /// cursor cleanup.
    fn mouse_release(&mut self, point: Point) {
        self.state.ui.mouse_down_cursor = None;
        if !self.state.ui.menu_armed || self.state.ui.menu_open.is_none() {
            return;
        }
        self.state.ui.menu_armed = false;
        let Some(open) = self.state.ui.menu_open else {
            return;
        };
        let (outer, inner, count) = self.menu_geometry(open);
        let list = if inner.width > 0 && inner.height > 0 {
            inner
        } else {
            outer
        };
        if list.width > 0 && list.height > 0 && list.contains(point) {
            let row = (point.y - list.y) as usize;
            if row < count.min(list.height as usize) {
                self.run_menu_item(open, row);
                return;
            }
        }
        // Releasing on the border keeps the menu open; only a release truly
        // outside dismisses it.
        if outer.width > 0 && outer.height > 0 && outer.contains(point) {
            return;
        }
        self.state.ui.menu_open = None;
        self.restore_focus_status();
    }

    /// Char index under a terminal cell inside a text area, or `None` when the
    /// cell is outside it. Shared by click and drag so both agree.
    fn text_position_at(
        &self,
        point: Point,
        area: Rect,
        scroll: usize,
        buffer: &EditorBuffer,
    ) -> Option<(usize, usize)> {
        if !area.contains(point) || area.width < 2 || area.height < 1 {
            return None;
        }
        let (wrap_width, gutter) =
            wrap_and_gutter(area.width as usize, buffer, self.state.ui.show_line_numbers);
        let column = (point.x as usize).saturating_sub(area.x as usize + gutter);
        let row = (point.y as usize).saturating_sub(area.y as usize);
        Some((
            buffer.visual_cursor_for_column(wrap_width, scroll + row, column),
            wrap_width,
        ))
    }

    fn editor_text_area(&self) -> Rect {
        window_text_area(&self.state, self.state.active_editor_area())
    }

    fn mouse_press(&mut self, point: Point) {
        if self.handle_picker_mouse_press(point) {
            return;
        }
        if self.handle_menu_mouse_press(point) {
            return;
        }
        // Panels are modal: a press inside one belongs to the panel, never
        // to the text behind it.
        if self.state.panels.is_open() {
            self.panel_mouse_press(point);
            return;
        }
        self.focus_window_at(point);
        let editor_area = self.editor_text_area();
        let editor_cursor = self.text_position_at(
            point,
            editor_area,
            self.state.editor().scroll,
            &self.state.editor().buffer,
        );
        if let Some((cursor, wrap_width)) = editor_cursor {
            let height = editor_area.height.max(1) as usize;
            self.state.editor_mut().buffer.record_jump();
            self.state
                .editor_mut()
                .buffer
                .set_cursor_with_anchor(cursor, cursor);
            self.state.ui.mouse_down_cursor = Some(cursor);
            self.state
                .editor_mut()
                .ensure_cursor_visible(wrap_width, height);
        }
    }

    /// Press inside an open panel. Pages and Workspace act on the row under
    /// the pointer; Notes takes the text cursor.
    fn panel_mouse_press(&mut self, point: Point) {
        let Some(kind) = self.state.panels.panel else {
            return;
        };
        match kind {
            PanelKind::Pages => {
                if let Some(row) = panel_row_at_point(&self.state, point) {
                    let rows = self.state.page_rows();
                    if let Some(page) = rows.get(row) {
                        let index = page.index;
                        self.state.panels.close();
                        self.switch_to_draft(index);
                    }
                }
            }
            PanelKind::Workspace => {
                if let Some(row) = panel_row_at_point(&self.state, point) {
                    let rows = self.state.workspace_rows();
                    if let Some(entry) = rows.get(row) {
                        match &entry.kind {
                            crate::state::WorkspaceRowKind::Folder(path) => {
                                let path = path.clone();
                                self.state.toggle_workspace_expanded(&path);
                            }
                            crate::state::WorkspaceRowKind::File(path) => {
                                let path = path.clone();
                                self.state.panels.close();
                                self.open_or_focus_path(path);
                            }
                        }
                    }
                }
            }
            PanelKind::Notes => {
                let content = panel_content_rect(&self.state);
                let notes_cursor = self.text_position_at(
                    point,
                    content,
                    self.state.notes().scroll,
                    &self.state.notes().buffer,
                );
                if let Some((cursor, wrap_width)) = notes_cursor {
                    let height = content.height.max(1) as usize;
                    self.state.notes_mut().buffer.record_jump();
                    self.state
                        .notes_mut()
                        .buffer
                        .set_cursor_with_anchor(cursor, cursor);
                    self.state.ui.mouse_down_cursor = Some(cursor);
                    self.state
                        .notes_mut()
                        .ensure_cursor_visible(wrap_width, height);
                }
            }
            _ => {}
        }
    }

    fn mouse_drag(&mut self, point: Point) {
        // An open menu traces the dragged cursor across its rows (press on
        // a title, drag, release runs, same highlight, no click behind it).
        if let Some(open) = self.state.ui.menu_open {
            let (outer, inner, count) = self.menu_geometry(open);
            let list = if inner.width > 0 && inner.height > 0 {
                inner
            } else {
                outer
            };
            if list.width > 0 && list.height > 0 && list.contains(point) {
                let row = (point.y - list.y) as usize;
                if row < count.min(list.height as usize) {
                    self.state.ui.menu_selected = row;
                }
            }
            return;
        }
        let Some(down) = self.state.ui.mouse_down_cursor else {
            return;
        };
        // Drags reach the text only while no panel covers it; the Notes
        // panel keeps its own drag on its own buffer.
        let notes_drag = self.state.panels.is(PanelKind::Notes);
        if self.state.panels.is_open() && !notes_drag {
            return;
        }
        let (area, scroll) = if notes_drag {
            (panel_content_rect(&self.state), self.state.notes().scroll)
        } else {
            (self.editor_text_area(), self.state.editor().scroll)
        };
        // The probe borrows shared and returns an owned position, so the
        // borrow ends at the semicolon and mutation below is free.
        let cursor_at = if notes_drag {
            self.text_position_at(point, area, scroll, &self.state.notes().buffer)
        } else {
            self.text_position_at(point, area, scroll, &self.state.editor().buffer)
        };
        if let Some((cursor, wrap_width)) = cursor_at {
            let height = area.height.max(1) as usize;
            if notes_drag {
                self.state
                    .notes_mut()
                    .buffer
                    .set_cursor_with_anchor(down, cursor);
                self.state
                    .notes_mut()
                    .ensure_cursor_visible(wrap_width, height);
            } else {
                self.state
                    .editor_mut()
                    .buffer
                    .set_cursor_with_anchor(down, cursor);
                self.state
                    .editor_mut()
                    .ensure_cursor_visible(wrap_width, height);
            }
        }
    }

    fn mouse_scroll(&mut self, point: Point, up: bool) {
        let delta = 3;
        if self.state.panels.is_open() {
            if self.state.panels.is(PanelKind::Notes) {
                let notes = self.state.notes_mut();
                if up {
                    notes.scroll = notes.scroll.saturating_sub(delta);
                } else {
                    notes.scroll = notes.scroll.saturating_add(delta);
                }
                notes.free_scroll_at = Some(notes.buffer.cursor());
            } else if self.state.panels.is(PanelKind::Pages)
                || self.state.panels.is(PanelKind::Workspace)
            {
                let step = if up { -1 } else { 1 };
                self.nudge_panel_selection(step, delta);
            } else {
                if up {
                    self.state.panels.scroll = self.state.panels.scroll.saturating_sub(delta);
                } else {
                    self.state.panels.scroll = self.state.panels.scroll.saturating_add(delta);
                }
            }
            return;
        }
        if self.state.ui.layout.editor.contains(point) {
            let editor = self.state.editor_mut();
            if up {
                editor.scroll = editor.scroll.saturating_sub(delta);
            } else {
                editor.scroll = editor.scroll.saturating_add(delta);
            }
            // Free scroll: the view may leave the cursor until it moves.
            editor.free_scroll_at = Some(editor.buffer.cursor());
        }
    }

    fn handle_key_event(&mut self, key: KeyEvent) {
        // Terminals without "Option as Meta" deliver Option chords as the
        // literal glyph. Retype them before anything else sees the glyph, or
        // an editing shortcut silently becomes document text.
        let key = normalize_macos_option_key(key);
        if self.state.ui.about_open {
            if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char(' ')) {
                self.close_about();
            }
            return;
        }
        // A pending page delete is answered Y/N: Y deletes, N (or anything
        // else) cancels. Intercept before the disarm below so Y/N never
        // reach the editor as typed text.
        if self.state.ui.pending_close_draft.is_some() {
            if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                if let Some(index) = self.state.ui.pending_close_draft {
                    self.close_unsaved_draft(index, true);
                }
                return;
            }
            if matches!(key.code, KeyCode::Char('n') | KeyCode::Char('N')) {
                self.state.ui.pending_close_draft = None;
                self.restore_focus_status();
                return;
            }
        }
        if self.state.ui.pending_trash.is_some() {
            if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                self.trash_requested(true);
                return;
            }
            if matches!(key.code, KeyCode::Char('n') | KeyCode::Char('N')) {
                self.state.ui.pending_trash = None;
                self.restore_focus_status();
                return;
            }
        }
        // An armed trash/close survives only a confirming Ctrl+R. Any other
        // key disarms it and wipes its message (Delete re-arms through its
        // action right below, so it never fires). Ctrl+Shift+R is revisions:
        // Shift disarms.
        let is_confirm_key = key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::SHIFT)
            && matches!(key.code, KeyCode::Char('r') | KeyCode::Char('R'));
        if !is_confirm_key
            && (self.state.ui.pending_trash.is_some()
                || self.state.ui.pending_close_draft.is_some())
        {
            self.state.ui.pending_trash = None;
            self.state.ui.pending_close_draft = None;
            self.restore_focus_status();
        }
        // Paste owns its chords on the very first press, in every pane and
        // modal. The exact-match keymap misses Kitty Shift encodings, and
        // several modals (quit confirmation, prefix, file browser, pickers)
        // otherwise swallow the first chord silently, forcing a second press.
        // Handling here clears that transient state and pastes in the same
        // press.
        if is_paste_key(key) {
            self.state.ui.prefix = None;
            self.state.ui.menu_open = None;
            if self.state.ui.quit_confirmation.is_some() {
                self.state.ui.quit_confirmation = None;
                // Fall through to paste below in the same press.
            }
            self.handle_paste_key();
            return;
        }
        if self.state.ui.quit_confirmation.is_some() {
            self.handle_quit_confirmation_key(key);
            return;
        }

        if self.handle_file_browser_key(key) {
            return;
        }

        if self.handle_prompt_key(key) {
            return;
        }

        if self.handle_menu_key(key) {
            return;
        }

        if self.handle_settings_picker_key(key) {
            return;
        }

        // Esc with no modal open backs out one step: zoom first, then the
        // topmost panel.
        if key.code == KeyCode::Esc
            && key.modifiers == KeyModifiers::NONE
            && self.state.ui.prefix.is_none()
        {
            if self.state.ui.zoomed {
                self.toggle_zoom();
                return;
            }
            // Esc closes the open panel, the topmost overlay the earlier
            // handlers (menu, prompt, picker) did not already claim.
            if self.state.panels.is_open() && !self.state.show_welcome_screen() {
                self.state.panels.close();
                self.restore_focus_status();
                return;
            }
        }

        if self.handle_prefix_key(key) {
            return;
        }

        if let Some(command) = self.keymap.resolve(key) {
            self.dispatch(UiEvent::Command(command));
        } else {
            self.dispatch(UiEvent::Key(key));
        }
    }

    /// Rewrites the recovery snapshot at most every few seconds, so a crash or a
    /// closed terminal costs a couple of seconds of typing rather than a session.
    fn maybe_write_recovery(&mut self) {
        let now = Instant::now();
        if now < self.next_recovery_write {
            return;
        }
        self.next_recovery_write = now + RECOVERY_INTERVAL;

        let file = RecoveryFile {
            drafts: self.state.unsaved_drafts(),
        };
        if file.is_empty() {
            // Nothing unsaved: drop any snapshot left by an earlier session.
            if self.recovered_at_startup > 0 {
                if let Err(error) = clear_recovery() {
                    self.state
                        .set_status(format!("Recovery file could not be cleared: {error}"));
                }
            }
            self.recovered_at_startup = 0;
            return;
        }

        self.tasks
            .spawn(Some(TaskKey::from("recovery")), move |token| {
                if token.is_cancelled() {
                    return Err(TaskError::new("recovery write cancelled"));
                }
                save_recovery(&file).map_err(TaskError::new)?;
                if token.is_cancelled() {
                    return Err(TaskError::new("recovery write cancelled"));
                }
                Ok(AppMessage::RecoveryCompleted)
            });
    }

    fn request_quit(&mut self) {
        // Files that already live on disk flush now, then quit without asking.
        // New/untitled drafts are never auto-materialized: quitting with one
        // asks (Y saves, N quits without saving, anything else keeps editing).
        // Backups ride along best-effort when enabled, after the flush so
        // they capture the just-saved versions.
        let flushed = self.flush_dirty_drafts();
        if self.state.ui.backup_enabled && self.state.ui.backup_on_close {
            let _ = self.run_backup();
        }
        match flushed {
            Ok(()) => {
                self.state.ui.quit_confirmation = None;
                self.state.ui.should_quit = true;
            }
            Err(dirty_titles) => {
                self.state.ui.quit_confirmation = Some(QuitConfirmation { dirty_titles });
            }
        }
    }

    /// Synchronous save of every dirty draft that already has a file path.
    /// Untitled drafts are never written here: non-empty ones are reported as
    /// unsaved titles so quitting asks instead of silently creating files.
    /// Empty scratch is skipped. Returns the titles that could not be written
    /// or were never saved.
    fn flush_dirty_drafts(&mut self) -> Result<(), Vec<String>> {
        let mut failed = Vec::new();
        for index in 0..self.state.drafts.len() {
            let job = self.state.drafts.get(index).and_then(|draft| {
                if !draft.editor.is_dirty() {
                    return None;
                }
                let plain_text = draft.editor.buffer.plain_text();
                let Some(path) = draft.editor.file_path.clone() else {
                    if plain_text.trim().is_empty() {
                        return None;
                    }
                    failed.push(draft.editor.title());
                    return None;
                };
                Some((
                    normalize_document_path(path),
                    plain_text,
                    draft.editor.buffer.to_ord_html(),
                    draft.editor.title(),
                    draft.editor.revision,
                    draft.editor.doc_status.clone(),
                    draft.editor.doc_auto_title,
                    draft.editor.doc_label_color.clone(),
                ))
            });
            let Some((
                path,
                plain_text,
                content_html,
                title,
                revision,
                doc_status,
                doc_auto_title,
                doc_label_color,
            )) = job
            else {
                continue;
            };
            match Self::write_draft_file(
                &path,
                &plain_text,
                &content_html,
                &title,
                &doc_status,
                doc_auto_title,
                &doc_label_color,
            ) {
                Ok(()) => {
                    if let Some(draft) = self.state.drafts.get_mut(index) {
                        draft.editor.mark_saved(revision, path.clone());
                    }
                    self.state
                        .history
                        .record(&history_key_for(&path), &plain_text, unix_now());
                    self.state.remember_recent_path(path);
                }
                Err(_) => failed.push(title),
            }
        }
        let _ = save_history(&self.state.history);
        let _ = self.persist_settings();
        if failed.is_empty() {
            Ok(())
        } else {
            Err(failed)
        }
    }

    /// Autosave: every few seconds, background-save each dirty draft that
    /// already lives on disk (opened or manually saved at least once).
    /// Silent by design, failures surface through the normal background-task
    /// error status. New/untitled drafts are never autosaved: quitting with
    /// one asks instead of silently materializing a file.
    fn maybe_autosave(&mut self) {
        let now = Instant::now();
        if now < self.next_autosave_at {
            return;
        }
        self.next_autosave_at = now + AUTOSAVE_DELAY;
        if !self.state.ui.autosave_enabled {
            return;
        }
        for index in 0..self.state.drafts.len() {
            let Some(draft) = self.state.drafts.get(index) else {
                continue;
            };
            if !draft.editor.is_dirty() || draft.editor.pending_save {
                continue;
            }
            let Some(path) = draft.editor.file_path.clone() else {
                continue;
            };
            self.spawn_save_draft(index, path, true);
        }
    }

    fn handle_quit_confirmation_key(&mut self, key: KeyEvent) {
        let save = matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y'));
        let discard = matches!(key.code, KeyCode::Char('n') | KeyCode::Char('N'));

        let Some(pending) = self.state.ui.quit_confirmation.take() else {
            return;
        };

        if discard {
            self.state.ui.should_quit = true;
            return;
        }

        if save {
            self.confirm_quit_save();
            return;
        }

        let count = pending.dirty_titles.len();
        self.state
            .set_status_quiet(format!("Quit cancelled. {count} unsaved draft(s) kept."));
    }

    /// Y on the quit prompt: save everything, then quit. Drafts that already
    /// live on disk flush synchronously; the first remaining untitled draft
    /// opens a save-path prompt instead (quitting continues once that save
    /// lands). Failed writes keep the prompt open on the failed titles.
    fn confirm_quit_save(&mut self) {
        let mut failed = Vec::new();
        for index in 0..self.state.drafts.len() {
            let job = self.state.drafts.get(index).and_then(|draft| {
                if !draft.editor.is_dirty() {
                    return None;
                }
                let path = draft.editor.file_path.clone()?;
                Some((
                    normalize_document_path(path),
                    draft.editor.buffer.plain_text(),
                    draft.editor.buffer.to_ord_html(),
                    draft.editor.title(),
                    draft.editor.revision,
                    draft.editor.doc_status.clone(),
                    draft.editor.doc_auto_title,
                    draft.editor.doc_label_color.clone(),
                ))
            });
            let Some((
                path,
                plain_text,
                content_html,
                title,
                revision,
                doc_status,
                doc_auto_title,
                doc_label_color,
            )) = job
            else {
                continue;
            };
            match Self::write_draft_file(
                &path,
                &plain_text,
                &content_html,
                &title,
                &doc_status,
                doc_auto_title,
                &doc_label_color,
            ) {
                Ok(()) => {
                    if let Some(draft) = self.state.drafts.get_mut(index) {
                        draft.editor.mark_saved(revision, path.clone());
                    }
                    self.state
                        .history
                        .record(&history_key_for(&path), &plain_text, unix_now());
                    self.state.remember_recent_path(path);
                }
                Err(_) => failed.push(title),
            }
        }
        let _ = save_history(&self.state.history);
        let _ = self.persist_settings();
        if !failed.is_empty() {
            self.state.ui.quit_confirmation = Some(QuitConfirmation {
                dirty_titles: failed,
            });
            return;
        }
        if let Some(index) = self.state.drafts.iter().position(|draft| {
            draft.editor.is_dirty()
                && draft.editor.file_path.is_none()
                && !draft.editor.buffer.plain_text().trim().is_empty()
        }) {
            self.switch_to_draft(index);
            self.state.ui.quit_after_save = true;
            self.open_save_path_prompt();
            return;
        }
        // Anything left dirty here is empty scratch: nothing to lose.
        self.state.ui.quit_confirmation = None;
        self.state.ui.should_quit = true;
    }

    /// Resolves the `Ctrl-K` two-stroke prefix.
    ///
    /// This runs before the main keymap so the leader chord never reaches the
    /// editor as a literal keystroke.
    fn handle_prefix_key(&mut self, key: KeyEvent) -> bool {
        // Quick sub-mode: `Ctrl-K Q` then a digit jumps to that numbered
        // bookmark, `P` jumps to the previous position.
        if matches!(self.state.ui.prefix, Some(PrefixMode::CtrlKQuick)) {
            self.state.ui.prefix = None;
            self.restore_focus_status();
            if key.modifiers == KeyModifiers::NONE {
                match key.code {
                    KeyCode::Char(ch) if ch.is_ascii_digit() => {
                        self.jump_bookmark((ch as u8 - b'0') as usize);
                        return true;
                    }
                    KeyCode::Char('p') | KeyCode::Char('P') => {
                        self.dispatch(UiEvent::Command(Command::JumpPreviousPosition));
                        return true;
                    }
                    // Bare Q: back to the last-dropped mark, no slot memorized.
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        self.jump_last_bookmark();
                        return true;
                    }
                    KeyCode::Esc => return true,
                    _ => {}
                }
            }
            self.state
                .set_status("⌃K Q cancelled. No quick jump matched that key.");
            return true;
        }

        if matches!(self.state.ui.prefix, Some(PrefixMode::CtrlO)) {
            self.state.ui.prefix = None;
            self.restore_focus_status();
            // Esc cancels quietly: `restore_focus_status` has already put the
            // open panel's hint (or the welcome hint) back on the line, and
            // that hint should not be overwritten by a cancel note.
            if key.modifiers == KeyModifiers::NONE && key.code == KeyCode::Esc {
                return true;
            }
            let Some(command) = self.o_prefix_keymap.resolve(key) else {
                self.state
                    .set_status("⌃O cancelled. No command matched that key.");
                return true;
            };
            self.dispatch(UiEvent::Command(command));
            return true;
        }

        if matches!(self.state.ui.prefix, Some(PrefixMode::CtrlK)) {
            self.state.ui.prefix = None;
            self.restore_focus_status();
            // Digits set numbered bookmarks in slots 0-9.
            if key.modifiers == KeyModifiers::NONE {
                if let KeyCode::Char(ch) = key.code {
                    if ch.is_ascii_digit() {
                        self.set_bookmark((ch as u8 - b'0') as usize);
                        return true;
                    }
                    if ch == 'q' || ch == 'Q' {
                        self.state.ui.prefix = Some(PrefixMode::CtrlKQuick);
                        self.state.set_status_persistent(
                            "Quick jump. Enter jumps to the last mark, digit 0-9 to that bookmark, P jumps back, Esc cancels."
                                .to_string(),
                        );
                        return true;
                    }
                }
            }
            // Esc cancels quietly: `restore_focus_status` has already put the
            // open panel's hint (or the welcome hint) back on the line, and
            // that hint should not be overwritten by a cancel note.
            if key.modifiers == KeyModifiers::NONE && key.code == KeyCode::Esc {
                return true;
            }
            let Some(command) = self.prefix_keymap.resolve(key) else {
                self.state
                    .set_status("⌃K cancelled. No command matched that key.");
                return true;
            };
            self.dispatch(UiEvent::Command(command));
            return true;
        }

        let chord = KeyChord::from(key);
        if chord == o_prefix_leader() {
            self.state.ui.prefix = Some(PrefixMode::CtrlO);
            self.state.set_status_persistent(
                "⌃O pressed. C centers, R right-aligns, L left-aligns, J justifies; Esc cancels."
                    .to_string(),
            );
            return true;
        }
        if chord != prefix_leader() {
            return false;
        }

        self.state.ui.prefix = Some(PrefixMode::CtrlK);
        self.state.set_status_persistent(
            "⌃K pressed. Follow with a command letter or a digit (0-9 sets a bookmark), Esc to cancel."
                .to_string(),
        );
        true
    }

    /// Drops a numbered bookmark at the cursor of the focused text pane.
    fn set_bookmark(&mut self, index: usize) {
        let cursor = self.current_text_buffer().cursor();
        if self.focused_notes_buffer() {
            let notes = self.state.notes_mut();
            notes.bookmarks[index] = Some(cursor);
            notes.last_bookmark = Some(index);
        } else {
            let editor = self.state.editor_mut();
            editor.bookmarks[index] = Some(cursor);
            editor.last_bookmark = Some(index);
        }
        self.state
            .set_status_quiet(format!("Bookmark {index} set. ⌃K Q {index} jumps back."));
    }

    /// Drops a bookmark in the lowest free slot, so no slot number has to
    /// be memorized. A full rack reuses slot 0 and says so.
    fn drop_quick_bookmark(&mut self) {
        let full = if self.focused_notes_buffer() {
            self.state
                .notes()
                .bookmarks
                .iter()
                .all(|mark| mark.is_some())
        } else {
            self.state
                .editor()
                .bookmarks
                .iter()
                .all(|mark| mark.is_some())
        };
        let slot = if full {
            0
        } else if self.focused_notes_buffer() {
            self.state
                .notes()
                .bookmarks
                .iter()
                .position(|mark| mark.is_none())
                .unwrap_or(0)
        } else {
            self.state
                .editor()
                .bookmarks
                .iter()
                .position(|mark| mark.is_none())
                .unwrap_or(0)
        };
        self.set_bookmark(slot);
        if full {
            self.state
                .set_status("All bookmarks full. Bookmark 0 reused.".to_string());
        }
    }

    /// Jumps to the most recently dropped bookmark, whatever its slot.
    fn jump_last_bookmark(&mut self) {
        let last = if self.focused_notes_buffer() {
            self.state.notes().last_bookmark
        } else {
            self.state.editor().last_bookmark
        };
        match last {
            Some(index) => self.jump_bookmark(index),
            None => self
                .state
                .set_status("No bookmarks yet. ⌃K 0-9 drops one.".to_string()),
        }
    }

    /// Jumps to a numbered bookmark, recording the jump so `P` returns.
    fn jump_bookmark(&mut self, index: usize) {
        let mark = if self.focused_notes_buffer() {
            self.state.notes().bookmarks[index]
        } else {
            self.state.editor().bookmarks[index]
        };
        let Some(mark) = mark else {
            self.state
                .set_status(format!("Bookmark {index} is empty. ⌃K {index} sets it."));
            return;
        };
        if self.focused_notes_buffer() {
            let notes = self.state.notes_mut();
            notes.buffer.record_jump();
            let pos = mark.min(notes.buffer.char_count());
            notes.buffer.set_cursor_with_anchor(pos, pos);
            let area = panel_content_rect(&self.state);
            let height = area.height.max(1) as usize;
            let width = area.width.max(1) as usize;
            self.state.notes_mut().ensure_cursor_visible(width, height);
        } else {
            let editor_area = self.editor_text_area();
            let show_line_numbers = self.state.ui.show_line_numbers;
            let editor = self.state.editor_mut();
            editor.buffer.record_jump();
            let pos = mark.min(editor.buffer.char_count());
            editor.buffer.set_cursor_with_anchor(pos, pos);
            let height = editor_area.height.max(1) as usize;
            let (wrap_width, _) = crate::components::wrap_and_gutter(
                editor_area.width as usize,
                &editor.buffer,
                show_line_numbers,
            );
            self.state
                .editor_mut()
                .ensure_cursor_visible(wrap_width, height);
        }
        self.state
            .set_status_quiet(format!("Jumped to bookmark {index}."));
    }

    fn handle_paste_event(&mut self, pasted: String) {
        if self.handle_prompt_paste(&pasted) {
            return;
        }

        if let Some(path) = parse_pasted_single_path(&pasted) {
            self.state.ui.file_browser = None;
            self.open_or_focus_path(path);
            return;
        }

        if self.handle_file_browser_paste(&pasted) {
            return;
        }

        self.paste_text_into_focused_buffer(&pasted);
    }

    fn handle_prompt_paste(&mut self, pasted: &str) -> bool {
        let Some(prompt) = self.state.ui.prompt.as_mut() else {
            return false;
        };
        let value = pasted.trim_end_matches(&['\n', '\r'][..]);
        prompt.value.push_str(value);
        true
    }

    fn handle_file_browser_paste(&mut self, pasted: &str) -> bool {
        let Some(mut browser) = self.state.ui.file_browser.take() else {
            return false;
        };

        for ch in pasted.chars() {
            if ch == '\n' || ch == '\r' || ch.is_control() {
                continue;
            }
            browser.push_char(ch);
        }

        self.state.set_status_quiet(browser.message.clone());
        self.state.ui.file_browser = Some(browser);
        true
    }

    fn paste_text_into_focused_buffer(&mut self, pasted: &str) {
        let text = pasted.replace("\r\n", "\n").replace('\r', "\n");
        if text.is_empty() {
            return;
        }

        if self.state.show_welcome_screen() {
            self.create_new_draft();
        }

        if self.focused_notes_buffer() {
            let notes = self.state.notes_mut();
            notes.buffer.insert_text(&text);
            notes.mark_dirty();
        } else {
            let editor = self.state.editor_mut();
            editor.buffer.insert_text(&text);
            editor.mark_dirty();
        }
    }

    fn handle_file_browser_key(&mut self, key: KeyEvent) -> bool {
        let Some(mut browser) = self.state.ui.file_browser.take() else {
            return false;
        };

        // On the welcome screen the menu owns single letters while merely
        // browsing, so D/O/H/X hotkeys keep working with the rail open.
        // Tab still enters search/path mode, where typing goes to the
        // browser as usual.
        if self.state.show_welcome_screen()
            && browser.mode == FileBrowserMode::Browse
            && matches!(key.code, KeyCode::Char(_))
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            self.state.ui.file_browser = Some(browser);
            return false;
        }

        let mut close = false;
        let mut open_path = None;
        let mut prompt = None;
        let result = match key.code {
            KeyCode::Esc => {
                close = true;
                Ok(FileBrowserAction::None)
            }
            KeyCode::Tab => {
                browser.cycle_mode();
                Ok(FileBrowserAction::None)
            }
            KeyCode::Up => {
                browser.move_up();
                Ok(FileBrowserAction::None)
            }
            KeyCode::Down => {
                browser.move_down();
                Ok(FileBrowserAction::None)
            }
            KeyCode::Home => {
                browser.move_home();
                Ok(FileBrowserAction::None)
            }
            KeyCode::End => {
                browser.move_end();
                Ok(FileBrowserAction::None)
            }
            KeyCode::PageUp => {
                browser.page_up(8);
                Ok(FileBrowserAction::None)
            }
            KeyCode::PageDown => {
                browser.page_down(8);
                Ok(FileBrowserAction::None)
            }
            KeyCode::Char('r') if key.modifiers == KeyModifiers::CONTROL => {
                prompt = self.browser_rename_prompt(&browser);
                Ok(FileBrowserAction::None)
            }
            KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => {
                prompt = self.browser_copy_prompt(&browser);
                Ok(FileBrowserAction::None)
            }
            KeyCode::Left if browser.mode == FileBrowserMode::Browse => browser.go_parent(),
            KeyCode::Backspace => browser.backspace(),
            KeyCode::Enter => browser.activate_selected(),
            KeyCode::Char(ch)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                browser.push_char(ch);
                Ok(FileBrowserAction::None)
            }
            _ => Ok(FileBrowserAction::None),
        };

        match result {
            Ok(FileBrowserAction::Open(path)) => {
                open_path = Some(path);
            }
            Ok(FileBrowserAction::None) => {}
            Err(error) => {
                browser.message = error;
            }
        }

        if let Some(path) = open_path {
            self.open_or_focus_path(path);
            self.restore_focus_status();
            return true;
        }

        if let Some(prompt) = prompt {
            self.state.ui.prompt = Some(prompt);
        }

        if close {
            self.restore_focus_status();
        } else {
            self.state.set_status_quiet(browser.message.clone());
            self.state.ui.file_browser = Some(browser);
        }

        true
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) -> bool {
        let Some(mut prompt) = self.state.ui.prompt.take() else {
            return false;
        };

        let plain_key = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);

        if plain_key && key.code == KeyCode::Tab {
            if let PromptKind::FindReplaceValue { query } = prompt.kind.clone() {
                self.run_replace_all(&query, prompt.value);
                return true;
            }
        }

        // Support pasting via Ctrl-V or Alt-V into prompts (e.g. pasting API keys or paths)
        if (key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('v') | KeyCode::Char('V')))
            || (key.modifiers.contains(KeyModifiers::ALT)
                && matches!(key.code, KeyCode::Char('v') | KeyCode::Char('V')))
        {
            if let Some((text, _)) = self.state.clipboard.paste() {
                prompt
                    .value
                    .push_str(text.trim_end_matches(&['\n', '\r'][..]));
                self.state.ui.prompt = Some(prompt);
                return true;
            }
        }

        // Search flags flip live while a find/replace prompt is open (Alt-R/W/C or Mac Option characters).
        let is_search_prompt = matches!(
            prompt.kind,
            PromptKind::FindText
                | PromptKind::FindReplaceQuery
                | PromptKind::FindReplaceValue { .. }
        );
        if is_search_prompt {
            if matches!(prompt.kind, PromptKind::FindText)
                && key.modifiers == KeyModifiers::NONE
                && matches!(
                    key.code,
                    KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
                )
                && !prompt.value.trim().is_empty()
            {
                let query = prompt.value.clone();
                self.run_find_direction(&query, matches!(key.code, KeyCode::Right | KeyCode::Down));
                self.state.ui.prompt = Some(prompt);
                return true;
            }
            let toggled = if key.modifiers.contains(KeyModifiers::ALT) {
                if let KeyCode::Char(ch) = key.code {
                    match ch.to_ascii_lowercase() {
                        'r' => {
                            self.state.ui.search_options.regex ^= true;
                            true
                        }
                        'w' => {
                            self.state.ui.search_options.whole_word ^= true;
                            true
                        }
                        'c' => {
                            self.state.ui.search_options.case_sensitive ^= true;
                            true
                        }
                        _ => false,
                    }
                } else {
                    false
                }
            } else {
                match key.code {
                    KeyCode::Char('®') => {
                        self.state.ui.search_options.regex ^= true;
                        true
                    }
                    KeyCode::Char('∑') => {
                        self.state.ui.search_options.whole_word ^= true;
                        true
                    }
                    KeyCode::Char('ç') => {
                        self.state.ui.search_options.case_sensitive ^= true;
                        true
                    }
                    _ => false,
                }
            };
            if toggled {
                prompt.hint =
                    crate::prompt::search_hint(&prompt.kind, self.state.ui.search_options);
                self.state.ui.prompt = Some(prompt);
                return true;
            }
        }

        match key.code {
            KeyCode::Esc => {
                if matches!(prompt.kind, PromptKind::SavePath) {
                    self.state.ui.quit_after_save = false;
                    self.pending_after_save = CloseIntent::None;
                }
                self.restore_focus_status();
            }
            KeyCode::Enter => {
                self.submit_prompt(prompt);
            }
            KeyCode::Backspace => {
                prompt.backspace();
                self.state.ui.prompt = Some(prompt);
            }
            KeyCode::Char(ch) if plain_key => {
                prompt.push(ch);
                self.state.ui.prompt = Some(prompt);
            }
            _ => {
                self.state.ui.prompt = Some(prompt);
            }
        }

        true
    }

    /// Sorts the selected lines A–Z, or reports why there is nothing to do.
    fn sort_selected_lines(&mut self) {
        let sorted = if self.focused_notes_buffer() {
            let notes = self.state.notes_mut();
            let ok = notes.buffer.sort_selected_lines();
            if ok {
                notes.mark_dirty();
            }
            ok
        } else {
            let editor = self.state.editor_mut();
            let ok = editor.buffer.sort_selected_lines();
            if ok {
                editor.mark_dirty();
            }
            ok
        };
        self.state.set_status_quiet(if sorted {
            "Lines sorted A–Z. ⌃Z undoes.".to_string()
        } else {
            "Select more than one line to sort.".to_string()
        });
    }

    /// Outer and inner rectangles for the open menu, plus the number of
    /// rows it currently offers. One source for rendering, hover, clicks,
    /// drags, and keyboard navigation.
    fn menu_geometry(
        &self,
        menu: usize,
    ) -> (ordet_tui_framework::Rect, ordet_tui_framework::Rect, usize) {
        let count = crate::commands::menu_items(menu, self.state.window_count()).len();
        let header = self.state.ui.layout.header;
        let bottom = self.state.ui.layout.status.y;
        (
            crate::state::menu_drop_rect(header, bottom, menu, count),
            crate::state::menu_drop_inner_rect(header, bottom, menu, count),
            count,
        )
    }

    fn clear_status(&mut self) {
        self.state.ui.status.clear();
        self.state.ui.status_expires_at = None;
    }

    /// The top menu bar owns plain keys while open (F10 toggles, arrows
    /// move, Enter runs, Esc closes). Modified chords and function keys
    /// close it and pass through so nothing behind it is trapped.
    fn handle_menu_key(&mut self, key: KeyEvent) -> bool {
        // A modal owns its input; the menu never fights it.
        if self.state.ui.prompt.is_some()
            || self.state.ui.file_browser.is_some()
            || self.state.ui.quit_confirmation.is_some()
            || self.state.show_welcome_screen()
        {
            self.state.ui.menu_open = None;
            return false;
        }

        if key.code == KeyCode::F(10) && key.modifiers == KeyModifiers::NONE {
            if self.state.ui.menu_open.is_some() {
                self.state.ui.menu_open = None;
                self.restore_focus_status();
            } else {
                self.state.ui.menu_open = Some(0);
                self.state.ui.menu_selected = 0;
                self.clear_status();
            }
            return true;
        }

        let Some(open) = self.state.ui.menu_open else {
            return false;
        };

        if key.modifiers != KeyModifiers::NONE || matches!(key.code, KeyCode::F(_)) {
            self.state.ui.menu_open = None;
            return false;
        }

        let count = crate::commands::menu_items(open, self.state.window_count()).len();
        if count == 0 {
            self.state.ui.menu_open = None;
            return true;
        }
        self.state.ui.menu_selected = self.state.ui.menu_selected.min(count - 1);
        match key.code {
            KeyCode::Esc => {
                self.state.ui.menu_open = None;
                self.restore_focus_status();
            }
            KeyCode::Left => {
                let total = crate::commands::MENU_BAR.len();
                self.state.ui.menu_open = Some((open + total - 1) % total);
                self.state.ui.menu_selected = 0;
            }
            KeyCode::Right => {
                let total = crate::commands::MENU_BAR.len();
                self.state.ui.menu_open = Some((open + 1) % total);
                self.state.ui.menu_selected = 0;
            }
            KeyCode::Up => {
                self.state.ui.menu_selected = self.state.ui.menu_selected.saturating_sub(1);
            }
            KeyCode::Down => {
                self.state.ui.menu_selected = (self.state.ui.menu_selected + 1).min(count - 1);
            }
            KeyCode::Home => {
                self.state.ui.menu_selected = 0;
            }
            KeyCode::End => {
                self.state.ui.menu_selected = count - 1;
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                self.run_menu_item(open, self.state.ui.menu_selected);
            }
            _ => {}
        }
        true
    }

    /// Runs a menu item's action or command, then closes the menu. Focus
    /// stays where it was; the item's own status replaces the menu hint.
    fn run_menu_item(&mut self, menu: usize, item: usize) {
        let effect = crate::commands::menu_items(menu, self.state.window_count())
            .get(item)
            .map(|entry| entry.effect.clone());
        self.state.ui.menu_open = None;
        self.state.ui.menu_selected = 0;
        self.state.ui.menu_armed = false;
        let Some(effect) = effect else {
            self.restore_focus_status();
            return;
        };
        match effect {
            crate::commands::MenuEffect::Action(action) => self.apply_action(action),
            crate::commands::MenuEffect::Command(command) => {
                self.dispatch(UiEvent::Command(command))
            }
        }
    }

    /// Option picker for settings rows: arrows/Home/End move, Enter or
    /// Right confirms, Esc or Left closes. Consumes every key while open
    /// (mirroring the chat pickers) so row navigation cannot double-handle.
    fn handle_settings_picker_key(&mut self, key: KeyEvent) -> bool {
        if self.state.panels.settings_picker.is_none() {
            return false;
        }
        let visible = crate::state::settings_picker_list_rect(
            self.full_area(),
            self.state
                .panels
                .settings_picker
                .as_ref()
                .map(|picker| picker.options.len())
                .unwrap_or(0),
        )
        .height as usize;
        match key.code {
            KeyCode::Esc | KeyCode::Left => {
                self.cancel_settings_picker();
                true
            }
            KeyCode::Up => {
                if let Some(picker) = self.state.panels.settings_picker.as_mut() {
                    picker.move_up();
                    picker.clamp_to(visible.max(1));
                }
                self.preview_settings_picker_theme();
                true
            }
            KeyCode::Down => {
                if let Some(picker) = self.state.panels.settings_picker.as_mut() {
                    picker.move_down();
                    picker.clamp_to(visible.max(1));
                }
                self.preview_settings_picker_theme();
                true
            }
            KeyCode::Home => {
                if let Some(picker) = self.state.panels.settings_picker.as_mut() {
                    picker.move_home();
                }
                self.preview_settings_picker_theme();
                true
            }
            KeyCode::End => {
                if let Some(picker) = self.state.panels.settings_picker.as_mut() {
                    picker.move_end();
                    picker.clamp_to(visible.max(1));
                }
                self.preview_settings_picker_theme();
                true
            }
            KeyCode::Enter | KeyCode::Right => {
                self.confirm_settings_picker();
                true
            }
            _ => true,
        }
    }

    fn submit_prompt(&mut self, prompt: PromptState) {
        let value = prompt.value.trim().to_string();
        match prompt.kind {
            PromptKind::Theme => match ThemeSelection::from_query(&value) {
                Some(theme) => {
                    self.apply_theme(theme, true);
                }
                None => {
                    let mut retry = PromptState::theme();
                    retry.value = prompt.value;
                    retry.hint = crate::theme::ThemeSelection::picker_hint().to_string();
                    self.state.ui.prompt = Some(retry);
                    self.state.set_status("Unknown theme name.");
                }
            },
            PromptKind::CustomColor => match self
                .state
                .ui
                .theme
                .with_custom_accent(&value, self.state.theme_context())
            {
                Ok(theme) => self.apply_theme(theme, true),
                Err(error) => {
                    let mut retry = PromptState::custom_color(Some(prompt.value.as_str()));
                    retry.hint = error;
                    self.state.ui.prompt = Some(retry);
                }
            },
            PromptKind::WordTarget => match parse_word_target(&value) {
                Ok(target) => self.apply_word_target(target),
                Err(error) => {
                    let mut retry = PromptState::word_target(self.state.ui.word_target);
                    retry.value = prompt.value;
                    retry.hint = error;
                    self.state.ui.prompt = Some(retry);
                }
            },
            PromptKind::ParagraphAlignment => match parse_alignment_input(&value) {
                Ok(alignment) => self.apply_paragraph_alignment(alignment),
                Err(error) => {
                    let mut retry = PromptState::paragraph_alignment(
                        self.current_text_buffer().current_alignment(),
                    );
                    retry.value = prompt.value;
                    retry.hint = error;
                    self.state.ui.prompt = Some(retry);
                }
            },
            PromptKind::SavePath => {
                match parse_save_path_input(&value, &self.state.editor().title()) {
                    Ok(path) => self.spawn_save_to(path),
                    Err(error) => {
                        let mut retry = PromptState::save_path(Some(prompt.value.as_str()));
                        retry.hint = error;
                        self.state.ui.prompt = Some(retry);
                    }
                }
            }
            PromptKind::OpenPath => match parse_open_path_input(&value) {
                Ok(path) => self.open_or_focus_path(path),
                Err(error) => {
                    let mut retry = PromptState::open_path(Some(prompt.value.as_str()));
                    retry.hint = error;
                    self.state.ui.prompt = Some(retry);
                }
            },
            PromptKind::WorkspacePath => match parse_workspace_path_input(&value) {
                Ok(path) => self.add_workspace(path),
                Err(error) => {
                    let mut retry = PromptState::workspace_path(Some(prompt.value.as_str()));
                    retry.hint = error;
                    self.state.ui.prompt = Some(retry);
                }
            },
            PromptKind::DocumentStatus => self.apply_document_status(value),
            PromptKind::BackupDirectory => self.apply_backup_location(value),
            PromptKind::RenameBrowserEntry { source, is_dir } => {
                match parse_browser_target_input(&value, &source) {
                    Ok(target) => {
                        if let Err(error) =
                            self.rename_browser_entry(source.clone(), target, is_dir)
                        {
                            let mut retry = PromptState::rename_browser_entry(&source, is_dir);
                            retry.value = prompt.value;
                            retry.hint = error;
                            self.state.ui.prompt = Some(retry);
                        }
                    }
                    Err(error) => {
                        let mut retry = PromptState::rename_browser_entry(&source, is_dir);
                        retry.value = prompt.value;
                        retry.hint = error;
                        self.state.ui.prompt = Some(retry);
                    }
                }
            }
            PromptKind::CopyBrowserEntry { source, is_dir } => {
                match parse_browser_target_input(&value, &source) {
                    Ok(target) => {
                        if let Err(error) = self.copy_browser_entry(source.clone(), target, is_dir)
                        {
                            let mut retry = PromptState::copy_browser_entry(
                                &source,
                                is_dir,
                                prompt.value.as_str(),
                            );
                            retry.hint = error;
                            self.state.ui.prompt = Some(retry);
                        }
                    }
                    Err(error) => {
                        let mut retry =
                            PromptState::copy_browser_entry(&source, is_dir, prompt.value.as_str());
                        retry.hint = error;
                        self.state.ui.prompt = Some(retry);
                    }
                }
            }
            PromptKind::FindText => {
                if value.is_empty() {
                    self.state.ui.prompt = Some(retry_prompt(
                        PromptState::find(None),
                        prompt.value,
                        "Enter text to find",
                    ));
                    return;
                }
                self.run_find(value.clone());
                // Keep the find bar open so Left/Right can walk matches
                // without another command chord.
                self.state.ui.prompt = Some(PromptState::find(Some(&value)));
            }
            PromptKind::FindReplaceQuery => {
                if value.is_empty() {
                    self.state.ui.prompt = Some(retry_prompt(
                        PromptState::find_replace_query(None),
                        prompt.value,
                        "Enter the text you want to replace",
                    ));
                    return;
                }
                self.state.ui.last_search = Some(value.clone());
                self.state
                    .ui
                    .prompt
                    .replace(PromptState::find_replace_value(&value, None));
            }
            PromptKind::FindReplaceValue { query } => self.run_replace(&query, value),
            PromptKind::GoToLine => match parse_line_number_input(&value) {
                Ok(line) => self.run_go_to_line(line),
                Err(error) => {
                    self.state.ui.prompt =
                        Some(retry_prompt(PromptState::go_to_line(), prompt.value, error));
                }
            },
            PromptKind::WriteBlock => match parse_block_target_input(&value) {
                Ok(path) => self.run_write_block(path),
                Err(error) => {
                    self.state.ui.prompt = Some(retry_prompt(
                        PromptState::write_block(None),
                        prompt.value,
                        error,
                    ));
                }
            },
            PromptKind::ReadFile => match parse_block_target_input(&value) {
                Ok(path) => self.run_read_file(path),
                Err(error) => {
                    self.state.ui.prompt = Some(retry_prompt(
                        PromptState::read_file(None),
                        prompt.value,
                        error,
                    ));
                }
            },
        }
    }

    fn dispatch(&mut self, event: UiEvent<Command, AppMessage>) {
        // F1,<key>: with the help panel open, a command key jumps to the
        // tab that documents it instead of running: per-command help lookup.
        // Tab, Shift+Tab and the horizontal arrows keep switching tabs.
        if let UiEvent::Command(command) = &event {
            if self.state.panels.panel == Some(PanelKind::Help)
                && !self.state.show_welcome_screen()
                && !help_panel_navigation(*command)
            {
                if let Some(tab) = help_jump_target(*command) {
                    self.state.panels.help_tab = tab;
                    self.state.panels.scroll = 0;
                    return;
                }
            }
        }
        match &event {
            UiEvent::Message(message) => {
                self.apply_message(message.clone());
                return;
            }
            UiEvent::Resize(area) => {
                self.state.update_layout(*area);
                self.refresh_window_views();
                if self.state.panels.is(PanelKind::Notes) {
                    let content = panel_content_rect(&self.state);
                    let (notes_width, _) = wrap_and_gutter(
                        content.width as usize,
                        &self.state.notes().buffer,
                        self.state.ui.show_line_numbers,
                    );
                    self.state
                        .notes_mut()
                        .ensure_cursor_visible(notes_width, content.height.max(1) as usize);
                }
                return;
            }
            UiEvent::Tick => {
                self.state.ui.local_hour = current_local_hour();
                // The system-theme probe runs a `defaults` process on macOS;
                // checked every ~2s rather than on every tick.
                if self.state.ui.tick_count.is_multiple_of(8)
                    && matches!(self.state.ui.theme, ThemeSelection::System)
                {
                    self.state.ui.system_is_dark = detect_system_dark_mode();
                }
                // Transient messages fall back to the focus hint on their
                // own; persistent ones (focus hints, live cues) never arm
                // the timer in the first place.
                if self
                    .state
                    .ui
                    .status_expires_at
                    .is_some_and(|at| Instant::now() >= at)
                {
                    self.state.ui.status_expires_at = None;
                    self.restore_focus_status();
                }
                return;
            }
            _ => {}
        }

        let mut cx = EventCx::new();
        match &event {
            UiEvent::Command(Command::Quit) => cx.emit(Action::Quit),
            UiEvent::Command(Command::NewDraft) => cx.emit(Action::NewDraft),
            UiEvent::Command(Command::Save) => cx.emit(Action::SaveRequested),
            UiEvent::Command(Command::TrashFile) => {
                cx.emit(Action::TrashRequested { confirm: true })
            }
            UiEvent::Command(Command::OpenSavePathPrompt) => cx.emit(Action::OpenSavePathPrompt),
            UiEvent::Command(Command::OpenFilePrompt) => cx.emit(Action::OpenFilePrompt),
            UiEvent::Command(Command::CycleTheme) => cx.emit(Action::CycleTheme),
            UiEvent::Command(Command::NextPage) => cx.emit(Action::NextPage),
            UiEvent::Command(Command::PreviousPage) => cx.emit(Action::PreviousPage),
            UiEvent::Command(Command::MovePageUp) => {
                let index = self.selected_page_or_active();
                cx.emit(Action::MoveDraft { index, delta: -1 });
            }
            UiEvent::Command(Command::MovePageDown) => {
                let index = self.selected_page_or_active();
                cx.emit(Action::MoveDraft { index, delta: 1 });
            }
            UiEvent::Command(Command::ToggleRuler) => cx.emit(Action::ToggleRuler),
            UiEvent::Command(Command::ToggleMenuBar) => cx.emit(Action::ToggleMenuBar),
            UiEvent::Command(Command::NextWindow) => cx.emit(Action::NextWindow),
            UiEvent::Command(Command::CloseWindow) => {
                if self.state.window_count() > 1 {
                    cx.emit(Action::CloseWindow);
                } else {
                    self.state
                        .set_status("One window is open. \u{2325}N opens another.".to_string());
                }
            }

            UiEvent::Command(Command::OpenThemePrompt) => cx.emit(Action::OpenThemePrompt),
            UiEvent::Command(Command::OpenCustomColorPrompt) => {
                cx.emit(Action::OpenCustomColorPrompt)
            }
            UiEvent::Command(Command::OpenWordTargetPrompt) => {
                cx.emit(Action::OpenWordTargetPrompt)
            }
            UiEvent::Command(Command::OpenAlignmentPrompt) => cx.emit(Action::OpenAlignmentPrompt),
            UiEvent::Command(Command::ToggleNotes) => {
                cx.emit(Action::TogglePanel(PanelKind::Notes))
            }
            UiEvent::Command(Command::ToggleZoom) => cx.emit(Action::ToggleZoom),
            UiEvent::Command(Command::ToggleHelp) => cx.emit(Action::TogglePanel(PanelKind::Help)),
            UiEvent::Command(Command::ToggleStats) => {
                cx.emit(Action::TogglePanel(PanelKind::Stats))
            }
            UiEvent::Command(Command::ToggleSettings) => {
                cx.emit(Action::TogglePanel(PanelKind::Settings))
            }
            UiEvent::Command(Command::ToggleRevisions) => {
                cx.emit(Action::TogglePanel(PanelKind::Revisions))
            }
            // The ^O alignment strokes, straight to the action so no
            // prompt interrupts the flow.
            UiEvent::Command(Command::AlignLeft) => {
                cx.emit(Action::AlignParagraph(ParagraphAlignment::Left))
            }
            UiEvent::Command(Command::AlignCenter) => {
                cx.emit(Action::AlignParagraph(ParagraphAlignment::Center))
            }
            UiEvent::Command(Command::AlignRight) => {
                cx.emit(Action::AlignParagraph(ParagraphAlignment::Right))
            }
            UiEvent::Command(Command::AlignJustify) => {
                cx.emit(Action::AlignParagraph(ParagraphAlignment::Justify))
            }
            UiEvent::Command(Command::OpenWorkspacePrompt) => cx.emit(Action::OpenWorkspacePrompt),
            UiEvent::Command(Command::OpenStatusPrompt) => cx.emit(Action::OpenStatusPrompt),
            _ => self.root.handle_event(&mut self.state, &event, &mut cx),
        }

        let (actions, _focus_request) = cx.into_parts();
        for action in actions {
            self.apply_action(action);
        }
    }

    fn apply_action(&mut self, action: Action) {
        // An armed trash/close survives only a confirming Ctrl+R: any other
        // action disarms it and wipes its message before running.
        if !matches!(action, Action::TrashRequested { .. })
            && (self.state.ui.pending_trash.is_some()
                || self.state.ui.pending_close_draft.is_some())
        {
            self.state.ui.pending_trash = None;
            self.state.ui.pending_close_draft = None;
            self.restore_focus_status();
        }
        match action {
            Action::Quit => self.request_quit(),
            Action::NewDraft => self.create_new_draft(),
            Action::SaveRequested => {
                if self.state.editor().file_path.is_some() {
                    self.spawn_save();
                } else {
                    self.open_save_path_prompt();
                }
            }
            Action::TrashRequested { confirm } => self.trash_requested(confirm),
            Action::OpenSavePathPrompt => self.open_save_path_prompt(),
            Action::OpenFilePrompt => self.open_file_prompt(),
            Action::OpenPath(path) => self.open_or_focus_path(path),
            Action::CycleTheme => {
                let next = self.state.ui.theme.quick_cycle(self.state.theme_context());
                self.apply_theme(next, true);
            }
            Action::CycleThemePrevious => {
                let previous = self
                    .state
                    .ui
                    .theme
                    .quick_cycle_previous(self.state.theme_context());
                self.apply_theme(previous, true);
            }
            Action::NextPage => self.next_page(),
            Action::PreviousPage => self.previous_page(),
            Action::SwitchDraft(index) => self.switch_to_draft(index),
            Action::MoveDraft { index, delta } => {
                if !self.state.move_draft(index, delta) {
                    let edge = if delta < 0 { "first" } else { "last" };
                    self.state.set_status(format!("Already the {edge} page."));
                }
            }
            Action::AlignParagraph(alignment) => self.apply_paragraph_alignment(alignment),
            Action::TogglePanel(kind) => self.toggle_panel(kind),
            Action::ToggleRuler => self.toggle_ruler(),
            Action::ToggleMenuBar => self.toggle_menu_bar(),
            Action::NextWindow => self.next_window(),
            Action::CloseWindow => self.close_window(),
            Action::MoveWindowToSlot(slot) => self.move_window_to_slot(slot),
            Action::About => self.show_about(),
            Action::OpenThemePrompt => {
                let mut prompt = PromptState::theme();
                prompt.hint = crate::theme::ThemeSelection::picker_hint().to_string();
                self.state.ui.prompt = Some(prompt);
            }
            Action::OpenCustomColorPrompt => {
                let current = self.state.ui.theme.custom_accent();
                self.state.ui.prompt = Some(PromptState::custom_color(current));
            }
            Action::OpenWordTargetPrompt => {
                self.state.ui.prompt = Some(PromptState::word_target(self.state.ui.word_target));
            }
            Action::OpenAlignmentPrompt => {
                self.state.ui.prompt = Some(PromptState::paragraph_alignment(
                    self.current_text_buffer().current_alignment(),
                ));
            }
            Action::ToggleAutosave => self.toggle_autosave(),
            Action::CycleLanguage => self.open_settings_picker(SettingsPickerKind::Language),
            Action::CycleIndentWidth => self.cycle_indent_width(),
            Action::ToggleLiveCounts => self.toggle_live_counts(),
            Action::ToggleZoom => self.toggle_zoom(),
            Action::ToggleBackup => self.toggle_backup(),
            Action::ToggleBackupOnClose => self.toggle_backup_on_close(),
            Action::CycleBackupKeep => self.cycle_backup_keep(),
            Action::BackupLocationPrompt => self.open_backup_location_prompt(),
            Action::BackupNow => self.backup_now(),
            Action::FindRequested(seed) => self.open_find_prompt(seed),
            Action::ReplaceRequested(seed) => self.open_replace_prompt(seed),
            Action::FindNextRequested => self.find_next_requested(),
            Action::GoToLineRequested => self.open_go_to_line_prompt(),
            Action::ToggleInsertMode => self.toggle_insert_mode(),
            Action::ToggleLineNumbers => self.toggle_line_numbers(),
            Action::WriteBlockRequested => self.open_write_block_prompt(),
            Action::ReadFileRequested => self.open_read_file_prompt(),
            Action::SaveAndCloseRequested => self.save_and_close(),
            Action::SaveAndExitRequested => self.save_and_exit(),
            Action::CopyRequested => self.copy_requested(),
            Action::CutRequested => self.cut_requested(),
            Action::SortSelectedLines => self.sort_selected_lines(),
            Action::DropQuickBookmark => self.drop_quick_bookmark(),
            Action::JumpLastBookmark => self.jump_last_bookmark(),
            Action::PasteRequested => self.paste_requested(),
            Action::OpenWorkspacePrompt => self.open_workspace_prompt(),
            Action::OpenStatusPrompt => self.open_status_prompt(),
            Action::OpenSettingsPicker(kind) => self.open_settings_picker(kind),
            Action::RemoveWorkspace(path) => self.remove_workspace(path),
            Action::RestoreSelectedRevision => self.restore_selected_revision(),
            Action::SetStatus(status) => self.state.set_status(status),
        }
    }

    fn apply_theme(&mut self, theme: ThemeSelection, persist: bool) {
        self.state.ui.theme = theme.sanitize();
        let name = self.state.theme_name();
        if persist {
            if let Err(error) = self.persist_settings() {
                self.state.set_status(format!(
                    "Theme changed to {name}, but settings could not be saved: {error}"
                ));
                return;
            }
        }
        self.state.set_status_quiet(format!(
            "Theme switched to {name}. ⌃T cycles, ⌃⇧T opens the picker."
        ));
    }

    fn apply_word_target(&mut self, target: Option<usize>) {
        self.state.ui.word_target = target;
        if let Err(error) = self.persist_settings() {
            self.state.set_status(format!(
                "Word target updated, but settings could not be saved: {error}"
            ));
            return;
        }

        match self.state.word_target_progress() {
            Some(progress) if progress.met => self.state.set_status_quiet(format!(
                "Word target set to {}. Target already met by {} words.",
                progress.target, progress.current
            )),
            Some(progress) => self.state.set_status_quiet(format!(
                "Word target set to {}. {} words remaining.",
                progress.target, progress.remaining
            )),
            None => self.state.set_status_quiet("Word target cleared."),
        }
    }

    fn editor_text_width(&self) -> usize {
        // The keyboard window's text width, not the whole editor's: a split
        // halves the wrap width the cursor math must respect.
        let text_area = window_text_area(&self.state, self.state.active_editor_area());
        wrap_and_gutter(
            text_area.width as usize,
            &self.state.editor().buffer,
            self.state.ui.show_line_numbers,
        )
        .0
    }

    fn editor_text_height(&self) -> usize {
        (window_text_area(&self.state, self.state.active_editor_area()).height as usize).max(1)
    }

    fn sync_editor_view(&mut self) {
        let (width, height) = (self.editor_text_width(), self.editor_text_height());
        self.state.editor_mut().ensure_cursor_visible(width, height);
    }

    fn open_find_prompt(&mut self, seed: Option<String>) {
        let seed = seed
            .filter(|value| !value.trim().is_empty())
            .or_else(|| self.state.ui.last_search.clone());
        self.state.ui.prompt = Some(PromptState::find(seed.as_deref()));
    }

    fn open_replace_prompt(&mut self, seed: Option<String>) {
        let seed = seed
            .filter(|value| !value.trim().is_empty())
            .or_else(|| self.state.ui.last_search.clone());
        self.state
            .ui
            .prompt
            .replace(PromptState::find_replace_query(seed.as_deref()));
    }

    fn run_find(&mut self, query: String) {
        self.run_find_direction(&query, true);
    }

    fn run_find_direction(&mut self, query: &str, forward: bool) {
        if query.trim().is_empty() {
            return;
        }
        self.state.ui.last_search = Some(query.to_string());
        let options = self.state.ui.search_options;
        let flags = search_flag_suffix(options);
        let result = if forward {
            self.state
                .editor_mut()
                .buffer
                .find_next_with(query, &options)
        } else {
            self.state
                .editor_mut()
                .buffer
                .find_previous_with(query, &options)
        };
        match result {
            Ok(Some((index, total, _, _))) => {
                self.sync_editor_view();
                self.state.set_status_quiet(format!(
                    "Match {index} of {total} for '{query}'{flags}. ← previous · → next."
                ));
            }
            Ok(None) => self
                .state
                .set_status(format!("No matches for '{query}'{flags}.")),
            Err(error) => {
                let mut retry = PromptState::find(Some(query));
                retry.hint = format!("{error} [{}]", options.describe());
                self.state.ui.prompt = Some(retry);
            }
        }
    }

    fn find_next_requested(&mut self) {
        let Some(query) = self
            .state
            .ui
            .last_search
            .clone()
            .filter(|value| !value.trim().is_empty())
        else {
            self.state
                .set_status("No previous search yet. Opening the find prompt.".to_string());
            self.open_find_prompt(None);
            return;
        };
        self.run_find(query);
    }

    fn run_replace(&mut self, query: &str, replacement: String) {
        if query.trim().is_empty() {
            return;
        }
        let options = self.state.ui.search_options;
        let flags = search_flag_suffix(options);
        let outcome = self
            .state
            .editor_mut()
            .buffer
            .replace_current_and_find_next_with(query, &replacement, &options);
        match outcome {
            Ok(Some((index, total))) => {
                self.state.editor_mut().mark_dirty();
                self.sync_editor_view();
                self.state.set_status_quiet(format!(
                    "Replaced one '{query}'{flags}. Now on match {index} of {total}."
                ));
            }
            Ok(None) => self.state.set_status(format!(
                "No more matches for '{query}'{flags}. Press Tab to replace all."
            )),
            Err(error) => self.state.set_status(error),
        }
    }

    fn run_replace_all(&mut self, query: &str, replacement: String) {
        if query.trim().is_empty() {
            return;
        }
        let options = self.state.ui.search_options;
        let flags = search_flag_suffix(options);
        match self
            .state
            .editor_mut()
            .buffer
            .replace_all_with(query, &replacement, &options)
        {
            Ok(0) => {
                self.state
                    .set_status(format!("No matches for '{query}'{flags}."));
            }
            Ok(count) => {
                self.state.editor_mut().mark_dirty();
                self.sync_editor_view();
                self.state
                    .set_status_quiet(format!("Replaced {count} of '{query}'{flags}."));
            }
            Err(error) => self.state.set_status(error),
        }
    }

    fn open_go_to_line_prompt(&mut self) {
        self.state.ui.prompt = Some(PromptState::go_to_line());
    }

    fn run_go_to_line(&mut self, line: usize) {
        let total = self.state.editor().buffer.line_count();
        if line == 0 || line > total {
            self.state
                .set_status(format!("Line must be between 1 and {total}."));
            self.state.ui.prompt = Some(PromptState::go_to_line());
            return;
        }
        let width = self.editor_text_width();
        self.state.editor_mut().buffer.move_to_line(line, width);
        self.sync_editor_view();
        let (current, column) = self.state.editor().buffer.cursor_line_and_col(width);
        self.state
            .set_status_quiet(format!("Line {current}, column {column}."));
    }

    fn toggle_insert_mode(&mut self) {
        let insert = !self.state.ui.insert_mode;
        self.state.ui.insert_mode = insert;
        self.state.set_status_quiet(if insert {
            "Insert mode.".to_string()
        } else {
            "Overwrite mode.".to_string()
        });
    }

    fn toggle_line_numbers(&mut self) {
        let show = !self.state.ui.show_line_numbers;
        self.state.ui.show_line_numbers = show;
        let status = if show {
            "Line numbers on.".to_string()
        } else {
            "Line numbers off.".to_string()
        };
        if let Err(error) = self.persist_settings() {
            self.state
                .set_status(format!("{status} But settings could not be saved: {error}"));
        } else {
            self.state.set_status_quiet(status);
        }
    }

    fn toggle_autosave(&mut self) {
        let enabled = !self.state.ui.autosave_enabled;
        self.state.ui.autosave_enabled = enabled;
        let status = if enabled {
            "Autosave on.".to_string()
        } else {
            "Autosave off. ⌃P still saves.".to_string()
        };
        if let Err(error) = self.persist_settings() {
            self.state
                .set_status(format!("{status} But settings could not be saved: {error}"));
        } else {
            self.state.set_status_quiet(status);
        }
    }

    fn cycle_indent_width(&mut self) {
        let next = match self.state.ui.indent_width {
            2 => 4,
            4 => 8,
            _ => 2,
        };
        self.apply_indent_width(next);
    }

    fn apply_indent_width(&mut self, next: usize) {
        self.state.ui.indent_width = next;
        for editor in self.state.drafts.iter_mut().map(|d| &mut d.editor) {
            editor.buffer.set_indent_width(next);
        }
        if let Err(error) = self.persist_settings() {
            self.state.set_status(format!(
                "Indent width {next}. But settings could not be saved: {error}"
            ));
        } else {
            self.state
                .set_status_quiet(format!("Indent width {next} spaces."));
        }
    }

    fn toggle_live_counts(&mut self) {
        let show = !self.state.ui.show_live_counts;
        self.state.ui.show_live_counts = show;
        let status = if show {
            "Live counts on.".to_string()
        } else {
            "Live counts off.".to_string()
        };
        if let Err(error) = self.persist_settings() {
            self.state
                .set_status(format!("{status} But settings could not be saved: {error}"));
        } else {
            self.state.set_status_quiet(status);
        }
    }

    /// Fullscreen-editor zoom. Zooming in hides the ruler row and the hint
    /// row so the text fills the screen; zooming out restores them. The
    /// terminal's own font size stays in charge of glyph size (⌃+/- there).
    fn toggle_zoom(&mut self) {
        if self.state.ui.zoomed {
            self.state.ui.zoomed = false;
            self.state
                .set_status_quiet("Zoomed out. F11 zooms back in.".to_string());
            return;
        }
        self.state.ui.zoomed = true;
        self.state.set_status_quiet(
            "Zoomed in: the document fills the screen. F11 zooms out.".to_string(),
        );
    }

    /// Alt-R / View menu: the ruler row above the text.
    fn toggle_ruler(&mut self) {
        let show = !self.state.ui.show_ruler;
        self.state.ui.show_ruler = show;
        let status = if show {
            "Ruler on.".to_string()
        } else {
            "Ruler off.".to_string()
        };
        if let Err(error) = self.persist_settings() {
            self.state
                .set_status(format!("{status} But settings could not be saved: {error}"));
        } else {
            self.state.set_status_quiet(status);
        }
    }

    /// Alt-M / View menu: the top bar with the menus and the page name.
    /// Hiding it hands its row back to the text; F10 still drops the menus
    /// over the first line.
    fn toggle_menu_bar(&mut self) {
        let show = !self.state.ui.show_menu_bar;
        self.state.ui.show_menu_bar = show;
        self.state.ui.menu_open = None;
        self.state.ui.menu_armed = false;
        let status = if show {
            "Menu bar on.".to_string()
        } else {
            "Menu bar off. F10 still opens the menus.".to_string()
        };
        if let Err(error) = self.persist_settings() {
            self.state
                .set_status(format!("{status} But settings could not be saved: {error}"));
        } else {
            self.state.set_status_quiet(status);
        }
    }

    /// Help menu's About row. This is a sheet, not a status note: build,
    /// session, and document status live in one place the reader can dismiss.
    fn show_about(&mut self) {
        self.state.ui.about_open = true;
        self.state.ui.menu_open = None;
        self.state.ui.prefix = None;
        self.state.ui.status.clear();
        self.state.ui.status_expires_at = None;
    }

    fn close_about(&mut self) {
        if !self.state.ui.about_open {
            return;
        }
        self.state.ui.about_open = false;
        self.restore_focus_status();
    }

    /// Opening a pane while zoomed first restores the stashed layout, so the
    /// pane the user asked for is actually visible.
    fn unzoom_for_pane(&mut self) {
        if !self.state.ui.zoomed {
            return;
        }
        self.state.ui.zoomed = false;
    }

    fn toggle_backup(&mut self) {
        let enabled = !self.state.ui.backup_enabled;
        self.state.ui.backup_enabled = enabled;
        let status = if enabled {
            "Automatic backups on.".to_string()
        } else {
            "Automatic backups off.".to_string()
        };
        if let Err(error) = self.persist_settings() {
            self.state
                .set_status(format!("{status} But settings could not be saved: {error}"));
        } else {
            self.state.set_status_quiet(status);
        }
    }

    fn toggle_backup_on_close(&mut self) {
        let enabled = !self.state.ui.backup_on_close;
        self.state.ui.backup_on_close = enabled;
        let status = if enabled {
            "Back up on close on.".to_string()
        } else {
            "Back up on close off.".to_string()
        };
        if let Err(error) = self.persist_settings() {
            self.state
                .set_status(format!("{status} But settings could not be saved: {error}"));
        } else {
            self.state.set_status_quiet(status);
        }
    }

    fn cycle_backup_keep(&mut self) {
        const KEEPS: [usize; 5] = [1, 3, 5, 10, 25];
        let position = KEEPS
            .iter()
            .position(|keep| *keep >= self.state.ui.backup_keep)
            .map(|index| (index + 1) % KEEPS.len())
            .unwrap_or(2);
        self.state.ui.backup_keep = KEEPS[position];
        let status = format!("Keeping {} recent backups.", KEEPS[position]);
        if let Err(error) = self.persist_settings() {
            self.state
                .set_status(format!("{status} But settings could not be saved: {error}"));
        } else {
            self.state.set_status_quiet(status);
        }
    }

    fn open_backup_location_prompt(&mut self) {
        let current = self.effective_backup_dir().display().to_string();
        let mut prompt = PromptState::backup_directory(&current);
        prompt.hint = "Empty resets to the default backup folder.".to_string();
        self.state.ui.prompt = Some(prompt);
    }

    fn apply_backup_location(&mut self, value: String) {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            self.state.ui.backup_dir = String::new();
            if let Err(error) = self.persist_settings() {
                self.state.set_status(format!(
                    "Backup folder reset, but settings could not be saved: {error}"
                ));
            } else {
                self.state.set_status_quiet(format!(
                    "Backup folder reset to {}.",
                    self.effective_backup_dir().display()
                ));
            }
            return;
        }
        let path = normalize_document_path(expand_path(PathBuf::from(trimmed)));
        if let Err(error) = std::fs::create_dir_all(&path) {
            let mut retry = PromptState::backup_directory(&value);
            retry.hint = format!("Cannot use {}: {error}", path.display());
            self.state.ui.prompt = Some(retry);
            return;
        }
        if !path.is_dir() {
            let mut retry = PromptState::backup_directory(&value);
            retry.hint = format!("Not a directory: {}", path.display());
            self.state.ui.prompt = Some(retry);
            return;
        }
        self.state.ui.backup_dir = path.display().to_string();
        if let Err(error) = self.persist_settings() {
            self.state.set_status(format!(
                "Backup folder set, but settings could not be saved: {error}"
            ));
        } else {
            self.state
                .set_status_quiet(format!("Backups go to {}.", path.display()));
        }
    }

    /// The backup folder in force: the custom one, or the default beside
    /// settings when blank.
    fn effective_backup_dir(&self) -> PathBuf {
        let custom = self.state.ui.backup_dir.trim();
        if custom.is_empty() {
            match crate::settings::settings_path() {
                Ok(path) => crate::backup::default_backup_dir(&path),
                Err(_) => PathBuf::from("Backups"),
            }
        } else {
            normalize_document_path(expand_path(PathBuf::from(custom)))
        }
    }

    /// Copy every draft that lives on disk into the backup folder, pruning
    /// each file to the newest `backup_keep`. Returns (backed up, failures).
    fn run_backup(&mut self) -> (usize, Vec<String>) {
        let dir = self.effective_backup_dir();
        let keep = self.state.ui.backup_keep.max(1);
        let now = crate::backup::now_millis();
        let mut done = 0usize;
        let mut failed = Vec::new();
        for index in 0..self.state.drafts.len() {
            let job = self.state.drafts.get(index).and_then(|draft| {
                let path = draft.editor.file_path.clone()?;
                Some((path, draft.editor.title()))
            });
            let Some((path, title)) = job else {
                continue;
            };
            if !path.exists() {
                continue;
            }
            match crate::backup::backup_file(&path, &dir, now) {
                Ok(_) => {
                    let name = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("");
                    crate::backup::prune_backups(&dir, name, keep);
                    done += 1;
                }
                Err(_) => failed.push(title),
            }
        }
        (done, failed)
    }

    /// Manual "Back up now": always runs, even with automatic backups off.
    fn backup_now(&mut self) {
        let dir = self.effective_backup_dir();
        let (done, failed) = self.run_backup();
        if done == 0 && failed.is_empty() {
            self.state
                .set_status("Nothing to back up: no saved files open.".to_string());
        } else if failed.is_empty() {
            self.state.set_status_quiet(format!(
                "Backed up {done} file{} to {}.",
                if done == 1 { "" } else { "s" },
                dir.display()
            ));
        } else {
            self.state.set_status(format!(
                "Backed up {done} file(s); failed: {}.",
                failed.join(", ")
            ));
        }
    }

    /// Copy the selection, or the whole current line when nothing is selected.
    /// Lands on the internal ring and the system clipboard (OSC 52 plus the
    /// platform tool) so pasting works inside Ordet and in other apps.
    fn copy_requested(&mut self) {
        let (text, what) = {
            let buffer = self.current_text_buffer();
            match buffer.selected_text() {
                Some(text) => (text, "selection"),
                None => (buffer.current_line_text(), "line"),
            }
        };
        if text.is_empty() {
            self.state.set_status("Nothing to copy.".to_string());
            return;
        }
        let chars = text.chars().count();
        self.state.clipboard.copy(&text);
        crate::clipboard::write_system_clipboard(&text);
        self.state
            .ui
            .pending_overlays
            .push(crate::clipboard::osc52_sequence(&text));
        // Clear the highlight so the next Ctrl-V inserts instead of replacing
        // the just-copied text (which reads as "first paste does nothing").
        // Cursor stays where the selection ended, so Ctrl-A Ctrl-C Ctrl-V
        // duplicates in a single press.
        if self.focused_notes_buffer() {
            self.state.notes_mut().buffer.collapse_selection();
        } else {
            self.state.editor_mut().buffer.collapse_selection();
        }
        self.state
            .set_status_quiet(format!("Copied {chars} chars ({what})."));
    }

    /// Copy, then delete the copied text. Line cut removes the whole line,
    /// newline included, so no blank line is left behind.
    fn cut_requested(&mut self) {
        let (text, has_selection) = {
            let buffer = self.current_text_buffer();
            match buffer.selected_text() {
                Some(text) => (text, true),
                None => (buffer.current_line_text(), false),
            }
        };
        if text.is_empty() {
            self.state.set_status("Nothing to cut.".to_string());
            return;
        }
        let chars = text.chars().count();
        self.state.clipboard.copy(&text);
        crate::clipboard::write_system_clipboard(&text);
        self.state
            .ui
            .pending_overlays
            .push(crate::clipboard::osc52_sequence(&text));
        let deleted = if self.focused_notes_buffer() {
            let notes = self.state.notes_mut();
            let ok = if has_selection {
                notes.buffer.delete_block()
            } else {
                notes.buffer.delete_line()
            };
            if ok {
                notes.mark_dirty();
            }
            ok
        } else {
            let editor = self.state.editor_mut();
            let ok = if has_selection {
                editor.buffer.delete_block()
            } else {
                editor.buffer.delete_line()
            };
            if ok {
                editor.mark_dirty();
            }
            ok
        };
        if deleted {
            self.state.set_status_quiet(format!("Cut {chars} chars."));
        } else {
            self.state
                .set_status_quiet("Copied, but there was nothing to delete.".to_string());
        }
    }

    /// Ctrl-V / Alt-V pressed: paste on this very press, wherever focus is.
    ///
    /// Prompts and the file browser own text input, so the clipboard lands in
    /// their field; everywhere else it goes to the focused buffer through
    /// [`Self::paste_requested`]. Never silently swallows: an empty clipboard
    /// reports instead, so the first press always answers.
    fn handle_paste_key(&mut self) {
        if self.state.ui.prompt.is_some() {
            match self.state.clipboard.paste() {
                Some((text, _)) => {
                    if let Some(prompt) = self.state.ui.prompt.as_mut() {
                        prompt
                            .value
                            .push_str(text.trim_end_matches(&['\n', '\r'][..]));
                    }
                }
                None => {
                    self.state.set_status("Clipboard is empty.".to_string());
                }
            }
            return;
        }
        if self.state.ui.file_browser.is_some() {
            match self.state.clipboard.paste() {
                Some((text, _)) => {
                    if let Some(mut browser) = self.state.ui.file_browser.take() {
                        for ch in text.chars() {
                            if ch == '\n' || ch == '\r' || ch.is_control() {
                                continue;
                            }
                            browser.push_char(ch);
                        }
                        self.state.set_status_quiet(browser.message.clone());
                        self.state.ui.file_browser = Some(browser);
                    }
                }
                None => {
                    self.state.set_status("Clipboard is empty.".to_string());
                }
            }
            return;
        }
        self.paste_requested();
    }

    /// Insert the system clipboard when it has text, else the kill ring.
    /// Inserting through `insert_text` keeps the paste undoable.
    fn paste_requested(&mut self) {
        let Some((text, source)) = self.state.clipboard.paste() else {
            self.state.set_status("Clipboard is empty.".to_string());
            return;
        };
        let chars = text.chars().count();
        if self.focused_notes_buffer() {
            let notes = self.state.notes_mut();
            notes.buffer.insert_text(&text);
            notes.mark_dirty();
        } else {
            let editor = self.state.editor_mut();
            editor.buffer.insert_text(&text);
            editor.mark_dirty();
        }
        self.state
            .set_status_quiet(format!("Pasted {chars} chars from {source}."));
    }

    fn open_status_prompt(&mut self) {
        let initial = self.state.editor().doc_status.clone();
        self.state.ui.prompt = Some(PromptState::document_status(&initial));
    }

    fn apply_document_status(&mut self, status: String) {
        let trimmed = status.trim().to_string();
        if trimmed.is_empty() {
            let mut retry = PromptState::document_status(&self.state.editor().doc_status.clone());
            retry.hint = "Enter a status like Draft.".to_string();
            self.state.ui.prompt = Some(retry);
            return;
        }
        let index = self.state.active_draft;
        self.state.editor_mut().doc_status = trimmed.clone();
        self.state.mark_draft_dirty(index);
        self.state
            .set_status_quiet(format!("Status set to {trimmed}. Save to keep it."));
    }

    fn open_workspace_prompt(&mut self) {
        let initial = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .display()
            .to_string();
        self.state.ui.prompt = Some(PromptState::workspace_path(Some(&initial)));
    }

    fn add_workspace(&mut self, path: PathBuf) {
        match self.state.add_workspace(path) {
            Ok(normalized) => {
                let name = normalized
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("folder");
                let (files, _) = self.state.workspace_summary(&normalized);
                let status = format!("Tracking {name} ({files} pages).");
                if let Err(error) = self.persist_settings() {
                    self.state.set_status(format!(
                        "Tracking {name}, but settings could not be saved: {error}"
                    ));
                } else {
                    self.state.set_status_quiet(status);
                }
            }
            Err(error) => {
                let mut retry = PromptState::workspace_path(None);
                retry.hint = error;
                self.state.ui.prompt = Some(retry);
            }
        }
    }

    /// Snapshots for the active draft's file, newest last. Empty when the
    /// draft was never saved: scratch text has no history yet.
    fn active_file_revisions(&self) -> Vec<crate::history::Revision> {
        self.state
            .editor()
            .file_path
            .as_ref()
            .map(|path| {
                self.state
                    .history
                    .revisions_for(&history_key_for(path))
                    .to_vec()
            })
            .unwrap_or_default()
    }

    /// Replace the buffer with the selected snapshot. Goes through
    /// `select_all` + `insert_text`, so one undo brings the current text back.
    fn restore_selected_revision(&mut self) {
        let revisions = self.active_file_revisions();
        if revisions.is_empty() {
            self.state.set_status(
                "No revisions yet. Save the draft first, then snapshots appear here.".to_string(),
            );
            return;
        }
        let selected = self.state.panels.revision_selected.min(revisions.len() - 1);
        let revision = &revisions[selected];
        if revision.text == self.state.editor().buffer.plain_text() {
            self.state
                .set_status_quiet("That revision already matches the draft.".to_string());
            return;
        }
        let when = relative_time(unix_now(), revision.timestamp);
        let text = revision.text.clone();
        let editor = self.state.editor_mut();
        editor.buffer.select_all();
        editor.buffer.insert_text(&text);
        editor.mark_dirty();
        self.state.set_status(format!(
            "Restored the revision from {when}. Undo brings it back."
        ));
    }

    fn remove_workspace(&mut self, path: PathBuf) {
        if self.state.remove_workspace(&path) {
            if let Err(error) = self.persist_settings() {
                self.state.set_status(format!(
                    "Stopped tracking that folder, but settings could not be saved: {error}"
                ));
            } else {
                self.state
                    .set_status_quiet("Stopped tracking that folder.".to_string());
            }
        }
    }

    fn open_write_block_prompt(&mut self) {
        self.state.ui.prompt = Some(PromptState::write_block(None));
    }

    fn run_write_block(&mut self, path: PathBuf) {
        let buffer = &self.state.editor().buffer;
        let block_only = buffer.selection_range().is_some();
        let content = buffer
            .selected_text()
            .unwrap_or_else(|| buffer.plain_text());
        if content.is_empty() {
            self.state.set_status(
                "Nothing to write. Mark a block or add text before writing.".to_string(),
            );
            return;
        }

        let chars = content.chars().count();
        let scope = if block_only { "block" } else { "document" };
        self.state.set_status_quiet(format!(
            "Writing {chars} character(s) from the {scope} to {} in the background...",
            Self::display_path_tail(&path)
        ));
        self.tasks
            .spawn(Some(TaskKey::from("write-block")), move |token| {
                if token.is_cancelled() {
                    return Err(TaskError::new("write block cancelled"));
                }
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() {
                        fs::create_dir_all(parent)
                            .map_err(|error| TaskError::new(error.to_string()))?;
                    }
                }
                fs::write(&path, &content).map_err(|error| TaskError::new(error.to_string()))?;
                if token.is_cancelled() {
                    return Err(TaskError::new("write block cancelled"));
                }
                Ok(AppMessage::WriteBlockCompleted {
                    path,
                    chars,
                    block_only,
                })
            });
    }

    fn open_read_file_prompt(&mut self) {
        self.state.ui.prompt = Some(PromptState::read_file(None));
    }

    fn run_read_file(&mut self, path: PathBuf) {
        let draft_id = self.state.active_draft_id();
        let revision = self.state.editor().revision;
        self.state.set_status_quiet(format!(
            "Reading {} into the cursor in the background...",
            Self::display_path_tail(&path)
        ));
        self.tasks
            .spawn(Some(TaskKey::from("read-file")), move |token| {
                if token.is_cancelled() {
                    return Err(TaskError::new("read file cancelled"));
                }
                let text =
                    fs::read_to_string(&path).map_err(|error| TaskError::new(error.to_string()))?;
                let chars = text.chars().count();
                if token.is_cancelled() {
                    return Err(TaskError::new("read file cancelled"));
                }
                Ok(AppMessage::ReadFileCompleted {
                    draft_id,
                    revision,
                    path,
                    text,
                    chars,
                })
            });
    }

    fn save_and_close(&mut self) {
        if self.state.drafts.len() <= 1 {
            self.state
                .set_status("This is the only draft. ⌃K X saves and exits instead.".to_string());
            return;
        }
        self.pending_after_save = CloseIntent::CloseDraft;
        self.save_active_draft();
    }

    fn save_and_exit(&mut self) {
        self.pending_after_save = CloseIntent::Exit;
        self.save_active_draft();
    }

    fn save_active_draft(&mut self) {
        if self.state.editor().file_path.is_some() {
            self.spawn_save();
        } else {
            self.open_save_path_prompt();
        }
    }

    fn apply_paragraph_alignment(&mut self, alignment: ParagraphAlignment) {
        if self.focused_notes_buffer() {
            let changed = self
                .state
                .notes_mut()
                .buffer
                .set_alignment_for_selection(alignment);
            if changed {
                self.state.notes_mut().mark_dirty();
                self.state.set_status_quiet(format!(
                    "Notes alignment set to {}.",
                    alignment.display_name()
                ));
            } else {
                self.state.set_status_quiet(format!(
                    "Notes are already {} aligned.",
                    alignment.display_name()
                ));
            }
        } else {
            let changed = self
                .state
                .editor_mut()
                .buffer
                .set_alignment_for_selection(alignment);
            if changed {
                self.state.editor_mut().mark_dirty();
                self.state.set_status_quiet(format!(
                    "Paragraph alignment set to {}.",
                    alignment.display_name()
                ));
            } else {
                self.state.set_status_quiet(format!(
                    "Paragraphs are already {} aligned.",
                    alignment.display_name()
                ));
            }
        }
    }

    fn current_text_buffer(&self) -> &EditorBuffer {
        if self.focused_notes_buffer() {
            &self.state.notes().buffer
        } else {
            &self.state.editor().buffer
        }
    }

    fn focused_notes_buffer(&self) -> bool {
        self.state.panels.is(PanelKind::Notes)
    }

    fn persist_settings(&self) -> Result<(), String> {
        save_settings(&AppSettings {
            theme: self.state.ui.theme.clone(),
            word_target: self.state.ui.word_target,
            recent_files: self.state.recent_files.clone(),
            ui_language: self.state.ui.language,
            show_line_numbers: self.state.ui.show_line_numbers,
            show_ruler: self.state.ui.show_ruler,
            show_menu_bar: self.state.ui.show_menu_bar,
            autosave_enabled: self.state.ui.autosave_enabled,
            indent_width: self.state.ui.indent_width,
            show_live_counts: self.state.ui.show_live_counts,
            backup_enabled: self.state.ui.backup_enabled,
            backup_on_close: self.state.ui.backup_on_close,
            backup_keep: self.state.ui.backup_keep,
            backup_dir: self.state.ui.backup_dir.clone(),
            workspaces: self
                .state
                .workspace_folders
                .iter()
                .map(|folder| folder.path.clone())
                .collect(),
        })
    }

    fn apply_language(&mut self, next: crate::i18n::UiLanguage) {
        self.state.ui.language = next;
        if let Err(error) = self.persist_settings() {
            self.state.set_status(format!(
                "Language changed, but settings could not be saved: {error}"
            ));
            return;
        }
        self.state
            .set_status_quiet(next.language_changed_status(next));
    }

    fn open_save_path_prompt(&mut self) {
        let initial = self.state.editor().default_save_path();
        let mut prompt = PromptState::save_path(initial.to_str());
        prompt.hint = "Enter a path like ./draft.ord, ./draft.txt, or ./draft.md".to_string();
        self.state.ui.prompt = Some(prompt);
    }

    fn open_file_prompt(&mut self) {
        let initial = self
            .state
            .editor()
            .file_path
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let browser = FileBrowserState::open(initial);
        self.state.set_status_quiet(browser.message.clone());
        self.state.ui.file_browser = Some(browser);
    }

    fn browser_rename_prompt(&self, browser: &FileBrowserState) -> Option<PromptState> {
        let entry = browser.selected_entry()?;
        Some(PromptState::rename_browser_entry(&entry.path, entry.is_dir))
    }

    fn browser_copy_prompt(&self, browser: &FileBrowserState) -> Option<PromptState> {
        let entry = browser.selected_entry()?;
        let suggested = suggest_copy_destination(&entry.path);
        Some(PromptState::copy_browser_entry(
            &entry.path,
            entry.is_dir,
            &suggested.display().to_string(),
        ))
    }

    fn rename_browser_entry(
        &mut self,
        source: PathBuf,
        target: PathBuf,
        source_is_dir: bool,
    ) -> Result<(), String> {
        if paths_match(&source, &target) {
            return Err("Choose a different name or destination.".to_string());
        }
        if target.exists() {
            return Err("That destination already exists.".to_string());
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("Failed to prepare {}: {error}", parent.display()))?;
        }
        fs::rename(&source, &target)
            .map_err(|error| format!("Failed to rename {}: {error}", source.display()))?;

        self.state
            .retarget_path_references(&source, &target, source_is_dir);
        // A rename can add, remove, or move workspace files, so cached
        // directory scans are rebuilt on next use.
        self.state.workspace_scans.borrow_mut().invalidate_all();
        let _ = self.persist_settings();
        self.refresh_browser_after_path_change(&target)?;
        self.state.set_status_quiet(format!(
            "Renamed {} to {}.",
            source
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| source.display().to_string()),
            target
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| target.display().to_string())
        ));
        Ok(())
    }

    fn copy_browser_entry(
        &mut self,
        source: PathBuf,
        target: PathBuf,
        source_is_dir: bool,
    ) -> Result<(), String> {
        if paths_match(&source, &target) {
            return Err("Choose a different destination for the copy.".to_string());
        }
        if target.exists() {
            return Err("That destination already exists.".to_string());
        }
        copy_path(&source, &target, source_is_dir)
            .map_err(|error| format!("Failed to copy {}: {error}", source.display()))?;

        self.refresh_browser_after_path_change(&target)?;
        self.state.set_status_quiet(format!(
            "Copied {} to {}.",
            source
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| source.display().to_string()),
            target
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| target.display().to_string())
        ));
        Ok(())
    }

    fn refresh_browser_after_path_change(&mut self, target: &Path) -> Result<(), String> {
        if let Some(browser) = &mut self.state.ui.file_browser {
            if let Some(parent) = target.parent() {
                browser.current_dir = parent.to_path_buf();
            }
            browser.search.clear();
            browser.selected = 0;
            browser.refresh()?;
            browser.select_path(target);
            browser.message = format!("Selected {}.", target.display());
        }
        Ok(())
    }

    /// Delete asks, Y (or a confirming Ctrl+R) confirms: `confirm` is true
    /// only for the Ctrl+R key. Anything armed fires solely on a confirming
    /// press for the same target; anything else re-arms or disarms.
    fn trash_requested(&mut self, confirm: bool) {
        let target = match self.trash_target_from_context() {
            Ok(path) => path,
            Err(error) => {
                // No file to trash: an unsaved page closes instead. Anything
                // else keeps the error.
                if let Some(index) = self.unsaved_draft_target() {
                    self.close_unsaved_draft(index, confirm);
                } else {
                    self.state.set_status(error);
                }
                return;
            }
        };
        if confirm && self.state.ui.pending_trash.as_ref() == Some(&target) {
            self.state.ui.pending_trash = None;
            self.trash_confirmed(target);
        } else {
            self.state.ui.pending_trash = Some(target.clone());
            self.state.set_status_persistent(
                "Are you sure you want to delete this page? Y/N".to_string(),
            );
        }
    }

    fn trash_confirmed(&mut self, path: PathBuf) {
        if path.exists() {
            self.spawn_trash(path);
            return;
        }
        let removed = self.state.remove_drafts_for_path(&path);
        self.state.remove_recent_path(&path);
        let _ = self.persist_settings();
        if removed > 0 {
            self.state.set_status(format!(
                "{} was already missing. Its draft was closed.",
                Self::display_path_tail(&path)
            ));
        } else {
            self.state.set_status(format!(
                "{} was already missing, so it was removed from recents.",
                Self::display_path_tail(&path)
            ));
        }
    }

    /// The evident close target when there is no file to trash: the current
    /// draft, when it has no path yet.
    fn unsaved_draft_target(&self) -> Option<usize> {
        if self.state.editor().file_path.is_none() {
            Some(self.state.active_draft)
        } else {
            None
        }
    }

    fn close_unsaved_draft(&mut self, index: usize, confirm: bool) {
        if self.state.drafts.get(index).is_none() {
            return;
        }
        // Every close confirms, empty page or not: Delete asks, Y fires, N
        // (or anything else) cancels (see the disarm in
        // handle_key_event/apply_action, which also wipes the message).
        if confirm && self.state.ui.pending_close_draft == Some(index) {
            self.state.ui.pending_close_draft = None;
            let words = self
                .state
                .drafts
                .get(index)
                .map(|draft| draft.editor.word_stats().word_count)
                .unwrap_or(0);
            self.state.remove_draft_at(index);
            if words > 0 {
                self.state
                    .set_status(format!("Discarded the unsaved page ({words} words)."));
            } else {
                self.state
                    .set_status("Discarded the empty page.".to_string());
            }
        } else {
            self.state.ui.pending_close_draft = Some(index);
            self.state.set_status_persistent(
                "Are you sure you want to delete this page? Y/N".to_string(),
            );
        }
    }

    fn trash_target_from_context(&self) -> Result<PathBuf, String> {
        self.state
            .editor()
            .file_path
            .clone()
            .ok_or_else(|| "The current draft has no file to move to Trash yet.".to_string())
    }

    fn toggle_panel(&mut self, kind: PanelKind) {
        // Asking for a panel while zoomed first leaves zoom: a dialog is
        // worth more than the extra row.
        self.unzoom_for_pane();
        let was_open = self.state.panels.is(kind);
        self.state.panels.toggle(kind);
        if was_open {
            // Closing any panel drops a picker riding on it.
            self.cancel_settings_picker();
            self.restore_focus_status();
            return;
        }

        if kind == PanelKind::Revisions {
            let count = self.active_file_revisions().len();
            self.state.panels.revision_selected = count.saturating_sub(1);
        }
        if kind == PanelKind::Pages {
            self.state.panels.pages_selected = self.state.active_visible_draft_index();
        }
        if kind == PanelKind::Workspace {
            self.state.panels.workspace_selected = 0;
        }
        if kind != PanelKind::Settings {
            self.cancel_settings_picker();
        }
        self.restore_focus_status();
    }

    fn apply_message(&mut self, message: AppMessage) {
        match message {
            AppMessage::SaveCompleted {
                draft_id,
                path,
                revision,
            } => {
                // Resolve by id: the page may have moved (or been closed)
                // since the save was spawned.
                if let Some(index) = self.state.draft_index_by_id(draft_id) {
                    self.state.drafts[index]
                        .editor
                        .mark_saved(revision, path.clone());
                }
                self.state.remember_recent_path(path.clone());
                let intent = std::mem::replace(&mut self.pending_after_save, CloseIntent::None);
                let should_quit = self.state.ui.quit_after_save;
                self.state.ui.quit_after_save = false;
                self.state
                    .set_status_quiet(format!("Saved {}.", Self::display_path_tail(&path)));
                let _ = self.persist_settings();
                match intent {
                    CloseIntent::CloseDraft => {
                        if self.state.close_active_draft() {
                            self.state.set_status_quiet(format!(
                                "Saved {} and closed that draft.",
                                path.display()
                            ));
                        }
                    }
                    CloseIntent::Exit => self.state.ui.should_quit = true,
                    CloseIntent::None => {
                        if should_quit {
                            // A quit-time save just landed: re-run the quit
                            // sequence so remaining dirty drafts prompt again
                            // instead of being abandoned.
                            self.request_quit();
                        }
                    }
                }
            }
            AppMessage::AutosaveCompleted {
                draft_id,
                path,
                revision,
            } => {
                // Quiet by design: no status, no intents. The file is
                // current; the UI need not mention it.
                if let Some(index) = self.state.draft_index_by_id(draft_id) {
                    self.state.drafts[index]
                        .editor
                        .mark_saved(revision, path.clone());
                }
                self.state.remember_recent_path(path);
                let _ = self.persist_settings();
            }
            AppMessage::TrashCompleted { path, trashed_to } => {
                let removed = self.state.remove_drafts_for_path(&path);
                self.state.remove_recent_path(&path);
                let _ = self.persist_settings();
                if removed > 0 {
                    self.state.set_status(format!(
                        "Moved {} to Trash at {}. Its draft was closed.",
                        Self::display_path_tail(&path),
                        Self::display_path_tail(&trashed_to)
                    ));
                } else {
                    self.state.set_status(format!(
                        "Moved {} to Trash at {}.",
                        Self::display_path_tail(&path),
                        Self::display_path_tail(&trashed_to)
                    ));
                }
            }
            AppMessage::OpenCompleted {
                path,
                buffer,
                metadata,
            } => {
                let mut buffer = *buffer;
                buffer.set_indent_width(self.state.ui.indent_width);
                self.state.ui.pending_open = false;
                self.state.remember_recent_path(path.clone());
                let _ = self.persist_settings();
                if let Some(existing) = self.state.find_draft_by_path(&path) {
                    self.switch_to_draft(existing);
                    self.state.set_status_quiet(format!(
                        "Switched to {}.",
                        Self::display_path_tail(&path)
                    ));
                } else {
                    if self.state.has_only_auto_scratch() {
                        self.state.replace_active_draft(buffer, Some(path.clone()));
                        self.state
                            .set_draft_metadata(self.state.active_draft, metadata);
                    } else {
                        let index = self.state.add_draft(buffer, Some(path.clone()));
                        self.state.set_draft_metadata(index, metadata);
                    }
                    self.state
                        .set_status_quiet(format!("Opened {}.", Self::display_path_tail(&path)));
                }
            }
            AppMessage::RecoveryCompleted => {}
            AppMessage::HistoryPersisted => {}
            AppMessage::WriteBlockCompleted {
                path,
                chars,
                block_only,
            } => {
                let scope = if block_only { "block" } else { "document" };
                self.state.remember_recent_path(path.clone());
                self.state.set_status_quiet(format!(
                    "Wrote {chars} character(s) from the {scope} to {}.",
                    Self::display_path_tail(&path)
                ));
            }
            AppMessage::ReadFileCompleted {
                draft_id,
                revision,
                path,
                text,
                chars,
            } => {
                let Some(draft_index) = self.state.draft_index_by_id(draft_id) else {
                    self.state.set_status(format!(
                        "Read {} but that draft was closed, so nothing was inserted.",
                        path.display()
                    ));
                    return;
                };
                if draft_index != self.state.active_draft {
                    self.state.set_status(format!(
                        "Read {} but the active draft changed, so nothing was inserted.",
                        path.display()
                    ));
                    return;
                }
                if self.state.editor().revision != revision {
                    self.state.set_status(format!(
                        "{} changed while reading, so nothing was inserted.",
                        path.display()
                    ));
                    return;
                }
                self.state.editor_mut().buffer.insert_text(&text);
                self.state.editor_mut().mark_dirty();
                self.sync_editor_view();
                self.state.set_status_quiet(format!(
                    "Inserted {chars} character(s) from {}.",
                    path.display()
                ));
            }
        }
    }

    fn spawn_save(&mut self) {
        let path = self.state.editor().default_save_path();
        self.spawn_save_to(path);
    }

    /// Last two path components for status lines: `Ordet-final/i.ord`
    /// instead of the full path. Never touches the stored path.
    fn display_path_tail(path: &Path) -> String {
        let parts: Vec<String> = path
            .components()
            .filter_map(|component| match component {
                std::path::Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        if parts.is_empty() {
            return path.display().to_string();
        }
        let start = parts.len().saturating_sub(2);
        parts[start..].join("/")
    }
    /// Writes one draft snapshot to disk, synchronously. Shared by the
    /// background save task and the quit-time flush, so both write byte-
    /// identical files.
    #[allow(clippy::too_many_arguments)]
    fn write_draft_file(
        path: &Path,
        plain_text: &str,
        content_html: &str,
        title: &str,
        doc_status: &str,
        doc_auto_title: bool,
        doc_label_color: &str,
    ) -> Result<(), String> {
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("ord"))
            .unwrap_or(false)
        {
            let document = OrdetDocument {
                title: title.to_string(),
                status: doc_status.to_string(),
                auto_title: doc_auto_title,
                label_color: doc_label_color.to_string(),
                content_html: content_html.to_string(),
                plain_text: plain_text.to_string(),
            };
            write_to_path(path, &document).map_err(|error| error.to_string())?;
        } else {
            write_bytes_atomically(path, plain_text.as_bytes())
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn spawn_save_to(&mut self, path: PathBuf) {
        // Normalize first so the save target, the recorded draft path, and
        // the history key all agree (macOS temp dirs symlink /var to
        // /private/var, which otherwise keys the same file twice).
        let path = normalize_document_path(path);
        let draft_id = self.state.active_draft_id();
        let plain_text = self.state.editor().buffer.plain_text();
        // Document HTML is a whole-document format pass. Snapshot the source
        // now and render it on the worker, so the event loop never stalls on
        // it (autosave used to pay a millisecond per thousand lines here).
        let snapshot = DraftSnapshot::capture(&self.state.editor().buffer);
        let is_ord = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("ord"))
            .unwrap_or(false);
        let revision = self.state.editor().revision;
        let title = self.state.editor().title();
        let doc_status = self.state.editor().doc_status.clone();
        let doc_auto_title = self.state.editor().doc_auto_title;
        let doc_label_color = self.state.editor().doc_label_color.clone();
        // Snapshot this save for the revision history, then persist the whole
        // history file off the main loop. Keyed replacement means a newer save
        // always wins over a write still in flight.
        self.state
            .history
            .record(&history_key_for(&path), &plain_text, unix_now());
        let history = self.state.history.clone();
        self.tasks
            .spawn(Some(TaskKey::from("history")), move |token| {
                if token.is_cancelled() {
                    return Err(TaskError::new("history write cancelled"));
                }
                save_history(&history).map_err(TaskError::new)?;
                if token.is_cancelled() {
                    return Err(TaskError::new("history write cancelled"));
                }
                Ok(AppMessage::HistoryPersisted)
            });
        self.state.editor_mut().pending_save = true;
        self.state.set_status_quiet(format!(
            "Saving {} in the background...",
            Self::display_path_tail(&path)
        ));

        self.tasks
            .spawn(Some(save_task_key(draft_id)), move |token| {
                if token.is_cancelled() {
                    return Err(TaskError::new("save cancelled"));
                }

                let content_html = if is_ord {
                    snapshot.to_ord_html()
                } else {
                    String::new()
                };
                Self::write_draft_file(
                    &path,
                    &plain_text,
                    &content_html,
                    &title,
                    &doc_status,
                    doc_auto_title,
                    &doc_label_color,
                )
                .map_err(TaskError::new)?;

                if token.is_cancelled() {
                    return Err(TaskError::new("save cancelled"));
                }

                Ok(AppMessage::SaveCompleted {
                    draft_id,
                    path,
                    revision,
                })
            });
    }

    /// Background save of any draft by index. Autosave uses this with
    /// `quiet` set, skipping the status chatter; manual saves keep it loud
    /// through `spawn_save_to`.
    fn spawn_save_draft(&mut self, index: usize, path: PathBuf, quiet: bool) {
        let path = normalize_document_path(path);
        let Some(draft) = self.state.drafts.get(index) else {
            return;
        };
        let draft_id = draft.id;
        let plain_text = draft.editor.buffer.plain_text();
        // As in `spawn_save_to`: render the document HTML on the worker, from
        // a snapshot, so autosave never blocks the keystroke path.
        let snapshot = DraftSnapshot::capture(&draft.editor.buffer);
        let is_ord = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("ord"))
            .unwrap_or(false);
        let revision = draft.editor.revision;
        let title = draft.editor.title();
        let doc_status = draft.editor.doc_status.clone();
        let doc_auto_title = draft.editor.doc_auto_title;
        let doc_label_color = draft.editor.doc_label_color.clone();
        self.state
            .history
            .record(&history_key_for(&path), &plain_text, unix_now());
        let history = self.state.history.clone();
        self.tasks
            .spawn(Some(TaskKey::from("history")), move |token| {
                if token.is_cancelled() {
                    return Err(TaskError::new("history write cancelled"));
                }
                save_history(&history).map_err(TaskError::new)?;
                if token.is_cancelled() {
                    return Err(TaskError::new("history write cancelled"));
                }
                Ok(AppMessage::HistoryPersisted)
            });
        if let Some(draft) = self.state.drafts.get_mut(index) {
            draft.editor.pending_save = true;
        }
        if !quiet {
            self.state.set_status_quiet(format!(
                "Saving {} in the background...",
                Self::display_path_tail(&path)
            ));
        }

        self.tasks
            .spawn(Some(save_task_key(draft_id)), move |token| {
                if token.is_cancelled() {
                    return Err(TaskError::new("save cancelled"));
                }

                let content_html = if is_ord {
                    snapshot.to_ord_html()
                } else {
                    String::new()
                };
                Self::write_draft_file(
                    &path,
                    &plain_text,
                    &content_html,
                    &title,
                    &doc_status,
                    doc_auto_title,
                    &doc_label_color,
                )
                .map_err(TaskError::new)?;

                if token.is_cancelled() {
                    return Err(TaskError::new("save cancelled"));
                }

                if quiet {
                    Ok(AppMessage::AutosaveCompleted {
                        draft_id,
                        path,
                        revision,
                    })
                } else {
                    Ok(AppMessage::SaveCompleted {
                        draft_id,
                        path,
                        revision,
                    })
                }
            });
    }

    fn spawn_trash(&mut self, path: PathBuf) {
        self.state.set_status_quiet(format!(
            "Moving {} to Trash...",
            Self::display_path_tail(&path)
        ));
        self.tasks
            .spawn(Some(TaskKey::from("trash")), move |token| {
                if token.is_cancelled() {
                    return Err(TaskError::new("trash cancelled"));
                }

                let trashed_to =
                    move_path_to_trash(&path).map_err(|error| TaskError::new(error.to_string()))?;

                if token.is_cancelled() {
                    return Err(TaskError::new("trash cancelled"));
                }

                Ok(AppMessage::TrashCompleted { path, trashed_to })
            });
    }

    fn open_or_focus_path(&mut self, path: PathBuf) {
        let path = normalize_document_path(path);
        if let Some(existing) = self.state.find_draft_by_path(&path) {
            self.switch_to_draft(existing);
            self.state.set_status_quiet(format!(
                "{} is already open. Focus moved to that draft.",
                path.display()
            ));
            return;
        }

        self.spawn_open(path);
    }

    fn spawn_open(&mut self, path: PathBuf) {
        self.state.ui.pending_open = true;
        self.state.set_status_quiet(format!(
            "Opening {} in the background...",
            Self::display_path_tail(&path)
        ));
        self.tasks.spawn(Some(TaskKey::from("open")), move |token| {
            if token.is_cancelled() {
                return Err(TaskError::new("open cancelled"));
            }

            let loaded =
                load_buffer_from_path(&path).map_err(|error| TaskError::new(error.to_string()))?;

            if token.is_cancelled() {
                return Err(TaskError::new("open cancelled"));
            }

            Ok(AppMessage::OpenCompleted {
                path,
                buffer: Box::new(loaded.buffer),
                metadata: loaded.metadata,
            })
        });
    }

    fn create_new_draft(&mut self) {
        let mut buffer = EditorBuffer::new();
        buffer.set_indent_width(self.state.ui.indent_width);
        if self.state.has_only_auto_scratch() {
            self.state.replace_active_draft(buffer, None);
        } else {
            self.state.add_draft(buffer, None);
        }
        if self.state.panels.is(PanelKind::Pages) {
            self.state.panels.pages_selected = self.state.active_visible_draft_index();
        }
        self.restore_focus_status();
    }

    fn switch_to_draft(&mut self, index: usize) {
        self.state.switch_to_draft(index);
        let title = self.state.editor().title();
        self.state.set_status_quiet(format!("Switched to {title}."));
    }

    /// F6: the next listed page, wrapping at the end.
    /// Every visible window keeps its own cursor on screen. A split hands
    /// each draft a different wrap width, so the pass runs per window.
    fn refresh_window_views(&mut self) {
        let rects = self.state.ui.layout.window_rects.clone();
        let drafts = self.state.visible_window_drafts();
        for (position, draft_index) in drafts.iter().enumerate() {
            let Some(rect) = rects.get(position).copied() else {
                break;
            };
            if rect.width == 0 || rect.height == 0 {
                continue;
            }
            let text_area = window_text_area(&self.state, rect);
            let (width, _) = wrap_and_gutter(
                text_area.width as usize,
                &self.state.drafts[*draft_index].editor.buffer,
                self.state.ui.show_line_numbers,
            );
            let height = text_area.height.max(1) as usize;
            self.state.drafts[*draft_index]
                .editor
                .ensure_cursor_visible(width, height);
        }
    }

    /// Recompute the window tiling after a split or close without waiting for
    /// the next terminal resize.
    fn refresh_layout(&mut self) {
        let header = self.state.ui.layout.header;
        let status = self.state.ui.layout.status;
        let area = Rect::new(
            header.x,
            header.y,
            header.width,
            status.bottom().saturating_sub(header.y),
        );
        if area.width == 0 || area.height == 0 {
            return;
        }
        self.state.update_layout(area);
        self.refresh_window_views();
    }

    /// Move the keyboard to the next (`delta` ±1) window, reporting where it
    /// landed. False when the plain editor is all there is.
    fn cycle_window(&mut self, delta: isize) -> bool {
        if !self.state.cycle_window(delta) {
            return false;
        }
        let position = self.state.active_window_position() + 1;
        let title = self.state.drafts[self.state.active_window_draft()].page_label();
        self.state.set_status_quiet(format!(
            "Window {position} of {}: {title}.",
            self.state.window_count()
        ));
        true
    }

    /// Alt-N / Window menu: open or switch windows. Open the next
    /// listed page in a new window until four are tiled, then cycle through
    /// the open windows.
    fn next_window(&mut self) {
        let visible = self.state.visible_drafts();
        if visible.len() < 2 {
            self.state.set_status(
                "Only one page is open. \u{2303}N makes another, then \u{2325}N opens a window."
                    .to_string(),
            );
            return;
        }

        let open = self.state.window_drafts();
        let active = self.state.active_visible_draft_index();
        let next = (1..=visible.len())
            .map(|offset| visible[(active + offset) % visible.len()])
            .find(|index| !open.contains(index));

        if let Some(index) = next {
            if self.state.open_window(index) {
                self.refresh_layout();
                let position = self.state.active_window_position() + 1;
                let title = self.state.drafts[index].page_label();
                self.state.set_status_quiet(format!(
                    "Window {position} of {}: {title}.",
                    self.state.window_count()
                ));
                return;
            }
        }

        if !self.cycle_window(1) {
            self.state.set_status("The desktop is full.".to_string());
        }
    }

    /// Alt-W / Window menu: drop the window that owns the keyboard.
    fn close_window(&mut self) {
        if !self.state.close_active_window() {
            self.state.set_status("One window is open.".to_string());
            return;
        }
        self.refresh_layout();
        self.state
            .set_status_quiet(if self.state.window_count() > 1 {
                format!(
                    "Window {} of {}.",
                    self.state.active_window_position() + 1,
                    self.state.window_count()
                )
            } else {
                "One window again.".to_string()
            });
    }

    /// Window menu: put the focused page in a specific tile slot. The window
    /// already there trades places, so the desktop keeps the same shape.
    fn move_window_to_slot(&mut self, slot: usize) {
        let count = self.state.window_count();
        if count < 2 {
            self.state
                .set_status("Open a second window first.".to_string());
            return;
        }
        if slot >= count {
            self.state
                .set_status(format!("Slot {} is not on the desktop.", slot + 1));
            return;
        }
        if !self.state.move_active_window_to(slot) {
            self.state
                .set_status(format!("Window is already in slot {}.", slot + 1));
            return;
        }
        self.refresh_layout();
        let title = self.state.drafts[self.state.active_window_draft()].page_label();
        self.state
            .set_status_quiet(format!("Moved {title} to slot {} of {count}.", slot + 1));
    }

    /// Focus the window under a click, so a press in a split pane edits the
    /// page that is actually under the pointer.
    fn focus_window_at(&mut self, point: Point) {
        if self.state.window_count() < 2 {
            return;
        }
        let rects = self.state.ui.layout.window_rects.clone();
        let drafts = self.state.visible_window_drafts();
        for (position, rect) in rects.iter().enumerate() {
            if !rect.contains(point) {
                continue;
            }
            let Some(draft_index) = drafts.get(position).copied() else {
                return;
            };
            if position == self.state.active_window_position() {
                return;
            }
            let id = self.state.drafts[draft_index].id;
            if self.state.focus_window_id(id) {
                self.state.set_status_quiet(format!(
                    "Window {} of {}.",
                    self.state.active_window_position() + 1,
                    self.state.window_count()
                ));
            }
            return;
        }
    }

    fn next_page(&mut self) {
        // F6 walks the desktop once a split is open: every window
        // already holds a page, so moving the keyboard moves the page.
        if self.state.window_count() > 1 {
            self.cycle_window(1);
            return;
        }
        let visible = self.state.visible_drafts();
        if visible.len() < 2 {
            self.state.set_status("Only one page is open.".to_string());
            return;
        }
        let next = (self.state.active_visible_draft_index() + 1) % visible.len();
        self.switch_to_draft(visible[next]);
    }

    /// Shift-F6: the previous listed page, wrapping at the start.
    fn previous_page(&mut self) {
        if self.state.window_count() > 1 {
            self.cycle_window(-1);
            return;
        }
        let visible = self.state.visible_drafts();
        if visible.len() < 2 {
            self.state.set_status("Only one page is open.".to_string());
            return;
        }
        let previous =
            (self.state.active_visible_draft_index() + visible.len() - 1) % visible.len();
        self.switch_to_draft(visible[previous]);
    }

    /// Opens the option picker for one settings row: every choice listed at
    /// once, preselected on the current value.
    fn open_settings_picker(&mut self, kind: SettingsPickerKind) {
        let (title, options, selected) = match kind {
            SettingsPickerKind::Theme => {
                let presets = ThemePreset::ALL;
                let current = self.state.theme_context();
                let selected = presets
                    .iter()
                    .position(|preset| {
                        *preset == self.state.ui.theme.base_preset(current).canonical()
                    })
                    .unwrap_or(0);
                (
                    "Theme".to_string(),
                    presets
                        .iter()
                        .map(|preset| preset.name().to_string())
                        .collect(),
                    selected,
                )
            }
            SettingsPickerKind::Language => {
                let languages = crate::i18n::UiLanguage::ALL;
                let selected = languages
                    .iter()
                    .position(|language| *language == self.state.ui.language)
                    .unwrap_or(0);
                (
                    "Language".to_string(),
                    languages
                        .iter()
                        .map(|language| language.display_name().to_string())
                        .collect(),
                    selected,
                )
            }
            SettingsPickerKind::IndentWidth => {
                let widths = [2, 4, 8];
                let selected = widths
                    .iter()
                    .position(|width| *width == self.state.ui.indent_width)
                    .unwrap_or(1);
                (
                    "Indent width".to_string(),
                    widths.iter().map(|width| width.to_string()).collect(),
                    selected,
                )
            }
        };

        let mut picker = SettingsPickerState {
            kind,
            title: title.clone(),
            options,
            selected,
            scroll: 0,
            active: selected,
            preview_from: if kind == SettingsPickerKind::Theme {
                Some(self.state.ui.theme.clone())
            } else {
                None
            },
        };
        let visible =
            crate::state::settings_picker_list_rect(self.full_area(), picker.options.len()).height
                as usize;
        picker.clamp_to(visible.max(1));
        self.state.panels.settings_picker = Some(picker);
        self.state.set_status(format!(
            "Select {title}. Up/Down to navigate, Enter to choose, Esc to cancel."
        ));
    }

    /// Shows the highlighted theme without saving it, so browsing the list
    /// previews live. Silent by design: no status per step, no persist.
    /// Enter persists; Esc restores whatever was in force.
    fn preview_settings_picker_theme(&mut self) {
        let Some(picker) = self.state.panels.settings_picker.as_ref() else {
            return;
        };
        if picker.kind != SettingsPickerKind::Theme {
            return;
        }
        // ALL holds canonical presets, so no sanitize step is needed.
        if let Some(preset) = ThemePreset::ALL.get(picker.selected) {
            self.state.ui.theme = ThemeSelection::Preset { preset: *preset };
        }
    }

    /// Closes the picker, restoring the pre-browse theme when a theme
    /// preview is showing. Used by every dismiss path so none can strand a
    /// preview nobody chose.
    fn cancel_settings_picker(&mut self) {
        let Some(picker) = self.state.panels.settings_picker.take() else {
            return;
        };
        if picker.kind == SettingsPickerKind::Theme {
            if let Some(original) = picker.preview_from {
                self.state.ui.theme = original;
            }
        }
        self.state
            .set_status_quiet("Setting selection closed.".to_string());
    }

    /// Applies the highlighted choice and closes the picker. Shared by the
    /// Enter key and mouse clicks so both confirm the same row.
    fn confirm_settings_picker(&mut self) {
        let Some(picker) = self.state.panels.settings_picker.take() else {
            return;
        };
        let choice = picker.options.get(picker.selected).cloned();
        let Some(choice) = choice else {
            return;
        };
        match picker.kind {
            SettingsPickerKind::Theme => {
                if let Some(preset) = ThemePreset::ALL.get(picker.selected) {
                    self.apply_theme(ThemeSelection::Preset { preset: *preset }, true);
                }
            }
            SettingsPickerKind::Language => {
                if let Some(language) = crate::i18n::UiLanguage::ALL.get(picker.selected).copied() {
                    self.apply_language(language);
                }
            }
            SettingsPickerKind::IndentWidth => {
                if let Ok(width) = choice.parse::<usize>() {
                    self.apply_indent_width(width);
                }
            }
        }
    }

    fn handle_task_result(&mut self, task: TaskResult<AppMessage>) {
        match task.result {
            Ok(message) => self.dispatch(UiEvent::Message(message)),
            Err(error)
                if error.message == "save cancelled"
                    || error.message == "open cancelled"
                    || error.message == "trash cancelled"
                    || error.message == "history write cancelled" => {}
            Err(error) => {
                if let Some(draft_id) = draft_id_from_task_key(task.key.as_ref()) {
                    if let Some(index) = self.state.draft_index_by_id(draft_id) {
                        self.state.drafts[index].editor.pending_save = false;
                    }
                }
                self.state.ui.pending_open = false;
                self.state.ui.quit_after_save = false;
                self.pending_after_save = CloseIntent::None;
                self.state
                    .set_status(format!("Background task failed: {error}"));
            }
        }
    }

    /// The page a reorder chord acts on: the highlighted Pages row when the
    /// panel is open, otherwise the page being edited.
    fn selected_page_or_active(&self) -> usize {
        if self.state.panels.is(PanelKind::Pages) {
            if let Some(row) = self.state.page_rows().get(self.state.panels.pages_selected) {
                return row.index;
            }
        }
        self.state.active_draft
    }

    /// Wheel/hover-free row stepping for the compact list panels. Keeps the
    /// highlight inside the visible list as it scrolls.
    fn nudge_panel_selection(&mut self, direction: isize, amount: usize) {
        let Some(kind) = self.state.panels.panel else {
            return;
        };
        let count = match kind {
            PanelKind::Pages => self.state.page_rows().len(),
            PanelKind::Workspace => self.state.workspace_rows().len(),
            _ => return,
        };
        if count == 0 {
            return;
        }
        let selected = match kind {
            PanelKind::Pages => &mut self.state.panels.pages_selected,
            PanelKind::Workspace => &mut self.state.panels.workspace_selected,
            _ => return,
        };
        let step = amount as isize * direction.signum();
        let next = (*selected as isize + step).clamp(0, count.saturating_sub(1) as isize);
        *selected = next as usize;
        crate::components::follow_panel_scroll(&mut self.state);
    }

    fn restore_focus_status(&mut self) {
        if self.state.ui.prompt.is_some() {
            return;
        }

        if self.state.show_welcome_screen() {
            self.state.set_status(
                "Welcome. Enter opens the selected option, Up/Down changes selection, ⌃N makes a new draft, ⌃B opens a file.",
            );
            return;
        }

        let message = match self.state.panels.panel {
            None => "",
            Some(PanelKind::Notes) => "Notes. ↑↓ edit, Esc closes.",
            Some(PanelKind::Help) => "",
            Some(PanelKind::Stats) => "Word stats. ⌃G sets the target, Esc closes.",
            Some(PanelKind::Settings) => "Settings. ↑↓ selects, Enter changes, Esc closes.",
            Some(PanelKind::Revisions) => {
                "Revisions. ↑↓ picks a snapshot, Enter restores it, Esc closes."
            }
            Some(PanelKind::Pages) => "Pages. Enter switches, ⌃N new, ⌦ close, Esc closes.",
            Some(PanelKind::Workspace) => {
                "Workspace. Enter opens, ⌦ untracks, ⌃⇧W adds a folder, Esc closes."
            }
        };
        self.state.set_status(message);
        // Panel hints are the default text: they never expire (transient
        // messages fall back TO this).
        self.state.ui.status_expires_at = None;
    }
}

fn load_buffer_from_path(path: &PathBuf) -> Result<LoadedDraft, Box<dyn std::error::Error>> {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .unwrap_or_default();

    if extension == "ord" {
        match read_from_path(path) {
            Ok(document) => {
                let mut buffer =
                    EditorBuffer::from_rich_text(&document.plain_text, &document.content_html);
                buffer.move_cursor_to_end();
                Ok(LoadedDraft {
                    buffer,
                    metadata: DocumentMetadata {
                        status: document.status,
                        auto_title: document.auto_title,
                        label_color: document.label_color,
                    },
                })
            }
            Err(FormatError::InvalidDocument) => {
                let raw = fs::read_to_string(path)?;
                let mut buffer = EditorBuffer::from_text(&raw);
                buffer.move_cursor_to_end();
                Ok(LoadedDraft {
                    buffer,
                    metadata: DocumentMetadata::default(),
                })
            }
            Err(error) => Err(Box::new(error)),
        }
    } else {
        let mut buffer = EditorBuffer::from_text(&fs::read_to_string(path)?);
        buffer.move_cursor_to_end();
        Ok(LoadedDraft {
            buffer,
            metadata: DocumentMetadata::default(),
        })
    }
}

#[cfg(test)]
fn load_plain_text_from_path(path: &PathBuf) -> Result<String, Box<dyn std::error::Error>> {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .unwrap_or_default();
    let text = if extension == "ord" {
        match read_from_path(path) {
            Ok(document) => document.plain_text,
            Err(FormatError::InvalidDocument) => {
                let raw = fs::read_to_string(path)?;
                if raw.trim().is_empty() {
                    String::new()
                } else {
                    raw
                }
            }
            Err(error) => return Err(Box::new(error)),
        }
    } else {
        fs::read_to_string(path)?
    };
    Ok(text)
}

fn sanitize_indent_width(width: usize) -> usize {
    match width {
        2 | 4 | 8 => width,
        _ => 4,
    }
}

fn expand_path(path: PathBuf) -> PathBuf {
    if let Ok(stripped) = path.strip_prefix("~") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    }
    path
}

fn parse_save_path_input(input: &str, title: &str) -> Result<PathBuf, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Enter a file path like ./draft.ord or ./draft.txt.".to_string());
    }

    let mut path = expand_path(PathBuf::from(trimmed));
    if path.exists() && path.is_dir() {
        path = path.join(ordet_format::default_save_file_name(Some(title)));
    } else if path.extension().is_none() {
        path.set_extension("ord");
    }

    Ok(path)
}

/// " [regex + whole word]" after a search status, or nothing for the default
/// plain case-insensitive search.
fn search_flag_suffix(options: SearchOptions) -> String {
    if options == SearchOptions::default() {
        String::new()
    } else {
        format!(" [{}]", options.describe())
    }
}

fn parse_workspace_path_input(input: &str) -> Result<PathBuf, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Enter a folder to track.".to_string());
    }
    let path = expand_path(PathBuf::from(trimmed));
    if !path.is_dir() {
        return Err("That folder does not exist.".to_string());
    }
    Ok(path)
}

fn parse_open_path_input(input: &str) -> Result<PathBuf, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Enter a file path to open.".to_string());
    }

    let path = expand_path(PathBuf::from(trimmed));
    if path.is_dir() {
        return Err("That path is a folder. Enter a document file instead.".to_string());
    }
    if !path.exists() {
        return Err(
            "That file does not exist yet. Use New Draft if you want a blank document.".to_string(),
        );
    }

    Ok(path)
}

fn parse_pasted_single_path(input: &str) -> Option<PathBuf> {
    let candidates = parse_pasted_path_candidates(input);
    resolve_existing_pasted_path_candidate(&candidates)
}

fn parse_pasted_path_candidates(input: &str) -> Vec<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut candidates = Vec::new();
    candidates.push(trimmed.to_string());

    for line in trimmed.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        candidates.push(line.to_string());
        if let Some(index) = line.find("file://") {
            candidates.push(line[index..].to_string());
        }
        if line.starts_with('/') || line.starts_with('~') {
            candidates.push(line.to_string());
        }

        for token in line.split_whitespace() {
            if token.starts_with("file://") || token.starts_with('/') || token.starts_with('~') {
                candidates.push(token.to_string());
            }
        }
    }

    candidates
}

fn resolve_existing_pasted_path_candidate(candidates: &[String]) -> Option<PathBuf> {
    for candidate in candidates {
        let mut candidate = trim_pasted_path_wrappers(candidate.trim()).to_string();
        if candidate.is_empty() {
            continue;
        }

        if let Some(decoded_file_uri) = decode_file_uri_path(candidate.as_str()) {
            candidate = decoded_file_uri;
        }

        let candidate = unescape_terminal_path(&candidate);
        if candidate.is_empty() {
            continue;
        }

        let path = normalize_document_path(expand_path(PathBuf::from(candidate)));
        if path.exists() {
            return Some(path);
        }
    }

    None
}

fn trim_pasted_path_wrappers(value: &str) -> &str {
    let mut value = value.trim();
    loop {
        let wrapped = value.len() >= 2
            && matches!(
                (value.as_bytes()[0], value.as_bytes()[value.len() - 1]),
                (b'<', b'>')
                    | (b'(', b')')
                    | (b'[', b']')
                    | (b'{', b'}')
                    | (b'"', b'"')
                    | (b'\'', b'\'')
            );
        let next = wrapped.then(|| &value[1..value.len() - 1]);

        match next {
            Some(next) => value = next.trim(),
            None => break,
        }
    }
    value
        .trim_matches(|ch: char| {
            matches!(
                ch,
                '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\''
            )
        })
        .trim_end_matches(['.', ',', ';'])
}

fn decode_file_uri_path(candidate: &str) -> Option<String> {
    let file_uri = candidate.strip_prefix("file://")?;
    let decoded = urlencoding::decode(file_uri.trim()).ok()?.into_owned();
    #[cfg(windows)]
    let decoded = decoded.replace('\\', "/");

    #[cfg(windows)]
    if let Some(rest) = decoded.strip_prefix("localhost") {
        // Be forgiving of paths built with `Path::display` rather than a
        // proper URI, e.g. `file://localhostC:\\Users\\...`.
        if rest.len() >= 2 && rest.as_bytes()[1] == b':' {
            return Some(rest.to_string());
        }
    }

    let (authority, path) = match decoded.find('/') {
        Some(index) => (&decoded[..index], &decoded[index..]),
        None => (decoded.as_str(), ""),
    };
    #[cfg(windows)]
    if authority.len() == 2 && authority.as_bytes()[1] == b':' {
        // `file://C:/dir` is a drive-qualified local path, not a host.
        return Some(format!("{authority}{path}"));
    }
    let local = authority.is_empty() || authority.eq_ignore_ascii_case("localhost");
    if !local {
        // A file URI with a real host names a UNC share.
        #[cfg(windows)]
        {
            return Some(format!("//{authority}{path}").replace('/', "\\"));
        }
        #[cfg(not(windows))]
        {
            return Some(format!("//{authority}{path}"));
        }
    }

    #[cfg(windows)]
    if path.len() >= 3 && path.as_bytes()[0] == b'/' && path.as_bytes()[2] == b':' {
        return Some(path[1..].to_string());
    }

    Some(if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    })
}

#[cfg(not(windows))]
fn unescape_terminal_path(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut escaped = false;
    for ch in input.chars() {
        if escaped {
            output.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else {
            output.push(ch);
        }
    }
    if escaped {
        output.push('\\');
    }
    output
}

#[cfg(windows)]
fn unescape_terminal_path(input: &str) -> String {
    // Backslashes are path separators on Windows, so only undo the terminal
    // convention of escaping a literal space. Other separators stay intact.
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' && chars.peek() == Some(&' ') {
            output.push(' ');
            chars.next();
        } else {
            output.push(ch);
        }
    }
    output
}

fn parse_browser_target_input(input: &str, source: &Path) -> Result<PathBuf, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Enter a name or destination path.".to_string());
    }

    let raw = expand_path(PathBuf::from(trimmed));
    let target = if raw.is_absolute() {
        raw
    } else {
        source.parent().unwrap_or_else(|| Path::new(".")).join(raw)
    };

    Ok(target)
}

fn parse_alignment_input(input: &str) -> Result<ParagraphAlignment, String> {
    ParagraphAlignment::parse(input)
        .ok_or_else(|| "Enter left, center, right, or justify.".to_string())
}

fn retry_prompt(mut prompt: PromptState, value: String, hint: impl Into<String>) -> PromptState {
    prompt.value = value;
    prompt.hint = hint.into();
    prompt
}

fn parse_line_number_input(input: &str) -> Result<usize, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Enter a line number.".to_string());
    }
    trimmed
        .parse::<usize>()
        .map_err(|_| "Enter a line number like 42.".to_string())
}

fn parse_block_target_input(input: &str) -> Result<PathBuf, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Enter a file path.".to_string());
    }
    Ok(expand_path(PathBuf::from(trimmed)))
}

fn suggest_copy_destination(source: &Path) -> PathBuf {
    let parent = source.parent().unwrap_or_else(|| Path::new("."));
    let stem = source
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| {
            source
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| "Copy".to_string())
        });
    let extension = source
        .extension()
        .map(|ext| ext.to_string_lossy().to_string());

    let base_name = match extension {
        Some(ext) if !ext.is_empty() => format!("{stem} copy.{ext}"),
        _ => format!("{stem} copy"),
    };
    let mut candidate = parent.join(base_name);
    let mut suffix = 2usize;
    while candidate.exists() {
        let name = match source
            .extension()
            .map(|ext| ext.to_string_lossy().to_string())
        {
            Some(ext) if !ext.is_empty() => format!("{stem} copy {suffix}.{ext}"),
            _ => format!("{stem} copy {suffix}"),
        };
        candidate = parent.join(name);
        suffix += 1;
    }
    candidate
}

fn save_task_key(draft_id: DraftId) -> TaskKey {
    TaskKey::from(format!("save:{}", draft_id.0))
}

/// Retypes macOS Option-letter output as the Alt chord the keymap binds.
/// Without "Use Option as Meta", the terminal sends the glyph (∂, ç, …)
/// rather than Alt+letter, so shortcuts would insert text instead of
/// firing. Covers every Alt chord the keymap binds (US layout); anything
/// else passes through untouched, and real Alt (Meta mode) never reaches
/// this table at all. Option-N is the one exception: it is a dead key the
/// terminal composes itself, so `⌥N` (next window) needs Meta mode.
///
/// Tradeoff, stated plainly: with Option-as-unicode these glyphs become
/// shortcuts instead of typable text. Terminals with Meta mode are
/// unaffected either way, and dead-key accents that terminals compose
/// themselves never arrive here as lone characters.
fn normalize_macos_option_key(key: KeyEvent) -> KeyEvent {
    if key.modifiers != KeyModifiers::NONE {
        return key;
    }
    let KeyCode::Char(ch) = key.code else {
        return key;
    };
    let letter = match ch {
        'å' => 'a',
        '∫' => 'b',
        'ç' => 'c',
        '∂' => 'd',
        '´' => 'e',
        'ˆ' => 'i',
        'µ' => 'm',
        '®' => 'r',
        'ß' => 's',
        '†' => 't',
        '¨' => 'u',
        '√' => 'v',
        '∑' => 'w',
        '≈' => 'x',
        _ => return key,
    };
    KeyEvent {
        code: KeyCode::Char(letter),
        modifiers: KeyModifiers::ALT,
        kind: key.kind,
        state: key.state,
    }
}

/// Ctrl-V / Alt-V (paste) in every terminal encoding.
///
/// Both letter cases count: terminals disagree about whether Ctrl-Shift-V
/// arrives as `v`, `V`, or an explicit Shift flag, and all of them mean paste.
fn is_paste_key(key: KeyEvent) -> bool {
    if !matches!(key.code, KeyCode::Char('v') | KeyCode::Char('V')) {
        return false;
    }
    key.modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

fn copy_path(source: &Path, target: &Path, source_is_dir: bool) -> io::Result<()> {
    if source_is_dir {
        copy_dir_recursive(source, target)
    } else {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, target)?;
        Ok(())
    }
}

fn copy_dir_recursive(source: &Path, target: &Path) -> io::Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        let destination = target.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &destination)?;
        } else {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&path, &destination)?;
        }
    }
    Ok(())
}

fn draft_id_from_task_key(key: Option<&TaskKey>) -> Option<DraftId> {
    key.and_then(|key| key.0.strip_prefix("save:"))
        .and_then(|suffix| suffix.parse::<u64>().ok())
        .map(DraftId)
}

fn move_path_to_trash(path: &Path) -> io::Result<PathBuf> {
    let trash_dir = trash_directory()?;
    fs::create_dir_all(&trash_dir)?;
    let destination = unique_trash_destination(&trash_dir, path);
    if let Err(rename_error) = fs::rename(path, &destination) {
        // The file may live on another volume (common on Windows). Copy it
        // into the trash, then remove the source only after the copy lands.
        if fs::copy(path, &destination).is_err() {
            return Err(rename_error);
        }
        if let Err(remove_error) = fs::remove_file(path) {
            let _ = fs::remove_file(&destination);
            return Err(remove_error);
        }
    }
    Ok(destination)
}

fn trash_directory() -> io::Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "Home directory is not available")
        })?;
        Ok(home.join(".Trash"))
    }

    #[cfg(target_os = "windows")]
    {
        let data = dirs::data_local_dir().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Local data directory is not available",
            )
        })?;
        // Keep the beta trash location for the same reason as settings.
        Ok(data.join("ordet-tui").join("Trash").join("files"))
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let data = dirs::data_local_dir().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Local data directory is not available",
            )
        })?;
        Ok(data.join("Trash").join("files"))
    }
}

fn unique_trash_destination(trash_dir: &Path, original_path: &Path) -> PathBuf {
    let file_name = original_path
        .file_name()
        .filter(|name| !name.is_empty())
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "Untitled".into());
    let base = PathBuf::from(&file_name);
    let stem = base
        .file_stem()
        .filter(|stem| !stem.is_empty())
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| "Untitled".to_string());
    let extension = base
        .extension()
        .map(|ext| ext.to_string_lossy().to_string());

    let mut candidate = trash_dir.join(&file_name);
    let mut suffix = 2usize;
    while candidate.exists() {
        let name = match &extension {
            Some(ext) if !ext.is_empty() => format!("{stem} {suffix}.{ext}"),
            _ => format!("{stem} {suffix}"),
        };
        candidate = trash_dir.join(name);
        suffix += 1;
    }

    candidate
}

fn merge_recent_paths(
    mut recent_files: Vec<PathBuf>,
    paths: impl IntoIterator<Item = PathBuf>,
) -> Vec<PathBuf> {
    for path in paths {
        let path = normalize_document_path(path);
        recent_files.retain(|candidate| !paths_match(candidate, &path));
        recent_files.insert(0, path);
    }
    recent_files.truncate(crate::state::MAX_RECENT_FILES);
    recent_files
}

/// A lone draft with no path and no text is the placeholder scratch buffer, so
/// recovered work can take its place instead of stacking up behind it.
fn is_empty_scratch(drafts: &[(EditorBuffer, Option<PathBuf>)]) -> bool {
    drafts.len() == 1 && drafts[0].1.is_none() && drafts[0].0.plain_text().is_empty()
}

fn current_local_hour() -> u8 {
    time::OffsetDateTime::now_local()
        .map(|value| value.hour())
        .unwrap_or(12)
}

/// Local date and time for the timestamp insert.
pub(crate) fn local_timestamp() -> String {
    let Ok(now) = time::OffsetDateTime::now_local() else {
        return String::new();
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute()
    )
}

fn detect_system_dark_mode() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        let output = ProcessCommand::new("defaults")
            .args(["read", "-g", "AppleInterfaceStyle"])
            .output()
            .ok()?;
        if !output.status.success() {
            return Some(false);
        }
        let value = String::from_utf8_lossy(&output.stdout);
        return Some(value.trim().eq_ignore_ascii_case("dark"));
    }

    #[cfg(target_os = "windows")]
    {
        use winreg::enums::HKEY_CURRENT_USER;

        let personalize = winreg::RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize")
            .ok()?;
        let apps_use_light_theme: u32 = personalize.get_value("AppsUseLightTheme").ok()?;
        return Some(apps_use_light_theme == 0);
    }

    #[allow(unreachable_code)]
    None
}

fn parse_word_target(input: &str) -> Result<Option<usize>, String> {
    let trimmed = input.trim();
    if trimmed.is_empty()
        || matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "0" | "off" | "none" | "clear"
        )
    {
        return Ok(None);
    }

    let digits = trimmed
        .chars()
        .filter(|ch| ch.is_ascii_digit())
        .collect::<String>();
    if digits.is_empty() {
        return Err("Enter a number like 1000, or 0/off/none to clear.".to_string());
    }

    let value = digits
        .parse::<usize>()
        .map_err(|_| "That target is too large.".to_string())?;
    if value == 0 {
        return Ok(None);
    }

    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use ordet_format::{read_from_path, write_to_path, OrdetDocument};

    use super::{
        load_buffer_from_path, load_plain_text_from_path, parse_alignment_input,
        parse_browser_target_input, parse_cli_args, parse_pasted_single_path, parse_word_target,
        suggest_copy_destination, unique_trash_destination, CliAction, OrdetApp,
    };
    use crate::commands::Command;
    use crate::editor_buffer::{EditorBuffer, ParagraphAlignment};
    use crate::state::PanelKind;
    use crate::state::{AppState, HelpTab};
    use crate::theme::ThemeSelection;
    use ordet_tui_framework::{Rect, UiEvent};

    #[test]
    fn esc_leaves_welcome_help_on_the_first_press() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        assert!(app.state.show_welcome_screen());
        app.state
            .set_welcome_selection(crate::state::WelcomeOption::Help);
        app.state.focus_welcome_help();
        assert!(app.state.welcome_help_active());

        // The app-level Esc handling must not swallow this first: on the
        // welcome screen Esc belongs to the help overlay.
        let esc = KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        app.handle_key_event(esc);
        assert!(!app.state.welcome_help_active());
        assert_eq!(
            app.state.ui.welcome_selected,
            crate::state::WelcomeOption::Help
        );
    }

    #[test]
    fn word_target_prompt_accepts_numbers_and_clear_commands() {
        assert_eq!(parse_word_target("1,500"), Ok(Some(1500)));
        assert_eq!(parse_word_target("off"), Ok(None));
        assert!(parse_word_target("nope").is_err());
    }

    #[test]
    fn empty_ord_files_open_as_blank_documents() {
        let path = unique_temp_path("ordet-empty", "ord");
        fs::write(&path, "").expect("empty ord file");

        let text = load_plain_text_from_path(&path).expect("open empty ord");

        assert!(text.is_empty());
    }

    #[test]
    fn alignment_prompt_accepts_named_values() {
        assert_eq!(parse_alignment_input("left"), Ok(ParagraphAlignment::Left));
        assert_eq!(
            parse_alignment_input("center"),
            Ok(ParagraphAlignment::Center)
        );
        assert_eq!(
            parse_alignment_input("right"),
            Ok(ParagraphAlignment::Right)
        );
        assert_eq!(
            parse_alignment_input("justify"),
            Ok(ParagraphAlignment::Justify)
        );
        assert!(parse_alignment_input("diagonal").is_err());
    }

    #[test]
    fn missing_startup_path_opens_unsaved_draft_instead_of_binding_file() {
        let path = unique_temp_path("ordet-missing-startup", "ord");
        if path.exists() {
            fs::remove_file(&path).expect("remove stale temp file");
        }

        let app = with_test_settings(|| {
            OrdetApp::from_args(vec![path.to_string_lossy().into_owned()])
                .expect("app from missing arg path")
        });

        assert!(app.state.editor().file_path.is_none());
        assert!(
            app.state.ui.status.contains("did not exist"),
            "status was {:?}",
            app.state.ui.status
        );
    }

    #[test]
    fn startup_without_args_keeps_the_auto_scratch_invisible() {
        let app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app from empty args"));

        // The auto-scratch draft is a private landing pad: the page list
        // shows none until the user types or opens something.
        assert_eq!(app.state.visible_draft_count(), 0);
        assert!(app.state.page_rows().is_empty());
    }

    #[test]
    fn idle_loop_waits_for_the_next_housekeeping_deadline() {
        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app from empty args"));
        let now = std::time::Instant::now();
        app.next_tick = now + std::time::Duration::from_millis(100);
        app.redraw_requested = false;
        app.next_recovery_write = now + std::time::Duration::from_secs(3);
        app.next_autosave_at = now + std::time::Duration::from_secs(2);

        let (deadlines, count) = app.next_deadlines();
        assert_eq!(count, 3);
        let wait = deadlines
            .iter()
            .take(count)
            .map(|at| at.saturating_duration_since(now))
            .min()
            .expect("deadlines");
        assert!(wait >= std::time::Duration::from_millis(90));
    }

    #[test]
    fn startup_positions_cursor_at_end_for_existing_files() {
        let path = unique_temp_path("ordet-startup-cursor", "txt");
        fs::write(&path, "hello world").expect("startup text file");

        let app = with_test_settings(|| {
            OrdetApp::from_args(vec![path.to_string_lossy().into_owned()])
                .expect("app from startup path")
        });

        assert_eq!(
            app.state.editor().buffer.cursor(),
            app.state.editor().buffer.char_count()
        );
    }

    #[test]
    fn saving_over_a_directory_reports_an_error_instead_of_panicking() {
        let dir = unique_temp_path("ordet-save-dir", "");
        fs::create_dir_all(&dir).expect("directory target");

        let result = OrdetApp::write_draft_file(&dir, "text", "", "title", "Draft", true, "");

        assert!(result.is_err(), "a directory is not a writable file target");
    }

    #[test]
    fn binary_file_is_rejected_without_panicking() {
        let path = unique_temp_path("ordet-binary", "bin");
        fs::write(&path, [0xff, 0xfe, 0x00, 0x80]).expect("binary fixture");

        let error = load_buffer_from_path(&path).expect_err("binary file must not open as text");
        assert!(
            !error.to_string().is_empty(),
            "the load error should be readable"
        );
    }

    #[test]
    fn opening_an_already_open_file_focuses_instead_of_duplicating() {
        let path = unique_temp_path("ordet-open-twice", "txt");
        fs::write(&path, "one draft only").expect("fixture");
        let mut app = with_test_settings(|| {
            OrdetApp::from_args(vec![path.to_string_lossy().into_owned()]).expect("app from path")
        });
        let before = app.state.drafts.len();

        app.open_or_focus_path(path.clone());
        app.open_or_focus_path(path.clone());

        assert_eq!(app.state.drafts.len(), before);
        assert_eq!(app.state.window_count(), 1);
        assert_eq!(
            app.state.editor().title(),
            path.file_name().unwrap().to_string_lossy()
        );
    }

    #[test]
    fn f6_cycles_four_windows_forward_and_back() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        for title in ["one", "two", "three", "four"] {
            app.state.add_draft(EditorBuffer::from_text(title), None);
        }
        app.state.update_layout(Rect::new(0, 0, 120, 30));
        for _ in 0..3 {
            app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT));
        }
        assert_eq!(app.state.window_count(), 4);

        for expected in [0, 1, 2, 3, 0] {
            app.handle_key_event(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE));
            assert_eq!(app.state.active_window_position(), expected);
        }
        for expected in [3, 2, 1, 0, 3] {
            app.handle_key_event(KeyEvent::new(KeyCode::F(6), KeyModifiers::SHIFT));
            assert_eq!(app.state.active_window_position(), expected);
        }
    }

    #[test]
    fn invalid_regex_reopens_find_without_dirtying_the_draft() {
        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state
            .add_draft(EditorBuffer::from_text("hello world"), None);
        app.state.ui.search_options.regex = true;
        let revision = app.state.editor().revision;

        app.run_find("[".to_string());

        let prompt = app.state.ui.prompt.as_ref().expect("invalid regex retries");
        assert!(
            prompt.hint.to_ascii_lowercase().contains("regex"),
            "unexpected hint: {}",
            prompt.hint
        );
        assert_eq!(app.state.editor().revision, revision);
    }

    #[test]
    fn unique_trash_destination_avoids_name_collisions() {
        let trash = unique_temp_path("ordet-trash-dir", "tmp");
        fs::create_dir_all(&trash).expect("trash dir");
        fs::write(trash.join("draft.ord"), "older").expect("existing trash file");

        let destination = unique_trash_destination(&trash, std::path::Path::new("draft.ord"));

        assert_eq!(
            destination.file_name().and_then(|name| name.to_str()),
            Some("draft 2.ord")
        );
    }

    #[test]
    fn find_save_as_and_status_shortcuts_open_their_prompts() {
        use crate::prompt::PromptKind;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::from_text("hello"), None);

        app.handle_key_event(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert!(matches!(
            app.state.ui.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(PromptKind::FindText)
        ));
        app.state.ui.prompt = None;

        app.handle_key_event(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert!(matches!(
            app.state.ui.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(PromptKind::SavePath)
        ));
        app.state.ui.prompt = None;

        app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::ALT));
        assert!(matches!(
            app.state.ui.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(PromptKind::DocumentStatus)
        ));
    }

    #[test]
    fn ctrl_r_renames_in_the_browser_instead_of_trashing_the_page() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let dir = unique_temp_path("ordet-browser-rename", "");
        fs::create_dir_all(&dir).expect("browser dir");
        let file = dir.join("draft.txt");
        fs::write(&file, "x").expect("browser file");
        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state
            .add_draft(EditorBuffer::from_text("x"), Some(file.clone()));
        app.open_file_prompt();
        assert!(app.state.ui.file_browser.is_some());

        app.handle_key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));

        assert!(app.state.ui.prompt.is_some(), "Ctrl-R opens Rename");
        assert!(app.state.ui.file_browser.is_some(), "browser stays open");
        assert_eq!(app.state.ui.pending_trash, None);
    }

    #[test]
    fn browser_target_input_resolves_relative_to_the_source_parent() {
        let source = PathBuf::from("/tmp/ordet-browser/chapter1.ord");

        let target = parse_browser_target_input("chapter2.ord", &source).expect("target");

        assert_eq!(target, PathBuf::from("/tmp/ordet-browser/chapter2.ord"));
    }

    #[test]
    fn copy_suggestion_adds_a_copy_suffix() {
        let suggestion = suggest_copy_destination(Path::new("/tmp/ordet-browser/chapter1.ord"));
        assert_eq!(
            suggestion.file_name().and_then(|name| name.to_str()),
            Some("chapter1 copy.ord")
        );
    }

    #[test]
    fn pasted_paths_with_escaped_spaces_are_unescaped() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("ordet dropped {stamp}.ord"));
        fs::write(&path, "note").expect("text fixture");
        let escaped = path.to_string_lossy().replace(' ', "\\ ");

        let parsed = parse_pasted_single_path(&escaped).expect("parsed path");

        assert_eq!(parsed, super::normalize_document_path(path));
    }

    #[test]
    fn pasted_file_uri_with_localhost_is_parsed() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("ordet-dropped-{stamp}.ord"));
        fs::write(&path, "note").expect("text fixture");
        let uri = format!("file://localhost{}", path.display());

        let parsed = parse_pasted_single_path(&uri).expect("parsed path");

        assert_eq!(parsed, super::normalize_document_path(path));
    }

    #[test]
    fn pasted_wrapped_file_uri_inside_text_is_parsed() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("ordet-dropped-{stamp}.ord"));
        fs::write(&path, "note").expect("text fixture");
        let uri = format!("Dropped: <file://{}>", path.display());

        let parsed = parse_pasted_single_path(&uri).expect("parsed path");

        assert_eq!(parsed, super::normalize_document_path(path));
    }

    fn unique_temp_path(prefix: &str, extension: &str) -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("{prefix}-test"));
        fs::create_dir_all(&root).expect("test temp root");
        if extension.is_empty() {
            root.join(format!("artifact-{stamp}"))
        } else {
            root.join(format!("artifact-{stamp}.{extension}"))
        }
    }

    #[test]
    fn test_settings_override_outlives_the_construction_closure() {
        // The app keeps writing long after construction (settings saves, quit
        // backups, recovery snapshots). A regression here would send those
        // writes into the real user config directory (Application Support on
        // macOS, %APPDATA% on Windows), which is exactly how the Backups dir
        // filled up with test artifacts.
        with_test_settings(|| {});
        let path = std::env::var_os("ORDET_TUI_SETTINGS_PATH")
            .expect("the settings override must outlive the closure");
        let path = std::path::PathBuf::from(path);
        assert!(
            path.starts_with(std::env::temp_dir()),
            "post-closure writes must stay under the temp dir, got {}",
            path.display()
        );
    }

    fn with_test_settings<T>(run: impl FnOnce() -> T) -> T {
        // Settings lock first: the same order every test uses, so
        // opposite-order deadlocks are impossible.
        let guard = crate::settings::settings_env_lock()
            .lock()
            .expect("test settings lock");
        let settings_path = unique_temp_path("ordet-settings", "json");
        if settings_path.exists() {
            fs::remove_file(&settings_path).expect("clear stale test settings");
        }
        unsafe {
            std::env::set_var("ORDET_TUI_SETTINGS_PATH", &settings_path);
        }
        let result = run();
        // The settings override stays in place on purpose: the app keeps
        // writing after the closure returns (settings saves, quit backups,
        // recovery snapshots), and those writes must land in the temp dir
        // too, never in the real user config directory.
        drop(guard);
        result
    }

    #[test]
    fn loading_an_ord_file_keeps_its_metadata() {
        let path = unique_temp_path("ordet-docmeta", "ord");
        write_to_path(
            &path,
            &OrdetDocument {
                title: "Chapter One".to_string(),
                status: "Done".to_string(),
                auto_title: false,
                label_color: "blue".to_string(),
                content_html: "<p>once upon a time</p>".to_string(),
                plain_text: "once upon a time".to_string(),
            },
        )
        .expect("write test ord");

        let loaded = load_buffer_from_path(&path).expect("load test ord");
        assert_eq!(loaded.buffer.plain_text(), "once upon a time");
        assert_eq!(loaded.buffer.cursor(), loaded.buffer.char_count());
        assert_eq!(loaded.metadata.status, "Done");
        assert!(!loaded.metadata.auto_title);
        assert_eq!(loaded.metadata.label_color, "blue");
    }

    #[test]
    fn saving_an_ord_file_preserves_its_metadata() {
        let path = unique_temp_path("ordet-docmeta-save", "ord");
        write_to_path(
            &path,
            &OrdetDocument {
                title: "Chapter One".to_string(),
                status: "In Progress".to_string(),
                auto_title: false,
                label_color: "green".to_string(),
                content_html: "<p>once upon a time</p>".to_string(),
                plain_text: "once upon a time".to_string(),
            },
        )
        .expect("write test ord");

        let mut app = with_test_settings(|| {
            OrdetApp::from_args(vec![path.to_string_lossy().into_owned()])
                .expect("app from ord path")
        });
        assert_eq!(app.state.editor().doc_status, "In Progress");
        assert_eq!(app.state.editor().doc_label_color, "green");

        // An edit plus a background save must keep the loaded metadata while
        // storing the new text.
        app.state.editor_mut().buffer.move_cursor_to_end();
        app.state.editor_mut().buffer.insert_text(" and then");
        app.state.editor_mut().mark_dirty();
        app.spawn_save_to(path.clone());

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut saved = false;
        while !saved && std::time::Instant::now() < deadline {
            while let Some(task) = app.tasks.try_recv() {
                if matches!(
                    task.result,
                    Ok(crate::commands::AppMessage::SaveCompleted { .. })
                ) {
                    saved = true;
                    break;
                }
            }
            if !saved {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        assert!(saved, "save task completed");

        let round_tripped = read_from_path(&path).expect("read saved ord");
        assert_eq!(round_tripped.status, "In Progress");
        assert!(!round_tripped.auto_title);
        assert_eq!(round_tripped.label_color, "green");
        assert_eq!(round_tripped.plain_text, "once upon a time and then");
    }

    #[test]
    fn cli_args_split_flags_from_files() {
        let args = ["--config", "/tmp/x.json", "a.ord", "--", "--weird.ord"]
            .iter()
            .map(|arg| arg.to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            parse_cli_args(&args),
            CliAction::Run {
                files: vec!["a.ord".to_string(), "--weird.ord".to_string()],
                config: Some("/tmp/x.json".to_string()),
                latency_log: false,
            }
        );

        let help = ["--help".to_string()];
        assert_eq!(parse_cli_args(&help), CliAction::Help);
        let version = ["-V".to_string()];
        assert_eq!(parse_cli_args(&version), CliAction::Version);

        let missing = ["--config".to_string()];
        assert!(matches!(parse_cli_args(&missing), CliAction::UsageError(_)));
        let unknown = ["--frobnicate".to_string()];
        assert!(matches!(parse_cli_args(&unknown), CliAction::UsageError(_)));
        let print = [
            "--config=/tmp/y.json".to_string(),
            "--print-config".to_string(),
        ];
        assert_eq!(
            parse_cli_args(&print),
            CliAction::PrintConfig {
                config: Some("/tmp/y.json".to_string())
            }
        );
    }

    #[test]
    fn macos_option_glyphs_resolve_to_their_alt_chords() {
        use crate::commands::Command;
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        let press = |code| KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        // (glyph typed, chord meant, command fired)
        let cases = [
            ('∫', 'b', Command::ToggleBold),
            ('ç', 'c', Command::CopySelection),
            ('´', 'e', Command::ToggleItalic),
            ('µ', 'm', Command::ToggleMenuBar),
            ('ß', 's', Command::OpenStatusPrompt),
            ('†', 't', Command::InsertTimestamp),
            ('¨', 'u', Command::CycleCase),
            ('√', 'v', Command::PasteClipboard),
            ('∑', 'w', Command::CloseWindow),
            ('≈', 'x', Command::CutSelection),
        ];
        for (glyph, letter, command) in cases {
            let normalized = super::normalize_macos_option_key(press(KeyCode::Char(glyph)));
            assert_eq!(normalized.code, KeyCode::Char(letter), "glyph {glyph}");
            assert_eq!(normalized.modifiers, KeyModifiers::ALT);
            assert_eq!(app.keymap.resolve(normalized), Some(command));
        }
        // Untouched: ordinary text, other glyphs, already-modified keys.
        assert_eq!(
            super::normalize_macos_option_key(press(KeyCode::Char('é'))).code,
            KeyCode::Char('é')
        );
        assert_eq!(
            super::normalize_macos_option_key(press(KeyCode::Enter)).code,
            KeyCode::Enter
        );
        let alt_c = KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::ALT,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        assert_eq!(super::normalize_macos_option_key(alt_c), alt_c);
    }

    #[test]
    fn ctrl_a_selects_all_with_a_visible_selection() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::new(), None);
        app.state.editor_mut().buffer = EditorBuffer::from_text("hello world");
        let key_ctrl_a = KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        app.handle_key_event(key_ctrl_a);
        // The whole buffer is selected; word movement now lives on the
        // Option-Left / Option-Right pair.
        assert_eq!(app.state.editor().buffer.selection_range(), Some((0, 11)));
    }

    #[test]
    fn f9_toggles_the_stats_pane() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::new(), None);
        let key_f9 = KeyEvent {
            code: KeyCode::F(9),
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        app.handle_key_event(key_f9);
        assert!(app.state.panels.is(crate::state::PanelKind::Stats));
        app.handle_key_event(key_f9);
        assert!(!app.state.panels.is_open());
    }

    #[test]
    fn ctrl_p_on_a_scratch_draft_opens_the_save_prompt() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::new(), None);
        let key_ctrl_p = KeyEvent {
            code: KeyCode::Char('p'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        app.handle_key_event(key_ctrl_p);
        let prompt = app.state.ui.prompt.as_ref().expect("save prompt");
        assert!(matches!(prompt.kind, crate::prompt::PromptKind::SavePath));
    }

    #[test]
    fn ctrl_c_copies_and_ctrl_v_pastes_in_the_editor() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::new(), None);
        app.state.editor_mut().buffer = EditorBuffer::from_text("copy me");
        // Select-all via Ctrl-K A, copy via Ctrl-C.
        let key = |code, modifiers| KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        app.handle_key_event(key(KeyCode::Char('k'), KeyModifiers::CONTROL));
        app.handle_key_event(key(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(app.state.editor().buffer.selection_range().is_some());
        app.handle_key_event(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(app.state.clipboard.ring_top(), Some("copy me"));

        // Fresh buffer, no selection: Ctrl-V pastes the ring top.
        app.state.editor_mut().buffer = EditorBuffer::from_text("copy me");
        app.handle_key_event(key(KeyCode::Char('v'), KeyModifiers::CONTROL));
        assert_eq!(app.state.editor().buffer.plain_text(), "copy mecopy me");
    }

    #[test]
    fn ctrl_a_ctrl_c_ctrl_v_duplicates_in_a_single_paste() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::new(), None);
        app.state.editor_mut().buffer = EditorBuffer::from_text("copy me");
        let key = |code, modifiers| KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        // Select all, copy: the highlight must clear so the next paste
        // inserts instead of replacing the just-copied text.
        app.handle_key_event(key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert!(app.state.editor().buffer.selection_range().is_some());
        app.handle_key_event(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.state.editor().buffer.selection_range().is_none());

        app.handle_key_event(key(KeyCode::Char('v'), KeyModifiers::CONTROL));
        assert_eq!(app.state.editor().buffer.plain_text(), "copy mecopy me");
    }

    #[test]
    fn quit_asks_for_untitled_but_autosaves_existing_files() {
        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        // Untitled dirty draft: quit must ask, not silently save.
        app.state.add_draft(EditorBuffer::new(), None);
        app.state.editor_mut().buffer.insert_text("unsaved work");
        app.state.editor_mut().mark_dirty();
        app.request_quit();
        assert!(!app.state.ui.should_quit);
        assert!(app.state.ui.quit_confirmation.is_some());

        // Existing file dirty draft: quit flushes and leaves quietly.
        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        let path = std::env::temp_dir().join(format!(
            "ordet-quit-test-{}-existing.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        app.state.add_draft(
            crate::editor_buffer::EditorBuffer::new(),
            Some(path.clone()),
        );
        app.state.editor_mut().buffer.insert_text("saved work");
        app.state.editor_mut().mark_dirty();
        app.request_quit();
        assert!(app.state.ui.should_quit);
        assert!(app.state.ui.quit_confirmation.is_none());
        assert_eq!(
            std::fs::read_to_string(&path).expect("saved file"),
            "saved work"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn quit_n_discards_and_quits_without_saving() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::new(), None);
        app.state.editor_mut().buffer.insert_text("unsaved work");
        app.state.editor_mut().mark_dirty();
        app.request_quit();
        assert!(app.state.ui.quit_confirmation.is_some());

        app.handle_key_event(KeyEvent {
            code: KeyCode::Char('n'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        });
        assert!(app.state.ui.should_quit);
        assert!(app.state.ui.quit_confirmation.is_none());
    }

    #[test]
    fn quit_y_opens_save_prompt_for_untitled() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::new(), None);
        app.state.editor_mut().buffer.insert_text("unsaved work");
        app.state.editor_mut().mark_dirty();
        app.request_quit();
        assert!(app.state.ui.quit_confirmation.is_some());

        app.handle_key_event(KeyEvent {
            code: KeyCode::Char('y'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        });
        // Y saves: no quit yet, a save-path prompt is open instead, with the
        // quit resuming once the save lands.
        assert!(!app.state.ui.should_quit);
        assert!(app.state.ui.quit_confirmation.is_none());
        assert!(
            matches!(
                app.state.ui.prompt.as_ref().map(|prompt| &prompt.kind),
                Some(crate::prompt::PromptKind::SavePath)
            ),
            "Y must open a save prompt for untitled drafts"
        );
        assert!(app.state.ui.quit_after_save);
    }

    #[test]
    fn quit_y_saves_backed_files_and_quits() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        let path =
            std::env::temp_dir().join(format!("ordet-quit-test-{}-y-save.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);
        // Seed the file first so the quit prompt comes from the failure path:
        // point the draft at an unwritable directory target instead.
        let dir =
            std::env::temp_dir().join(format!("ordet-quit-test-{}-y-dir", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        app.state
            .add_draft(crate::editor_buffer::EditorBuffer::new(), Some(dir.clone()));
        app.state.editor_mut().buffer.insert_text("x");
        app.state.editor_mut().mark_dirty();
        app.request_quit();
        assert!(app.state.ui.quit_confirmation.is_some());

        // N discards even failed writes and quits.
        app.handle_key_event(KeyEvent {
            code: KeyCode::Char('n'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        });
        assert!(app.state.ui.should_quit);
        let _ = std::fs::remove_dir(&dir);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn zoom_hides_the_chrome_and_panels_leave_zoom_first() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::new(), None);
        assert!(!app.state.ui.zoomed);
        let esc = || KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };

        app.toggle_zoom();
        assert!(app.state.ui.zoomed);

        // Asking for a panel while zoomed restores the full layout first.
        app.toggle_panel(crate::state::PanelKind::Notes);
        assert!(!app.state.ui.zoomed);
        assert!(app.state.panels.is(crate::state::PanelKind::Notes));

        app.toggle_zoom();
        assert!(app.state.ui.zoomed);

        // Esc leaves zoom first, then closes the panel: the overlay cascade.
        app.handle_key_event(esc());
        assert!(!app.state.ui.zoomed);
        assert!(app.state.panels.is(crate::state::PanelKind::Notes));
        app.handle_key_event(esc());
        assert!(!app.state.panels.is_open());
    }

    #[test]
    fn option_m_hides_the_menu_bar_and_persists_it() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        // This test reads the settings file back after a save, so parallel
        // tests must not repoint `ORDET_TUI_SETTINGS_PATH` between the write
        // and the read. `with_test_settings` releases the lock when its
        // closure returns (construction only); here the whole test holds it.
        // `from_args` runs directly because `with_test_settings` would try to
        // take the already-held lock and deadlock.
        let guard = crate::settings::settings_env_lock()
            .lock()
            .expect("test settings lock");
        let settings_path = unique_temp_path("ordet-settings", "json");
        if settings_path.exists() {
            fs::remove_file(&settings_path).expect("clear stale test settings");
        }
        unsafe {
            std::env::set_var("ORDET_TUI_SETTINGS_PATH", &settings_path);
        }
        let mut app = OrdetApp::from_args(Vec::new()).expect("app with scratch draft");
        app.state.add_draft(EditorBuffer::new(), None);
        assert!(app.state.ui.show_menu_bar);

        let option_m = || KeyEvent {
            code: KeyCode::Char('m'),
            modifiers: KeyModifiers::ALT,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };

        app.handle_key_event(option_m());
        assert!(!app.state.ui.show_menu_bar, "⌥M hides the bar");
        assert!(
            !crate::settings::load_settings()
                .expect("settings load")
                .show_menu_bar,
            "the hidden bar is remembered"
        );

        app.handle_key_event(option_m());
        assert!(app.state.ui.show_menu_bar, "⌥M shows it again");
        assert!(
            crate::settings::load_settings()
                .expect("settings load")
                .show_menu_bar,
            "showing it is remembered too"
        );
        // Release the lock after the last read; the override itself stays
        // set by design (see with_test_settings' comment).
        drop(guard);
    }

    #[test]
    fn settings_toggles_flip_and_persist() {
        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        assert!(app.state.ui.show_live_counts);
        assert!(app.state.ui.backup_enabled);

        app.toggle_live_counts();
        assert!(!app.state.ui.show_live_counts);
        app.toggle_backup();
        assert!(!app.state.ui.backup_enabled);
        app.toggle_backup_on_close();
        assert!(!app.state.ui.backup_on_close);

        let keep = app.state.ui.backup_keep;
        app.cycle_backup_keep();
        assert_ne!(app.state.ui.backup_keep, keep);
    }

    #[test]
    fn page_rows_stay_clickable_with_live_counts_off() {
        use ordet_tui_framework::{Point, Rect};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::new(), None);
        app.state.panels.open(PanelKind::Pages);
        app.state.ui.show_live_counts = false;
        app.state.update_layout(Rect::new(0, 0, 120, 60));
        let area = crate::state::panel_rect(&app.state);
        let mut seen = std::collections::BTreeSet::new();
        for y in area.y..area.bottom() {
            if let Some(index) =
                crate::components::panel_row_at_point(&app.state, Point::new(area.x + 2, y))
            {
                seen.insert(index);
            }
        }
        assert_eq!(
            seen.len(),
            app.state.page_rows().len(),
            "every page row stays clickable with counts off"
        );
    }

    #[test]
    fn backup_location_prompt_resets_and_rejects() {
        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        // Empty resets to the default folder.
        app.state.ui.backup_dir = std::env::temp_dir()
            .join("ordet-custom")
            .display()
            .to_string();
        app.open_backup_location_prompt();
        let prompt = app.state.ui.prompt.take().expect("prompt");
        assert!(matches!(
            prompt.kind,
            crate::prompt::PromptKind::BackupDirectory
        ));
        app.submit_prompt(crate::prompt::PromptState {
            value: "   ".to_string(),
            ..prompt
        });
        assert!(app.state.ui.backup_dir.is_empty());
        assert!(app.state.ui.prompt.is_none());

        // An uncreatable path reopens the prompt instead of saving. A file
        // in the way fails the same way on every platform, unlike a
        // Unix-only pseudo-filesystem path.
        let blocker = unique_temp_path("ordet-backup-blocker", "txt");
        fs::write(&blocker, "not a directory").expect("blocker file");
        let impossible = blocker.join("child").display().to_string();
        app.open_backup_location_prompt();
        let prompt = app.state.ui.prompt.take().expect("prompt");
        app.submit_prompt(crate::prompt::PromptState {
            value: impossible,
            ..prompt
        });
        assert!(app.state.ui.prompt.is_some(), "bad path reopens");
        assert!(app.state.ui.backup_dir.is_empty(), "bad path not saved");
        let _ = fs::remove_file(blocker);
    }

    #[test]
    fn tab_on_a_clean_paragraph_still_indents() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::new(), None);
        app.state
            .editor_mut()
            .buffer
            .insert_text("The cat sat on the mat.");
        app.handle_key_event(KeyEvent {
            code: KeyCode::Tab,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        });
        // A Tab with nothing to fix puts an indent at the cursor.
        assert_eq!(
            app.state.editor().buffer.plain_text(),
            "The cat sat on the mat.    "
        );
    }

    #[test]
    fn tab_at_a_line_start_indents() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::new(), None);
        app.state
            .editor_mut()
            .buffer
            .insert_text("i will come to kevins house");
        app.state.editor_mut().buffer.move_to_top();
        app.handle_key_event(KeyEvent {
            code: KeyCode::Tab,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        });
        // A line start reads as indent intent.
        assert_eq!(
            app.state.editor().buffer.plain_text(),
            "    i will come to kevins house"
        );
    }

    #[test]
    fn quit_backs_up_saved_files_and_prunes() {
        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        let root =
            std::env::temp_dir().join(format!("ordet-backup-quit-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp dir");
        let path = root.join("draft.txt");
        std::fs::write(&path, "v1").expect("seed");
        let backups = root.join("Backups");
        app.state.ui.backup_dir = backups.display().to_string();
        app.state.ui.backup_keep = 2;
        app.state.add_draft(
            crate::editor_buffer::EditorBuffer::new(),
            Some(path.clone()),
        );
        app.state.editor_mut().buffer.insert_text("v2");
        app.state.editor_mut().mark_dirty();

        // Two quits with edits between them must leave two backups, then
        // pruning keeps only the newest two of three.
        app.request_quit();
        assert!(app.state.ui.should_quit);
        app.state.ui.should_quit = false;
        app.state.editor_mut().buffer.insert_text(" more");
        app.state.editor_mut().mark_dirty();
        app.request_quit();
        app.state.ui.should_quit = false;
        app.state.editor_mut().buffer.insert_text(" again");
        app.state.editor_mut().mark_dirty();
        app.request_quit();
        let count = std::fs::read_dir(&backups)
            .expect("read")
            .filter_map(|entry| entry.ok())
            .count();
        assert_eq!(count, 2, "pruning must keep newest two");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn autosave_skips_untitled_drafts() {
        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::new(), None);
        app.state.editor_mut().buffer.insert_text("untitled work");
        app.state.editor_mut().mark_dirty();
        app.next_autosave_at = std::time::Instant::now() - std::time::Duration::from_secs(10);
        app.maybe_autosave();
        assert!(
            !app.state.editor().pending_save,
            "untitled drafts must never autosave"
        );
    }

    #[test]
    fn ctrl_v_pastes_on_the_first_press_in_every_encoding() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let key = |code, modifiers| KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        // Every Ctrl-V and Alt-V encoding is paste, whatever the terminal
        // reports for the letter case or the Shift flag.
        for (code, modifiers) in [
            (KeyCode::Char('v'), KeyModifiers::CONTROL),
            (KeyCode::Char('V'), KeyModifiers::CONTROL),
            (
                KeyCode::Char('v'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
            (
                KeyCode::Char('V'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
            (KeyCode::Char('v'), KeyModifiers::ALT),
            (KeyCode::Char('V'), KeyModifiers::ALT),
        ] {
            assert!(
                super::is_paste_key(key(code, modifiers)),
                "{code:?} {modifiers:?}"
            );
        }
        assert!(!super::is_paste_key(key(
            KeyCode::Char('v'),
            KeyModifiers::NONE
        )));

        // One press pastes even with a Ctrl-K prefix armed: the prefix is
        // cleared and the paste lands in the same press, not the next one.
        // Note: `paste()` prefers the real system clipboard, so tests assert
        // a paste happened (non-empty + prefix cleared), not exact text.
        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::new(), None);
        app.state.clipboard.copy("pasted");
        app.state.editor_mut().buffer = EditorBuffer::from_text("");
        app.handle_key_event(key(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert!(app.state.ui.prefix.is_some());
        app.handle_key_event(key(KeyCode::Char('v'), KeyModifiers::CONTROL));
        assert!(
            !app.state.editor().buffer.plain_text().is_empty(),
            "first Ctrl-V with prefix armed must paste in the same press"
        );
        assert!(app.state.ui.prefix.is_none());

        // Alt-V variants paste on the first press too.
        for code in [KeyCode::Char('v'), KeyCode::Char('V')] {
            let mut app = with_test_settings(|| {
                OrdetApp::from_args(Vec::new()).expect("app with scratch draft")
            });
            app.state.add_draft(EditorBuffer::new(), None);
            app.state.clipboard.copy("alt-paste");
            app.state.editor_mut().buffer = EditorBuffer::from_text("");
            app.handle_key_event(key(code, KeyModifiers::ALT));
            assert!(
                !app.state.editor().buffer.plain_text().is_empty(),
                "Alt-V {code:?} must paste first press"
            );
        }
    }

    #[test]
    fn ctrl_v_pastes_into_prompt_and_browser_on_first_press() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let key = |code, modifiers| KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        // Prompt: first Ctrl-V lands in the prompt value (system clipboard
        // wins over the ring, so assert non-empty rather than exact text).
        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::new(), None);
        app.state.clipboard.copy("prompt-text");
        app.state.ui.prompt = Some(crate::prompt::PromptState::find(Some("")));
        app.handle_key_event(key(KeyCode::Char('v'), KeyModifiers::CONTROL));
        let prompt = app.state.ui.prompt.as_ref().expect("prompt stays open");
        assert!(
            !prompt.value.is_empty(),
            "first Ctrl-V must fill prompt, got {:?}",
            prompt.value
        );
    }

    #[test]
    fn macos_option_c_copies_the_editor_selection() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::new(), None);
        app.state.editor_mut().buffer = EditorBuffer::from_text("copy me");
        // Select-all in the editor (Ctrl-K A), then Option+C (arrives as ç).
        let key_ctrl_k = KeyEvent {
            code: KeyCode::Char('k'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        let key_a = KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        app.handle_key_event(key_ctrl_k);
        app.handle_key_event(key_a);
        assert!(app.state.editor().buffer.selection_range().is_some());
        let option_c = KeyEvent {
            code: KeyCode::Char('ç'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        app.handle_key_event(option_c);
        assert_eq!(app.state.clipboard.ring_top(), Some("copy me"));
    }

    #[test]
    fn f6_hops_pages_and_shift_f6_hops_back() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
        use ordet_tui_framework::Rect;

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::from_text("one"), None);
        app.state.add_draft(EditorBuffer::from_text("two"), None);
        app.state.add_draft(EditorBuffer::from_text("three"), None);
        app.state.update_layout(Rect::new(0, 0, 120, 30));
        assert_eq!(app.state.editor().title(), "three");

        let press = |code, modifiers| KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        // F6 wraps from the last page to the first.
        app.handle_key_event(press(KeyCode::F(6), KeyModifiers::NONE));
        assert_eq!(app.state.editor().title(), "one");
        // Shift-F6 steps back.
        app.handle_key_event(press(KeyCode::F(6), KeyModifiers::SHIFT));
        assert_eq!(app.state.editor().title(), "three");
        // A single visible page says so instead of pretending to move.
        let mut solo =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        solo.state.add_draft(EditorBuffer::from_text("only"), None);
        assert_eq!(solo.state.visible_draft_count(), 1);
        solo.handle_key_event(press(KeyCode::F(6), KeyModifiers::NONE));
        assert!(
            solo.state.ui.status.contains("Only one page"),
            "{}",
            solo.state.ui.status
        );
    }

    #[test]
    fn pages_panel_enter_switches_and_closes() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::from_text("one"), None);
        app.state.add_draft(EditorBuffer::from_text("two"), None);
        app.toggle_panel(PanelKind::Pages);
        assert_eq!(app.state.panels.pages_selected, 1);

        // Enter on the first row switches the editor there and the panel
        // steps out of the way so typing resumes immediately.
        app.state.panels.pages_selected = 0;
        app.handle_key_event(KeyEvent {
            code: KeyCode::Enter,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        });
        assert_eq!(app.state.editor().title(), "one");
        assert!(!app.state.panels.is_open());
    }

    #[test]
    fn alt_n_splits_the_desktop_and_alt_w_closes_it() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::from_text("one"), None);
        app.state.add_draft(EditorBuffer::from_text("two"), None);
        app.state.update_layout(Rect::new(0, 0, 120, 30));

        // The next listed page opens beside the current one and takes the
        // keyboard.
        app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT));
        assert_eq!(app.state.window_count(), 2);
        assert_eq!(app.state.ui.layout.window_rects.len(), 2);
        assert_eq!(app.state.editor().title(), "one");
        assert!(
            app.state.ui.status.is_empty(),
            "splitting is routine and stays quiet: {}",
            app.state.ui.status
        );

        // F6 walks the desktop once it is split, and the split survives.
        app.handle_key_event(KeyEvent::new(KeyCode::F(6), KeyModifiers::empty()));
        assert_eq!(app.state.editor().title(), "two");
        assert_eq!(app.state.window_count(), 2);

        app.handle_key_event(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::ALT));
        assert_eq!(app.state.window_count(), 1);
        assert_eq!(app.state.ui.layout.window_rects.len(), 1);
    }

    #[test]
    fn alt_n_opens_four_windows_then_cycles() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        for title in ["one", "two", "three", "four", "five"] {
            app.state.add_draft(EditorBuffer::from_text(title), None);
        }
        app.state.update_layout(Rect::new(0, 0, 120, 30));

        // Each press opens the next unopened page, up to four windows.
        for (title, windows, position) in [("one", 2, 2), ("two", 3, 3), ("three", 4, 4)] {
            app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT));
            assert_eq!(app.state.window_count(), windows);
            assert_eq!(app.state.ui.layout.window_rects.len(), windows);
            assert_eq!(app.state.active_window_position() + 1, position);
            assert_eq!(app.state.editor().title(), title);
        }

        // A fifth page cannot open on a full desktop; Alt-N cycles instead.
        app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT));
        assert_eq!(app.state.window_count(), 4);
        assert_eq!(app.state.editor().title(), "five");
        assert!(
            app.state.ui.status.is_empty(),
            "cycling is routine and stays quiet: {}",
            app.state.ui.status
        );
    }

    #[test]
    fn window_menu_moves_the_focused_page_to_another_slot() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        for title in ["one", "two", "three", "four"] {
            app.state.add_draft(EditorBuffer::from_text(title), None);
        }
        app.state.update_layout(Rect::new(0, 0, 120, 30));
        for _ in 0..3 {
            app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT));
        }
        assert_eq!(app.state.active_window_position(), 3);
        assert_eq!(app.state.editor().title(), "three");

        // Window ▸ Move to slot 1 swaps the focused page into the first tile.
        app.run_menu_item(6, 2);
        assert_eq!(app.state.active_window_position(), 0);
        assert_eq!(app.state.editor().title(), "three");
        let labels: Vec<String> = app
            .state
            .window_drafts()
            .iter()
            .map(|index| app.state.drafts[*index].page_label())
            .collect();
        assert_eq!(labels, vec!["three", "one", "two", "four"]);
        assert!(
            app.state.ui.status.is_empty(),
            "moving a tile is routine and stays quiet: {}",
            app.state.ui.status
        );
    }

    #[test]
    fn ctrl_o_aligns_the_paragraph_without_a_prompt() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::from_text("hello"), None);

        // The on-screen formatting prefix: ⌃O C centers the line.
        app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert!(app.state.ui.prefix.is_some(), "⌃O starts a prefix");
        app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        assert!(app.state.ui.prefix.is_none(), "the stroke ends the prefix");
        assert_eq!(
            app.state.editor().buffer.current_alignment(),
            ParagraphAlignment::Center
        );

        // Right-align, then a stray letter: cancels and leaves the line alone.
        app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        app.handle_key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        assert_eq!(
            app.state.editor().buffer.current_alignment(),
            ParagraphAlignment::Right
        );
        app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        app.handle_key_event(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        assert!(app.state.ui.prefix.is_none());
        assert_eq!(
            app.state.editor().buffer.current_alignment(),
            ParagraphAlignment::Right
        );
    }

    #[test]
    fn f1_then_a_key_jumps_to_that_commands_help() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::from_text("hello"), None);
        app.state.update_layout(Rect::new(0, 0, 120, 30));

        app.handle_key_event(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
        assert_eq!(app.state.panels.panel, Some(PanelKind::Help));
        assert_eq!(app.state.panels.help_tab, HelpTab::Movement);
        assert_eq!(app.state.panels.scroll, 0);

        // The key is looked up, not run: Save must not write the draft out.
        let before = app.state.drafts.len();
        app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(
            app.state.panels.help_tab,
            HelpTab::Workspace,
            "Save lives on the Workspace page"
        );
        assert_eq!(app.state.drafts.len(), before);
        assert_eq!(app.state.panels.panel, Some(PanelKind::Help));

        // Tab, Shift+Tab and the horizontal arrows switch sections instead
        // of being looked up themselves.
        app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.state.panels.help_tab, HelpTab::Tools);
        app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));
        assert_eq!(app.state.panels.help_tab, HelpTab::Workspace);
        app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.state.panels.help_tab, HelpTab::Tools);
        app.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.state.panels.help_tab, HelpTab::Workspace);

        // Up/Down are inert here, and an unknown key leaves the page alone.
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.state.panels.help_tab, HelpTab::Workspace);
        app.handle_key_event(KeyEvent::new(KeyCode::Char('\u{0}'), KeyModifiers::NONE));
        assert_eq!(app.state.panels.help_tab, HelpTab::Workspace);
    }

    #[test]
    fn clicking_a_window_focuses_the_page_under_the_pointer() {
        use crossterm::event::{
            KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
        };

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::from_text("one"), None);
        app.state.add_draft(EditorBuffer::from_text("two"), None);
        app.state.update_layout(Rect::new(0, 0, 120, 30));
        app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT));
        // The new window is the right one; page two sits on the left.
        assert_eq!(app.state.editor().title(), "one");
        assert_eq!(app.state.active_window_position(), 1);

        app.handle_mouse_event(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.state.active_window_position(), 0);
        assert_eq!(app.state.editor().title(), "two");
    }

    #[test]
    fn alt_arrows_reorder_pages() {
        use crate::commands::Command;
        use ordet_tui_framework::UiEvent;

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::from_text("one"), None);
        app.state.add_draft(EditorBuffer::from_text("two"), None);
        assert_eq!(app.state.editor().title(), "two");

        // Without the panel, Alt-Up moves the page being edited: "two"
        // trades places with "one" and stays current.
        app.dispatch(UiEvent::Command(Command::MovePageUp));
        let labels: Vec<String> = app
            .state
            .page_rows()
            .iter()
            .map(|row| row.label.clone())
            .collect();
        assert_eq!(labels, vec!["two".to_string(), "one".to_string()]);
        assert_eq!(app.state.editor().title(), "two");

        // At the top already: no move, and the status says why.
        app.dispatch(UiEvent::Command(Command::MovePageUp));
        assert_eq!(app.state.editor().title(), "two");
        assert!(
            app.state.ui.status.contains("first page"),
            "{}",
            app.state.ui.status
        );

        // With the panel open the chord acts on the highlighted row.
        app.state.panels.open(PanelKind::Pages);
        app.state.panels.pages_selected = 0;
        app.dispatch(UiEvent::Command(Command::MovePageDown));
        let labels: Vec<String> = app
            .state
            .page_rows()
            .iter()
            .map(|row| row.label.clone())
            .collect();
        assert_eq!(labels, vec!["one".to_string(), "two".to_string()]);

        // The bottom row cannot move down.
        app.state.panels.pages_selected = 1;
        app.dispatch(UiEvent::Command(Command::MovePageDown));
        assert!(
            app.state.ui.status.contains("last page"),
            "{}",
            app.state.ui.status
        );
    }

    #[test]
    fn reordering_pages_keeps_in_flight_saves_pointing_at_the_right_draft() {
        use crate::commands::AppMessage;

        let dir = unique_temp_path("ordet-reorder-save", "");
        fs::create_dir_all(&dir).expect("temp dir");
        // Seed the files first: the app canonicalizes existing paths (macOS
        // temp dirs symlink /var to /private/var), so compare against that
        // canonical form.
        fs::write(dir.join("one.txt"), "one").expect("seed one");
        fs::write(dir.join("two.txt"), "two").expect("seed two");
        let one_path = crate::state::normalize_document_path(dir.join("one.txt"));
        let two_path = crate::state::normalize_document_path(dir.join("two.txt"));

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        let one = app
            .state
            .add_draft(EditorBuffer::from_text("one"), Some(one_path.clone()));
        let two = app
            .state
            .add_draft(EditorBuffer::from_text("two"), Some(two_path.clone()));
        let one_id = app.state.drafts[one].id;
        let two_id = app.state.drafts[two].id;

        // Kick off a save of the trailing page...
        app.state.editor_mut().buffer.insert_text(" edited");
        app.state.editor_mut().mark_dirty();
        app.spawn_save_to(two_path.clone());
        assert!(app.state.drafts[two].editor.pending_save);

        // ...reorder while it is in flight, then save the page that inherited
        // its old row. Index-keyed plumbing used to cancel the first save here
        // and stamp the second page's completion onto the wrong draft.
        assert!(app.state.move_draft(two, -1));
        assert_eq!(app.state.draft_index_by_id(two_id), Some(two - 1));
        let one_now = app.state.draft_index_by_id(one_id).expect("one open");
        app.state.switch_to_draft(one_now);
        app.spawn_save_to(one_path.clone());

        // Both completions must land...
        let mut completed_saves = 0usize;
        let mut results = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while completed_saves < 2 && std::time::Instant::now() < deadline {
            while let Some(task) = app.tasks.try_recv() {
                if matches!(task.result, Ok(AppMessage::SaveCompleted { .. })) {
                    completed_saves += 1;
                }
                results.push(task);
            }
            if completed_saves < 2 {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        assert_eq!(completed_saves, 2, "both in-flight saves must complete");
        for task in results {
            app.handle_task_result(task);
        }

        // ...and each on the page it was started for.
        let two_index = app.state.draft_index_by_id(two_id).expect("two open");
        let one_index = app.state.draft_index_by_id(one_id).expect("one open");
        assert_eq!(
            app.state.drafts[two_index].editor.file_path.as_deref(),
            Some(two_path.as_path()),
            "the save must land on the page it was started for"
        );
        assert_eq!(
            app.state.drafts[one_index].editor.file_path.as_deref(),
            Some(one_path.as_path()),
            "no page may inherit another page's path"
        );
        assert!(!app.state.drafts[two_index].editor.pending_save);
        assert!(!app.state.drafts[one_index].editor.pending_save);
    }

    #[test]
    fn trash_arms_before_it_trashes() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::new(), None);
        // Missing on disk on purpose: the confirm path then only touches
        // test state, never the filesystem.
        let missing = std::env::temp_dir().join("ordet-trash-arm-test-no-such-file.ord");
        app.state.editor_mut().file_path = Some(missing.clone());

        // The first trash command only arms: draft kept, nothing forgotten.
        app.dispatch(UiEvent::Command(Command::TrashFile));
        assert_eq!(app.state.ui.pending_trash, Some(missing.clone()));
        assert_eq!(app.state.editor().file_path, Some(missing.clone()));
        assert!(
            app.state
                .ui
                .status
                .contains("Are you sure you want to delete this page? Y/N"),
            "unexpected prompt: {}",
            app.state.ui.status
        );

        // Any other key disarms: the next trash command re-arms instead of firing.
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::empty()));
        assert_eq!(app.state.ui.pending_trash, None);
        app.dispatch(UiEvent::Command(Command::TrashFile));
        assert_eq!(app.state.ui.pending_trash, Some(missing.clone()));
        assert_eq!(app.state.editor().file_path, Some(missing.clone()));

        // N disarms without trashing and types nothing.
        app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::empty()));
        assert_eq!(app.state.ui.pending_trash, None);
        assert_eq!(app.state.editor().file_path, Some(missing.clone()));

        // Y fires: missing file, draft closed.
        app.dispatch(UiEvent::Command(Command::TrashFile));
        assert_eq!(app.state.ui.pending_trash, Some(missing.clone()));
        app.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::empty()));
        assert_eq!(app.state.ui.pending_trash, None);
        assert_eq!(app.state.editor().file_path, None);

        // Second consecutive Ctrl+R still fires: missing file, draft closed.
        app.state.editor_mut().file_path = Some(missing.clone());
        app.dispatch(UiEvent::Command(Command::TrashFile));
        app.dispatch(UiEvent::Command(Command::TrashFile));
        assert_eq!(app.state.ui.pending_trash, None);
        assert_eq!(app.state.editor().file_path, None);
    }

    #[test]
    fn delete_confirms_before_closing_even_a_clean_unsaved_page() {
        use crate::commands::Action;

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::new(), None);

        // Delete asks...
        app.apply_action(Action::TrashRequested { confirm: false });
        assert_eq!(app.state.ui.pending_close_draft, Some(1));
        assert_eq!(app.state.drafts.len(), 2);

        // ...Delete again still only asks, never fires.
        app.apply_action(Action::TrashRequested { confirm: false });
        assert_eq!(app.state.ui.pending_close_draft, Some(1));
        assert_eq!(app.state.drafts.len(), 2);

        // ...only Ctrl+R confirms.
        app.apply_action(Action::TrashRequested { confirm: true });
        assert_eq!(app.state.ui.pending_close_draft, None);
        assert_eq!(app.state.drafts.len(), 1);
        assert_eq!(app.state.active_draft, 0);
        assert!(app.state.editor().buffer.plain_text().is_empty());
    }

    #[test]
    fn delete_lands_on_the_surviving_page() {
        use crate::commands::Action;

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::new(), None);
        app.state.add_draft(EditorBuffer::new(), None);
        assert_eq!(app.state.active_draft, 2);

        // On the last page: Delete asks, Y closes it, and the editor lands
        // on the surviving page instead of the removed slot.
        app.apply_action(Action::TrashRequested { confirm: false });
        assert_eq!(app.state.ui.pending_close_draft, Some(2));
        app.apply_action(Action::TrashRequested { confirm: true });
        assert_eq!(app.state.drafts.len(), 2);
        assert_eq!(app.state.active_draft, 1);
    }

    #[test]
    fn delete_arms_before_discarding_a_dirty_unsaved_page() {
        use crate::commands::Action;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state
            .add_draft(EditorBuffer::from_text("words here"), None);
        app.state.editor_mut().buffer.insert_text("more");
        app.state.editor_mut().mark_dirty();

        // First Delete only arms: the page survives.
        app.apply_action(Action::TrashRequested { confirm: false });
        assert_eq!(app.state.ui.pending_close_draft, Some(1));
        assert_eq!(app.state.drafts.len(), 2);

        // Any other key cancels the discard and wipes its message.
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::empty()));
        assert_eq!(app.state.ui.pending_close_draft, None);
        assert_eq!(app.state.drafts.len(), 2);
        assert!(
            !app.state.ui.status.contains("delete this page?"),
            "stale confirm message survived moving: {}",
            app.state.ui.status
        );

        // Delete asks, Y discards it.
        app.apply_action(Action::TrashRequested { confirm: false });
        assert!(
            app.state
                .ui
                .status
                .contains("Are you sure you want to delete this page? Y/N"),
            "unexpected prompt: {}",
            app.state.ui.status
        );
        app.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::empty()));
        assert_eq!(app.state.ui.pending_close_draft, None);
        assert_eq!(app.state.drafts.len(), 1);

        // Delete asks, N keeps the page and types nothing.
        app.apply_action(Action::TrashRequested { confirm: false });
        assert_eq!(app.state.ui.pending_close_draft, Some(0));
        app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::empty()));
        assert_eq!(app.state.ui.pending_close_draft, None);
        assert_eq!(app.state.drafts.len(), 1);
    }

    #[test]
    fn esc_closes_the_open_panel_but_yields_to_prefix() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        // Leave the welcome screen: panel Esc yields to it there.
        app.state.add_draft(EditorBuffer::new(), None);
        let esc = KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        };
        app.state.panels.open(PanelKind::Notes);
        app.handle_key_event(esc);
        assert!(!app.state.panels.is_open());

        app.handle_key_event(esc);
        assert!(!app.state.panels.is_open());

        // A pending Ctrl-K prefix owns Esc: it cancels the prefix instead.
        // The waiting hint is persistent, so it does not vanish after three
        // seconds while the chord is still armed.
        app.state.panels.open(PanelKind::Notes);
        app.handle_key_event(KeyEvent {
            code: KeyCode::Char('k'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        });
        assert_eq!(app.state.ui.prefix, Some(crate::state::PrefixMode::CtrlK));
        assert!(
            app.state.ui.status.starts_with("⌃K pressed"),
            "unexpected hint: {}",
            app.state.ui.status
        );
        assert!(app.state.ui.status_expires_at.is_none());
        app.handle_key_event(esc);
        assert!(app.state.panels.is(PanelKind::Notes));
        assert_eq!(app.state.ui.prefix, None);
        assert!(
            app.state.ui.status.starts_with("Notes."),
            "panel hint should return: {}",
            app.state.ui.status
        );
    }

    #[test]
    fn mouse_click_drag_and_wheel_drive_the_editor() {
        use crate::components::editor_text_area;
        use ordet_tui_framework::{Point, Rect};

        let path = unique_temp_path("ordet-mouse", "txt");
        fs::write(&path, "alpha beta\ngamma delta").expect("write test file");
        let mut app = with_test_settings(|| {
            OrdetApp::from_args(vec![path.to_string_lossy().into_owned()])
                .expect("app from text path")
        });
        app.state.update_layout(Rect::new(0, 0, 80, 24));
        let text_area = editor_text_area(app.state.ui.layout.editor);

        // Click on the second visual line, a few columns in.
        let click = Point::new(text_area.x + 3, text_area.y + 1);
        app.mouse_press(click);
        let (wrap_width, _) = crate::components::wrap_and_gutter(
            text_area.width as usize,
            &app.state.editor().buffer,
            app.state.ui.show_line_numbers,
        );
        let expected = app
            .state
            .editor()
            .buffer
            .visual_cursor_for_column(wrap_width, 1, 3);
        assert_eq!(app.state.editor().buffer.cursor(), expected);

        // Drag extends a selection from the press point.
        app.mouse_drag(Point::new(text_area.x + 6, text_area.y + 1));
        let selection = app.state.editor().buffer.selection_range();
        assert!(selection.is_some());
        assert_ne!(selection.unwrap().0, selection.unwrap().1);
        app.mouse_drag(Point::new(text_area.x + 6, text_area.y + 1));
        // Releasing ends the gesture without clearing the selection.
        app.handle_mouse_event(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: text_area.x + 6,
            row: text_area.y + 1,
            modifiers: crossterm::event::KeyModifiers::NONE,
        });
        assert!(app.state.editor().buffer.selection_range().is_some());

        // Wheel over the editor scrolls it.
        let before = app.state.editor().scroll;
        app.mouse_scroll(Point::new(text_area.x, text_area.y), false);
        assert_eq!(app.state.editor().scroll, before + 3);
        app.mouse_scroll(Point::new(text_area.x, text_area.y), true);
        assert_eq!(app.state.editor().scroll, before);
    }

    #[test]
    fn hovering_a_page_row_moves_the_highlight() {
        use ordet_tui_framework::{Point, Rect};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::from_text("page"), None);
        app.state.panels.open(PanelKind::Pages);
        app.state.update_layout(Rect::new(0, 0, 120, 60));
        let area = crate::state::panel_rect(&app.state);
        let point = (area.y..area.bottom())
            .find_map(|y| {
                let point = Point::new(area.x + 2, y);
                (crate::components::panel_row_at_point(&app.state, point) == Some(0))
                    .then_some(point)
            })
            .expect("clickable page row");
        app.mouse_hover(point);
        assert_eq!(app.state.panels.pages_selected, 0);
        // Hovering only moves the highlight: the edited page never changed.
        assert_eq!(app.state.editor().title(), "page");
    }

    #[test]
    fn clicking_a_page_row_switches_and_closes_the_panel() {
        use ordet_tui_framework::{Point, Rect};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::from_text("one"), None);
        app.state.add_draft(EditorBuffer::from_text("two"), None);
        app.state.panels.open(PanelKind::Pages);
        app.state.update_layout(Rect::new(0, 0, 120, 60));
        let area = crate::state::panel_rect(&app.state);
        let point = (area.y..area.bottom())
            .find_map(|y| {
                let point = Point::new(area.x + 2, y);
                (crate::components::panel_row_at_point(&app.state, point) == Some(0))
                    .then_some(point)
            })
            .expect("clickable page row");
        app.mouse_press(point);
        assert_eq!(app.state.editor().title(), "one");
        assert!(!app.state.panels.is_open());
    }

    #[test]
    fn restoring_a_revision_brings_back_saved_text_undoably() {
        let path = unique_temp_path("ordet-revision-restore", "txt");
        fs::write(&path, "first version").expect("write test file");

        let mut app = with_test_settings(|| {
            OrdetApp::from_args(vec![path.to_string_lossy().into_owned()])
                .expect("app from text path")
        });

        // Save "first version" so the history has a snapshot, then keep
        // typing without saving.
        app.spawn_save_to(path.clone());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut saved = false;
        while !saved && std::time::Instant::now() < deadline {
            while let Some(task) = app.tasks.try_recv() {
                if matches!(
                    task.result,
                    Ok(crate::commands::AppMessage::SaveCompleted { .. })
                ) {
                    saved = true;
                    break;
                }
            }
            if !saved {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        assert!(saved, "save task completed");

        app.state.editor_mut().buffer.move_cursor_to_end();
        app.state.editor_mut().buffer.insert_text(" plus unsaved");
        app.state.editor_mut().mark_dirty();
        assert_eq!(
            app.state.editor().buffer.plain_text(),
            "first version plus unsaved"
        );

        app.restore_selected_revision();
        assert_eq!(app.state.editor().buffer.plain_text(), "first version");

        // The restore went through the edit path, so one undo returns the
        // unsaved text instead of stranding it.
        assert!(app.state.editor_mut().buffer.undo());
        assert_eq!(
            app.state.editor().buffer.plain_text(),
            "first version plus unsaved"
        );
    }

    #[test]
    fn only_unsaved_drafts_are_snapshotted_for_recovery() {
        with_test_settings(|| {
            let saved = std::env::temp_dir().join("Saved.ord");
            let dirty = std::env::temp_dir().join("Dirty.ord");
            let mut state = AppState::new(
                vec![
                    (EditorBuffer::from_text("saved already"), Some(saved)),
                    (EditorBuffer::from_text("still typing"), Some(dirty.clone())),
                ],
                Vec::new(),
                ThemeSelection::default(),
                None,
                12,
                Some(false),
            );

            state.switch_to_draft(1);
            state.editor_mut().buffer.move_cursor_to_end();
            state.editor_mut().buffer.insert_text(" more");
            state.editor_mut().mark_dirty();

            let snapshot = state.unsaved_drafts();

            assert_eq!(snapshot.len(), 1);
            assert_eq!(snapshot[0].title, "Dirty.ord");
            assert_eq!(snapshot[0].text, "still typing more");
            assert_eq!(
                snapshot[0].path,
                Some(crate::state::normalize_document_path(&dirty))
            );
        });
    }

    #[test]
    fn a_scratch_draft_is_snapshotted_without_a_path() {
        with_test_settings(|| {
            let mut state = AppState::new(
                vec![(EditorBuffer::from_text("notes"), None)],
                Vec::new(),
                ThemeSelection::default(),
                None,
                12,
                Some(false),
            );
            state.editor_mut().mark_dirty();

            let snapshot = state.unsaved_drafts();

            assert_eq!(snapshot.len(), 1);
            assert_eq!(snapshot[0].path, None);
            assert_eq!(snapshot[0].text, "notes");
        });
    }

    #[test]
    fn recovered_drafts_stay_dirty_when_the_launch_opens_files() {
        with_test_settings(|| {
            // A crashed session left one unsaved draft behind...
            let recovery = unique_temp_path("ordet-recovery-at-launch", "json");
            crate::recovery::write_recovery_to(
                &recovery,
                &crate::recovery::RecoveryFile {
                    drafts: vec![crate::recovery::RecoveredDraft {
                        path: None,
                        title: "Untitled".to_string(),
                        text: "words the crash did not eat".to_string(),
                    }],
                },
            )
            .expect("write recovery snapshot");
            unsafe {
                std::env::set_var("ORDET_TUI_RECOVERY_PATH", &recovery);
            }

            // ...and this launch also opens a file that is already saved.
            let opened = unique_temp_path("ordet-recovery-opened", "txt");
            fs::write(&opened, "already on disk").expect("write the opened file");

            let app = OrdetApp::from_args(vec![opened.display().to_string()]).expect("app");

            unsafe {
                std::env::remove_var("ORDET_TUI_RECOVERY_PATH");
            }

            // Only the recovered draft needs saving; the opened file does not.
            let snapshot = app.state.unsaved_drafts();
            assert_eq!(
                snapshot.len(),
                1,
                "the recovered draft must remain marked unsaved"
            );
            assert_eq!(snapshot[0].text, "words the crash did not eat");
        });
    }

    #[test]
    fn a_lone_empty_scratch_draft_gives_way_to_recovered_work() {
        let scratch = vec![(EditorBuffer::new(), None)];
        assert!(super::is_empty_scratch(&scratch));

        let with_text = vec![(EditorBuffer::from_text("typed"), None)];
        assert!(!super::is_empty_scratch(&with_text));

        let named = vec![(EditorBuffer::new(), Some(PathBuf::from("/tmp/A.ord")))];
        assert!(!super::is_empty_scratch(&named));

        let two = vec![(EditorBuffer::new(), None), (EditorBuffer::new(), None)];
        assert!(!super::is_empty_scratch(&two));
    }

    #[test]
    fn settings_picker_preselects_the_current_value() {
        use crate::state::SettingsPickerKind;
        use crate::theme::{ThemePreset, ThemeSelection};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.ui.theme = ThemeSelection::Preset {
            preset: ThemePreset::Stalker,
        };
        app.open_settings_picker(SettingsPickerKind::Theme);
        let picker = app
            .state
            .panels
            .settings_picker
            .as_ref()
            .expect("theme picker");
        assert_eq!(picker.options.len(), ThemePreset::ALL.len());
        assert_eq!(picker.selected, 2, "Stalker is third");
        assert_eq!(picker.active, 2);

        app.state.panels.settings_picker = None;
        app.state.ui.language = crate::i18n::UiLanguage::Indonesian;
        app.open_settings_picker(SettingsPickerKind::Language);
        let picker = app
            .state
            .panels
            .settings_picker
            .as_ref()
            .expect("language picker");
        assert_eq!(picker.options.len(), 4);
        assert_eq!(picker.selected, 1);

        app.state.panels.settings_picker = None;
        app.state.ui.indent_width = 8;
        app.open_settings_picker(SettingsPickerKind::IndentWidth);
        let picker = app
            .state
            .panels
            .settings_picker
            .as_ref()
            .expect("indent picker");
        assert_eq!(
            picker.options,
            vec!["2".to_string(), "4".to_string(), "8".to_string()]
        );
        assert_eq!(picker.selected, 2);
    }

    #[test]
    fn settings_submit_opens_a_picker_and_enter_applies() {
        use crate::state::SettingsPickerKind;
        use crate::theme::{ThemePreset, ThemeSelection};
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::new(), None);
        app.state.ui.theme = ThemeSelection::Preset {
            preset: ThemePreset::Midnight,
        };
        app.state.panels.open(PanelKind::Settings);
        assert_eq!(app.state.panels.settings_selected, 0, "Theme row");

        // Enter on the Theme row lists instead of cycling.
        app.dispatch(UiEvent::Command(Command::Submit));
        let picker = app
            .state
            .panels
            .settings_picker
            .as_ref()
            .expect("picker opens on Submit");
        assert_eq!(picker.title, "Theme");
        assert_eq!(picker.options.len(), ThemePreset::ALL.len());
        assert_eq!(picker.selected, 0, "Midnight preselected");
        assert_eq!(
            app.state.ui.theme,
            ThemeSelection::Preset {
                preset: ThemePreset::Midnight
            },
            "opening changes nothing"
        );

        let key = |code| KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        };
        app.handle_key_event(key(KeyCode::Down));
        app.handle_key_event(key(KeyCode::Down));
        assert_eq!(
            app.state
                .panels
                .settings_picker
                .as_ref()
                .expect("open")
                .selected,
            2
        );
        app.handle_key_event(key(KeyCode::Enter));
        assert_eq!(app.state.panels.settings_picker, None, "Enter closes");
        assert_eq!(
            app.state.ui.theme,
            ThemeSelection::Preset {
                preset: ThemePreset::Stalker
            }
        );

        // Esc and Left close without applying.
        app.open_settings_picker(SettingsPickerKind::Language);
        app.handle_key_event(key(KeyCode::Down));
        app.handle_key_event(key(KeyCode::Esc));
        assert_eq!(app.state.panels.settings_picker, None);
        assert_eq!(app.state.ui.language, crate::i18n::UiLanguage::English);
        app.open_settings_picker(SettingsPickerKind::Language);
        app.handle_key_event(key(KeyCode::Left));
        assert_eq!(app.state.panels.settings_picker, None);
        assert_eq!(app.state.ui.language, crate::i18n::UiLanguage::English);

        // Right confirms like Enter.
        app.open_settings_picker(SettingsPickerKind::IndentWidth);
        app.handle_key_event(key(KeyCode::Right));
        assert_eq!(app.state.panels.settings_picker, None);
    }

    #[test]
    fn settings_picker_mouse_click_confirms_and_outside_closes() {
        use ordet_tui_framework::Point;

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::new(), None);
        app.state.ui.theme = crate::theme::ThemeSelection::Preset {
            preset: crate::theme::ThemePreset::Midnight,
        };
        app.state.panels.open(PanelKind::Settings);
        app.state.update_layout(Rect::new(0, 0, 120, 30));
        app.open_settings_picker(crate::state::SettingsPickerKind::Theme);

        // Hover retargets the highlight.
        let list = crate::state::settings_picker_list_rect(
            app.full_area(),
            crate::theme::ThemePreset::ALL.len(),
        );
        assert!(
            list.height > 2,
            "all {} themes fit: {list:?}",
            crate::theme::ThemePreset::ALL.len()
        );
        let hover = Point::new(list.x + 1, list.y + 5);
        app.mouse_hover(hover);
        assert_eq!(
            app.state
                .panels
                .settings_picker
                .as_ref()
                .expect("open")
                .selected,
            5
        );

        // Clicking a row applies it: Jodorowsky is eighth.
        let click = Point::new(list.x + 1, list.y + 7);
        app.mouse_press(click);
        assert_eq!(app.state.panels.settings_picker, None, "click closes");
        assert_eq!(
            app.state.ui.theme,
            crate::theme::ThemeSelection::Preset {
                preset: crate::theme::ThemePreset::Jodorowsky
            },
            "eighth row applies"
        );

        // Clicking outside closes without applying.
        app.open_settings_picker(crate::state::SettingsPickerKind::Language);
        app.mouse_press(Point::new(0, 0));
        assert_eq!(app.state.panels.settings_picker, None);
        assert_eq!(app.state.ui.language, crate::i18n::UiLanguage::English);
    }

    #[test]
    fn settings_picker_movement_clamps_at_both_ends() {
        use crate::state::{SettingsPickerKind, SettingsPickerState};

        let mut picker = SettingsPickerState {
            kind: SettingsPickerKind::Theme,
            title: "Theme".to_string(),
            options: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            selected: 0,
            scroll: 0,
            active: 0,
            preview_from: None,
        };
        picker.move_up();
        assert_eq!((picker.selected, picker.scroll), (0, 0));
        picker.move_end();
        assert_eq!((picker.selected, picker.scroll), (2, 0));
        picker.move_down();
        assert_eq!((picker.selected, picker.scroll), (2, 0));
        picker.move_home();
        assert_eq!((picker.selected, picker.scroll), (0, 0));
        // A two-row window pushes the scroll after the highlight.
        picker.selected = 2;
        picker.scroll = 0;
        picker.clamp_to(2);
        assert_eq!(picker.scroll, 1);
        picker.move_up();
        assert_eq!((picker.selected, picker.scroll), (1, 1));
    }

    #[test]
    fn settings_picker_theme_previews_live_and_esc_restores() {
        use crate::state::SettingsPickerKind;
        use crate::theme::{ThemePreset, ThemeSelection};
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.ui.theme = ThemeSelection::Preset {
            preset: ThemePreset::Midnight,
        };
        app.open_settings_picker(SettingsPickerKind::Theme);
        assert!(app.state.panels.settings_picker.is_some());

        let key = |code| KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        };
        // Browsing applies immediately, without persisting or closing.
        app.handle_key_event(key(KeyCode::Down));
        assert_eq!(
            app.state.ui.theme,
            ThemeSelection::Preset {
                preset: ThemePreset::Ordet
            },
            "highlight previews live"
        );
        assert!(app.state.panels.settings_picker.is_some());

        // Esc restores whatever was in force and closes.
        app.handle_key_event(key(KeyCode::Down));
        assert_eq!(
            app.state.ui.theme,
            ThemeSelection::Preset {
                preset: ThemePreset::Stalker
            }
        );
        app.handle_key_event(key(KeyCode::Esc));
        assert_eq!(app.state.panels.settings_picker, None);
        assert_eq!(
            app.state.ui.theme,
            ThemeSelection::Preset {
                preset: ThemePreset::Midnight
            },
            "Esc restores the pre-browse theme"
        );

        // Enter keeps the previewed theme and closes.
        app.open_settings_picker(SettingsPickerKind::Theme);
        app.handle_key_event(key(KeyCode::Down));
        app.handle_key_event(key(KeyCode::Enter));
        assert_eq!(app.state.panels.settings_picker, None);
        assert_eq!(
            app.state.ui.theme,
            ThemeSelection::Preset {
                preset: ThemePreset::Ordet
            }
        );
    }

    #[test]
    fn settings_picker_only_themes_preview_live() {
        use crate::state::SettingsPickerKind;
        use crate::theme::{ThemePreset, ThemeSelection};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.ui.theme = ThemeSelection::Preset {
            preset: ThemePreset::Stalker,
        };
        app.state.ui.language = crate::i18n::UiLanguage::Indonesian;
        app.open_settings_picker(SettingsPickerKind::Language);
        app.state
            .panels
            .settings_picker
            .as_mut()
            .expect("open")
            .selected = 2;
        app.preview_settings_picker_theme();
        assert_eq!(
            app.state.ui.theme,
            ThemeSelection::Preset {
                preset: ThemePreset::Stalker
            },
            "other kinds never touch the theme"
        );
    }

    #[test]
    fn test_menu_catalog_is_fully_wired() {
        use crate::commands::{MenuEffect, MENU_BAR};

        assert!(!MENU_BAR.is_empty());
        for menu in MENU_BAR {
            assert!(!menu.title.is_empty());
            assert!(!menu.items.is_empty());
            for item in menu.items {
                assert!(!item.label.is_empty());
                match &item.effect {
                    MenuEffect::Action(_) | MenuEffect::Command(_) => {}
                }
            }
        }
        // Spot-check titles the header paints.
        let titles: Vec<&str> = MENU_BAR.iter().map(|m| m.title).collect();
        assert_eq!(
            titles,
            vec!["File", "Edit", "View", "Insert", "Format", "Page", "Window", "Help"]
        );
    }

    #[test]
    fn window_menu_only_lists_windows_that_exist() {
        use crate::commands::{menu_items, MENU_BAR};

        let window = MENU_BAR
            .iter()
            .position(|menu| menu.title == "Window")
            .expect("Window menu");
        let labels = |windows: usize| {
            menu_items(window, windows)
                .into_iter()
                .map(|item| item.label)
                .collect::<Vec<_>>()
        };

        assert_eq!(labels(1), vec!["Next window"]);
        assert_eq!(
            labels(2),
            vec![
                "Next window",
                "Close window",
                "Move to slot 1",
                "Move to slot 2",
            ]
        );
        assert_eq!(labels(3).len(), 5);
        assert_eq!(labels(4).len(), 6);
    }

    #[test]
    fn test_menu_keys_open_navigate_run_and_close() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        // A second draft leaves the welcome screen so menu keys route.
        app.state
            .add_draft(crate::editor_buffer::EditorBuffer::new(), None);
        assert!(!app.state.show_welcome_screen());

        let key = |code: KeyCode| KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };

        // F10 opens the File menu. The menu explains itself through its rows;
        // it does not take over the status line with a hint.
        app.handle_key_event(key(KeyCode::F(10)));
        assert_eq!(app.state.ui.menu_open, Some(0));
        assert_eq!(app.state.ui.menu_selected, 0);
        assert!(
            app.state.ui.status.is_empty(),
            "status: {}",
            app.state.ui.status
        );

        // Right switches to Edit, Down/Up move, End clamps to the last item.
        app.handle_key_event(key(KeyCode::Right));
        assert_eq!(app.state.ui.menu_open, Some(1));
        assert_eq!(app.state.ui.menu_selected, 0);
        app.handle_key_event(key(KeyCode::Down));
        app.handle_key_event(key(KeyCode::Down));
        assert_eq!(app.state.ui.menu_selected, 2);
        app.handle_key_event(key(KeyCode::Up));
        assert_eq!(app.state.ui.menu_selected, 1);
        app.handle_key_event(key(KeyCode::Left));
        assert_eq!(app.state.ui.menu_open, Some(0));

        // Esc closes without running anything.
        let drafts = app.state.drafts.len();
        app.handle_key_event(key(KeyCode::Esc));
        assert!(app.state.ui.menu_open.is_none());
        assert_eq!(app.state.drafts.len(), drafts);

        // Enter on File → New draft adds a draft and closes the menu.
        app.handle_key_event(key(KeyCode::F(10)));
        app.handle_key_event(key(KeyCode::Enter));
        assert!(app.state.ui.menu_open.is_none());
        assert_eq!(app.state.drafts.len(), drafts + 1);

        // F10 toggles an open menu closed.
        app.handle_key_event(key(KeyCode::F(10)));
        assert!(app.state.ui.menu_open.is_some());
        app.handle_key_event(key(KeyCode::F(10)));
        assert!(app.state.ui.menu_open.is_none());
    }

    #[test]
    fn test_menu_mouse_opens_runs_and_dismisses() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state
            .add_draft(crate::editor_buffer::EditorBuffer::new(), None);
        app.state.ui.layout.header = ordet_tui_framework::Rect::new(0, 0, 80, 1);
        app.state.ui.layout.status = ordet_tui_framework::Rect::new(0, 20, 80, 2);

        let click = |column: u16, row: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        let release = |column: u16, row: u16| MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        let hover = |column: u16, row: u16| MouseEvent {
            kind: MouseEventKind::Moved,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };

        // Click the File title: cells start at x=0, "File" is 4 wide.
        let cells = crate::state::menu_title_cells(app.state.ui.layout.header);
        assert!(!cells.is_empty());
        app.handle_mouse_event(click(cells[0].x + 1, 0));
        assert_eq!(app.state.ui.menu_open, Some(0));

        // Hover traces the highlight without running anything. Rows live
        // inside the border, so hit-test the inner list rect.
        let inner = crate::state::menu_drop_inner_rect(
            app.state.ui.layout.header,
            app.state.ui.layout.status.y,
            0,
            crate::commands::MENU_BAR[0].items.len(),
        );
        assert!(inner.height > 0);
        app.handle_mouse_event(hover(inner.x + 1, inner.y + 2));
        assert_eq!(app.state.ui.menu_open, Some(0));
        assert_eq!(app.state.ui.menu_selected, 2);

        // Press arms, release on the row runs: File → New draft.
        let drafts = app.state.drafts.len();
        app.handle_mouse_event(click(inner.x + 1, inner.y));
        assert_eq!(app.state.ui.menu_open, Some(0));
        assert_eq!(app.state.drafts.len(), drafts);
        app.handle_mouse_event(release(inner.x + 1, inner.y));
        assert!(app.state.ui.menu_open.is_none());
        assert_eq!(app.state.drafts.len(), drafts + 1);

        // Clicking outside an open menu just closes it.
        app.handle_mouse_event(click(cells[1].x + 1, 0));
        assert_eq!(app.state.ui.menu_open, Some(1));
        app.handle_mouse_event(click(79, 10));
        assert!(app.state.ui.menu_open.is_none());
        app.handle_mouse_event(release(79, 10));
        assert!(app.state.ui.menu_open.is_none());

        // Press-drag-release across rows runs the release row (here Open
        // file, which opens the browser, so it goes last).
        app.handle_mouse_event(click(cells[0].x + 1, 0));
        assert_eq!(app.state.ui.menu_open, Some(0));
        let inner = crate::state::menu_drop_inner_rect(
            app.state.ui.layout.header,
            app.state.ui.layout.status.y,
            0,
            crate::commands::MENU_BAR[0].items.len(),
        );
        app.handle_mouse_event(click(inner.x + 1, inner.y));
        app.handle_mouse_event(hover(inner.x + 1, inner.y + 1));
        app.handle_mouse_event(release(inner.x + 1, inner.y + 1));
        assert!(app.state.ui.menu_open.is_none());
    }

    #[test]
    fn test_bookmarks_set_and_jump_through_ctrl_k() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state
            .add_draft(crate::editor_buffer::EditorBuffer::new(), None);
        // A real editor rect so visual-line movement is not degenerate.
        app.state.ui.layout.editor = ordet_tui_framework::Rect::new(0, 0, 80, 24);

        let key = |code: KeyCode, modifiers: KeyModifiers| KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        let ctrl_k = || key(KeyCode::Char('k'), KeyModifiers::CONTROL);

        // Type some text; the cursor ends at 11.
        for ch in "hello world".chars() {
            app.handle_key_event(key(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        assert_eq!(app.state.editor().buffer.cursor(), 11);

        // Ctrl-K 3 drops bookmark 3 at the cursor.
        app.handle_key_event(ctrl_k());
        app.handle_key_event(key(KeyCode::Char('3'), KeyModifiers::NONE));
        assert_eq!(app.state.editor().bookmarks[3], Some(11));

        app.handle_key_event(key(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(app.state.editor().buffer.cursor(), 0);
        app.handle_key_event(ctrl_k());
        app.handle_key_event(key(KeyCode::Char('q'), KeyModifiers::NONE));
        app.handle_key_event(key(KeyCode::Char('3'), KeyModifiers::NONE));
        assert_eq!(app.state.editor().buffer.cursor(), 11);

        // Jumping to an empty bookmark reports it and stays put.
        app.handle_key_event(ctrl_k());
        app.handle_key_event(key(KeyCode::Char('q'), KeyModifiers::NONE));
        app.handle_key_event(key(KeyCode::Char('7'), KeyModifiers::NONE));
        assert_eq!(app.state.editor().buffer.cursor(), 11);
        assert!(app.state.editor().bookmarks[7].is_none());
    }

    #[test]
    fn test_quick_bookmarks_need_no_slot_numbers() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state
            .add_draft(crate::editor_buffer::EditorBuffer::new(), None);
        app.state.ui.layout.editor = ordet_tui_framework::Rect::new(0, 0, 80, 24);

        let key = |code: KeyCode, modifiers: KeyModifiers| KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };

        // No marks yet: jumping to the last one reports it and stays put.
        app.apply_action(crate::commands::Action::JumpLastBookmark);
        assert_eq!(app.state.editor().buffer.cursor(), 0);

        for ch in "hello world".chars() {
            app.handle_key_event(key(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        // Dropping picks the lowest free slot on its own.
        app.apply_action(crate::commands::Action::DropQuickBookmark);
        assert_eq!(app.state.editor().bookmarks[0], Some(11));
        app.handle_key_event(key(KeyCode::Home, KeyModifiers::NONE));
        app.apply_action(crate::commands::Action::DropQuickBookmark);
        assert_eq!(app.state.editor().bookmarks[1], Some(0));

        // The menu-free chord way: Ctrl-K Q Enter jumps to the last mark.
        app.handle_key_event(key(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(app.state.editor().buffer.cursor(), 11);
        app.handle_key_event(key(KeyCode::Char('k'), KeyModifiers::CONTROL));
        app.handle_key_event(key(KeyCode::Char('q'), KeyModifiers::NONE));
        app.handle_key_event(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.state.editor().buffer.cursor(), 0);
    }

    #[test]
    fn test_alt_bold_italic_style_the_selection() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state
            .add_draft(crate::editor_buffer::EditorBuffer::new(), None);

        let key = |code: KeyCode, modifiers: KeyModifiers| KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        for ch in "hello".chars() {
            app.handle_key_event(key(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        // Select "llo" with Shift+Left x3, then Alt-B bolds it.
        for _ in 0..3 {
            app.handle_key_event(key(KeyCode::Left, KeyModifiers::SHIFT));
        }
        app.handle_key_event(key(KeyCode::Char('b'), KeyModifiers::ALT));
        let bolds: Vec<(usize, usize)> = app
            .state
            .editor()
            .buffer
            .attr_runs_in(0, 5)
            .into_iter()
            .filter(|(_, _, bold, _)| *bold)
            .map(|(s, e, _, _)| (s, e))
            .collect();
        assert_eq!(bolds, vec![(2, 5)]);

        // Alt-E italicizes the same range on top; Alt-B again unbolds.
        app.handle_key_event(key(KeyCode::Char('e'), KeyModifiers::ALT));
        app.handle_key_event(key(KeyCode::Char('b'), KeyModifiers::ALT));
        let runs = app.state.editor().buffer.attr_runs_in(0, 5);
        assert_eq!(runs, vec![(0, 2, false, false), (2, 5, false, true)]);
    }

    #[test]
    fn test_repeated_up_keys_never_move_the_cursor_down() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state
            .add_draft(crate::editor_buffer::EditorBuffer::new(), None);
        app.state.ui.layout.editor = ordet_tui_framework::Rect::new(0, 0, 80, 24);

        let text = (0..500)
            .map(|i| format!("line {i:03} with some words to wrap around"))
            .collect::<Vec<_>>()
            .join("\n");
        app.state.editor_mut().buffer = crate::editor_buffer::EditorBuffer::from_text(&text);
        app.state.editor_mut().buffer.move_cursor_to_end();

        let key_up = KeyEvent {
            code: KeyCode::Up,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        let mut previous = app.state.editor().buffer.cursor();
        for _ in 0..600 {
            app.handle_key_event(key_up);
            let cursor = app.state.editor().buffer.cursor();
            assert!(
                cursor <= previous,
                "cursor snapped down from {previous} to {cursor}"
            );
            previous = cursor;
        }
    }

    #[test]
    fn test_block_quote_command_marks_the_current_line() {
        use crate::commands::Command;
        use ordet_tui_framework::UiEvent;

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state
            .add_draft(crate::editor_buffer::EditorBuffer::new(), None);

        for ch in "wise words".chars() {
            app.state.editor_mut().buffer.insert_text(&ch.to_string());
        }
        app.dispatch(UiEvent::Command(Command::BlockQuote));
        assert_eq!(app.state.editor().buffer.plain_text(), "> wise words");
        app.dispatch(UiEvent::Command(Command::BlockBody));
        assert_eq!(app.state.editor().buffer.plain_text(), "wise words");
    }

    #[test]
    fn test_welcome_opening_menu_letters() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        assert!(app.state.show_welcome_screen());

        let key = |code: KeyCode| KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };

        // D opens a draft: the scratch is replaced and the
        // welcome screen leaves at once.
        app.handle_key_event(key(KeyCode::Char('d')));
        assert_eq!(app.state.drafts.len(), 1);
        assert!(!app.state.show_welcome_screen());
    }

    #[test]
    fn test_welcome_o_hotkey_selects_open_and_browses() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        assert!(app.state.show_welcome_screen());

        let key = |code: KeyCode| KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };

        // O opens the browser and moves the highlight along with it.
        app.handle_key_event(key(KeyCode::Char('o')));
        assert!(app.state.ui.file_browser.is_some());
        assert_eq!(
            app.state.ui.welcome_selected,
            crate::state::WelcomeOption::OpenFile
        );
        assert!(app.state.show_welcome_screen());

        // H then jumps to help: highlight follows, the browser dismisses
        // so the shortcuts rail actually shows.
        app.handle_key_event(key(KeyCode::Char('h')));
        assert_eq!(
            app.state.ui.welcome_selected,
            crate::state::WelcomeOption::Help
        );
        assert!(app.state.welcome_help_active());
        assert!(app.state.ui.file_browser.is_none());
        assert!(app.state.show_welcome_screen());
    }

    #[test]
    fn test_welcome_browser_search_mode_keeps_typing() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        assert!(app.state.show_welcome_screen());

        let key = |code: KeyCode| KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };

        // Open the browser rail, then Tab into search mode.
        app.handle_key_event(key(KeyCode::Char('o')));
        assert!(app.state.ui.file_browser.is_some());
        app.handle_key_event(key(KeyCode::Tab));
        assert!(matches!(
            app.state
                .ui
                .file_browser
                .as_ref()
                .map(|browser| browser.mode),
            Some(crate::file_browser::FileBrowserMode::Search)
        ));

        // Typing now filters instead of firing menu hotkeys.
        app.handle_key_event(key(KeyCode::Char('h')));
        let browser = app.state.ui.file_browser.as_ref().expect("browser stays");
        assert!(browser.search.contains('h'));
        assert_eq!(
            app.state.ui.welcome_selected,
            crate::state::WelcomeOption::OpenFile
        );
    }

    #[test]
    fn test_welcome_h_hotkey_opens_help_without_enter() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        assert!(app.state.show_welcome_screen());

        app.handle_key_event(KeyEvent {
            code: KeyCode::Char('h'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        });
        assert_eq!(
            app.state.ui.welcome_selected,
            crate::state::WelcomeOption::Help
        );
        assert!(app.state.welcome_help_active());
    }

    #[test]
    fn test_welcome_x_quits() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        assert!(app.state.show_welcome_screen());

        app.handle_key_event(KeyEvent {
            code: KeyCode::Char('x'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        });
        assert!(app.state.ui.should_quit);
    }

    fn autosave_temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ordet-autosave-test-{}-{name}", std::process::id()))
    }

    #[test]
    fn test_flush_writes_dirty_drafts_to_disk() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        let path = autosave_temp_path("flush.txt");
        let _ = std::fs::remove_file(&path);
        app.state.add_draft(
            crate::editor_buffer::EditorBuffer::new(),
            Some(path.clone()),
        );
        // Type a word: the draft is dirty and the file does not exist yet.
        app.handle_key_event(KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        });
        assert!(app.state.editor().is_dirty());
        assert!(!path.exists());

        app.flush_dirty_drafts().expect("flush writes");
        assert!(!app.state.editor().is_dirty());
        assert_eq!(std::fs::read_to_string(&path).expect("saved file"), "a");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_quit_saves_and_leaves_without_asking() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        let path = autosave_temp_path("quit.txt");
        let _ = std::fs::remove_file(&path);
        app.state.add_draft(
            crate::editor_buffer::EditorBuffer::new(),
            Some(path.clone()),
        );
        app.handle_key_event(KeyEvent {
            code: KeyCode::Char('b'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        });

        app.request_quit();
        assert!(app.state.ui.should_quit);
        assert!(app.state.ui.quit_confirmation.is_none());
        assert_eq!(std::fs::read_to_string(&path).expect("saved file"), "b");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_quit_asks_only_when_the_write_fails() {
        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        // A directory is not writable as a file: the flush must fail.
        let dir =
            std::env::temp_dir().join(format!("ordet-autosave-test-{}-dir", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        app.state
            .add_draft(crate::editor_buffer::EditorBuffer::new(), Some(dir.clone()));
        app.state.editor_mut().buffer.insert_text("x");
        app.state.editor_mut().mark_dirty();

        app.request_quit();
        assert!(!app.state.ui.should_quit);
        assert!(app.state.ui.quit_confirmation.is_some());
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn test_autosave_tick_saves_dirty_drafts_quietly() {
        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        // Clean scratch first: nothing to do, no task spawns.
        app.next_autosave_at = std::time::Instant::now() - std::time::Duration::from_secs(10);
        app.maybe_autosave();
        assert!(!app.state.editor().pending_save);

        let path = autosave_temp_path("tick.txt");
        let _ = std::fs::remove_file(&path);
        app.state.add_draft(
            crate::editor_buffer::EditorBuffer::new(),
            Some(path.clone()),
        );
        app.state.editor_mut().buffer.insert_text("z");
        app.state.editor_mut().mark_dirty();

        // The tick schedules a background save...
        app.next_autosave_at = std::time::Instant::now() - std::time::Duration::from_secs(10);
        app.maybe_autosave();
        assert!(app.state.editor().pending_save);

        // ...whose completion lands quietly: no status chatter, just a
        // clean draft and a current file.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.state.editor().is_dirty() {
            assert!(
                std::time::Instant::now() < deadline,
                "autosave task never completed"
            );
            while let Some(task) = app.tasks.try_recv() {
                app.handle_task_result(task);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn test_status_paths_show_parent_and_file_only() {
        use std::path::Path;

        assert_eq!(
            OrdetApp::display_path_tail(Path::new("/Users/leon/Documents/Ordet-final/i.ord")),
            "Ordet-final/i.ord"
        );
        assert_eq!(
            OrdetApp::display_path_tail(Path::new("/notes.txt")),
            "notes.txt"
        );
        assert_eq!(OrdetApp::display_path_tail(Path::new("/")), "/");
    }

    #[test]
    fn test_status_messages_expire_back_to_the_default_hint() {
        use ordet_tui_framework::UiEvent;

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state
            .add_draft(crate::editor_buffer::EditorBuffer::new(), None);

        // Transient messages arm the timer.
        app.state
            .set_status("Opened Ordet-final/i.ord.".to_string());
        assert!(app.state.ui.status_expires_at.is_some());

        // Persistent ones never do.
        app.state
            .set_status_persistent("A persistent note that never expires.".to_string());
        assert!(app.state.ui.status_expires_at.is_none());

        // A past deadline clears the message on tick: an idle editor keeps
        // the bar empty and the cluster alone carries the state.
        app.state
            .set_status("Opened Ordet-final/i.ord.".to_string());
        app.state.ui.status_expires_at =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(10));
        app.dispatch(UiEvent::Tick);
        assert!(app.state.ui.status_expires_at.is_none());
        assert!(
            app.state.ui.status.is_empty(),
            "unexpected status: {}",
            app.state.ui.status
        );
    }

    #[test]
    fn routine_status_is_silent_and_keeps_persistent_prompts() {
        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state
            .add_draft(crate::editor_buffer::EditorBuffer::new(), None);

        // A routine success clears an old transient note instead of showing
        // another one.
        app.state.set_status("No matches.".to_string());
        app.state.set_status_quiet("Saved.".to_string());
        assert!(app.state.ui.status.is_empty());
        assert!(app.state.ui.status_expires_at.is_none());

        // Persistent confirmations and modal hints survive routine actions.
        app.state
            .set_status_persistent("Delete this page? Y/N".to_string());
        app.state.set_status_quiet("Saved.".to_string());
        assert_eq!(app.state.ui.status, "Delete this page? Y/N");
        assert!(app.state.ui.status_expires_at.is_none());
    }

    #[test]
    fn test_wheel_scroll_stays_where_put_until_cursor_moves() {
        use ordet_tui_framework::Point;

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state
            .add_draft(crate::editor_buffer::EditorBuffer::new(), None);
        app.state.ui.layout.editor = ordet_tui_framework::Rect::new(0, 1, 80, 20);

        let text = (0..100)
            .map(|i| format!("line {i:03}"))
            .collect::<Vec<_>>()
            .join("\n");
        app.state.editor_mut().buffer = crate::editor_buffer::EditorBuffer::from_text(&text);
        assert_eq!(app.state.editor().buffer.cursor(), 0);

        // Trackpad fling down: the view moves and stays moved.
        for _ in 0..3 {
            app.mouse_scroll(Point::new(5, 5), false);
        }
        assert_eq!(app.state.editor().scroll, 9);
        app.state.editor_mut().ensure_cursor_visible(70, 20);
        assert_eq!(
            app.state.editor().scroll,
            9,
            "free scroll must survive the per-frame ensure"
        );

        // Moving the cursor re-pins the view to it.
        app.state.editor_mut().buffer.move_down(70, false);
        app.state.editor_mut().ensure_cursor_visible(70, 20);
        assert!(app.state.editor().free_scroll_at.is_none());
        assert_eq!(app.state.editor().scroll, 1);
    }

    #[test]
    fn about_menu_opens_a_sheet_and_never_writes_a_status_note() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.set_status("stale note".to_string());

        // Help ▸ About Ordet: after Keyboard help in the last menu.
        app.run_menu_item(7, 1);
        assert!(app.state.ui.about_open);
        assert!(
            app.state.ui.status.is_empty(),
            "About must not announce itself in the status bar: {}",
            app.state.ui.status
        );
        assert!(app.state.ui.status_expires_at.is_none());

        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.state.ui.about_open);
    }

    #[test]
    fn test_notes_wheel_scroll_stays_until_cursor_moves() {
        use ordet_tui_framework::Point;

        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state
            .add_draft(crate::editor_buffer::EditorBuffer::new(), None);
        app.state
            .update_layout(ordet_tui_framework::Rect::new(0, 0, 120, 30));
        app.state.panels.open(PanelKind::Notes);

        let text = (0..100)
            .map(|i| format!("note {i:03}"))
            .collect::<Vec<_>>()
            .join("\n");
        app.state.notes_mut().buffer = crate::editor_buffer::EditorBuffer::from_text(&text);

        app.mouse_scroll(Point::new(85, 5), false);
        assert_eq!(app.state.notes().scroll, 3);
        app.state.notes_mut().ensure_cursor_visible(30, 20);
        assert_eq!(app.state.notes().scroll, 3);

        app.state.notes_mut().buffer.move_down(30, false);
        app.state.notes_mut().ensure_cursor_visible(30, 20);
        assert!(app.state.notes().free_scroll_at.is_none());
        assert_eq!(app.state.notes().scroll, 1);
    }
}
