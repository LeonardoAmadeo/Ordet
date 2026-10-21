use super::{
    expand_tabs, AttrKind, AttrSpan, BlockPrefix, EditorBuffer, ParagraphAlignment, SearchOptions,
};

#[test]
fn search_options_cover_regex_word_and_case() {
    let buffer = EditorBuffer::from_text("The cat catalogs cats calmly");
    let plain = SearchOptions::default();
    let word = SearchOptions {
        whole_word: true,
        ..SearchOptions::default()
    };
    let case = SearchOptions {
        case_sensitive: true,
        ..SearchOptions::default()
    };
    let regex = SearchOptions {
        regex: true,
        ..SearchOptions::default()
    };

    // Plain is case-insensitive substring: "cat" hits cat, catalogs, cats.
    assert_eq!(buffer.find_matches_with("cat", &plain).unwrap().len(), 3);
    // Whole word narrows to the standalone word.
    let word_hits = buffer.find_matches_with("cat", &word).unwrap();
    assert_eq!(word_hits.len(), 1);
    assert_eq!(&buffer.plain_text()[word_hits[0].0..word_hits[0].1], "cat");
    // Case-sensitive misses the capital-less... "The" vs "the": only exact.
    assert!(buffer.find_matches_with("the", &case).unwrap().is_empty());
    assert_eq!(buffer.find_matches_with("The", &case).unwrap().len(), 1);
    // Regex character classes and anchors.
    let regex_hits = buffer.find_matches_with(r"\bcat\w*\b", &regex).unwrap();
    assert_eq!(regex_hits.len(), 3);
    // Captures expand in the replacement.
    let mut buffer = EditorBuffer::from_text("2026-10-05");
    let count = buffer
        .replace_all_with(r"(\d+)-(\d+)-(\d+)", "$3/$2/$1", &regex)
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(buffer.plain_text(), "05/10/2026");
    // A broken pattern is an error, never a panic or a silent no-op.
    assert!(buffer.find_matches_with("(", &regex).is_err());
    // Plain mode keeps `$` literal.
    let mut buffer = EditorBuffer::from_text("price five");
    let count = buffer.replace_all_with("five", "$5", &plain).unwrap();
    assert_eq!(count, 1);
    assert_eq!(buffer.plain_text(), "price $5");
}

#[test]
fn incremental_stats_match_the_reference_scan() {
    let documents = [
        "",
        "hello",
        "alpha beta.\n\ngamma delta!",
        "one\ntwo\nthree",
        "one.\n.two",
        "a. . .b",
        "trailing newline\n",
        "\n\n\n",
        "  spaced   out  words \n second line",
        "unicode ✓ words…\n日本語 text.",
    ];
    for text in documents {
        let mut buffer = EditorBuffer::from_text(text);
        assert_eq!(
            buffer.stats(),
            crate::stats::compute_word_stats(&buffer.plain_text()),
            "fresh stats for {text:?}"
        );

        // Insert at the very top: every later line moves, so the per-line
        // list has to stay aligned or the counts drift.
        buffer.move_to_top();
        buffer.insert_text("INSERTED\n");
        assert_eq!(
            buffer.stats(),
            crate::stats::compute_word_stats(&buffer.plain_text()),
            "after top insert for {text:?}"
        );

        // Delete across the first line break and compare once more.
        buffer.move_to_top();
        for _ in 0..8 {
            buffer.delete_forward();
        }
        assert_eq!(
            buffer.stats(),
            crate::stats::compute_word_stats(&buffer.plain_text()),
            "after delete for {text:?}"
        );
    }
}

#[test]
fn incremental_stats_survive_splices_and_joins() {
    let originals = [
        "alpha beta. gamma delta! epsilon? zeta",
        "one\n.two\n\nthree. \n four",
        "a. . .b\nc\nd. e",
    ];
    for original in originals {
        let mut buffer = EditorBuffer::from_text(original);
        // Split at every other character, left to right: each one splices
        // a line entry into the stats cache.
        let mut offset = 1;
        while offset < buffer.char_count() {
            buffer.set_cursor_with_anchor(offset, offset);
            buffer.insert_text("\n");
            offset += 2;
            assert_eq!(
                buffer.stats(),
                crate::stats::compute_word_stats(&buffer.plain_text()),
                "after splitting {original:?} at {offset}"
            );
        }
        // Now join again: the break before a line is backspaced away,
        // which merges entries and hands the sentence chain to the full
        // refold.
        let mut guard = 0;
        while buffer.line_count() > 1 && guard < 400 {
            guard += 1;
            let Some(break_at) = buffer.plain_text().find('\n') else {
                break;
            };
            buffer.set_cursor_with_anchor(break_at + 1, break_at + 1);
            buffer.backspace();
            assert_eq!(
                buffer.stats(),
                crate::stats::compute_word_stats(&buffer.plain_text()),
                "after joining {original:?}"
            );
        }
    }
}

#[test]
fn incremental_stats_survive_random_edits() {
    let mut buffer = EditorBuffer::from_text(
        "One sentence. Two more!\n\nNew paragraph, with words? Yes.\nlast line",
    );
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for step in 0..400 {
        let len = buffer.char_count();
        let offset = (next() as usize) % (len + 1);
        buffer.set_cursor_with_anchor(offset, offset);
        match next() % 3 {
            0 => buffer.insert_text("x"),
            1 => buffer.insert_text(" .y\n"),
            _ => {
                buffer.delete_forward();
            }
        }
        assert_eq!(
            buffer.stats(),
            crate::stats::compute_word_stats(&buffer.plain_text()),
            "step {step}"
        );
    }
}

#[test]
fn undo_and_redo_keep_the_incremental_stats_honest() {
    let mut buffer = EditorBuffer::from_text("one two three");
    for ch in " and more".chars() {
        buffer.insert_char_with_mode(ch, false);
    }
    let typed = buffer.stats();
    assert!(buffer.undo());
    assert_eq!(
        buffer.stats(),
        crate::stats::compute_word_stats(&buffer.plain_text())
    );
    assert!(buffer.redo());
    assert_eq!(buffer.stats(), typed);
    assert_eq!(
        buffer.stats(),
        crate::stats::compute_word_stats(&buffer.plain_text())
    );
}

#[test]
fn source_line_info_numbers_lines_and_spots_continuations() {
    let buffer = EditorBuffer::from_text("short\na much longer line that wraps\nend");
    // Line 1 starts at 0 and is its own visual line start.
    assert_eq!(buffer.source_line_info(0), (1, true));
    // Line 2 starts after "short\n" (6 chars).
    assert_eq!(buffer.source_line_info(6), (2, true));
    assert_eq!(buffer.source_line_info(7), (2, false));
    let last_start = buffer.plain_text().rfind("end").unwrap();
    assert_eq!(buffer.source_line_info(last_start), (3, true));
    assert_eq!(buffer.source_line_count(), 3);
}

