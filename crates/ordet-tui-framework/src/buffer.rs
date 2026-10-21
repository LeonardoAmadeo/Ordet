use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{Point, Rect, Style};

thread_local! {
    /// Interner for cell symbols. Rendering is single-threaded, so a
    /// thread-local table keeps `Symbol::intern` free of locks and its
    /// payload non-atomic, which is what makes `Buffer::clear` and the frame
    /// diff allocation-churn-free.
    static INTERNER: RefCell<HashSet<Rc<str>>> = RefCell::new(HashSet::new());
    /// Pre-interned single-byte ASCII symbols. Editor text is overwhelmingly
    /// ASCII, and a table index beats a hash lookup per grapheme per row.
    static ASCII: RefCell<Vec<Rc<str>>> = RefCell::new(
        (0u8..128)
            .map(|byte| Rc::from(char::from(byte).to_string()))
            .collect(),
    );
}

/// Interned cell symbol. Cloning a `Symbol` is a refcount bump and comparing
/// two of them is usually a pointer compare, which keeps `Buffer::clear` and
/// the frame diff allocation-free.
#[derive(Clone, Debug)]
pub struct Symbol(Rc<str>);

impl Symbol {
    pub fn intern(symbol: &str) -> Self {
        let bytes = symbol.as_bytes();
        if bytes.len() == 1 && bytes[0] < 128 {
            return Self::from_ascii(bytes[0]);
        }
        INTERNER.with(|interner| {
            let mut interner = interner.borrow_mut();
            if let Some(existing) = interner.get(symbol) {
                return Self(Rc::clone(existing));
            }
            // Bound long-running sessions that paste many distinct symbols.
            // Existing cells keep their own Rc, so clearing only resets the
            // sharing table; it never invalidates rendered text.
            if interner.len() >= 4096 {
                interner.clear();
            }
            let entry: Rc<str> = Rc::from(symbol);
            interner.insert(Rc::clone(&entry));
            Self(entry)
        })
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Symbol for one ASCII byte. Table-backed: no hashing, no allocation.
    fn from_ascii(byte: u8) -> Self {
        Self(ascii(byte))
    }

    fn space() -> Self {
        Self::from_ascii(b' ')
    }
}

fn ascii(byte: u8) -> Rc<str> {
    ASCII.with(|table| Rc::clone(&table.borrow()[usize::from(byte)]))
}

impl Default for Symbol {
    fn default() -> Self {
        Self::space()
    }
}

impl PartialEq for Symbol {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0) || self.as_str() == other.as_str()
    }
}

