//! Double-buffered, coalescing ANSI terminal renderer.
//!
//! [`Terminal`] diffs the current [`Buffer`] against the previous frame,
//! coalesces adjacent cursor moves and identical styles into a single buffered
//! byte slice, brackets dirty repaints with Synchronized Output (`DEC 2026`),
//! and flushes in one syscall.

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

/// Terminal renderer owning front/back [`Buffer`]s and an output writer.
///
/// Defaults to [`Stdout`] for live terminals, or accepts any [`Write`] via
/// [`Terminal::with_writer`] for headless rendering and deterministic tests.
pub struct Terminal<W: Write = Stdout> {
    writer: W,
    /// Reusable per-frame ANSI staging buffer so every dirty frame lands in a
    /// single `write_all` + `flush` syscall.
    out_buf: Vec<u8>,
    current: Buffer,
    previous: Buffer,
    /// Cached terminal size. `Terminal::size` serves reads (layout, resize
    /// handling) without an ioctl per frame; `update_size` refreshes it on
    /// resize events.
    area: Rect,
    cursor: Option<Point>,
    cursor_visible: bool,
    /// Last caret position actually sent. Idle frames resend nothing, so
    /// the terminal's blink cycle is never poked and the cursor blinks
    /// instead of sitting static.
    sent_cursor: Option<Point>,
    /// True when this terminal manages a real TTY raw-mode / alt-screen session.
    owns_tty: bool,
    restored: bool,
}

impl Terminal<Stdout> {
    /// Enters raw mode, alternate screen, mouse capture, bracketed paste, and
    /// Kitty keyboard disambiguation on [`Stdout`].
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
            writer: stdout,
            out_buf: Vec::with_capacity(8192),
            current: Buffer::new(area),
            previous: Buffer::new(area),
            area,
            cursor: None,
            cursor_visible: false,
            sent_cursor: None,
            owns_tty: true,
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
        stdout.flush()?;
        let area = terminal_area()?;
        Ok((stdout, area))
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
}

impl<W: Write> Terminal<W> {
    /// Creates a headless terminal backed by any [`Write`] sink (e.g. `Vec<u8>`)
    /// without touching TTY raw mode or alternate screen state.
    pub fn with_writer(writer: W, area: Rect) -> Self {
        Self {
            writer,
            out_buf: Vec::with_capacity(4096),
            current: Buffer::new(area),
            previous: Buffer::new(area),
            area,
            cursor: None,
            cursor_visible: false,
            sent_cursor: None,
            owns_tty: false,
            restored: true,
        }
    }

    pub fn writer(&self) -> &W {
        &self.writer
    }

    pub fn writer_mut(&mut self) -> &mut W {
        &mut self.writer
    }

    pub fn restore(&mut self) -> io::Result<()> {
        if !self.owns_tty || self.restored {
            return Ok(());
        }

        // Attempt both halves even if one fails: a broken raw-mode call must
        // not leave the user inside the alternate screen.
        let raw_result = disable_raw_mode();
        let screen_result = restore_sequences(&mut self.writer);
        self.restored = true;
        screen_result?;
        raw_result?;
        Ok(())
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
        self.out_buf.clear();
        let dirty = self.draw_frame(render)?;
        if !dirty {
            return Ok(());
        }
        // Synchronized output: the terminal holds everything until the end
        // marker and flips at once, so a full repaint (theme switch, resize)
        // never shows half old theme and half new. Terminals without support
        // ignore both markers and behave exactly as before.
        queue!(self.writer, BeginSynchronizedUpdate)?;
        let write_result = self.writer.write_all(&self.out_buf);
        let _ = queue!(self.writer, EndSynchronizedUpdate);
        write_result?;
        self.writer.flush()
    }

    /// Paints one frame into the grid and stages its minimal ANSI diff into
    /// `self.out_buf`. Returns whether anything changed.
    fn draw_frame<F>(&mut self, render: F) -> io::Result<bool>
    where
        F: FnOnce(&mut Frame<'_>),
    {
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

        let mut cells_painted = false;
        let mut last_pos: Option<(u16, u16)> = None;
        let mut last_style: Option<Style> = None;
        let mut paint_error: Option<io::Error> = None;
        let out_buf = &mut self.out_buf;

        self.current.diff_for_each(&self.previous, |point, cell| {
            if cell.continuation || paint_error.is_some() {
                return;
            }
            let result = (|| -> io::Result<()> {
                // Skip MoveTo when the hardware cursor is already at this cell.
                if last_pos != Some((point.x, point.y)) {
                    queue!(out_buf, MoveTo(point.x, point.y))?;
                }
                // Skip style escapes when consecutive cells share the same style.
                if last_style != Some(cell.style) {
                    queue!(out_buf, SetAttribute(Attribute::Reset))?;
                    queue!(
                        out_buf,
                        SetForegroundColor(to_crossterm_color(
                            cell.style.fg.unwrap_or(Color::Reset)
                        )),
                        SetBackgroundColor(to_crossterm_color(
                            cell.style.bg.unwrap_or(Color::Reset)
                        ))
                    )?;
                    apply_modifiers(out_buf, cell.style.modifier)?;
                    last_style = Some(cell.style);
                }
                queue!(out_buf, Print(cell.symbol.as_str()))?;
                let advance = cell.symbol.display_width();
                last_pos = Some((point.x.saturating_add(advance), point.y));
                Ok(())
            })();
            match result {
                Ok(()) => cells_painted = true,
                Err(error) => paint_error = Some(error),
            }
        });
        if let Some(error) = paint_error {
            return Err(error);
        }

        if cells_painted {
            queue!(self.out_buf, SetAttribute(Attribute::Reset))?;
        }

        // `Print` parks the hardware caret on the last cell written. A
        // status-only repaint (the "Opened …" line expiring) does not move
        // the logical caret, so it has to be put back anyway.
        let cursor_changed = self.place_cursor(cells_painted)?;
        let dirty = cells_painted || cursor_changed;
        if cells_painted {
            std::mem::swap(&mut self.current, &mut self.previous);
        }
        Ok(dirty)
    }

    /// Shows, hides or moves the caret into `self.out_buf`. Returns whether
    /// anything was queued.
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
                    queue!(self.out_buf, Show)?;
                    self.cursor_visible = true;
                    dirty = true;
                }
                if caret_needs_move(self.sent_cursor, point, cells_painted) {
                    queue!(self.out_buf, MoveTo(point.x, point.y))?;
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
                    queue!(self.out_buf, Hide)?;
                }
                Ok(was_visible)
            }
        }
    }

    pub fn overlay_sequence(&mut self, sequence: &str) -> io::Result<()> {
        self.writer.write_all(sequence.as_bytes())?;

        // The sequence may have moved the hardware caret. Put it back.
        self.out_buf.clear();
        if self.place_cursor(true)? {
            self.writer.write_all(&self.out_buf)?;
        }
        self.writer.flush()
    }
}

