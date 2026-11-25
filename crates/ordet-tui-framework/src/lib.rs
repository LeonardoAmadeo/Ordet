//! # `ordet-tui-framework`
//!
//! A small, explicit, event-driven TUI framework inspired by GPUI's separation
//! of state, layout, rendering, focus, and background tasks—without relying on
//! ratatui or retained widget trees.
//!
//! ## Core primitives
//!
//! - **Rendering**: [`Buffer`], [`Cell`], [`Symbol`], [`Frame`], and [`Terminal`]
//!   with Unicode grapheme segmentation, wide-character continuation cleanup,
//!   zero-allocation streaming diffing, ANSI cursor/style coalescing, and
//!   Synchronized Output (`DEC 2026`).
//! - **Layout & Geometry**: [`Point`], [`Rect`], [`split`], [`Direction`],
//!   [`Constraint`], and [`Alignment`].
//! - **Styling & Text**: [`Color`], [`Modifier`], [`Style`], [`Span`], and [`Line`].
//! - **Architecture**: [`Component`], [`EventCx`], [`UiEvent`], [`FocusState`],
//!   [`Keymap`], and [`TaskPool`].
//!
//! ## Quick headless example
//!
//! ```rust
//! use ordet_tui_framework::{
//!     Alignment, Color, Line, Point, Rect, Style, Terminal,
//! };
//!
//! let area = Rect::new(0, 0, 20, 3);
//! let mut terminal = Terminal::with_writer(Vec::<u8>::new(), area);
//! terminal.draw(|frame| {
//!     let inner = frame.draw_border(area, Some("Demo"), Style::default(), Style::default());
//!     frame.draw_line(
//!         inner,
//!         0,
//!         &Line::from("Hello, TUI!"),
//!         Style::new().fg(Color::Cyan).bold(),
//!         Alignment::Center,
//!     );
//!     frame.set_cursor(Point::new(inner.x, inner.y));
//! }).unwrap();
//! assert!(!terminal.writer().is_empty());
//! ```

mod buffer;
mod command;
mod component;
mod event;
mod focus;
mod frame;
mod geometry;
mod layout;
mod style;
mod tasks;
mod terminal;
mod text;

pub use buffer::{Buffer, Cell, Symbol};
pub use command::{KeyBindConflict, KeyChord, Keymap};
pub use component::{Component, EventCx};
pub use event::UiEvent;
pub use focus::{FocusId, FocusState};
pub use frame::Frame;
pub use geometry::{Point, Rect};
pub use layout::{split, Alignment, Constraint, Direction};
pub use style::{Color, Modifier, Style};
pub use tasks::{CancelToken, TaskError, TaskHandle, TaskKey, TaskPool, TaskResult};
pub use terminal::Terminal;
pub use text::{Line, Span};
