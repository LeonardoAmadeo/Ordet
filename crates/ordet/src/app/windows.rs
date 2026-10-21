use super::*;

impl OrdetApp {
    pub(super) fn create_new_draft(&mut self) {
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

    pub(super) fn switch_to_draft(&mut self, index: usize) {
        self.state.switch_to_draft(index);
        let title = self.state.editor().title();
        self.state.set_status_quiet(format!("Switched to {title}."));
    }

    /// F6: the next listed page, wrapping at the end.
    /// Every visible window keeps its own cursor on screen. A split hands
    /// each draft a different wrap width, so the pass runs per window.
    pub(super) fn refresh_window_views(&mut self) {
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
    pub(super) fn refresh_layout(&mut self) {
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
    pub(super) fn cycle_window(&mut self, delta: isize) -> bool {
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
    pub(super) fn next_window(&mut self) {
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
    pub(super) fn close_window(&mut self) {
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
    pub(super) fn move_window_to_slot(&mut self, slot: usize) {
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
    pub(super) fn focus_window_at(&mut self, point: Point) {
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

    pub(super) fn next_page(&mut self) {
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
    pub(super) fn previous_page(&mut self) {
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
}
