use gpui::prelude::FluentBuilder as _;
use gpui::*;

use super::{ClickHandler, Icon, IconName};
use crate::theme::{ICON_SIZE_SM, RADIUS_MD, SPACE_1_5, SPACE_2, SPACE_3, TEXT_SM, theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonVariant {
    /// Bordered neutral button.
    #[default]
    Secondary,
    /// Filled with the accent colour, for the main action of a view.
    Primary,
    /// No background until hovered (toolbars).
    Ghost,
    /// Destructive action.
    Danger,
}

/// Text button with an optional leading icon.
///
/// ```ignore
/// Button::new("new-mail").primary().icon(IconName::SquarePen).label("New mail")
///     .on_click(|_, window, cx| { /* ... */ })
/// ```
#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: Option<SharedString>,
    icon: Option<IconName>,
    variant: ButtonVariant,
    disabled: bool,
    compact: bool,
    on_click: Option<ClickHandler>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            label: None,
            icon: None,
            variant: ButtonVariant::default(),
            disabled: false,
            compact: false,
            on_click: None,
        }
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn primary(self) -> Self {
        self.variant(ButtonVariant::Primary)
    }

    pub fn ghost(self) -> Self {
        self.variant(ButtonVariant::Ghost)
    }

    pub fn danger(self) -> Self {
        self.variant(ButtonVariant::Danger)
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Smaller height for dense toolbars.
    pub fn compact(mut self) -> Self {
        self.compact = true;
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

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx).clone();
        let transparent = t.background.alpha(0.);
        let (bg, fg, border, hover_bg) = match self.variant {
            ButtonVariant::Primary => (t.accent, t.on_accent, t.accent, t.accent_hover),
            ButtonVariant::Secondary => (t.elevated, t.text, t.border, t.hover),
            ButtonVariant::Ghost => (transparent, t.text, transparent, t.hover),
            ButtonVariant::Danger => (t.danger, t.on_accent, t.danger, t.danger.alpha(0.85)),
        };
        let interactive = !self.disabled;

        div()
            .id(self.id)
            .flex()
            .flex_row()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .gap(SPACE_1_5)
            .h(if self.compact { px(24.) } else { px(30.) })
            .px(if self.compact { SPACE_2 } else { SPACE_3 })
            .rounded(RADIUS_MD)
            .border_1()
            .border_color(border)
            .bg(bg)
            .text_color(fg)
            .text_size(TEXT_SM)
            .font_weight(FontWeight::MEDIUM)
            .when_some(self.icon, |el, icon| {
                el.child(Icon::new(icon).size(ICON_SIZE_SM).text_color(fg))
            })
            .when_some(self.label, |el, label| el.child(label))
            .when(self.disabled, |el| el.opacity(0.45))
            .when(interactive, |el| {
                el.cursor_pointer().hover(move |style| style.bg(hover_bg))
            })
            .when_some(self.on_click.filter(|_| interactive), |el, handler| {
                el.on_click(move |event, window, cx| {
                    // A button inside a clickable row must not click the row too.
                    cx.stop_propagation();
                    handler(event, window, cx)
                })
            })
    }
}
