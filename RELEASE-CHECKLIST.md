# Ordet beta release checklist

Run the automated gate from a clean checkout:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets -- --test-threads=1
cargo build --release -p ordet
```

Then run `target/release/ordet` in the terminal application you intend to
support and complete this manual smoke test:

1. Create a draft, type text, select text, undo and redo it.
2. Save as `.ord`, quit, reopen it, and verify the text and styles remain.
3. Make an unsaved edit, stop the process, relaunch, and verify recovery.
4. Open Revisions, inspect a diff, restore one, and undo the restore.
5. Open `F2` Settings, change and relaunch to verify a preference persists.
6. Press `F10`; test mouse and keyboard navigation, Escape, and a long menu
   in a short terminal.
7. Press `F1`; cycle the Movement, Writing, Workspace, Tools, and Blocks
   tabs with Tab and the arrow keys. Confirm actions align left, shortcuts
   align right, nothing is ellipsized, and the footer closes with Esc. Also
   open Help from the welcome screen and confirm its key/description table
   still aligns at the supported terminal width.
8. Test the exact terminal's clipboard and mouse wheel.
9. Send SIGINT (or terminate the process externally) and confirm the alternate
   screen, cursor, and mouse capture are restored.
10. Trigger a panic in a debug build and confirm the same terminal restoration.
11. Save over an existing file and confirm the old file remains complete until
    the atomic replacement succeeds.

Publish only the source tree plus the two license files. State that this is a
beta, list supported platforms/terminals actually smoke-tested, and link to
the issue tracker for data-loss or rendering reports.

Platform status for 0.1.0:

- macOS: local interactive smoke test.
- Linux: Ubuntu CI build/test; X11/Wayland interactive testing is tracked for
  the next release.
- Windows: native CI build/test and `.exe` launch; labeled experimental until
  an interactive Windows Terminal test and community feedback arrive.
