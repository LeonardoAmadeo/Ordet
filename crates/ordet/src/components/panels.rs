use super::*;

pub(super) struct PanelView;

/// Room a panel keeps between itself and the editor body.
const PANEL_MARGIN: u16 = 4;
/// Narrowest a panel shrinks to: the title row still carries its name and
/// `Esc closes`, and one column of text stays readable.
const PANEL_MIN_WIDTH: u16 = 28;
/// Widest a prose panel (notes, help, revisions) grows. Short
/// content used to stretch a box across the terminal; a card this size keeps
/// the text together rather than leaving a sheet with an empty right half.
const PANEL_MAX_WIDTH: u16 = 72;
/// Widest a pages/workspace list grows.
const PANEL_LIST_MAX_WIDTH: u16 = 56;
/// Outermost rows a panel may take. Taller content scrolls inside the box.
const PANEL_MAX_HEIGHT: u16 = 22;
/// The two border rows. The name rides in the top one, so content starts on
/// the row below it and there is no title row inside.
const PANEL_CHROME_ROWS: u16 = 2;

/// Outer rect of the open panel: a content-sized box hung off the top-right
/// of the editor body, straight under the menu bar. This lives here, not in
/// `state.rs`, because only the panel code knows how many rows each surface
/// paints; `state::panel_rect` and the mouse hit test both come through here.
pub(crate) fn panel_rect(model: &AppState) -> Rect {
    let Some(kind) = model.panels.panel else {
        return Rect::new(0, 0, 0, 0);
    };
    // The editor rect, not `body_area`: panels are overlays over the text
    // area and the welcome screen paints no panels at all, so a first-run
    // screen must not shift the geometry.
    let body = model.ui.layout.editor;
    if body.width < 8 || body.height < 3 {
        return Rect::new(body.x, body.y, 0, 0);
    }

    let width = match kind {
        PanelKind::Notes => notes_panel_width(model),
        PanelKind::Help => help_panel_width(),
        // Counters change on every keystroke; the box keeps one width so it
        // never twitches while you type, and 32 columns is what they need.
        PanelKind::Stats => 32,
        PanelKind::Settings => SETTINGS_PANEL_WIDTH,
        PanelKind::Revisions => revisions_panel_width(),
        PanelKind::Pages => pages_panel_width(model),
        PanelKind::Workspace => workspace_panel_width(model),
    }
    .min(PANEL_MAX_WIDTH)
    .min(body.width.saturating_sub(PANEL_MARGIN))
    .max(PANEL_MIN_WIDTH.min(body.width));

    // Rows count what the renderer paints *at this width*: wrapping folds
    // long settings values, help descriptions and definitions into more
    // rows, so the card grows with them and never reserves rows it does not
    // use.
    let content_width = width.saturating_sub(2);
    let rows = match kind {
        PanelKind::Notes => 18,
        PanelKind::Stats => stats_panel_rows(model),
        PanelKind::Settings => settings_panel_rows(model, content_width),
        PanelKind::Revisions => revisions_panel_rows(model, content_width as usize),
        PanelKind::Help => help_panel_rows() as u16,
        PanelKind::Pages => model.visible_drafts().len().max(1) as u16,
        PanelKind::Workspace => model.workspace_rows().len().max(1) as u16,
    };
    let height = rows
        .saturating_add(PANEL_CHROME_ROWS)
        .min(PANEL_MAX_HEIGHT)
        .min(body.height.saturating_sub(2))
        .max(PANEL_CHROME_ROWS.min(body.height));
    // Hung off the top-right of the page, straight under the menu bar, so the
    // box never covers the text as it is written and the card reads as a
    // margin note. The text stays visible around it.
    Rect::new(body.right().saturating_sub(width), body.y, width, height)
}

/// Rows the stats panel paints: the seven counters plus either the
/// `Target off` line or, with a word target set, target, remaining/overflow,
/// progress and the bar.
pub(super) fn stats_panel_rows(model: &AppState) -> u16 {
    if model.word_target_progress().is_some() {
        11
    } else {
        8
    }
}

/// Rows the settings list paints at this width: one per row, one per section
/// heading, and one per wrapped value line, the same walk the renderer does.
pub(super) fn settings_panel_rows(model: &AppState, content_width: u16) -> u16 {
    let value_width = (content_width as usize)
        .saturating_sub(SETTINGS_LABEL_WIDTH + 1)
        .max(1);
    let mut rows = 0u16;
    let mut section: Option<&'static str> = None;
    for row in settings_rows(model) {
        if section != Some(row.section) {
            rows = rows.saturating_add(1);
            section = Some(row.section);
        }
        rows = rows.saturating_add(split_to_width(&row.value, value_width).len() as u16);
    }
    rows
}

/// Rows the revisions panel paints at this width: one per snapshot, the
/// separator, the selected diff (wrapped) and the closing hint. Counting
/// stops at the panel's own row cap, a full-size diff never costs more than
/// the rows the card can show.
pub(super) fn revisions_panel_rows(model: &AppState, width: usize) -> u16 {
    let cap = (PANEL_MAX_HEIGHT - PANEL_CHROME_ROWS) as usize;
    let revisions = revisions_for_active_file(model);
    if revisions.is_empty() {
        return 2;
    }
    let selected = model
        .panels
        .revision_selected
        .min(revisions.len().saturating_sub(1));
    let current = model.editor().buffer.plain_text();
    let mut rows = revisions.len() + 2;
    if rows >= cap {
        return cap as u16;
    }
    for diff in diff_lines(&revisions[selected].text, &current, 2) {
        rows += wrap_text(&format!("{} {}", diff.tag, diff.text), width.max(1)).len();
        if rows >= cap {
            return cap as u16;
        }
    }
    rows as u16
}

/// Rows the Help card paints: the tab bar, a breathing row, one row per
/// action, filler down to the in-modal key footer pinned at the bottom. No
/// prefix dump and no second column of section titles.
pub(super) fn help_panel_rows() -> usize {
    let entries = EDITOR_HELP_SECTIONS
        .iter()
        .map(|section| section.entries.len())
        .max()
        .unwrap_or(0);
    entries + 4
}

/// Width of the Pages card: the widest `n. label` row plus its border, or
/// the empty-state line, whichever is longer. Never wider than the cap.
pub(super) fn pages_panel_width(model: &AppState) -> u16 {
    let widest = model
        .page_rows()
        .iter()
        .map(|row| {
            let bullet = if row.unsaved { 2 } else { 0 };
            1 + row.number.to_string().len()
                + 2
                + UnicodeWidthStr::width(row.label.as_str())
                + bullet
        })
        .max()
        .unwrap_or(0);
    // " No pages yet. ⌃N makes one." keeps its words on the card.
    (widest + 2).clamp(30, PANEL_LIST_MAX_WIDTH as usize) as u16
}

/// Width of the Workspace card: the widest folder/file row plus its border,
/// or the empty-state line. Never wider than the cap.
pub(super) fn workspace_panel_width(model: &AppState) -> u16 {
    let widest = model
        .workspace_rows()
        .iter()
        .map(|row| {
            let counter = if row.unsaved > 0 {
                2 + row.unsaved.to_string().len() + 1 + row.file_count.to_string().len()
            } else {
                0
            };
            1 + UnicodeWidthStr::width(row.label.as_str()) + counter
        })
        .max()
        .unwrap_or(0);
    // " No folders tracked. ⌃⇧W adds one." keeps its words on the card.
    (widest + 2).clamp(36, PANEL_LIST_MAX_WIDTH as usize) as u16
}

