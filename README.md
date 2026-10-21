# Ordet

> **A quiet terminal word processor.**  
> Built in Rust on its own TUI framework.

Ordet is a local-first, distraction-free word processor for the terminal. It is designed to get out of the way and let you write. It does a focused set of things well, gives you pure keystroke fidelity, and makes zero network requests.

The name comes from Kaj Munk's play and Carl Theodor Dreyer's film *Ordet* (Danish for *"The Word"*).

---

## Features

- **Writing Canvas:** Paragraph and character styles, visible margin ruler, live word/character counts, and word target goals.
- **Tiled Windows:** Up to four pages side by side in a split grid, each with its own ruler and centered title.
- **Classic Chords:** `Ctrl-K` and `Ctrl-O` prefix commands alongside standard modern arrow key navigation.
- **Search & Replace:** Plain, regex (`⌥R`), whole-word (`⌥W`), and case-sensitive (`⌥C`) modes.
- **Block Editing:** Mark regions to copy, move, delete, or write directly to a separate file.
- **Safety Net:** Autosave, crash recovery snapshots, revision history with diff/restore, and backups.
- **Film Themes:** Arthouse cinema-inspired presets (*Stalker*, *Persona*, *In the Mood for Love*, *Amber*, and more).
- **System Clipboard:** Native clipboard integration plus OSC 52 (copying works seamlessly over SSH).
- **Formats:** Saves as `.ord` or standard plain text / Markdown.

---

## Installation

### Requirements

- Stable [Rust toolchain](https://rustup.rs/)
- A UTF-8 terminal emulator
- *Optional (Linux):* `wl-copy` (Wayland) or `xclip` / `xsel` (X11) for the system clipboard bridge

### Build and Install

```sh
git clone https://github.com/LeonardoAmadeo/ordet.git
cd ordet
cargo install --path crates/ordet --locked
```

This installs the `ordet` binary into `~/.cargo/bin`. Ensure that directory is in your `$PATH`.

## Usage

```sh
ordet                        # Start with a blank draft
ordet draft.ord notes.txt    # Open one or more files
```

| Option            | Effect                                        |
| :---------------- | :-------------------------------------------- |
| `--config <path>` | Use this settings file instead of the default |
| `--print-config`  | Print effective settings as JSON and exit     |
| `--reset-config`  | Overwrite settings with defaults and exit     |
| `-h`, `--help`    | Show command-line help                        |
| `-V`, `--version` | Show version                                  |

## Key Shortcuts

Press `F1` inside Ordet for the complete shortcut guide. The essentials:

| Key                            | Action                               |
| :----------------------------- | :----------------------------------- |
| `F1`                           | Keyboard shortcut guide              |
| `F2`                           | Settings panel (themes, preferences) |
| `F6`                           | Cycle split windows (or pages)       |
| `F10`                          | Open top menu bar                    |
| `Ctrl-S`                       | Save                                 |
| `Ctrl-Shift-S`                 | Save As                              |
| `Ctrl-F`                       | Find                                 |
| `Ctrl-Z` / `Ctrl-Shift-Z`      | Undo / Redo                          |
| `Ctrl-X` / `Ctrl-C` / `Ctrl-V` | Cut / Copy / Paste                   |
| `Alt-N`                        | Open next page in a new split window |
| `Alt-W`                        | Close focused window                 |
| `Ctrl-Q`                       | Quit                                 |
| `Esc`                          | Close topmost panel or modal         |

## Command Prefixes

Ordet uses two command prefixes. Press the leader, then the command key:

- `Ctrl-K`: blocks, bookmarks, save and close (`Ctrl-K H`), and file operations.
- `Ctrl-O`: onscreen formatting: align left (`Ctrl-O L`), center (`Ctrl-O C`), right (`Ctrl-O R`), or justify (`Ctrl-O J`).

## Configuration

Preferences live in a single JSON file, editable in-app with `F2`:

| OS          | Default location                                                                      |
| :---------- | :------------------------------------------------------------------------------------ |
| **macOS**   | `~/Library/Application Support/ordet-tui/settings.json`                               |
| **Linux**   | `$XDG_CONFIG_HOME/ordet-tui/settings.json` *(or `~/.config/ordet-tui/settings.json`)* |
| **Windows** | `%APPDATA%\ordet-tui\settings.json`                                                   |

Crash recovery snapshots (`recovery.json`) and revision logs (`history.json`) are stored alongside your settings file.

## Platform Support

| Platform    | Status                                                                                           |
| :---------- | :----------------------------------------------------------------------------------------------- |
| **macOS**   | Primary development platform; fully tested.                                                      |
| **Linux**   | Ubuntu CI builds and tests pass. Interactive X11/Wayland testing welcome.                        |
| **Windows** | Native CI builds and tests `ordet.exe`. Works best in Windows Terminal or WezTerm with VT input. |

## Beta Status

Ordet is currently in beta (v0.1.0) and ready for focused prose, draft, and Markdown writing.

While autosave, crash recovery snapshots, and revision history are designed to protect your work, please maintain backups of critical drafts. If you encounter any rendering quirks or edge-case bugs, please open an issue with your terminal emulator name and dimensions.

## License

Dual-licensed under either of:

- Apache License, Version 2.0
- MIT License

at your option.
