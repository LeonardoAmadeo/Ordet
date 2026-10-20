use unicode_width::UnicodeWidthStr;

use crate::Style;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub content: String,
    pub style: Style,
}

impl Span {
    pub fn raw(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            style: Style::default(),
        }
    }

    pub fn styled(content: impl Into<String>, style: Style) -> Self {
        Self {
            content: content.into(),
            style,
        }
    }

    pub fn width(&self) -> usize {
        UnicodeWidthStr::width(self.content.as_str())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    pub spans: Vec<Span>,
}

impl Line {
    pub fn width(&self) -> usize {
        self.spans.iter().map(Span::width).sum()
    }
}

impl From<&str> for Line {
    fn from(value: &str) -> Self {
        Self {
            spans: vec![Span::raw(value)],
        }
    }
}

impl From<String> for Line {
    fn from(value: String) -> Self {
        Self {
            spans: vec![Span::raw(value)],
        }
    }
}

impl From<Span> for Line {
    fn from(value: Span) -> Self {
        Self { spans: vec![value] }
    }
}

impl From<Vec<Span>> for Line {
    fn from(value: Vec<Span>) -> Self {
        Self { spans: value }
    }
}
