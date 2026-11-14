//! Search box in the top bar. Writes `AppState::search_query`; views filter on it.
//! Owner: search agent (advanced syntax, suggestions, cross-module results).

use gpui::component::input::{Input, InputEvent, InputState};
use gpui::*;

use crate::app_state::AppState;
use crate::components::{Icon, IconName};
use crate::theme::{ICON_SIZE_SM, theme};

pub struct SearchView {
    state: Entity<AppState>,
    input: Entity<InputState>,
    _subscription: Subscription,
}

impl SearchView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search mail, people, events"));
        let subscription =
            cx.subscribe_in(&input, window, |this, input, event: &InputEvent, _, cx| {
                if let InputEvent::Change = event {
                    let query = input.read(cx).value().to_string();
                    this.state
                        .update(cx, |state, cx| state.set_search_query(query, cx));
                }
            });
        Self {
            state,
            input,
            _subscription: subscription,
        }
    }
}

impl Render for SearchView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = theme(cx).text_subtle;
        Input::new(&self.input)
            .prefix(
                Icon::new(IconName::Search)
                    .size(ICON_SIZE_SM)
                    .text_color(muted),
            )
            .cleanable(true)
    }
}
