use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command as ProcessCommand;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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

mod cli;
mod documents;
pub(crate) mod helpers;
mod input;
mod prompts;
mod settings;
mod windows;

#[cfg(test)]
mod tests;

pub(crate) use cli::run;

use helpers::*;

/// Housekeeping cadence for time-derived UI (clock, status expiry) and the
/// periodic disk work (recovery, autosave). This is not a redraw timer: a
/// tick only asks for a frame when one of those visible values changed.
const TICK_INTERVAL: Duration = Duration::from_millis(250);

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
    save_gates: HashMap<DraftId, Arc<SaveGate>>,
    history_gate: Arc<SaveGate>,
    recovery_gate: Arc<SaveGate>,
}

pub(super) struct SaveGate {
    generation: AtomicU64,
    write_lock: Mutex<()>,
}

impl SaveGate {
    pub(super) fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            write_lock: Mutex::new(()),
        }
    }

    pub(super) fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub(super) fn run<T>(
        &self,
        generation: u64,
        write: impl FnOnce() -> Result<T, TaskError>,
    ) -> Result<T, TaskError> {
        let _guard = self
            .write_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.generation.load(Ordering::SeqCst) != generation {
            return Err(TaskError::new("save superseded"));
        }
        write()
    }
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
            let mut restored_metadata = Vec::new();
            for draft in recovered {
                let mut buffer = if draft.content_html.is_empty() {
                    EditorBuffer::from_text(&draft.text)
                } else {
                    EditorBuffer::from_rich_text(&draft.text, &draft.content_html)
                };
                buffer.move_cursor_to_end();
                restored.push((buffer, draft.path));
                let defaults = DocumentMetadata::default();
                restored_metadata.push(DocumentMetadata {
                    status: draft.status.unwrap_or(defaults.status),
                    auto_title: draft.auto_title.unwrap_or(defaults.auto_title),
                    label_color: draft.label_color.unwrap_or(defaults.label_color),
                });
            }
            if drafts.len() == 1 && is_empty_scratch(&drafts) {
                drafts.clear();
            }
            recovered_start = drafts.len();
            drafts.extend(restored);
            draft_metadata.extend(restored_metadata);
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
            save_gates: HashMap::new(),
            history_gate: Arc::new(SaveGate::new()),
            recovery_gate: Arc::new(SaveGate::new()),
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
                let generation = self.recovery_gate.next_generation();
                let cleared = self
                    .recovery_gate
                    .run(generation, || clear_recovery().map_err(TaskError::new));
                if let Err(error) = cleared {
                    self.state
                        .set_status(format!("Recovery file could not be cleared: {error}"));
                }
            }
            self.recovered_at_startup = 0;
            return;
        }

        let generation = self.recovery_gate.next_generation();
        let gate = Arc::clone(&self.recovery_gate);
        self.tasks
            .spawn(Some(TaskKey::from("recovery")), move |token| {
                if token.is_cancelled() {
                    return Err(TaskError::new("recovery write cancelled"));
                }
                gate.run(generation, || save_recovery(&file).map_err(TaskError::new))?;
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
                    draft.editor.buffer.alignments().to_vec(),
                    draft.editor.buffer.attrs().to_vec(),
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
                alignments,
                attrs,
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
                    self.state.history.record(
                        &history_key_for(&path),
                        &plain_text,
                        &alignments,
                        &attrs,
                        unix_now(),
                    );
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
                    draft.editor.buffer.alignments().to_vec(),
                    draft.editor.buffer.attrs().to_vec(),
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
                alignments,
                attrs,
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
                    self.state.history.record(
                        &history_key_for(&path),
                        &plain_text,
                        &alignments,
                        &attrs,
                        unix_now(),
                    );
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

    /// Replace the buffer with the selected snapshot in one undo record,
    /// including paragraph alignment and inline styles.
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
        let when = relative_time(unix_now(), revision.timestamp);
        let text = revision.text.clone();
        let alignments = revision.alignments.clone();
        let attrs = revision.attrs.clone();
        let editor = self.state.editor_mut();
        if !editor
            .buffer
            .replace_with_rich_text(&text, &alignments, &attrs)
        {
            self.state
                .set_status_quiet("That revision already matches the draft.".to_string());
            return;
        }
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

    fn handle_task_result(&mut self, task: TaskResult<AppMessage>) {
        match task.result {
            Ok(message) => self.dispatch(UiEvent::Message(message)),
            Err(error)
                if error.message == "save cancelled"
                    || error.message == "save superseded"
                    || error.message == "open cancelled"
                    || error.message == "trash cancelled"
                    || error.message == "history write cancelled" => {}
            Err(error) => {
                if let Some(draft_id) = draft_id_from_task_key(task.key.as_ref()) {
                    if let Some(index) = self.state.draft_index_by_id(draft_id) {
                        self.state.drafts[index].editor.pending_save = false;
                    }
                    self.state.ui.quit_after_save = false;
                    self.pending_after_save = CloseIntent::None;
                } else if task.key.as_ref().is_some_and(|key| key.0 == "open") {
                    self.state.ui.pending_open = false;
                }
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
