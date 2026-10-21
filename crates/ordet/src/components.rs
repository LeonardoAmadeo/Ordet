use std::path::Path;

use crossterm::event::{KeyCode, KeyModifiers};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use ordet_tui_framework::{
    Alignment, Color, Component, EventCx, Frame, Line, Modifier, Point, Rect, Span, Style, UiEvent,
};

use crate::commands::{Action, AppMessage, Command};
use crate::editor_buffer::{AttrKind, EditorBuffer, WrappedLine, TAB_STOP};
use crate::file_browser::{browser_layout, FileBrowserMode, FileBrowserState};
use crate::history::{diff_lines, history_key_for, relative_time, Revision};
use crate::i18n::TextKey;
use crate::state::{
    panel_content_rect, wrap_text, AppState, HelpTab, NotesPaneState, PanelKind,
    SettingsPickerKind, SettingsPickerState, WelcomeOption,
};
use crate::stats::format_reading_time;
use crate::theme::ThemePalette;

mod modals;
mod panels;
mod status;

use modals::*;
use panels::*;
use status::*;

pub(crate) use panels::{follow_panel_scroll, panel_rect, panel_row_at_point};

mod welcome;

use welcome::*;

pub struct RootView {
    editor: EditorPane,
    panels: PanelView,
    status: StatusBar,
}

impl RootView {
    pub fn new() -> Self {
        Self {
            editor: EditorPane,
            panels: PanelView,
            status: StatusBar,
        }
    }
}

impl Component<AppState, Command, AppMessage, Action> for RootView {
    fn render(&self, model: &AppState, frame: &mut Frame<'_>, area: Rect, _focused: bool) {
        let theme = model.theme_palette();
        draw_header(model, frame, model.ui.layout.header, theme);
        if model.show_welcome_screen() {
            render_welcome_screen(frame, model.body_area(), model, theme);
        } else {
            self.editor
                .render(model, frame, model.ui.layout.editor, true);
            // Panels are overlays over the body, never reserved columns, so
            // the text keeps its width when one opens.
            if model.panels.is_open() {
                self.panels.render(model, frame, theme);
            }
        }
        if should_render_browser_modal(model) {
            if let Some(browser) = &model.ui.file_browser {
                render_file_browser(frame, model.body_area(), model, browser, theme);
            }
        }
        if let Some(picker) = &model.panels.settings_picker {
            render_settings_picker_modal(frame, area, model, picker, theme);
        }
        if let Some(open) = model.ui.menu_open {
            render_menu_dropdown(frame, model, open, theme);
        }
        if model.ui.about_open {
            render_about_modal(frame, area, model, theme);
        }
        self.status
            .render(model, frame, model.ui.layout.status, false);
    }

    fn handle_event(
        &mut self,
        model: &mut AppState,
        event: &UiEvent<Command, AppMessage>,
        cx: &mut EventCx<Action>,
    ) {
        if model.show_welcome_screen() {
            handle_welcome_event(model, event, cx);
            return;
        }

        if model.panels.is_open() {
            self.panels.handle_event(model, event, cx);
        } else {
            self.editor.handle_event(model, event, cx);
        }
    }
}

/// Paint one split pane's title row. Split panes need their own names: the
/// header keeps the menus, so it cannot identify whichever pane has focus.
/// The name is centred, like the panel titles.
fn draw_window_title(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    focused: bool,
    theme: ThemePalette,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let style = if focused {
        Style::default()
            .fg(theme.accent)
            .bg(theme.panel_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.muted_fg).bg(theme.panel_bg)
    };
    let label = fit_text_to_width(title, area.width.saturating_sub(2) as usize);
    let title = if area.width >= 2 {
        format!(" {label} ")
    } else {
        label
    };
    frame.draw_line(
        Rect::new(area.x, area.y, area.width, 1),
        0,
        &Line::from(Span::styled(title, style)),
        Style::default().bg(theme.panel_bg),
        Alignment::Center,
    );
}

