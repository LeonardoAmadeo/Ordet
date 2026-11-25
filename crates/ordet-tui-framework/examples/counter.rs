//! Minimal interactive counter app demonstrating `ordet-tui-framework`.
//!
//! Run with: `cargo run -p ordet-tui-framework --example counter`

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ordet_tui_framework::{
    split, Alignment, Color, Component, Constraint, Direction, EventCx, Frame, KeyChord, Keymap,
    Line, Rect, Span, Style, Terminal, UiEvent,
};

#[derive(Default)]
struct Model {
    count: i64,
    should_quit: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Command {
    Increment,
    Decrement,
    Reset,
    Quit,
}

enum Action {
    Quit,
}

struct CounterView;

impl Component<Model, Command, (), Action> for CounterView {
    fn render(&self, model: &Model, frame: &mut Frame<'_>, area: Rect, _focused: bool) {
        let rows = split(
            area,
            Direction::Vertical,
            &[Constraint::Fill(1), Constraint::Length(1)],
        );
        let inner = frame.draw_border(
            rows[0],
            Some("ordet-tui-framework counter"),
            Style::default(),
            Style::new().fg(Color::Cyan),
        );
        let center_row = inner.height / 2;
        let line = Line::from(vec![
            Span::styled("Count: ", Style::new().dim()),
            Span::styled(
                model.count.to_string(),
                Style::new().fg(Color::Yellow).bold(),
            ),
        ]);
        frame.draw_line(
            inner,
            center_row,
            &line,
            Style::default(),
            Alignment::Center,
        );
        frame.draw_line(
            rows[1],
            0,
            &Line::from(" [+] / Up: increment   [-] / Down: decrement   [r]: reset   [q]: quit"),
            Style::new().dim(),
            Alignment::Left,
        );
    }

    fn handle_event(
        &mut self,
        model: &mut Model,
        event: &UiEvent<Command, ()>,
        cx: &mut EventCx<Action>,
    ) {
        if let UiEvent::Command(cmd) = event {
            match cmd {
                Command::Increment => model.count += 1,
                Command::Decrement => model.count -= 1,
                Command::Reset => model.count = 0,
                Command::Quit => cx.emit(Action::Quit),
            }
        }
    }
}

fn main() -> io::Result<()> {
    let mut keymap = Keymap::new();
    keymap.bind(KeyChord::plain(KeyCode::Char('+')), Command::Increment);
    keymap.bind(KeyChord::plain(KeyCode::Up), Command::Increment);
    keymap.bind(KeyChord::plain(KeyCode::Char('-')), Command::Decrement);
    keymap.bind(KeyChord::plain(KeyCode::Down), Command::Decrement);
    keymap.bind(KeyChord::plain(KeyCode::Char('r')), Command::Reset);
    keymap.bind(KeyChord::plain(KeyCode::Char('q')), Command::Quit);
    keymap.bind(KeyChord::ctrl('c'), Command::Quit);

    let mut terminal = Terminal::enter()?;
    let mut model = Model::default();
    let mut view = CounterView;

    while !model.should_quit {
        let area = terminal.size()?;
        terminal.draw(|frame| view.render(&model, frame, area, true))?;

        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if let Some(cmd) = keymap.resolve(key) {
                        let mut cx = EventCx::new();
                        view.handle_event(&mut model, &UiEvent::Command(cmd), &mut cx);
                        let (actions, _) = cx.into_parts();
                        for Action::Quit in actions {
                            model.should_quit = true;
                        }
                    }
                }
                Event::Resize(w, h) => terminal.update_size(Rect::new(0, 0, w, h)),
                _ => {}
            }
        }
    }

    terminal.restore()
}
