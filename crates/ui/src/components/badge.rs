use gpui::*;

use crate::theme::{RADIUS_FULL, SPACE_1_5, TEXT_XS, theme};

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum BadgeVariant {
    /// Muted pill (counts in lists).
    #[default]
    Neutral,
    /// Accent filled (unread counts).
    Accent,
    Danger,
    /// Tinted with an arbitrary colour (categories).
    Color(Hsla),
}

/// Small pill for counts, categories and statuses.
#[derive(IntoElement)]
pub struct Badge {
    text: SharedString,
    variant: BadgeVariant,
}

impl Badge {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            variant: BadgeVariant::default(),
        }
    }

    /// Count badge; shows "99+" above 99.
    pub fn count(count: i64) -> Self {
        Self::new(if count > 99 {
            "99+".to_owned()
        } else {
            count.to_string()
        })
    }

    pub fn variant(mut self, variant: BadgeVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn accent(self) -> Self {
        self.variant(BadgeVariant::Accent)
    }

    pub fn color(self, color: Hsla) -> Self {
        self.variant(BadgeVariant::Color(color))
    }
}

impl RenderOnce for Badge {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx);
        let (bg, fg) = match self.variant {
            BadgeVariant::Neutral => (t.active, t.text_muted),
            BadgeVariant::Accent => (t.accent, t.on_accent),
            BadgeVariant::Danger => (t.danger, t.on_accent),
            BadgeVariant::Color(color) => (color.alpha(if t.is_dark { 0.25 } else { 0.15 }), color),
        };
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .h(px(18.))
            .min_w(px(18.))
            .px(SPACE_1_5)
            .rounded(RADIUS_FULL)
            .bg(bg)
            .text_color(fg)
            .text_size(TEXT_XS)
            .font_weight(FontWeight::SEMIBOLD)
            .child(self.text)
    }
}