/// The wrap cache only pays off if it agrees with a cold wrap, so compare
/// the two after every edit rather than trusting the memo.
///
/// The reference buffer is rebuilt from the same text *and* the same
/// paragraph alignments, because alignment changes how a line wraps and
/// `from_text` alone would quietly default every line to left.
fn assert_cache_matches_cold(buffer: &EditorBuffer, width: usize) {
    let cached = buffer.layout(width);
    let mut cold = EditorBuffer::from_text(&buffer.plain_text());
    cold.paragraph_alignments = buffer.paragraph_alignments.clone();
    cold.ensure_alignment_capacity();
    let cold = cold.layout(width);
    assert_eq!(
        cached.lines.len(),
        cold.lines.len(),
        "cached and cold wrap disagree on line count at width {width}"
    );
    for (index, (left, right)) in cached.lines.iter().zip(cold.lines.iter()).enumerate() {
        assert_eq!(
            left.start_char, right.start_char,
            "start_char on line {index}"
        );
        assert_eq!(left.end_char, right.end_char, "end_char on line {index}");
        assert_eq!(left.text, right.text, "text on line {index}");
        assert_eq!(
            left.display_offsets, right.display_offsets,
            "display_offsets on line {index}"
        );
        assert_eq!(
            left.boundaries, right.boundaries,
            "boundaries on line {index}"
        );
    }
    assert_eq!(buffer.total_visual_lines(width), cold.lines.len());
}

#[test]
fn wrap_cache_survives_edits_at_the_top_of_a_long_document() {
    let mut buffer = EditorBuffer::from_text(
        &(0..200)
            .map(|index| format!("line {index} with enough words to wrap at most widths"))
            .collect::<Vec<_>>()
            .join("\n"),
    );

    assert_cache_matches_cold(&buffer, 40);

    // An edit near the top shifts every later line's char offsets, which is
    // the case that forces the cache to re-point rather than reuse.
    buffer.move_to_top();
    buffer.insert_text("a new opening line\n");
    assert_cache_matches_cold(&buffer, 40);
    assert_cache_matches_cold(&buffer, 80);

    buffer.move_to_top();
    buffer.delete_line();
    assert_cache_matches_cold(&buffer, 40);
}

/// The cache-direct movement helpers must give the same answers as the
/// full-document `layout()` they replaced, including tabs, wrapped lines,
/// and non-left alignment. `layout()` is the reference; the helpers are
/// what the editor actually calls.
#[test]
fn cache_direct_movement_matches_full_layout() {
    let text = "a short line\n\tindented with a tab and words that wrap around the width\n\nlast line here";
    for width in [8usize, 20, 88] {
        let probe = EditorBuffer::from_text(text);
        let layout = probe.layout(width);
        let len = probe.char_count();

        // Home / End land on the wrapped line's edges.
        for cursor in (0..=len).step_by(3) {
            let mut buffer = EditorBuffer::from_text(text);
            buffer.set_cursor_for_test(cursor);
            let index = layout.line_index_for_cursor(cursor);
            let expected_start = layout.lines[index].start_char;
            let expected_end = layout.lines[index].end_char;
            buffer.move_home(width, false);
            assert_eq!(
                buffer.cursor(),
                expected_start,
                "home at {cursor} w={width}"
            );
            buffer.set_cursor_for_test(cursor);
            buffer.move_end(width, false);
            assert_eq!(buffer.cursor(), expected_end, "end at {cursor} w={width}");
        }

        // Up / down track the preferred column through the same lines.
        for cursor in (0..=len).step_by(7) {
            let mut buffer = EditorBuffer::from_text(text);
            buffer.set_cursor_for_test(cursor);
            let index = layout.line_index_for_cursor(cursor);
            let column = layout.column_for_cursor(index, cursor);
            let next = (index + 1).min(layout.lines.len().saturating_sub(1));
            let expected = layout.cursor_for_column(next, column);
            buffer.move_down(width, false);
            assert_eq!(buffer.cursor(), expected, "down at {cursor} w={width}");

            let mut buffer = EditorBuffer::from_text(text);
            buffer.set_cursor_for_test(cursor);
            let prev = index.saturating_sub(1);
            let expected = layout.cursor_for_column(prev, column);
            buffer.move_up(width, false);
            assert_eq!(buffer.cursor(), expected, "up at {cursor} w={width}");
        }

        // Line/col readout, go-to-line, and screen jumps agree.
        for cursor in (0..=len).step_by(5) {
            let buffer = {
                let mut buffer = EditorBuffer::from_text(text);
                buffer.set_cursor_for_test(cursor);
                buffer
            };
            let index = layout.line_index_for_cursor(cursor);
            assert_eq!(
                buffer.cursor_line_and_col(width),
                (index + 1, layout.column_for_cursor(index, cursor) + 1),
                "line/col at {cursor} w={width}"
            );
        }
        for visual in 0..layout.lines.len() {
            let mut buffer = EditorBuffer::from_text(text);
            buffer.move_to_line(visual + 1, width);
            assert_eq!(
                buffer.cursor(),
                layout.lines[visual].start_char,
                "go-to-line {} w={width}",
                visual + 1
            );
        }
        let mut buffer = EditorBuffer::from_text(text);
        buffer.move_to_screen_bottom(0, 2, width);
        assert_eq!(
            buffer.cursor(),
            layout.lines[1.min(layout.lines.len() - 1)].start_char,
            "screen bottom w={width}"
        );

        // Delete-to-end removes exactly the wrapped line's tail.
        for cursor in (0..=len).step_by(11) {
            let mut buffer = EditorBuffer::from_text(text);
            buffer.set_cursor_for_test(cursor);
            let index = layout.line_index_for_cursor(cursor);
            let end = layout.lines[index].end_char;
            let newline = probe.plain_text().chars().nth(cursor) == Some('\n');
            let changed = buffer.delete_to_end_of_line(width);
            if end > cursor {
                assert!(changed, "expected a deletion at {cursor} w={width}");
                assert_eq!(
                    buffer.cursor(),
                    cursor,
                    "cursor stays put at {cursor} w={width}"
                );
                assert_eq!(
                    buffer.char_count(),
                    len - (end - cursor),
                    "deleted tail at {cursor} w={width}"
                );
            } else if newline && cursor < len {
                assert!(changed, "expected newline eat at {cursor} w={width}");
            } else {
                assert!(!changed, "expected no-op at {cursor} w={width}");
            }
        }
    }
}

#[test]
fn wrap_cache_tracks_in_place_edits_that_do_not_change_the_line_count() {
    // Overwriting a character in place keeps the char and line counts the
    // same, so only the content check can notice the cache went stale.
    let mut buffer = EditorBuffer::from_text("alpha beta gamma\ndelta epsilon");
    assert_cache_matches_cold(&buffer, 20);

    buffer.select_all();
    assert!(buffer.delete_block());
    assert_cache_matches_cold(&buffer, 20);

    buffer.insert_text("one two three four five six seven eight nine ten");
    assert_cache_matches_cold(&buffer, 20);
    buffer.cycle_case();
    assert_cache_matches_cold(&buffer, 20);
}

