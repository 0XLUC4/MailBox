use gpui::component::tooltip::Tooltip;
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use super::{ClickHandler, Icon, IconName};
use crate::theme::{ICON_SIZE, RADIUS_MD, theme};

/// Square icon-only button (rail, toolbars). `active` shows the selected state.
#[derive(IntoElement)]
pub struct IconButton {
    id: ElementId,
    icon: IconName,
    size: Pixels,
    icon_size: Pixels,
    active: bool,
    disabled: bool,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

impl IconButton {
    pub fn new(id: impl Into<ElementId>, icon: IconName) -> Self {
        Self {
            id: id.into(),
            icon,
            size: px(28.),
            icon_size: ICON_SIZE,
            active: false,
            disabled: false,
            tooltip: None,
            on_click: None,
        }
    }

    /// Button side length (icon scales proportionally if not set explicitly).
    pub fn size(mut self, size: Pixels) -> Self {
        self.size = size;
        self
    }

    pub fn icon_size(mut self, size: Pixels) -> Self {
        self.icon_size = size;
        self
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for IconButton {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx).clone();
        let interactive = !self.disabled;
        let fg = if self.active { t.accent } else { t.text_muted };
        let hover_bg = t.hover;
        let hover_fg = t.text;

        div()
            .id(self.id)
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .size(self.size)
            .rounded(RADIUS_MD)
            .text_color(fg)
            .when(self.active, |el| el.bg(t.accent_subtle))
            .child(Icon::new(self.icon).size(self.icon_size))
            .when(self.disabled, |el| el.opacity(0.4))
            .when(interactive && !self.active, |el| {
                el.cursor_pointer()
                    .hover(move |style| style.bg(hover_bg).text_color(hover_fg))
            })
            .when(interactive && self.active, |el| el.cursor_pointer())
            .when_some(self.tooltip, |el, tooltip| {
                el.tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            })
            .when_some(self.on_click.filter(|_| interactive), |el, handler| {
                el.on_click(move |event, window, cx| handler(event, window, cx))
            })
    }
}
