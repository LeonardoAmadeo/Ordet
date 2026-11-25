//! Explicit focus ring for keyboard navigation between panes.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FocusId(pub &'static str);

#[derive(Clone, Debug)]
pub struct FocusState {
    order: Vec<FocusId>,
    current: FocusId,
}

impl FocusState {
    pub fn new(order: Vec<FocusId>) -> Self {
        let current = order.first().copied().unwrap_or(FocusId("root"));
        Self { order, current }
    }

    pub fn current(&self) -> FocusId {
        self.current
    }

    pub fn is_focused(&self, id: FocusId) -> bool {
        self.current == id
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub fn set(&mut self, id: FocusId) {
        if self.order.contains(&id) {
            self.current = id;
        }
    }

    /// Replaces the focus ring order, preserving the current focus if it still
    /// exists in the new order or falling back to the first entry.
    pub fn set_order(&mut self, order: Vec<FocusId>) {
        if !order.contains(&self.current) {
            self.current = order.first().copied().unwrap_or(FocusId("root"));
        }
        self.order = order;
    }

    /// Advances focus through the configured pane order.
    pub fn advance(&mut self) -> FocusId {
        if self.order.is_empty() {
            return self.current;
        }

        let index = self
            .order
            .iter()
            .position(|candidate| *candidate == self.current)
            .unwrap_or(0);
        self.current = self.order[(index + 1) % self.order.len()];
        self.current
    }

    /// Moves focus backward through the configured pane order.
    pub fn previous(&mut self) -> FocusId {
        if self.order.is_empty() {
            return self.current;
        }

        let index = self
            .order
            .iter()
            .position(|candidate| *candidate == self.current)
            .unwrap_or(0);
        let next = if index == 0 {
            self.order.len().saturating_sub(1)
        } else {
            index - 1
        };
        self.current = self.order[next];
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::{FocusId, FocusState};

    #[test]
    fn focus_cycles_forward_and_backward() {
        let mut focus = FocusState::new(vec![FocusId("a"), FocusId("b"), FocusId("c")]);
        assert_eq!(focus.current(), FocusId("a"));
        assert_eq!(focus.advance(), FocusId("b"));
        assert_eq!(focus.advance(), FocusId("c"));
        assert_eq!(focus.advance(), FocusId("a"));
        assert_eq!(focus.previous(), FocusId("c"));
        focus.set(FocusId("unknown"));
        assert_eq!(focus.current(), FocusId("c"));
    }

    #[test]
    fn set_order_preserves_existing_focus_or_falls_back() {
        let mut focus = FocusState::new(vec![FocusId("a"), FocusId("b")]);
        focus.set(FocusId("b"));
        focus.set_order(vec![FocusId("b"), FocusId("c")]);
        assert_eq!(focus.current(), FocusId("b"));
        focus.set_order(vec![FocusId("x"), FocusId("y")]);
        assert_eq!(focus.current(), FocusId("x"));
    }
}
