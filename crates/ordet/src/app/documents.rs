use super::*;

impl OrdetApp {
    pub(super) fn open_save_path_prompt(&mut self) {
        let initial = self.state.editor().default_save_path();
        let mut prompt = PromptState::save_path(initial.to_str());
        prompt.hint = "Enter a path like ./draft.ord, ./draft.txt, or ./draft.md".to_string();
        self.state.ui.prompt = Some(prompt);
    }

    pub(super) fn open_file_prompt(&mut self) {
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

    pub(super) fn browser_rename_prompt(&self, browser: &FileBrowserState) -> Option<PromptState> {
        let entry = browser.selected_entry()?;
        Some(PromptState::rename_browser_entry(&entry.path, entry.is_dir))
    }

    pub(super) fn browser_copy_prompt(&self, browser: &FileBrowserState) -> Option<PromptState> {
        let entry = browser.selected_entry()?;
        let suggested = suggest_copy_destination(&entry.path);
        Some(PromptState::copy_browser_entry(
            &entry.path,
            entry.is_dir,
            &suggested.display().to_string(),
        ))
    }

    pub(super) fn rename_browser_entry(
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

    pub(super) fn copy_browser_entry(
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

    pub(super) fn refresh_browser_after_path_change(
        &mut self,
        target: &Path,
    ) -> Result<(), String> {
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
    pub(super) fn trash_requested(&mut self, confirm: bool) {
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

    pub(super) fn trash_confirmed(&mut self, path: PathBuf) {
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
    pub(super) fn unsaved_draft_target(&self) -> Option<usize> {
        if self.state.editor().file_path.is_none() {
            Some(self.state.active_draft)
        } else {
            None
        }
    }

    pub(super) fn close_unsaved_draft(&mut self, index: usize, confirm: bool) {
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

    pub(super) fn trash_target_from_context(&self) -> Result<PathBuf, String> {
        self.state
            .editor()
            .file_path
            .clone()
            .ok_or_else(|| "The current draft has no file to move to Trash yet.".to_string())
    }

    pub(super) fn toggle_panel(&mut self, kind: PanelKind) {
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

    pub(super) fn apply_message(&mut self, message: AppMessage) {
        match message {
            AppMessage::SaveCompleted {
                draft_id,
                path,
                revision,
            } => {
                // Resolve by id: the page may have moved (or been closed)
                // since the save was spawned.
                let current_revision = self
                    .state
                    .draft_index_by_id(draft_id)
                    .map(|index| self.state.drafts[index].editor.revision);
                if let Some(index) = self.state.draft_index_by_id(draft_id) {
                    self.state.drafts[index]
                        .editor
                        .mark_saved(revision, path.clone());
                }
                self.state.remember_recent_path(path.clone());
                // A save of an older snapshot must not close the page or quit:
                // the newer edits are still unsaved. A later save owns those
                // intents.
                if current_revision.is_none_or(|current| revision < current) {
                    return;
                }
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

    fn save_gate(&mut self, draft_id: DraftId) -> Arc<SaveGate> {
        Arc::clone(
            self.save_gates
                .entry(draft_id)
                .or_insert_with(|| Arc::new(SaveGate::new())),
        )
    }

    pub(super) fn spawn_save(&mut self) {
        let path = self.state.editor().default_save_path();
        self.spawn_save_to(path);
    }

    /// Last two path components for status lines: `Ordet-final/i.ord`
    /// instead of the full path. Never touches the stored path.
    pub(super) fn display_path_tail(path: &Path) -> String {
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
    pub(super) fn write_draft_file(
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

    pub(super) fn spawn_save_to(&mut self, path: PathBuf) {
        // Normalize first so the save target, the recorded draft path, and
        // the history key all agree (macOS temp dirs symlink /var to
        // /private/var, which otherwise keys the same file twice).
        let path = normalize_document_path(path);
        let draft_id = self.state.active_draft_id();
        let gate = self.save_gate(draft_id);
        let generation = gate.next_generation();
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
        self.state.history.record(
            &history_key_for(&path),
            &plain_text,
            snapshot.alignments(),
            snapshot.attrs(),
            unix_now(),
        );
        let history = self.state.history.clone();
        let history_gate = Arc::clone(&self.history_gate);
        let history_generation = history_gate.next_generation();
        self.tasks
            .spawn(Some(TaskKey::from("history")), move |token| {
                if token.is_cancelled() {
                    return Err(TaskError::new("history write cancelled"));
                }
                history_gate.run(history_generation, || {
                    save_history(&history).map_err(TaskError::new)
                })?;
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

        let gate_for_worker = Arc::clone(&gate);
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
                gate_for_worker.run(generation, || {
                    Self::write_draft_file(
                        &path,
                        &plain_text,
                        &content_html,
                        &title,
                        &doc_status,
                        doc_auto_title,
                        &doc_label_color,
                    )
                    .map_err(TaskError::new)
                })?;

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
    pub(super) fn spawn_save_draft(&mut self, index: usize, path: PathBuf, quiet: bool) {
        let path = normalize_document_path(path);
        let Some(draft_id) = self.state.drafts.get(index).map(|draft| draft.id) else {
            return;
        };
        let gate = self.save_gate(draft_id);
        let generation = gate.next_generation();
        let Some(draft) = self.state.drafts.get(index) else {
            return;
        };
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
        self.state.history.record(
            &history_key_for(&path),
            &plain_text,
            snapshot.alignments(),
            snapshot.attrs(),
            unix_now(),
        );
        let history = self.state.history.clone();
        let history_gate = Arc::clone(&self.history_gate);
        let history_generation = history_gate.next_generation();
        self.tasks
            .spawn(Some(TaskKey::from("history")), move |token| {
                if token.is_cancelled() {
                    return Err(TaskError::new("history write cancelled"));
                }
                history_gate.run(history_generation, || {
                    save_history(&history).map_err(TaskError::new)
                })?;
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

        let gate_for_worker = Arc::clone(&gate);
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
                gate_for_worker.run(generation, || {
                    Self::write_draft_file(
                        &path,
                        &plain_text,
                        &content_html,
                        &title,
                        &doc_status,
                        doc_auto_title,
                        &doc_label_color,
                    )
                    .map_err(TaskError::new)
                })?;

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

    pub(super) fn spawn_trash(&mut self, path: PathBuf) {
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

    pub(super) fn open_or_focus_path(&mut self, path: PathBuf) {
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

    pub(super) fn spawn_open(&mut self, path: PathBuf) {
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
}
