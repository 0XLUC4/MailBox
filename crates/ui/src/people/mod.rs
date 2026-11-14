//! People module: contact list, details, editing.
//! STUB: placeholder render only. Owner: people agent.

use gpui::*;

use crate::app_state::AppState;
use crate::components::{EmptyState, IconName};

pub struct PeopleView {
    state: Entity<AppState>,
    _subscription: Subscription,
}

impl PeopleView {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&state, |_, _, cx| cx.notify());
        Self {
            state,
            _subscription: subscription,
        }
    }
}

impl Render for PeopleView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let store = self.state.read(cx).store();
        let description = format!(
            "{} contacts. Contact list and details come next.",
            store.list_contacts().map_or(0, |contacts| contacts.len())
        );
        div()
            .size_full()
            .child(EmptyState::new(IconName::Users, "People").description(description))
    }
}
