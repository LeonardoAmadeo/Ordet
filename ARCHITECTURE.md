# Ordet Architecture

`ordet` is split into a reusable low-level framework crate and a word processor application crate.

## Design goals

- Keep the framework small and explicit.
- Separate state, layout, rendering, input, and background work.
- Make the app compose views from pure-ish render functions and thin stateful components.
- Keep terminal I/O behind a backend boundary.
- Use a production-grade text buffer internally, but hide it behind an Ordet-specific API.
- Hold ~0.1ms input-to-flush latency on large documents: no per-frame or per-keystroke work may scale with document size.

## Workspace structure

- `crates/ordet-tui-framework`: reusable TUI primitives and runtime
- `crates/ordet`: terminal word processor built on the framework
- `crates/ordet-format`: `.ord` file loading and saving

`components.rs` keeps `RootView` and the shared rendering helpers. Panels,
the welcome screen, modal dialogs, the status bar, and component tests live
under `crates/ordet/src/components/`.

## 1. State and vie

The application owns one top-level state container, `AppState`, in `crates/ordet/src/state.rs`.

It holds:
- document state
- window state (the panes on screen) and open-panel state
- focus state
- layout cache (header, ruler, writing area, status, and one rect per window)
- transient UI status
- shared clipboard kill ring (`clipboard.rs`)
- registered workspace folders plus their cached scans (`workspace.rs`)
- revision history (`history.rs`)

Views do not own hidden document state. They receive immutable model access when rendering and mutable model access only when handling events.

## 2. Component model

The framework exposes `Component<Model, Command, Message, Action>` in `crates/ordet-tui-framework/src/component.rs`.

Each component can:

- `render(...)`
- `handle_event(...)`
- `on_focus_changed(...)`
- declare a `focus_id()`
- emit actions through `EventCx`
- request focus through `EventCx`

This keeps components composable and testable. The app-level `RootView` coordinates child components instead of embedding hidden widget trees.

## 3. Layout

`crates/ordet-tui-framework/src/layout.rs` provides a small split-based layout engine (fixed lengths, percentages, minimums, splits). Window rectangles are computed by hand in `AppState::update_layout`: the writing rect is tiled by `split_rects` into up to `MAX_WINDOWS` panes (one full, two columns, three as a full-height left pane beside a stacked pair, four as quadrants) with a one-cell gutter between panes and a fallback to a single pane when the terminal is too small to split. `WindowState::panes` is the slot order: `Window ▸ Move to slot` swaps two pane ids without changing the tile geometry.

`AppState::window_rects()` is the single source the renderer, the cursor and scroll bookkeeping, and mouse hit-testing all read: `active_editor_area` answers for the focused window, and `window_text_area` applies the same title-row and margin inset the renderer paints, so typing, clicking, wrapping, and the ruler always agree on which pane owns the keyboard.

Text width has one subtlety: the line-number gutter steals columns from the text, and every cursor, scroll, and render computation for the same buffer must wrap at the same width, or the cursor lands on a different wrap than the one on screen. `components::wrap_and_gutter` is the single source: it returns `(wrap_width, gutter_width)`, and the writing pane, notes pane, scroll bookkeeping, go-to-line, and mouse hit-testing all go through it.

## 4. Rendering pipeline

The rendering path is intentionally explicit:

1. `Terminal::draw(...)` creates a fresh `Frame`
2. components render into an offscreen `Buffer`
3. the new `Buffer` is diffed against the previous frame
4. only changed cells are written to the terminal backend
5. the cursor is restored after drawing
6. queued raw overlays (OSC 52 clipboard writes) flush through `overlay_sequence`

Core files:

- `crates/ordet-tui-framework/src/buffer.rs`
- `crates/ordet-tui-framework/src/frame.rs`
- `crates/ordet-tui-framework/src/terminal.rs`

The buffer handles:

- Unicode grapheme segmentation
- wide-character continuation cells
- clipping into rectangles

This is a low-level rendering model, not a retained widget renderer.

