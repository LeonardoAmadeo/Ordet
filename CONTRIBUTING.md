# Contributing to Ordet

Ordet is a terminal-first word processor built in Rust. Small,
focused changes are easiest to review and safest to ship.

## Before opening a pull request

1. Explain the user-facing problem and keep unrelated formatting out of the
   change.
2. Add or update a regression test when behaviour changes.
3. Run the same checks enforced by CI:

   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace -- --test-threads=1
   ```

4. Test interactive terminal behaviour manually when your change touches input,
   focus, rendering, clipboard, or filesystem operations.

## Design rules

- Keep the UI event-driven. Request a redraw when visible state changes; do not
  add a polling render loop or a frame timer.
- Redraw immediately after handling an input batch, and never redraw an idle
  terminal.
- Keep blocking filesystem work off the input/render path.
- Preserve keyboard-first access and mouse parity for interactive controls.
- Prefer a named function or small type over duplicated control flow. Document
  non-obvious terminal workarounds where they live.

## Scope

Bug fixes, tests, documentation, and focused word processor improvements are welcome.
Large subsystems such as printing, mail merge, and macros should begin as a
design issue so their commands, data model, and test plan can be agreed before
implementation.
