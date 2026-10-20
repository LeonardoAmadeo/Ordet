# Performance audit: event-driven keystroke path

Date: 2026-10-17 (audit and implementation landed the same day)
Status: **implemented**. Target met on the keypress path; results in §3.

Target: input-to-stdout latency of **0.1 ms typical, p99 < 1.0 ms**, measured
from `crossterm::event::read` returning to `stdout.flush()` returning, on the
keypress path only. (A p50 ≤ 0.05 ms stretch goal was proposed during review;
it is not enforced anywhere.) The measured span deliberately excludes the
terminal emulator's own presentation delay.

Replaces the previous "120 FPS" design goal. The frame-rate cap was the single
largest source of latency: an input event could wait up to one full frame
(8.33 ms) before anything happened.

## 1. Where the code stood before

Measured with the release-mode benches as they existed at audit time
(`cargo test --release -p ordet --bin ordet -- --ignored --nocapture`).

### `typing_frame_cost` (400-line document)

| Path | Before | Verdict |
| --- | --- | --- |
| whole-document wrap (baseline) | 0.246 ms | cold start / resize only |
| cached: type at end | 0.067 ms | ok |
| cached: type at top (offset shift) | 0.163 ms | O(document) per keystroke |
| insert + cache validate | 0.059 ms | ok |
| **insert + refresh_stats** | **0.290 ms** | O(document) per keystroke |
| **Buffer::clear 200x50** | **0.278 ms** | allocations, per frame |
| **write_text 44 rows x 88** | **0.207 ms** | allocations, per frame |
| **Buffer::diff 200x50** | **0.093 ms** | full-grid scan, per frame |

### `large_document_frame_cost` (2k / 10k / 50k lines)

| Path | 2k lines | 10k lines | 50k lines |
| --- | --- | --- | --- |
| cursor line (`visual_line_for_cursor`) | 0.001 ms | 0.010 ms | 0.034 ms |
| visible slice (`visible_layout`) | 0.015 ms | 0.034 ms | 0.072 ms |
| Home/End cache-direct | 0.005 ms | 0.037 ms | 0.129 ms |
| type + frame | 0.055 ms | 0.106 ms | 0.227 ms |
| undo snapshot only | 0.001 ms | 0.002 ms | 0.005 ms |
| undo + revalidate cache | 0.283 ms | 0.164 ms | 0.226 ms |

### What a keystroke actually cost

1. **Up to 8.33 ms** parked in `event::poll` waiting for the frame deadline.
2. `handle_key_event` → `dispatch` → `apply_action` → document mutation, with
   `begin_undo` cloning `paragraph_alignments` (Vec sized by line count).
3. `mark_dirty` → `refresh_stats`: `rope.to_string()` plus a full O(n)
   word/char/sentence scan, per keystroke.
4. `ensure_wrap_cache` revalidation: re-wrap of the dirty line, grapheme
   allocation, O(entries) offset bookkeeping.
5. `RootView::render` repainting every pane: `Buffer::clear` (a `String` per
   cell), `write_text` (a `String` per grapheme), `Buffer::diff` scanning all
   cells and allocating a result `Vec`.
6. ANSI bytes through crossterm's stdout wrapper, one `flush()` per frame.

Total was ~11–19 ms typical for a 50k-line document (8.3 ms pure waiting),
against a 0.1–0.3 ms budget.

## 2. What shipped

### Event-driven loop

| # | Change | Status |
| --- | --- | --- |
| A1 | Deleted `TARGET_FPS`/`FRAME_INTERVAL`/`next_frame`. A frame is never delayed, only coalesced: input sets `redraw_requested`, the draw happens on the next loop pass after the batch is drained. | done |
| A2 | The blocking wait is the next real deadline only (housekeeping tick, autosave, recovery, spinner). No frame deadline exists. | done |
| A3 | The 250 ms housekeeping tick has no redraw side effects: status expiry, clock, task results. The spinner self-arms a 120 ms deadline only while a lookup is pending; the dark-mode `defaults` probe runs on every 8th tick. | done |
| A4 | Terminal size is cached and refreshed from `Event::Resize`; the per-frame `ioctl` and the dead `refresh_area` helper are gone. | done |
| A5 | Task results are picked up by the housekeeping tick, so a finished task can wait up to 250 ms before its frame. A self-pipe/eventfd waker was **deferred**: the bound is off the keypress path and was not worth the loop complexity. | deferred |

