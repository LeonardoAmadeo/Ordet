# ordet-tui-framework

A small, explicit, event-driven terminal UI framework in Rust, originally built to power [Ordet](../../README.md).

Inspired by GPUI's clean separation of state, layout, rendering, focus, and background work, `ordet-tui-framework` avoids retained widget trees and heavy abstractions in favor of direct, predictable primitives.

## Highlights

- **Coalescing double-buffered renderer (`Terminal`)**: Diffs front and back `Buffer`s with zero per-frame allocations, coalesces adjacent cursor moves and identical style runs, brackets dirty repaints with Synchronized Output (`DEC 2026`), and skips empty/idle frames completely (0 bytes emitted).
- **Unicode-safe cell grid (`Buffer`)**: Grapheme-cluster aware with interned `Symbol`s (table-indexed ASCII fast path) and automatic wide-character continuation cleanup when cells are overwritten.
- **Headless testing out of the box**: `Terminal::with_writer(Vec::<u8>::new(), area)` lets you render, diff, and assert ANSI output deterministically without a TTY.
- **Explicit layout (`split`)**: Horizontal and vertical splits with `Length`, `Min`, `Max`, `Percentage`, and proportional `Fill` constraints.
- **Typed input & focus (`Keymap`, `FocusState`, `Component`)**: Conflict-checked key chords, explicit focus rings, and pure-ish `Component::render` + `EventCx` action emission.
- **Panic-isolated background work (`TaskPool`)**: Persistent worker threads with keyed task replacement, cooperative `CancelToken`s, and panic catching.

## Example

Run the included interactive counter example:

```sh
cargo run -p ordet-tui-framework --example counter
```

## License

Dual-licensed under Apache-2.0 or MIT at your option.
