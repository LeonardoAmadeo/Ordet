use std::collections::HashMap;
use std::hash::Hash;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyChord {
    pub fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }

    pub fn plain(code: KeyCode) -> Self {
        Self::new(code, KeyModifiers::NONE)
    }

    pub fn ctrl(ch: char) -> Self {
        Self::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
    }
}

impl From<KeyEvent> for KeyChord {
    fn from(value: KeyEvent) -> Self {
        Self {
            code: value.code,
            modifiers: value.modifiers,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Keymap<Command> {
    bindings: HashMap<KeyChord, Command>,
}

impl<Command> Keymap<Command>
where
    Command: Clone + Eq + Hash,
{
    pub fn new() -> Self {
        Self {
            bindings: HashMap::new(),
        }
    }

    /// Binds a chord to a command.
    ///
    /// Rebinding the same chord to the same command is a no-op. Rebinding it to a
    /// *different* command panics instead of silently dropping the earlier binding,
    /// because a shadowed binding is otherwise impossible to notice.
    pub fn bind(&mut self, chord: KeyChord, command: Command) {
        if let Some(previous) = self.bindings.get(&chord) {
            assert!(
                previous == &command,
                "conflicting key bindings for {chord:?}: rebound to a different command"
            );
            return;
        }
        self.bindings.insert(chord, command);
    }

    pub fn resolve(&self, key: KeyEvent) -> Option<Command> {
        self.bindings.get(&KeyChord::from(key)).cloned()
    }

    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{KeyChord, Keymap};

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    enum Command {
        Save,
        Quit,
    }

    #[test]
    fn resolves_bound_key_chords() {
        let mut keymap = Keymap::new();
        keymap.bind(KeyChord::ctrl('s'), Command::Save);

        let resolved = keymap.resolve(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));

        assert_eq!(resolved, Some(Command::Save));
    }

    #[test]
    fn rebinding_the_same_chord_to_the_same_command_is_allowed() {
        let mut keymap = Keymap::new();
        keymap.bind(KeyChord::ctrl('s'), Command::Save);
        keymap.bind(KeyChord::ctrl('s'), Command::Save);

        assert_eq!(keymap.len(), 1);
    }

    #[test]
    #[should_panic(expected = "conflicting key bindings")]
    fn rebinding_a_chord_to_a_different_command_panics() {
        let mut keymap = Keymap::new();
        keymap.bind(KeyChord::ctrl('s'), Command::Save);
        keymap.bind(KeyChord::ctrl('s'), Command::Quit);
    }
}