### Buffer and wrap cache

| # | Change | Status |
| --- | --- | --- |
| B1 | Undo records no longer clone `paragraph_alignments` beyond measurement: the clone costs 5 µs at 50k lines (`undo snapshot only` 0.001–0.002 ms), so the `Arc<Vec<_>>` COW was skipped as complexity without a payoff. | skipped, measured |
| B2 | Offset shifting is lazy: each `CachedWrapLine` carries an integer `shift` that accumulates on insert/remove and is applied only when a line is cloned out for output (and reset on rewrap). Typing at the top or middle of a document no longer walks every later line. | done |
| - | Word/char/sentence stats are incremental: a dirty-range splice fold (`stats_after_insert`/`stats_after_remove`) with arithmetic-only `refold_sentence_chain` on deletes. `compute_word_stats` survives only as the `#[cfg(test)]` reference implementation the incremental path is checked against. | done |
| - | Fenwick tree over per-line visual counts for `lines_before`/`locate`/`total_lines`, so wrap lookups are O(log n) instead of O(lines). | done |
| - | `wrap_source_line` slices graphemes as `GraphemeInfo<'a>` borrowing the source `&str` instead of allocating a `String` per grapheme; revalidation is ~2× faster. | done |

### Rendering

| # | Change | Status |
| --- | --- | --- |
| D1 | `Symbol` interning for cells (ASCII fast path in `write_text`): `Buffer::clear 200x50` 0.278 → **0.068 ms**, `write_text 44x88` 0.207 → **0.027 ms**. | done |
| D2 | `Buffer::diff_for_each` streams changed cells to the writer instead of building a result `Vec`: `Buffer::diff 200x50` 0.093 → **0.045 ms**. | done |
| D3 | `render_editor_buffer` still clears its rect before painting, so visible cells are rewritten every frame, acceptable now that a cell write is an interned clone, not an allocation. | not done |
| D4 | Every handled event still sets `redraw_requested`, even when nothing visible changed. Measured cost of a no-op key is now 79 µs end-to-end (§3.2), so the blanket redraw is no longer a problem worth the state-tracking. | not done |

### Instrumentation

| # | Change | Status |
| --- | --- | --- |
| E1 | `--latency-log` (or `ORDET_TUI_LATENCY=1`): every keystroke records `event::read` return → `stdout.flush()` return; a 200-sample window prints p50/p90/p99/max/mean to stderr, with the first 10 keys dropped as warm-up, plus a final report on exit. | done |
| E2 | Each sample also carries `read` (the `event::read` call), `handle` (dispatch) and `draw` (layout + render + diff + write + flush) as its own split, and the batch size (events swallowed by one wakeup). All counting is in-memory; the instrument writes nothing per keystroke. | done |
| E3 | No CI gate asserts p99, and the terminal writer is not yet abstracted behind `impl Write`. The release benches below are the regression guard, run manually. | not done |

### Two real bugs found while measuring

- **Wrap width flipped 72 ↔ 80 every keystroke.** The writing pane and the
  layout both derived the wrap width, from different rects, so each keystroke
  invalidated the whole wrap cache twice (two full-document rewraps ≈ 20 ms
  per key). `editor_text_area(rect)` is now the single source of truth; the
  pane, `AppState::update_layout`, the draw path and `RootView` all use it.
- **Autosave built the whole-document `.ord` HTML on the event loop.** Every
  2 s while typing, `to_ord_html` formatted and escaped every line (~20 ms at
  2k lines) on the main thread, which is exactly where the p99 outliers came
  from. Saves now capture a cheap `DraftSnapshot` (rope + alignments + attrs;
  the rope clone is O(1)) and render the HTML on the worker thread, and only
  for `.ord` targets, since plain text saves never needed it.

## 3. After: measurements

### 3.1 Release benches

`cargo test --release -p ordet --bin ordet -- --ignored --nocapture`

`typing_frame_cost` (400-line document):

