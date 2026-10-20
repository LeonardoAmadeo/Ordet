use crossterm::event::KeyEvent;

use crate::Rect;

#[derive(Clone, Debug)]
pub enum UiEvent<Command, Message> {
    Key(KeyEvent),
    Command(Command),
    Message(Message),
    Tick,
    Resize(Rect),
}
