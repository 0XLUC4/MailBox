use gpui::prelude::FluentBuilder as _;
use gpui::*;

use super::{Icon, IconName};
use crate::theme::{RADIUS_FULL, SPACE_1, SPACE_3, SPACE_4, TEXT_LG, TEXT_MD, theme};

/// Centered icon + title + optional description and action, for empty views.
#[derive(IntoElement)]
pub struct EmptyState {
    icon: IconName,
    title: SharedString,
    description: Option<SharedString>,
    action: Option<AnyElement>,
}

impl EmptyState {
    pub fn new(icon: IconName, title: impl Into<SharedString>) -> Self {
        Self {
            icon,
            title: title.into(),
            description: None,
            action: None,
        }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Usually a [`super::Button`].
    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.action = Some(action.into_any_element());
        self
    }
}

impl RenderOnce for EmptyState {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx).clone();
        div()
            .flex()
            .flex_col()
            .size_full()
            .items_center()
            .justify_center()
            .gap(SPACE_1)
            .p(SPACE_4)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(52.))
                    .mb(SPACE_3)
                    .rounded(RADIUS_FULL)
                    .bg(t.hover)
                    .text_color(t.text_subtle)
                    .child(Icon::new(self.icon).size(px(24.))),
            )
            .child(
                div()
                    .text_size(TEXT_LG)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.title),
            )
            .when_some(self.description, |el, description| {
                el.child(
                    div()
                        .max_w(px(340.))
                        .text_center()
                        .text_size(TEXT_MD)
                        .text_color(t.text_muted)
                        .child(description),
                )
            })
            .when_some(self.action, |el, action| {
                el.child(div().mt(SPACE_3).child(action))
            })
    }
}
