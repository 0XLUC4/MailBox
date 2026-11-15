use gpui::*;

use crate::theme::{PANE_HEADER_HEIGHT, SPACE_1, SPACE_3, theme};

/// Horizontal bar of actions with a bottom border, 40px high.
///
/// ```ignore
/// Toolbar::new().child(IconButton::new("reply", IconName::Reply)).child(toolbar_separator(cx))
/// ```
#[derive(IntoElement, Default)]
pub struct Toolbar {
    children: Vec<AnyElement>,
}

impl Toolbar {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ParentElement for Toolbar {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Toolbar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx);
        div()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .items_center()
            .gap(SPACE_1)
            .h(PANE_HEADER_HEIGHT)
            .px(SPACE_3)
            .border_b_1()
            .border_color(t.border_subtle)
            .children(self.children)
    }
}

/// Thin vertical divider between toolbar groups.
pub fn toolbar_separator(cx: &App) -> Div {
    div()
        .flex_shrink_0()
        .w(px(1.))
        .h(px(18.))
        .mx(SPACE_1)
        .bg(theme(cx).border)
}
