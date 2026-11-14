//! Calendar module: day/week/month views, event editor, recurrence expansion.
//! STUB: placeholder render only. Owner: calendar agent.

use gpui::*;

use crate::app_state::AppState;
use crate::components::{EmptyState, IconName};

pub struct CalendarView {
    state: Entity<AppState>,
    _subscription: Subscription,
}

impl CalendarView {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&state, |_, _, cx| cx.notify());
        Self {
            state,
            _subscription: subscription,
        }
    }
}

impl Render for CalendarView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let store = self.state.read(cx).store();
        let description = format!(
            "{} events in the next 7 days. Day, week and month views come next.",
            store
                .list_events_between(
                    chrono::Utc::now(),
                    chrono::Utc::now() + chrono::Duration::days(7)
                )
                .map_or(0, |events| events.len())
        );
        div()
            .size_full()
            .child(EmptyState::new(IconName::CalendarDays, "Calendar").description(description))
    }
}