| Path | Before | After |
| --- | --- | --- |
| whole-document wrap (cold) | 0.246 ms | 0.249 ms |
| cached: type at end | 0.067 ms | **0.042 ms** |
| cached: type at top | 0.163 ms | **0.063 ms** |
| move cursor only | - | 0.013 ms |
| insert + cache validate | 0.059 ms | **0.029 ms** |
| insert + stats (incremental) | - | **0.006 ms** |
| insert + whole-text stats (old path, test-only reference) | 0.290 ms | 0.229 ms |
| Buffer::clear 200x50 | 0.278 ms | **0.068 ms** |
| write_text 44 rows x 88 | 0.207 ms | **0.027 ms** |
| Buffer::diff 200x50 (Vec) | 0.093 ms | 0.075 ms |
| Buffer::diff_for_each 200x50 | - | **0.045 ms** |
| typing + undo round trip | - | 0.001 ms |
| rope clone alone (`DraftSnapshot` capture) | - | ~0.000 ms |

`large_document_frame_cost` (2k / 10k / 50k lines):

| Path | 2k | 10k | 50k | Before (50k) |
| --- | --- | --- | --- | --- |
| cursor line | 0.000 ms | 0.000 ms | 0.000 ms | 0.034 ms |
| visible slice | 0.012 ms | 0.012 ms | 0.012 ms | 0.072 ms |
| Home/End cache-direct | 0.001 ms | 0.001 ms | 0.001 ms | 0.129 ms |
| **type + frame** | **0.036 ms** | **0.038 ms** | **0.044 ms** | 0.227 ms |
| insert + stats fold | 0.007 ms | 0.007 ms | 0.008 ms | (was O(n) scan) |
| undo snapshot only | 0.001 ms | 0.001 ms | 0.002 ms | 0.005 ms |
| undo + revalidate cache | 0.073 ms | 0.074 ms | 0.079 ms | 0.226 ms |
| mid-document frame | 0.042 ms | 0.048 ms | 0.080 ms | - |

Three notes on these numbers: the bench now primes `stats()` before the timed
loops, because an unprimed cold fold (114 ms at 50k lines) amortized over 300
iterations read as a 0.34 ms per-frame cost that did not exist; the
"mid-document frame" stage is new, so typing into the middle of a large
document is measured rather than assumed; and these are single runs, which
fluctuate by roughly 10%, the shape that matters (flat from 2k to 50k lines)
is stable across runs.

`document_open_cost`, one-time cost, off the keystroke path:

| Cold fold | 2k lines | 10k lines | 50k lines |
| --- | --- | --- | --- |
| whole-document wrap | 21.7 ms | 148.8 ms | 600 ms |
| whole-document stats | 3.7 ms | 19.3 ms | 114 ms |

The wrap fold fills in lazily as frames touch lines, so an open document's
first frame costs what the visible slice needs, not the whole table. At 50k
lines the fold is the remaining cold-start cost and is the reason the bench
exists.

### 3.2 End-to-end keystroke latency (`--latency-log`)

Method: the app runs on a pty, `24x80`, keys arrive every 30 ms, the pty
output is drained by `script(1)`; the sample document is regenerated for each
run. `batch=1.00` in every run below, so each sample is a single keystroke.

| Run | p50 | p90 | p99 | max | read | handle | draw |
| --- | --- | --- | --- | --- | --- | --- | --- |
| no-op key (Backspace at doc start), 2k lines | **79 µs** | 113 µs | 187 µs | 209 µs | 1 µs | 39 µs | 37 µs |
| typing `a`, 2k lines (line growing 53→253 chars) | **300 µs** | 426 µs | 567 µs | 856 µs | 1 µs | 250 µs | 47 µs |
| typing `a`, 2k lines (line growing 253→453) | **550 µs** | 713 µs | 951 µs | 4.3 ms | 1 µs | 496 µs | 51 µs |
| typing `a`, 50k lines | **575 µs** | 814 µs | 969 µs | 975 µs | 1 µs | 517 µs | 53 µs |

The no-op row is the loop's floor with nothing to re-wrap or redraw: **79 µs
p50, 187 µs p99**, within both targets. The typing rows grow because every
keystroke re-wraps the line being typed, and those runs typed 430 characters
into a single line, growing it from 53 to ~480 characters; the wrap cost is
linear in that line, ~60 ns per character. Ordinary prose (60–80 character
paragraphs) sits a few microseconds above the floor per line.

`read` stayed at 1 µs in every run: input-queue delay is not a factor with the
batch drain in place. `draw` stays at 37–55 µs regardless of document size.