/// Border strokes around a panel's content: one `│` on each side.
pub(super) const PANEL_BORDER_COLUMNS: usize = 2;

/// Width of the Help card: the widest of the tab bar, the pinned footer, and
/// an action/shortcut row. The shortcut column is right-aligned inside this
/// width, so the card does not resize when switching tabs.
pub(super) fn help_panel_width() -> u16 {
    let table = EDITOR_HELP_SECTIONS
        .iter()
        .map(|section| {
            let action_width = section
                .entries
                .iter()
                .map(|entry| UnicodeWidthStr::width(entry.description))
                .max()
                .unwrap_or(0);
            let key_width = section
                .entries
                .iter()
                .map(|entry| UnicodeWidthStr::width(entry.keys))
                .max()
                .unwrap_or(0);
            action_width + 2 + key_width
        })
        .max()
        .unwrap_or(0);
    let content = help_tab_bar_width().max(help_footer_width()).max(table);
    (content + PANEL_BORDER_COLUMNS) as u16
}

/// Width of the tab strip. Every unselected tab carries one space on each
/// side; the selected one adds brackets.
pub(super) fn help_tab_bar_width() -> usize {
    HelpTab::ALL
        .iter()
        .map(|tab| UnicodeWidthStr::width(tab.title()) + 2)
        .sum::<usize>()
        + 2
}

const HELP_FOOTER_TEXT: &str = "[Tab / ←→]: Switch Section [Esc]: Close.";

pub(super) fn help_footer_width() -> usize {
    UnicodeWidthStr::width(HELP_FOOTER_TEXT)
}

/// Width of the Revisions card: its empty-state sentence is the widest fixed
/// line it prints, so the card is sized to it. Snapshots, the hint and the
/// diff all wrap inside, and the card never twitches between saves.
pub(super) fn revisions_panel_width() -> u16 {
    (UnicodeWidthStr::width(REVISIONS_EMPTY_HINT) + PANEL_BORDER_COLUMNS) as u16
}

/// Width of the Notes card: the widest line in the notes buffer, floored so
/// an empty page still opens a writing column instead of a title-sized
/// sliver. A longer line widens the card until the cap.
pub(super) fn notes_panel_width(model: &AppState) -> u16 {
    /// Narrowest writing column the card keeps with nothing typed yet.
    const MIN_CONTENT_WIDTH: usize = 38;
    let widest = model
        .notes()
        .buffer
        .plain_text()
        .lines()
        .map(UnicodeWidthStr::width)
        .max()
        .unwrap_or(0);
    (widest.max(MIN_CONTENT_WIDTH) + PANEL_BORDER_COLUMNS) as u16
}

impl PanelView {
    pub(super) fn render(&self, model: &AppState, frame: &mut Frame<'_>, theme: ThemePalette) {
        let Some(kind) = model.panels.panel else {
            return;
        };
        let outer = panel_rect(model);
        if outer.width < 3 || outer.height < 3 {
            return;
        }
        let inner = draw_panel_frame(frame, outer, kind.title(), theme);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        match kind {
            PanelKind::Notes => {
                let content = panel_content_rect(model);
                render_notes_buffer(
                    frame,
                    content,
                    model.notes(),
                    theme,
                    true,
                    model.ui.show_line_numbers,
                );
            }
            PanelKind::Stats => render_stats_panel(frame, panel_content_rect(model), model, theme),
            PanelKind::Settings => {
                render_settings_panel(frame, panel_content_rect(model), model, theme)
            }
            PanelKind::Revisions => {
                render_revisions_panel(frame, panel_content_rect(model), model, theme)
            }
            PanelKind::Help => render_help_panel(frame, panel_content_rect(model), model, theme),
            PanelKind::Pages => render_pages_panel(frame, model, theme),
            PanelKind::Workspace => render_workspace_panel(frame, model, theme),
        }
    }
}

/// Panel chrome: a single-line box with the name embedded in the top edge
/// (`┌───── Help ─────┐`), centred there so the card reads as titled rather
/// than captioned from the corner. The first inner row is already content,
/// whether stats, menu entries, or a table. There is no inner header row and
/// no corner hint: the keys and the way out live in the status line.
pub(super) fn draw_panel_frame(
    frame: &mut Frame<'_>,
    outer: Rect,
    title: &str,
    theme: ThemePalette,
) -> Rect {
    let fill = Style::default().bg(theme.panel_alt_bg);
    let border = Style::default().fg(theme.border).bg(theme.panel_alt_bg);
    let inner = frame.draw_border(outer, None, fill, border);
    draw_centered_border_title(frame, outer, title, theme.accent, theme.panel_alt_bg);
    inner
}

/// Paint a border title centred in the top edge, with a clean space on each
/// side so no extra border stroke is quoted into the name:
/// `┌───── Help ─────┐`.
pub(super) fn draw_centered_border_title(
    frame: &mut Frame<'_>,
    outer: Rect,
    title: &str,
    fg: Color,
    background: Color,
) {
    if outer.width <= 5 {
        return;
    }
    let label = format!(" {title} ");
    let row = Rect::new(
        outer.x.saturating_add(1),
        outer.y,
        outer.width.saturating_sub(2),
        1,
    );
    frame.draw_lines(
        row,
        &[Line::from(Span::styled(
            label,
            Style::default()
                .fg(fg)
                .bg(background)
                .add_modifier(Modifier::BOLD),
        ))],
        Style::default().bg(background),
        Alignment::Center,
    );
}

/// The rows area of a compact panel: the content below the title row.
pub(crate) fn panel_list_rect(model: &AppState) -> Rect {
    let content = panel_content_rect(model);
    if content.width == 0 || content.height == 0 {
        return Rect::new(0, 0, 0, 0);
    }
    let height = content.height;
    Rect::new(content.x, content.y, content.width, height)
}

/// The scroll offset a compact list draws from: the stored offset clamped so
/// the last row never scrolls past the bottom of the list area.
pub(crate) fn panel_scroll_for(model: &AppState, count: usize, height: usize) -> usize {
    if height == 0 {
        return 0;
    }
    model.panels.scroll.min(count.saturating_sub(height))
}

/// Every list panel shares one scroll offset; after a selection move the
/// offset follows so the highlighted row stays on screen.
pub(crate) fn follow_panel_scroll(model: &mut AppState) {
    let Some(kind) = model.panels.panel else {
        return;
    };
    let count = match kind {
        PanelKind::Pages => model.page_rows().len(),
        PanelKind::Workspace => model.workspace_rows().len(),
        _ => return,
    };
    let selected = match kind {
        PanelKind::Pages => model.panels.pages_selected,
        PanelKind::Workspace => model.panels.workspace_selected,
        _ => return,
    };
    let height = panel_list_rect(model).height as usize;
    if height == 0 || count == 0 {
        return;
    }
    let selected = selected.min(count - 1);
    let scroll = panel_scroll_for(model, count, height);
    let scroll = if selected < scroll {
        selected
    } else if selected >= scroll + height {
        selected + 1 - height
    } else {
        scroll
    };
    model.panels.scroll = scroll;
}

