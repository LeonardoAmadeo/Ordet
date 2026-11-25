//! Terminal colors, text modifiers, and composable cell [`Style`]s.

use std::ops::{BitAnd, BitOr, BitOrAssign, Sub};

/// Terminal foreground or background color.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Color {
    #[default]
    Reset,
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    Gray,
    DarkGray,
    LightRed,
    LightGreen,
    LightYellow,
    LightBlue,
    LightMagenta,
    LightCyan,
    White,
    /// 8-bit ANSI palette index (0..=255).
    Indexed(u8),
    /// 24-bit truecolor RGB.
    Rgb(u8, u8, u8),
}

impl Color {
    /// Approximate RGB tuple for colors that map to a concrete sRGB value, or
    /// `None` for [`Color::Reset`].
    pub const fn to_rgb(self) -> Option<(u8, u8, u8)> {
        match self {
            Color::Reset => None,
            Color::Black => Some((0, 0, 0)),
            Color::Red => Some((170, 0, 0)),
            Color::Green => Some((0, 170, 0)),
            Color::Yellow => Some((170, 85, 0)),
            Color::Blue => Some((0, 0, 170)),
            Color::Magenta => Some((170, 0, 170)),
            Color::Cyan => Some((0, 170, 170)),
            Color::Gray => Some((170, 170, 170)),
            Color::DarkGray => Some((85, 85, 85)),
            Color::LightRed => Some((255, 85, 85)),
            Color::LightGreen => Some((85, 255, 85)),
            Color::LightYellow => Some((255, 255, 85)),
            Color::LightBlue => Some((85, 85, 255)),
            Color::LightMagenta => Some((255, 85, 255)),
            Color::LightCyan => Some((85, 255, 255)),
            Color::White => Some((255, 255, 255)),
            Color::Indexed(index) => Some(ansi_index_to_rgb(index)),
            Color::Rgb(r, g, b) => Some((r, g, b)),
        }
    }
}

const fn ansi_index_to_rgb(index: u8) -> (u8, u8, u8) {
    match index {
        0 => (0, 0, 0),
        1 => (170, 0, 0),
        2 => (0, 170, 0),
        3 => (170, 85, 0),
        4 => (0, 0, 170),
        5 => (170, 0, 170),
        6 => (0, 170, 170),
        7 => (170, 170, 170),
        8 => (85, 85, 85),
        9 => (255, 85, 85),
        10 => (85, 255, 85),
        11 => (255, 255, 85),
        12 => (85, 85, 255),
        13 => (255, 85, 255),
        14 => (85, 255, 255),
        15 => (255, 255, 255),
        16..=231 => {
            let n = index - 16;
            let r = (n / 36) % 6;
            let g = (n / 6) % 6;
            let b = n % 6;
            const fn scale(v: u8) -> u8 {
                if v == 0 {
                    0
                } else {
                    55 + v * 40
                }
            }
            (scale(r), scale(g), scale(b))
        }
        232..=255 => {
            let gray = 8 + (index - 232) * 10;
            (gray, gray, gray)
        }
    }
}

/// Bitflag set of terminal text attributes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifier(u8);

impl Modifier {
    pub const NONE: Self = Self(0);
    pub const BOLD: Self = Self(1 << 0);
    pub const ITALIC: Self = Self(1 << 1);
    pub const UNDERLINED: Self = Self(1 << 2);
    pub const DIM: Self = Self(1 << 3);
    pub const REVERSED: Self = Self(1 << 4);
    pub const CROSSED_OUT: Self = Self(1 << 5);

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }
}

impl BitOr for Modifier {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for Modifier {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for Modifier {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        Self(self.0 & rhs.0)
    }
}

impl Sub for Modifier {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self(self.0 & !rhs.0)
    }
}

/// Composable foreground, background, and text modifier attributes for a cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub modifier: Modifier,
}

impl Style {
    pub const fn new() -> Self {
        Self {
            fg: None,
            bg: None,
            modifier: Modifier::NONE,
        }
    }

    pub const fn reset() -> Self {
        Self {
            fg: Some(Color::Reset),
            bg: Some(Color::Reset),
            modifier: Modifier::NONE,
        }
    }

    pub const fn fg(mut self, color: Color) -> Self {
        self.fg = Some(color);
        self
    }

    pub const fn bg(mut self, color: Color) -> Self {
        self.bg = Some(color);
        self
    }

    pub const fn add_modifier(mut self, modifier: Modifier) -> Self {
        self.modifier = self.modifier.union(modifier);
        self
    }

    pub const fn remove_modifier(mut self, modifier: Modifier) -> Self {
        self.modifier = self.modifier.without(modifier);
        self
    }

    pub const fn bold(self) -> Self {
        self.add_modifier(Modifier::BOLD)
    }

    pub const fn italic(self) -> Self {
        self.add_modifier(Modifier::ITALIC)
    }

    pub const fn underlined(self) -> Self {
        self.add_modifier(Modifier::UNDERLINED)
    }

    pub const fn dim(self) -> Self {
        self.add_modifier(Modifier::DIM)
    }

    pub const fn reversed(self) -> Self {
        self.add_modifier(Modifier::REVERSED)
    }

    /// Layers `overlay` on top of `self`: explicitly set colors in `overlay`
    /// replace `self`, and modifiers are unioned.
    pub fn patch(self, overlay: Style) -> Self {
        Self {
            fg: overlay.fg.or(self.fg),
            bg: overlay.bg.or(self.bg),
            modifier: self.modifier | overlay.modifier,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Color, Modifier, Style};

    #[test]
    fn style_patch_merges_colors_and_modifiers() {
        let base = Style::new().fg(Color::White).bg(Color::Black).bold();
        let overlay = Style::new().fg(Color::Cyan).italic();
        let merged = base.patch(overlay);
        assert_eq!(merged.fg, Some(Color::Cyan));
        assert_eq!(merged.bg, Some(Color::Black));
        assert!(merged.modifier.contains(Modifier::BOLD | Modifier::ITALIC));
    }

    #[test]
    fn modifier_bit_ops_add_and_remove_flags() {
        let m = Modifier::BOLD | Modifier::DIM | Modifier::UNDERLINED;
        assert!(m.contains(Modifier::BOLD));
        assert!(!(m - Modifier::DIM).contains(Modifier::DIM));
        assert_eq!(m & Modifier::UNDERLINED, Modifier::UNDERLINED);
    }
}
