//! 2D terminal grid geometry: [`Point`] and [`Rect`].

/// A zero-based cell coordinate `(x, y)` in a terminal grid.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Point {
    pub x: u16,
    pub y: u16,
}

impl Point {
    pub const fn new(x: u16, y: u16) -> Self {
        Self { x, y }
    }
}

impl From<(u16, u16)> for Point {
    fn from(value: (u16, u16)) -> Self {
        Self::new(value.0, value.1)
    }
}

/// An axis-aligned rectangle in terminal cell coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    pub const fn right(self) -> u16 {
        self.x.saturating_add(self.width)
    }

    pub const fn bottom(self) -> u16 {
        self.y.saturating_add(self.height)
    }

    pub const fn area(self) -> usize {
        self.width as usize * self.height as usize
    }

    pub const fn contains(self, point: Point) -> bool {
        point.x >= self.x && point.x < self.right() && point.y >= self.y && point.y < self.bottom()
    }

    /// Shrinks the rectangle symmetrically on both axes, saturating at zero size.
    pub const fn inset(self, horizontal: u16, vertical: u16) -> Self {
        let width = self.width.saturating_sub(horizontal.saturating_mul(2));
        let height = self.height.saturating_sub(vertical.saturating_mul(2));
        Self {
            x: self.x.saturating_add(horizontal),
            y: self.y.saturating_add(vertical),
            width,
            height,
        }
    }

    /// Returns the overlapping region of two rectangles, or an empty rectangle
    /// if they do not overlap.
    pub const fn intersect(self, other: Self) -> Self {
        let x = if self.x > other.x { self.x } else { other.x };
        let y = if self.y > other.y { self.y } else { other.y };
        let self_r = self.right();
        let other_r = other.right();
        let right = if self_r < other_r { self_r } else { other_r };
        let self_b = self.bottom();
        let other_b = other.bottom();
        let bottom = if self_b < other_b { self_b } else { other_b };
        Self {
            x,
            y,
            width: right.saturating_sub(x),
            height: bottom.saturating_sub(y),
        }
    }

    /// Returns the smallest rectangle that encloses both `self` and `other`.
    pub const fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let x = if self.x < other.x { self.x } else { other.x };
        let y = if self.y < other.y { self.y } else { other.y };
        let self_r = self.right();
        let other_r = other.right();
        let right = if self_r > other_r { self_r } else { other_r };
        let self_b = self.bottom();
        let other_b = other.bottom();
        let bottom = if self_b > other_b { self_b } else { other_b };
        Self {
            x,
            y,
            width: right.saturating_sub(x),
            height: bottom.saturating_sub(y),
        }
    }

    /// Clamps a point to the closest cell inside this rectangle.
    /// Returns `None` if the rectangle is empty.
    pub const fn clamp_point(self, point: Point) -> Option<Point> {
        if self.is_empty() {
            return None;
        }
        let max_x = self.right().saturating_sub(1);
        let max_y = self.bottom().saturating_sub(1);
        let x = if point.x < self.x {
            self.x
        } else if point.x > max_x {
            max_x
        } else {
            point.x
        };
        let y = if point.y < self.y {
            self.y
        } else if point.y > max_y {
            max_y
        } else {
            point.y
        };
        Some(Point::new(x, y))
    }
}

#[cfg(test)]
mod tests {
    use super::{Point, Rect};

    #[test]
    fn intersect_and_union_handle_overlapping_and_disjoint_rects() {
        let a = Rect::new(2, 2, 10, 6);
        let b = Rect::new(8, 4, 10, 6);
        assert_eq!(a.intersect(b), Rect::new(8, 4, 4, 4));
        assert_eq!(a.union(b), Rect::new(2, 2, 16, 8));

        let c = Rect::new(30, 30, 5, 5);
        assert!(a.intersect(c).is_empty());
        assert_eq!(Rect::default().union(a), a);
    }

    #[test]
    fn clamp_point_bounds_coordinates_inside_rect() {
        let rect = Rect::new(5, 3, 4, 2);
        assert_eq!(rect.clamp_point(Point::new(0, 0)), Some(Point::new(5, 3)));
        assert_eq!(rect.clamp_point(Point::new(99, 99)), Some(Point::new(8, 4)));
        assert_eq!(rect.clamp_point(Point::new(6, 4)), Some(Point::new(6, 4)));
        assert_eq!(Rect::new(0, 0, 0, 5).clamp_point(Point::new(0, 0)), None);
    }
}
