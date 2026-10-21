use super::EditorBuffer;
use std::time::Instant;

fn sample_document(lines: usize) -> String {
    (0..lines)
        .map(|index| {
            format!(
                "Paragraph {index} holds a sentence or two of ordinary prose so that the \
                 wrapper has real work to do at a realistic terminal width."
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn report(label: &str, iterations: u32, elapsed: std::time::Duration) {
    let per_frame = elapsed.as_secs_f64() / f64::from(iterations);
    println!(
        "{label:<34} {:>8.3} ms/frame  {:>9.0} frames/sec",
        per_frame * 1000.0,
        1.0 / per_frame
    );
}

/// A full frame as the app does it: scroll bookkeeping plus wrapping only
/// the visible rows.
#[test]
#[ignore]
fn typing_frame_cost() {
    let text = sample_document(400);
    let width = 88usize;
    let rows = 44usize;
    let iterations = 600;

    // Baseline: what the app used to pay, wrapping the whole document.
    let buffer = EditorBuffer::from_text(&text);
    let started = Instant::now();
    for _ in 0..iterations {
        let layout = buffer.layout(width);
        let _ = layout.line_index_for_cursor(buffer.cursor());
    }
    report("cold: whole-document wrap", iterations, started.elapsed());

    // Cached, typing at the end of the document.
    let mut buffer = EditorBuffer::from_text(&text);
    buffer.move_cursor_to_end();
    let _ = buffer.visible_layout(width, 0, rows, 1);
    let started = Instant::now();
    for _ in 0..iterations {
        buffer.insert_char_with_mode('a', false);
        let _ = buffer.visual_line_for_cursor(width, buffer.cursor());
        let _ = buffer.visible_layout(width, 0, rows, 1);
    }
    report("cached: type at end", iterations, started.elapsed());

    // Cached, typing at the top, which shifts every later line's offsets.
    let mut buffer = EditorBuffer::from_text(&text);
    let _ = buffer.visible_layout(width, 0, rows, 1);
    let started = Instant::now();
    for _ in 0..iterations {
        buffer.insert_char_with_mode('a', false);
        buffer.move_to_top();
        buffer.insert_char_with_mode('b', false);
        let _ = buffer.visual_line_for_cursor(width, buffer.cursor());
        let _ = buffer.visible_layout(width, 0, rows, 1);
    }
    report("cached: type at top", iterations, started.elapsed());

    // Cursor movement only, where the memo should do nothing at all.
    let mut buffer = EditorBuffer::from_text(&text);
    let _ = buffer.visible_layout(width, 0, rows, 1);
    let started = Instant::now();
    for _ in 0..iterations {
        buffer.move_left(false);
        buffer.move_right(false);
        let _ = buffer.visual_line_for_cursor(width, buffer.cursor());
        let _ = buffer.visible_layout(width, 0, rows, 1);
    }
    report("cached: move cursor only", iterations, started.elapsed());

    // Split the cached path to find where the time actually goes.
    let mut buffer = EditorBuffer::from_text(&text);
    buffer.move_cursor_to_end();
    let _ = buffer.total_visual_lines(width);
    let started = Instant::now();
    for _ in 0..iterations {
        buffer.insert_char_with_mode('a', false);
        let _ = buffer.total_visual_lines(width);
    }
    report("insert + cache validate", iterations, started.elapsed());

    let mut buffer = EditorBuffer::from_text(&text);
    buffer.move_cursor_to_end();
    let _ = buffer.total_visual_lines(width);
    let started = Instant::now();
    for _ in 0..iterations {
        buffer.insert_char_with_mode('a', false);
        let _ = buffer.visual_line_for_cursor(width, buffer.cursor());
    }
    report(
        "insert + visual_line_for_cursor",
        iterations,
        started.elapsed(),
    );

    // Which rope primitive is actually expensive?
    let rope = ropey::Rope::from_str(&text);
    let started = Instant::now();
    for _ in 0..iterations {
        for i in 0..rope.len_lines() {
            std::hint::black_box(rope.line_to_char(i));
        }
    }
    report("line_to_char x 400 lines", iterations, started.elapsed());

    let started = Instant::now();
    for _ in 0..iterations {
        for i in 0..rope.len_lines() {
            std::hint::black_box(rope.line(i).len_chars());
        }
    }
    report(
        "rope.line(i).len_chars() x 400",
        iterations,
        started.elapsed(),
    );

    let started = Instant::now();
    for _ in 0..iterations {
        for i in 0..rope.len_lines() {
            std::hint::black_box(rope.line_to_byte(i));
        }
    }
    report("line_to_byte x 400 lines", iterations, started.elapsed());

    let started = Instant::now();
    for _ in 0..iterations {
        for i in 0..rope.len_lines() {
            std::hint::black_box(rope.byte_to_char(rope.line_to_byte(i)));
        }
    }
    report("line_to_byte + byte_to_char", iterations, started.elapsed());

    // Stats: the old path rebuilt the whole text and rescanned it on
    // every keystroke. The new one records a line range and folds once
    // per read. Both are measured so the comparison stays honest.
    let mut buffer = EditorBuffer::from_text(&text);
    let started = Instant::now();
    for _ in 0..iterations {
        buffer.insert_char_with_mode('a', false);
        std::hint::black_box(buffer.stats());
    }
    report(
        "insert + stats (incremental)",
        iterations,
        started.elapsed(),
    );

    let mut buffer = EditorBuffer::from_text(&text);
    let started = Instant::now();
    for _ in 0..iterations {
        buffer.insert_char_with_mode('a', false);
        let plain = buffer.plain_text();
        std::hint::black_box(crate::stats::compute_word_stats(&plain));
    }
    report(
        "insert + whole-text stats (old)",
        iterations,
        started.elapsed(),
    );

    let buffer = EditorBuffer::from_text(&text);
    let started = Instant::now();
    for _ in 0..iterations {
        std::hint::black_box(buffer.plain_text());
    }
    report("plain_text() (whole doc)", iterations, started.elapsed());

    // The render path: clear, fill, diff at a realistic terminal size.
    use ordet_tui_framework::{Buffer, Point, Rect, Style};
    let area = Rect::new(0, 0, 200, 50);
    let mut front = Buffer::new(area);
    let mut back = Buffer::new(area);
    let started = Instant::now();
    for _ in 0..iterations {
        front.clear(Style::default());
    }
    report("Buffer::clear 200x50", iterations, started.elapsed());

    let line = "the quick brown fox jumps over the lazy dog again and again ".repeat(3);
    let started = Instant::now();
    for _ in 0..iterations {
        for row in 0..44u16 {
            front.write_text(Point::new(4, row), &line, Style::default(), 88);
        }
    }
    report("write_text 44 rows x 88", iterations, started.elapsed());

    back.clear(Style::default());
    let started = Instant::now();
    for _ in 0..iterations {
        std::hint::black_box(front.diff(&back));
    }
    report("Buffer::diff 200x50 (vec)", iterations, started.elapsed());

    let started = Instant::now();
    for _ in 0..iterations {
        let mut changed = 0usize;
        front.diff_for_each(&back, |_, _| changed += 1);
        std::hint::black_box(changed);
    }
    report(
        "Buffer::diff_for_each 200x50",
        iterations,
        started.elapsed(),
    );

    // Isolate the undo snapshot, which clones the rope on every keystroke.
    let mut buffer = EditorBuffer::from_text(&text);
    let started = Instant::now();
    for _ in 0..iterations {
        buffer.insert_char_with_mode('a', false);
    }
    report(
        "typing: with rope-snapshot undo",
        iterations,
        started.elapsed(),
    );

    let rope = ropey::Rope::from_str(&text);
    let started = Instant::now();
    for _ in 0..iterations {
        let _clone = rope.clone();
        std::hint::black_box(&_clone);
    }
    report("rope clone alone", iterations, started.elapsed());

    let started = Instant::now();
    for _ in 0..iterations {
        buffer.insert_char_with_mode('a', false);
        buffer.undo();
    }
    report("typing + undo round trip", iterations, started.elapsed());

    println!("document: {} chars", text.len());
}

/// The one-shot cost of showing a large document: the first frame wraps
/// every line, and the status bar's first read folds the whole-document
/// stats. Neither is on the keystroke path, but both delay the first
/// frame after opening a file.
#[test]
#[ignore]
fn document_open_cost() {
    let width = 88usize;
    for lines in [2_000usize, 10_000, 50_000] {
        let text = sample_document(lines);

        let wrap_buffer = EditorBuffer::from_text(&text);
        let started = Instant::now();
        std::hint::black_box(wrap_buffer.visible_layout(width, 0, 44, 1));
        report(
            &format!("cold first frame, {lines} lines"),
            1,
            started.elapsed(),
        );

        let started = Instant::now();
        while wrap_buffer.warm_wrap_cache(width, 512) {}
        report(
            &format!("full wrap warm, {lines} lines"),
            1,
            started.elapsed(),
        );

        let stats_buffer = EditorBuffer::from_text(&text);
        let started = Instant::now();
        std::hint::black_box(stats_buffer.stats_if_ready());
        report(
            &format!("cold first stats, {lines} lines"),
            1,
            started.elapsed(),
        );

        let started = Instant::now();
        std::hint::black_box(stats_buffer.stats());
        report(
            &format!("full stats fold, {lines} lines"),
            1,
            started.elapsed(),
        );
    }
}

/// The per-keystroke path on a big document, scrolled to the bottom,
/// retyping a character. This is where an O(scroll) walk in
/// `visible_layout` or `visual_line_for_cursor` shows up, because neither is
/// exercised by the small-document bench above.
#[test]
#[ignore]
fn large_document_frame_cost() {
    let width = 88usize;
    let rows = 44usize;
    let iterations = 300;

    for lines in [2_000usize, 10_000, 50_000] {
        let text = sample_document(lines);
        let mut buffer = EditorBuffer::from_text(&text);
        buffer.move_cursor_to_end();
        while buffer.warm_wrap_cache(width, 512) {}
        let total = buffer.total_visual_lines(width);
        let scroll = total.saturating_sub(rows);
        // Warm the cache exactly as a real frame would. Stats too: the
        // first `stats()` folds the whole document, and leaving that fold
        // inside the timed loops amortizes ~88ms at 50k lines over the
        // iterations, which reads as a per-frame cost that does not exist.
        let _ = buffer.visual_line_for_cursor(width, buffer.cursor());
        let _ = buffer.visible_layout(width, scroll, rows, 1);
        let _ = buffer.stats();

        let started = Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(buffer.visual_line_for_cursor(width, buffer.cursor()));
        }
        report(
            &format!("cursor line, {lines} lines"),
            iterations,
            started.elapsed(),
        );

        let started = Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(buffer.visible_layout(width, scroll, rows, 1));
        }
        report(
            &format!("visible slice, {lines} lines"),
            iterations,
            started.elapsed(),
        );

        // Home/End go through the cache-direct helpers, which clone one
        // wrapped line instead of the whole document.
        let started = Instant::now();
        for _ in 0..iterations {
            buffer.move_home(width, false);
            buffer.move_end(width, false);
        }
        report(
            &format!("Home/End cache-direct, {lines} lines"),
            iterations,
            started.elapsed(),
        );

        // What a keystroke pays before rendering: undo record, wrap
        // validation, stats fold, then the frame itself.
        let started = Instant::now();
        for _ in 0..iterations {
            buffer.insert_char_with_mode('a', false);
            std::hint::black_box(buffer.visual_line_for_cursor(width, buffer.cursor()));
            std::hint::black_box(buffer.visible_layout(width, scroll, rows, 1));
            std::hint::black_box(buffer.stats());
        }
        report(
            &format!("type + frame, {lines} lines"),
            iterations,
            started.elapsed(),
        );

        let started = Instant::now();
        for _ in 0..iterations {
            buffer.insert_char_with_mode('a', false);
            std::hint::black_box(buffer.stats());
        }
        report(
            &format!("  insert + stats fold, {lines} lines"),
            iterations,
            started.elapsed(),
        );

        // Split the keystroke cost: the undo rope clone versus the wrap
        // cache revalidation that the edit forces.
        let started = Instant::now();
        for _ in 0..iterations {
            buffer.insert_char_with_mode('a', false);
        }
        report(
            &format!("  undo snapshot only, {lines} lines"),
            iterations,
            started.elapsed(),
        );

        let started = Instant::now();
        for _ in 0..iterations {
            buffer.insert_char_with_mode('a', false);
            std::hint::black_box(buffer.visual_line_for_cursor(width, buffer.cursor()));
        }
        report(
            &format!("  undo + revalidate cache, {lines} lines"),
            iterations,
            started.elapsed(),
        );

        // A full keystroke frame typing into a mid-document paragraph.
        // This is where an O(entries) span shift or an O(lines) fold
        // shows up, because the edit is neither at the top nor at the
        // end of the document.
        let mut mid = EditorBuffer::from_text(&text);
        mid.move_to_line(lines / 2, width);
        let _ = mid.visual_line_for_cursor(width, mid.cursor());
        let _ = mid.stats();
        let mid_scroll = mid.total_visual_lines(width).saturating_sub(rows);
        let _ = mid.visible_layout(width, mid_scroll, rows, 1);
        let started = Instant::now();
        for _ in 0..iterations {
            mid.insert_char_with_mode('a', false);
            std::hint::black_box(mid.visual_line_for_cursor(width, mid.cursor()));
            std::hint::black_box(mid.visible_layout(width, mid_scroll, rows, 1));
            std::hint::black_box(mid.stats());
        }
        report(
            &format!("  mid-document frame, {lines} lines"),
            iterations,
            started.elapsed(),
        );
        println!();
    }
}