impl PartialEq<&str> for Symbol {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl std::ops::Deref for Symbol {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl Eq for Symbol {}

impl From<&str> for Symbol {
    fn from(value: &str) -> Self {
        Self::intern(value)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cell {
    pub symbol: Symbol,
    pub style: Style,
    pub continuation: bool,
}

impl Cell {
    pub fn blank(style: Style) -> Self {
        Self {
            symbol: Symbol::default(),
            style,
            continuation: false,
        }
    }

    pub fn continuation(style: Style) -> Self {
        Self {
            symbol: Symbol::intern(""),
            style,
            continuation: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Buffer {
    area: Rect,
    cells: Vec<Cell>,
}

impl Buffer {
    pub fn new(area: Rect) -> Self {
        Self {
            area,
            cells: vec![Cell::default(); area.area()],
        }
    }

    pub fn area(&self) -> Rect {
        self.area
    }

    pub fn resize(&mut self, area: Rect) {
        self.area = area;
        self.cells = vec![Cell::default(); area.area()];
    }

    pub fn clear(&mut self, style: Style) {
        self.cells.fill(Cell::blank(style));
    }

    pub fn fill_rect(&mut self, rect: Rect, style: Style) {
        let clipped = self.clip(rect);
        for y in clipped.y..clipped.bottom() {
            for x in clipped.x..clipped.right() {
                self.set_cell(Point::new(x, y), Cell::blank(style));
            }
        }
    }

    pub fn write_text(&mut self, point: Point, text: &str, style: Style, max_width: u16) -> u16 {
        if point.y < self.area.y || point.y >= self.area.bottom() {
            return 0;
        }
        let limit = point.x.saturating_add(max_width).min(self.area.right());
        let mut cursor_x = point.x;

        // Fast path for plain ASCII, which is nearly all editor text: no
        // grapheme segmentation, no width tables, one byte one column.
        if text.is_ascii() {
            for &byte in text.as_bytes() {
                if cursor_x >= self.area.right() || cursor_x >= limit {
                    break;
                }
                self.set_cell(
                    Point::new(cursor_x, point.y),
                    Cell {
                        symbol: Symbol::from_ascii(byte),
                        style,
                        continuation: false,
                    },
                );
                cursor_x += 1;
            }
            return cursor_x.saturating_sub(point.x);
        }

        for grapheme in UnicodeSegmentation::graphemes(text, true) {
            let width = UnicodeWidthStr::width(grapheme).max(1) as u16;
            if cursor_x >= self.area.right() || cursor_x >= limit {
                break;
            }
            if cursor_x.saturating_add(width) > self.area.right()
                || cursor_x.saturating_add(width) > limit
            {
                break;
            }

            self.set_cell(
                Point::new(cursor_x, point.y),
                Cell {
                    symbol: Symbol::intern(grapheme),
                    style,
                    continuation: false,
                },
            );

            for offset in 1..width {
                self.set_cell(
                    Point::new(cursor_x + offset, point.y),
                    Cell::continuation(style),
                );
            }

            cursor_x += width;
        }

        cursor_x.saturating_sub(point.x)
    }

    pub fn diff(&self, previous: &Self) -> Vec<(Point, &Cell)> {
        self.cells
            .iter()
            .zip(previous.cells.iter())
            .enumerate()
            .filter_map(|(index, (current, previous))| {
                if current == previous {
                    return None;
                }
                let x = index as u16 % self.area.width.max(1);
                let y = index as u16 / self.area.width.max(1);
                Some((
                    Point::new(self.area.x.saturating_add(x), self.area.y.saturating_add(y)),
                    current,
                ))
            })
            .collect()
    }

    /// Walk every cell and hand changed ones to `visit` without allocating.
    /// This is the frame-diff hot path: streaming the dirty cells keeps the
    /// per-keystroke render free of a per-frame `Vec` and its capacity churn.
    pub fn diff_for_each<F>(&self, previous: &Self, mut visit: F)
    where
        F: FnMut(Point, &Cell),
    {
        let width = self.area.width.max(1);
        for (index, (current, previous)) in self.cells.iter().zip(previous.cells.iter()).enumerate()
        {
            if current == previous {
                continue;
            }
            let x = index as u16 % width;
            let y = index as u16 / width;
            visit(
                Point::new(self.area.x.saturating_add(x), self.area.y.saturating_add(y)),
                current,
            );
        }
    }

    pub fn cell(&self, point: Point) -> Option<&Cell> {
        let index = self.index_of(point)?;
        self.cells.get(index)
    }

    fn set_cell(&mut self, point: Point, cell: Cell) {
        if let Some(index) = self.index_of(point) {
            self.cells[index] = cell;
        }
    }

    fn index_of(&self, point: Point) -> Option<usize> {
        if !self.area.contains(point) {
            return None;
        }
        let x = usize::from(point.x - self.area.x);
        let y = usize::from(point.y - self.area.y);
        Some(y * usize::from(self.area.width) + x)
    }

    fn clip(&self, rect: Rect) -> Rect {
        let area_right = self.area.right();
        let area_bottom = self.area.bottom();
        let x = rect.x.max(self.area.x).min(area_right);
        let y = rect.y.max(self.area.y).min(area_bottom);
        let right = rect.right().max(self.area.x).min(area_right);
        let bottom = rect.bottom().max(self.area.y).min(area_bottom);
        Rect::new(x, y, right.saturating_sub(x), bottom.saturating_sub(y))
    }
}

#[cfg(test)]
mod tests {
    use super::{Buffer, Symbol};
    use crate::{Point, Rect, Style};

    #[test]
    fn nonzero_origin_maps_cells_and_diff_to_absolute_points() {
        let area = Rect::new(5, 3, 4, 2);
        let mut buffer = Buffer::new(area);
        buffer.write_text(Point::new(5, 3), "ab", Style::default(), 4);
        assert_eq!(buffer.cell(Point::new(5, 3)).unwrap().symbol.as_str(), "a");
        assert_eq!(buffer.cell(Point::new(6, 3)).unwrap().symbol.as_str(), "b");
        assert!(buffer.cell(Point::new(4, 3)).is_none());
        assert!(buffer.cell(Point::new(5, 2)).is_none());

        let blank = Buffer::new(area);
        let changed = buffer.diff(&blank);
        assert!(changed
            .iter()
            .any(|(point, cell)| *point == Point::new(5, 3) && cell.symbol.as_str() == "a"));
        assert!(changed.iter().all(|(point, _)| point.y >= 3));
    }

    #[test]
    fn wide_grapheme_marks_continuation_cells() {
        let mut buffer = Buffer::new(Rect::new(0, 0, 4, 1));
        buffer.write_text(Point::new(0, 0), "界", Style::default(), 4);
        assert_eq!(
            buffer.cell(Point::new(0, 0)).unwrap().symbol,
            Symbol::intern("界")
        );
        assert!(buffer.cell(Point::new(1, 0)).unwrap().continuation);
    }

    #[test]
    fn interned_symbols_are_shared_not_allocated_per_cell() {
        let blank = super::Cell::default();
        assert_eq!(blank.symbol, super::Cell::blank(Style::default()).symbol);
        assert_eq!(Symbol::intern(" "), Symbol::intern(" "));
    }

    #[test]
    fn streaming_diff_visits_only_changed_cells() {
        let area = Rect::new(0, 0, 4, 1);
        let mut current = Buffer::new(area);
        let mut previous = Buffer::new(area);

        let mut visited = Vec::new();
        current.diff_for_each(&previous, |point, _| visited.push(point));
        assert!(visited.is_empty(), "identical buffers must not dirty cells");

        current.write_text(Point::new(2, 0), "x", Style::default(), 4);
        current.diff_for_each(&previous, |point, cell| {
            visited.push(point);
            assert_eq!(cell.symbol, Symbol::intern("x"));
        });
        assert_eq!(visited, vec![Point::new(2, 0)]);

        previous.write_text(Point::new(2, 0), "x", Style::default(), 4);
        visited.clear();
        current.diff_for_each(&previous, |point, _| visited.push(point));
        assert!(visited.is_empty(), "matching buffers must go quiet again");
    }
}
