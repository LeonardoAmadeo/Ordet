//! Key chord representation and conflict-checked [`Keymap`].

use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A normalized key combination (code + modifier flags).
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

    pub fn alt(ch: char) -> Self {
        Self::new(KeyCode::Char(ch), KeyModifiers::ALT)
    }

    pub fn shift(code: KeyCode) -> Self {
        Self::new(code, KeyModifiers::SHIFT)
    }

    pub fn ctrl_shift(ch: char) -> Self {
        Self::new(
            KeyCode::Char(ch),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        )
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

/// Error returned by [`Keymap::try_bind`] when a chord is already bound to a
/// different command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyBindConflict<Command> {
    pub chord: KeyChord,
    pub existing: Command,
    pub attempted: Command,
}

impl<Command: fmt::Debug> fmt::Display for KeyBindConflict<Command> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "conflicting key bindings for {:?}: already bound to {:?}, cannot bind {:?}",
            self.chord, self.existing, self.attempted
        )
    }
}

impl<Command: fmt::Debug> std::error::Error for KeyBindConflict<Command> {}

/// Maps [`KeyChord`]s to typed application commands.
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
    /// For a non-panicking version, use [`Self::try_bind`].
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

    /// Non-panicking version of [`Self::bind`].
    pub fn try_bind(
        &mut self,
        chord: KeyChord,
        command: Command,
    ) -> Result<(), KeyBindConflict<Command>> {
        if let Some(previous) = self.bindings.get(&chord) {
            if previous == &command {
                return Ok(());
            }
            return Err(KeyBindConflict {
                chord,
                existing: previous.clone(),
                attempted: command,
            });
        }
        self.bindings.insert(chord, command);
        Ok(())
    }

    /// Removes the binding for `chord`, returning the previously bound command if any.
    pub fn unbind(&mut self, chord: &KeyChord) -> Option<Command> {
        self.bindings.remove(chord)
    }

    pub fn resolve(&self, key: KeyEvent) -> Option<Command> {
        self.bindings.get(&KeyChord::from(key)).cloned()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&KeyChord, &Command)> {
        self.bindings.iter()
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

    #[test]
    fn try_bind_and_unbind_handle_conflicts_without_panicking() {
        let mut keymap = Keymap::new();
        assert!(keymap.try_bind(KeyChord::alt('q'), Command::Quit).is_ok());
        assert!(keymap.try_bind(KeyChord::alt('q'), Command::Save).is_err());
        assert_eq!(keymap.unbind(&KeyChord::alt('q')), Some(Command::Quit));
        assert!(keymap.is_empty());
    }
}
