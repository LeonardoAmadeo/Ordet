use super::modals::file_browser_list_lines;
use super::*;

pub(super) fn render_welcome_screen(
    frame: &mut Frame<'_>,
    area: Rect,
    model: &AppState,
    theme: ThemePalette,
) {
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
pub(super) fn render_welcome_strip(
    _frame: &mut Frame<'_>,
    _area: Rect,
    _model: &AppState,
    _theme: ThemePalette,
    _background: Color,
) {
}

pub(super) fn render_welcome_logo(
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

pub(super) fn welcome_wordmark_lines(
    theme: ThemePalette,
    background: Color,
    width: u16,
) -> Vec<Line> {
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

pub(super) fn render_welcome_menu(
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

pub(super) fn render_welcome_side_panel(
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
pub(super) fn render_welcome_browser(
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
pub(super) fn should_render_browser_modal(model: &AppState) -> bool {
    model.ui.file_browser.is_some() && !model.show_welcome_screen()
}

pub(super) fn welcome_action_lines(
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

pub(super) fn render_welcome_recent(
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

pub(super) fn render_welcome_shortcuts(
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

pub(super) fn welcome_shortcut_lines(
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
pub(super) fn welcome_help_lines(
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

pub(super) fn welcome_option_hotkey(option: WelcomeOption) -> &'static str {
    match option {
        WelcomeOption::ResumeRecent => "R",
        WelcomeOption::NewDraft => "D",
        WelcomeOption::OpenFile => "O",
        WelcomeOption::Help => "H",
        WelcomeOption::Exit => "X",
    }
}

pub(super) fn welcome_option_copy(
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

pub(super) fn welcome_layout(area: Rect, show_side_panel: bool) -> WelcomeLayout {
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

pub(super) fn welcome_help_background(model: &AppState) -> Color {
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

pub(super) fn welcome_help_viewport(model: &AppState) -> Rect {
    welcome_layout(model.body_area(), model.welcome_side_panel_visible()).help_area
}

pub(super) fn welcome_help_page_step(model: &AppState) -> usize {
    welcome_help_viewport(model).height.max(1) as usize
}

pub(super) fn welcome_help_max_scroll(model: &AppState) -> usize {
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

pub(super) fn welcome_surface_bg(theme: ThemePalette) -> Color {
    blend_color(theme.panel_bg, theme.panel_alt_bg, 0.10)
}

pub(super) fn welcome_side_panel_bg(theme: ThemePalette) -> Color {
    blend_color(theme.panel_alt_bg, theme.panel_bg, 0.40)
}

pub(super) fn welcome_border_color(theme: ThemePalette) -> Color {
    blend_color(theme.border, theme.accent, 0.28)
}

pub(super) fn handle_welcome_event(
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

pub(super) fn emit_welcome_action(
    option: WelcomeOption,
    model: &mut AppState,
    cx: &mut EventCx<Action>,
) {
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

pub(super) fn welcome_section_title(title: &str, theme: ThemePalette, background: Color) -> Line {
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
pub(super) fn welcome_path_directory(path: &Path) -> Option<String> {
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

pub(super) fn render_welcome_footer(
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

pub(super) fn welcome_path_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| header_path_label(Some(path)))
}