/// Paint one window of the desktop: its title row (in a split), ruler row,
/// and the draft's visible text. `focused` gates the cursor block and the
/// title/ruler accent, so only the window that owns the keyboard lights up.
fn render_editor_window(
    model: &AppState,
    frame: &mut Frame<'_>,
    mut area: Rect,
    draft_index: usize,
    focused: bool,
    theme: ThemePalette,
    show_title: bool,
) {
    let Some(draft) = model.drafts.get(draft_index) else {
        return;
    };
    if show_title {
        draw_window_title(frame, area, &draft.page_label(), focused, theme);
        area = Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            area.height.saturating_sub(1),
        );
    }
    let editor = &draft.editor;
    let text_area = editor_text_area(area);

    // The ruler rides the editor's top padding row, exactly over the text
    // columns, so it never steals a text row. Zoom hides it.
    if model.ui.show_ruler && !model.ui.zoomed && area.height > 1 && text_area.width > 0 {
        let (_, gutter) = wrap_and_gutter(
            text_area.width as usize,
            &editor.buffer,
            model.ui.show_line_numbers,
        );
        let width = (text_area.width as usize).saturating_sub(gutter);
        if width > 0 {
            let ruler_area = Rect::new(
                text_area.x.saturating_add(gutter as u16),
                area.y,
                width as u16,
                1,
            );
            // The ruler doubles as the focus marker: in a split the keyboard
            // pane's ruler wears the accent, the others stay in the border
            // colour, and only the focused one tracks the caret with one
            // inverted cell. A single window keeps its quieter ruler.
            let ruler_fg = if focused && show_title {
                theme.accent
            } else if focused {
                theme.muted_fg
            } else {
                theme.border
            };
            let base = Style::default().fg(ruler_fg).bg(theme.panel_bg);
            let caret_column = focused.then(|| editor.buffer.cursor_visual_column(width));
            let (before, caret, after) = ruler_caret_split(width, caret_column);
            let mut spans = Vec::with_capacity(3);
            if !before.is_empty() {
                spans.push(Span::styled(before, base));
            }
            if let Some(glyph) = caret {
                spans.push(Span::styled(
                    glyph.to_string(),
                    Style::default().fg(theme.panel_bg).bg(ruler_fg),
                ));
            }
            if !after.is_empty() {
                spans.push(Span::styled(after, base));
            }
            frame.draw_line(
                ruler_area,
                0,
                &Line::from(spans),
                Style::default().bg(theme.panel_bg),
                Alignment::Left,
            );
        }
    }

    render_editor_buffer(
        frame,
        text_area,
        &editor.buffer,
        editor.scroll,
        theme,
        focused,
        model.ui.show_line_numbers,
    );
}

/// The gutters between windows: a dim border column and a plain rule for
/// stacked windows. Each pane names its own page in its title row, so
/// separators carry no repeated titles.
fn draw_window_separators(frame: &mut Frame<'_>, rects: &[Rect], theme: ThemePalette) {
    let style = Style::default().fg(theme.border).bg(theme.panel_bg);
    for rect in rects {
        // A window directly right of this one gets the column between.
        if let Some(other) = rects.iter().find(|other| {
            other.x == rect.right().saturating_add(1)
                && other.y < rect.bottom()
                && rect.y < other.bottom()
        }) {
            let top = rect.y.max(other.y);
            let bottom = rect.bottom().min(other.bottom());
            for y in top..bottom {
                frame.draw_line(
                    Rect::new(rect.right(), y, 1, 1),
                    0,
                    &Line::from(Span::styled("│", style)),
                    style,
                    Alignment::Left,
                );
            }
        }
        // A window directly below gets the row between, as a plain rule.
        if let Some(other) = rects.iter().find(|other| {
            other.y == rect.bottom().saturating_add(1)
                && other.x < rect.right()
                && rect.x < other.right()
        }) {
            let left = rect.x.max(other.x);
            let width = rect.right().min(other.right()).saturating_sub(left);
            if width == 0 {
                continue;
            }
            frame.draw_line(
                Rect::new(left, rect.bottom(), width, 1),
                0,
                &Line::from(Span::styled("─".repeat(width as usize), style)),
                style,
                Alignment::Left,
            );
        }
    }
}

struct EditorPane;

impl Component<AppState, Command, AppMessage, Action> for EditorPane {
    fn render(&self, model: &AppState, frame: &mut Frame<'_>, area: Rect, focused: bool) {
        let theme = model.theme_palette();

        // Clear the entire editor area with background color to eliminate arbitrary gray spots
        frame.clear_rect(area, Style::default().bg(theme.panel_bg));

        let drafts = model.visible_window_drafts();
        let rects = &model.ui.layout.window_rects;
        let active = model.active_window_position();
        let show_titles = model.window_titles_visible();

        if drafts.len() < 2 || rects.len() < 2 {
            let draft = drafts.get(active).copied().unwrap_or(model.active_draft);
            render_editor_window(model, frame, area, draft, focused, theme, false);
            return;
        }

        for (position, rect) in rects.iter().enumerate() {
            let Some(draft) = drafts.get(position).copied() else {
                break;
            };
            if rect.width == 0 || rect.height == 0 {
                continue;
            }
            render_editor_window(
                model,
                frame,
                *rect,
                draft,
                position == active,
                theme,
                show_titles,
            );
        }
        draw_window_separators(frame, rects, theme);
    }