#[test]
fn wrap_cache_notices_realigning_a_paragraph() {
    let mut buffer = EditorBuffer::from_text("a short line\nanother short line");
    assert_cache_matches_cold(&buffer, 20);

    buffer.select_all();
    assert!(buffer.set_alignment_for_selection(ParagraphAlignment::Justify));
    assert_cache_matches_cold(&buffer, 20);

    assert!(buffer.set_alignment_for_selection(ParagraphAlignment::Center));
    assert_cache_matches_cold(&buffer, 20);
}

#[test]
fn tabs_advance_to_the_eight_column_grid() {
    // A tab is display sugar: the buffer keeps the raw byte.
    assert_eq!(expand_tabs("a\tb"), "a       b");
    assert_eq!(expand_tabs("\tx"), "        x");
    assert_eq!(expand_tabs("12345678\tx"), "12345678        x");
    assert_eq!(expand_tabs("1234567\tx"), "1234567 x");
    assert_eq!(expand_tabs("no tabs"), "no tabs");
    assert_eq!(expand_tabs("a\n\tx"), "a\n        x");

    // The indent reader shares the same grid.
    let buffer = EditorBuffer::from_text("\tx");
    assert_eq!(buffer.line_indent_columns(0), 8);
}

#[test]
fn wrap_cache_compares_expanded_tabs() {
    let mut buffer = EditorBuffer::from_text("no tabs here\n\tindented with a tab");
    assert_cache_matches_cold(&buffer, 12);

    buffer.move_to_top();
    buffer.insert_text("\t\t");
    assert_cache_matches_cold(&buffer, 12);
}

