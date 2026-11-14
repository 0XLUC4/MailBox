//! Shared dialogs (confirmations, folder picker, category picker, event editor...).
//! STUB. Owner: dialogs agent. Use `gpui::component::WindowExt::open_dialog` to show them;
//! create persistent entities (inputs) before opening, as the builder may re-run.

use gpui::*;

use crate::app_state::AppState;
use crate::components::{EmptyState, IconName};

/// Placeholder host for dialog content.
pub struct DialogsView {
    state: Entity<AppState>,
}

impl DialogsView {
    pub fn new(state: Entity<AppState>, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self { state }
    }

    pub fn state(&self) -> &Entity<AppState> {
        &self.state
    }
}

impl Render for DialogsView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(EmptyState::new(IconName::MessageSquare, "Dialogs"))
    }
}
