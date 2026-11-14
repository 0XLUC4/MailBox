//! Tasks module: task lists, due dates, completion.
//! STUB: placeholder render only. Owner: tasks agent.

use gpui::*;

use crate::app_state::AppState;
use crate::components::{EmptyState, IconName};

pub struct TasksView {
    state: Entity<AppState>,
    _subscription: Subscription,
}

impl TasksView {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&state, |_, _, cx| cx.notify());
        Self {
            state,
            _subscription: subscription,
        }
    }
}

impl Render for TasksView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let store = self.state.read(cx).store();
        let description = format!(
            "{} open tasks. Task lists come next.",
            store.list_tasks().map_or(0, |tasks| tasks
                .iter()
                .filter(|task| !task.completed)
                .count())
        );
        div()
            .size_full()
            .child(EmptyState::new(IconName::SquareCheck, "Tasks").description(description))
    }
}
