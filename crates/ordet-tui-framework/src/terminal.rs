use std::io::{self, Stdout, Write};

use crossterm::cursor::{Hide, MoveTo, SetCursorStyle, Show};
use crossterm::event::{EnableBracketedPaste, EnableMouseCapture};
use crossterm::queue;
use crossterm::style::{Attribute, Print, SetAttribute, SetBackgroundColor, SetForegroundColor};
use crossterm::terminal::{
    self, disable_raw_mode, enable_raw_mode, BeginSynchronizedUpdate, EndSynchronizedUpdate,
    EnterAlternateScreen,
};

use crate::{Buffer, Color, Frame, Modifier, Point, Rect, Style};

pub struct Terminal {
    stdout: Stdout,
    current: Buffer,
    previous: Buffer,
    /// Cached terminal size. `Terminal::size` serves reads (layout, resize
    /// handling) without an ioctl per frame; `draw` refreshes it so a size
    /// change the app never saw as an event still lands correctly.
    area: Rect,
    cursor: Option<Point>,
    cursor_visible: bool,
    /// Last caret position actually sent. Idle frames resend nothing, so
    /// the terminal's blink cycle is never poked and the cursor blinks
    /// instead of sitting static.
    sent_cursor: Option<Point>,
    restored: bool,
}