/// One pages row per listed page, numbered like the header's `Pg n/m`.
pub(super) fn render_pages_panel(frame: &mut Frame<'_>, model: &AppState, theme: ThemePalette) {
    let list = panel_list_rect(model);
    if list.width == 0 || list.height == 0 {
        return;
    }
    let background = theme.panel_alt_bg;
    let rows = model.page_rows();
    if rows.is_empty() {
        frame.draw_lines(
            list,
            &[Line::from(Span::styled(
                " No pages yet. ⌃N makes one.",
                Style::default().fg(theme.muted_fg).bg(background),
            ))],
            Style::default().bg(background).fg(theme.text_fg),
            Alignment::Left,
        );
        return;
    }
    let selected = model.panels.pages_selected.min(rows.len() - 1);
    let scroll = panel_scroll_for(model, rows.len(), list.height as usize);
    let lines = rows
        .iter()
        .enumerate()
        .skip(scroll)
        .map(|(index, row)| {
            let bullet = if row.unsaved { " •" } else { "" };
            let label = format!(" {}. {}{}", row.number, row.label, bullet);
            let style = if index == selected {
                Style::default()
                    .fg(theme.text_fg)
                    .bg(theme.selected_bg)
                    .add_modifier(Modifier::BOLD)
            } else if row.active {
                Style::default().fg(theme.accent).bg(background)
            } else {
                Style::default().fg(theme.text_fg).bg(background)
            };
            Line::from(Span::styled(label, style))
        })
        .collect::<Vec<_>>();
    frame.clear_rect(list, Style::default().bg(background));
    frame.draw_lines(
        list,
        &lines,
        Style::default().bg(background).fg(theme.text_fg),
        Alignment::Left,
    );
}

/// Tracked workspace folders (▸/▾) and, when expanded, their documents.
pub(super) fn render_workspace_panel(frame: &mut Frame<'_>, model: &AppState, theme: ThemePalette) {
    let list = panel_list_rect(model);
    if list.width == 0 || list.height == 0 {
        return;
    }
    let background = theme.panel_alt_bg;
    let rows = model.workspace_rows();
    if rows.is_empty() {
        frame.draw_lines(
            list,
            &[Line::from(Span::styled(
                " No folders tracked. ⌃⇧W adds one.",
                Style::default().fg(theme.muted_fg).bg(background),
            ))],
            Style::default().bg(background).fg(theme.text_fg),
            Alignment::Left,
        );
        return;
    }
    let selected = model.panels.workspace_selected.min(rows.len() - 1);
    let scroll = panel_scroll_for(model, rows.len(), list.height as usize);
    let lines = rows
        .iter()
        .enumerate()
        .skip(scroll)
        .map(|(index, row)| {
            let counter = match (&row.kind, row.unsaved) {
                (crate::state::WorkspaceRowKind::Folder(_), unsaved) if unsaved > 0 => {
                    format!("  {unsaved}/{}", row.file_count)
                }
                _ => String::new(),
            };
            let style = if index == selected {
                Style::default()
                    .fg(theme.text_fg)
                    .bg(theme.selected_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text_fg).bg(background)
            };
            Line::from(vec![
                Span::styled(format!(" {}", row.label), style),
                Span::styled(
                    counter,
                    Style::default()
                        .fg(theme.muted_fg)
                        .bg(style.bg.unwrap_or(background)),
                ),
            ])
        })
        .collect::<Vec<_>>();
    frame.clear_rect(list, Style::default().bg(background));
    frame.draw_lines(
        list,
        &lines,
        Style::default().bg(background).fg(theme.text_fg),
        Alignment::Left,
    );
}

/// The list row a point lands on inside an open Pages or Workspace panel.
/// Shared with the mouse code so a click selects the painted row.
pub(crate) fn panel_row_at_point(model: &AppState, point: Point) -> Option<usize> {
    if !model.panels.is_open() {
        return None;
    }
    let list = panel_list_rect(model);
    if list.width == 0 || list.height == 0 || !list.contains(point) {
        return None;
    }
    let count = match model.panels.panel {
        Some(PanelKind::Pages) => model.page_rows().len(),
        Some(PanelKind::Workspace) => model.workspace_rows().len(),
        _ => return None,
    };
    let scroll = panel_scroll_for(model, count, list.height as usize);
    let row = scroll + (point.y - list.y) as usize;
    (row < count).then_some(row)
}

impl Component<AppState, Command, AppMessage, Action> for PanelView {
    fn render(&self, model: &AppState, frame: &mut Frame<'_>, _area: Rect, _focused: bool) {
        let theme = model.theme_palette();
        self.render(model, frame, theme);
    }

