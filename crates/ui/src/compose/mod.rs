//! Compose window/pane: recipients, subject, body editor, attachments, send.
//! STUB: placeholder render only. Owner: compose agent.

use gpui::*;

use crate::app_state::AppState;
use crate::components::{EmptyState, IconName};

pub struct ComposeView {
    state: Entity<AppState>,
    _subscription: Subscription,
}

impl ComposeView {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&state, |_, _, cx| cx.notify());
        Self {
            state,
            _subscription: subscription,
        }
    }
}

impl Render for ComposeView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Placeholder: the real view reads `self.state.read(cx).store()`.
        let _ = self.state.read(cx);
        let description = "The editor (to, cc, subject, body, attachments, send) comes next.";
        div()
            .size_full()
            .child(EmptyState::new(IconName::SquarePen, "New message").description(description))
    }
}