impl Terminal {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let (stdout, area) = match Self::enter_screen() {
            Ok(screen) => screen,
            Err(error) => {
                // A failed sequence must not leave the terminal raw, in the
                // alternate screen, or holding mouse capture.
                Self::emergency_restore();
                return Err(error);
            }
        };
        Ok(Self {
            stdout,
            current: Buffer::new(area),
            previous: Buffer::new(area),
            area,
            cursor: None,
            cursor_visible: false,
            sent_cursor: None,
            restored: false,
        })
    }

    fn enter_screen() -> io::Result<(Stdout, Rect)> {
        let mut stdout = io::stdout();
        crossterm::execute!(
            stdout,
            EnterAlternateScreen,
            SetCursorStyle::BlinkingBar,
            EnableBracketedPaste,
            EnableMouseCapture,
            Hide
        )?;
        // Any-event mouse tracking (1003) on top of crossterm's capture, so
        // passive motion arrives as Moved events and dropdowns can trace the
        // cursor. Terminals without it ignore the sequence and everything
        // keeps working with clicks. Paired with the 1003 reset below.
        queue!(stdout, Print("\x1b[?1003h"))?;
        // Disambiguate escape codes (kitty keyboard bit 1) so modified
        // keys arrive distinctly: Shift+Enter otherwise looks exactly
        // like Enter. Written directly because crossterm's Windows
        // implementation rejects the command even when the terminal
        // understands the sequence. Terminals without support ignore the
        // request and everything keeps working with legacy encodings.
        // Paired with the pop in `restore_sequences`.
        queue!(stdout, Print("\x1b[>1u"))?;
        let area = terminal_area()?;
        Ok((stdout, area))
    }

    pub fn restore(&mut self) -> io::Result<()> {
        if self.restored {
            return Ok(());
        }

        // Attempt both halves even if one fails: a broken raw-mode call must
        // not leave the user inside the alternate screen.
        let raw_result = disable_raw_mode();
        let screen_result = restore_sequences(&mut self.stdout);
        self.restored = true;
        screen_result?;
        raw_result?;
        Ok(())
    }

    /// Best-effort restoration for panic and signal paths, where there is no
    /// `Terminal` borrow to use. This writes one fixed ANSI sequence rather
    /// than going through `execute!`, so a single failed command cannot skip
    /// the rest of the cleanup. Ignore errors: a dying terminal is allowed to
    /// be gone, but if it is present it must get its cursor and screen back.
    pub fn emergency_restore() {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = stdout.write_all(RESTORE_SEQUENCES);
        let _ = stdout.flush();
    }

    pub fn size(&self) -> io::Result<Rect> {
        Ok(self.area)
    }

    /// Record a terminal size the event loop learned from a resize event, so
    /// the cached size serves the next frame without an ioctl.
    pub fn update_size(&mut self, area: Rect) {
        self.area = area;
    }

    pub fn draw<F>(&mut self, render: F) -> io::Result<()>
    where
        F: FnOnce(&mut Frame<'_>),
    {
        // Synchronized output: the terminal holds everything until the end
        // marker and flips at once, so a full repaint (theme switch, resize)
        // never shows half old theme and half new. Terminals without support
        // ignore both markers and behave exactly as before. The end marker
        // is best-effort so a mid-frame error can never leave sync mode
        // stuck on; the flush below still gates on success.
        queue!(self.stdout, BeginSynchronizedUpdate)?;
        let result = self.draw_frame(render);
        let _ = queue!(self.stdout, EndSynchronizedUpdate);
        if result.is_ok() {
            self.stdout.flush()?;
        }
        result.map(|_| ())
    }

    /// Paints one frame into the grid and queues its cells. No flushing:
    /// the caller brackets with the sync markers and flushes once.
    /// Returns whether anything was queued.
    fn draw_frame<F>(&mut self, render: F) -> io::Result<bool>
    where
        F: FnOnce(&mut Frame<'_>),
    {
        // The size comes from `Event::Resize` (`update_size`) and the cached
        // `area`; no ioctl per frame. A terminal that resizes without
        // delivering the event would be caught by the next event anyway.
        let area = self.area;
        if area != self.current.area() {
            self.current.resize(area);
            self.previous.resize(area);
            // New grid, unknown caret spot: reposition unconditionally next.
            self.sent_cursor = None;
        }

        self.current.clear(Style::default());
        let mut frame = Frame::new(area, &mut self.current);
        render(&mut frame);
        self.cursor = frame.cursor();

        let mut dirty = false;
        let mut paint_error: Option<io::Error> = None;
        self.current.diff_for_each(&self.previous, |point, cell| {
            if cell.continuation || paint_error.is_some() {
                return;
            }
            // `visit` cannot return errors, so the first failure is parked and
            // re-raised after the walk; nothing else is painted.
            let result = (|| -> io::Result<()> {
                queue!(self.stdout, MoveTo(point.x, point.y))?;
                queue!(self.stdout, SetAttribute(Attribute::Reset))?;
                queue!(
                    self.stdout,
                    SetForegroundColor(to_crossterm_color(cell.style.fg.unwrap_or(Color::Reset))),
                    SetBackgroundColor(to_crossterm_color(cell.style.bg.unwrap_or(Color::Reset)))
                )?;
                apply_modifiers(&mut self.stdout, cell.style.modifier)?;
                queue!(self.stdout, Print(cell.symbol.as_str()))
            })();
            match result {
                Ok(()) => dirty = true,
                Err(error) => paint_error = Some(error),
            }
        });
        if let Some(error) = paint_error {
            return Err(error);
        }

        // `Print` parks the hardware caret on the last cell written. A
        // status-only repaint (the "Opened …" line expiring) does not move
        // the logical caret, so it has to be put back anyway.
        dirty |= self.place_cursor(dirty)?;
        if dirty {
            queue!(self.stdout, SetAttribute(Attribute::Reset))?;
            std::mem::swap(&mut self.current, &mut self.previous);
        }
        Ok(dirty)
    }

    /// Shows, hides or moves the caret. Returns whether anything was queued.
    ///
    /// Idle frames resend nothing, so the blink cycle is never poked and
    /// the bar keeps blinking. `cells_painted` means this frame already
    /// moved the hardware caret onto the last cell written, so it is put
    /// back even when its position did not change.
    fn place_cursor(&mut self, cells_painted: bool) -> io::Result<bool> {
        match self.cursor {
            Some(point) => {
                let mut dirty = false;
                if !self.cursor_visible {
                    queue!(self.stdout, Show)?;
                    self.cursor_visible = true;
                    dirty = true;
                }
                if caret_needs_move(self.sent_cursor, point, cells_painted) {
                    queue!(self.stdout, MoveTo(point.x, point.y))?;
                    self.sent_cursor = Some(point);
                    dirty = true;
                }
                Ok(dirty)
            }
            None => {
                let was_visible = self.cursor_visible;
                self.cursor_visible = false;
                self.sent_cursor = None;
                if was_visible {
                    queue!(self.stdout, Hide)?;
                }
                Ok(was_visible)
            }
        }
    }

    pub fn overlay_sequence(&mut self, sequence: &str) -> io::Result<()> {
        self.stdout.write_all(sequence.as_bytes())?;

        // The sequence may have moved the hardware caret. Put it back.
        self.place_cursor(true)?;
        self.stdout.flush()
    }
}

/// Painting cells parks the hardware caret on the last cell written, so a
/// later frame must move it back even when the logical position is unchanged.
/// Idle frames (nothing painted, same position) skip the move so the blink
/// cycle is not reset.
fn caret_needs_move(sent: Option<Point>, desired: Point, cells_painted: bool) -> bool {
    cells_painted || sent != Some(desired)
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Fixed restoration bytes. Writing these directly avoids crossterm's
/// Windows console-mode calls, which fail on a detached console even though
/// the ANSI sequences themselves are still valid in Windows Terminal.
const RESTORE_SEQUENCES: &[u8] = b"\x1b[<1u\x1b[?25h\x1b[0 q\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1015l\x1b[?1006l\x1b[?2026l\x1b[?1049l";

fn restore_sequences<W: Write>(stdout: &mut W) -> io::Result<()> {
    stdout.write_all(RESTORE_SEQUENCES)?;
    stdout.flush()
}

fn terminal_area() -> io::Result<Rect> {
    let (width, height) = terminal::size()?;
    Ok(Rect::new(0, 0, width, height))
}

fn to_crossterm_color(color: Color) -> crossterm::style::Color {
    match color {
        Color::Reset => crossterm::style::Color::Reset,
        Color::Black => crossterm::style::Color::Black,
        Color::White => crossterm::style::Color::White,
        Color::Rgb(r, g, b) => crossterm::style::Color::Rgb { r, g, b },
    }
}

fn apply_modifiers(stdout: &mut Stdout, modifier: Modifier) -> io::Result<()> {
    if modifier.contains(Modifier::BOLD) {
        queue!(stdout, SetAttribute(Attribute::Bold))?;
    }
    if modifier.contains(Modifier::ITALIC) {
        queue!(stdout, SetAttribute(Attribute::Italic))?;
    }
    if modifier.contains(Modifier::UNDERLINED) {
        queue!(stdout, SetAttribute(Attribute::Underlined))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{caret_needs_move, restore_sequences};
    use crate::Point;

    #[test]
    fn restore_sequences_leave_alt_screen_mouse_capture_and_cursor_visible() {
        let mut output = Vec::new();
        restore_sequences(&mut output).expect("restore sequence");

        let text = String::from_utf8_lossy(&output);
        assert!(
            text.contains("\x1b[?1049l"),
            "leave alternate screen: {text:?}"
        );
        assert!(
            text.contains("\x1b[?1003l"),
            "disable all-motion mouse: {text:?}"
        );
        assert!(
            text.contains("\x1b[?1000l"),
            "disable mouse capture: {text:?}"
        );
        assert!(text.contains("\x1b[?25h"), "show cursor: {text:?}");
    }

    #[test]
    fn status_repaint_puts_the_caret_back_when_it_did_not_move() {
        let editor = Point::new(4, 12);
        // Same caret as last frame, but the status line was just repainted.
        assert!(caret_needs_move(Some(editor), editor, true));
    }

    #[test]
    fn idle_frame_leaves_the_blink_alone() {
        let editor = Point::new(4, 12);
        assert!(!caret_needs_move(Some(editor), editor, false));
    }

    #[test]
    fn a_moved_caret_is_sent_even_on_an_idle_grid() {
        let editor = Point::new(4, 12);
        assert!(caret_needs_move(Some(Point::new(3, 12)), editor, false));
        assert!(caret_needs_move(None, editor, false));
    }
}
