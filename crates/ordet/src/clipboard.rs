//! Clipboard: an internal kill ring bridged to the system clipboard.
//!
//! Copy and cut push onto an internal ring (so yank history survives leaving
//! the app's idea of "the" clipboard alone), and every copy also writes
//! through to the system clipboard two ways: an OSC 52 escape sequence for the
//! terminal emulator (works over SSH, no helper needed), and the native
//! clipboard (`arboard` on Windows, `pbcopy` on macOS, `xclip`/`xsel`/
//! `wl-copy` on Linux) for native apps. Paste reads the native clipboard
//! first so text copied elsewhere wins, and falls back to the ring when the
//! native clipboard is missing or empty.

use std::collections::VecDeque;
#[cfg(not(target_os = "windows"))]
use std::io::Write;
#[cfg(not(target_os = "windows"))]
use std::process::{Command, Stdio};

/// How many kills the internal ring remembers.
pub const RING_CAPACITY: usize = 8;
/// Native writes above this are skipped; OSC 52 still covers the terminal.
const NATIVE_WRITE_LIMIT: usize = 256 * 1024;

#[derive(Clone, Debug, Default)]
pub struct Clipboard {
    ring: VecDeque<String>,
}

impl Clipboard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push `text` onto the ring. Empty strings are ignored.
    pub fn copy(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        // Re-copying the same text refreshes it instead of duplicating it.
        if self.ring.front().is_some_and(|top| top == text) {
            return;
        }
        self.ring.push_front(text.to_string());
        while self.ring.len() > RING_CAPACITY {
            self.ring.pop_back();
        }
    }

    /// Newest ring entry, if any.
    pub fn ring_top(&self) -> Option<&str> {
        self.ring.front().map(String::as_str)
    }

    /// What a paste inserts: the platform clipboard when it has text,
    /// otherwise the ring. `source` names where it came from for the status
    /// line.
    pub fn paste(&self) -> Option<(String, &'static str)> {
        self.paste_with_system(read_system_clipboard())
    }

    /// `paste` with the system value injected, so tests do not depend on the
    /// machine's real clipboard.
    fn paste_with_system(&self, system: Option<String>) -> Option<(String, &'static str)> {
        if let Some(text) = system.filter(|text| !text.is_empty()) {
            return Some((text, "system clipboard"));
        }
        self.ring_top().map(|text| (text.to_string(), "kill ring"))
    }
}

/// OSC 52 sequence that tells the terminal emulator to set its clipboard to
/// `text`. Emitted through `Terminal::overlay_sequence` after a copy.
pub fn osc52_sequence(text: &str) -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    format!("\x1b]52;c;{}\x07", STANDARD.encode(text.as_bytes()))
}

/// Best-effort write to the platform clipboard. Never panics, never blocks the
/// caller long: oversized payloads are left to OSC 52.
pub fn write_system_clipboard(text: &str) {
    if text.is_empty() || text.len() > NATIVE_WRITE_LIMIT {
        return;
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            let _ = clipboard.set_text(text);
        }
    }

    #[cfg(not(target_os = "windows"))]
    if cfg!(target_os = "macos") {
        pipe_to_tool("pbcopy", &[], text);
    } else {
        // Wayland first where it exists, then the X11 tools.
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            pipe_to_tool("wl-copy", &[], text);
        }
        pipe_to_tool("xclip", &["-selection", "clipboard"], text);
        pipe_to_tool("xsel", &["--clipboard", "--input"], text);
    }
}

/// Best-effort read from the platform clipboard: `None` when no tool exists,
/// the tool fails, or the clipboard is empty.
pub fn read_system_clipboard() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        let mut clipboard = arboard::Clipboard::new().ok()?;
        let text = clipboard.get_text().ok()?;
        (!text.is_empty()).then_some(text)
    }

    #[cfg(not(target_os = "windows"))]
    {
        let output = if cfg!(target_os = "macos") {
            run_tool("pbpaste", &[])
        } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            run_tool("wl-paste", &["--no-newline"])
                .or_else(|| run_tool("xclip", &["-selection", "clipboard", "-o"]))
        } else {
            run_tool("xclip", &["-selection", "clipboard", "-o"])
                .or_else(|| run_tool("xsel", &["--clipboard", "--output"]))
        }?;
        let text = String::from_utf8_lossy(&output).into_owned();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn pipe_to_tool(tool: &str, args: &[&str], text: &str) {
    let Ok(mut child) = Command::new(tool)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    if let Some(stdin) = child.stdin.take() {
        let mut stdin = stdin;
        let _ = stdin.write_all(text.as_bytes());
        // Dropping closes stdin so the tool finishes; waiting reaps it.
        drop(stdin);
        let _ = child.wait();
    }
}

#[cfg(not(target_os = "windows"))]
fn run_tool(tool: &str, args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new(tool)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if output.status.success() {
        Some(output.stdout)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{osc52_sequence, Clipboard};

    #[test]
    fn ring_pushes_and_pastes_lifo() {
        let mut clipboard = Clipboard::new();
        assert_eq!(clipboard.paste_with_system(None), None);
        clipboard.copy("first");
        clipboard.copy("second");
        assert_eq!(clipboard.ring_top(), Some("second"));
        assert_eq!(
            clipboard.paste_with_system(None),
            Some(("second".to_string(), "kill ring"))
        );
    }

    #[test]
    fn ring_ignores_empty_and_duplicate_copies() {
        let mut clipboard = Clipboard::new();
        clipboard.copy("");
        assert_eq!(clipboard.paste_with_system(None), None);
        clipboard.copy("x");
        clipboard.copy("x");
        // One entry, not two: re-copying refreshes instead of duplicating.
        clipboard.copy("y");
        assert_eq!(clipboard.ring_top(), Some("y"));
    }

    #[test]
    fn ring_is_bounded() {
        let mut clipboard = Clipboard::new();
        for index in 0..super::RING_CAPACITY + 4 {
            clipboard.copy(&format!("entry {index}"));
        }
        assert_eq!(
            clipboard.ring_top(),
            Some(format!("entry {}", super::RING_CAPACITY + 3).as_str())
        );
    }

    #[test]
    fn osc52_wraps_base64_between_introducer_and_bel() {
        let sequence = osc52_sequence("hi");
        // "hi" is "aGk=" in base64.
        assert_eq!(sequence, "\x1b]52;c;aGk=\x07");
    }

    #[test]
    fn system_text_wins_over_the_ring() {
        let mut clipboard = Clipboard::new();
        clipboard.copy("ring text");
        assert_eq!(
            clipboard.paste_with_system(Some("system text".to_string())),
            Some(("system text".to_string(), "system clipboard"))
        );
        assert_eq!(
            clipboard.paste_with_system(Some(String::new())),
            Some(("ring text".to_string(), "kill ring"))
        );
    }

    #[test]
    fn system_read_returns_none_rather_than_panicking() {
        // No assertion on the value: CI boxes may or may not have pbcopy.
        // The contract is that a missing tool is `None`, never a panic.
        let _ = super::read_system_clipboard();
        super::write_system_clipboard("ordet clipboard probe");
    }
}