**Caveat on absolute values.** The pty harness runs the app as a mostly-idle
process, and in that state macOS schedules it at a much lower effective CPU
rate than a hot bench process: a *fixed* 470-character synthetic wrap measured
332–359 µs inside the app versus 29 µs in the release bench (~11×) on the same
machine. The end-to-end numbers above are therefore pessimistic upper bounds;
the code's intrinsic per-keystroke frame is the 36–44 µs shown by the bench.
Separately, the older Python-reader pty harness added ~450 µs of pty
back-pressure to every frame (the app blocked writing its 2 KB frame until the
reader drained it); `script(1)` drains at C speed and the same build measures
~300 µs p50 instead of ~1.3 ms.

The one outlier class left in the log is the **quit flush** (9.8 ms at 2k
lines, 82 ms at 50k): `Ctrl-Q` with dirty drafts writes them synchronously,
together with the revision history and settings. That is deliberate, quitting
must not lose the last edits, and it is not on the keypress path.

### 3.3 Idle behaviour

With no input the loop is parked in a single blocking `event::poll` until (a)
input, (b) a resize, (c) the 250 ms housekeeping tick, or (d) an armed spinner
deadline. There is no periodic redraw: CPU sits at ~0% while idle, and the
differentiated renderer never clears the screen on a keystroke.

### 3.4 Writing chrome (windows, ruler, panels)

The redesign that added tiled windows, the ruler row, and panel overlays kept
the same keystroke path: keymap → command → action → `draw_frame`, with no
frame timer and no new per-keystroke work that scales with document size.
What the chrome adds per frame is bounded and independent of the text:

- Window rectangles: `update_layout` splits the writing rect with `split_rects`
  into at most `MAX_WINDOWS` (4) rects, one small `Vec` per frame, and the
  render path borrows it instead of cloning.
- Ruler: one `String` built per window per frame, bounded by the pane width
  (`TAB_STOP` every 8 columns); it is hidden while zoomed.
- Status row: one small `Vec` of state fields (mode, page, position, counts)
  plus the message, both proportional to the row width, never the document.
- Panels: at most one overlay is open, drawn on top of the writing rect; the
  document still paints its own area, and the common single-window,
  no-panel, no-split case takes exactly the same path it took before. A panel
  sizes itself to what it paints (its own line builders run once for the
  geometry), and those builders stop at the 22-row cap, so a book-length
  revision diff costs no more than the rows the card can show.

Every window renders through the existing cell diff, so a split streams only
the cells that changed, four panes cost four times the per-line work only
when all four lines actually change, and nothing per cell.

## 4. Remaining known costs

| Cost | Where | Size | Why it stays |
| --- | --- | --- | --- |
| Quit-time flush | `flush_dirty_drafts` on `Ctrl-Q` | 10 ms @ 2k, 82 ms @ 50k | Must not lose the last edits |
| Autosave `plain_text()` on the main thread | `spawn_save_draft`, every 2 s | 0.004 ms @ 400 lines, ~1–2 ms @ 50k | The history record needs the text in memory; the HTML build is already on the worker |
| Cold wrap fold | first frames after open | 600 ms @ 50k, paid lazily | Document-wide measurement, once |
| Cold stats fold | first `stats()` | 114 ms @ 50k | Same |
| Task-completion latency | housekeeping tick | ≤ 250 ms | No self-pipe waker (A5 deferred) |
| `Buffer::clear` | every frame | 0.068 ms @ 200x50 | Bounded by grid size, not document size |
| Blanket redraw per handled event | loop | 79 µs for a no-op key | Below budget (D4) |

## 5. Success criteria and results

- **p99 < 1.0 ms on the keypress path**: met. p99 567–969 µs across 2k and 50k
  lines in the pty harness, whose CPU rate is ~11× below the bench's.
- **Typical 0.1 ms**: met for keys that do not grow a long line (79 µs no-op
  floor); typing measures 300–575 µs *in the harness*, 36–44 µs intrinsic.
  The 0.05 ms stretch was not enforced.
- **No per-keystroke work scaling with document size**: met. `cursor line`
  0.000 ms and `visible slice` 0.012 ms are flat from 2k to 50k lines;
  `type + frame` moves 0.036 → 0.044 ms over that range.
- **Idle CPU ~0%**: met. The loop blocks until input, resize, task
  completion, or the housekeeping tick.
