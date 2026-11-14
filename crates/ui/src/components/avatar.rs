use gpui::*;

use crate::theme::{RADIUS_FULL, hex};

/// Pleasant, distinguishable avatar backgrounds (white text stays readable).
const PALETTE: &[u32] = &[
    0x2563eb, 0x7c3aed, 0xdb2777, 0xea580c, 0x16a34a, 0x0d9488, 0x0891b2, 0x9333ea, 0xc2410c,
    0x4f46e5,
];

/// Up to two uppercase initials: "Ada Lovelace" -> "AL", "bob@x.com" -> "B".
pub fn initials(name: &str) -> String {
    let name = name.split('@').next().unwrap_or(name);
    let initials: String = name
        .split(|c: char| c.is_whitespace() || c == '.' || c == '-' || c == '_')
        .filter_map(|word| word.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    if initials.is_empty() {
        "?".to_owned()
    } else {
        initials
    }
}

/// Stable colour derived from a name (same person, same colour everywhere).
pub fn avatar_color(name: &str) -> Hsla {
    let hash = name.to_lowercase().bytes().fold(5381u32, |hash, byte| {
        hash.wrapping_mul(33) ^ u32::from(byte)
    });
    hex(PALETTE[hash as usize % PALETTE.len()])
}

/// Round avatar with initials on a colour derived from the name.
#[derive(IntoElement)]
pub struct Avatar {
    name: SharedString,
    size: Pixels,
}

impl Avatar {
    pub fn new(name: impl Into<SharedString>) -> Self {
        Self {
            name: name.into(),
            size: px(32.),
        }
    }

    pub fn size(mut self, size: Pixels) -> Self {
        self.size = size;
        self
    }
}

impl RenderOnce for Avatar {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .size(self.size)
            .rounded(RADIUS_FULL)
            .bg(avatar_color(&self.name))
            .text_color(hex(0xffffff))
            .text_size(self.size * 0.4)
            .font_weight(FontWeight::SEMIBOLD)
            .child(initials(&self.name))
    }
}

#[cfg(test)]
mod tests {
    use super::{avatar_color, initials};

    #[test]
    fn initials_cover_common_shapes() {
        assert_eq!(initials("Ada Lovelace"), "AL");
        assert_eq!(initials("sofia.ramirez@northwind.example"), "SR");
        assert_eq!(initials("bob"), "B");
        assert_eq!(initials(""), "?");
        assert_eq!(avatar_color("Ada"), avatar_color("ada"));
    }
}
