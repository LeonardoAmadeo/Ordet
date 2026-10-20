use crate::{Alignment, Buffer, Line, Point, Rect, Style};

pub struct Frame<'a> {
    area: Rect,
    buffer: &'a mut Buffer,
    cursor: Option<Point>,
}

impl<'a> Frame<'a> {
    /// Borrow a buffer for drawing. Public so embedders and integration
    /// tests can render straight into an owned [`Buffer`].
    pub fn new(area: Rect, buffer: &'a mut Buffer) -> Self {
        Self {
            area,
            buffer,
            cursor: None,
        }
    }

    pub fn area(&self) -> Rect {
        self.area
    }

    pub fn set_cursor(&mut self, point: impl Into<Point>) {
        self.cursor = Some(point.into());
    }

    /// Drops a cursor set earlier this frame. Overlays that own the screen
    /// (menu dropdowns, pickers) call this last so the text cursor frozen
    /// behind them never blinks through.
    pub fn clear_cursor(&mut self) {
        self.cursor = None;
    }

    pub(crate) fn cursor(&self) -> Option<Point> {
        self.cursor
    }

    pub fn clear_rect(&mut self, area: Rect, style: Style) {
        self.buffer.fill_rect(area, style);
    }

    pub fn draw_border(
        &mut self,
        area: Rect,
        title: Option<&str>,
        fill_style: Style,
        border_style: Style,
    ) -> Rect {
        if area.width == 0 || area.height == 0 {
            return area;
        }

        self.clear_rect(area, fill_style);

        let left = area.x;
        let right = area.right().saturating_sub(1);
        let top = area.y;
        let bottom = area.bottom().saturating_sub(1);

        if area.width >= 2 {
            for x in left.saturating_add(1)..right {
                self.buffer
                    .write_text(Point::new(x, top), "─", border_style, 1);
                if area.height >= 2 {
                    self.buffer
                        .write_text(Point::new(x, bottom), "─", border_style, 1);
                }
            }
        }

        if area.height >= 2 {
            for y in top.saturating_add(1)..bottom {
                self.buffer
                    .write_text(Point::new(left, y), "│", border_style, 1);
                if area.width >= 2 {
                    self.buffer
                        .write_text(Point::new(right, y), "│", border_style, 1);
                }
            }
        }

        self.buffer
            .write_text(Point::new(left, top), "┌", border_style, 1);
        if area.width >= 2 {
            self.buffer
                .write_text(Point::new(right, top), "┐", border_style, 1);
        }
        if area.height >= 2 {
            self.buffer
                .write_text(Point::new(left, bottom), "└", border_style, 1);
            if area.width >= 2 {
                self.buffer
                    .write_text(Point::new(right, bottom), "┘", border_style, 1);
            }
        }

        if let Some(title) = title {
            let title = title.trim();
            if !title.is_empty() && area.width > 4 {
                let decorated = format!(" {title} ");
                let max_width = area.width.saturating_sub(4);
                self.buffer.write_text(
                    Point::new(left.saturating_add(2), top),
                    &decorated,
                    fill_style.patch(border_style),
                    max_width,
                );
            }
        }

        area.inset(1, 1)
    }

    pub fn draw_line(
        &mut self,
        area: Rect,
        row: u16,
        line: &Line,
        base_style: Style,
        alignment: Alignment,
    ) {
        if row >= area.height || area.width == 0 {
            return;
        }

        let y = area.y.saturating_add(row);
        let line_width = line.width().min(area.width as usize) as u16;
        let start_x = match alignment {
            Alignment::Left => area.x,
            Alignment::Center => area
                .x
                .saturating_add(area.width.saturating_sub(line_width) / 2),
            Alignment::Right => area.x.saturating_add(area.width.saturating_sub(line_width)),
        };

        let mut cursor_x = start_x;
        let right = area.right();

        for span in &line.spans {
            if cursor_x >= right {
                break;
            }
            let style = base_style.patch(span.style);
            let available = right.saturating_sub(cursor_x);
            let used =
                self.buffer
                    .write_text(Point::new(cursor_x, y), &span.content, style, available);
            cursor_x = cursor_x.saturating_add(used);
        }
    }

    pub fn draw_lines(
        &mut self,
        area: Rect,
        lines: &[Line],
        base_style: Style,
        alignment: Alignment,
    ) {
        for (row, line) in lines.iter().enumerate().take(area.height as usize) {
            self.draw_line(area, row as u16, line, base_style, alignment);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Frame;
    use crate::{Buffer, Point, Rect};

    #[test]
    fn clear_cursor_drops_a_cursor_set_earlier_this_frame() {
        let area = Rect::new(0, 0, 80, 24);
        let mut buffer = Buffer::new(area);
        let mut frame = Frame::new(area, &mut buffer);
        assert_eq!(frame.cursor(), None);
        frame.set_cursor(Point::new(3, 4));
        assert_eq!(frame.cursor(), Some(Point::new(3, 4)));
        frame.clear_cursor();
        assert_eq!(frame.cursor(), None);
    }
}
