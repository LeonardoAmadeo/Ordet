# Ordet

**A fast, distraction-free terminal word processor written in Rust — built on its own GPUI-inspired TUI framework.**

Ordet is local-first, offline-only, and engineered for sub-millisecond keystroke latency on large documents. Rather than wrapping `ratatui`, it runs on [`ordet-tui-framework`](crates/ordet-tui-framework/README.md): a custom, event-driven terminal engine with coalescing ANSI diff rendering, explicit layout, conflict-checked keymaps, and panic-isolated background tasks.

![Ordet terminal word processor demo](docs/ordet-loop.gif)

---

## Why Ordet?

- **Built for prose, not just code:** Paragraph alignment, character styles, a live margin ruler, word/character counts, and word target goals.
- **Fast on large files:** Rope-backed editing (`ropey`), incremental wrap/stats caching, and zero-allocation frame diffing keep typing under ~0.15 ms/frame even at 50,000 lines.
- **Split-screen writing:** Tile up to four documents side by side in a responsive grid, each with its own ruler and title bar.
- **Zero data loss:** Background autosave, crash recovery snapshots, revision history with diff/restore, and atomic file writes.
- **Cross-platform:** Native builds and CI verification on macOS, Linux, and Windows (including WSL clipboard bridging and OSC 52 over SSH).

---

## Features

- **Writing Canvas:** Paragraph and inline styles, margin ruler, live word/character counts, and word count targets.
- **Tiled Windows:** Up to four split panes (`Alt-N` to split, `F6` to cycle, `Alt-W` to close).
- **Classic Chords & Modern Keys:** `Ctrl-K` and `Ctrl-O` prefix chords alongside standard arrow/selection navigation.
- **Search & Replace:** Plain, regex (`Alt-R`), whole-word (`Alt-W`), and case-sensitive (`Alt-C`) modes.
- **Block Operations:** Mark regions to copy, move, delete, or export directly to another file.
- **Themes:** Curated dark, light, and system-adaptive palettes (*Midnight*, *Stalker*, *Persona*, *In the Mood for Love*, *Amber*, and more).
- **System Clipboard:** Native OS clipboard integration (`pbcopy`/`pbpaste`, `wl-copy`/`xclip`/`xsel`, `arboard`, WSL `clip.exe`) plus OSC 52 for SSH sessions.
- **Open Formats:** Saves as `.ord` or plain text / Markdown.

---

## Installation

### Requirements

- Stable [Rust toolchain](https://rustup.rs/)
- A UTF-8 terminal emulator (truecolor recommended)
- *Optional (Linux):* `wl-copy` / `wl-paste` (Wayland) or `xclip` / `xsel` (X11) for native clipboard sync

### From crates.io

```sh
cargo install ordet
```

### From source

```sh
git clone https://github.com/LeonardoAmadeo/ordet.git
cd ordet
cargo install --path crates/ordet --locked
```

Ensure `~/.cargo/bin` is in your `PATH`.

---

## Usage

```sh
ordet                        # Open with a blank draft
ordet draft.ord notes.txt    # Open one or more files
```

| Flag              | Description                                   |
| :---------------- | :-------------------------------------------- |
| `--config <path>` | Use a custom settings file                    |
| `--print-config`  | Print effective settings as JSON and exit     |
| `--reset-config`  | Reset settings to defaults and exit           |
| `--latency-log`   | Print keystroke latency percentiles on exit   |
| `-h`, `--help`    | Show CLI help                                 |
| `-V`, `--version` | Show version                                  |

---

## Essential Shortcuts

Press `F1` inside Ordet for the full interactive shortcut guide.

| Shortcut                       | Action                               |
| :----------------------------- | :----------------------------------- |
| `F1`                           | Interactive shortcut guide           |
| `F2`                           | Settings (themes, preferences)       |
| `F6`                           | Cycle split windows / pages          |
| `F10`                          | Top menu bar                         |
| `Ctrl-S` / `Ctrl-Shift-S`      | Save / Save As                       |
| `Ctrl-F`                       | Find                                 |
| `Ctrl-Z` / `Ctrl-Shift-Z`      | Undo / Redo                          |
| `Ctrl-X` / `Ctrl-C` / `Ctrl-V` | Cut / Copy / Paste                   |
| `Alt-N` / `Alt-W`              | Open new split window / Close window |
| `Ctrl-Q`                       | Quit                                 |
| `Esc`                          | Dismiss topmost panel or modal       |

**Prefix chords:**
- `Ctrl-K`: block operations, bookmarks, save & close (`Ctrl-K H`), and file commands.
- `Ctrl-O`: paragraph alignment — left (`Ctrl-O L`), center (`Ctrl-O C`), right (`Ctrl-O R`), justify (`Ctrl-O J`).

---

## Workspace & `ordet-tui-framework`

This repository is organized as a Cargo workspace:

| Crate | Purpose |
| :---- | :------ |
| [`crates/ordet`](crates/ordet) | The Ordet word processor binary |
| [`crates/ordet-tui-framework`](crates/ordet-tui-framework) | Standalone, GPUI-inspired TUI engine (coalescing ANSI renderer, Unicode buffer, layout, keymaps, focus, task pool) |
| [`crates/ordet-format`](crates/ordet-format) | `.ord` document parser, serializer, and atomic file writer |

You can try the framework standalone with its included example:

```sh
cargo run -p ordet-tui-framework --example counter
```

---

## Configuration

Settings are stored as JSON and editable in-app via `F2`:

| OS          | Default Path                                                                          |
| :---------- | :------------------------------------------------------------------------------------ |
| **macOS**   | `~/Library/Application Support/ordet-tui/settings.json`                               |
| **Linux**   | `$XDG_CONFIG_HOME/ordet-tui/settings.json` *(or `~/.config/ordet-tui/settings.json`)* |
| **Windows** | `%APPDATA%\ordet-tui\settings.json`                                                   |

Crash recovery (`recovery.json`) and revision history (`history.json`) live in the same directory.

---

## Platform Support

| Platform    | Status                                                                                                             |
| :---------- | :----------------------------------------------------------------------------------------------------------------- |
| **macOS**   | Primary development platform; full native clipboard, dark mode, and Trash support.                                 |
| **Linux**   | CI-verified with PTY smoke tests; Wayland/X11/WSL clipboard, GNOME/KDE/GTK/`COLORFGBG` dark mode, FreeDesktop Trash. |
| **Windows** | CI-verified with ConPTY smoke tests; `arboard` clipboard, Registry dark mode, retry-hardened atomic saves. Best in Windows Terminal or WezTerm. |

---

## License

Dual-licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.
