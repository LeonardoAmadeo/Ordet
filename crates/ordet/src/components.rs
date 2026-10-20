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
struct PanelView;

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
fn stats_panel_rows(model: &AppState) -> u16 {
    if model.word_target_progress().is_some() {
        11
    } else {
        8
    }
}

/// Rows the settings list paints at this width: one per row, one per section
/// heading, and one per wrapped value line, the same walk the renderer does.
fn settings_panel_rows(model: &AppState, content_width: u16) -> u16 {
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
fn revisions_panel_rows(model: &AppState, width: usize) -> u16 {
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
fn help_panel_rows() -> usize {
    let entries = EDITOR_HELP_SECTIONS
        .iter()
        .map(|section| section.entries.len())
        .max()
        .unwrap_or(0);
    entries + 4
}

/// Width of the Pages card: the widest `n. label` row plus its border, or
/// the empty-state line, whichever is longer. Never wider than the cap.
fn pages_panel_width(model: &AppState) -> u16 {
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
fn workspace_panel_width(model: &AppState) -> u16 {
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
const PANEL_BORDER_COLUMNS: usize = 2;

/// Width of the Help card: the widest of the tab bar, the pinned footer, and
/// an action/shortcut row. The shortcut column is right-aligned inside this
/// width, so the card does not resize when switching tabs.
fn help_panel_width() -> u16 {
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
fn help_tab_bar_width() -> usize {
    HelpTab::ALL
        .iter()
        .map(|tab| UnicodeWidthStr::width(tab.title()) + 2)
        .sum::<usize>()
        + 2
}

const HELP_FOOTER_TEXT: &str = "[Tab / ←→]: Switch Section [Esc]: Close.";

fn help_footer_width() -> usize {
    UnicodeWidthStr::width(HELP_FOOTER_TEXT)
}

/// Width of the Revisions card: its empty-state sentence is the widest fixed
/// line it prints, so the card is sized to it. Snapshots, the hint and the
/// diff all wrap inside, and the card never twitches between saves.
fn revisions_panel_width() -> u16 {
    (UnicodeWidthStr::width(REVISIONS_EMPTY_HINT) + PANEL_BORDER_COLUMNS) as u16
}

/// Width of the Notes card: the widest line in the notes buffer, floored so
/// an empty page still opens a writing column instead of a title-sized
/// sliver. A longer line widens the card until the cap.
fn notes_panel_width(model: &AppState) -> u16 {
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
    fn render(&self, model: &AppState, frame: &mut Frame<'_>, theme: ThemePalette) {
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
fn draw_panel_frame(frame: &mut Frame<'_>, outer: Rect, title: &str, theme: ThemePalette) -> Rect {
    let fill = Style::default().bg(theme.panel_alt_bg);
    let border = Style::default().fg(theme.border).bg(theme.panel_alt_bg);
    let inner = frame.draw_border(outer, None, fill, border);
    draw_centered_border_title(frame, outer, title, theme.accent, theme.panel_alt_bg);
    inner
}

/// Paint a border title centred in the top edge, with a clean space on each
/// side so no extra border stroke is quoted into the name:
/// `┌───── Help ─────┐`.
fn draw_centered_border_title(
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
fn render_pages_panel(frame: &mut Frame<'_>, model: &AppState, theme: ThemePalette) {
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
fn render_workspace_panel(frame: &mut Frame<'_>, model: &AppState, theme: ThemePalette) {
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
fn status_wrap_width(model: &AppState) -> usize {
    let area = model
        .ui
        .layout
        .window_rects
        .get(model.active_window_position())
        .copied()
        .unwrap_or(model.ui.layout.editor);
    let text_area = window_text_area(model, area);
    let editor = model.editor();
    let (width, _) = wrap_and_gutter(
        text_area.width as usize,
        &editor.buffer,
        model.ui.show_line_numbers,
    );
    width
}

/// The state cluster on the right of the status row: block mark,
/// insert/overwrite, work in flight, page, position, the
/// optional word target and the optional live counts. Fields are ordered
/// least-important first so a narrow row can shed the leading ones.
fn status_fields(model: &AppState, theme: ThemePalette) -> Vec<(String, Color)> {
    let editor = model.editor();
    let mut fields: Vec<(String, Color)> = Vec::new();

    // A marked block, shown as `<B>`. The end of the block is wherever the
    // cursor is, so one marker says it all.
    if editor.buffer.selection_range().is_some() {
        fields.push(("<B>".to_string(), theme.bar_emphasis_fg(theme.accent)));
    }
    let mode = if model.ui.insert_mode {
        "Insert"
    } else {
        "Overwrite"
    };
    fields.push((mode.to_string(), theme.bar_muted_fg()));
    // Work in flight, so a slow save or open stays visible without a menu.
    let task = if model.ui.pending_open {
        Some("open")
    } else if editor.pending_save {
        Some("save")
    } else {
        None
    };
    if let Some(task) = task {
        fields.push((task.to_string(), theme.bar_emphasis_fg(theme.accent)));
    }
    let page = match model.active_page_number() {
        Some(page) => format!("Pg {page}/{}", model.visible_draft_count()),
        None => "Pg -".to_string(),
    };
    fields.push((page, theme.bar_muted_fg()));
    let (line, column) = editor.buffer.cursor_line_and_col(status_wrap_width(model));
    fields.push((format!("L{line} C{column}"), theme.bar_muted_fg()));
    // A word target is opt-in, so the slot only exists once one is set.
    if let Some(progress) = model.word_target_progress() {
        let fg = if progress.met {
            theme.bar_emphasis_fg(theme.accent)
        } else {
            theme.bar_muted_fg()
        };
        fields.push((
            format!("target: {}/{}", progress.current, progress.target),
            fg,
        ));
    }
    if model.ui.show_live_counts {
        let stats = editor.word_stats();
        fields.push((
            format!("W:{} C:{}", stats.word_count, stats.chars),
            theme.bar_muted_fg(),
        ));
    }
    if model.window_count() > 1 {
        fields.push((
            format!(
                "W{}/{}",
                model.active_window_position() + 1,
                model.window_count()
            ),
            theme.muted_fg,
        ));
    }
    fields
}

/// Joins cluster fields with `│` separators, and closes the row with a
/// one-cell margin.
fn status_line(fields: &[(String, Color)], background: Color) -> Line {
    let mut spans = Vec::with_capacity(fields.len() * 2 + 1);
    for (index, (text, fg)) in fields.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" │ ", Style::default().bg(background)));
        }
        spans.push(Span::styled(
            text.clone(),
            Style::default().bg(background).fg(*fg),
        ));
    }
    spans.push(Span::styled(" ", Style::default().bg(background)));
    Line::from(spans)
}

/// Fits a status message into a fixed width the shell way: `$HOME` becomes
/// `~`, and overflow keeps the tail (filename end) instead of splitting a
/// directory name in half.
fn fit_status_message(message: &str, home_prefix: Option<&str>, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    let shortened = match home_prefix {
        Some(prefix) if !prefix.is_empty() => message.replacen(prefix, "~/", 1),
        _ => message.to_string(),
    };
    let text = format!(" {shortened} ");
    if UnicodeWidthStr::width(text.as_str()) <= max_width {
        return text;
    }
    if max_width <= 4 {
        return take_prefix_width(&text, max_width);
    }
    let tail = take_suffix_width(&shortened, max_width.saturating_sub(1 + 3));
    format!(" ...{tail}")
}

/// A prompt as one status row. When the row is narrow the value (a path or a
/// query) gives way first, keeping the end that is being typed, so the label
/// and the key hint always survive; prompts live on the bottom line.
fn prompt_status_text(prompt: &crate::prompt::PromptState, width: usize) -> String {
    let full = prompt.render_status();
    if width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(full.as_str()) <= width {
        return full;
    }
    let fixed = format!("{} >   [{}]", prompt.label, prompt.hint);
    let fixed_width = UnicodeWidthStr::width(fixed.as_str());
    if width <= fixed_width + 1 {
        return take_prefix_width(&full, width);
    }
    let value = take_suffix_width(&prompt.value, width - fixed_width);
    format!("{} > {}  [{}]", prompt.label, value, prompt.hint)
}

struct StatusBar;

impl Component<AppState, Command, AppMessage, Action> for StatusBar {
    fn render(&self, model: &AppState, frame: &mut Frame<'_>, area: Rect, _focused: bool) {
        let theme = model.theme_palette();
        if area.width == 0 || area.height == 0 {
            return;
        }

        let welcome_screen = model.show_welcome_screen();
        if welcome_screen && model.ui.prompt.is_none() {
            frame.clear_rect(area, Style::default().bg(theme.panel_bg));
            return;
        }

        let background = theme.header_bg;
        frame.clear_rect(area, Style::default().bg(background));

        if let Some(prompt) = &model.ui.prompt {
            let lines = wrap_lines(
                &prompt_status_text(prompt, area.width as usize),
                area.width as usize,
                theme.header_fg,
                background,
            );
            frame.draw_lines(
                area,
                &lines,
                Style::default().bg(background),
                Alignment::Left,
            );
            return;
        }

        if let Some(_pending) = &model.ui.quit_confirmation {
            let message =
                "This draft contains unsaved edits. Do you want to save it? Y/N".to_string();
            let lines = wrap_lines(&message, area.width as usize, theme.header_fg, background);
            frame.draw_lines(
                area,
                &lines,
                Style::default().bg(background),
                Alignment::Left,
            );
            return;
        }

        let left_msg = model.ui.status.as_str();

        // One row: the transient message on the left, the editor state
        // cluster flush right. A narrow row sheds
        // the cluster's leading fields before they would push the message
        // off the row, so nothing ever clips.
        let mut fields = status_fields(model, theme);
        let mut cluster = status_line(&fields, background);
        while !fields.is_empty() && cluster.width() > area.width as usize {
            fields.remove(0);
            cluster = status_line(&fields, background);
        }
        let message_width = (area.width as usize).saturating_sub(cluster.width() + 1);
        if message_width > 1 && !left_msg.is_empty() {
            let home_prefix = dirs::home_dir().map(|home| {
                let mut prefix = home.to_string_lossy().into_owned();
                if !prefix.ends_with(std::path::MAIN_SEPARATOR) {
                    prefix.push(std::path::MAIN_SEPARATOR);
                }
                prefix
            });
            let message = fit_status_message(left_msg, home_prefix.as_deref(), message_width);
            frame.draw_line(
                Rect::new(area.x, area.y, message_width as u16, 1),
                0,
                &Line::from(Span::styled(
                    message,
                    Style::default()
                        .bg(background)
                        .fg(theme.bar_emphasis_fg(theme.warn))
                        .add_modifier(Modifier::BOLD),
                )),
                Style::default().bg(background),
                Alignment::Left,
            );
        }
        if cluster.width() > 0 {
            frame.draw_line(
                area,
                0,
                &cluster,
                Style::default().bg(background),
                Alignment::Right,
            );
        }
    }

    fn handle_event(
        &mut self,
        _model: &mut AppState,
        _event: &UiEvent<Command, AppMessage>,
        _cx: &mut EventCx<Action>,
    ) {
    }
}

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

/// The open menu's dropdown, hanging off its title over the body. Rows
/// highlight like the pickers; the renderer, keys and mouse share the rect
/// math in `menu_drop_rect` / `menu_drop_inner_rect` so a click runs the
/// painted row.
///
/// The dropdown owns a bordered box (same chrome as the pickers and file
/// browser). Without the border the menu's dark fill sits directly against
/// the editor's dark fill and same-row editor cells left/right of the menu
/// read as one merged line.
fn render_menu_dropdown(frame: &mut Frame<'_>, model: &AppState, open: usize, theme: ThemePalette) {
    if crate::commands::MENU_BAR.get(open).is_none() {
        return;
    }
    let items = crate::commands::menu_items(open, model.window_count());
    let header = model.ui.layout.header;
    let outer = crate::state::menu_drop_rect(header, model.ui.layout.status.y, open, items.len());
    if items.is_empty() || outer.width == 0 || outer.height == 0 {
        return;
    }
    // Solid opaque sheet: the same fill as the picker modals, with a
    // border, so it never reads as merged editor text.
    // See `ThemePalette::menu_bg`.
    let menu_bg = theme.menu_bg();
    let fill = Style::default().bg(menu_bg);
    let border_style = Style::default().fg(theme.border).bg(menu_bg);
    // Tiny screens cannot fit a border plus a row: fall back to a borderless
    // box so the menu stays reachable instead of vanishing.
    let inner = if outer.width >= 3 && outer.height >= 3 {
        frame.draw_border(outer, None, fill, border_style)
    } else {
        frame.clear_rect(outer, fill);
        outer
    };
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let selected = model.ui.menu_selected.min(items.len() - 1);
    let scroll = model.ui.menu_scroll.min(items.len().saturating_sub(1));
    let pad_style = Style::default().bg(menu_bg);
    let mut lines: Vec<Line> = Vec::new();
    for (idx, item) in items
        .iter()
        .enumerate()
        .skip(scroll)
        .take(inner.height as usize)
    {
        // 1-cell left/right padding in menu bg so the selection block never
        // touches the border. Middle (`label + 2 spaces + hint + filler`)
        // carries the row bg; pads stay menu bg on every row.
        let middle_width = (inner.width as usize).saturating_sub(2);
        let middle_len = UnicodeWidthStr::width(item.label) + 2 + UnicodeWidthStr::width(item.hint);
        let filler = middle_width.saturating_sub(middle_len);
        let row_bg = if idx == selected {
            theme.accent
        } else {
            menu_bg
        };
        let (label_style, hint_style) = if idx == selected {
            (
                Style::default()
                    .fg(theme.panel_bg)
                    .bg(theme.accent)
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(theme.panel_bg).bg(theme.accent),
            )
        } else {
            (
                Style::default().fg(theme.text_fg).bg(menu_bg),
                Style::default().fg(theme.muted_fg).bg(menu_bg),
            )
        };
        let mut spans = vec![
            Span::styled(" ", pad_style),
            Span::styled(item.label.to_string(), label_style),
            Span::styled("  ", Style::default().bg(row_bg)),
            Span::styled(item.hint.to_string(), hint_style),
        ];
        if filler > 0 {
            spans.push(Span::styled(
                " ".repeat(filler),
                Style::default().bg(row_bg),
            ));
        }
        spans.push(Span::styled(" ", pad_style));
        lines.push(Line::from(spans));
    }
    frame.clear_rect(inner, fill);
    frame.draw_lines(
        inner,
        &lines,
        Style::default().bg(menu_bg).fg(theme.text_fg),
        Alignment::Left,
    );
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

#[derive(Clone, Copy)]
struct HelpEntry {
    commands: &'static [Command],
    keys: &'static str,
    description: &'static str,
}

#[derive(Clone, Copy)]
struct HelpSection {
    tab: HelpTab,
    entries: &'static [HelpEntry],
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

const EDITOR_WORKSPACE_HELP: &[HelpEntry] = &[
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

const EDITOR_HELP_SECTIONS: &[HelpSection] = &[
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

fn help_entries(tab: HelpTab) -> &'static [HelpEntry] {
    EDITOR_HELP_SECTIONS
        .iter()
        .find(|section| section.tab == tab)
        .map(|section| section.entries)
        .unwrap_or(&[])
}

const WELCOME_MENU_HELP: &[HelpEntry] = &[
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

fn render_help_panel(frame: &mut Frame<'_>, area: Rect, model: &AppState, theme: ThemePalette) {
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
fn help_tab_bar(selected: HelpTab, theme: ThemePalette) -> Line {
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
fn help_footer_line(theme: ThemePalette) -> Line {
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

fn help_section_title(title: &str, theme: ThemePalette, background: Color) -> Line {
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
struct SettingsRow {
    section: &'static str,
    label: &'static str,
    value: String,
    action: Action,
}

fn settings_rows(model: &AppState) -> Vec<SettingsRow> {
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

fn render_settings_panel(frame: &mut Frame<'_>, area: Rect, model: &AppState, theme: ThemePalette) {
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
fn split_to_width(text: &str, max_width: usize) -> Vec<String> {
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
fn revisions_for_active_file(model: &AppState) -> Vec<Revision> {
    model
        .editor()
        .file_path
        .as_ref()
        .map(|path| model.history.revisions_for(&history_key_for(path)).to_vec())
        .unwrap_or_default()
}

fn revision_count_for(model: &AppState) -> usize {
    revisions_for_active_file(model).len()
}

/// Empty-state copy of the Revisions card; also the widest fixed line it
/// prints, so `revisions_panel_width` sizes the card to it.
const REVISIONS_EMPTY_HINT: &str = "Save the draft; every save keeps a snapshot.";

fn render_revisions_panel(
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

fn render_settings_picker_modal(
    frame: &mut Frame<'_>,
    area: Rect,
    _model: &AppState,
    picker: &SettingsPickerState,
    theme: ThemePalette,
) {
    let modal_rect = crate::state::settings_picker_modal_rect(area, picker.options.len());
    if modal_rect.width < 3 || modal_rect.height < 3 {
        return;
    }
    let inner = frame.draw_border(
        modal_rect,
        None,
        Style::default().bg(theme.panel_alt_bg),
        Style::default().fg(theme.border).bg(theme.panel_alt_bg),
    );
    frame.clear_rect(inner, Style::default().bg(theme.panel_alt_bg));

    let width = inner.width as usize;
    let mut lines = Vec::new();

    let title_left = format!("Select {}", picker.title.to_lowercase());
    let title_right = "esc";
    let title_padding = width.saturating_sub(title_left.len() + title_right.len());
    lines.push(Line::from(vec![
        Span::styled(
            title_left,
            Style::default()
                .fg(theme.text_fg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ".repeat(title_padding)),
        Span::styled(title_right, Style::default().fg(theme.muted_fg)),
    ]));

    // Keys row instead of a search box: the lists are short enough to scan.
    lines.push(Line::from(Span::styled(
        fit_text_to_width("Up/Down choose · Enter confirm", width),
        Style::default().fg(theme.muted_fg),
    )));

    lines.push(Line::from(""));

    let list_height = inner.height.saturating_sub(3) as usize;
    let start = picker
        .scroll
        .min(picker.options.len().saturating_sub(list_height));
    for (offset, option) in picker
        .options
        .iter()
        .skip(start)
        .take(list_height.max(1))
        .enumerate()
    {
        let idx = start + offset;
        let is_selected = idx == picker.selected;
        let is_active = idx == picker.active;

        let bullet = if is_active { "● " } else { "  " };
        let label = format!("{bullet}{option}");

        if is_selected {
            let padded_label = format!("{label:<width$}");
            lines.push(Line::from(Span::styled(
                padded_label,
                Style::default()
                    .fg(theme.panel_bg)
                    .bg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                label,
                Style::default().fg(theme.text_fg).bg(theme.panel_alt_bg),
            )));
        }
    }

    frame.draw_lines(
        inner,
        &lines,
        Style::default().bg(theme.panel_alt_bg).fg(theme.text_fg),
        Alignment::Left,
    );
}

fn render_about_modal(frame: &mut Frame<'_>, area: Rect, model: &AppState, theme: ThemePalette) {
    if area.width < 3 || area.height < 3 {
        return;
    }
    let width = 64.min(area.width.saturating_sub(4)).max(34);
    let height = 22.min(area.height.saturating_sub(1)).max(7);
    let modal = crate::state::centered_rect(area, width, height);
    let inner = frame.draw_border(
        modal,
        None,
        Style::default().bg(theme.panel_alt_bg),
        Style::default()
            .fg(theme.accent)
            .bg(theme.panel_alt_bg)
            .add_modifier(Modifier::BOLD),
    );
    draw_centered_border_title(
        frame,
        modal,
        "About Ordet",
        theme.accent,
        theme.panel_alt_bg,
    );

    let lines = about_lines(model, area, inner.width as usize, theme);
    frame.draw_lines(
        inner,
        &lines,
        Style::default().bg(theme.panel_alt_bg).fg(theme.text_fg),
        Alignment::Left,
    );
}

fn about_lines(model: &AppState, area: Rect, width: usize, theme: ThemePalette) -> Vec<Line> {
    let editor = model.editor();
    let stats = editor.word_stats();
    let (line, column) = editor.buffer.cursor_line_and_col(status_wrap_width(model));
    let page = match model.active_page_number() {
        Some(page) => format!("{page} of {}", model.visible_draft_count()),
        None => "scratch".to_string(),
    };
    let title = fit_text_to_width(&format!("{} ({page})", editor.title()), value_width(width));
    let build = format!(
        "{}, {} {}",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        std::env::consts::OS,
        std::env::consts::ARCH
    );

    vec![
        Line::from(Span::styled(
            format!("Ordet v{}", env!("CARGO_PKG_VERSION")),
            Style::default()
                .fg(theme.text_fg)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "A local, terminal-first word processor.",
            Style::default().fg(theme.text_fg),
        )),
        Line::from(""),
        help_section_title("SESSION", theme, theme.panel_alt_bg),
        about_stat("Build", build, width, theme),
        about_stat(
            "Terminal",
            format!("{} x {}", area.width, area.height),
            width,
            theme,
        ),
        about_stat("Theme", model.theme_name(), width, theme),
        about_stat("Language", model.ui.language.display_name(), width, theme),
        Line::from(""),
        help_section_title("DOCUMENT", theme, theme.panel_alt_bg),
        about_stat("Page", title, width, theme),
        about_stat("Cursor", format!("L{line} C{column}"), width, theme),
        about_stat("Words", stats.word_count, width, theme),
        about_stat("Characters", stats.chars, width, theme),
        about_stat("Lines", editor.buffer.source_line_count(), width, theme),
        about_stat(
            "Modified",
            if editor.is_dirty() { "yes" } else { "no" },
            width,
            theme,
        ),
        Line::from(""),
        Line::from(Span::styled(
            "MIT OR Apache-2.0",
            Style::default().fg(theme.muted_fg),
        )),
        Line::from(Span::styled(
            "github.com/LeonardoAmadeo/ordet",
            Style::default().fg(theme.muted_fg),
        )),
        Line::from(Span::styled(
            "Esc or Enter closes",
            Style::default().fg(theme.muted_fg),
        )),
    ]
}

fn value_width(width: usize) -> usize {
    width.saturating_sub(15).max(1)
}

fn about_stat(label: &str, value: impl ToString, width: usize, theme: ThemePalette) -> Line {
    let value = fit_text_to_width(&value.to_string(), value_width(width));
    stat_line_on(label, value, theme, theme.panel_alt_bg)
}

fn render_stats_panel(frame: &mut Frame<'_>, area: Rect, model: &AppState, theme: ThemePalette) {
    let stats = model.editor().word_stats();
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

/// The browser's rows for any surface: modal popup or welcome rail.
/// The location is one bare path row; a live search query shares the message
/// row with errors and action reports, while stock mode instructions are
/// dropped entirely.
fn file_browser_list_lines(
    browser: &FileBrowserState,
    language: crate::i18n::UiLanguage,
    theme: ThemePalette,
    background: Color,
    width: usize,
    height: usize,
) -> Vec<Line> {
    let mut lines = Vec::new();
    // The drive and path print bare instead of being labelled "Dir:" or
    // "Path:". Ordet's equivalent is the current location; in path mode that
    // same row becomes the editable buffer, so it never appears twice.
    let current_dir = browser.current_dir.display().to_string();
    let (location, location_fg) = if browser.mode == FileBrowserMode::Path {
        (browser.manual_path.as_str(), theme.accent)
    } else {
        (current_dir.as_str(), theme.text_fg)
    };
    lines.extend(wrap_lines(location, width, location_fg, background));
    if let Some(message) = browser_message_line(browser, language) {
        lines.extend(wrap_lines(&message, width, theme.muted_fg, background));
    }
    lines.push(Line::from(""));

    let entries = browser.filtered_entries();
    let entry_rows = height.saturating_sub(lines.len());
    if entry_rows > 0 {
        if entries.is_empty() {
            lines.push(Line::from(Span::styled(
                language.text(TextKey::FileBrowserNoMatches),
                Style::default().fg(theme.muted_fg).bg(background),
            )));
        } else {
            let start = browser
                .selected
                .saturating_sub(entry_rows.saturating_sub(1) / 2)
                .min(entries.len().saturating_sub(entry_rows));
            let end = (start + entry_rows).min(entries.len());
            for (offset, entry) in entries[start..end].iter().enumerate() {
                let index = start + offset;
                let selected = index == browser.selected;
                let prefix = if entry.is_dir { "[D] " } else { "    " };
                let style = if selected {
                    Style::default()
                        .fg(theme.text_fg)
                        .bg(theme.selected_bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.text_fg).bg(background)
                };
                let name = fit_text_to_width_tail(&entry.name, width.saturating_sub(4).max(8));
                lines.push(Line::from(Span::styled(format!("{prefix}{name}"), style)));
            }
        }
    }
    lines
}

/// The message row under the path. Stock mode instructions are dropped; only
/// a live search query, an error, or a real action report earns a row.
fn browser_message_line(
    browser: &FileBrowserState,
    language: crate::i18n::UiLanguage,
) -> Option<String> {
    if !browser.is_mode_hint() {
        return Some(browser.message.clone());
    }
    if browser.search.is_empty() {
        return None;
    }
    // Keep an active filter visible in every mode. Otherwise switching from
    // search to path would leave the list filtered with no clue why.
    Some(format!(
        "{}: {}",
        language.text(TextKey::FileBrowserSearch),
        browser.search
    ))
}

fn render_file_browser(
    frame: &mut Frame<'_>,
    area: Rect,
    model: &AppState,
    browser: &FileBrowserState,
    theme: ThemePalette,
) {
    let language = model.ui.language;
    let layout = browser_layout(area);
    let modal = layout.modal;
    frame.draw_border(
        modal,
        None,
        Style::default().bg(theme.panel_alt_bg),
        Style::default()
            .fg(theme.accent)
            .bg(theme.panel_alt_bg)
            .add_modifier(Modifier::BOLD),
    );
    draw_centered_border_title(
        frame,
        modal,
        language.text(TextKey::FileBrowserTitle),
        theme.accent,
        theme.panel_alt_bg,
    );
    let list_area = layout.list_area;

    let lines = file_browser_list_lines(
        browser,
        language,
        theme,
        theme.panel_alt_bg,
        list_area.width as usize,
        list_area.height as usize,
    );

    frame.clear_rect(
        list_area,
        Style::default().bg(theme.panel_alt_bg).fg(theme.text_fg),
    );
    frame.draw_lines(
        list_area,
        &lines,
        Style::default().bg(theme.panel_alt_bg).fg(theme.text_fg),
        Alignment::Left,
    );
}

fn render_welcome_screen(frame: &mut Frame<'_>, area: Rect, model: &AppState, theme: ThemePalette) {
    frame.clear_rect(area, Style::default().bg(theme.panel_bg));
    let surface_bg = welcome_surface_bg(theme);
    let rail_bg = welcome_side_panel_bg(theme);
    let layout = welcome_layout(area, model.welcome_side_panel_visible());
    frame.draw_border(
        layout.shell,
        None,
        Style::default().bg(surface_bg),
        Style::default()
            .fg(welcome_border_color(theme))
            .bg(surface_bg),
    );

    // The chrome, strip and footer bands share the surface fill: a bar-tinted
    // band reads as a stray header under a bright-bar theme, so the welcome
    // screen is one black field inside its border.
    frame.clear_rect(layout.chrome_area, Style::default().bg(surface_bg));
    frame.clear_rect(layout.strip_area, Style::default().bg(surface_bg));
    frame.clear_rect(layout.footer_area, Style::default().bg(surface_bg));
    render_welcome_strip(frame, layout.strip_area, model, theme, surface_bg);
    render_welcome_logo(frame, layout.logo_area, model, theme, surface_bg);
    // Footer first: the full-height rail paints over it on its own columns.
    render_welcome_footer(frame, layout.footer_area, model, theme, surface_bg);

    if let Some(rail_area) = layout.rail_area {
        // Flat column, not a box: fill plus a left divider only.
        frame.clear_rect(rail_area, Style::default().bg(rail_bg));
        let divider_style = Style::default()
            .fg(blend_color(theme.border, theme.text_fg, 0.18))
            .bg(rail_bg);
        let divider_line = Line::from(Span::styled("│", divider_style));
        let divider_lines = vec![divider_line; rail_area.height as usize];
        frame.draw_lines(
            Rect::new(rail_area.x, rail_area.y, 1, rail_area.height),
            &divider_lines,
            divider_style,
            Alignment::Left,
        );
        render_welcome_menu(
            frame,
            layout.menu_area,
            model,
            model.ui.welcome_selected,
            theme,
            surface_bg,
        );
        if let Some(side_panel_area) = layout.side_panel_area {
            render_welcome_side_panel(frame, side_panel_area, model, theme, rail_bg);
        }
    } else if model.welcome_help_active() {
        render_welcome_shortcuts(frame, layout.help_area, model, theme, surface_bg);
    } else {
        render_welcome_menu(
            frame,
            layout.menu_area,
            model,
            model.ui.welcome_selected,
            theme,
            surface_bg,
        );
    }
}

/// Blank breathing room under the welcome title. Debug and status readouts
/// stay out of the header; the rect itself is already cleared by the caller.
fn render_welcome_strip(
    _frame: &mut Frame<'_>,
    _area: Rect,
    _model: &AppState,
    _theme: ThemePalette,
    _background: Color,
) {
}

fn render_welcome_logo(
    frame: &mut Frame<'_>,
    area: Rect,
    _model: &AppState,
    theme: ThemePalette,
    background: Color,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let lines = welcome_wordmark_lines(theme, background, area.width);
    let block_height = lines.len() as u16;
    let top = area
        .y
        .saturating_add(area.height.saturating_sub(block_height).min(1));
    let mark_area = Rect::new(area.x, top, area.width, area.bottom().saturating_sub(top));
    frame.draw_lines(
        mark_area,
        &lines,
        Style::default().bg(background).fg(theme.text_fg),
        Alignment::Center,
    );
}

fn welcome_wordmark_lines(theme: ThemePalette, background: Color, width: u16) -> Vec<Line> {
    let subtitle = format!("beta v{APP_VERSION}");

    if width < 18 {
        return vec![
            Line::from(Span::styled(
                "ordet",
                Style::default()
                    .fg(theme.text_fg)
                    .bg(background)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                subtitle,
                Style::default()
                    .fg(blend_color(theme.muted_fg, theme.accent, 0.08))
                    .bg(background),
            )),
        ];
    }

    let wordmark = if width >= 56 {
        "o  r  d  e  t"
    } else if width >= 28 {
        "o r d e t"
    } else {
        "ordet"
    };

    vec![
        Line::from(Span::styled(
            wordmark,
            Style::default()
                .fg(theme.text_fg)
                .bg(background)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            subtitle,
            Style::default()
                .fg(blend_color(theme.muted_fg, theme.accent, 0.08))
                .bg(background),
        )),
    ]
}

fn render_welcome_menu(
    frame: &mut Frame<'_>,
    area: Rect,
    model: &AppState,
    selected: WelcomeOption,
    theme: ThemePalette,
    background: Color,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let language = model.ui.language;
    let mut lines = vec![
        welcome_section_title(language.text(TextKey::WelcomeMenu), theme, background),
        Line::from(""),
    ];

    let options = model.welcome_options();
    for (index, option) in options.iter().enumerate() {
        lines.extend(welcome_action_lines(
            *option,
            *option == selected,
            model,
            theme,
            background,
            area.width as usize,
        ));
        if index + 1 < options.len() {
            lines.push(Line::from(""));
        }
    }

    // No shortcut preview on highlight: Help reveals its list only after
    // Enter (or Right) moves focus into it, rail or narrow alike.

    frame.draw_lines(
        area,
        &lines,
        Style::default().bg(background).fg(theme.text_fg),
        Alignment::Left,
    );
}

fn render_welcome_side_panel(
    frame: &mut Frame<'_>,
    area: Rect,
    model: &AppState,
    theme: ThemePalette,
    background: Color,
) {
    // Open lives here on the home page: the file list takes the rail
    // instead of popping a modal. The editor keeps the popup.
    if let Some(browser) = &model.ui.file_browser {
        render_welcome_browser(frame, area, model, browser, theme, background);
    } else if model.welcome_help_active() {
        render_welcome_shortcuts(frame, area, model, theme, background);
    } else {
        render_welcome_recent(frame, area, model, theme, background);
    }
}

/// The same browser rows as the editor popup, fitted to the rail: no modal
/// frame, no preview, hint bar dropped. Keys are unchanged (they already
/// route globally), so browse/search/path/Enter/Esc all just work.
fn render_welcome_browser(
    frame: &mut Frame<'_>,
    area: Rect,
    model: &AppState,
    browser: &FileBrowserState,
    theme: ThemePalette,
    background: Color,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let lines = file_browser_list_lines(
        browser,
        model.ui.language,
        theme,
        background,
        area.width as usize,
        area.height as usize,
    );
    frame.clear_rect(area, Style::default().bg(background));
    frame.draw_lines(
        area,
        &lines,
        Style::default().bg(background).fg(theme.text_fg),
        Alignment::Left,
    );
}

/// The big Open popup belongs to the editor. On the home page the same
/// browser renders inline in the rail instead (see above).
fn should_render_browser_modal(model: &AppState) -> bool {
    model.ui.file_browser.is_some() && !model.show_welcome_screen()
}

fn welcome_action_lines(
    option: WelcomeOption,
    selected: bool,
    model: &AppState,
    theme: ThemePalette,
    background: Color,
    width: usize,
) -> Vec<Line> {
    let (_, label, detail) = welcome_option_copy(option, model);
    // Selected row: one solid accent band, dark text. The hotkey chip
    // inverts to stay readable on top of it.
    let highlight_bg = if selected { theme.accent } else { background };
    let base = if selected {
        Style::default().fg(theme.panel_bg).bg(theme.accent)
    } else {
        Style::default().fg(theme.text_fg).bg(background)
    };
    let hotkey_style = if selected {
        Style::default()
            .fg(theme.accent)
            .bg(theme.panel_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(blend_color(theme.accent, theme.warn, 0.20))
            .bg(background)
            .add_modifier(Modifier::BOLD)
    };
    // 3-cell slot, caret in the middle: one band-colored cell each side.
    // (Styled, not raw: a raw span would inherit the draw base style and
    // punch a hole in the band.)
    let prefix = if selected { " > " } else { "   " };
    let mut label_line = Line::from(vec![
        Span::styled(prefix, base),
        Span::styled(format!("[{}]", welcome_option_hotkey(option)), hotkey_style),
        Span::styled(" ", Style::default().bg(highlight_bg)),
        Span::styled(label, base.add_modifier(Modifier::BOLD)),
    ]);
    let remaining = width.saturating_sub(label_line.width());
    if remaining > 0 {
        label_line.spans.push(Span::styled(
            " ".repeat(remaining),
            Style::default().bg(highlight_bg),
        ));
    }

    let mut lines = vec![label_line];
    lines.extend(wrap_prefixed_lines(
        "    ",
        &detail,
        width.max(8),
        theme.muted_fg,
        background,
    ));
    lines
}

fn render_welcome_recent(
    frame: &mut Frame<'_>,
    area: Rect,
    model: &AppState,
    theme: ThemePalette,
    background: Color,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let language = model.ui.language;
    let recent_paths = model.welcome_recent_paths();
    let mut lines = Vec::new();
    lines.push(welcome_section_title(
        language.text(TextKey::WelcomeLastPage),
        theme,
        background,
    ));
    if let Some(path) = recent_paths.first() {
        lines.push(Line::from(Span::styled(
            welcome_path_label(path.as_path()),
            Style::default()
                .fg(theme.text_fg)
                .bg(background)
                .add_modifier(Modifier::BOLD),
        )));
        if let Some(directory) = welcome_path_directory(path.as_path()) {
            lines.push(Line::from(Span::styled(
                fit_text_to_width_tail(&directory, area.width as usize),
                Style::default().fg(theme.muted_fg).bg(background),
            )));
        }
    } else {
        lines.push(Line::from(Span::styled(
            language.text(TextKey::WelcomeOpenToBegin),
            Style::default().fg(theme.muted_fg).bg(background),
        )));
    }

    lines.push(Line::from(""));

    // Static cheat sheet: the six chords worth knowing before opening
    // anything. No filesystem scan, no interaction, the menu on the left
    // already owns every action. Key names stay untranslated, like help.
    lines.push(welcome_section_title("keys", theme, background));
    for (key, what) in [
        ("Arrows", "move"),
        ("⌃S", "save"),
        ("⌃B", "open"),
        ("F2", "settings"),
        ("F10", "menu"),
        ("⌃Q", "quit"),
    ] {
        lines.push(Line::from(vec![
            Span::styled(
                format!("[{key}]"),
                Style::default()
                    .fg(theme.accent)
                    .bg(background)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {what}"),
                Style::default().fg(theme.text_fg).bg(background),
            ),
        ]));
    }

    let filler = area
        .height
        .saturating_sub(lines.len() as u16)
        .saturating_sub(1);
    for _ in 0..filler {
        lines.push(Line::from(""));
    }
    lines.push(Line::from(""));

    frame.draw_lines(
        area,
        &lines,
        Style::default().bg(background).fg(theme.text_fg),
        Alignment::Left,
    );
}

fn render_welcome_shortcuts(
    frame: &mut Frame<'_>,
    area: Rect,
    model: &AppState,
    theme: ThemePalette,
    background: Color,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let lines = welcome_shortcut_lines(model, area.width as usize, theme, background);
    let max_start = lines.len().saturating_sub(area.height as usize);
    let start = model.ui.welcome_help_scroll.min(max_start);
    let end = (start + area.height as usize).min(lines.len());
    let visible = &lines[start..end];
    frame.clear_rect(area, Style::default().bg(background).fg(theme.text_fg));
    frame.draw_lines(
        area,
        visible,
        Style::default().bg(background).fg(theme.text_fg),
        Alignment::Left,
    );
}

fn welcome_shortcut_lines(
    model: &AppState,
    width: usize,
    theme: ThemePalette,
    background: Color,
) -> Vec<Line> {
    let mut lines = vec![Line::from(Span::styled(
        " help ",
        if model.welcome_help_active() {
            Style::default()
                .fg(theme.panel_bg)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(theme.accent)
                .bg(background)
                .add_modifier(Modifier::BOLD)
        },
    ))];

    lines.push(Line::from(""));
    lines.push(help_section_title("Menu", theme, background));
    let mut menu_entries = WELCOME_MENU_HELP.to_vec();
    if model.primary_recent_path().is_none() {
        menu_entries.retain(|entry| entry.keys != "R");
    }
    lines.extend(welcome_help_lines(&menu_entries, width, theme, background));

    for section in EDITOR_HELP_SECTIONS {
        lines.push(Line::from(""));
        lines.push(help_section_title(section.tab.title(), theme, background));
        lines.extend(welcome_help_lines(
            section.entries,
            width,
            theme,
            background,
        ));
    }
    lines
}

/// The welcome rail uses a compact two-column table: commands are bright on
/// the left and their muted descriptions begin on the same row. Long commands
/// wrap inside the command column rather than being shortened with ellipses.
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

fn welcome_help_lines(
    entries: &[HelpEntry],
    width: usize,
    theme: ThemePalette,
    background: Color,
) -> Vec<Line> {
    let width = width.max(1);
    let (key_column, description_width) = help_columns(width);
    let mut lines = Vec::new();
    for entry in entries {
        let commands = wrap_text(entry.keys, key_column.saturating_sub(1).max(1));
        let descriptions = wrap_text(entry.description, description_width);
        let rows = commands.len().max(descriptions.len());
        for row in 0..rows {
            let command = commands.get(row).map(String::as_str).unwrap_or("");
            let description = descriptions.get(row).map(String::as_str).unwrap_or("");
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{command:<key_column$}"),
                    Style::default()
                        .fg(theme.text_fg)
                        .bg(background)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    description,
                    Style::default().fg(theme.muted_fg).bg(background),
                ),
            ]));
        }
    }
    lines
}

fn welcome_option_hotkey(option: WelcomeOption) -> &'static str {
    match option {
        WelcomeOption::ResumeRecent => "R",
        WelcomeOption::NewDraft => "D",
        WelcomeOption::OpenFile => "O",
        WelcomeOption::Help => "H",
        WelcomeOption::Exit => "X",
    }
}

fn welcome_option_copy(
    option: WelcomeOption,
    model: &AppState,
) -> (&'static str, &'static str, String) {
    let language = model.ui.language;
    match option {
        WelcomeOption::ResumeRecent => (
            "R",
            language.text(TextKey::WelcomeResumeLabel),
            model
                .primary_recent_path()
                .map(|path| welcome_path_label(path.as_path()))
                .as_deref()
                .map(|path| language.welcome_resume_detail(Some(path)))
                .unwrap_or_else(|| language.welcome_resume_detail(None)),
        ),
        WelcomeOption::NewDraft => (
            "D",
            language.text(TextKey::WelcomeNewDraftLabel),
            language.text(TextKey::WelcomeNewDraftDetail).to_string(),
        ),
        WelcomeOption::OpenFile => (
            "O",
            language.text(TextKey::WelcomeOpenFileLabel),
            language.text(TextKey::WelcomeOpenFileDetail).to_string(),
        ),
        WelcomeOption::Help => (
            "H",
            language.text(TextKey::WelcomeHelpLabel),
            if model.welcome_help_active() {
                language.text(TextKey::WelcomeHelpActiveDetail).to_string()
            } else {
                language.text(TextKey::WelcomeHelpDetail).to_string()
            },
        ),
        WelcomeOption::Exit => (
            "X",
            language.text(TextKey::WelcomeExitLabel),
            language.text(TextKey::WelcomeExitDetail).to_string(),
        ),
    }
}

fn welcome_layout(area: Rect, show_side_panel: bool) -> WelcomeLayout {
    if area.width < 68 || area.height < 18 {
        let shell = centered_rect(
            area,
            area.width.saturating_sub(4).clamp(48, 84),
            area.height.saturating_sub(1).clamp(16, 22),
        );
        let inner = shell.inset(1, 1);
        let chrome_area = Rect::new(inner.x, inner.y, inner.width, 1);
        let strip_area = Rect::new(inner.x, chrome_area.bottom(), inner.width, 1);
        let footer_area = Rect::new(
            inner.x,
            inner.bottom().saturating_sub(1),
            inner.width,
            1.min(inner.height),
        );
        let content = Rect::new(
            inner.x,
            strip_area.bottom(),
            inner.width,
            footer_area.y.saturating_sub(strip_area.bottom()),
        );
        let hero_height = content.height.saturating_sub(7).clamp(4, 5);
        let menu_area = Rect::new(
            content.x.saturating_add(2),
            content.y.saturating_add(hero_height),
            content.width.saturating_sub(4),
            content
                .bottom()
                .saturating_sub(content.y.saturating_add(hero_height)),
        );
        let logo_area = Rect::new(
            content.x.saturating_add(1),
            content.y.saturating_add(1),
            content.width.saturating_sub(2),
            hero_height,
        );
        return WelcomeLayout {
            shell,
            chrome_area,
            strip_area,
            logo_area,
            menu_area,
            rail_area: None,
            side_panel_area: None,
            help_area: menu_area,
            footer_area,
        };
    }

    let shell = centered_rect(
        area,
        area.width.saturating_sub(4).clamp(78, 118),
        area.height.saturating_sub(1).clamp(20, 30),
    );
    let inner = shell.inset(1, 1);
    let chrome_area = Rect::new(inner.x, inner.y, inner.width, 1);
    let strip_area = Rect::new(inner.x, chrome_area.bottom(), inner.width, 1);
    let footer_area = Rect::new(
        inner.x,
        inner.bottom().saturating_sub(1),
        inner.width,
        1.min(inner.height),
    );
    let content = Rect::new(
        inner.x,
        strip_area.bottom(),
        inner.width,
        footer_area.y.saturating_sub(strip_area.bottom()),
    );
    let show_recent_panel = show_side_panel && content.width >= 90 && content.height >= 14;
    if show_recent_panel {
        let rail_width = ((content.width as usize * 28) / 100).clamp(28, 34) as u16;
        let rail_width = rail_width.min(content.width.saturating_sub(34));
        let left_width = content.width.saturating_sub(rail_width).saturating_sub(1);
        let left = Rect::new(content.x, content.y, left_width, content.height);
        let logo_area = Rect::new(
            left.x.saturating_add(2),
            left.y.saturating_add(1),
            left.width.saturating_sub(4),
            left.height.saturating_sub(12).clamp(4, 6),
        );
        let menu_area = Rect::new(
            left.x.saturating_add(3),
            logo_area.bottom(),
            left.width.saturating_sub(6),
            left.bottom().saturating_sub(logo_area.bottom()),
        );
        // Full-height column: the whole inner height, so the welcome rail
        // never floats with gaps above or below. It paints over the chrome, strip
        // and footer rows on its columns (all left-aligned/short, so nothing
        // readable is covered). Tight 1-cell padding inside the border.
        let rail_area = Rect::new(
            left.right().saturating_add(1),
            inner.y,
            rail_width,
            inner.height,
        );
        let side_panel_area = rail_area.inset(1, 1);
        let footer_area = Rect::new(content.x, footer_area.y, left_width, footer_area.height);
        return WelcomeLayout {
            shell,
            chrome_area,
            strip_area,
            logo_area,
            menu_area,
            rail_area: Some(rail_area),
            side_panel_area: Some(side_panel_area),
            help_area: side_panel_area,
            footer_area,
        };
    }

    let logo_area = Rect::new(
        content.x.saturating_add(2),
        content.y.saturating_add(1),
        content.width.saturating_sub(4),
        content.height.saturating_sub(12).clamp(4, 6),
    );
    let menu_area = Rect::new(
        content.x.saturating_add(3),
        logo_area.bottom(),
        content.width.saturating_sub(6),
        content.bottom().saturating_sub(logo_area.bottom()),
    );
    WelcomeLayout {
        shell,
        chrome_area,
        strip_area,
        logo_area,
        menu_area,
        rail_area: None,
        side_panel_area: None,
        help_area: menu_area,
        footer_area,
    }
}

fn welcome_help_background(model: &AppState) -> Color {
    let theme = model.theme_palette();
    if welcome_layout(model.body_area(), model.welcome_side_panel_visible())
        .side_panel_area
        .is_some()
    {
        welcome_side_panel_bg(theme)
    } else {
        welcome_surface_bg(theme)
    }
}

fn welcome_help_viewport(model: &AppState) -> Rect {
    welcome_layout(model.body_area(), model.welcome_side_panel_visible()).help_area
}

fn welcome_help_page_step(model: &AppState) -> usize {
    welcome_help_viewport(model).height.max(1) as usize
}

fn welcome_help_max_scroll(model: &AppState) -> usize {
    let viewport = welcome_help_viewport(model);
    if viewport.width == 0 || viewport.height == 0 {
        return 0;
    }

    let theme = model.theme_palette();
    let lines = welcome_shortcut_lines(
        model,
        viewport.width as usize,
        theme,
        welcome_help_background(model),
    );
    lines.len().saturating_sub(viewport.height as usize)
}

fn welcome_surface_bg(theme: ThemePalette) -> Color {
    blend_color(theme.panel_bg, theme.panel_alt_bg, 0.10)
}

fn welcome_side_panel_bg(theme: ThemePalette) -> Color {
    blend_color(theme.panel_alt_bg, theme.panel_bg, 0.40)
}

fn welcome_border_color(theme: ThemePalette) -> Color {
    blend_color(theme.border, theme.accent, 0.28)
}

fn handle_welcome_event(
    model: &mut AppState,
    event: &UiEvent<Command, AppMessage>,
    cx: &mut EventCx<Action>,
) {
    let help_page_step = welcome_help_page_step(model);
    let help_max_scroll = welcome_help_max_scroll(model);

    match event {
        UiEvent::Command(command) => {
            if model.welcome_help_active() {
                model.clamp_welcome_help_scroll(help_max_scroll);
                match command {
                    Command::MoveUp | Command::SelectUp => {
                        model.scroll_welcome_help(false, 1, help_max_scroll);
                    }
                    Command::MoveDown | Command::SelectDown => {
                        model.scroll_welcome_help(true, 1, help_max_scroll);
                    }
                    Command::PageUp => {
                        model.scroll_welcome_help(
                            false,
                            help_page_step.saturating_sub(1),
                            help_max_scroll,
                        );
                    }
                    Command::PageDown => {
                        model.scroll_welcome_help(
                            true,
                            help_page_step.saturating_sub(1),
                            help_max_scroll,
                        );
                    }
                    Command::MoveHome => model.reset_welcome_help_scroll(),
                    Command::MoveEnd => model.set_welcome_help_scroll(help_max_scroll),
                    Command::MoveLeft | Command::SelectLeft | Command::MoveWordLeft => {
                        model.blur_welcome_help();
                    }
                    Command::Quit => cx.emit(Action::Quit),
                    Command::NewDraft => emit_welcome_action(WelcomeOption::NewDraft, model, cx),
                    Command::OpenFilePrompt => {
                        emit_welcome_action(WelcomeOption::OpenFile, model, cx)
                    }
                    _ => {}
                }
                return;
            }

            match command {
                Command::MoveUp
                | Command::SelectUp
                | Command::MoveLeft
                | Command::SelectLeft
                | Command::MoveWordLeft => model.cycle_welcome_selection(true),
                Command::MoveDown | Command::SelectDown | Command::InsertTab => {
                    model.cycle_welcome_selection(false)
                }
                Command::MoveRight | Command::SelectRight | Command::MoveWordRight => {
                    if model.ui.welcome_selected == WelcomeOption::Help {
                        model.focus_welcome_help();
                    } else {
                        model.cycle_welcome_selection(false);
                    }
                }
                Command::Submit => {
                    if model.ui.welcome_selected == WelcomeOption::Help {
                        model.focus_welcome_help();
                    } else {
                        emit_welcome_action(model.ui.welcome_selected, model, cx);
                    }
                }
                Command::NewDraft => emit_welcome_action(WelcomeOption::NewDraft, model, cx),
                Command::OpenFilePrompt => emit_welcome_action(WelcomeOption::OpenFile, model, cx),
                Command::Quit => cx.emit(Action::Quit),
                _ => {}
            }
        }
        UiEvent::Key(key)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) =>
        {
            // Scrolling the help list is the only welcome state with
            // somewhere to go back to: Esc returns to the menu like Left.
            if key.code == KeyCode::Esc {
                if model.welcome_help_active() {
                    model.blur_welcome_help();
                }
                return;
            }
            if let KeyCode::Char(ch) = key.code {
                match ch.to_ascii_lowercase() {
                    'r' => emit_welcome_action(WelcomeOption::ResumeRecent, model, cx),
                    'd' => emit_welcome_action(WelcomeOption::NewDraft, model, cx),
                    'o' => emit_welcome_action(WelcomeOption::OpenFile, model, cx),
                    'h' | '?' => {
                        // Open straight away like every other hotkey: no
                        // extra Enter needed. The rail shows the browser
                        // first, so dismiss it or the shortcuts stay hidden.
                        model.set_welcome_selection(WelcomeOption::Help);
                        model.ui.file_browser = None;
                        model.focus_welcome_help();
                        cx.emit(Action::SetStatus(
                            "Help open. Up/Down scrolls, Left returns to the menu.".to_string(),
                        ));
                    }
                    'x' => cx.emit(Action::Quit),
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn emit_welcome_action(option: WelcomeOption, model: &mut AppState, cx: &mut EventCx<Action>) {
    match option {
        WelcomeOption::ResumeRecent => {
            if let Some(path) = model.primary_recent_path() {
                // Leaving the welcome screen: don't strand the rail browser
                // as a modal over the editor.
                model.ui.file_browser = None;
                cx.emit(Action::OpenPath(path));
            }
        }
        WelcomeOption::NewDraft => {
            model.ui.file_browser = None;
            cx.emit(Action::NewDraft)
        }
        WelcomeOption::OpenFile => {
            // The browser opens in the rail while staying on this screen, so
            // move the highlight along with the action.
            model.set_welcome_selection(WelcomeOption::OpenFile);
            cx.emit(Action::OpenFilePrompt)
        }
        WelcomeOption::Help => {}
        WelcomeOption::Exit => cx.emit(Action::Quit),
    }
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

fn welcome_section_title(title: &str, theme: ThemePalette, background: Color) -> Line {
    Line::from(Span::styled(
        title.to_string(),
        Style::default()
            .fg(blend_color(theme.accent, theme.warn, 0.16))
            .bg(background)
            .add_modifier(Modifier::BOLD),
    ))
}

/// Directory half of a recent path: filename goes bold on the line above,
/// the containing folder dim on the line below. Home shortens to `~`, and
/// the caller trims the tail (never the middle) to fit.
fn welcome_path_directory(path: &Path) -> Option<String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())?;
    let display = parent.display().to_string();
    if display.is_empty() {
        return None;
    }
    Some(match dirs::home_dir() {
        Some(home) => {
            let home = home.to_string_lossy();
            if display.starts_with(home.as_ref()) {
                format!("~{}", &display[home.len()..])
            } else {
                display
            }
        }
        None => display,
    })
}

fn render_welcome_footer(
    frame: &mut Frame<'_>,
    area: Rect,
    model: &AppState,
    theme: ThemePalette,
    background: Color,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let text = if model.welcome_help_active() && area.width >= 68 {
        "Up/Down·scroll  ·  PgUp/PgDn·faster  ·  Left·back  ·  ⌃Q·quit"
    } else if model.welcome_help_active() {
        "Up/Down·scroll  ·  Left·back  ·  ⌃Q·quit"
    } else if area.width >= 68 {
        "Tab·next  ·  Enter·open  ·  H/?·help  ·  ⌃B·open  ·  ⌃Q·quit"
    } else {
        "Tab·next  ·  Enter·open  ·  H/?·help"
    };
    let line = Line::from(Span::styled(
        fit_text_to_width_tail(text, area.width as usize),
        Style::default().fg(theme.muted_fg).bg(background),
    ));
    frame.draw_line(
        area,
        0,
        &line,
        Style::default().bg(background),
        Alignment::Center,
    );
}

fn welcome_path_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| header_path_label(Some(path)))
}

#[cfg(test)]
mod welcome_path_directory_tests {
    use super::welcome_path_directory;
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
            let stamp = crate::app::local_timestamp();
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
mod tests {
    use std::collections::HashSet;

    use ordet_tui_framework::{EventCx, Rect, UiEvent};

    use crate::commands::Command;
    use crate::state::{AppState, HelpTab, PanelKind, WelcomeOption};
    use crate::theme::ThemeSelection;

    use super::{
        fit_text_to_width, fit_text_to_width_tail, handle_welcome_event, prompt_status_text,
        render_editor_buffer, render_wrapped_line, ruler_caret_split, ruler_marks, settings_rows,
        welcome_help_lines, welcome_help_max_scroll, welcome_shortcut_lines, HelpEntry,
        EDITOR_HELP_SECTIONS, EDITOR_WORKSPACE_HELP, WELCOME_MENU_HELP,
    };

    fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("{prefix}-{stamp}"));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn empty_row_never_paints_cursor_line_background() {
        use crate::editor_buffer::{Boundary, EditorBuffer, WrappedLine};

        let state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        let theme = state.theme_palette();
        let buffer = EditorBuffer::from_text("");
        let row = WrappedLine {
            start_char: 0,
            end_char: 0,
            text: String::new(),
            display_offsets: vec![0],
            boundaries: vec![Boundary {
                char_index: 0,
                column: 0,
            }],
        };
        // The startup situation: cursor line, focused, no selection. The
        // placeholder must be plain panel background, never cursor-line
        // blue masquerading as a selection.
        let painted = render_wrapped_line(&row, &buffer, None, true, theme);
        assert!(!painted.spans.is_empty());
        for span in &painted.spans {
            assert_eq!(span.style.bg, Some(theme.panel_bg));
        }

        // A real selection covering the row still highlights it.
        let painted = render_wrapped_line(&row, &buffer, Some((0, 1)), true, theme);
        assert!(painted
            .spans
            .iter()
            .any(|span| span.style.bg == Some(theme.selection_bg)));

        // Non-empty rows paint plain too: no row wash follows the cursor,
        // only the caret marks it.
        let text_row = WrappedLine {
            start_char: 0,
            end_char: 5,
            text: "hello".to_string(),
            display_offsets: vec![0, 1, 2, 3, 4, 5],
            boundaries: vec![],
        };
        let painted = render_wrapped_line(&text_row, &buffer, None, true, theme);
        for span in &painted.spans {
            assert_eq!(span.style.bg, Some(theme.panel_bg));
        }
    }

    #[test]
    fn centered_and_right_aligned_rows_paint_their_leading_padding() {
        use crate::editor_buffer::{EditorBuffer, ParagraphAlignment};

        let state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        let theme = state.theme_palette();
        let mut buffer = EditorBuffer::from_text("alpha beta");
        buffer.set_alignment_for_selection(ParagraphAlignment::Center);

        let layout = buffer.layout(20);
        let row = &layout.lines[0];
        // The wrap put five columns of padding in front of the text.
        assert_eq!(row.boundaries[0].column, 5);
        // The caret column counts the padding, so the text and the caret
        // have to agree on where the row starts.
        let painted = render_wrapped_line(row, &buffer, None, true, theme);
        let rendered = painted
            .spans
            .iter()
            .map(|span| span.content.as_str())
            .collect::<String>();
        assert_eq!(rendered, "     alpha beta");

        // Right alignment pushes the same way, only from the other side.
        buffer.set_alignment_for_selection(ParagraphAlignment::Right);
        let layout = buffer.layout(20);
        let row = &layout.lines[0];
        assert_eq!(row.boundaries[0].column, 10);
        let painted = render_wrapped_line(row, &buffer, None, true, theme);
        let rendered = painted
            .spans
            .iter()
            .map(|span| span.content.as_str())
            .collect::<String>();
        assert_eq!(rendered, "          alpha beta");
    }

    #[test]
    fn a_centered_paragraph_is_painted_around_the_middle_of_the_editor() {
        use ordet_tui_framework::{Buffer, Frame, Point, Rect};

        use crate::editor_buffer::{EditorBuffer, ParagraphAlignment};

        let state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        let theme = state.theme_palette();
        let mut buffer = EditorBuffer::from_text("Song of the Day:\n- \"Midnight\"");
        // Center the second paragraph only.
        buffer.move_to_line(2, 20);
        buffer.set_alignment_for_selection(ParagraphAlignment::Center);

        let area = Rect::new(0, 0, 40, 4);
        let mut cells = Buffer::new(area);
        {
            let mut frame = Frame::new(area, &mut cells);
            render_editor_buffer(&mut frame, area, &buffer, 0, theme, true, false);
        }
        let row_text = |y: u16| {
            (0..area.width)
                .map(|x| {
                    cells
                        .cell(Point::new(x, y))
                        .map(|cell| cell.symbol.as_str().to_string())
                        .unwrap_or_default()
                })
                .collect::<String>()
        };

        assert!(row_text(0).starts_with("Song of the Day:"));
        // The second one is centered: 14 columns of padding in a 40 wide
        // pane, so the text starts past the middle.
        let second = row_text(1);
        assert!(
            second.starts_with("              - \"Midnight\""),
            "row: {second:?}"
        );
        // The caret counts the same padding, so it sits on the first
        // character of the text instead of floating in the margin.
        assert_eq!(buffer.cursor_visual_column(area.width as usize), 14);
        buffer.move_end(area.width as usize, false);
        assert_eq!(buffer.cursor_visual_column(area.width as usize), 26);
    }

    #[test]
    fn prompt_text_fits_one_status_row_by_trimming_its_value() {
        use crate::prompt::PromptState;
        use unicode_width::UnicodeWidthStr;

        let prompt = PromptState::save_path(Some(
            "/Users/someone/very/long/path/into/a/deep/tree/draft.ord",
        ));
        assert_eq!(prompt_status_text(&prompt, 200), prompt.render_status());

        // Narrow rows keep the label, the value's tail, the part being
        // typed, and the key hint.
        let trimmed = prompt_status_text(&prompt, 90);
        assert!(
            UnicodeWidthStr::width(trimmed.as_str()) <= 90,
            "row: {trimmed:?}"
        );
        assert!(trimmed.contains("draft.ord"), "keeps the tail: {trimmed:?}");
        assert!(
            !trimmed.contains("/Users"),
            "the head gives way: {trimmed:?}"
        );
        assert!(
            trimmed.ends_with(&format!("[{}]", prompt.hint)),
            "keeps the hint: {trimmed:?}"
        );

        // A row too narrow for the hint as well falls back to a prefix of
        // the whole prompt, and a zero-width row paints nothing at all.
        let narrow = prompt_status_text(&prompt, 48);
        assert!(
            UnicodeWidthStr::width(narrow.as_str()) <= 48,
            "row: {narrow:?}"
        );
        assert!(!narrow.is_empty());
        assert_eq!(prompt_status_text(&prompt, 0), "");
    }

    #[test]
    fn status_messages_collapse_home_and_keep_the_tail() {
        use super::fit_status_message;
        use unicode_width::UnicodeWidthStr;

        let home = "/Users/leonardoamadeo/";
        // Fits: home collapses to ~, nothing else changes.
        assert_eq!(
            fit_status_message(
                "Opened /Users/leonardoamadeo/Documents/Ordet-final/i.ord",
                Some(home),
                60
            ),
            " Opened ~/Documents/Ordet-final/i.ord "
        );
        // Overflow: the filename end survives, never a split directory.
        let fitted = fit_status_message(
            "Opened /Users/leonardoamadeo/Documents/Ordet-final/i.ord",
            Some(home),
            20,
        );
        assert!(fitted.ends_with("i.ord"), "keeps the tail: {fitted}");
        assert!(
            UnicodeWidthStr::width(fitted.as_str()) <= 20,
            "fits the budget: {fitted}"
        );
        // No home match: plain message, fitted if needed.
        assert_eq!(
            fit_status_message("Saved draft.", None, 60),
            " Saved draft. "
        );
    }

    #[test]
    fn welcome_rail_runs_strip_to_bottom_and_footer_clears_it() {
        let area = ordet_tui_framework::Rect::new(0, 0, 200, 55);
        let layout = super::welcome_layout(area, true);
        let rail = layout
            .rail_area
            .expect("a wide window shows the sidebar rail");
        let side = layout.side_panel_area.expect("rail carries a panel");
        // No gaps: the rail spans the whole inner height, over the chrome,
        // strip and footer rows on its own columns.
        let inner_top = layout.shell.y + 1;
        let inner_bottom = layout.shell.bottom().saturating_sub(1);
        assert_eq!(rail.y, inner_top);
        assert_eq!(rail.bottom(), inner_bottom);
        // The footer stays in the menu column instead of slicing the rail.
        assert!(
            layout.footer_area.right() <= rail.x,
            "footer {:?} overlaps rail {:?}",
            layout.footer_area,
            rail
        );
        // Panel content sits inside the rail with breathing room.
        assert!(side.y > rail.y && side.bottom() < rail.bottom());
        assert!(side.right() < rail.right());
    }

    #[test]
    fn welcome_side_panel_needs_enter_not_just_selection() {
        use crate::file_browser::FileBrowserState;
        use crate::state::WelcomeOption;

        let area = ordet_tui_framework::Rect::new(0, 0, 200, 55);
        let mut state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        // Highlighting any entry, Open file and Help included, shows no
        // rail. The menu keeps the full width until Enter is pressed.
        for option in [
            WelcomeOption::ResumeRecent,
            WelcomeOption::NewDraft,
            WelcomeOption::OpenFile,
            WelcomeOption::Help,
            WelcomeOption::Exit,
        ] {
            state.set_welcome_selection(option);
            assert!(
                !state.welcome_side_panel_visible(),
                "{option:?} highlight should hide the side panel"
            );
            assert!(
                super::welcome_layout(area, state.welcome_side_panel_visible())
                    .rail_area
                    .is_none(),
                "{option:?} highlight should leave no rail"
            );
        }
        // Enter on Help (focus into its content) reveals the shortcut rail.
        state.set_welcome_selection(WelcomeOption::Help);
        state.focus_welcome_help();
        assert!(state.welcome_side_panel_visible());
        assert!(
            super::welcome_layout(area, state.welcome_side_panel_visible())
                .rail_area
                .is_some(),
            "entered help should keep the rail"
        );
        // Leaving help hides it again.
        state.blur_welcome_help();
        assert!(!state.welcome_side_panel_visible());
        // Enter on Open file (browser opens) reveals the browser rail.
        state.set_welcome_selection(WelcomeOption::OpenFile);
        assert!(!state.welcome_side_panel_visible());
        state.ui.file_browser = Some(FileBrowserState::open(std::env::temp_dir()));
        assert!(state.welcome_side_panel_visible());
        assert!(
            super::welcome_layout(area, state.welcome_side_panel_visible())
                .rail_area
                .is_some(),
            "open browser should keep the rail"
        );
    }

    #[test]
    fn welcome_sidebar_column_is_gapless_from_strip_to_shell_edge() {
        use ordet_tui_framework::{Buffer, Frame, Point};

        let area = ordet_tui_framework::Rect::new(0, 0, 200, 60);
        let mut buffer = Buffer::new(area);
        let mut state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        assert!(state.show_welcome_screen());
        // The rail only shows once Help is entered, not on highlight.
        state.set_welcome_selection(crate::state::WelcomeOption::Help);
        state.focus_welcome_help();
        let theme = state.theme_palette();
        let layout = super::welcome_layout(area, state.welcome_side_panel_visible());
        {
            let mut frame = Frame::new(area, &mut buffer);
            super::render_welcome_screen(&mut frame, area, &state, theme);
        }
        let rail = layout
            .rail_area
            .expect("a wide window shows the sidebar rail");
        let rail_bg = Some(super::welcome_side_panel_bg(theme));
        // Every interior row of the rail column must carry the sidebar
        // background: chrome, strip and footer rows included.
        let mid_x = rail.x + rail.width / 2;
        let mut y = rail.y + 1;
        let mut checked = 0;
        while y < rail.bottom().saturating_sub(1) {
            let cell = buffer
                .cell(Point::new(mid_x, y))
                .unwrap_or_else(|| panic!("no cell at ({mid_x}, {y})"));
            assert_eq!(
                cell.style.bg, rail_bg,
                "rail column row {y} is not sidebar background: {:?}",
                cell.symbol,
            );
            checked += 1;
            y += 1;
        }
        assert!(checked > 10, "probe covered {checked} rows");
    }

    #[test]
    fn browser_list_lines_fit_a_narrow_rail() {
        use crate::file_browser::FileBrowserState;

        let dir = unique_temp_dir("ordet-browser-lines");
        std::fs::write(dir.join("alpha.ord"), "hello").expect("seed file");
        std::fs::write(dir.join("beta.ord"), "world").expect("seed file");
        let browser = FileBrowserState::open(dir.clone());
        let state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        let theme = state.theme_palette();

        let lines = super::file_browser_list_lines(
            &browser,
            state.ui.language,
            theme,
            theme.panel_alt_bg,
            30,
            24,
        );
        assert!(!lines.is_empty());
        for line in &lines {
            assert!(
                line.width() <= 30,
                "browser line overflows the rail: {:?}",
                line.spans
                    .iter()
                    .map(|span| span.content.as_str())
                    .collect::<Vec<_>>()
                    .join("")
            );
        }
        let text: String = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_str())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("alpha.ord"), "entries missing: {text}");
        assert!(text.contains("beta.ord"), "entries missing: {text}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn browser_shows_the_path_without_a_label() {
        use crate::file_browser::FileBrowserState;

        fn browser_text(browser: &FileBrowserState, state: &AppState) -> String {
            let theme = state.theme_palette();
            super::file_browser_list_lines(
                browser,
                state.ui.language,
                theme,
                theme.panel_alt_bg,
                200,
                30,
            )
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
        }

        let dir = unique_temp_dir("ordet-browser-one-dir");
        std::fs::create_dir_all(&dir).expect("seed dir");
        let state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        let mut browser = FileBrowserState::open(dir.clone());
        let current = dir.display().to_string();

        // Browse mode: one bare path row, with no label and no duplicate.
        let text = browser_text(&browser, &state);
        assert_eq!(
            text.lines().next(),
            Some(current.as_str()),
            "path header: {text}"
        );
        assert_eq!(text.matches(&current).count(), 1, "path repeated: {text}");
        assert!(!text.contains("Dir:"), "directory label survives: {text}");
        assert!(!text.contains("Path:"), "path label survives: {text}");
        assert!(!text.contains("Mode:"), "mode chrome survives: {text}");
        assert!(!text.contains("Search:"), "search row survives: {text}");

        // Path mode reuses the same row for the editable buffer.
        browser.cycle_mode();
        browser.cycle_mode();
        browser.manual_path = "~/Documents/typed.ord".to_string();
        let text = browser_text(&browser, &state);
        assert_eq!(
            text.lines().next(),
            Some("~/Documents/typed.ord"),
            "editable path row: {text}"
        );
        assert!(
            !text.contains(&current),
            "old path still appears beside the document: {text}"
        );
        assert!(!text.contains("Mode:"), "mode chrome survives: {text}");
        assert!(!text.contains("Search:"), "search row survives: {text}");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn browser_modal_is_fixed_and_its_title_is_centred() {
        use crate::file_browser::{FileBrowserState, BROWSER_WIDTH};
        use ordet_tui_framework::{Buffer, Frame, Point, Rect};

        let dir = unique_temp_dir("ordet-browser-title");
        std::fs::create_dir_all(&dir).expect("seed dir");
        let browser = FileBrowserState::open(dir.clone());
        let state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        let theme = state.theme_palette();
        let area = Rect::new(0, 0, 100, 40);
        let mut buffer = Buffer::new(area);
        {
            let mut frame = Frame::new(area, &mut buffer);
            super::render_file_browser(&mut frame, area, &state, &browser, theme);
        }

        let row: String = (0..BROWSER_WIDTH)
            .map(|x| {
                buffer
                    .cell(Point::new(x, 0))
                    .map(|cell| cell.symbol.as_str().to_string())
                    .unwrap_or_default()
            })
            .collect();
        let label = " Open ";
        let start_byte = row.find(label).expect("the title rides in the top border");
        let start = row[..start_byte].chars().count();
        let left_gap = start;
        let right_gap = BROWSER_WIDTH as usize - (start + label.chars().count());
        assert!(
            left_gap.abs_diff(right_gap) <= 1,
            "title should sit centred: {left_gap} left, {right_gap} right, row: {row:?}"
        );
        assert!(
            row.starts_with('┌') && row.ends_with('┐'),
            "the border still closes both corners: {row:?}"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn browser_reports_search_in_the_message_line() {
        use crate::file_browser::FileBrowserState;

        fn browser_text(browser: &FileBrowserState, state: &AppState) -> String {
            let theme = state.theme_palette();
            super::file_browser_list_lines(
                browser,
                state.ui.language,
                theme,
                theme.panel_alt_bg,
                200,
                30,
            )
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
        }

        let dir = unique_temp_dir("ordet-browser-search-message");
        std::fs::create_dir_all(&dir).expect("seed dir");
        std::fs::write(dir.join("alpha.ord"), "hello").expect("seed file");
        let state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        let mut browser = FileBrowserState::open(dir.clone());
        let current = dir.display().to_string();

        // Search mode with no query adds no instruction row.
        browser.cycle_mode();
        let text = browser_text(&browser, &state);
        assert_eq!(text.lines().next(), Some(current.as_str()));
        assert!(
            !text.contains("Search:"),
            "empty search prompt survives: {text}"
        );

        // Typing adds the live query, once.
        browser.push_char('a');
        let text = browser_text(&browser, &state);
        assert_eq!(text.lines().nth(1), Some("Search: a"), "live query: {text}");
        assert_eq!(
            text.matches("Search:").count(),
            1,
            "search shown more than once: {text}"
        );

        // The filter stays visible after cycling back to browse mode.
        browser.cycle_mode();
        browser.cycle_mode();
        assert_eq!(browser.mode, crate::file_browser::FileBrowserMode::Browse);
        let text = browser_text(&browser, &state);
        assert_eq!(
            text.lines().nth(1),
            Some("Search: a"),
            "active filter hidden: {text}"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn welcome_rail_drops_mode_hints_but_keeps_errors() {
        use crate::file_browser::FileBrowserState;

        fn rail_text(browser: &FileBrowserState, state: &AppState) -> String {
            let theme = state.theme_palette();
            let lines = super::file_browser_list_lines(
                browser,
                state.ui.language,
                theme,
                theme.panel_alt_bg,
                30,
                24,
            );
            lines
                .iter()
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|span| span.content.as_str())
                        .collect::<Vec<_>>()
                        .join("")
                })
                .collect::<Vec<_>>()
                .join("\n")
        }

        let dir = unique_temp_dir("ordet-browser-rail-hints");
        std::fs::create_dir_all(&dir).expect("seed dir");
        let state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );

        // Stock mode hints stay out of the rail…
        let browser = FileBrowserState::open(dir.clone());
        assert!(browser.is_mode_hint());
        let text = rail_text(&browser, &state);
        assert!(
            !text.contains("Browse mode. Enter opens."),
            "rail shows the mode hint: {text}"
        );
        // …and no stock instruction replaces it.
        assert!(
            !text.contains("[Tab, then type]"),
            "rail still shows a hint: {text}"
        );

        // …but errors still surface.
        let mut failing = FileBrowserState::open(dir.clone());
        failing.message = "Failed to read nowhere: nope".to_string();
        assert!(!failing.is_mode_hint());
        let text = rail_text(&failing, &state);
        assert!(
            text.contains("Failed to read nowhere"),
            "error hidden: {text}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn welcome_shows_the_browser_in_the_rail_instead_of_a_modal() {
        use crate::file_browser::FileBrowserState;
        use ordet_tui_framework::{Buffer, Frame, Point};

        let dir = unique_temp_dir("ordet-browser-rail");
        std::fs::write(dir.join("alpha.ord"), "hello").expect("seed file");
        let area = ordet_tui_framework::Rect::new(0, 0, 200, 60);
        let mut state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        assert!(state.show_welcome_screen());
        state.ui.file_browser = Some(FileBrowserState::open(dir.clone()));
        // The popup stays down on the home page...
        assert!(!super::should_render_browser_modal(&state));

        let theme = state.theme_palette();
        let layout = super::welcome_layout(area, state.welcome_side_panel_visible());
        let side = layout.side_panel_area.expect("rail panel");
        let mut buffer = Buffer::new(area);
        {
            let mut frame = Frame::new(area, &mut buffer);
            super::render_welcome_screen(&mut frame, area, &state, theme);
        }
        let mut panel_text = String::new();
        for y in side.y..side.bottom() {
            for x in side.x..side.right() {
                if let Some(cell) = buffer.cell(Point::new(x, y)) {
                    panel_text.push_str(&cell.symbol);
                }
            }
            panel_text.push('\n');
        }
        assert!(
            panel_text.contains("alpha.ord"),
            "rail shows no browser entries: {panel_text}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn browser_modal_stays_for_the_editor() {
        use crate::file_browser::FileBrowserState;

        let dir = unique_temp_dir("ordet-browser-modal");
        let mut state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        // A real draft leaves welcome: same open browser, popup allowed.
        state.add_draft(crate::editor_buffer::EditorBuffer::new(), None);
        assert!(!state.show_welcome_screen());
        state.ui.file_browser = Some(FileBrowserState::open(dir.clone()));
        assert!(super::should_render_browser_modal(&state));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn editor_renders_cjk_hebrew_and_emoji_without_overflow() {
        use ordet_tui_framework::{Buffer, Frame, Rect};

        let area = Rect::new(0, 0, 30, 10);
        let state = AppState::new(
            vec![(
                crate::editor_buffer::EditorBuffer::from_text(
                    "日本語テスト\nשלום עולם\nあいう\nmix 👨‍👩‍👧‍👦 end",
                ),
                None,
            )],
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        let theme = state.theme_palette();
        let mut buffer = Buffer::new(area);
        {
            let mut frame = Frame::new(area, &mut buffer);
            super::render_editor_buffer(
                &mut frame,
                area,
                &state.editor().buffer,
                0,
                theme,
                true,
                false,
            );
        }
        let mut text = String::new();
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                if let Some(cell) = buffer.cell(ordet_tui_framework::Point::new(x, y)) {
                    text.push_str(&cell.symbol);
                }
            }
            text.push('\n');
        }
        assert!(text.contains("日本語"), "kanji missing: {text}");
        assert!(text.contains("שלום"), "hebrew missing: {text}");
        assert!(text.contains("あいう"), "hiragana missing: {text}");
    }

    #[test]
    fn pages_panel_numbers_rows_in_list_order() {
        use ordet_tui_framework::{Buffer, Component, Frame, Point};

        let mut state = AppState::new(
            vec![
                (crate::editor_buffer::EditorBuffer::from_text("one"), None),
                (crate::editor_buffer::EditorBuffer::from_text("two"), None),
                (crate::editor_buffer::EditorBuffer::from_text("three"), None),
            ],
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        state.panels.open(crate::state::PanelKind::Pages);
        let area = ordet_tui_framework::Rect::new(0, 0, 140, 30);
        state.update_layout(area);

        let panel_rows = |state: &AppState| -> Vec<String> {
            let mut buffer = Buffer::new(area);
            {
                let pane = super::PanelView;
                let mut frame = Frame::new(area, &mut buffer);
                Component::render(&pane, state, &mut frame, area, false);
            }
            let outer = crate::state::panel_rect(state);
            let mut rows = Vec::new();
            for y in outer.y..outer.bottom() {
                let mut row = String::new();
                for x in outer.x..outer.right() {
                    if let Some(cell) = buffer.cell(Point::new(x, y)) {
                        row.push_str(&cell.symbol);
                    }
                }
                const LABELS: [&str; 10] = [
                    "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
                ];
                if LABELS.iter().any(|label| row.contains(label)) {
                    rows.push(row);
                }
            }
            rows
        };

        let rows = panel_rows(&state);
        assert!(rows[0].contains("1. one"), "first page numbered: {rows:?}");
        assert!(rows[1].contains("2. two"), "second page numbered: {rows:?}");
        assert!(
            rows[2].contains("3. three"),
            "third page numbered: {rows:?}"
        );

        // Reordering renumbers by position: three jumps to page two.
        assert!(state.move_draft(2, -1));
        let rows = panel_rows(&state);
        assert!(
            rows[1].contains("2. three"),
            "renumbered in place: {rows:?}"
        );
        assert!(rows[2].contains("3. two"), "renumbered in place: {rows:?}");

        // Ten pages, one row each, numbers read in list order.
        for name in ["four", "five", "six", "seven", "eight", "nine", "ten"] {
            state.add_draft(crate::editor_buffer::EditorBuffer::from_text(name), None);
        }
        let rows = panel_rows(&state);
        assert_eq!(rows.len(), 10, "one row per page: {rows:?}");
        assert!(rows[0].contains("1. one"), "first row: {rows:?}");
        assert!(rows[9].contains("10. ten"), "last row: {rows:?}");
    }

    #[test]
    fn mac_delete_key_trashes_from_the_pages_panel_like_forward_delete() {
        use crate::commands::Action;
        use ordet_tui_framework::{Component, EventCx};

        for command in [Command::Backspace, Command::DeleteForward] {
            let mut state = AppState::new(
                vec![(crate::editor_buffer::EditorBuffer::from_text("x"), None)],
                Vec::new(),
                ThemeSelection::default(),
                None,
                12,
                Some(false),
            );
            state.panels.open(crate::state::PanelKind::Pages);
            let mut pane = super::PanelView;
            let mut cx: EventCx<Action> = EventCx::new();
            pane.handle_event(&mut state, &UiEvent::Command(command), &mut cx);
            let (actions, _) = cx.into_parts();
            assert!(
                actions
                    .iter()
                    .any(|action| matches!(action, Action::TrashRequested { .. })),
                "{command:?} from the pages panel must request trash"
            );
        }
    }

    /// Flatten the painted cells into one string per row, so split layouts
    /// can be asserted without counting columns.
    fn rendered_text(buffer: &ordet_tui_framework::Buffer, area: Rect) -> String {
        let mut out = String::new();
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                out.push_str(
                    buffer
                        .cell(ordet_tui_framework::Point::new(x, y))
                        .map(|cell| cell.symbol.as_str())
                        .unwrap_or(" "),
                );
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn hiding_the_menu_bar_moves_the_text_up_one_row() {
        use ordet_tui_framework::{Buffer, Component, Frame};

        let area = Rect::new(0, 0, 80, 18);
        let mut state = AppState::new(
            vec![(
                crate::editor_buffer::EditorBuffer::from_text("first line"),
                None,
            )],
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        state.update_layout(area);

        let paint = |state: &AppState| {
            let mut buffer = Buffer::new(area);
            let mut frame = Frame::new(area, &mut buffer);
            super::RootView::new().render(state, &mut frame, area, false);
            rendered_text(&buffer, area)
                .lines()
                .map(str::to_string)
                .collect::<Vec<String>>()
        };

        // Shown: menus on row 0, the ruler on row 1, text below it.
        let rows = paint(&state);
        assert!(rows[0].contains("File"), "row 0 is the bar: {:?}", rows[0]);
        assert!(
            rows[1].contains('\u{25B6}'),
            "row 1 is the ruler: {:?}",
            rows[1]
        );
        assert!(
            rows[2].contains("first line"),
            "row 2 starts the text: {:?}",
            rows[2]
        );

        // Hidden: the ruler takes row 0 and the text row 1, one line higher.
        state.ui.show_menu_bar = false;
        state.update_layout(area);
        let rows = paint(&state);
        assert!(
            !rows[0].contains("File"),
            "the bar paints nothing: {:?}",
            rows[0]
        );
        assert!(
            rows[0].contains('\u{25B6}'),
            "row 0 is the ruler: {:?}",
            rows[0]
        );
        assert!(
            rows[1].contains("first line"),
            "row 1 starts the text: {:?}",
            rows[1]
        );
        // The status row keeps its place at the bottom either way.
        assert!(rows[17].contains("Insert"), "status row: {:?}", rows[17]);
    }

    #[test]
    fn a_split_paints_both_pages_with_a_separator() {
        use ordet_tui_framework::{Buffer, Component, Frame, Point};

        let area = Rect::new(0, 0, 120, 30);
        let mut state = AppState::new(
            vec![(
                crate::editor_buffer::EditorBuffer::from_text("LEFT PAGE"),
                None,
            )],
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        state.add_draft(
            crate::editor_buffer::EditorBuffer::from_text("RIGHT PAGE"),
            None,
        );
        state.update_layout(area);
        let rows = state.page_rows();
        assert!(state.open_window(rows[0].index));
        state.update_layout(area);
        assert_eq!(state.ui.layout.window_rects.len(), 2);

        let mut buffer = Buffer::new(area);
        let mut frame = Frame::new(area, &mut buffer);
        super::RootView::new().render(&state, &mut frame, area, false);

        let text = rendered_text(&buffer, area);
        assert!(text.contains("LEFT PAGE"), "left window paints: {text}");
        assert!(text.contains("RIGHT PAGE"), "right window paints: {text}");
        assert!(text.contains('\u{2502}'), "windows are separated: {text}");

        let theme = state.theme_palette();
        let rects = state.ui.layout.window_rects.clone();
        let window_drafts = state.window_drafts();
        let active = state.active_window_position();
        let plain_area = super::editor_text_area(rects[0]);
        let title_area = super::window_text_area(&state, rects[0]);
        assert_eq!(title_area.y, plain_area.y + 1, "title row pushes the text");
        assert_eq!(
            title_area.height,
            plain_area.height - 1,
            "the title row costs one text row"
        );
        let row_text = |y: u16| {
            (area.x..area.right())
                .map(|x| {
                    buffer
                        .cell(Point::new(x, y))
                        .map(|cell| cell.symbol.as_str().to_string())
                        .unwrap_or_default()
                })
                .collect::<String>()
        };

        // Each pane names its own page, and the focused pane's title wears
        // the accent while the other stays muted.
        for (position, rect) in rects.iter().enumerate() {
            let title = state.drafts[window_drafts[position]].page_label();
            let title_row: String = (rect.x..rect.right())
                .map(|x| {
                    buffer
                        .cell(Point::new(x, rect.y))
                        .map(|cell| cell.symbol.as_str().to_string())
                        .unwrap_or_default()
                })
                .collect();
            assert!(
                title_row.contains(&title),
                "pane {position} carries its own title {title:?}: {title_row:?}"
            );
            let leading = title_row.len() - title_row.trim_start().len();
            let trailing = title_row.len() - title_row.trim_end().len();
            assert!(
                leading.abs_diff(trailing) <= 1,
                "pane {position} title is centred: {title_row:?}"
            );
            let first_cell = (rect.x..rect.right())
                .find_map(|x| {
                    buffer
                        .cell(Point::new(x, rect.y))
                        .filter(|cell| cell.symbol != " ")
                })
                .expect("pane title paints");
            let title_fg = if position == active {
                theme.accent
            } else {
                theme.muted_fg
            };
            assert_eq!(
                first_cell.style.fg,
                Some(title_fg),
                "pane {position} title focus"
            );
            let ruler_fg = if position == active {
                theme.accent
            } else {
                theme.border
            };
            let ruler = (rect.x..rect.right())
                .find_map(|x| {
                    buffer
                        .cell(Point::new(x, rect.y.saturating_add(1)))
                        .filter(|cell| cell.symbol == "\u{25B6}")
                })
                .expect("pane ruler tab stop");
            assert_eq!(
                ruler.style.fg,
                Some(ruler_fg),
                "pane {position} ruler focus"
            );
        }

        // The header keeps the menus but drops the floating page name when
        // the panes name themselves.
        let header_row = row_text(state.ui.layout.header.y);
        assert!(
            header_row.contains("File"),
            "split header keeps the menus: {header_row:?}"
        );
        for draft in &state.drafts {
            assert!(
                !header_row.contains(&draft.page_label()),
                "split header must not name a pane: {header_row:?}"
            );
        }
    }

    #[test]
    fn a_four_way_split_survives_tiny_terminals() {
        use ordet_tui_framework::{Buffer, Component, Frame};

        for area in [
            Rect::new(0, 0, 10, 5),
            Rect::new(0, 0, 12, 7),
            Rect::new(0, 0, 20, 9),
        ] {
            let mut state = AppState::new(
                vec![(crate::editor_buffer::EditorBuffer::from_text("one"), None)],
                Vec::new(),
                ThemeSelection::default(),
                None,
                12,
                Some(false),
            );
            for title in ["two", "three", "four"] {
                state.add_draft(crate::editor_buffer::EditorBuffer::from_text(title), None);
            }
            state.update_layout(area);
            let rows = state.page_rows();
            for row in rows.iter().take(3) {
                state.open_window(row.index);
            }
            state.update_layout(area);

            let mut buffer = Buffer::new(area);
            let mut frame = Frame::new(area, &mut buffer);
            super::RootView::new().render(&state, &mut frame, area, false);
        }
    }

    #[test]
    fn a_three_way_split_draws_a_plain_separator() {
        use ordet_tui_framework::{Buffer, Component, Frame, Point};

        let area = Rect::new(0, 0, 120, 30);
        let mut state = AppState::new(
            vec![(
                crate::editor_buffer::EditorBuffer::from_text("one page"),
                None,
            )],
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        state.add_draft(
            crate::editor_buffer::EditorBuffer::from_text("two page"),
            None,
        );
        state.add_draft(
            crate::editor_buffer::EditorBuffer::from_text("three page"),
            None,
        );
        state.update_layout(area);
        let rows = state.page_rows();
        assert!(state.open_window(rows[0].index));
        assert!(state.open_window(rows[1].index));
        assert_eq!(state.window_count(), 3);
        state.update_layout(area);
        let rects = state.ui.layout.window_rects.clone();
        assert_eq!(rects.len(), 3);

        let mut buffer = Buffer::new(area);
        let mut frame = Frame::new(area, &mut buffer);
        super::RootView::new().render(&state, &mut frame, area, false);

        // Left pane full height, right column stacked: the row between the
        // right panes is a plain rule. The pane titles name each page.
        let separator: String = (rects[1].x..rects[1].right())
            .map(|x| {
                buffer
                    .cell(Point::new(x, rects[1].bottom()))
                    .map(|cell| cell.symbol.as_str())
                    .unwrap_or_default()
            })
            .collect();
        assert!(
            !separator.is_empty() && separator.chars().all(|ch| ch == '\u{2500}'),
            "stacked panes get a plain rule: {separator:?}"
        );
        let text = rendered_text(&buffer, area);
        assert!(
            text.contains("one page") && text.contains("two page"),
            "{text}"
        );
    }

    #[test]
    fn pages_panel_rows_match_the_mouse_hit_test() {
        use ordet_tui_framework::{Point, Rect};

        let mut state = AppState::new(
            vec![(crate::editor_buffer::EditorBuffer::from_text("x"), None)],
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        state.add_draft(crate::editor_buffer::EditorBuffer::from_text("y"), None);
        state.panels.open(crate::state::PanelKind::Pages);
        state.update_layout(Rect::new(0, 0, 120, 60));
        let area = crate::state::panel_rect(&state);
        let expected = state.page_rows().len();
        let mut seen = std::collections::BTreeSet::new();
        for y in area.y..area.bottom() {
            if let Some(index) = super::panel_row_at_point(&state, Point::new(area.x + 2, y)) {
                seen.insert(index);
            }
        }
        assert_eq!(seen.len(), expected, "every visible page row is clickable");
    }

    #[test]
    fn long_draft_title_is_capped_inside_the_header_gap() {
        use ordet_tui_framework::{Buffer, Frame, Point};

        // An untitled draft is named after its first line, so a long first
        // line used to paint the title straight over the menu bar and the
        // right-hand zone.
        let area = ordet_tui_framework::Rect::new(0, 0, 120, 24);
        let long_title = "W".repeat(120);
        let mut state = AppState::new(
            vec![(
                crate::editor_buffer::EditorBuffer::from_text(&long_title),
                None,
            )],
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        state.update_layout(area);
        let header = state.ui.layout.header;
        let theme = state.theme_palette();
        let mut buffer = Buffer::new(area);
        {
            let mut frame = Frame::new(area, &mut buffer);
            super::draw_header(&state, &mut frame, header, theme);
        }

        let mut row = String::new();
        let mut longest_run = 0usize;
        let mut run = 0usize;
        for x in header.x..header.right() {
            let symbol = buffer
                .cell(Point::new(x, header.y))
                .map(|cell| cell.symbol.as_str().to_string())
                .unwrap_or_default();
            if symbol == "W" {
                run += 1;
                longest_run = longest_run.max(run);
            } else {
                run = 0;
            }
            row.push_str(&symbol);
        }
        assert!(
            longest_run <= 30,
            "title painted {longest_run} columns, row: {row:?}"
        );
        // 27 W's plus "..." = the full 30-column cap; the title sits centred
        // on the bar and is pushed right of the menus only when the bar is
        // tight, never over a neighbour. (``WW`` skips the lone W of the
        // Window menu title.)
        let title_start = row.find("WW").expect("the title must be painted");
        let menus_end = row.find("Help").expect("menus paint") + 4;
        assert!(
            title_start > menus_end,
            "title must stay right of the menus: starts at {title_start}, row: {row:?}"
        );
        assert!(
            row.contains("File") && row.contains("Edit"),
            "menus must survive a long title, row: {row:?}"
        );
        // The header is a menu bar and a title; counts and position live in
        // the status row, so nothing can crowd them out here.
        assert!(
            !row.contains("Pg ") && !row.contains("L1 C1"),
            "the header carries no counts, row: {row:?}"
        );
    }

    #[test]
    fn the_page_name_is_centred_in_the_header_bar() {
        use ordet_tui_framework::{Buffer, Frame, Point, Rect};

        let area = Rect::new(0, 0, 120, 24);
        let mut state = AppState::new(
            vec![(
                crate::editor_buffer::EditorBuffer::from_text("Song of the Day"),
                None,
            )],
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        state.update_layout(area);
        let header = state.ui.layout.header;
        let theme = state.theme_palette();
        let mut buffer = Buffer::new(area);
        {
            let mut frame = Frame::new(area, &mut buffer);
            super::draw_header(&state, &mut frame, header, theme);
        }
        let row: String = (header.x..header.right())
            .map(|x| {
                buffer
                    .cell(Point::new(x, header.y))
                    .map(|cell| cell.symbol.as_str().to_string())
                    .unwrap_or_default()
            })
            .collect();

        let name = "Song of the Day";
        let title_start = row.find(name).expect("the title is painted");
        let menus_end = row.find("Help").expect("menus paint") + "Help".len();
        assert!(
            title_start > menus_end,
            "title must clear the menu bar: starts at {title_start}, row: {row:?}"
        );
        // The name reads as the middle of the bar itself, not as the middle
        // of the free space right of `Help`.
        let bar_middle = header.width as usize / 2;
        let title_middle = title_start + name.len() / 2;
        assert!(
            title_middle.abs_diff(bar_middle) <= 1,
            "title midpoint {title_middle} should sit at the bar's middle {bar_middle}: row {row:?}"
        );
    }

    #[test]
    fn status_row_shows_the_page_number_of_the_open_draft() {
        use ordet_tui_framework::{Buffer, Component, Frame, Point};

        let area = ordet_tui_framework::Rect::new(0, 0, 120, 24);
        let mut state = AppState::new(
            vec![
                (crate::editor_buffer::EditorBuffer::from_text("one"), None),
                (crate::editor_buffer::EditorBuffer::from_text("two"), None),
                (crate::editor_buffer::EditorBuffer::from_text("three"), None),
            ],
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        state.update_layout(area);

        let status_row = |state: &AppState| -> String {
            let mut buffer = Buffer::new(area);
            {
                let root = super::RootView {
                    editor: super::EditorPane,
                    panels: super::PanelView,
                    status: super::StatusBar,
                };
                let mut frame = Frame::new(area, &mut buffer);
                Component::render(&root, state, &mut frame, area, false);
            }
            let row = state.ui.layout.status;
            let mut text = String::new();
            for x in row.x..row.right() {
                text.push_str(
                    buffer
                        .cell(Point::new(x, row.y))
                        .map(|cell| cell.symbol.as_str())
                        .unwrap_or(" "),
                );
            }
            text
        };

        assert!(status_row(&state).contains("Pg 1/3"));
        state.switch_to_draft(1);
        assert!(status_row(&state).contains("Pg 2/3"));
        state.switch_to_draft(2);
        assert!(status_row(&state).contains("Pg 3/3"));

        // Reordering renumbers the page wherever it lands, and the open
        // page follows the row rather than the slot.
        assert!(state.move_draft(2, -1));
        assert_eq!(state.active_draft, 1);
        assert!(status_row(&state).contains("Pg 2/3"));
    }

    #[test]
    fn status_row_carries_no_hint_catalog() {
        use ordet_tui_framework::{Buffer, Component, Frame};

        let area = Rect::new(0, 0, 100, 24);
        let mut state = AppState::new(
            vec![(
                crate::editor_buffer::EditorBuffer::from_text("one two three"),
                None,
            )],
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        state.update_layout(area);

        let status_row = |state: &AppState| -> String {
            let mut buffer = Buffer::new(area);
            {
                let root = super::RootView {
                    editor: super::EditorPane,
                    panels: super::PanelView,
                    status: super::StatusBar,
                };
                let mut frame = Frame::new(area, &mut buffer);
                Component::render(&root, state, &mut frame, area, false);
            }
            let screen = rendered_text(&buffer, area);
            screen
                .lines()
                .nth(state.ui.layout.status.y as usize)
                .unwrap_or_default()
                .to_string()
        };

        // Mode, page, position, live counts.
        let row = status_row(&state);
        assert!(row.contains("Insert"), "row: {row:?}");
        assert!(row.contains("Pg 1/1"), "row: {row:?}");
        assert!(row.contains("L1 C1"), "row: {row:?}");
        assert!(row.contains("W:3 C:13"), "row: {row:?}");
        // The separator between fields.
        assert!(row.contains(" │ "), "row: {row:?}");
        // A bar, not a key catalog: no bracketed hints survive anywhere.
        assert!(!row.contains('['), "row: {row:?}");

        // Overwrite mode swaps the mode word, and the message keeps the
        // left zone.
        state.ui.insert_mode = false;
        assert!(status_row(&state).contains("Overwrite"));
        state.ui.insert_mode = true;
        state.set_status("Saved.");
        let row = status_row(&state);
        assert!(row.starts_with(" Saved."), "row: {row:?}");
        assert!(row.trim_end().ends_with("C:13"), "row: {row:?}");
    }

    #[test]
    fn header_title_never_panics_when_the_zones_fill_the_bar() {
        use ordet_tui_framework::{Buffer, Frame, Point};

        // Eight menus overflow these widths, so there is no room left for a
        // centred title. The bar must degrade to the menus alone: it must
        // not panic or clobber a neighbor.
        let full = ordet_tui_framework::Rect::new(0, 0, 120, 24);
        let long_title = "W".repeat(120);
        let mut state = AppState::new(
            vec![(
                crate::editor_buffer::EditorBuffer::from_text(&long_title),
                None,
            )],
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        state.update_layout(full);
        let theme = state.theme_palette();
        for width in [1u16, 20, 40, 60, 70, 80, 120] {
            let header = ordet_tui_framework::Rect::new(0, 0, width, 1);
            let mut buffer = Buffer::new(full);
            let mut frame = Frame::new(full, &mut buffer);
            super::draw_header(&state, &mut frame, header, theme);

            let mut row = String::new();
            for x in 0..width {
                row.push_str(
                    buffer
                        .cell(Point::new(x, 0))
                        .map(|cell| cell.symbol.as_str())
                        .unwrap_or(" "),
                );
            }
            if width >= 20 {
                assert!(row.contains("File"), "width {width}: {row:?}");
            }
            // Counts live in the status row at every width, never up here.
            assert!(!row.contains("Pg "), "width {width}: {row:?}");
        }
    }

    #[test]
    fn ruler_marks_the_margins_and_tab_stops() {
        let ruler = ruler_marks(20);
        assert_eq!(ruler.chars().next(), Some('├'));
        assert_eq!(ruler.chars().last(), Some('┤'));
        assert_eq!(ruler.chars().count(), 20);
        // `▶` on every eight-column stop, `─` in between.
        let marks: Vec<usize> = ruler
            .chars()
            .enumerate()
            .filter(|(_, ch)| *ch == '▶')
            .map(|(idx, _)| idx)
            .collect();
        assert_eq!(marks, vec![8, 16]);
        assert_eq!(ruler.chars().skip(1).take(7).collect::<String>(), "───────");

        // Degenerate widths never panic and stay honest.
        assert_eq!(ruler_marks(0), "");
        assert_eq!(ruler_marks(1), "├");
        assert_eq!(ruler_marks(2), "├┤");
        assert_eq!(ruler_marks(3), "├─┤");
        // The right margin owns its cell: the stop at column 8 gives way.
        assert_eq!(ruler_marks(9), "├───────┤");
        assert_eq!(ruler_marks(10), "├───────▶┤");
    }

    #[test]
    fn ruler_caret_splits_the_line_at_the_cursor() {
        // The caret cell is a plain `─` under the cursor's column.
        let (before, caret, after) = ruler_caret_split(20, Some(3));
        assert_eq!((before.as_str(), caret), ("├──", Some('─')));
        assert_eq!(after, "────▶───────▶──┤");

        // It overwrites whatever sat there, tab stop or margin, because the
        // caret is painted last.
        let (before, caret, after) = ruler_caret_split(20, Some(8));
        assert_eq!(
            (before.as_str(), caret, after.as_str()),
            ("├───────", Some('─'), "───────▶──┤")
        );
        let (before, caret, after) = ruler_caret_split(20, Some(0));
        assert_eq!(
            (before.as_str(), caret, after.as_str()),
            ("", Some('─'), "───────▶───────▶──┤")
        );

        // Without a caret the marks stand as painted, and columns off the
        // ruler are ignored rather than panicking.
        let (before, caret, after) = ruler_caret_split(20, None);
        assert_eq!(before, ruler_marks(20));
        assert_eq!((caret, after.as_str()), (None, ""));
        assert_eq!(ruler_caret_split(20, Some(99)).1, None);
        assert_eq!(ruler_caret_split(0, Some(0)).1, None);
    }

    #[test]
    fn editor_paints_the_ruler_above_the_first_text_row() {
        use ordet_tui_framework::{Buffer, Component, Frame, Point};

        let area = ordet_tui_framework::Rect::new(0, 0, 60, 10);
        let mut state = AppState::new(
            vec![(
                crate::editor_buffer::EditorBuffer::from_text("hello world"),
                None,
            )],
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        state.update_layout(area);
        let editor = state.ui.layout.editor;
        let mut buffer = Buffer::new(area);
        {
            let root = super::RootView {
                editor: super::EditorPane,
                panels: super::PanelView,
                status: super::StatusBar,
            };
            let mut frame = Frame::new(area, &mut buffer);
            Component::render(&root, &state, &mut frame, area, false);
        }

        // The ruler sits one row above the text, spanning the text columns:
        // margins and tab-stop ticks, with the caret's own cell painted
        // inverted. The cursor starts at column 0, so the left margin carries
        // the caret cell here.
        let text_area = super::editor_text_area(editor);
        let theme = state.theme_palette();
        let mut ruler_row = String::new();
        for x in text_area.x..text_area.right() {
            ruler_row.push_str(
                buffer
                    .cell(Point::new(x, editor.y))
                    .map(|cell| cell.symbol.as_str())
                    .unwrap_or(" "),
            );
        }
        assert!(ruler_row.ends_with('┤'), "row: {ruler_row:?}");
        assert!(ruler_row.contains('▶'), "row: {ruler_row:?}");
        let caret = buffer
            .cell(Point::new(text_area.x, editor.y))
            .expect("the caret cell is painted");
        assert_eq!(caret.symbol.as_str(), "─", "row: {ruler_row:?}");
        assert_eq!(caret.style.bg, Some(theme.muted_fg), "inverted caret");
        assert_eq!(caret.style.fg, Some(theme.panel_bg), "inverted caret");

        state.editor_mut().buffer.set_cursor_with_anchor(3, 3);
        let mut buffer = Buffer::new(area);
        {
            let root = super::RootView {
                editor: super::EditorPane,
                panels: super::PanelView,
                status: super::StatusBar,
            };
            let mut frame = Frame::new(area, &mut buffer);
            Component::render(&root, &state, &mut frame, area, false);
        }
        let mut ruler_row = String::new();
        for x in text_area.x..text_area.right() {
            ruler_row.push_str(
                buffer
                    .cell(Point::new(x, editor.y))
                    .map(|cell| cell.symbol.as_str())
                    .unwrap_or(" "),
            );
        }
        assert!(ruler_row.starts_with("├──"), "row: {ruler_row:?}");
        assert_eq!(
            buffer
                .cell(Point::new(text_area.x + 3, editor.y))
                .map(|cell| cell.symbol.as_str().to_string())
                .unwrap_or_default(),
            "─",
            "row: {ruler_row:?}"
        );

        state.ui.show_ruler = false;
        let mut buffer = Buffer::new(area);
        {
            let root = super::RootView {
                editor: super::EditorPane,
                panels: super::PanelView,
                status: super::StatusBar,
            };
            let mut frame = Frame::new(area, &mut buffer);
            Component::render(&root, &state, &mut frame, area, false);
        }
        let mut ruler_row = String::new();
        for x in text_area.x..text_area.right() {
            ruler_row.push_str(
                buffer
                    .cell(Point::new(x, editor.y))
                    .map(|cell| cell.symbol.as_str())
                    .unwrap_or(" "),
            );
        }
        assert!(
            ruler_row.trim().is_empty(),
            "ruler still painted with show_ruler off: {ruler_row:?}"
        );
    }

    #[test]
    fn an_open_panel_always_has_a_painted_rect() {
        use crate::state::PanelKind;
        use ordet_tui_framework::Rect;

        let mut state = AppState::new(
            vec![(crate::editor_buffer::EditorBuffer::new(), None)],
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        state.panels.open(PanelKind::Notes);
        state.update_layout(Rect::new(0, 0, 120, 30));
        let rect = crate::state::panel_rect(&state);
        assert!(
            rect.width > 0 && rect.height > 0,
            "an open panel must have geometry to paint: {rect:?}"
        );

        state.panels.close();
        state.update_layout(Rect::new(0, 0, 120, 30));
        let rect = crate::state::panel_rect(&state);
        assert_eq!(
            (rect.width, rect.height),
            (0, 0),
            "a closed panel paints nothing"
        );
    }

    #[test]
    fn the_panel_name_sits_in_the_middle_of_the_top_border() {
        use ordet_tui_framework::{Buffer, Frame, Point, Rect};

        let state = AppState::new(
            vec![(crate::editor_buffer::EditorBuffer::new(), None)],
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        let theme = state.theme_palette();
        let area = Rect::new(0, 0, 80, 12);
        let outer = Rect::new(6, 2, 32, 8);
        let mut buffer = Buffer::new(area);
        {
            let mut frame = Frame::new(area, &mut buffer);
            super::draw_panel_frame(&mut frame, outer, "Word Stats", theme);
        }

        let row: String = (outer.x..outer.right())
            .map(|x| {
                buffer
                    .cell(Point::new(x, outer.y))
                    .map(|cell| cell.symbol.as_str().to_string())
                    .unwrap_or_default()
            })
            .collect();
        // A clean space opens the label, so the border strokes leading into
        // it are the top edge itself, not an extra `─` quoted into the title.
        let label = " Word Stats ";
        let start_byte = row.find(label).expect("the name rides in the top border");
        let start = row[..start_byte].chars().count();
        let label_width = label.chars().count();
        let left_gap = start;
        let right_gap = (outer.width as usize).saturating_sub(start + label_width);
        assert!(
            left_gap.abs_diff(right_gap) <= 1,
            "name should sit centred: {left_gap} left, {right_gap} right, row: {row:?}"
        );
        assert!(
            row.starts_with('┌') && row.ends_with('┐'),
            "the border still closes both corners: {row:?}"
        );
    }

    #[test]
    fn settings_rows_cover_every_persisted_preference() {
        let state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        let rows = settings_rows(&state);
        let labels: Vec<&str> = rows.iter().map(|row| row.label).collect();
        assert_eq!(
            labels,
            vec![
                "Theme",
                "Accent",
                "Line numbers",
                "Ruler",
                "Menu bar",
                "Live counts",
                "Word target",
                "Autosave",
                "Indent width",
                "Automatic backups",
                "Back up on close",
                "Keep backups",
                "Backup location",
                "Back up now",
            ]
        );
        assert!(rows.iter().all(|row| !row.value.is_empty()));
    }

    #[test]
    fn about_modal_shows_session_and_document_status() {
        use ordet_tui_framework::{Buffer, Frame, Point};

        let area = Rect::new(0, 0, 100, 30);
        let mut state = AppState::new(
            vec![(
                crate::editor_buffer::EditorBuffer::from_text("one two three"),
                None,
            )],
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        state.update_layout(area);
        let theme = state.theme_palette();
        let mut buffer = Buffer::new(area);
        {
            let mut frame = Frame::new(area, &mut buffer);
            super::render_about_modal(&mut frame, area, &state, theme);
        }

        let mut text = String::new();
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                if let Some(cell) = buffer.cell(Point::new(x, y)) {
                    text.push_str(&cell.symbol);
                }
            }
            text.push('\n');
        }

        assert!(text.contains("About Ordet"), "sheet needs its title");
        assert!(
            text.contains(&format!("Ordet v{}", env!("CARGO_PKG_VERSION"))),
            "sheet needs the version"
        );
        for row in [
            "SESSION",
            "Build",
            "Terminal",
            "Theme",
            "Language",
            "DOCUMENT",
            "Page",
            "Cursor",
            "Words",
            "Characters",
            "Lines",
            "Modified",
        ] {
            assert!(text.contains(row), "sheet lost its {row} row: {text}");
        }
        assert!(text.contains("100 x 30"), "sheet needs the terminal size");
        assert!(text.contains("L1 C1"), "sheet needs the cursor position");
        assert!(
            text.contains("MIT OR Apache-2.0"),
            "sheet needs the license"
        );
        assert!(
            text.contains("github.com/LeonardoAmadeo/ordet"),
            "sheet needs the repository"
        );
        assert!(
            text.contains("Esc or Enter closes"),
            "sheet needs a visible dismissal hint"
        );
    }

    #[test]
    fn settings_picker_modal_lists_every_choice_with_a_marker() {
        use ordet_tui_framework::{Buffer, Frame, Point};

        let area = Rect::new(0, 0, 120, 30);
        let state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        let theme = state.theme_palette();
        let options: Vec<String> = crate::theme::ThemePreset::ALL
            .iter()
            .map(|preset| preset.name().to_string())
            .collect();
        assert_eq!(options.len(), 13);
        let picker = crate::state::SettingsPickerState {
            kind: crate::state::SettingsPickerKind::Theme,
            title: "Theme".to_string(),
            options: options.clone(),
            selected: 2,
            scroll: 0,
            active: 0,
            preview_from: None,
        };
        let mut buffer = Buffer::new(area);
        {
            let mut frame = Frame::new(area, &mut buffer);
            super::render_settings_picker_modal(&mut frame, area, &state, &picker, theme);
        }
        let mut text = String::new();
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                if let Some(cell) = buffer.cell(Point::new(x, y)) {
                    text.push_str(&cell.symbol);
                }
            }
            text.push('\n');
        }
        // Every choice visible at once: the whole point of the picker.
        for option in &options {
            assert!(text.contains(option), "missing option: {option}");
        }
        assert!(text.contains("●"), "current value needs its marker");
        assert!(text.contains("Select theme"), "title names the setting");
        let modal = crate::state::settings_picker_modal_rect(area, options.len());
        let inner = modal.inset(1, 1);
        let stalker_row = text
            .lines()
            .position(|line| line.contains("Stalker"))
            .expect("highlighted row");
        let y = area.y + stalker_row as u16;
        let highlighted = buffer.cell(Point::new(inner.x, y)).expect("highlight cell");
        assert_eq!(highlighted.style.bg, Some(theme.accent));
    }

    #[test]
    fn menu_and_settings_highlights_share_one_recipe() {
        use ordet_tui_framework::{Buffer, Frame, Point};

        // Both highlights must stay the same style forever: the menu title,
        // the menu rows and the picker rows are one visual control.
        let area = Rect::new(0, 0, 120, 30);
        let mut state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        state.update_layout(area);
        let theme = state.theme_palette();

        state.ui.menu_open = Some(0);
        state.ui.menu_selected = 0;
        let mut menu_buffer = Buffer::new(area);
        {
            let mut frame = Frame::new(area, &mut menu_buffer);
            super::render_menu_dropdown(&mut frame, &state, 0, theme);
        }

        let picker = crate::state::SettingsPickerState {
            kind: crate::state::SettingsPickerKind::Theme,
            title: "Theme".to_string(),
            options: vec!["Midnight".to_string(), "Ordet".to_string()],
            selected: 1,
            scroll: 0,
            active: 0,
            preview_from: None,
        };
        let mut picker_buffer = Buffer::new(area);
        {
            let mut frame = Frame::new(area, &mut picker_buffer);
            super::render_settings_picker_modal(&mut frame, area, &state, &picker, theme);
        }

        // First accent-backed row cell in each render, skipping the menu
        // title row (same recipe, different row).
        let find_row_highlight = |buffer: &Buffer| {
            for y in 1..30 {
                for x in 0..120 {
                    if let Some(cell) = buffer.cell(Point::new(x, y)) {
                        if cell.style.bg == Some(theme.accent) {
                            return cell.style;
                        }
                    }
                }
            }
            panic!("no highlighted row painted");
        };
        assert_eq!(
            find_row_highlight(&menu_buffer),
            find_row_highlight(&picker_buffer),
            "menu rows and picker rows must highlight identically"
        );
    }

    #[test]
    fn help_panel_width_hugs_its_longest_rendered_line() {
        use ordet_tui_framework::{Buffer, Frame, Point, Rect};
        use unicode_width::UnicodeWidthStr;

        let state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        let theme = state.theme_palette();
        // The action rows pin their shortcuts to the right edge, so the
        // widest rendered row is exactly the content width.
        let content_width = super::help_panel_width() - super::PANEL_BORDER_COLUMNS as u16;
        let area = Rect::new(0, 0, content_width, 200);
        let mut buffer = Buffer::new(area);
        {
            let mut frame = Frame::new(area, &mut buffer);
            super::render_help_panel(&mut frame, area, &state, theme);
        }

        let longest = (area.y..area.bottom())
            .map(|y| {
                let row: String = (area.x..area.right())
                    .filter_map(|x| buffer.cell(Point::new(x, y)))
                    .map(|cell| cell.symbol.as_str())
                    .collect();
                UnicodeWidthStr::width(row.trim_end())
            })
            .max()
            .unwrap_or(0);
        assert_eq!(
            longest, content_width as usize,
            "the card must be exactly as wide as its longest row"
        );
    }

    #[test]
    fn help_panel_puts_actions_left_and_shortcuts_right_on_tabbed_pages() {
        use ordet_tui_framework::{Buffer, Frame, Point};
        use unicode_width::UnicodeWidthStr;

        let mut state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        state.panels.open(PanelKind::Help);
        let theme = state.theme_palette();
        let content_width = super::help_panel_width() - super::PANEL_BORDER_COLUMNS as u16;
        let area = Rect::new(0, 0, content_width, super::help_panel_rows() as u16);
        let mut buffer = Buffer::new(area);
        {
            let mut frame = Frame::new(area, &mut buffer);
            super::render_help_panel(&mut frame, area, &state, theme);
        }

        let row_text = |y: u16| -> String {
            (area.x..area.right())
                .filter_map(|x| buffer.cell(Point::new(x, y)))
                .map(|cell| cell.symbol.as_str())
                .collect()
        };

        let tabs = row_text(area.y);
        assert!(tabs.contains("[ Movement ]"), "selected tab: {tabs:?}");
        for title in ["Writing", "Workspace", "Tools", "Blocks"] {
            assert!(tabs.contains(title), "missing tab {title}: {tabs:?}");
        }

        let action = (area.y..area.bottom())
            .map(row_text)
            .find(|row| row.starts_with("Previous word"))
            .expect("Movement action row");
        let shortcut = "⌥←";
        let shortcut_start = action.rfind(shortcut).expect("right-aligned shortcut");
        assert_eq!(
            UnicodeWidthStr::width(&action[..shortcut_start]),
            content_width as usize - UnicodeWidthStr::width(shortcut),
            "shortcut must sit on the right edge: {action:?}"
        );

        let footer = row_text(area.bottom() - 1);
        assert!(footer.contains("[Tab / ←→]: Switch Section"));
        assert!(footer.contains("[Esc]: Close."));

        let all_rows: String = (area.y..area.bottom()).map(row_text).collect();
        assert!(
            !all_rows.contains("⌃K A"),
            "the full prefix dump must stay out of the modal"
        );
        assert!(
            !all_rows.contains("⌃O C"),
            "alignment is consolidated into one row"
        );
    }

    #[test]
    fn every_help_tab_fits_its_actions_without_ellipsis() {
        use ordet_tui_framework::{Buffer, Frame, Point};

        let mut state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        let theme = state.theme_palette();
        let content_width = super::help_panel_width() - super::PANEL_BORDER_COLUMNS as u16;
        let area = Rect::new(0, 0, content_width, super::help_panel_rows() as u16);

        for tab in HelpTab::ALL {
            state.panels.help_tab = tab;
            let mut buffer = Buffer::new(area);
            {
                let mut frame = Frame::new(area, &mut buffer);
                super::render_help_panel(&mut frame, area, &state, theme);
            }
            for y in (area.y + 2)..(area.bottom() - 1) {
                let row: String = (area.x..area.right())
                    .filter_map(|x| buffer.cell(Point::new(x, y)))
                    .map(|cell| cell.symbol.as_str())
                    .collect();
                assert!(
                    !row.contains("..."),
                    "{} row {y} truncated an action: {row:?}",
                    tab.title()
                );
            }
        }
    }

    #[test]
    fn editor_help_tabs_cover_core_commands_and_drop_staples() {
        let actual: HashSet<Command> = EDITOR_HELP_SECTIONS
            .iter()
            .flat_map(|section| section.entries.iter())
            .flat_map(|entry| entry.commands.iter().copied())
            .collect();

        let core = [
            Command::MoveWordLeft,
            Command::MoveWordRight,
            Command::SelectLeft,
            Command::SelectRight,
            Command::SelectUp,
            Command::SelectDown,
            Command::MoveHome,
            Command::MoveEnd,
            Command::PageUp,
            Command::PageDown,
            Command::JumpPreviousPosition,
            Command::MoveDocumentTop,
            Command::MoveDocumentBottom,
            Command::MoveWindowTop,
            Command::MoveWindowBottom,
            Command::InsertTab,
            Command::Outdent,
            Command::CycleCase,
            Command::ToggleBold,
            Command::ToggleItalic,
            Command::BlockTitle,
            Command::BlockHeading,
            Command::BlockQuote,
            Command::BlockBody,
            Command::InsertTimestamp,
            Command::CopySelection,
            Command::CutSelection,
            Command::PasteClipboard,
            Command::Undo,
            Command::Redo,
            Command::AlignLeft,
            Command::AlignCenter,
            Command::AlignRight,
            Command::AlignJustify,
            Command::OpenLine,
            Command::DeleteLine,
            Command::DeleteToEndOfLine,
            Command::DeleteWordLeft,
            Command::DeleteWordForward,
            Command::ToggleInsertMode,
            Command::NewDraft,
            Command::OpenFilePrompt,
            Command::OpenReadFilePrompt,
            Command::Save,
            Command::OpenSavePathPrompt,
            Command::TrashFile,
            Command::NextPage,
            Command::PreviousPage,
            Command::NextWindow,
            Command::CloseWindow,
            Command::Quit,
            Command::ToggleNotes,
            Command::ToggleZoom,
            Command::ToggleStats,
            Command::OpenWordTargetPrompt,
            Command::CycleTheme,
            Command::OpenThemePrompt,
            Command::OpenCustomColorPrompt,
            Command::ToggleSettings,
            Command::ToggleRevisions,
            Command::OpenWorkspacePrompt,
            Command::OpenStatusPrompt,
            Command::MoveToBlockStart,
            Command::MoveToBlockEnd,
            Command::MarkBlockBegin,
            Command::MarkBlockEnd,
            Command::CopyBlock,
            Command::MoveBlock,
            Command::DeleteBlock,
            Command::OpenWriteBlockPrompt,
        ];
        for command in core {
            assert!(actual.contains(&command), "help lost {command:?}");
        }

        for staple in [
            Command::MoveLeft,
            Command::MoveRight,
            Command::MoveUp,
            Command::MoveDown,
            Command::Submit,
            Command::Backspace,
            Command::DeleteForward,
            Command::SelectAll,
            Command::OpenAlignmentPrompt,
            Command::ToggleHelp,
        ] {
            assert!(
                !actual.contains(&staple),
                "staple {staple:?} should not be in the trimmed help"
            );
        }

        let tabs: Vec<HelpTab> = EDITOR_HELP_SECTIONS
            .iter()
            .map(|section| section.tab)
            .collect();
        assert_eq!(tabs, HelpTab::ALL);
        for section in EDITOR_HELP_SECTIONS {
            assert!(
                (8..=12).contains(&section.entries.len()),
                "{} has {} rows, want a short page",
                section.tab.title(),
                section.entries.len()
            );
        }
    }

    #[test]
    fn welcome_help_covers_editor_help_commands() {
        let editor_commands: HashSet<Command> = EDITOR_HELP_SECTIONS
            .iter()
            .flat_map(|section| section.entries.iter())
            .flat_map(|entry| entry.commands.iter().copied())
            .collect();
        let welcome_commands: HashSet<Command> = WELCOME_MENU_HELP
            .iter()
            .chain(
                EDITOR_HELP_SECTIONS
                    .iter()
                    .flat_map(|section| section.entries.iter()),
            )
            .flat_map(|entry| entry.commands.iter().copied())
            .collect();

        assert!(editor_commands.is_subset(&welcome_commands));
        assert!(welcome_commands.contains(&Command::Submit));
        assert!(welcome_commands.contains(&Command::MoveLeft));
    }

    #[test]
    fn help_lines_wrap_to_fit_narrow_widths() {
        let state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        let theme = state.theme_palette();

        let editor_lines = welcome_help_lines(EDITOR_WORKSPACE_HELP, 18, theme, theme.panel_alt_bg);
        assert!(editor_lines.iter().all(|line| line.width() <= 18));

        let welcome_lines = welcome_shortcut_lines(&state, 18, theme, theme.panel_alt_bg);
        assert!(welcome_lines.iter().all(|line| line.width() <= 18));
    }

    #[test]
    fn wrapped_help_descriptions_continue_under_the_description_column() {
        let state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        let theme = state.theme_palette();

        let entries = [HelpEntry {
            commands: &[],
            keys: "Enter",
            description: "open the selected option or enter help",
        }];
        let lines = welcome_help_lines(&entries, 18, theme, theme.panel_alt_bg);
        let rendered = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let description_index = rendered
            .iter()
            .position(|line| line.contains("open"))
            .expect("wrapped help line");
        let continuation = rendered
            .get(description_index + 1)
            .expect("wrapped continuation line");

        assert!(continuation.starts_with(" "));
    }

    #[test]
    fn welcome_help_uses_one_description_column_across_sections() {
        let state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        let theme = state.theme_palette();
        let menu = welcome_help_lines(WELCOME_MENU_HELP, 34, theme, theme.panel_alt_bg);
        let editor = welcome_help_lines(EDITOR_WORKSPACE_HELP, 34, theme, theme.panel_alt_bg);
        let menu_description = menu
            .iter()
            .find(|line| line.spans.len() == 2)
            .expect("menu description row");
        let editor_description = editor
            .iter()
            .find(|line| line.spans.len() == 2)
            .expect("workspace description row");
        assert_eq!(
            unicode_width::UnicodeWidthStr::width(menu_description.spans[0].content.as_str()),
            unicode_width::UnicodeWidthStr::width(editor_description.spans[0].content.as_str())
        );
    }

    #[test]
    fn welcome_help_enters_scroll_mode_on_submit() {
        let mut state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        state.update_layout(Rect::new(0, 0, 100, 30));
        state.set_welcome_selection(WelcomeOption::Help);

        let mut cx = EventCx::new();
        handle_welcome_event(&mut state, &UiEvent::Command(Command::Submit), &mut cx);

        assert!(state.welcome_help_active());
        assert!(cx.into_parts().0.is_empty());
    }

    #[test]
    fn welcome_help_scrolls_without_changing_selection_when_focused() {
        let mut state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        state.update_layout(Rect::new(0, 0, 100, 30));
        state.set_welcome_selection(WelcomeOption::Help);
        state.focus_welcome_help();

        let mut cx = EventCx::new();
        handle_welcome_event(&mut state, &UiEvent::Command(Command::MoveDown), &mut cx);

        assert_eq!(state.ui.welcome_selected, WelcomeOption::Help);
        assert!(state.welcome_help_active());
        assert_eq!(state.ui.welcome_help_scroll, 1);
        assert!(cx.into_parts().0.is_empty());
    }

    #[test]
    fn welcome_help_stops_scrolling_at_the_bottom() {
        let mut state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        state.update_layout(Rect::new(0, 0, 100, 30));
        state.set_welcome_selection(WelcomeOption::Help);
        state.focus_welcome_help();

        let max_scroll = welcome_help_max_scroll(&state);
        let mut cx = EventCx::new();
        for _ in 0..(max_scroll + 10) {
            handle_welcome_event(&mut state, &UiEvent::Command(Command::MoveDown), &mut cx);
        }

        assert_eq!(state.ui.welcome_help_scroll, max_scroll);
        assert!(cx.into_parts().0.is_empty());
    }

    #[test]
    fn welcome_help_end_jumps_to_the_bottom() {
        let mut state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        state.update_layout(Rect::new(0, 0, 100, 30));
        state.set_welcome_selection(WelcomeOption::Help);
        state.focus_welcome_help();

        let max_scroll = welcome_help_max_scroll(&state);
        let mut cx = EventCx::new();
        handle_welcome_event(&mut state, &UiEvent::Command(Command::MoveEnd), &mut cx);

        assert_eq!(state.ui.welcome_help_scroll, max_scroll);
        assert!(cx.into_parts().0.is_empty());
    }

    #[test]
    fn welcome_help_reclamps_when_the_layout_changes() {
        let mut state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        // Same width, taller viewport: wrapped line counts cannot grow, so
        // the max can only shrink. (Across widths the rail squeezes help
        // narrow and scroll may legitimately grow with the terminal.)
        state.update_layout(Rect::new(0, 0, 120, 24));
        state.set_welcome_selection(WelcomeOption::Help);
        state.focus_welcome_help();

        let old_max_scroll = welcome_help_max_scroll(&state);
        state.set_welcome_help_scroll(old_max_scroll);

        state.update_layout(Rect::new(0, 0, 120, 40));
        let new_max_scroll = welcome_help_max_scroll(&state);
        assert!(new_max_scroll <= old_max_scroll);

        let mut cx = EventCx::new();
        handle_welcome_event(&mut state, &UiEvent::Command(Command::MoveDown), &mut cx);

        assert_eq!(state.ui.welcome_help_scroll, new_max_scroll);
        assert!(cx.into_parts().0.is_empty());
    }

    #[test]
    fn wrap_and_gutter_steals_columns_for_numbers() {
        let buffer = crate::editor_buffer::EditorBuffer::from_text("one\ntwo\nthree");
        // 3 source lines: 1 digit + 1 space.
        assert_eq!(super::wrap_and_gutter(80, &buffer, true), (78, 2));
        assert_eq!(super::wrap_and_gutter(80, &buffer, false), (80, 0));
        // Too narrow for both: the gutter loses, text keeps everything.
        assert_eq!(super::wrap_and_gutter(2, &buffer, true), (2, 0));

        let big = crate::editor_buffer::EditorBuffer::from_text(
            &(0..150)
                .map(|i| format!("line {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        // 150 lines: 3 digits + 1 space.
        assert_eq!(super::wrap_and_gutter(80, &big, true), (76, 4));
    }

    #[test]
    fn middle_truncation_keeps_both_ends_for_paths() {
        assert_eq!(fit_text_to_width("kokokokokokokokoko", 12), "kokok...koko");
    }

    #[test]
    fn tail_truncation_keeps_the_front_of_labels() {
        assert_eq!(
            fit_text_to_width_tail("kokokokokokokokoko", 12),
            "kokokokok..."
        );
    }

    #[test]
    fn split_to_width_wraps_long_values_without_ellipsis() {
        use super::split_to_width;

        assert_eq!(split_to_width("off", 10), vec!["off".to_string()]);
        assert_eq!(
            split_to_width("/Users/a/bb", 8),
            vec!["/Users/a".to_string(), "/bb".to_string()]
        );
        // Rejoining the chunks restores the full text: nothing is cut.
        let path = "/Users/leonardoamadeo/.config/ordet/settings.json";
        let chunks = split_to_width(path, 20);
        assert!(chunks.len() > 1);
        assert!(!chunks.iter().any(|chunk| chunk.contains("...")));
        assert_eq!(chunks.concat(), path);
    }

    #[test]
    fn welcome_help_left_returns_to_menu_navigation() {
        let mut state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        state.update_layout(Rect::new(0, 0, 100, 30));
        state.set_welcome_selection(WelcomeOption::Help);
        state.focus_welcome_help();

        let mut cx = EventCx::new();
        handle_welcome_event(&mut state, &UiEvent::Command(Command::MoveLeft), &mut cx);

        assert_eq!(state.ui.welcome_selected, WelcomeOption::Help);
        assert!(!state.welcome_help_active());
        assert!(cx.into_parts().0.is_empty());
    }

    #[test]
    fn welcome_help_esc_returns_to_menu_navigation() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        state.update_layout(Rect::new(0, 0, 100, 30));
        state.set_welcome_selection(WelcomeOption::Help);
        state.focus_welcome_help();
        assert!(state.welcome_help_active());

        let esc = UiEvent::Key(KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        });
        let mut cx = EventCx::new();
        handle_welcome_event(&mut state, &esc, &mut cx);

        assert_eq!(state.ui.welcome_selected, WelcomeOption::Help);
        assert!(!state.welcome_help_active());
        assert!(cx.into_parts().0.is_empty());
    }

    #[test]
    fn welcome_esc_on_the_plain_menu_changes_nothing() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        state.update_layout(Rect::new(0, 0, 100, 30));
        state.set_welcome_selection(WelcomeOption::NewDraft);

        let esc = UiEvent::Key(KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        });
        let mut cx = EventCx::new();
        handle_welcome_event(&mut state, &esc, &mut cx);

        assert_eq!(state.ui.welcome_selected, WelcomeOption::NewDraft);
        assert!(!state.welcome_help_active());
        assert!(cx.into_parts().0.is_empty());
    }

    #[test]
    fn welcome_help_right_enters_scroll_mode() {
        let mut state = AppState::new(
            Vec::new(),
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        state.update_layout(Rect::new(0, 0, 100, 30));
        state.set_welcome_selection(WelcomeOption::Help);

        let mut cx = EventCx::new();
        handle_welcome_event(&mut state, &UiEvent::Command(Command::MoveRight), &mut cx);

        assert!(state.welcome_help_active());
        assert!(cx.into_parts().0.is_empty());
    }
}