    fn handle_event(
        &mut self,
        model: &mut AppState,
        event: &UiEvent<Command, AppMessage>,
        cx: &mut EventCx<Action>,
    ) {
        // The text area, matching the draw path: `wrap_and_gutter` and every
        // buffered wrap query must agree on one width per frame, or each
        // keystroke flips the wrap cache width and re-wraps the document.
        let text_area = window_text_area(model, model.active_editor_area());
        let (width, _) = wrap_and_gutter(
            text_area.width as usize,
            &model.editor().buffer,
            model.ui.show_line_numbers,
        );
        let height = text_area.height.max(1) as usize;

        if let UiEvent::Command(cmd) = event {
            match cmd {
                Command::ToggleInsertMode => {
                    cx.emit(Action::ToggleInsertMode);
                    return;
                }
                Command::ToggleLineNumbers => {
                    cx.emit(Action::ToggleLineNumbers);
                    return;
                }
                Command::OpenFindPrompt => {
                    cx.emit(Action::FindRequested(None));
                    return;
                }
                Command::OpenReplacePrompt => {
                    cx.emit(Action::ReplaceRequested(None));
                    return;
                }
                Command::FindNext => {
                    cx.emit(Action::FindNextRequested);
                    return;
                }
                Command::OpenGoToLinePrompt => {
                    cx.emit(Action::GoToLineRequested);
                    return;
                }
                Command::OpenWriteBlockPrompt => {
                    cx.emit(Action::WriteBlockRequested);
                    return;
                }
                Command::OpenReadFilePrompt => {
                    cx.emit(Action::ReadFileRequested);
                    return;
                }
                Command::SaveAndClose => {
                    cx.emit(Action::SaveAndCloseRequested);
                    return;
                }
                Command::SaveAndExit => {
                    cx.emit(Action::SaveAndExitRequested);
                    return;
                }
                Command::CopySelection => {
                    cx.emit(Action::CopyRequested);
                    return;
                }
                Command::CutSelection => {
                    cx.emit(Action::CutRequested);
                    return;
                }
                Command::PasteClipboard => {
                    cx.emit(Action::PasteRequested);
                    return;
                }
                Command::MarkBlockBegin => {
                    let cursor = model.editor().buffer.cursor();
                    model.editor_mut().buffer.set_block_begin();
                    cx.emit(Action::SetStatus(format!(
                        "Block start marked at character {cursor}. Move the cursor, then press ⌃K B again."
                    )));
                    return;
                }
                Command::MarkBlockEnd => {
                    model.editor_mut().buffer.set_block_end();
                    let selected = model.editor().buffer.selected_text();
                    let chars = selected
                        .as_ref()
                        .map(|text| text.chars().count())
                        .unwrap_or(0);
                    let words = selected
                        .as_ref()
                        .map(|text| text.split_whitespace().count())
                        .unwrap_or(0);
                    cx.emit(Action::SetStatus(format!(
                        "Block marked, {chars} character(s), {words} word(s). ⌃K V copies it, ⌃K I writes it to a file."
                    )));
                    return;
                }
                Command::CopyBlock => {
                    if model.editor_mut().buffer.copy_block() {
                        model.editor_mut().mark_dirty();
                    } else {
                        cx.emit(Action::SetStatus(
                            "Mark a block first with ⌃K B.".to_string(),
                        ));
                    }
                    return;
                }
                Command::MoveBlock => {
                    if model.editor_mut().buffer.move_block() {
                        model.editor_mut().mark_dirty();
                    } else {
                        cx.emit(Action::SetStatus(
                            "Mark a block first with ⌃K B.".to_string(),
                        ));
                    }
                    return;
                }
                Command::DeleteBlock => {
                    if model.editor_mut().buffer.delete_block() {
                        model.editor_mut().mark_dirty();
                    } else {
                        cx.emit(Action::SetStatus(
                            "Mark a block first with ⌃K B.".to_string(),
                        ));
                    }
                    return;
                }
                Command::HideBlock => {
                    model.editor_mut().buffer.hide_block();
                    return;
                }
                Command::MoveToBlockStart => {
                    model.editor_mut().buffer.move_to_block_start();
                    return;
                }
                Command::MoveToBlockEnd => {
                    model.editor_mut().buffer.move_to_block_end();
                    return;
                }
                Command::MoveWindowTop => {
                    let scroll = model.editor().scroll;
                    model.editor_mut().buffer.move_to_screen_top(scroll, width);
                    return;
                }
                Command::MoveWindowBottom => {
                    let scroll = model.editor().scroll;
                    model
                        .editor_mut()
                        .buffer
                        .move_to_screen_bottom(scroll, height, width);
                    return;
                }
                _ => {}
            }
        }

        let overwrite_mode = !model.ui.insert_mode;
        let changed = handle_text_buffer_event(
            event,
            &mut model.editor_mut().buffer,
            width,
            height,
            overwrite_mode,
        );
        if changed {
            model.editor_mut().mark_dirty();
        }
        model.editor_mut().ensure_cursor_visible(width, height);
    }
}

pub(crate) fn editor_text_area(area: Rect) -> Rect {
    let margin = 4;
    Rect::new(
        area.x.saturating_add(margin),
        area.y.saturating_add(1),
        area.width.saturating_sub(margin * 2),
        area.height.saturating_sub(2),
    )
}

/// The text area of one window. Split panes spend their first row on a title
/// above the ruler, so input hit-testing, wrapping, and cursor bookkeeping
/// must use the same inset the renderer paints.
pub(crate) fn window_text_area(model: &AppState, area: Rect) -> Rect {
    if !model.window_titles_visible() {
        return editor_text_area(area);
    }
    editor_text_area(Rect::new(
        area.x,
        area.y.saturating_add(1),
        area.width,
        area.height.saturating_sub(1),
    ))
}

