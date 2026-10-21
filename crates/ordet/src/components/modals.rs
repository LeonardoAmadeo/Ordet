use super::*;

/// The open menu's dropdown, hanging off its title over the body. Rows
/// highlight like the pickers; the renderer, keys and mouse share the rect
/// math in `menu_drop_rect` / `menu_drop_inner_rect` so a click runs the
/// painted row.
///
/// The dropdown owns a bordered box (same chrome as the pickers and file
/// browser). Without the border the menu's dark fill sits directly against
/// the editor's dark fill and same-row editor cells left/right of the menu
/// read as one merged line.
pub(super) fn render_menu_dropdown(
    frame: &mut Frame<'_>,
    model: &AppState,
    open: usize,
    theme: ThemePalette,
) {
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

pub(super) fn render_settings_picker_modal(
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

pub(super) fn render_about_modal(
    frame: &mut Frame<'_>,
    area: Rect,
    model: &AppState,
    theme: ThemePalette,
) {
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

pub(super) fn about_lines(
    model: &AppState,
    area: Rect,
    width: usize,
    theme: ThemePalette,
) -> Vec<Line> {
    let editor = model.editor();
    let stats = editor.word_stats_if_ready();
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
        about_stat(
            "Words",
            stats.as_ref().map_or_else(
                || "counting...".to_string(),
                |stats| stats.word_count.to_string(),
            ),
            width,
            theme,
        ),
        about_stat(
            "Characters",
            stats.as_ref().map_or_else(
                || "counting...".to_string(),
                |stats| stats.chars.to_string(),
            ),
            width,
            theme,
        ),
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

pub(super) fn value_width(width: usize) -> usize {
    width.saturating_sub(15).max(1)
}

pub(super) fn about_stat(
    label: &str,
    value: impl ToString,
    width: usize,
    theme: ThemePalette,
) -> Line {
    let value = fit_text_to_width(&value.to_string(), value_width(width));
    stat_line_on(label, value, theme, theme.panel_alt_bg)
}

/// The browser's rows for any surface: modal popup or welcome rail.
/// The location is one bare path row; a live search query shares the message
/// row with errors and action reports, while stock mode instructions are
/// dropped entirely.
pub(super) fn file_browser_list_lines(
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
pub(super) fn browser_message_line(
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

pub(super) fn render_file_browser(
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
