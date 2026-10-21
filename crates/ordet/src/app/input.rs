use super::*;

impl OrdetApp {
    /// Returns a resize the caller must forward to the terminal backend, if
    /// the event carried one. Everything else is applied in place.
    pub(super) fn handle_terminal_event(
        &mut self,
        event: Event,
    ) -> Option<ordet_tui_framework::Rect> {
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

    pub(super) fn handle_mouse_event(&mut self, mouse: MouseEvent) {
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
    pub(super) fn full_area(&self) -> Rect {
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
    pub(super) fn mouse_hover(&mut self, point: Point) {
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
    pub(super) fn handle_picker_mouse_press(&mut self, point: Point) -> bool {
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
    pub(super) fn handle_menu_mouse_press(&mut self, point: Point) -> bool {
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
    pub(super) fn mouse_release(&mut self, point: Point) {
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
    pub(super) fn text_position_at(
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

    pub(super) fn editor_text_area(&self) -> Rect {
        window_text_area(&self.state, self.state.active_editor_area())
    }

    pub(super) fn mouse_press(&mut self, point: Point) {
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
    pub(super) fn panel_mouse_press(&mut self, point: Point) {
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

    pub(super) fn mouse_drag(&mut self, point: Point) {
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

    pub(super) fn mouse_scroll(&mut self, point: Point, up: bool) {
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

    pub(super) fn handle_key_event(&mut self, key: KeyEvent) {
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
}