/// Painting cells parks the hardware caret on the last cell written, so a
/// later frame must move it back even when the logical position is unchanged.
/// Idle frames (nothing painted, same position) skip the move so the blink
/// cycle is not reset.
fn caret_needs_move(sent: Option<Point>, desired: Point, cells_painted: bool) -> bool {
    cells_painted || sent != Some(desired)
}

impl<W: Write> Drop for Terminal<W> {
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
        Color::Red => crossterm::style::Color::DarkRed,
        Color::Green => crossterm::style::Color::DarkGreen,
        Color::Yellow => crossterm::style::Color::DarkYellow,
        Color::Blue => crossterm::style::Color::DarkBlue,
        Color::Magenta => crossterm::style::Color::DarkMagenta,
        Color::Cyan => crossterm::style::Color::DarkCyan,
        Color::Gray => crossterm::style::Color::Grey,
        Color::DarkGray => crossterm::style::Color::DarkGrey,
        Color::LightRed => crossterm::style::Color::Red,
        Color::LightGreen => crossterm::style::Color::Green,
        Color::LightYellow => crossterm::style::Color::Yellow,
        Color::LightBlue => crossterm::style::Color::Blue,
        Color::LightMagenta => crossterm::style::Color::Magenta,
        Color::LightCyan => crossterm::style::Color::Cyan,
        Color::White => crossterm::style::Color::White,
        Color::Indexed(value) => crossterm::style::Color::AnsiValue(value),
        Color::Rgb(r, g, b) => crossterm::style::Color::Rgb { r, g, b },
    }
}

fn apply_modifiers<W: Write>(out: &mut W, modifier: Modifier) -> io::Result<()> {
    if modifier.contains(Modifier::BOLD) {
        queue!(out, SetAttribute(Attribute::Bold))?;
    }
    if modifier.contains(Modifier::DIM) {
        queue!(out, SetAttribute(Attribute::Dim))?;
    }
    if modifier.contains(Modifier::ITALIC) {
        queue!(out, SetAttribute(Attribute::Italic))?;
    }
    if modifier.contains(Modifier::UNDERLINED) {
        queue!(out, SetAttribute(Attribute::Underlined))?;
    }
    if modifier.contains(Modifier::REVERSED) {
        queue!(out, SetAttribute(Attribute::Reverse))?;
    }
    if modifier.contains(Modifier::CROSSED_OUT) {
        queue!(out, SetAttribute(Attribute::CrossedOut))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{caret_needs_move, restore_sequences, Terminal};
    use crate::{Alignment, Color, Line, Point, Rect, Style};

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

    #[test]
    fn adjacent_styled_cells_coalesce_cursor_moves_and_style_escapes() {
        let area = Rect::new(0, 0, 20, 2);
        let mut term = Terminal::with_writer(Vec::<u8>::new(), area);
        let style = Style::new().fg(Color::Rgb(120, 179, 255)).bold();

        term.draw(|frame| {
            frame.draw_line(area, 0, &Line::from("hello world"), style, Alignment::Left);
        })
        .expect("draw frame");

        let rendered = String::from_utf8_lossy(term.writer());
        // Contiguous run "hello world" should appear verbatim with no inter-char
        // cursor-move escapes between its letters.
        assert!(
            rendered.contains("hello world"),
            "expected contiguous run without redundant escapes, got: {rendered:?}"
        );
        // Exactly one MoveTo (CSI 1;1H) for the entire contiguous run.
        assert_eq!(rendered.matches("\x1b[1;1H").count(), 1);
    }

    #[test]
    fn idle_frame_emits_zero_bytes() {
        let area = Rect::new(0, 0, 10, 2);
        let mut term = Terminal::with_writer(Vec::<u8>::new(), area);

        term.draw(|frame| {
            frame.draw_line(
                area,
                0,
                &Line::from("hi"),
                Style::default(),
                Alignment::Left,
            );
            frame.set_cursor(Point::new(2, 0));
        })
        .expect("first draw");

        term.writer_mut().clear();
        term.draw(|frame| {
            frame.draw_line(
                area,
                0,
                &Line::from("hi"),
                Style::default(),
                Alignment::Left,
            );
            frame.set_cursor(Point::new(2, 0));
        })
        .expect("second identical draw");

        assert!(
            term.writer().is_empty(),
            "identical frame must emit zero bytes, got: {:?}",
            String::from_utf8_lossy(term.writer())
        );
    }
}
