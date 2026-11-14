use gpui::component::notification::Notification;
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use super::{Button, Icon, IconName};
use crate::theme::{ICON_SIZE, RADIUS_MD, SPACE_2, TEXT_SM, theme};

/// Messages longer than this are shortened; the full text stays copyable.
const MAX_CHARS: usize = 280;
/// Zero-width space: an invisible line-break opportunity.
const BREAK: char = '\u{200B}';

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BannerKind {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

impl BannerKind {
    fn icon(self) -> IconName {
        match self {
            Self::Info => IconName::Info,
            Self::Success => IconName::CircleCheck,
            Self::Warning => IconName::TriangleAlert,
            Self::Error => IconName::CircleX,
        }
    }

    fn color(self, cx: &App) -> Hsla {
        let t = theme(cx);
        match self {
            Self::Info => t.accent,
            Self::Success => t.success,
            Self::Warning => t.warning,
            Self::Error => t.danger,
        }
    }
}

/// Inline status or error message for dialogs and cards. The text always wraps inside its
/// container (long hostnames and URLs included); very long server replies are shortened with
/// a button that copies the full text.
#[derive(IntoElement)]
pub struct Banner {
    id: ElementId,
    kind: BannerKind,
    icon: Option<IconName>,
    text: SharedString,
    plain: bool,
}

impl Banner {
    pub fn new(id: impl Into<ElementId>, kind: BannerKind, text: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            kind,
            icon: None,
            text: text.into(),
            plain: false,
        }
    }

    pub fn info(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        Self::new(id, BannerKind::Info, text)
    }

    pub fn success(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        Self::new(id, BannerKind::Success, text)
    }

    pub fn warning(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        Self::new(id, BannerKind::Warning, text)
    }

    pub fn error(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        Self::new(id, BannerKind::Error, text)
    }

    /// Replaces the kind's default icon.
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    /// No tinted background (field-level errors under an input).
    pub fn plain(mut self) -> Self {
        self.plain = true;
        self
    }
}

impl RenderOnce for Banner {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = self.kind.color(cx);
        let text_color = theme(cx).text;
        let (preview, shortened) = shorten(&self.text, MAX_CHARS);
        let full = self.text.clone();
        let copy = shortened.then(|| {
            // Scoped under the banner's own id, so several banners never clash.
            Button::new("banner-copy")
                .ghost()
                .compact()
                .icon(IconName::Copy)
                .label("Copy full message")
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(full.to_string()))
                })
        });
        div()
            .id(self.id)
            .flex()
            .flex_row()
            .items_start()
            .gap(SPACE_2)
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .text_size(TEXT_SM)
            .when(!self.plain, |el| {
                el.p(SPACE_2)
                    .rounded(RADIUS_MD)
                    .bg(color.alpha(0.1))
                    .text_color(text_color)
            })
            .when(self.plain, |el| el.text_color(color))
            .child(
                // Same height as one text line, so the icon lines up with the first line.
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .h(px(18.))
                    .child(
                        Icon::new(self.icon.unwrap_or(self.kind.icon()))
                            .size(ICON_SIZE)
                            .text_color(color),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .line_height(px(18.))
                    .whitespace_normal()
                    .child(div().w_full().child(breakable(&preview)))
                    .children(copy),
            )
    }
}

/// Toast for a message that may be very long (server replies): wraps, is shortened, and
/// offers to copy the full text.
pub fn toast(notification: Notification, message: &str) -> Notification {
    let (preview, shortened) = shorten(message, MAX_CHARS);
    let notification = notification.message(breakable(&preview));
    if !shortened {
        return notification;
    }
    let full: SharedString = message.to_owned().into();
    // Stays until dismissed, so there is time to copy it.
    notification.autohide(false).content(move |_, _, _| {
        let full = full.clone();
        Button::new("toast-copy")
            .ghost()
            .compact()
            .icon(IconName::Copy)
            .label("Copy full message")
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(full.to_string()))
            })
            .into_any_element()
    })
}

/// Adds invisible break opportunities after `/ . @ : _ - ? & =` so hostnames, addresses and
/// URLs wrap instead of running past their container.
pub fn breakable(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    for character in text.chars() {
        out.push(character);
        if matches!(
            character,
            '/' | '.' | '@' | ':' | '_' | '-' | '?' | '&' | '='
        ) {
            out.push(BREAK);
        }
    }
    out
}

/// First `max_chars` characters (whole words when possible) plus "…", and whether it was cut.
pub fn shorten(text: &str, max_chars: usize) -> (String, bool) {
    let text = text.trim();
    let Some((cut, _)) = text.char_indices().nth(max_chars) else {
        return (text.to_owned(), false);
    };
    let head = &text[..cut];
    let head = match head.rfind(char::is_whitespace) {
        Some(space) if space > cut / 2 => &head[..space],
        _ => head,
    };
    (format!("{}…", head.trim_end()), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breakable_adds_break_after_separators() {
        assert_eq!(
            breakable("a@b.c/d"),
            format!("a@{BREAK}b.{BREAK}c/{BREAK}d")
        );
        assert_eq!(breakable("plain words"), "plain words");
    }

    #[test]
    fn shorten_keeps_short_text() {
        assert_eq!(shorten("  short  ", 10), ("short".to_owned(), false));
    }

    #[test]
    fn shorten_cuts_at_word_and_counts_chars() {
        assert_eq!(
            shorten("hello wonderful world", 18),
            ("hello wonderful…".to_owned(), true)
        );
        // Multi-byte characters never split.
        let (text, cut) = shorten(&"é".repeat(20), 5);
        assert!(cut);
        assert_eq!(text, format!("{}…", "é".repeat(5)));
    }
}