    fn handle_event(
        &mut self,
        model: &mut AppState,
        event: &UiEvent<Command, AppMessage>,
        cx: &mut EventCx<Action>,
    ) {
        let Some(kind) = model.panels.panel else {
            return;
        };
        let content = panel_content_rect(model);
        let (width, _) = wrap_and_gutter(
            content.width as usize,
            match kind {
                PanelKind::Notes => &model.notes().buffer,
                _ => &model.editor().buffer,
            },
            model.ui.show_line_numbers,
        );
        // Scrolling steps follow the visible rows, one page per wheel notch.
        let height = match kind {
            PanelKind::Pages | PanelKind::Workspace => panel_list_rect(model).height,
            _ => content.height,
        }
        .max(1) as usize;

        match kind {
            PanelKind::Notes => {
                if let UiEvent::Command(Command::CopySelection) = event {
                    cx.emit(Action::CopyRequested);
                    return;
                }
                if let UiEvent::Command(Command::CutSelection) = event {
                    cx.emit(Action::CutRequested);
                    return;
                }
                if let UiEvent::Command(Command::PasteClipboard) = event {
                    cx.emit(Action::PasteRequested);
                    return;
                }
                let changed = handle_text_buffer_event(
                    event,
                    &mut model.notes_mut().buffer,
                    width,
                    height,
                    false,
                );
                if changed {
                    model.notes_mut().mark_dirty();
                }
                model.notes_mut().ensure_cursor_visible(width, height);
            }
            PanelKind::Settings => {
                if let UiEvent::Command(command) = event {
                    let rows = settings_rows(model);
                    match command {
                        Command::MoveUp | Command::SelectUp => {
                            model.panels.settings_selected =
                                model.panels.settings_selected.saturating_sub(1);
                        }
                        Command::MoveDown | Command::SelectDown => {
                            model.panels.settings_selected = (model.panels.settings_selected + 1)
                                .min(rows.len().saturating_sub(1));
                        }
                        Command::MoveLeft | Command::SelectLeft => {
                            let selected = rows.get(model.panels.settings_selected);
                            if selected.map(|row| row.label) == Some("Theme") {
                                cx.emit(Action::CycleThemePrevious);
                            }
                        }
                        Command::MoveRight | Command::SelectRight => {
                            let selected = rows.get(model.panels.settings_selected);
                            if selected.map(|row| row.label) == Some("Theme") {
                                cx.emit(Action::CycleTheme);
                            }
                        }
                        Command::Submit => {
                            if let Some(row) = rows.get(model.panels.settings_selected) {
                                match row.action {
                                    // Multi-option rows list every choice
                                    // at once instead of blind cycling.
                                    Action::CycleTheme => cx.emit(Action::OpenSettingsPicker(
                                        SettingsPickerKind::Theme,
                                    )),
                                    Action::CycleLanguage => cx.emit(Action::OpenSettingsPicker(
                                        SettingsPickerKind::Language,
                                    )),
                                    Action::CycleIndentWidth => cx.emit(
                                        Action::OpenSettingsPicker(SettingsPickerKind::IndentWidth),
                                    ),
                                    _ => {
                                        cx.emit(row.action.clone());
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            PanelKind::Revisions => {
                if let UiEvent::Command(command) = event {
                    let count = revision_count_for(model);
                    match command {
                        Command::MoveUp | Command::SelectUp => {
                            // The visible list is newest-first but snapshots
                            // are stored oldest-first. Going visually up
                            // therefore increments the storage index.
                            model.panels.revision_selected =
                                (model.panels.revision_selected + 1).min(count.saturating_sub(1));
                            model.panels.scroll = 0;
                        }
                        Command::MoveDown | Command::SelectDown => {
                            model.panels.revision_selected =
                                model.panels.revision_selected.saturating_sub(1);
                            model.panels.scroll = 0;
                        }
                        Command::PageUp => {
                            model.panels.scroll =
                                model.panels.scroll.saturating_sub(height.saturating_sub(1));
                        }
                        Command::PageDown => {
                            model.panels.scroll =
                                model.panels.scroll.saturating_add(height.saturating_sub(1));
                        }
                        Command::MoveHome => {
                            model.panels.revision_selected = count.saturating_sub(1);
                            model.panels.scroll = 0;
                        }
                        Command::MoveEnd => {
                            model.panels.revision_selected = 0;
                            model.panels.scroll = 0;
                        }
                        Command::Submit => {
                            cx.emit(Action::RestoreSelectedRevision);
                        }
                        _ => {}
                    }
                }
            }
            PanelKind::Pages => {
                let count = model.page_rows().len();
                if let UiEvent::Command(command) = event {
                    match command {
                        Command::MoveUp | Command::SelectUp => {
                            model.panels.pages_selected =
                                model.panels.pages_selected.saturating_sub(1);
                            follow_panel_scroll(model);
                        }
                        Command::MoveDown | Command::SelectDown => {
                            model.panels.pages_selected =
                                (model.panels.pages_selected + 1).min(count.saturating_sub(1));
                            follow_panel_scroll(model);
                        }
                        Command::MoveHome => {
                            model.panels.pages_selected = 0;
                            follow_panel_scroll(model);
                        }
                        Command::MoveEnd => {
                            model.panels.pages_selected = count.saturating_sub(1);
                            follow_panel_scroll(model);
                        }
                        Command::Submit => {
                            if let Some(row) = model.page_rows().get(model.panels.pages_selected) {
                                let index = row.index;
                                model.panels.close();
                                cx.emit(Action::SwitchDraft(index));
                            }
                        }
                        Command::DeleteForward | Command::Backspace => {
                            // Delete asks, Ctrl+R (or Y) confirms: the same
                            // close flow the editor uses for the open page.
                            model.panels.close();
                            cx.emit(Action::TrashRequested { confirm: false });
                        }
                        _ => {}
                    }
                }
            }
            PanelKind::Workspace => {
                let rows = model.workspace_rows();
                if let UiEvent::Command(command) = event {
                    match command {
                        Command::MoveUp | Command::SelectUp => {
                            model.panels.workspace_selected =
                                model.panels.workspace_selected.saturating_sub(1);
                            follow_panel_scroll(model);
                        }
                        Command::MoveDown | Command::SelectDown => {
                            model.panels.workspace_selected = (model.panels.workspace_selected + 1)
                                .min(rows.len().saturating_sub(1));
                            follow_panel_scroll(model);
                        }
                        Command::MoveHome => {
                            model.panels.workspace_selected = 0;
                            follow_panel_scroll(model);
                        }
                        Command::MoveEnd => {
                            model.panels.workspace_selected = rows.len().saturating_sub(1);
                            follow_panel_scroll(model);
                        }
                        Command::Submit => {
                            if let Some(row) = rows.get(model.panels.workspace_selected) {
                                match &row.kind {
                                    crate::state::WorkspaceRowKind::Folder(path) => {
                                        let path = path.clone();
                                        model.toggle_workspace_expanded(&path);
                                    }
                                    crate::state::WorkspaceRowKind::File(path) => {
                                        let path = path.clone();
                                        model.panels.close();
                                        cx.emit(Action::OpenPath(path));
                                    }
                                }
                            }
                        }
                        Command::DeleteForward | Command::Backspace => {
                            if let Some(row) = rows.get(model.panels.workspace_selected) {
                                match &row.kind {
                                    crate::state::WorkspaceRowKind::Folder(path) => {
                                        let path = path.clone();
                                        cx.emit(Action::RemoveWorkspace(path));
                                    }
                                    crate::state::WorkspaceRowKind::File(path) => {
                                        cx.emit(Action::SetStatus(format!(
                                            "{} is a file row; Delete untracks a folder.",
                                            path.display()
                                        )));
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            PanelKind::Help => {
                if let UiEvent::Command(command) = event {
                    match command {
                        Command::InsertTab | Command::MoveRight => {
                            model.panels.help_tab = model.panels.help_tab.next();
                            model.panels.scroll = 0;
                        }
                        Command::Outdent | Command::MoveLeft => {
                            model.panels.help_tab = model.panels.help_tab.previous();
                            model.panels.scroll = 0;
                        }
                        _ => {}
                    }
                }
            }
            PanelKind::Stats => {
                if let UiEvent::Command(command) = event {
                    match command {
                        Command::MoveUp | Command::SelectUp => {
                            model.panels.scroll = model.panels.scroll.saturating_sub(1);
                        }
                        Command::MoveDown | Command::SelectDown => {
                            model.panels.scroll = model.panels.scroll.saturating_add(1);
                        }
                        Command::PageUp => {
                            model.panels.scroll =
                                model.panels.scroll.saturating_sub(height.saturating_sub(1));
                        }
                        Command::PageDown => {
                            model.panels.scroll =
                                model.panels.scroll.saturating_add(height.saturating_sub(1));
                        }
                        Command::MoveHome => model.panels.scroll = 0,
                        _ => {}
                    }
                }
            }
        }
    }
}

/// The wrap width of the focused window's text columns. Both the cursor
/// position and the char counters count wrapped lines the way the editor
/// paints them, so they need the window that owns the keyboard.

#[derive(Clone, Copy)]
pub(super) struct HelpEntry {
    pub(super) commands: &'static [Command],
    pub(super) keys: &'static str,
    pub(super) description: &'static str,
}

#[derive(Clone, Copy)]
pub(super) struct HelpSection {
    pub(super) tab: HelpTab,
    pub(super) entries: &'static [HelpEntry],
}

const HELP_NONE: &[Command] = &[];
const HELP_MOVE_LEFT: &[Command] = &[Command::MoveLeft];
const HELP_MOVE_RIGHT: &[Command] = &[Command::MoveRight];
const HELP_MOVE_WORD_LEFT: &[Command] = &[Command::MoveWordLeft];
const HELP_MOVE_WORD_RIGHT: &[Command] = &[Command::MoveWordRight];
const HELP_SELECT_ARROWS: &[Command] = &[
    Command::SelectLeft,
    Command::SelectRight,
    Command::SelectUp,
    Command::SelectDown,
];
const HELP_MOVE_HOME_END: &[Command] = &[Command::MoveHome, Command::MoveEnd];
const HELP_PAGE_MOVEMENT: &[Command] = &[Command::PageUp, Command::PageDown];
const HELP_INSERT_NEWLINE: &[Command] = &[Command::Submit];
const HELP_INDENT: &[Command] = &[Command::InsertTab, Command::Outdent];
const HELP_INSERT_TAB: &[Command] = &[Command::InsertTab];
const HELP_CYCLE_CASE: &[Command] = &[Command::CycleCase];
const HELP_BOLD_ITALIC: &[Command] = &[Command::ToggleBold, Command::ToggleItalic];
const HELP_FORMAT: &[Command] = &[
    Command::BlockTitle,
    Command::BlockHeading,
    Command::BlockQuote,
    Command::BlockBody,
];
const HELP_INSERT_TIMESTAMP: &[Command] = &[Command::InsertTimestamp];
const HELP_CLIPBOARD: &[Command] = &[
    Command::CopySelection,
    Command::CutSelection,
    Command::PasteClipboard,
];
const HELP_UNDO_REDO: &[Command] = &[Command::Undo, Command::Redo];
const HELP_ALIGNMENT: &[Command] = &[
    Command::AlignLeft,
    Command::AlignCenter,
    Command::AlignRight,
    Command::AlignJustify,
];
const HELP_OPEN_LINE: &[Command] = &[Command::OpenLine];
const HELP_DELETE_LINE_END: &[Command] = &[Command::DeleteLine, Command::DeleteToEndOfLine];
const HELP_DELETE_WORD: &[Command] = &[Command::DeleteWordLeft, Command::DeleteWordForward];
const HELP_TOGGLE_INSERT: &[Command] = &[Command::ToggleInsertMode];
const HELP_NEW_DRAFT: &[Command] = &[Command::NewDraft];
const HELP_OPEN_FILE: &[Command] = &[Command::OpenFilePrompt];
const HELP_READ_FILE: &[Command] = &[Command::OpenReadFilePrompt];
const HELP_SAVE: &[Command] = &[Command::Save];
const HELP_SAVE_AS: &[Command] = &[Command::OpenSavePathPrompt];
const HELP_TRASH_FILE: &[Command] = &[Command::TrashFile];
const HELP_NEXT_PAGE: &[Command] = &[Command::NextPage, Command::PreviousPage];
const HELP_MOVE_PAGE: &[Command] = &[Command::MovePageUp, Command::MovePageDown];
const HELP_WINDOWS: &[Command] = &[Command::NextWindow, Command::CloseWindow];
const HELP_QUIT: &[Command] = &[Command::Quit];
const HELP_WORKSPACE: &[Command] = &[Command::OpenWorkspacePrompt];
const HELP_DOC_STATUS: &[Command] = &[Command::OpenStatusPrompt];
const HELP_NOTES: &[Command] = &[Command::ToggleNotes];
const HELP_ZOOM: &[Command] = &[Command::ToggleZoom];
const HELP_STATS: &[Command] = &[Command::ToggleStats];
const HELP_TARGET: &[Command] = &[Command::OpenWordTargetPrompt];
const HELP_THEMES: &[Command] = &[Command::CycleTheme, Command::OpenThemePrompt];
const HELP_CUSTOM_ACCENT: &[Command] = &[Command::OpenCustomColorPrompt];
const HELP_SETTINGS: &[Command] = &[Command::ToggleSettings];
const HELP_REVISIONS: &[Command] = &[Command::ToggleRevisions];
const HELP_JUMP_BACK: &[Command] = &[Command::JumpPreviousPosition];
const HELP_DOC_ENDS: &[Command] = &[Command::MoveDocumentTop, Command::MoveDocumentBottom];
const HELP_WINDOW_ENDS: &[Command] = &[Command::MoveWindowTop, Command::MoveWindowBottom];
const HELP_BLOCK_JUMP_START: &[Command] = &[Command::MoveToBlockStart];
const HELP_BLOCK_JUMP_END: &[Command] = &[Command::MoveToBlockEnd];
const HELP_BLOCK_MARK_START: &[Command] = &[Command::MarkBlockBegin];
const HELP_BLOCK_MARK_END: &[Command] = &[Command::MarkBlockEnd];
const HELP_BLOCK_COPY: &[Command] = &[Command::CopyBlock];
const HELP_BLOCK_MOVE: &[Command] = &[Command::MoveBlock];
const HELP_BLOCK_DELETE: &[Command] = &[Command::DeleteBlock];
const HELP_BLOCK_WRITE: &[Command] = &[Command::OpenWriteBlockPrompt];

const EDITOR_MOVEMENT_HELP: &[HelpEntry] = &[
    HelpEntry {
        commands: HELP_MOVE_WORD_LEFT,
        keys: "⌥←",
        description: "Previous word",
    },
    HelpEntry {
        commands: HELP_MOVE_WORD_RIGHT,
        keys: "⌥→",
        description: "Next word",
    },
    HelpEntry {
        commands: HELP_SELECT_ARROWS,
        keys: "⇧Arrows",
        description: "Extend selection",
    },
    HelpEntry {
        commands: HELP_MOVE_HOME_END,
        keys: "Home / End",
        description: "Line start / end",
    },
    HelpEntry {
        commands: HELP_PAGE_MOVEMENT,
        keys: "PgUp / PgDn",
        description: "Page up / down",
    },
    HelpEntry {
        commands: HELP_DOC_ENDS,
        keys: "⌃Home / ⌃End",
        description: "Document start / end",
    },
    HelpEntry {
        commands: HELP_WINDOW_ENDS,
        keys: "⌃⇧PgUp / PgDn",
        description: "Window start / end",
    },
    HelpEntry {
        commands: HELP_JUMP_BACK,
        keys: "⌃⇧J",
        description: "Jump back",
    },
    HelpEntry {
        commands: HELP_NONE,
        keys: "⌃K 0-9 / ⌃K Q",
        description: "Set / jump bookmark",
    },
];

const EDITOR_WRITING_HELP: &[HelpEntry] = &[
    HelpEntry {
        commands: HELP_INDENT,
        keys: "Tab / ⇧Tab",
        description: "Indent / outdent",
    },
    HelpEntry {
        commands: HELP_CYCLE_CASE,
        keys: "⌥U",
        description: "Cycle case",
    },
    HelpEntry {
        commands: HELP_BOLD_ITALIC,
        keys: "⌥B / ⌥E",
        description: "Bold / italic",
    },
    HelpEntry {
        commands: HELP_FORMAT,
        keys: "Format menu",
        description: "Title / heading / quote",
    },
    HelpEntry {
        commands: HELP_INSERT_TIMESTAMP,
        keys: "⌥T",
        description: "Insert timestamp",
    },
    HelpEntry {
        commands: HELP_CLIPBOARD,
        keys: "⌃C / ⌃X / ⌃V",
        description: "Copy / cut / paste",
    },
    HelpEntry {
        commands: HELP_UNDO_REDO,
        keys: "⌃Z / ⌃⇧Z",
        description: "Undo / redo",
    },
    HelpEntry {
        commands: HELP_ALIGNMENT,
        keys: "⌃O + L / C / R / J",
        description: "Align paragraph (left/center/right/justify)",
    },
    HelpEntry {
        commands: HELP_OPEN_LINE,
        keys: "⌃⇧O",
        description: "Open line",
    },
    HelpEntry {
        commands: HELP_DELETE_LINE_END,
        keys: "⌃Y / ⌃⌥K",
        description: "Delete line / to end",
    },
    HelpEntry {
        commands: HELP_DELETE_WORD,
        keys: "⌃Backspace / ⌃Delete",
        description: "Delete word left / right",
    },
    HelpEntry {
        commands: HELP_TOGGLE_INSERT,
        keys: "Insert",
        description: "Insert / overwrite",
    },
];

pub(super) const EDITOR_WORKSPACE_HELP: &[HelpEntry] = &[
    HelpEntry {
        commands: HELP_NEW_DRAFT,
        keys: "⌃N",
        description: "New draft",
    },
    HelpEntry {
        commands: HELP_OPEN_FILE,
        keys: "⌃B",
        description: "Open file",
    },
    HelpEntry {
        commands: HELP_READ_FILE,
        keys: "⌃K O",
        description: "Read file at cursor",
    },
    HelpEntry {
        commands: HELP_SAVE,
        keys: "⌃S / ⌃K S",
        description: "Save",
    },
    HelpEntry {
        commands: HELP_SAVE_AS,
        keys: "⌃⇧S",
        description: "Save as",
    },
    HelpEntry {
        commands: HELP_TRASH_FILE,
        keys: "⌃K R",
        description: "Trash file",
    },
    HelpEntry {
        commands: HELP_NEXT_PAGE,
        keys: "F6 / ⇧F6",
        description: "Next / previous page",
    },
    HelpEntry {
        commands: HELP_MOVE_PAGE,
        keys: "⌥↑ / ⌥↓",
        description: "Move page up / down",
    },
    HelpEntry {
        commands: HELP_WINDOWS,
        keys: "⌥N / ⌥W",
        description: "Next / close window",
    },
    HelpEntry {
        commands: HELP_WORKSPACE,
        keys: "⌃⇧W",
        description: "Track workspace folder",
    },
    HelpEntry {
        commands: HELP_DOC_STATUS,
        keys: "⌥S",
        description: "Set document status",
    },
    HelpEntry {
        commands: HELP_QUIT,
        keys: "⌃Q",
        description: "Quit",
    },
];

const EDITOR_TOOLS_HELP: &[HelpEntry] = &[
    HelpEntry {
        commands: HELP_NOTES,
        keys: "F4 / ⌃K N",
        description: "Toggle notes",
    },
    HelpEntry {
        commands: HELP_ZOOM,
        keys: "F11",
        description: "Zoom document",
    },
    HelpEntry {
        commands: HELP_STATS,
        keys: "F9 / ⌃K W",
        description: "Word stats",
    },
    HelpEntry {
        commands: HELP_TARGET,
        keys: "⌃G",
        description: "Word target",
    },
    HelpEntry {
        commands: HELP_THEMES,
        keys: "⌃T / ⇧⌃T",
        description: "Cycle / choose theme",
    },
    HelpEntry {
        commands: HELP_CUSTOM_ACCENT,
        keys: "⌃K C",
        description: "Accent color",
    },
    HelpEntry {
        commands: HELP_SETTINGS,
        keys: "F2",
        description: "Settings",
    },
    HelpEntry {
        commands: HELP_REVISIONS,
        keys: "F5",
        description: "Revisions",
    },
];

const EDITOR_BLOCKS_HELP: &[HelpEntry] = &[
    HelpEntry {
        commands: HELP_BLOCK_JUMP_START,
        keys: "⌃⇧B",
        description: "Jump to block start",
    },
    HelpEntry {
        commands: HELP_BLOCK_JUMP_END,
        keys: "⌃⇧K",
        description: "Jump to block end",
    },
    HelpEntry {
        commands: HELP_BLOCK_MARK_START,
        keys: "⌃K B",
        description: "Mark block start",
    },
    HelpEntry {
        commands: HELP_BLOCK_MARK_END,
        keys: "⌃K K",
        description: "Mark block end",
    },
    HelpEntry {
        commands: HELP_BLOCK_COPY,
        keys: "⌃K V",
        description: "Copy block",
    },
    HelpEntry {
        commands: HELP_BLOCK_MOVE,
        keys: "⌃K M",
        description: "Move block",
    },
    HelpEntry {
        commands: HELP_BLOCK_DELETE,
        keys: "⌃K Z",
        description: "Delete block",
    },
    HelpEntry {
        commands: HELP_BLOCK_WRITE,
        keys: "⌃K I",
        description: "Write block to file",
    },
];

pub(super) const EDITOR_HELP_SECTIONS: &[HelpSection] = &[
    HelpSection {
        tab: HelpTab::Movement,
        entries: EDITOR_MOVEMENT_HELP,
    },
    HelpSection {
        tab: HelpTab::Writing,
        entries: EDITOR_WRITING_HELP,
    },
    HelpSection {
        tab: HelpTab::Workspace,
        entries: EDITOR_WORKSPACE_HELP,
    },
    HelpSection {
        tab: HelpTab::Tools,
        entries: EDITOR_TOOLS_HELP,
    },
    HelpSection {
        tab: HelpTab::Blocks,
        entries: EDITOR_BLOCKS_HELP,
    },
];

pub(super) fn help_entries(tab: HelpTab) -> &'static [HelpEntry] {
    EDITOR_HELP_SECTIONS
        .iter()
        .find(|section| section.tab == tab)
        .map(|section| section.entries)
        .unwrap_or(&[])
}

pub(super) const WELCOME_MENU_HELP: &[HelpEntry] = &[
    HelpEntry {
        commands: HELP_INSERT_NEWLINE,
        keys: "Enter",
        description: "open selected option",
    },
    HelpEntry {
        commands: HELP_MOVE_RIGHT,
        keys: "Right",
        description: "enter Help",
    },
    HelpEntry {
        commands: HELP_MOVE_LEFT,
        keys: "Left",
        description: "return to menu",
    },
    HelpEntry {
        commands: HELP_INSERT_TAB,
        keys: "Tab",
        description: "next option",
    },
    HelpEntry {
        commands: &[
            Command::MoveUp,
            Command::MoveDown,
            Command::SelectUp,
            Command::SelectDown,
        ],
        keys: "Up/Down",
        description: "move or scroll",
    },
    HelpEntry {
        commands: HELP_PAGE_MOVEMENT,
        keys: "PgUp/PgDn",
        description: "fast scroll",
    },
    HelpEntry {
        commands: HELP_MOVE_HOME_END,
        keys: "Home/End",
        description: "top / bottom",
    },
    HelpEntry {
        commands: HELP_NONE,
        keys: "R",
        description: "resume last page",
    },
    HelpEntry {
        commands: HELP_NONE,
        keys: "D",
        description: "new draft",
    },
    HelpEntry {
        commands: HELP_NONE,
        keys: "O",
        description: "open browser",
    },
    HelpEntry {
        commands: HELP_NONE,
        keys: "H/?",
        description: "select Help",
    },
    HelpEntry {
        commands: HELP_QUIT,
        keys: "X",
        description: "exit Ordet",
    },
    HelpEntry {
        commands: HELP_QUIT,
        keys: "⌃Q",
        description: "quit",
    },
];

pub(super) fn render_help_panel(
    frame: &mut Frame<'_>,
    area: Rect,
    model: &AppState,
    theme: ThemePalette,
) {
    let tab = model.panels.help_tab;
    let entries = help_entries(tab);
    let base = Style::default().fg(theme.text_fg).bg(theme.panel_alt_bg);

    let mut lines = Vec::with_capacity(entries.len() + 4);
    lines.push(help_tab_bar(tab, theme));
    lines.push(Line::from(""));

    // Action on the left, shortcut pinned to the right edge. One row per
    // action: these pages are deliberately short enough not to scroll.
    let width = area.width as usize;
    let natural_key_width = entries
        .iter()
        .map(|entry| UnicodeWidthStr::width(entry.keys))
        .max()
        .unwrap_or(0);
    let key_width = natural_key_width.min(width.saturating_sub(12)).max(1);
    let action_width = width.saturating_sub(key_width + 2).max(1);
    for entry in entries {
        let action = fit_text_to_width(entry.description, action_width);
        let action_width_used = UnicodeWidthStr::width(action.as_str());
        let keys = fit_text_to_width(entry.keys, key_width);
        let keys_width = UnicodeWidthStr::width(keys.as_str());
        let padding = action_width.saturating_sub(action_width_used)
            + 2
            + key_width.saturating_sub(keys_width);
        lines.push(Line::from(vec![
            Span::styled(action, base),
            Span::styled(" ".repeat(padding), base),
            Span::styled(
                keys,
                Style::default()
                    .fg(theme.accent)
                    .bg(theme.panel_alt_bg)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    // Pin the footer to the bottom of the fixed-height card. Shorter tabs
    // leave quiet rows between their last action and the key guide instead
    // of making the card jump when sections change.
    let footer_row = (area.height as usize).saturating_sub(1);
    while lines.len() < footer_row {
        lines.push(Line::from(""));
    }
    lines.push(help_footer_line(theme));

    frame.clear_rect(area, base);
    frame.draw_lines(area, &lines, base, Alignment::Left);
}

/// The section strip: the selected tab is bracketed and accented, the rest
/// stay quiet. Every tab retains its surrounding spaces so switching one
/// never shifts the others sideways.
pub(super) fn help_tab_bar(selected: HelpTab, theme: ThemePalette) -> Line {
    let mut spans = Vec::with_capacity(HelpTab::ALL.len());
    for tab in HelpTab::ALL {
        let is_selected = tab == selected;
        let label = if is_selected {
            format!("[ {} ]", tab.title())
        } else {
            format!(" {} ", tab.title())
        };
        let style = if is_selected {
            Style::default()
                .fg(theme.accent)
                .bg(theme.panel_alt_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted_fg).bg(theme.panel_alt_bg)
        };
        spans.push(Span::styled(label, style));
    }
    Line::from(spans)
}

/// The small key guide pinned to the bottom of the Help card. The global
/// status row stays quiet for this panel; the card carries its own hint.
pub(super) fn help_footer_line(theme: ThemePalette) -> Line {
    let key_style = Style::default()
        .fg(theme.accent)
        .bg(theme.panel_alt_bg)
        .add_modifier(Modifier::BOLD);
    let text_style = Style::default().fg(theme.muted_fg).bg(theme.panel_alt_bg);
    Line::from(vec![
        Span::styled("[Tab / ←→]", key_style),
        Span::styled(": Switch Section ", text_style),
        Span::styled("[Esc]", key_style),
        Span::styled(": Close.", text_style),
    ])
}

pub(super) fn help_section_title(title: &str, theme: ThemePalette, background: Color) -> Line {
    Line::from(Span::styled(
        title.to_string(),
        Style::default()
            .fg(theme.accent)
            .bg(background)
            .add_modifier(Modifier::BOLD),
    ))
}

/// Label column of the settings list. Wide enough for "Automatic backups",
/// so every value starts in the same column.
const SETTINGS_LABEL_WIDTH: usize = 17;
/// Widest the value column grows before the value wraps.
const SETTINGS_VALUE_MAX_WIDTH: usize = 26;
/// Fixed settings card width. The card must not resize when a value changes,
/// because theme names range from "Edo" to "On the Silver Globe".
const SETTINGS_PANEL_WIDTH: u16 =
    (SETTINGS_LABEL_WIDTH + 1 + SETTINGS_VALUE_MAX_WIDTH + PANEL_BORDER_COLUMNS) as u16;

/// One settings row: its label, current value, and what Enter does.
pub(super) struct SettingsRow {
    pub(super) section: &'static str,
    pub(super) label: &'static str,
    pub(super) value: String,
    pub(super) action: Action,
}

pub(super) fn settings_rows(model: &AppState) -> Vec<SettingsRow> {
    let on_off = |on: bool| {
        if on {
            "on".to_string()
        } else {
            "off".to_string()
        }
    };
    vec![
        SettingsRow {
            section: "Appearance",
            label: "Theme",
            value: model.theme_name(),
            action: Action::CycleTheme,
        },
        SettingsRow {
            section: "Appearance",
            label: "Accent",
            value: model
                .ui
                .theme
                .custom_accent()
                .unwrap_or("default")
                .to_string(),
            action: Action::OpenCustomColorPrompt,
        },
        SettingsRow {
            section: "Appearance",
            label: "Line numbers",
            value: on_off(model.ui.show_line_numbers),
            action: Action::ToggleLineNumbers,
        },
        SettingsRow {
            section: "Appearance",
            label: "Ruler",
            value: on_off(model.ui.show_ruler),
            action: Action::ToggleRuler,
        },
        SettingsRow {
            section: "Appearance",
            label: "Menu bar",
            value: on_off(model.ui.show_menu_bar),
            action: Action::ToggleMenuBar,
        },
        SettingsRow {
            section: "Appearance",
            label: "Live counts",
            value: on_off(model.ui.show_live_counts),
            action: Action::ToggleLiveCounts,
        },
        SettingsRow {
            section: "Writing",
            label: "Word target",
            value: model
                .ui
                .word_target
                .map(|target| target.to_string())
                .unwrap_or_else(|| "off".to_string()),
            action: Action::OpenWordTargetPrompt,
        },
        SettingsRow {
            section: "Writing",
            label: "Autosave",
            value: on_off(model.ui.autosave_enabled),
            action: Action::ToggleAutosave,
        },
        SettingsRow {
            section: "Writing",
            label: "Indent width",
            value: model.ui.indent_width.to_string(),
            action: Action::CycleIndentWidth,
        },
        SettingsRow {
            section: "Backup",
            label: "Automatic backups",
            value: on_off(model.ui.backup_enabled),
            action: Action::ToggleBackup,
        },
        SettingsRow {
            section: "Backup",
            label: "Back up on close",
            value: on_off(model.ui.backup_on_close),
            action: Action::ToggleBackupOnClose,
        },
        SettingsRow {
            section: "Backup",
            label: "Keep backups",
            value: format!("{} recent", model.ui.backup_keep),
            action: Action::CycleBackupKeep,
        },
        SettingsRow {
            section: "Backup",
            label: "Backup location",
            value: if model.ui.backup_dir.trim().is_empty() {
                "default".to_string()
            } else {
                model.ui.backup_dir.clone()
            },
            action: Action::BackupLocationPrompt,
        },
        SettingsRow {
            section: "Backup",
            label: "Back up now",
            value: "Enter to run".to_string(),
            action: Action::BackupNow,
        },
    ]
}

pub(super) fn render_settings_panel(
    frame: &mut Frame<'_>,
    area: Rect,
    model: &AppState,
    theme: ThemePalette,
) {
    let rows = settings_rows(model);
    let selected = model
        .panels
        .settings_selected
        .min(rows.len().saturating_sub(1));
    let value_width = (area.width as usize)
        .saturating_sub(SETTINGS_LABEL_WIDTH + 1)
        .max(1);
    // Long values (e.g. the config path) wrap onto continuation lines
    // instead of truncating with `...`, so the full text always shows.
    // Visual lines carry their row index; the whole wrapped value wears the
    // selection highlight.
    let mut visual: Vec<(usize, Line)> = Vec::new();
    let mut previous_section = None;
    for (index, row) in rows.iter().enumerate() {
        if previous_section != Some(row.section) {
            visual.push((
                usize::MAX,
                Line::from(Span::styled(
                    row.section.to_uppercase(),
                    Style::default()
                        .fg(theme.accent)
                        .bg(theme.panel_alt_bg)
                        .add_modifier(Modifier::BOLD),
                )),
            ));
            previous_section = Some(row.section);
        }
        let is_selected = index == selected;
        let row_style = if is_selected {
            Style::default()
                .fg(theme.selected_fg())
                .bg(theme.selected_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text_fg).bg(theme.panel_alt_bg)
        };
        let row_bg = row_style.bg.unwrap_or(theme.panel_alt_bg);
        let chunks = split_to_width(&row.value, value_width);
        for (n, chunk) in chunks.iter().enumerate() {
            let line = if n == 0 {
                Line::from(vec![
                    Span::styled(
                        format!("{:<SETTINGS_LABEL_WIDTH$} ", row.label),
                        Style::default().fg(theme.muted_fg).bg(row_bg),
                    ),
                    Span::styled(chunk.clone(), row_style),
                ])
            } else {
                Line::from(vec![
                    Span::styled(
                        " ".repeat(SETTINGS_LABEL_WIDTH + 1),
                        Style::default().bg(row_bg),
                    ),
                    Span::styled(chunk.clone(), row_style),
                ])
            };
            visual.push((index, line));
        }
    }
    let height = area.height as usize;
    let sel_first = visual
        .iter()
        .position(|(index, _)| *index == selected)
        .unwrap_or(0);
    let start = sel_first
        .saturating_sub(height.saturating_sub(1))
        .min(visual.len().saturating_sub(height));
    let lines: Vec<Line> = visual
        .into_iter()
        .skip(start)
        .take(height.max(1))
        .map(|(_, line)| line)
        .collect();
    frame.clear_rect(area, Style::default().bg(theme.panel_alt_bg));
    frame.draw_lines(
        area,
        &lines,
        Style::default().bg(theme.panel_alt_bg).fg(theme.text_fg),
        Alignment::Left,
    );
}

/// Splits text into display-width chunks, breaking anywhere (paths have no
/// spaces to wrap on). Always returns at least one chunk.
pub(super) fn split_to_width(text: &str, max_width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthStr;
    let max_width = max_width.max(1);
    if text.is_empty() {
        return vec![String::new()];
    }
    if UnicodeWidthStr::width(text) <= max_width {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let char_width = ch.width().unwrap_or(0).max(1);
        if width + char_width > max_width && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
            width = 0;
        }
        current.push(ch);
        width = width.saturating_add(char_width);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    chunks
}

/// Snapshots for the active draft's file, oldest first. Empty for scratch
/// drafts, which have no file to key on.
pub(super) fn revisions_for_active_file(model: &AppState) -> Vec<Revision> {
    model
        .editor()
        .file_path
        .as_ref()
        .map(|path| model.history.revisions_for(&history_key_for(path)).to_vec())
        .unwrap_or_default()
}

pub(super) fn revision_count_for(model: &AppState) -> usize {
    revisions_for_active_file(model).len()
}

/// Empty-state copy of the Revisions card; also the widest fixed line it
/// prints, so `revisions_panel_width` sizes the card to it.
const REVISIONS_EMPTY_HINT: &str = "Save the draft; every save keeps a snapshot.";

pub(super) fn render_revisions_panel(
    frame: &mut Frame<'_>,
    area: Rect,
    model: &AppState,
    theme: ThemePalette,
) {
    let background = theme.panel_alt_bg;
    let mut lines = Vec::new();
    let revisions = revisions_for_active_file(model);

    if revisions.is_empty() {
        lines.push(Line::from(Span::styled(
            "No revisions yet.",
            Style::default().fg(theme.text_fg).bg(background),
        )));
        lines.push(Line::from(Span::styled(
            REVISIONS_EMPTY_HINT,
            Style::default().fg(theme.muted_fg).bg(background),
        )));
    } else {
        let now = crate::history::unix_now();
        let selected = model.panels.revision_selected.min(revisions.len() - 1);
        // Newest first: index 0 in the list is the latest snapshot.
        for index in (0..revisions.len()).rev() {
            let revision = &revisions[index];
            let delta = if index > 0 {
                revision.words as isize - revisions[index - 1].words as isize
            } else {
                0
            };
            let delta_text = if delta == 0 {
                String::new()
            } else if delta > 0 {
                format!(" +{delta}")
            } else {
                format!(" {delta}")
            };
            let text = format!(
                "{}  {}w{}",
                relative_time(now, revision.timestamp),
                revision.words,
                delta_text
            );
            let style = if index == selected {
                Style::default()
                    .fg(theme.text_fg)
                    .bg(theme.selected_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text_fg).bg(background)
            };
            lines.push(Line::from(Span::styled(
                fit_text_to_width_tail(&text, area.width as usize),
                style,
            )));
        }
        lines.push(Line::from(Span::styled(
            "------------------------------",
            Style::default().fg(theme.muted_fg).bg(background),
        )));
        let current = model.editor().buffer.plain_text();
        for diff in diff_lines(&revisions[selected].text, &current, 2) {
            let tag_style = match diff.tag {
                '-' => Style::default().fg(theme.warn).bg(background),
                '+' => Style::default().fg(theme.accent).bg(background),
                _ => Style::default().fg(theme.muted_fg).bg(background),
            };
            // A revision is source material: preserve every character and
            // wrap it into the panel instead of hiding its tail behind `...`.
            for wrapped in wrap_text(&format!("{} {}", diff.tag, diff.text), area.width as usize) {
                lines.push(Line::from(Span::styled(wrapped, tag_style)));
            }
        }
        lines.push(Line::from(Span::styled(
            "Enter restores, Up/Down picks",
            Style::default().fg(theme.muted_fg).bg(background),
        )));
    }

    let scroll = model.panels.scroll.min(lines.len().saturating_sub(1));
    let visible: Vec<Line> = lines.into_iter().skip(scroll).collect();
    frame.clear_rect(area, Style::default().bg(background));
    frame.draw_lines(
        area,
        &visible,
        Style::default().bg(background).fg(theme.text_fg),
        Alignment::Left,
    );
}

pub(super) fn render_stats_panel(
    frame: &mut Frame<'_>,
    area: Rect,
    model: &AppState,
    theme: ThemePalette,
) {
    let Some(stats) = model.editor().word_stats_if_ready() else {
        let lines = vec![Line::from(Span::styled(
            "Counting words and characters...",
            Style::default().fg(theme.muted_fg).bg(theme.panel_alt_bg),
        ))];
        frame.clear_rect(
            area,
            Style::default().bg(theme.panel_alt_bg).fg(theme.text_fg),
        );
        frame.draw_lines(
            area,
            &lines,
            Style::default().bg(theme.panel_alt_bg).fg(theme.text_fg),
            Alignment::Left,
        );
        return;
    };
    let mut lines = vec![
        stat_line("Words", stats.word_count, theme),
        stat_line("Characters", stats.chars, theme),
        stat_line("No spaces", stats.chars_no_spaces, theme),
        stat_line("Paragraphs", stats.paragraphs, theme),
        stat_line("Sentences", stats.sentences, theme),
        stat_line("Est. pages", format!("{} (250w/page)", stats.pages), theme),
        stat_line(
            "Reading time",
            format_reading_time(stats.reading_seconds),
            theme,
        ),
    ];

    if let Some(progress) = model.word_target_progress() {
        lines.push(stat_line(
            "Target",
            format!("{} (⌃G)", progress.target),
            theme,
        ));
        lines.push(stat_line(
            if progress.met {
                "Overflow"
            } else {
                "Remaining"
            },
            if progress.met {
                progress.overflow
            } else {
                progress.remaining
            },
            theme,
        ));
        lines.push(stat_line(
            "Progress",
            format!("{}%", progress.percent),
            theme,
        ));
        lines.push(progress_bar_line(
            progress.ratio,
            theme,
            area.width.saturating_sub(2) as usize,
        ));
    } else {
        lines.push(stat_line("Target", "off (⌃G)", theme));
    }

    let start = model.panels.scroll.min(lines.len().saturating_sub(1));
    let end = (start + area.height as usize).min(lines.len());
    let visible = &lines[start..end];

    frame.clear_rect(
        area,
        Style::default().bg(theme.panel_alt_bg).fg(theme.text_fg),
    );
    frame.draw_lines(
        area,
        visible,
        Style::default().bg(theme.panel_alt_bg).fg(theme.text_fg),
        Alignment::Left,
    );
}