/// One ruler row: `├` on the left margin, `┤` on the right, `▶` on every
/// tab stop, `─` between. Widths below three degrade to the marks that still
/// fit; nothing panics or wraps.
fn ruler_marks(width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut marks = vec!['─'; width];
    marks[0] = '├';
    if width > 1 {
        marks[width - 1] = '┤';
    }
    // The right margin owns its cell: a tab stop landing there gives way.
    let mut column = TAB_STOP;
    while column + 1 < width {
        marks[column] = '▶';
        column += TAB_STOP;
    }
    marks.into_iter().collect()
}

/// Splits the ruler at the caret's column so the renderer can paint that
/// one cell inverted. The caret glyph is a plain `─` and overwrites whatever
/// sat there, whether margin or tab stop.
fn ruler_caret_split(width: usize, caret: Option<usize>) -> (String, Option<char>, String) {
    let marks = ruler_marks(width);
    let Some(column) = caret.filter(|column| *column < width) else {
        return (marks, None, String::new());
    };
    let cells: Vec<char> = marks.chars().collect();
    let before: String = cells[..column].iter().collect();
    let after: String = cells[column + 1..].iter().collect();
    (before, Some('─'), after)
}

/// A centered overlay dialog over the body. Every tool lives here: the
/// editor keeps the whole screen when no panel is open, and one panel at a
/// time covers it while open. Esc closes.
fn draw_header(model: &AppState, frame: &mut Frame<'_>, area: Rect, theme: ThemePalette) {
    if model.show_welcome_screen() {
        return;
    }
    // Alt-M hides the bar by taking its row away (`update_layout`); the
    // editor then owns the terminal's top line.
    if area.height == 0 || area.width == 0 {
        return;
    }

    // One row: the menu bar on the left, the page name centred in a single
    // window. Counts, mode and position live in the status row, so the bar
    // stays a bar.
    let cells = crate::state::menu_title_cells(area);
    let title_style = Style::default().bg(theme.header_bg).fg(theme.header_fg);
    let mut title_spans = vec![Span::styled(" ", Style::default().bg(theme.header_bg))];
    for (index, menu) in crate::commands::MENU_BAR.iter().enumerate() {
        if index >= cells.len() {
            break;
        }
        if index > 0 {
            title_spans.push(Span::styled("  ", Style::default().bg(theme.header_bg)));
        }
        // The open menu wears the same highlight as a selected dropdown
        // row, so the bar and the menu read as one control.
        if model.ui.menu_open == Some(index) {
            title_spans.push(Span::styled(
                menu.title.to_string(),
                Style::default()
                    .bg(theme.accent)
                    .fg(theme.panel_bg)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            title_spans.push(Span::styled(menu.title.to_string(), title_style));
        }
    }
    let menus_line = Line::from(title_spans);

    let base = Style::default().bg(theme.header_bg);
    frame.clear_rect(area, base);
    frame.draw_line(area, 0, &menus_line, base, Alignment::Left);

    // A split gives every pane its own title row. The header then stays app
    // chrome instead of naming one pane and making the others anonymous.
    if model.window_titles_visible() {
        return;
    }

    // A draft title can be a whole sentence: an untitled draft is named
    // after its first line. Cap it at 30 columns and centre it on the bar
    // itself, so the name sits in the middle of the terminal rather than
    // halfway through the room the menus leave. On a narrow bar the title
    // stops at the menu bar instead of painting over it.
    const TITLE_MAX_WIDTH: usize = 30;
    let editor = model.editor();
    let width = area.width as usize;
    let menus_end = menus_line.width();
    let left_edge = menus_end.saturating_add(1);
    let title_room = width.saturating_sub(left_edge);
    let title = fit_text_to_width_tail(&editor.title(), title_room.min(TITLE_MAX_WIDTH));
    let title_width = UnicodeWidthStr::width(title.as_str());
    let title_start = (width.saturating_sub(title_width) / 2).max(left_edge);
    let name_line = Line::from(vec![Span::styled(
        title,
        Style::default()
            .bg(theme.header_bg)
            .fg(theme.header_fg)
            .add_modifier(Modifier::BOLD),
    )]);
    if title_width > 0 {
        let name_area = Rect::new(area.x + title_start as u16, area.y, title_width as u16, 1);
        frame.draw_line(name_area, 0, &name_line, base, Alignment::Left);
    }
}

/// Split a text area's columns into wrap width and gutter width.
///
/// Every cursor, scroll, and render computation for the same buffer must share
/// the wrap width from here, or the cursor is placed against a different wrap
/// than the one on screen. The gutter holds right-aligned source line numbers
/// plus one space; when the area is too narrow for both, the gutter loses.
pub(crate) fn wrap_and_gutter(
    area_width: usize,
    buffer: &EditorBuffer,
    show_line_numbers: bool,
) -> (usize, usize) {
    let width = area_width.max(1);
    if !show_line_numbers {
        return (width, 0);
    }
    let gutter = buffer.source_line_count().max(1).to_string().len() + 1;
    if gutter >= width {
        (width, 0)
    } else {
        (width - gutter, gutter)
    }
}

#[allow(clippy::too_many_arguments)]
fn render_editor_buffer(
    frame: &mut Frame<'_>,
    area: Rect,
    buffer: &EditorBuffer,
    scroll: usize,
    theme: ThemePalette,
    focused: bool,
    show_line_numbers: bool,
) {
    if area.width < 2 || area.height < 1 {
        return;
    }

    // Gutter for source line numbers. It steals columns from the text, so the
    // wrap width shrinks with it and the cursor column stays text-relative.
    let (width, gutter_width) = wrap_and_gutter(area.width as usize, buffer, show_line_numbers);
    // Only wrap what is on screen. Asking for the whole document here would
    // rebuild a `WrappedLine` for every visual line on every frame. One line of
    // context above and below keeps `column_for_cursor` answerable.
    let rows = area.height as usize;
    let layout = buffer.visible_layout(width, scroll, rows, 1);
    let start = scroll.min(layout.total_lines());
    let selection = buffer.selection_range();
    let visible = layout.rows_from(start, rows);
    let cursor_line = buffer.visual_line_for_cursor(width, buffer.cursor());
    let lines = visible
        .iter()
        .enumerate()
        .map(|(offset, line)| {
            let index = start + offset;
            let mut rendered = render_wrapped_line(line, buffer, selection, focused, theme);
            if gutter_width > 0 {
                let (number, first) = buffer.source_line_info(line.start_char);
                let gutter_text = if first {
                    let padding = gutter_width.saturating_sub(1);
                    format!("{number:>padding$} ")
                } else {
                    " ".repeat(gutter_width)
                };
                let gutter_style = if index == cursor_line && focused {
                    Style::default()
                        .fg(theme.accent)
                        .bg(theme.cursor_line_bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.muted_fg).bg(theme.panel_bg)
                };
                rendered
                    .spans
                    .insert(0, Span::styled(gutter_text, gutter_style));
            }
            rendered
        })
        .collect::<Vec<_>>();
    frame.clear_rect(area, Style::default().bg(theme.panel_bg).fg(theme.text_fg));
    frame.draw_lines(
        area,
        &lines,
        Style::default().bg(theme.panel_bg).fg(theme.text_fg),
        Alignment::Left,
    );
    if focused && cursor_line >= start && cursor_line < start + visible.len() {
        let cursor_x = layout.column_for_cursor(cursor_line, buffer.cursor()) as u16;
        let cursor_y = (cursor_line - start) as u16;
        frame.set_cursor((
            area.x.saturating_add(cursor_x + gutter_width as u16),
            area.y.saturating_add(cursor_y),
        ));
    }
}

fn render_notes_buffer(
    frame: &mut Frame<'_>,
    area: Rect,
    notes: &NotesPaneState,
    theme: ThemePalette,
    focused: bool,
    show_line_numbers: bool,
) {
    render_editor_buffer(
        frame,
        area,
        &notes.buffer,
        notes.scroll,
        ThemePalette {
            panel_bg: theme.panel_alt_bg,
            panel_alt_bg: theme.panel_alt_bg,
            cursor_line_bg: theme.selected_bg,
            ..theme
        },
        focused,
        show_line_numbers,
    );
}

/// Widest chord in the welcome rail's tables. The key column is this plus a
/// gap, so descriptions start right after the keys instead of after a fixed
/// gutter the keys never fill.
fn help_key_width() -> usize {
    EDITOR_HELP_SECTIONS
        .iter()
        .flat_map(|section| section.entries)
        .map(|entry| UnicodeWidthStr::width(entry.keys))
        .max()
        .unwrap_or(0)
}

/// Column layout shared by the welcome rail's tables, so keys stay left and
/// descriptions line up in one column while the list scrolls.
fn help_columns(width: usize) -> (usize, usize) {
    let width = width.max(1);
    let key_column = (help_key_width() + 2).min(width.saturating_sub(12).max(8));
    (key_column, width.saturating_sub(key_column).max(1))
}

/// The Help tab that documents `command`, if any. `F1` then the command's
/// key opens that page instead of running the command.
pub(crate) fn help_jump_target(command: Command) -> Option<HelpTab> {
    EDITOR_HELP_SECTIONS
        .iter()
        .find(|section| {
            section
                .entries
                .iter()
                .any(|entry| entry.commands.contains(&command))
        })
        .map(|section| section.tab)
}

/// Commands that keep their meaning while the help panel is open: Tab,
/// Shift+Tab and the horizontal arrows change sections instead of looking
/// themselves up. The vertical/page keys remain inert here but stay in the
/// navigator set so they cannot trigger a command lookup.
pub(crate) fn help_panel_navigation(command: Command) -> bool {
    matches!(
        command,
        Command::InsertTab
            | Command::Outdent
            | Command::MoveLeft
            | Command::MoveRight
            | Command::MoveUp
            | Command::MoveDown
            | Command::SelectUp
            | Command::SelectDown
            | Command::MoveHome
            | Command::MoveEnd
            | Command::PageUp
            | Command::PageDown
    )
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
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

fn blend_color(start: Color, end: Color, amount: f32) -> Color {
    let amount = amount.clamp(0.0, 1.0);
    let (sr, sg, sb) = color_components(start);
    let (er, eg, eb) = color_components(end);
    Color::Rgb(
        ((sr as f32) + ((er as f32 - sr as f32) * amount)).round() as u8,
        ((sg as f32) + ((eg as f32 - sg as f32) * amount)).round() as u8,
        ((sb as f32) + ((eb as f32 - sb as f32) * amount)).round() as u8,
    )
}

fn color_components(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Black => (0, 0, 0),
        Color::White => (255, 255, 255),
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Reset => (255, 255, 255),
    }
}

fn fit_text_to_width(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }

    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }

    let ellipsis = "...";
    let ellipsis_width = UnicodeWidthStr::width(ellipsis);
    if max_width <= ellipsis_width {
        return ".".repeat(max_width);
    }

    let target = max_width.saturating_sub(ellipsis_width);
    let suffix_target = (target / 2).max(1);
    let prefix_target = target.saturating_sub(suffix_target).max(1);

    let prefix = take_prefix_width(text, prefix_target);
    let suffix = take_suffix_width(text, suffix_target);
    format!("{prefix}{ellipsis}{suffix}")
}

