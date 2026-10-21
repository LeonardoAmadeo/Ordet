use super::*;

pub(super) fn status_wrap_width(model: &AppState) -> usize {
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
pub(super) fn status_fields(model: &AppState, theme: ThemePalette) -> Vec<(String, Color)> {
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
        if let Some(stats) = editor.word_stats_if_ready() {
            fields.push((
                format!("W:{} C:{}", stats.word_count, stats.chars),
                theme.bar_muted_fg(),
            ));
        }
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
pub(super) fn status_line(fields: &[(String, Color)], background: Color) -> Line {
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
pub(super) fn fit_status_message(
    message: &str,
    home_prefix: Option<&str>,
    max_width: usize,
) -> String {
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
pub(super) fn prompt_status_text(prompt: &crate::prompt::PromptState, width: usize) -> String {
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

pub(super) struct StatusBar;

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
