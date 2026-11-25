# Ordet

A terminal word processor written in Rust, built on its own TUI framework (`ordet-tui-framework`). Runs locally with no network requests.

![Ordet demo](docs/ordet-loop.gif)

## What it does

- Paragraph alignment, inline styles, margin ruler, word/character counts, and word targets
- Up to four tiled split windows (`Alt-N` to split, `F6` to cycle, `Alt-W` to close)
- `Ctrl-K` and `Ctrl-O` prefix chords alongside standard arrow-key editing
- Find and replace with plain, regex (`Alt-R`), whole-word (`Alt-W`), and case-sensitive (`Alt-C`) modes
- Block marking (copy, move, delete, or write a block to a file)
- Workspace folder browser and a side Notes panel
- Autosave, crash recovery snapshots, backups, and revision history
- Built-in color themes plus system dark/light mode detection
- System clipboard sync (`pbcopy`, `wl-copy`/`xclip`/`xsel`, `arboard`, WSL `clip.exe`) and OSC 52 over SSH
- Saves as `.ord`, plain text, or Markdown

## Install

Requires a stable [Rust toolchain](https://rustup.rs/) and a UTF-8 terminal. On Linux, install `wl-copy`/`wl-paste` (Wayland) or `xclip`/`xsel` (X11) if you want system clipboard support.

From crates.io:

```sh
cargo install ordet
```

From source:

```sh
git clone https://github.com/LeonardoAmadeo/Ordet.git
cd Ordet
cargo install --path crates/ordet --locked
```

Make sure `~/.cargo/bin` is in your `PATH`.

## Usage

```sh
ordet                        # Open a blank draft
ordet draft.ord notes.txt    # Open one or more files
```

Options:

| Flag | Effect |
| :--- | :--- |
| `--config <path>` | Use a specific settings file |
| `--print-config` | Print effective settings as JSON and exit |
| `--reset-config` | Overwrite settings with defaults and exit |
| `--latency-log` | Print keystroke latency percentiles to stderr on exit |
| `-h`, `--help` | Show help |
| `-V`, `--version` | Show version |

## Key shortcuts

Press `F1` inside Ordet for the full shortcut list.

| Key | Action |
| :--- | :--- |
| `F1` | Shortcut guide |
| `F2` | Settings |
| `F6` | Cycle windows / pages |
| `F10` | Menu bar |
| `Ctrl-S` / `Ctrl-Shift-S` | Save / Save As |
| `Ctrl-F` | Find |
| `Ctrl-Z` / `Ctrl-Shift-Z` | Undo / Redo |
| `Ctrl-X` / `Ctrl-C` / `Ctrl-V` | Cut / Copy / Paste |
| `Alt-N` / `Alt-W` | New split window / Close window |
| `Ctrl-Q` | Quit |
| `Esc` | Close active panel or modal |

Prefix chords:
- `Ctrl-K`: block operations, bookmarks, save and close (`Ctrl-K H`), file commands
- `Ctrl-O`: alignment — left (`Ctrl-O L`), center (`Ctrl-O C`), right (`Ctrl-O R`), justify (`Ctrl-O J`)

## Configuration

Settings are stored as JSON (editable in-app with `F2`):

- **macOS:** `~/Library/Application Support/ordet-tui/settings.json`
- **Linux:** `$XDG_CONFIG_HOME/ordet-tui/settings.json` (default `~/.config/ordet-tui/settings.json`)
- **Windows:** `%APPDATA%\ordet-tui\settings.json`

`recovery.json` and `history.json` are stored in the same directory.

## Workspace layout

- `crates/ordet` — the word processor binary
- `crates/ordet-tui-framework` — low-level TUI framework (ANSI diff renderer, buffer, layout, keymap, focus, task pool)
- `crates/ordet-format` — `.ord` file parser, serializer, and atomic file writes

## Platform notes

- **macOS:** Primary development platform.
- **Linux:** Built and tested in CI under a PTY. Supports Wayland/X11/WSL clipboards, GNOME/KDE/GTK/`COLORFGBG` theme detection, and FreeDesktop Trash.
- **Windows:** Built and tested in CI under ConPTY. Works best in Windows Terminal or WezTerm.

## License

Dual-licensed under [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
