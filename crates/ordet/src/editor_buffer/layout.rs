use super::*;

struct GraphemeInfo<'a> {
    /// Borrowed slice of the source line: wrapping never copies per grapheme.
    text: &'a str,
    chars: usize,
    /// True display width, as `UnicodeWidthStr` reports it.
    width: usize,
    /// Width used for fitting: a zero-width grapheme still occupies a cell.
    fit: usize,
    whitespace: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CharClass {
    Whitespace,
    Word,
    Punctuation,
}

pub(super) fn classify_char(ch: char) -> CharClass {
    if ch.is_whitespace() {
        CharClass::Whitespace
    } else if ch.is_alphanumeric() || ch == '_' {
        CharClass::Word
    } else {
        CharClass::Punctuation
    }
}

/// Leading padding an alignment puts in front of a segment `width` display
/// columns wide. Shared by the wrap and by blank lines, which align as if
/// they were zero columns wide so the caret sits where text will start.
fn alignment_padding(alignment: ParagraphAlignment, width: usize, wrap_width: usize) -> usize {
    match alignment {
        ParagraphAlignment::Left | ParagraphAlignment::Justify => 0,
        ParagraphAlignment::Center => wrap_width.saturating_sub(width) / 2,
        ParagraphAlignment::Right => wrap_width.saturating_sub(width),
    }
}

/// Wrap one source line into the visual lines it occupies at `wrap_width`.
pub(super) fn wrap_source_line(
    text: &str,
    start: usize,
    alignment: ParagraphAlignment,
    wrap_width: usize,
) -> Vec<WrappedLine> {
    let mut lines = Vec::new();
    if text.is_empty() {
        // A blank paragraph still hangs its caret where its first typed
        // character would land. Pinning it to column 0 left a centered
        // blank line looking left-aligned until something was typed.
        let column = alignment_padding(alignment, 0, wrap_width);
        lines.push(WrappedLine {
            start_char: start,
            end_char: start,
            text: String::new(),
            display_offsets: vec![0],
            boundaries: vec![Boundary {
                char_index: start,
                column,
            }],
        });
        return lines;
    }

    let graphemes = text
        .graphemes(true)
        .map(|grapheme| {
            let width = UnicodeWidthStr::width(grapheme);
            GraphemeInfo {
                text: grapheme,
                chars: grapheme.chars().count(),
                width,
                fit: width.max(1),
                whitespace: grapheme.chars().all(char::is_whitespace),
            }
        })
        .collect::<Vec<_>>();

    let mut char_prefix = Vec::with_capacity(graphemes.len() + 1);
    char_prefix.push(0);
    for grapheme in &graphemes {
        let next = char_prefix.last().copied().unwrap_or_default() + grapheme.chars;
        char_prefix.push(next);
    }

    let mut segment_start = 0usize;
    while segment_start < graphemes.len() {
        let mut segment_end = segment_start;
        let mut width_used = 0usize;
        let mut last_whitespace_fit = None;

        while segment_end < graphemes.len() {
            let next_width = width_used + graphemes[segment_end].fit;
            if next_width > wrap_width && segment_end > segment_start {
                break;
            }

            width_used = next_width;
            segment_end += 1;
            if graphemes[segment_end - 1].whitespace {
                last_whitespace_fit = Some(segment_end);
            }

            if next_width >= wrap_width {
                break;
            }
        }

        let chosen_end = if segment_end < graphemes.len() {
            last_whitespace_fit
                .filter(|value| *value > segment_start)
                .unwrap_or(segment_end)
        } else {
            segment_end
        };

        let chosen_end = chosen_end.max(segment_start + 1).min(graphemes.len());
        let start_char = start + char_prefix[segment_start];
        let end_char = start + char_prefix[chosen_end];
        let is_last_segment = chosen_end >= graphemes.len();
        let visible_end = if is_last_segment {
            chosen_end
        } else {
            trimmed_visible_end(&graphemes, segment_start, chosen_end)
        };
        let gap_indices = if alignment == ParagraphAlignment::Justify && !is_last_segment {
            graphemes[segment_start..visible_end]
                .iter()
                .enumerate()
                .filter_map(|(offset, grapheme)| grapheme.whitespace.then_some(offset))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let base_width = graphemes[segment_start..visible_end]
            .iter()
            .map(|grapheme| grapheme.fit)
            .sum::<usize>();
        let effective_alignment =
            if alignment == ParagraphAlignment::Justify && gap_indices.is_empty() {
                ParagraphAlignment::Left
            } else {
                alignment
            };
        let leading_padding = alignment_padding(effective_alignment, base_width, wrap_width);
        let extra_width = if effective_alignment == ParagraphAlignment::Justify {
            wrap_width.saturating_sub(base_width)
        } else {
            0
        };
        let mut boundaries = vec![Boundary {
            char_index: start_char,
            column: leading_padding,
        }];
        let mut text = " ".repeat(leading_padding);
        let mut display_offsets = vec![text.chars().count()];
        let mut display_char_count = text.chars().count();
        let mut column = leading_padding;
        for index in segment_start..chosen_end {
            let grapheme = &graphemes[index];
            let visible = index < visible_end;
            let display_width = if !visible {
                0
            } else if effective_alignment == ParagraphAlignment::Justify && grapheme.whitespace {
                let gap_position = gap_indices
                    .iter()
                    .position(|gap| *gap == (index - segment_start))
                    .unwrap_or(0);
                let gap_count = gap_indices.len().max(1);
                let expansion =
                    extra_width / gap_count + usize::from(gap_position < extra_width % gap_count);
                let spaces = grapheme.fit + expansion;
                for _ in 0..spaces {
                    text.push(' ');
                }
                display_char_count += spaces;
                spaces
            } else {
                text.push_str(grapheme.text);
                display_char_count += grapheme.chars;
                grapheme.width
            };
            for _ in 0..grapheme.chars {
                display_offsets.push(display_char_count);
            }
            column += display_width;
            boundaries.push(Boundary {
                char_index: start + char_prefix[index + 1],
                column,
            });
        }

        lines.push(WrappedLine {
            start_char,
            end_char,
            text,
            display_offsets,
            boundaries,
        });

        segment_start = chosen_end;
    }

    lines
}

fn trimmed_visible_end(graphemes: &[GraphemeInfo<'_>], start: usize, end: usize) -> usize {
    let mut visible_end = end;
    while visible_end > start && graphemes[visible_end - 1].whitespace {
        visible_end -= 1;
    }
    if visible_end == start {
        end
    } else {
        visible_end
    }
}
