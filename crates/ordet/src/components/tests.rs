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
