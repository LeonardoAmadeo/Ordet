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

    pub fn set(&mut self, id: FocusId) {
        if self.order.contains(&id) {
            self.current = id;
        }
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