fn fit_text_to_width_tail(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }

    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }

    let ellipsis = "...";
    let ellipsis_width = UnicodeWidthStr::width(ellipsis);
    if max_width <= ellipsis_width {
        return ".".repeat(max_width);
    }

    let prefix = take_prefix_width(text, max_width.saturating_sub(ellipsis_width));
    format!("{prefix}{ellipsis}")
}

fn take_prefix_width(text: &str, max_width: usize) -> String {
    let mut width = 0usize;
    let mut result = String::new();
    for ch in text.chars() {
        let ch_width = ch.width().unwrap_or(0).max(1);
        if width + ch_width > max_width {
            break;
        }
        result.push(ch);
        width += ch_width;
    }
    result
}

fn take_suffix_width(text: &str, max_width: usize) -> String {
    let mut width = 0usize;
    let mut reversed = Vec::new();
    for ch in text.chars().rev() {
        let ch_width = ch.width().unwrap_or(0).max(1);
        if width + ch_width > max_width {
            break;
        }
        reversed.push(ch);
        width += ch_width;
    }
    reversed.into_iter().rev().collect()
}

fn header_path_label(path: Option<&Path>) -> String {
    let Some(path) = path else {
        return "[scratch buffer]".to_string();
    };

    let file = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty());
    let folder = path
        .parent()
        .and_then(|parent| parent.file_name())
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty());

    match (folder, file) {
        (Some(folder), Some(file)) => format!("{folder}/{file}"),
        (_, Some(file)) => file,
        _ => path.display().to_string(),
    }
}