The chrome is deliberately thin: `components::draw_header` paints the menu
bar and, while one window is open, a centred page name (unless `Alt-M` hid
it: the bar then owns no rows, and `update_layout` starts the document on the
terminal's first line). In a split the header keeps only the menus;
`render_editor_window` gives each pane a centred title row and lays the
ruler over its top padding row (with the focused window's caret column
inverted), and `StatusBar` splits the bottom row into the important message
on the left and a right-aligned state cluster (`status_fields`). `set_status`
paints prompts, confirmations, warnings, and failures; routine success calls
`set_status_quiet`, so saving, switching, and other completed actions leave
the line blank. No hint bar exists; `F1` owns the key reference. Narrow rows
shed the cluster's leading fields rather than clipping.

Panels are cards sized to what they print (`components::panel_rect`): the
renderer counts the rows it will paint (stats lines, settings rows plus
their wrapped values, or a walked help table), and the box is that tall,
capped at 22 rows (taller content scrolls). Width follows the same rule:
each card is as wide as its widest line (a label column plus its longest
value, a key column plus its longest description), capped at 72 columns, so
a wide terminal gets the same compact card a narrow one does and no card
carries an empty right half. Settings is the exception: it keeps a fixed
width so changing a value never resizes the panel. Cards sit at
the body's top-right corner, under the menu bar, and carry their title
centred in the top border (`draw_panel_frame`) instead of an inner header
row, so a two-row box already holds content. `state::panel_rect` delegates
here, and the mouse hit test shares the result, so clicks always land on the
rows on screen.

There is no frame budget: the loop draws on demand. Rendering wraps only the visible rows (`visible_layout`), the cursor line comes from `visual_line_for_cursor`, and scroll bookkeeping from `total_visual_lines`, none of which build a document-wide layout.

## 5. Input and focus

Input is split into layers:

- raw terminal events from `crossterm` (keys, bracketed paste, mouse, resize)
- semantic commands through `Keymap<Command>`
- component event handling via `UiEvent`

Framework files:

- `crates/ordet-tui-framework/src/command.rs`
- `crates/ordet-tui-framework/src/event.rs`
- `crates/ordet-tui-framework/src/focus.rs`

The app keeps keybindings separate from editing behavior, so command policy is decoupled from raw key events.

`Keymap::bind` panics if a chord is rebound to a different command, so a shadowed binding fails at startup instead of quietly becoming unreachable. `commands::build_keymap`, `build_prefix_keymap` and `build_o_prefix_keymap` hold every binding; a test asserts the documented `Ctrl-K` and `Ctrl-O` tables stay in step with their prefix keymaps, and another asserts the help panel lists every main-map command.

### The `Ctrl-K` and `Ctrl-O` prefixes

Commands that do not fit on a single chord live behind `Ctrl-K`; on-screen formatting lives behind `Ctrl-O`. `OrdetApp::handle_prefix_key` resolves both prefixes before consulting the main keymap, so neither leader chord reaches the document as literal text. `PrefixMode` in `UiState` holds the pending prefix, and `commands::PREFIX_COMMANDS` / `commands::O_PREFIX_COMMANDS` are the single tables behind the keymaps and the help panel.

The help panel doubles as a lookup table: with it open, a command key scrolls to the section that documents that command (`components::help_jump_target`) instead of running it. Panel navigators keep scrolling, and commands without a help entry fall through to the normal dispatch.

### Mouse

`Terminal::enter` enables `EnableMouseCapture` (and `restore` disables it). `handle_mouse_event` ignores input while a prompt, browser, quit guard, or the welcome screen owns the screen. Otherwise left-press focuses the window under the pointer and places the cursor, left-drag extends the selection from the press point (`mouse_down_cursor`), and the wheel scrolls whichever pane is under the pointer. Click hit-testing goes through `visual_cursor_for_column`, so wrapped lines, alignment padding, and the gutter all agree with the renderer.

## 6. Async tasks

`TaskPool<Message>` in `crates/ordet-tui-framework/src/tasks.rs` provides:

- background task spawning
- keyed task replacement
- cancellation tokens
- error propagation back to the main loop

The app runs every file operation through the pool so input and rendering stay responsive: saving, trashing, opening, writing a block to disk, reading a file into the cursor, and persisting the revision history. Each carries a `TaskKey`, so starting a new operation of the same kind cancels the one still in flight. Saves, history writes, and recovery snapshots also pass through per-resource write gates: a stale generation is skipped, and the newest write wins.

A task that finishes after its draft moved on is discarded rather than applied: `AppMessage::ReadFileCompleted` checks both the draft index and the revision before touching the buffer.

## 7. Document buffer

The word processor uses `ropey` behind the thin `EditorBuffer` wrapper in `crates/ordet/src/editor_buffer.rs`. Wrapping geometry lives in `editor_buffer/layout.rs`, rich-text parsing and attribute helpers in `editor_buffer/formatting.rs`, and the tests and benchmarks are separate modules.

That wrapper is responsible for:

- insert/delete
- grapheme-aware cursor movement
- selection tracking
- change-based undo/redo
- incremental wrapped-line layout for rendering
- regex/whole-word/case-sensitive search and replace
- paragraph alignment, which survives a round trip through `.ord`

### Eager wrap cache

Rendering asks for the layout every frame, so recomputing it per keystroke would cost a full re-wrap each time. `WrapCache` memoises one `CachedWrapLine` per source line behind a `RefCell` (render only gets `&self`). The maintenance is eager rather than validated: `rope_insert` and `rope_remove`, the only two ways text enters or leaves the rope, splice the entry list, mark the edited lines dirty, and record a lazy integer shift on untouched lines, folded into their stored offsets when they are cloned out for output. `ensure_wrap_cache` then re-measures only dirty entries and never compares text. Undo and redo apply inverse ops through the same two helpers, so the cache stays correct for free; alignment-only changes mark their range dirty explicitly.

`layout()` still exists but only the reference tests use it. Production paths use `visible_layout` (visible slice plus margin), `visual_line_for_cursor`, `total_visual_lines`, and one-line helpers (`visual_line_start/end`, `visual_column_for_cursor`, `visual_cursor_for_column`) that clone at most a single wrapped line. A parity test asserts every movement command agrees with the full layout exactly.

### Change-based undo

Undo records hold inverse operations (`Insert { at, text }` / `Remove { start, end }`), cursor state, and a snapshot of the paragraph alignments (one byte per line, negligible next to the text). Typing a character logs a few offsets; only deletions carry text, bounded by the deleted span. Every public mutation opens a record with `begin_undo`, rope helpers log into it, and no-op discoveries drop it with `cancel_undo`. Undo opens a fresh record, applies inverses in reverse through the logging helpers (turning it into a redo record), then restores cursor state and alignments. A round-trip test drives every mutation through undo and redo and checks text, HTML, cursor, and cache agreement.

### Search

`SearchOptions { regex, whole_word, case_sensitive }` compiles the query to one `regex::Regex` (escaped unless regex, `(?i)` unless case-sensitive, `\b` bounded when whole-word). Matches map byte offsets back through `rope.byte_to_char`; zero-width matches are skipped. Replace-all expands `$1` captures per match in regex mode and stays literal otherwise. The find/replace prompts flip flags live with `Alt-R`/`Alt-W`/`Alt-C`, and an invalid pattern reopens the prompt with the error instead of searching.

## 8. Clipboard

`crates/ordet/src/clipboard.rs` pairs an internal kill ring (8 deep, duplicate-coalesced) with the system clipboard. Copy and cut push onto the ring and write through two ways: an OSC 52 sequence queued in `UiState.pending_overlays` and flushed after the frame through `Terminal::overlay_sequence` (works over SSH), plus the native clipboard (`arboard` on Windows; `pbcopy` on macOS; `wl-copy`/`xclip`/`xsel` on Linux). Paste reads the native clipboard first so outside copies win, falling back to the ring. Copy with no selection copies the current line; cut deletes through the same primitives as keyboard deletion, so both stay undoable.

## 9. Workspaces and history

`workspace.rs` scans registered folders for `.ord`/`.txt`/`.md` (one sublevel deep, hidden and build dirs skipped, capped) into `WorkspaceFile { path, relative }`. `WorkspaceScanCache` memoises scans behind the newest directory mtime plus a 2-second max age, so a frame never touches the filesystem. The Workspace panel shows per-folder unsaved/total counts; the Pages panel numbers the open drafts, the same order the header's `Pg n/m` reads, the window list follows, and `Alt-Up`/`Alt-Down` reorder (hidden scratch drafts are skipped, never pages). Folders persist in settings; renames invalidate the cache.

`history.rs` snapshots saved text, paragraph alignment, and inline styles per file (20 deep, 200KB cap, identical consecutive saves coalesced) into `history.json` next to settings, written atomically off the main loop. The Revisions panel lists snapshots newest-first with word deltas, diffs the selection against the buffer with a prefix/suffix-trimmed line diff, and restores through `replace_with_rich_text` so one undo returns. Older plain-text snapshots still load.

## 10. Configuration

`settings.rs` holds one JSON file (`AppSettings`, every field `#[serde(default)]` so old files keep parsing). A corrupt file heals itself: it is shelved as `settings.json.corrupt-<timestamp>` and the app starts from defaults with a status note. The CLI (`--config`, `--print-config`, `--reset-config`, `--help`, `--version`, `--` separator) is parsed in `app/cli.rs` before the terminal is touched. The Settings panel (`F2`) edits theme, accent, language, line numbers, the ruler, the menu bar, word target, autosave, and backups.

## 11. `.ord` fidelity

`OrdetDocument` round-trips title, status, title mode, label color, aligned HTML, and plain text. The app carries `DocumentMetadata` from load (`load_buffer_from_path`, `OpenCompleted`, startup args) into the draft and writes it back on save instead of resetting it, and `Ctrl-Shift-S` edits the status (marking the draft dirty).

## Main loop

`crates/ordet/src/app.rs` owns the runtime loop and `OrdetApp`. Input, documents, prompts, settings pickers, windows, helpers, and the CLI live in `crates/ordet/src/app/`.

1. draw the frame if a redraw is pending; a frame is never delayed, only
   coalesced with the events that arrived while the last one was painted
2. compute the next deadlines (housekeeping tick, autosave, recovery) and
   service the due ones
3. block in `event::poll` until input, a resize, or the next deadline
4. drain the queued events, translate them into commands or raw key input, and
   route them to the focused component tree
5. apply emitted actions and focus changes, then loop

Terminal size is read from `Event::Resize` and cached; there is no frame-rate
goal and no tick-driven redraw, so an idle terminal does no work at all.

## Prototype scope

The word processor UI covers:

- tiled windows (up to four pages on screen) with a bare menu bar, a ruler row, one status row, and modal panels
- focus switching between windows by keyboard and mouse
- help lookup (`F1` then a key) and the `Ctrl-O` alignment strokes
- rope-backed text editing with change-based undo
- regex/whole-word/case-sensitive find and replace, go-to-line, and marked blocks
- internal plus system clipboard
- workspace folders with unsaved tracking, and per-save revision history with restore
- background save, open, trash, and block file I/O
- double-buffered, fully event-driven rendering (no frame timer), with no as-you-type rewriting of the buffer: text changes only through explicit commands
- a quit guard that refuses to discard unsaved drafts silently

## Tradeoffs

- Untouched lines carry a lazy integer shift rather than eagerly rewritten offsets: an edit costs what it touched, at the price that reading a cached line must fold its pending shift in first.
- The revision history stores full text per save rather than deltas; the per-file cap and size limit bound it.
- History and recovery snapshots keep text, paragraph alignment, inline styles, and document metadata; older plain-text snapshots still load.
- Background tasks use threads and channels instead of a larger async runtime. That keeps the core small and portable.
- Mouse clicks behind modals and on the welcome screen are ignored rather than handled.
