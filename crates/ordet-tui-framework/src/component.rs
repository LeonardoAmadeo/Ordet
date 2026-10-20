use crate::{FocusId, Frame, Rect, UiEvent};

pub struct EventCx<Action> {
    actions: Vec<Action>,
    focus_request: Option<FocusId>,
}

impl<Action> EventCx<Action> {
    pub fn new() -> Self {
        Self {
            actions: Vec::new(),
            focus_request: None,
        }
    }

    pub fn emit(&mut self, action: Action) {
        self.actions.push(action);
    }

    pub fn request_focus(&mut self, focus: FocusId) {
        self.focus_request = Some(focus);
    }

    pub fn into_parts(self) -> (Vec<Action>, Option<FocusId>) {
        (self.actions, self.focus_request)
    }
}

impl<Action> Default for EventCx<Action> {
    fn default() -> Self {
        Self::new()
    }
}

pub trait Component<Model, Command, Message, Action> {
    fn focus_id(&self) -> Option<FocusId> {
        None
    }

    fn render(&self, model: &Model, frame: &mut Frame<'_>, area: Rect, focused: bool);

    fn handle_event(
        &mut self,
        model: &mut Model,
        event: &UiEvent<Command, Message>,
        cx: &mut EventCx<Action>,
    );

    fn on_focus_changed(&mut self, _model: &mut Model, _focused: bool, _cx: &mut EventCx<Action>) {}
}