fn stat_line(label: &str, value: impl ToString, theme: ThemePalette) -> Line {
    stat_line_on(label, value, theme, theme.panel_alt_bg)
}

fn stat_line_on(label: &str, value: impl ToString, theme: ThemePalette, background: Color) -> Line {
    const STAT_LABEL_WIDTH: usize = 14;

    Line::from(vec![
        Span::styled(
            format!("{label:<STAT_LABEL_WIDTH$}"),
            Style::default().fg(theme.muted_fg).bg(background),
        ),
        Span::styled(
            value.to_string(),
            Style::default()
                .fg(theme.text_fg)
                .bg(background)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

fn progress_bar_line(ratio: f32, theme: ThemePalette, width: usize) -> Line {
    progress_bar_line_on(ratio, theme, width, theme.panel_alt_bg)
}

fn progress_bar_line_on(ratio: f32, theme: ThemePalette, width: usize, background: Color) -> Line {
    let bar_width = width.clamp(10, 24);
    let clamped = ratio.clamp(0.0, 1.2);
    let filled = ((bar_width as f32) * clamped.min(1.0)).round() as usize;
    let overflow = ratio > 1.0;
    let fill_char = if overflow { '!' } else { '#' };
    let filled_text = fill_char.to_string().repeat(filled.min(bar_width));
    let empty_text = "-".repeat(bar_width.saturating_sub(filled.min(bar_width)));

    Line::from(vec![
        Span::styled(
            "[".to_string(),
            Style::default().fg(theme.muted_fg).bg(background),
        ),
        Span::styled(
            filled_text,
            Style::default()
                .fg(if overflow { theme.warn } else { theme.accent })
                .bg(background)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(empty_text, Style::default().fg(theme.border).bg(background)),
        Span::styled(
            "]".to_string(),
            Style::default().fg(theme.muted_fg).bg(background),
        ),
    ])
}

#[derive(Clone, Copy)]
struct WelcomeLayout {
    shell: Rect,
    chrome_area: Rect,
    strip_area: Rect,
    logo_area: Rect,
    menu_area: Rect,
    rail_area: Option<Rect>,
    side_panel_area: Option<Rect>,
    help_area: Rect,
    footer_area: Rect,
}

#[cfg(test)]
mod welcome_path_directory_tests {
    use super::welcome::welcome_path_directory;
    use std::path::Path;

    #[test]
    fn directory_splits_off_the_filename() {
        assert_eq!(
            welcome_path_directory(Path::new("/Users/me/docs/atomic.ord")),
            Some("/Users/me/docs".to_string())
        );
    }

    #[test]
    fn bare_filename_has_no_directory_line() {
        assert_eq!(welcome_path_directory(Path::new("atomic.ord")), None);
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_text_buffer_event(
    event: &UiEvent<Command, AppMessage>,
    buffer: &mut EditorBuffer,
    width: usize,
    height: usize,
    overwrite: bool,
) -> bool {
    match event {
        UiEvent::Command(command) => handle_text_buffer_command(command, buffer, width, height),
        UiEvent::Key(key) => {
            if let crossterm::event::KeyCode::Char(ch) = key.code {
                if !key.modifiers.intersects(
                    crossterm::event::KeyModifiers::CONTROL
                        | crossterm::event::KeyModifiers::ALT
                        | crossterm::event::KeyModifiers::SUPER,
                ) {
                    buffer.insert_char_with_mode(ch, overwrite);
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

fn handle_text_buffer_command(
    command: &Command,
    buffer: &mut EditorBuffer,
    width: usize,
    height: usize,
) -> bool {
    match command {
        Command::MoveLeft => {
            buffer.move_left(false);
            false
        }
        Command::MoveRight => {
            buffer.move_right(false);
            false
        }
        Command::MoveUp => {
            buffer.move_up(width, false);
            false
        }
        Command::MoveDown => {
            buffer.move_down(width, false);
            false
        }
        Command::MoveWordLeft => {
            buffer.move_word_left(false);
            false
        }
        Command::MoveWordRight => {
            buffer.move_word_right(false);
            false
        }
        Command::SelectLeft => {
            buffer.move_left(true);
            false
        }
        Command::SelectRight => {
            buffer.move_right(true);
            false
        }
        Command::SelectUp => {
            buffer.move_up(width, true);
            false
        }
        Command::SelectDown => {
            buffer.move_down(width, true);
            false
        }
        Command::MoveHome => {
            buffer.move_home(width, false);
            false
        }
        Command::MoveEnd => {
            buffer.move_end(width, false);
            false
        }
        Command::PageUp => {
            for _ in 0..height {
                buffer.move_up(width, false);
            }
            false
        }
        Command::PageDown => {
            for _ in 0..height {
                buffer.move_down(width, false);
            }
            false
        }
        Command::Backspace => buffer.backspace(),
        Command::DeleteForward => buffer.delete_forward(),
        Command::DeleteWordForward => buffer.delete_word_forward(),
        Command::DeleteWordLeft => buffer.delete_word_left(),
        Command::DeleteLine => buffer.delete_line(),
        Command::OpenLine => buffer.open_line(),
        Command::MoveDocumentTop => {
            buffer.move_to_top();
            false
        }
        Command::MoveDocumentBottom => {
            buffer.move_cursor_to_end();
            false
        }
        Command::DeleteToEndOfLine => buffer.delete_to_end_of_line(width),
        Command::JumpPreviousPosition => {
            buffer.jump_to_previous_cursor();
            false
        }
        Command::Submit => {
            buffer.insert_newline_with_indent();
            true
        }
        Command::InsertTab => buffer.indent(),
        Command::Outdent => buffer.outdent(),
        Command::CycleCase => buffer.cycle_case(),
        Command::ToggleBold => buffer.toggle_attr(AttrKind::Bold),
        Command::ToggleItalic => buffer.toggle_attr(AttrKind::Italic),
        Command::BlockTitle => buffer.apply_block_prefix(crate::editor_buffer::BlockPrefix::Title),
        Command::BlockHeading => {
            buffer.apply_block_prefix(crate::editor_buffer::BlockPrefix::Heading)
        }
        Command::BlockQuote => buffer.apply_block_prefix(crate::editor_buffer::BlockPrefix::Quote),
        Command::BlockBody => buffer.apply_block_prefix(crate::editor_buffer::BlockPrefix::Body),
        Command::InsertTimestamp => {
            let stamp = crate::app::helpers::local_timestamp();
            if stamp.is_empty() {
                false
            } else {
                buffer.insert_text(&stamp);
                true
            }
        }
        Command::Undo => buffer.undo(),
        Command::Redo => buffer.redo(),
        Command::SelectAll => {
            buffer.select_all();
            false
        }
        _ => false,
    }
}

fn render_wrapped_line(
    line: &WrappedLine,
    buffer: &EditorBuffer,
    selection: Option<(usize, usize)>,
    focused: bool,
    theme: ThemePalette,
) -> Line {
    // No cursor-line wash: the hardware caret alone marks the row. A
    // full-row background kept reading as a selection following the cursor.
    let background = theme.panel_bg;
    let base = Style::default().fg(theme.text_fg).bg(background);
    let selection_style = Style::default()
        .fg(if focused {
            theme.selection_fg
        } else {
            theme.text_fg
        })
        .bg(theme.selection_bg);

    let total_chars = line.text.chars().count();
    if total_chars == 0 {
        // An empty row paints a plain placeholder: never the cursor-line
        // background, which reads as a selected invisible space on a fresh
        // line. A selection covering the row still highlights it.
        let in_selection = selection.is_some_and(|(sel_start, sel_end)| {
            sel_start <= line.start_char && sel_end >= line.end_char
        });
        let style = if in_selection {
            selection_style
        } else {
            Style::default().fg(theme.text_fg).bg(theme.panel_bg)
        };
        return Line::from(Span::styled(" ", style));
    }
    // Cuts live in buffer-char space: display_offsets holds one entry per
    // buffer char in this row (hidden wrap-whitespace maps to zero-width
    // columns), while line.text is visible text plus the alignment
    // padding that leads it.
    let buf_len = line.end_char.saturating_sub(line.start_char);
    let max_offset = line.display_offsets.len().saturating_sub(1);
    let display_col = |local: usize| {
        line.display_offsets
            .get(local.min(max_offset).min(buf_len))
            .copied()
            .unwrap_or(total_chars)
    };

    // Cut points: selection edges plus every styled boundary crossing this
    // row. Each run between cuts has one style.
    let mut cuts = vec![0, buf_len];
    if let Some((selection_start, selection_end)) = selection {
        if selection_end > line.start_char && selection_start < line.end_char {
            cuts.push(selection_start.saturating_sub(line.start_char).min(buf_len));
            cuts.push(
                selection_end
                    .min(line.end_char)
                    .saturating_sub(line.start_char)
                    .min(buf_len),
            );
        }
    }
    for (run_start, run_end, _, _) in buffer.attr_runs_in(line.start_char, line.end_char) {
        cuts.push(run_start.saturating_sub(line.start_char).min(buf_len));
        cuts.push(run_end.saturating_sub(line.start_char).min(buf_len));
    }
    cuts.sort_unstable();
    cuts.dedup();

    let mut spans = Vec::new();
    // The alignment padding leads `line.text` but sits outside the
    // buffer-char range, so the runs below would slice it away and slide
    // centered text back to the left margin while the caret stayed where
    // the alignment put it. Paint the padding as its own run.
    let padding: String = line.text.chars().take(display_col(0)).collect();
    if !padding.is_empty() {
        spans.push(Span::styled(padding, base));
    }
    for pair in cuts.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        if from >= to {
            continue;
        }
        let buf_start = line.start_char + from;
        let buf_end = line.start_char + to;
        let selected = selection
            .is_some_and(|(sel_start, sel_end)| buf_start < sel_end && buf_end > sel_start);
        let (mut bold, mut italic) = (false, false);
        for (_, _, run_bold, run_italic) in buffer.attr_runs_in(buf_start, buf_end) {
            bold |= run_bold;
            italic |= run_italic;
        }
        let mut style = if selected { selection_style } else { base };
        if bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if italic {
            style = style.add_modifier(Modifier::ITALIC);
        }
        let from_col = display_col(from).min(total_chars);
        let to_col = display_col(to).min(total_chars).max(from_col);
        let text = slice_chars(&line.text, from_col, to_col);
        if !text.is_empty() {
            spans.push(Span::styled(text, style));
        }
    }
    if spans.is_empty() {
        spans.push(Span::styled(" ", base));
    }
    Line::from(spans)
}

fn wrap_lines(
    text: &str,
    width: usize,
    fg: ordet_tui_framework::Color,
    bg: ordet_tui_framework::Color,
) -> Vec<Line> {
    wrap_text(text, width)
        .into_iter()
        .map(|line| Line::from(Span::styled(line, Style::default().fg(fg).bg(bg))))
        .collect()
}

fn wrap_prefixed_lines(
    prefix: &str,
    text: &str,
    width: usize,
    fg: ordet_tui_framework::Color,
    bg: ordet_tui_framework::Color,
) -> Vec<Line> {
    let available = width.max(4);
    let prefix_width = UnicodeWidthStr::width(prefix).min(available.saturating_sub(1));
    let body_width = available.saturating_sub(prefix_width).max(1);
    let wrapped = wrap_text(text, body_width);
    if wrapped.is_empty() {
        return vec![Line::from(Span::styled(
            prefix.to_string(),
            Style::default().fg(fg).bg(bg),
        ))];
    }

    wrapped
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let left = if index == 0 {
                prefix.to_string()
            } else {
                " ".repeat(prefix_width)
            };
            Line::from(vec![
                Span::styled(left, Style::default().fg(fg).bg(bg)),
                Span::styled(line, Style::default().fg(fg).bg(bg)),
            ])
        })
        .collect()
}

fn slice_chars(text: &str, start: usize, end: usize) -> String {
    text.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests;
