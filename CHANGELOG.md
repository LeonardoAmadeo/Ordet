# Changelog

All notable changes to Ordet will be documented in this file.

## [0.1.0] - 2026-10-20

### Initial Release

Ordet is a distraction-free, terminal-first word processor built in Rust.

#### Highlights

- **Keystroke fidelity:** every keystroke inserts exactly what was typed, with
  no background rewriting and no unsolicited changes.
- **Classic command chords:** `Ctrl-K` and `Ctrl-O` prefixes sit alongside
  modern arrow-key movement and selection.
- **Tiled windows:** up to four pages on screen, each with its own title row
  and margin ruler.
- **Retro palettes:** built-in themes, including the Amber phosphor preset
  (`#AA5500`) and a monochrome paper preset.
- **Focused panels:** content-sized cards for Help, Notes, Word Stats,
  Settings, Revisions, Pages, and Workspace.
- **Local and private:** no network requests; drafts stay on the machine.

#### Platforms

- macOS: primary development platform, fully tested.
- Linux: Ubuntu CI build and test; interactive X11/Wayland smoke testing is
  pending.
- Windows: experimental, with a native CI build and test; interactive Windows
  Terminal smoke testing is pending.

#### Notes

- Keep backups during beta testing. Autosave, crash recovery, Trash, and
  revision restore are designed to protect work.
- Not yet implemented: printing, mail merge, and macros.
- Interface translations are available for English, Indonesian, Spanish, and
  French.
