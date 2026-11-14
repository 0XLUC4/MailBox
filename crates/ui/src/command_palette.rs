//! Command palette: fuzzy list of actions mapped to Commands.
//! STUB: placeholder render only. Owner: command palette agent.

use gpui::*;

use crate::app_state::AppState;
use crate::components::{EmptyState, IconName};

pub struct CommandPaletteView {
    state: Entity<AppState>,
    _subscription: Subscription,
}

impl CommandPaletteView {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&state, |_, _, cx| cx.notify());
        Self {
            state,
            _subscription: subscription,
        }
    }
}

impl Render for CommandPaletteView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Placeholder: the real view reads `self.state.read(cx).store()`.
        let _ = self.state.read(cx);
        let description = "Search and run any command (Ctrl+K) - coming next.";
        div()
            .size_full()
            .child(EmptyState::new(IconName::Search, "Command palette").description(description))
    }
}