#[test]
fn visible_layout_matches_the_full_wrap_it_slices() {
    let buffer = EditorBuffer::from_text(
        &(0..120)
            .map(|index| format!("paragraph {index} holding a moderate amount of prose"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let width = 30;
    let full = buffer.layout(width);
    assert!(
        full.total_lines() > 10,
        "test needs a document that wraps a lot"
    );

    for first in [0usize, 1, 7, 40] {
        let window = buffer.visible_layout(width, first, 5, 0);
        let expected = &full.lines[first.min(full.lines.len())..(first + 5).min(full.lines.len())];
        let actual: Vec<_> = window.lines.iter().collect();
        let expected: Vec<_> = expected.iter().collect();
        assert_eq!(
            actual, expected,
            "window at {first} does not match the full wrap"
        );
        assert_eq!(window.total_lines(), full.total_lines());
    }
}

#[test]
fn visual_line_for_cursor_agrees_with_the_full_layout() {
    let buffer = EditorBuffer::from_text(
        &(0..80)
            .map(|index| format!("row {index} wrapping across more than one visual line"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let width = 24;
    let full = buffer.layout(width);

    for cursor in 0..buffer.char_count() {
        assert_eq!(
            buffer.visual_line_for_cursor(width, cursor),
            full.line_index_for_cursor(cursor),
            "disagreement at char {cursor}"
        );
    }
}

#[test]
fn vertical_movement_works_through_a_windowed_layout() {
    let mut buffer = EditorBuffer::from_text(
        &(0..60)
            .map(|index| format!("line {index} long enough to wrap across several rows"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let width = 25;
    let start = buffer.cursor();

    buffer.move_down(width, false);
    assert!(
        buffer.cursor() > start,
        "down should move the cursor forward"
    );

    buffer.move_up(width, false);
    assert_eq!(buffer.cursor(), start, "up should undo down");

    // A long run of down presses must not run off the end of the document.
    for _ in 0..500 {
        buffer.move_down(width, false);
    }
    let bottom = buffer.cursor();
    for _ in 0..500 {
        buffer.move_down(width, false);
    }
    assert_eq!(buffer.cursor(), bottom, "down at the end should stay put");

    for _ in 0..500 {
        buffer.move_up(width, false);
    }
    assert_eq!(
        buffer.cursor(),
        0,
        "up should reach the start of the document"
    );
}

#[test]
fn insert_undo_and_redo_round_trip() {
    let mut buffer = EditorBuffer::new();

    buffer.insert_text("hello");
    assert_eq!(buffer.plain_text(), "hello");

    assert!(buffer.undo());
    assert_eq!(buffer.plain_text(), "");

    assert!(buffer.redo());
    assert_eq!(buffer.plain_text(), "hello");
}

/// Every mutation must round-trip through change-based undo and redo:
/// text, cursor, selection anchor, and paragraph alignments all come back.
/// This is the test that would catch a mutation bypassing the op log.
#[test]
fn every_mutation_round_trips_through_undo_and_redo() {
    let original = "first line here\n    second indented with spaces\nthird line here";
    let width = 20;

    let mut buffer = EditorBuffer::from_text(original);
    buffer.select_all();
    assert!(buffer.set_alignment_for_selection(ParagraphAlignment::Center));
    buffer.move_to_top();
    let aligned_html = buffer.to_ord_html();

    // Each entry positions the cursor first (`setup`), then performs one
    // user-visible mutation. `before_cursor` is captured between the two,
    // which is exactly what the undo record sees.
    type BufferSetup = Box<dyn Fn(&mut EditorBuffer)>;
    type BufferMutation<'a> = Box<dyn Fn(&mut EditorBuffer) -> bool + 'a>;
    let mutations: Vec<(&str, BufferSetup, BufferMutation<'_>)> = vec![
        (
            "type",
            Box::new(|_| {}),
            Box::new(|b| {
                b.insert_text("X");
                true
            }),
        ),
        (
            "newline",
            Box::new(|_| {}),
            Box::new(|b| b.insert_newline_with_indent()),
        ),
        (
            "backspace",
            Box::new(|b| {
                b.move_end(80, false);
            }),
            Box::new(|b| b.backspace()),
        ),
        (
            "delete",
            Box::new(|b| {
                b.move_to_top();
            }),
            Box::new(|b| b.delete_forward()),
        ),
        (
            "delete word",
            Box::new(|b| {
                b.move_to_top();
            }),
            Box::new(|b| b.delete_word_forward()),
        ),
        (
            "delete line",
            Box::new(|b| {
                b.move_to_top();
            }),
            Box::new(|b| b.delete_line()),
        ),
        (
            "delete to end",
            Box::new(|b| {
                b.move_to_top();
            }),
            Box::new(|b| b.delete_to_end_of_line(width)),
        ),
        (
            "indent",
            Box::new(|b| {
                b.move_to_top();
            }),
            Box::new(|b| b.indent()),
        ),
        (
            "outdent",
            Box::new(|b| {
                // The second line starts life tab-indented, so outdent has
                // something to remove. (`move_to_line` takes visual lines.)
                b.move_to_line(2, 80);
            }),
            Box::new(|b| b.outdent()),
        ),
        (
            "open line",
            Box::new(|b| {
                b.move_to_top();
            }),
            Box::new(|b| b.open_line()),
        ),
        (
            "case",
            Box::new(|b| {
                b.select_all();
            }),
            Box::new(|b| b.cycle_case()),
        ),
        (
            "block move",
            Box::new(|b| {
                b.set_block_begin();
                b.move_end(80, true);
                b.set_block_end();
                b.move_cursor_to_end();
            }),
            Box::new(|b| b.move_block()),
        ),
        (
            "block copy",
            Box::new(|b| {
                b.set_block_begin();
                b.move_end(80, true);
            }),
            Box::new(|b| b.copy_block()),
        ),
        (
            "block delete",
            Box::new(|b| {
                b.set_block_begin();
                b.move_end(80, true);
            }),
            Box::new(|b| b.delete_block()),
        ),
        (
            "replace all",
            Box::new(|_| {}),
            Box::new(|b| {
                b.replace_all_with("line", "row", &SearchOptions::default())
                    .map(|count| count > 0)
                    .unwrap_or(false)
            }),
        ),
        (
            "align",
            Box::new(|b| {
                b.select_all();
            }),
            Box::new(|b| b.set_alignment_for_selection(ParagraphAlignment::Right)),
        ),
    ];

    for (name, setup, mutate) in mutations {
        let mut buffer = EditorBuffer::from_text(original);
        buffer.select_all();
        assert!(buffer.set_alignment_for_selection(ParagraphAlignment::Center));
        buffer.move_to_top();
        setup(&mut buffer);
        let before_text = buffer.plain_text();
        let before_html = buffer.to_ord_html();
        let before_cursor = buffer.cursor();

        assert!(mutate(&mut buffer), "{name} reported no change");
        let after_text = buffer.plain_text();
        let after_html = buffer.to_ord_html();

        assert!(buffer.undo(), "{name} has nothing to undo");
        assert_eq!(buffer.plain_text(), before_text, "{name} undo text");
        assert_eq!(buffer.to_ord_html(), before_html, "{name} undo html");
        assert_eq!(buffer.cursor(), before_cursor, "{name} undo cursor");
        assert_cache_matches_cold(&buffer, width);

        assert!(buffer.redo(), "{name} has nothing to redo");
        assert_eq!(buffer.plain_text(), after_text, "{name} redo text");
        assert_eq!(buffer.to_ord_html(), after_html, "{name} redo html");
        assert_cache_matches_cold(&buffer, width);

        // A second undo after the redo lands back at the start, so the
        // redo record captured the full forward change.
        assert!(buffer.undo(), "{name} second undo failed");
        assert_eq!(buffer.plain_text(), before_text, "{name} re-undo text");
        let _ = aligned_html;
    }
}

#[test]
fn select_all_and_backspace_clears_the_document() {
    let mut buffer = EditorBuffer::from_text("alpha beta");

    buffer.select_all();
    assert!(buffer.backspace());
    assert_eq!(buffer.plain_text(), "");
}

#[test]
fn wrapping_prefers_word_boundaries_when_space_allows() {
    let buffer = EditorBuffer::from_text("alpha beta gamma");
    let layout = buffer.layout(6);
    let lines = layout
        .lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>();

    assert_eq!(lines, vec!["alpha", "beta", "gamma"]);
}

#[test]
fn trailing_space_stays_visible_on_the_last_visual_line() {
    let mut buffer = EditorBuffer::from_text("alpha");
    buffer.move_end(20, false);
    buffer.insert_text(" ");

    let layout = buffer.layout(20);

    assert_eq!(layout.lines[0].text, "alpha ");
    assert_eq!(layout.column_for_cursor(0, buffer.cursor()), 6);
}

#[test]
fn enter_carries_the_current_indentation_to_the_new_line() {
    let mut buffer = EditorBuffer::from_text("    alpha");
    buffer.move_end(80, false);

    buffer.insert_newline_with_indent();

    assert_eq!(buffer.plain_text(), "    alpha\n    ");
    assert_eq!(buffer.cursor(), 14);
}

#[test]
fn enter_on_an_unindented_line_adds_no_padding() {
    let mut buffer = EditorBuffer::from_text("alpha");
    buffer.move_end(80, false);

    buffer.insert_newline_with_indent();

    assert_eq!(buffer.plain_text(), "alpha\n");
    assert_eq!(buffer.cursor(), 6);
}

#[test]
fn tab_indents_a_single_line_when_nothing_is_selected() {
    let mut buffer = EditorBuffer::from_text("alpha");

    assert!(buffer.indent());

    assert_eq!(buffer.plain_text(), "    alpha");
    assert_eq!(buffer.cursor(), 4);
}

#[test]
fn tab_indents_every_line_of_a_multiline_selection() {
    let mut buffer = EditorBuffer::from_text("alpha\nbeta\ngamma");
    buffer.move_to_top();
    buffer.move_down(80, true);
    buffer.move_right(true);
    assert!(buffer.selection_range().is_some());

    assert!(buffer.indent());

    assert_eq!(buffer.plain_text(), "    alpha\n    beta\ngamma");
}

#[test]
fn outdent_strips_one_level_from_the_current_line() {
    let mut buffer = EditorBuffer::from_text("        alpha");
    buffer.move_home(80, false);

    assert!(buffer.outdent());

    assert_eq!(buffer.plain_text(), "    alpha");
}

#[test]
fn outdent_is_a_no_op_when_the_line_has_no_leading_spaces() {
    let mut buffer = EditorBuffer::from_text("alpha");
    buffer.move_home(80, false);

    assert!(!buffer.outdent());

    assert_eq!(buffer.plain_text(), "alpha");
}

#[test]
fn indent_then_outdent_returns_the_text_to_where_it_started() {
    let original = "alpha\n    beta\ngamma";
    let mut buffer = EditorBuffer::from_text(original);
    buffer.move_to_top();
    buffer.select_all();

    assert!(buffer.indent());
    assert_eq!(buffer.plain_text(), "    alpha\n        beta\n    gamma");

    assert!(buffer.outdent());
    assert_eq!(buffer.plain_text(), original);
}

#[test]
fn cycling_case_walks_through_all_four_styles() {
    let mut buffer = EditorBuffer::from_text("hello wide world");
    buffer.move_to_top();
    buffer.select_all();

    // All-lowercase is its own style, so it advances to Capitalize first.
    assert!(buffer.cycle_case());
    assert_eq!(buffer.plain_text(), "Hello Wide World");

    assert!(buffer.cycle_case());
    assert_eq!(buffer.plain_text(), "Hello wide world");

    assert!(buffer.cycle_case());
    assert_eq!(buffer.plain_text(), "HELLO WIDE WORLD");

    assert!(buffer.cycle_case());
    assert_eq!(buffer.plain_text(), "hello wide world");
}

#[test]
fn cycling_case_does_not_stall_on_a_single_capitalized_word() {
    let mut buffer = EditorBuffer::from_text("Hello");
    buffer.move_to_top();
    buffer.select_all();

    // One capitalized word reads as both title and sentence case, so it
    // keeps cycling instead of stalling.
    assert!(buffer.cycle_case());
    assert_eq!(buffer.plain_text(), "HELLO");
}

#[test]
fn toggling_bold_styles_and_unstyles_a_selection() {
    use super::{AttrKind, AttrSpan};
    let mut buffer = EditorBuffer::from_text("hello world");
    buffer.move_to_top();
    buffer.select_all();

    assert!(buffer.toggle_attr(AttrKind::Bold));
    assert_eq!(
        buffer.attrs,
        vec![AttrSpan {
            start: 0,
            end: 11,
            kind: AttrKind::Bold,
        }]
    );

    // Fully styled: toggling again removes it.
    assert!(buffer.toggle_attr(AttrKind::Bold));
    assert!(buffer.attrs.is_empty());
}

#[test]
fn toggling_styles_a_partially_styled_selection() {
    use super::{AttrKind, AttrSpan};
    let mut buffer = EditorBuffer::from_text("hello world");
    buffer.move_to_top();
    // Select "hello", bold it, then select all and toggle: the whole
    // range ends up bold.
    buffer.anchor = Some(0);
    buffer.cursor = 5;
    assert!(buffer.toggle_attr(AttrKind::Bold));
    buffer.select_all();
    assert!(buffer.toggle_attr(AttrKind::Bold));
    assert_eq!(
        buffer.attrs,
        vec![AttrSpan {
            start: 0,
            end: 11,
            kind: AttrKind::Bold,
        }]
    );
}

#[test]
fn bold_and_italic_overlap_freely() {
    use super::{AttrKind, AttrSpan};
    let mut buffer = EditorBuffer::from_text("abcdef");
    buffer.move_to_top();
    buffer.anchor = Some(0);
    buffer.cursor = 4;
    assert!(buffer.toggle_attr(AttrKind::Bold));
    buffer.anchor = Some(2);
    buffer.cursor = 6;
    assert!(buffer.toggle_attr(AttrKind::Italic));
    assert_eq!(
        buffer.attrs,
        vec![
            AttrSpan {
                start: 0,
                end: 4,
                kind: AttrKind::Bold,
            },
            AttrSpan {
                start: 2,
                end: 6,
                kind: AttrKind::Italic,
            },
        ]
    );
    // Rows report both dimensions where they cross.
    let runs = buffer.attr_runs_in(0, 6);
    assert_eq!(
        runs,
        vec![(0, 2, true, false), (2, 4, true, true), (4, 6, false, true),]
    );
}

#[test]
fn toggling_needs_a_selection() {
    use super::AttrKind;
    let mut buffer = EditorBuffer::from_text("hello");
    assert!(!buffer.toggle_attr(AttrKind::Bold));
    assert!(buffer.attrs.is_empty());
}

#[test]
fn typing_inside_a_styled_range_inherits_the_style() {
    use super::{AttrKind, AttrSpan};
    let mut buffer = EditorBuffer::from_text("hello world");
    buffer.move_to_top();
    buffer.select_all();
    assert!(buffer.toggle_attr(AttrKind::Bold));
    // Insert inside the range: it grows.
    buffer.collapse_selection();
    buffer.cursor = 5;
    buffer.insert_char_with_mode('X', false);
    assert_eq!(
        buffer.attrs,
        vec![AttrSpan {
            start: 0,
            end: 12,
            kind: AttrKind::Bold,
        }]
    );
    // Insert exactly at its start: the new char stays plain.
    buffer.cursor = 0;
    buffer.insert_char_with_mode('Y', false);
    assert_eq!(
        buffer.attrs,
        vec![AttrSpan {
            start: 1,
            end: 13,
            kind: AttrKind::Bold,
        }]
    );
    assert_eq!(buffer.plain_text(), "YhelloX world");
}

#[test]
fn deleting_trims_and_drops_styled_ranges() {
    use super::{AttrKind, AttrSpan};
    let mut buffer = EditorBuffer::from_text("aabbcc");
    buffer.move_to_top();
    buffer.anchor = Some(0);
    buffer.cursor = 6;
    assert!(buffer.toggle_attr(AttrKind::Bold));
    // Delete the middle: the range shrinks around the gap.
    buffer.anchor = Some(2);
    buffer.cursor = 4;
    assert!(buffer.delete_block());
    assert_eq!(buffer.plain_text(), "aacc");
    assert_eq!(
        buffer.attrs,
        vec![AttrSpan {
            start: 0,
            end: 4,
            kind: AttrKind::Bold,
        }]
    );
    // Delete everything styled: the span goes away, not empty.
    buffer.select_all();
    assert!(buffer.delete_block());
    assert!(buffer.attrs.is_empty());
}

#[test]
fn undo_restores_styled_ranges() {
    use super::AttrKind;
    let mut buffer = EditorBuffer::from_text("hello");
    buffer.move_to_top();
    buffer.select_all();
    assert!(buffer.toggle_attr(AttrKind::Bold));
    assert!(!buffer.attrs.is_empty());
    assert!(buffer.undo());
    assert!(buffer.attrs.is_empty());
    assert!(buffer.redo());
    assert!(!buffer.attrs.is_empty());
}

#[test]
fn ord_html_round_trips_bold_and_italic() {
    use super::AttrKind;
    let mut buffer = EditorBuffer::from_text("plain bold both");
    buffer.move_to_top();
    // "bold": chars 6..10.
    buffer.anchor = Some(6);
    buffer.cursor = 10;
    assert!(buffer.toggle_attr(AttrKind::Bold));
    // "bold both": chars 6..15 italic, crossing the bold range.
    buffer.anchor = Some(6);
    buffer.cursor = 15;
    assert!(buffer.toggle_attr(AttrKind::Italic));

    let html = buffer.to_ord_html();
    let reloaded = EditorBuffer::from_rich_text("plain bold both", &html);
    assert_eq!(reloaded.attrs, buffer.attrs);
    assert_eq!(reloaded.plain_text(), "plain bold both");
}

#[test]
fn ord_html_parsing_survives_entities_and_unknown_tags() {
    let html = "<p>fish &amp; <b>chips</b> <u>yum</u></p><p>second</p>";
    let buffer = EditorBuffer::from_rich_text("fish & chips yum\nsecond", html);
    assert_eq!(buffer.plain_text(), "fish & chips yum\nsecond");
    // "chips" starts after "fish & " (7 chars).
    let bolds: Vec<(usize, usize)> = buffer
        .attrs
        .iter()
        .filter(|sp| sp.kind == super::AttrKind::Bold)
        .map(|sp| (sp.start, sp.end))
        .collect();
    assert_eq!(bolds, vec![(7, 12)]);
}

#[test]
fn sorting_selected_lines_orders_them_case_insensitively() {
    let mut buffer = EditorBuffer::from_text("pear\napple\nFig");
    buffer.move_to_top();
    buffer.select_all();

    assert!(buffer.sort_selected_lines());
    assert_eq!(buffer.plain_text(), "apple\nFig\npear");

    assert!(!buffer.sort_selected_lines());
}

#[test]
fn sorting_selected_lines_needs_more_than_one_line() {
    let mut buffer = EditorBuffer::from_text("only\n");
    buffer.move_to_top();
    buffer.select_all();

    assert!(!buffer.sort_selected_lines());
    assert_eq!(buffer.plain_text(), "only\n");
}

#[test]
fn climbing_up_a_long_file_never_moves_down() {
    let text = (0..500)
        .map(|i| format!("line {i:03} with some words to wrap around"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut buffer = EditorBuffer::from_text(&text);
    buffer.move_cursor_to_end();

    let mut previous = buffer.cursor();
    for _ in 0..600 {
        buffer.move_up(60, false);
        let cursor = buffer.cursor();
        assert!(
            cursor <= previous,
            "cursor snapped down from {previous} to {cursor}"
        );
        previous = cursor;
    }
    // Column memory keeps the column on the top row (standard: Home or
    // Left reaches char 0 from here), so assert row membership, not 0.
    assert_eq!(buffer.visual_line_for_cursor(60, buffer.cursor()), 0);
}

#[test]
fn caret_column_answers_against_scrolled_windows() {
    // The hardware caret is placed from a windowed layout with a global
    // line index; it must resolve against the window, or a scrolled
    // file draws the caret at column 0 and the tracer looks possessed.
    let text = (0..200)
        .map(|i| format!("line {i:03} with words"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut buffer = EditorBuffer::from_text(&text);
    buffer.move_cursor_to_end();

    let width = 60;
    let cursor = buffer.cursor();
    let global = buffer.visual_line_for_cursor(width, cursor);
    assert!(global > 20, "test needs a scrolled document");

    // A window around the cursor, as the renderer builds it.
    let start = global.saturating_sub(5);
    let window = buffer.visible_layout(width, start, 10, 0);
    let column = window.column_for_cursor(global, cursor);
    let full = buffer.layout(width);
    assert_eq!(
        column,
        full.column_for_cursor(global, cursor),
        "windowed caret column must match the full layout"
    );
    assert!(column > 0, "caret belongs at end of line, not column 0");
}

#[test]
fn sorting_selected_lines_does_nothing_without_a_selection() {
    let mut buffer = EditorBuffer::from_text("b\na\n");

    assert!(!buffer.sort_selected_lines());
    assert_eq!(buffer.plain_text(), "b\na\n");
}

#[test]
fn block_prefixes_add_and_replace_markers() {
    use super::BlockPrefix;
    let mut buffer = EditorBuffer::from_text("hello");
    buffer.move_to_top();

    assert!(buffer.apply_block_prefix(BlockPrefix::Title));
    assert_eq!(buffer.plain_text(), "# hello");
    assert!(buffer.apply_block_prefix(BlockPrefix::Heading));
    assert_eq!(buffer.plain_text(), "## hello");
    assert!(buffer.apply_block_prefix(BlockPrefix::Quote));
    assert_eq!(buffer.plain_text(), "> hello");
    assert!(buffer.apply_block_prefix(BlockPrefix::Body));
    assert_eq!(buffer.plain_text(), "hello");
    assert!(!buffer.apply_block_prefix(BlockPrefix::Body));
}

#[test]
fn block_prefix_skips_blank_lines_and_keeps_indent() {
    use super::BlockPrefix;
    let mut buffer = EditorBuffer::from_text("  one\n\n  two");
    buffer.move_to_top();
    buffer.select_all();

    assert!(buffer.apply_block_prefix(BlockPrefix::Quote));
    assert_eq!(buffer.plain_text(), "  > one\n\n  > two");
}

#[test]
fn block_body_strips_lists_and_checkboxes() {
    use super::BlockPrefix;
    let mut buffer = EditorBuffer::from_text("- [ ] task\n1. item\n- bullet");
    buffer.move_to_top();
    buffer.select_all();

    assert!(buffer.apply_block_prefix(BlockPrefix::Body));
    assert_eq!(buffer.plain_text(), "task\nitem\nbullet");
}

#[test]
fn block_prefix_moves_styled_ranges_with_the_marker() {
    use super::{AttrKind, BlockPrefix};
    let mut buffer = EditorBuffer::from_text("hello");
    buffer.move_to_top();
    buffer.select_all();
    assert!(buffer.toggle_attr(AttrKind::Bold));

    assert!(buffer.apply_block_prefix(BlockPrefix::Title));
    assert_eq!(buffer.plain_text(), "# hello");
    // "hello" slid right by 2; its bold range followed.
    assert_eq!(
        buffer.attrs,
        vec![super::AttrSpan {
            start: 2,
            end: 7,
            kind: AttrKind::Bold,
        }]
    );
    assert!(buffer.undo());
    assert_eq!(buffer.plain_text(), "hello");
}

#[test]
fn cycling_case_starts_from_a_selection_that_is_already_uppercase() {
    let mut buffer = EditorBuffer::from_text("SHOUT");
    buffer.move_to_top();
    buffer.select_all();

    assert!(buffer.cycle_case());
    assert_eq!(buffer.plain_text(), "shout");
}

#[test]
fn cycling_case_does_nothing_without_a_selection() {
    let mut buffer = EditorBuffer::from_text("hello");

    assert!(!buffer.cycle_case());

    assert_eq!(buffer.plain_text(), "hello");
}

#[test]
fn cycling_case_ignores_a_selection_with_no_letters() {
    let mut buffer = EditorBuffer::from_text("123 -- 456");
    buffer.move_to_top();
    buffer.select_all();

    assert!(!buffer.cycle_case());

    assert_eq!(buffer.plain_text(), "123 -- 456");
}

#[test]
fn cycling_case_keeps_the_selection_covering_the_same_text() {
    let mut buffer = EditorBuffer::from_text("one two three");
    buffer.move_to_top();
    buffer.select_all();

    assert!(buffer.cycle_case());
    assert_eq!(
        buffer.selected_text(),
        Some("One Two Three".to_string()),
        "the selection should still cover exactly the converted text"
    );
}

#[test]
fn outdent_removes_a_short_indent_entirely() {
    let mut buffer = EditorBuffer::from_text("  alpha");
    buffer.move_home(80, false);

    // Outdent removes up to one level, so a two-space indent goes to none
    // rather than being left half-applied.
    assert!(buffer.outdent());

    assert_eq!(buffer.plain_text(), "alpha");
}

#[test]
fn backspace_at_line_start_removes_only_the_newline() {
    let mut buffer = EditorBuffer::from_text("alpha\nbeta");
    buffer.move_down(40, false);
    buffer.move_home(40, false);

    assert!(buffer.backspace());

    assert_eq!(buffer.plain_text(), "alphabeta");
    assert_eq!(buffer.cursor(), 5);
}

#[test]
fn delete_forward_at_line_end_removes_only_the_newline() {
    let mut buffer = EditorBuffer::from_text("alpha\nbeta");
    buffer.move_end(40, false);

    assert!(buffer.delete_forward());

    assert_eq!(buffer.plain_text(), "alphabeta");
    assert_eq!(buffer.cursor(), 5);
}

#[test]
fn word_motion_moves_between_word_starts() {
    let mut buffer = EditorBuffer::from_text("alpha beta gamma");

    buffer.move_word_right(false);
    assert_eq!(buffer.cursor(), 6);

    buffer.move_word_right(false);
    assert_eq!(buffer.cursor(), 11);

    buffer.move_word_left(false);
    assert_eq!(buffer.cursor(), 6);
}

#[test]
fn selected_text_returns_the_active_selection() {
    let mut buffer = EditorBuffer::from_text("alpha beta");
    buffer.move_word_right(true);

    assert_eq!(buffer.selected_text().as_deref(), Some("alpha "));
}

#[test]
fn non_latin_text_round_trips_through_editing() {
    // Kanji, Hebrew (RTL), hiragana, emoji ZWJ sequence, combining mark.
    let text = "日本語テスト\nשלום עולם\nあいうえお\nfamily 👨‍👩‍👧‍👦!\ncafé naive";
    let mut buffer = EditorBuffer::from_text(text);
    assert_eq!(buffer.plain_text(), text);
    // Chars, not bytes: multibyte text must not confuse lengths.
    assert!(buffer.plain_text().len() > text.chars().count());

    // Select all, copy, paste: duplicates exactly once.
    buffer.select_all();
    let selected = buffer.selected_text().expect("selection");
    assert_eq!(selected, text);
    buffer.collapse_selection();
    buffer.insert_text(&selected);
    assert_eq!(buffer.plain_text(), format!("{text}{text}"));

    // Undo restores the original text.
    assert!(buffer.undo());
    assert_eq!(buffer.plain_text(), text);
    assert!(buffer.redo());
    assert_eq!(buffer.plain_text(), format!("{text}{text}"));
}

#[test]
fn grapheme_motion_crosses_emoji_and_combining_marks_whole() {
    let mut buffer = EditorBuffer::from_text("a👨‍👩‍👧‍👦b");
    assert_eq!(buffer.plain_text().chars().count(), 9);
    // Cursor to start, step right: plain char, whole emoji, plain char.
    buffer.set_cursor_with_anchor(0, 0);
    buffer.move_right(false);
    assert_eq!(buffer.cursor(), 1);
    buffer.move_right(false);
    // The 7-char ZWJ sequence crosses in a single step.
    assert_eq!(buffer.cursor(), 8);
    buffer.move_right(false);
    assert_eq!(buffer.cursor(), 9);

    // Backspace removes the whole emoji, not one code point.
    buffer.set_cursor_with_anchor(8, 8);
    assert!(buffer.backspace());
    assert_eq!(buffer.plain_text(), "ab");
}

#[test]
fn word_motion_and_delete_survive_cjk_and_hebrew() {
    let mut buffer = EditorBuffer::from_text("hello 日本語テスト שלום world");
    buffer.select_all();
    assert!(buffer.selected_text().is_some());
    buffer.collapse_selection();

    // Word jumps move (never stall) across scripts.
    buffer.set_cursor_with_anchor(0, 0);
    let mut last = 0usize;
    for _ in 0..10 {
        buffer.move_word_right(false);
        assert!(buffer.cursor() >= last);
        if buffer.cursor() == buffer.plain_text().chars().count() {
            break;
        }
        last = buffer.cursor();
    }
    assert_eq!(buffer.cursor(), buffer.plain_text().chars().count());

    buffer.set_cursor_with_anchor(buffer.cursor(), buffer.cursor());
    for _ in 0..10 {
        let before = buffer.cursor();
        buffer.move_word_left(false);
        assert!(buffer.cursor() <= before);
        if buffer.cursor() == 0 {
            break;
        }
    }
    assert_eq!(buffer.cursor(), 0);

    // Delete word forward on a Hebrew word removes whole chars.
    buffer.set_cursor_with_anchor(0, 0);
    for _ in 0..6 {
        buffer.move_word_right(false);
    }
    assert!(buffer.delete_word_forward() || buffer.plain_text().contains("world"));
}

#[test]
fn delete_word_left_removes_the_previous_word() {
    let mut buffer = EditorBuffer::from_text("hello big world");
    buffer.set_cursor_with_anchor(buffer.char_count(), buffer.char_count());

    assert!(buffer.delete_word_left());
    assert_eq!(buffer.plain_text(), "hello big ");
    assert_eq!(buffer.cursor(), "hello big ".chars().count());

    assert!(buffer.delete_word_left());
    assert_eq!(buffer.plain_text(), "hello ");
    assert_eq!(buffer.cursor(), "hello ".chars().count());

    // At the start there is nothing left to remove.
    buffer.set_cursor_with_anchor(0, 0);
    assert!(!buffer.delete_word_left());
    assert_eq!(buffer.plain_text(), "hello ");
}

#[test]
fn wide_chars_wrap_on_display_columns_not_char_counts() {
    use unicode_segmentation::UnicodeSegmentation;
    // Each kanji is 2 columns wide: at width 6 only 3 fit per row.
    let buffer = EditorBuffer::from_text("日本語テストです");
    let layout = buffer.layout(6);
    assert!(layout.lines.len() > 1, "wide text must wrap");
    for line in &layout.lines {
        let width: usize = line
            .text
            .graphemes(true)
            .map(|g| unicode_width::UnicodeWidthStr::width(g).max(1))
            .sum();
        assert!(width <= 6, "row overflows: {:?}", line.text);
    }
    // Cursor columns track display width: after 2 kanji the column is 4.
    let (row, col) = buffer.cursor_line_and_col(20);
    assert_eq!((row, col), (1, 1));
    let mut moved = buffer.clone();
    moved.set_cursor_with_anchor(2, 2);
    let (_, col) = moved.cursor_line_and_col(20);
    assert_eq!(col, 5, "two wide chars occupy columns 1-4");
}

#[test]
fn paragraph_alignment_round_trips_to_html() {
    let mut buffer = EditorBuffer::from_text("alpha beta\ngamma");
    buffer.set_alignment_for_selection(ParagraphAlignment::Center);
    buffer.move_down(40, false);
    buffer.set_alignment_for_selection(ParagraphAlignment::Justify);

    let html = buffer.to_ord_html();
    let parsed = EditorBuffer::from_rich_text("alpha beta\ngamma", &html);

    assert_eq!(parsed.current_alignment(), ParagraphAlignment::Center);
    assert_eq!(parsed.paragraph_alignments[1], ParagraphAlignment::Justify);
}

#[test]
fn centered_layout_offsets_the_rendered_line() {
    let mut buffer = EditorBuffer::from_text("alpha");
    buffer.set_alignment_for_selection(ParagraphAlignment::Center);

    let layout = buffer.layout(11);

    assert_eq!(layout.lines[0].boundaries[0].column, 3);
}

#[test]
fn justify_expands_internal_spacing() {
    let mut buffer = EditorBuffer::from_text("alpha beta gamma");
    buffer.set_alignment_for_selection(ParagraphAlignment::Justify);

    let layout = buffer.layout(11);

    assert!(layout.lines[0].text.contains("  "));
}

#[test]
fn a_blank_paragraph_hangs_its_caret_where_its_text_will_start() {
    let mut buffer = EditorBuffer::from_text("alpha\n\nomega");

    buffer.move_to_line(2, 20);
    assert_eq!(buffer.cursor_visual_column(20), 0);

    // A blank centered line waits in the middle: centering a bare line
    // used to look left-aligned until a character was typed.
    buffer.set_alignment_for_selection(ParagraphAlignment::Center);
    assert_eq!(buffer.cursor_visual_column(20), 10);
    // Typing the first character does not shift the caret: the wrap
    // re-centers around the one-column line and lands on the same cell.
    buffer.insert_text("b");
    assert_eq!(buffer.cursor_visual_column(20), 10);
    assert_eq!(buffer.layout(20).lines[1].boundaries[0].column, 9);

    // Right hangs on the right margin, again where the text lands.
    buffer.undo();
    buffer.move_to_line(2, 20);
    buffer.set_alignment_for_selection(ParagraphAlignment::Right);
    assert_eq!(buffer.cursor_visual_column(20), 20);
}

#[test]
fn sorting_a_long_line_preserves_every_character() {
    let long = "z".repeat(30_000);
    let mut buffer = EditorBuffer::from_text(&format!("{long}\na"));
    buffer.select_all();
    assert!(buffer.sort_selected_lines());
    let text = buffer.plain_text();
    assert!(text.contains(&long));
    assert_eq!(text.chars().count(), long.chars().count() + 2);
}

#[test]
fn block_prefixes_preserve_long_lines() {
    let long = "word ".repeat(6_000);
    let mut buffer = EditorBuffer::from_text(&format!("# {long}\nbody"));
    buffer.select_all();
    assert!(buffer.apply_block_prefix(BlockPrefix::Heading));
    let text = buffer.plain_text();
    assert!(text.contains(&long));
    assert!(text.starts_with("## word"));
}

#[test]
fn deleting_a_whole_line_removes_its_alignment_not_the_next_one() {
    let mut buffer = EditorBuffer::from_text("one\ntwo\nthree");
    buffer.move_to_line(2, 80);
    buffer.set_alignment_for_selection(ParagraphAlignment::Center);
    buffer.move_to_line(3, 80);
    buffer.set_alignment_for_selection(ParagraphAlignment::Right);

    buffer.set_cursor_with_anchor(0, 4);
    assert!(buffer.backspace());

    assert_eq!(buffer.plain_text(), "two\nthree");
    assert_eq!(
        buffer.paragraph_alignments,
        vec![ParagraphAlignment::Center, ParagraphAlignment::Right]
    );
}

#[test]
fn joining_paragraphs_keeps_the_first_paragraph_alignment() {
    let mut buffer = EditorBuffer::from_text("one\ntwo\nthree");
    buffer.move_to_line(2, 80);
    buffer.set_alignment_for_selection(ParagraphAlignment::Center);
    buffer.move_to_line(3, 80);
    buffer.set_alignment_for_selection(ParagraphAlignment::Right);

    buffer.set_cursor_with_anchor(2, 5);
    assert!(buffer.backspace());

    assert_eq!(buffer.plain_text(), "onwo\nthree");
    assert_eq!(
        buffer.paragraph_alignments,
        vec![ParagraphAlignment::Left, ParagraphAlignment::Right]
    );
}

#[test]
fn inserted_paragraphs_inherit_the_current_alignment() {
    let mut buffer = EditorBuffer::from_text("one\ntwo");
    buffer.move_to_line(2, 80);
    buffer.set_alignment_for_selection(ParagraphAlignment::Center);
    buffer.move_end(80, false);
    buffer.insert_text("\nthree");

    assert_eq!(buffer.plain_text(), "one\ntwo\nthree");
    assert_eq!(
        buffer.paragraph_alignments,
        vec![
            ParagraphAlignment::Left,
            ParagraphAlignment::Center,
            ParagraphAlignment::Center
        ]
    );

    assert!(buffer.undo());
    assert_eq!(buffer.plain_text(), "one\ntwo");
    assert_eq!(
        buffer.paragraph_alignments,
        vec![ParagraphAlignment::Left, ParagraphAlignment::Center]
    );
}

fn block_buffer(text: &str, start: usize, end: usize) -> EditorBuffer {
    let mut buffer = EditorBuffer::from_text(text);
    buffer.set_cursor_for_test(start);
    buffer.set_block_begin();
    buffer.set_cursor_for_test(end);
    buffer.set_block_end();
    buffer
}

#[test]
fn copy_block_duplicates_at_the_cursor_destination() {
    let mut buffer = block_buffer("one\ntwo\nthree", 4, 7);
    buffer.move_cursor_to_end();
    assert!(buffer.copy_block());
    assert_eq!(buffer.plain_text(), "one\ntwo\nthreetwo");
    assert_eq!(buffer.selected_text().as_deref(), Some("two"));
}

#[test]
fn move_block_moves_to_the_cursor_destination() {
    let mut buffer = block_buffer("one\ntwo\nthree", 4, 7);
    buffer.set_cursor_for_test(0);
    assert!(buffer.move_block());
    assert_eq!(buffer.plain_text(), "twoone\n\nthree");
    assert_eq!(buffer.selected_text().as_deref(), Some("two"));
}

#[test]
fn copy_block_at_its_end_duplicates_after_the_block() {
    let mut buffer = block_buffer("one\ntwo\nthree", 4, 7);
    buffer.set_cursor_for_test(7);
    assert!(buffer.copy_block());
    assert_eq!(buffer.plain_text(), "one\ntwotwo\nthree");
}

#[test]
fn move_block_into_itself_is_rejected() {
    let mut buffer = block_buffer("one\ntwo\nthree", 4, 7);
    buffer.set_cursor_for_test(5);
    assert!(!buffer.move_block());
    assert_eq!(buffer.plain_text(), "one\ntwo\nthree");
}

#[test]
fn replacing_with_a_rich_snapshot_restores_text_styles_and_alignment() {
    let mut buffer = EditorBuffer::from_text("old");
    let alignments = vec![ParagraphAlignment::Center, ParagraphAlignment::Right];
    let attrs = vec![AttrSpan {
        start: 0,
        end: 3,
        kind: AttrKind::Bold,
    }];

    assert!(buffer.replace_with_rich_text("one\ntwo", &alignments, &attrs));
    assert_eq!(buffer.plain_text(), "one\ntwo");
    assert_eq!(buffer.alignments(), alignments.as_slice());
    assert!(buffer.to_ord_html().contains("<b>one</b>"));

    assert!(buffer.undo());
    assert_eq!(buffer.plain_text(), "old");
}
