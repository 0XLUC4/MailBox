//! Design tokens: colours (light/dark + accent), spacing, radii, fonts and font sizes.
//!
//! Read with `let t = theme(cx).clone();` at the top of `render`, then use `t.text`, `t.border`...
//! The appearance follows the system unless the user picks Light/Dark in settings.

use std::sync::{LazyLock, OnceLock};

use gpui::component::{Theme as ComponentTheme, ThemeMode};
use gpui::{App, Global, Hsla, Pixels, SharedString, Window, WindowAppearance, hsla, px, rgb};

// ---------- spacing (4px grid) ----------
pub const SPACE_0_5: Pixels = px(2.);
pub const SPACE_1: Pixels = px(4.);
pub const SPACE_1_5: Pixels = px(6.);
pub const SPACE_2: Pixels = px(8.);
pub const SPACE_3: Pixels = px(12.);
pub const SPACE_4: Pixels = px(16.);
pub const SPACE_6: Pixels = px(24.);
pub const SPACE_8: Pixels = px(32.);

// ---------- radii ----------
pub const RADIUS_SM: Pixels = px(4.);
pub const RADIUS_MD: Pixels = px(6.);
pub const RADIUS_LG: Pixels = px(8.);
pub const RADIUS_XL: Pixels = px(12.);
pub const RADIUS_FULL: Pixels = px(9999.);

// ---------- font sizes ----------
/// Dense captions only (calendar hour labels, badges).
pub const TEXT_2XS: Pixels = px(10.);
pub const TEXT_XS: Pixels = px(11.);
pub const TEXT_SM: Pixels = px(12.);
pub const TEXT_MD: Pixels = px(13.);
pub const TEXT_LG: Pixels = px(15.);
pub const TEXT_XL: Pixels = px(18.);
pub const TEXT_2XL: Pixels = px(22.);

// ---------- fonts ----------
/// UI families, first installed wins; none installed keeps gpui's `.SystemUIFont`.
const UI_FONTS: &[&str] = if cfg!(target_os = "macos") {
    &[".SystemUIFont"]
} else if cfg!(target_os = "windows") {
    &["Segoe UI Variable Text", "Segoe UI"]
} else {
    &["Inter", "Cantarell", "Ubuntu", "Noto Sans", "DejaVu Sans"]
};

/// Monospace families, first installed wins; none installed keeps gpui-kit's default.
const MONO_FONTS: &[&str] = if cfg!(target_os = "macos") {
    &["SF Mono", "Menlo"]
} else if cfg!(target_os = "windows") {
    &["Cascadia Mono", "Consolas"]
} else {
    &["DejaVu Sans Mono"]
};

/// The same UI stack for web views (compose editor, rendered mail).
pub const CSS_UI_FONT: &str = "'Segoe UI Variable Text','Segoe UI',-apple-system,\
     BlinkMacSystemFont,system-ui,Inter,Cantarell,Ubuntu,'Noto Sans',sans-serif";

// ---------- layout ----------
pub const RAIL_WIDTH: Pixels = px(56.);
pub const TOP_BAR_HEIGHT: Pixels = px(48.);
/// Every pane and module header (folders, list, reading pane, calendar, tasks, people, editors),
/// so their bottom borders line up.
pub const PANE_HEADER_HEIGHT: Pixels = px(44.);
/// Rows of the folder tree and the Calendar / Tasks / People sidebars.
pub const NAV_ROW_HEIGHT: Pixels = px(32.);
/// Dialog widths: confirmations, forms, wide editors.
pub const DIALOG_SM: Pixels = px(400.);
pub const DIALOG_MD: Pixels = px(480.);
pub const DIALOG_LG: Pixels = px(560.);
pub const ICON_SIZE: Pixels = px(16.);
pub const ICON_SIZE_SM: Pixels = px(14.);
/// Inline glyphs next to small text (attachment, flag, pin).
pub const ICON_SIZE_XS: Pixels = px(12.);
/// Classic ribbon buttons, empty-state headers.
pub const ICON_SIZE_LG: Pixels = px(22.);

/// Opacity of disabled controls.
pub const DISABLED_OPACITY: f32 = 0.4;

/// Default accent (blue).
pub const DEFAULT_ACCENT: u32 = 0x3b82f6;

/// Accent choices offered in settings.
pub const ACCENTS: &[(&str, u32)] = &[
    ("Blue", 0x3b82f6),
    ("Indigo", 0x6366f1),
    ("Violet", 0x8b5cf6),
    ("Pink", 0xec4899),
    ("Orange", 0xf97316),
    ("Green", 0x22c55e),
    ("Teal", 0x14b8a6),
];

/// User preference for light/dark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

/// Resolved colour tokens for the current appearance.
#[derive(Debug, Clone)]
pub struct Theme {
    pub is_dark: bool,
    /// App background (reading pane, main content).
    pub background: Hsla,
    /// Secondary panes (folder pane, top bar).
    pub surface: Hsla,
    /// Left rail.
    pub rail: Hsla,
    /// Cards, popovers, inputs.
    pub elevated: Hsla,
    pub border: Hsla,
    pub border_subtle: Hsla,
    pub text: Hsla,
    pub text_muted: Hsla,
    pub text_subtle: Hsla,
    /// Hover overlay for rows and ghost buttons.
    pub hover: Hsla,
    /// Pressed overlay.
    pub active: Hsla,
    pub accent: Hsla,
    pub accent_hover: Hsla,
    /// Selected row background.
    pub accent_subtle: Hsla,
    /// Text on an accent background. For other fills use [`text_on`].
    pub on_accent: Hsla,
    /// Backdrop behind modal sheets and popovers.
    pub scrim: Hsla,
    pub danger: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
}

/// `0xRRGGBB` to [`Hsla`].
pub fn hex(color: u32) -> Hsla {
    rgb(color).into()
}

/// A theme color as `(r, g, b)` (alpha ignored).
pub fn rgb_bytes(color: Hsla) -> (u8, u8, u8) {
    let color = color.to_rgb();
    let byte = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
    (byte(color.r), byte(color.g), byte(color.b))
}

/// A theme color as CSS `#rrggbb` (web pages in the app's colors).
pub fn css_color(color: Hsla) -> String {
    let (r, g, b) = rgb_bytes(color);
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// WCAG relative luminance (0 = black, 1 = white).
fn luminance(color: Hsla) -> f32 {
    let color = color.to_rgb();
    let linear = |c: f32| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
}

/// Readable text on a filled `background` (accent, danger, calendar colour): white, or dark
/// on light fills such as orange, green or teal.
pub fn text_on(background: Hsla) -> Hsla {
    if luminance(background) > 0.3 {
        hex(0x18181b)
    } else {
        hex(0xffffff)
    }
}

impl Theme {
    pub fn light(accent: u32) -> Self {
        let accent = hex(accent);
        Self {
            is_dark: false,
            background: hex(0xffffff),
            surface: hex(0xf8f8f9),
            rail: hex(0xf1f1f3),
            elevated: hex(0xffffff),
            border: hex(0xe2e2e6),
            border_subtle: hex(0xececef),
            text: hex(0x18181b),
            text_muted: hex(0x63636b),
            text_subtle: hex(0x82828a),
            hover: hsla(0., 0., 0., 0.045),
            active: hsla(0., 0., 0., 0.08),
            accent,
            accent_hover: Hsla {
                l: (accent.l - 0.06).max(0.),
                ..accent
            },
            accent_subtle: accent.alpha(0.12),
            on_accent: text_on(accent),
            scrim: hsla(0., 0., 0., 0.25),
            danger: hex(0xdc2626),
            success: hex(0x16a34a),
            warning: hex(0xd97706),
        }
    }

    pub fn dark(accent: u32) -> Self {
        let accent = hex(accent);
        Self {
            is_dark: true,
            background: hex(0x1b1c1f),
            surface: hex(0x202124),
            rail: hex(0x17181a),
            elevated: hex(0x26272b),
            border: hex(0x313236),
            border_subtle: hex(0x2a2b2f),
            text: hex(0xe8e8ea),
            text_muted: hex(0xa3a3ab),
            text_subtle: hex(0x7c7c84),
            hover: hsla(0., 0., 1., 0.05),
            active: hsla(0., 0., 1., 0.09),
            accent,
            accent_hover: Hsla {
                l: (accent.l + 0.06).min(1.),
                ..accent
            },
            accent_subtle: accent.alpha(0.2),
            on_accent: text_on(accent),
            scrim: hsla(0., 0., 0., 0.45),
            danger: hex(0xf87171),
            success: hex(0x4ade80),
            warning: hex(0xfbbf24),
        }
    }
}

struct GlobalTheme {
    theme: Theme,
    appearance: Appearance,
    accent: u32,
}

impl Global for GlobalTheme {}

static FALLBACK: LazyLock<Theme> = LazyLock::new(|| Theme::light(DEFAULT_ACCENT));

/// Current theme tokens.
pub fn theme(cx: &App) -> &Theme {
    cx.try_global::<GlobalTheme>()
        .map_or(&*FALLBACK, |global| &global.theme)
}

/// Current user preference.
pub fn appearance(cx: &App) -> Appearance {
    cx.try_global::<GlobalTheme>()
        .map_or(Appearance::System, |global| global.appearance)
}

/// Current accent colour (`0xRRGGBB`).
pub fn accent(cx: &App) -> u32 {
    cx.try_global::<GlobalTheme>()
        .map_or(DEFAULT_ACCENT, |global| global.accent)
}

/// Installs the theme, following the window's system appearance.
pub fn init(window: &Window, cx: &mut App) {
    apply(Appearance::System, DEFAULT_ACCENT, window, cx);
}

/// Re-resolves the theme, e.g. when the system appearance changes.
pub fn refresh(window: &Window, cx: &mut App) {
    apply(appearance(cx), accent(cx), window, cx);
}

pub fn set_appearance(appearance: Appearance, window: &Window, cx: &mut App) {
    apply(appearance, accent(cx), window, cx);
}

pub fn set_accent(accent: u32, window: &Window, cx: &mut App) {
    apply(appearance(cx), accent, window, cx);
}

fn apply(appearance: Appearance, accent: u32, window: &Window, cx: &mut App) {
    let dark = match appearance {
        Appearance::Light => false,
        Appearance::Dark => true,
        Appearance::System => {
            matches!(
                window.appearance(),
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            )
        }
    };
    let theme = if dark {
        Theme::dark(accent)
    } else {
        Theme::light(accent)
    };
    // Each frame starts from the theme background, never white (see vendor/README.md).
    #[cfg(windows)]
    {
        let (r, g, b) = rgb_bytes(theme.background);
        gpui_windows::set_opaque_background(u32::from_be_bytes([0, r, g, b]));
    }
    cx.set_global(GlobalTheme {
        theme,
        appearance,
        accent,
    });
    // Keep gpui-component widgets (inputs, notifications) in the same mode.
    ComponentTheme::change(
        if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        },
        None,
        cx,
    );
    // Mode changes reload the component theme, so the fonts are set again after it.
    let (ui_font, mono_font) = fonts(cx).clone();
    ComponentTheme::update(cx, |component| {
        if let Some(family) = ui_font {
            component.font_family = family;
        }
        if let Some(family) = mono_font {
            component.mono_font_family = family;
        }
    });
    cx.refresh_windows();
}

/// The installed UI and monospace families, looked up once (listing fonts is slow on macOS).
/// gpui panics on a family it cannot find, so only installed names are returned.
fn fonts(cx: &App) -> &'static (Option<SharedString>, Option<SharedString>) {
    static FONTS: OnceLock<(Option<SharedString>, Option<SharedString>)> = OnceLock::new();
    FONTS.get_or_init(|| {
        let installed = cx.text_system().all_font_names();
        let first_installed = |candidates: &[&str]| {
            candidates
                .iter()
                .find(|candidate| installed.iter().any(|name| name == *candidate))
                .map(|family| SharedString::from(*family))
        };
        (first_installed(UI_FONTS), first_installed(MONO_FONTS))
    })
}

#[cfg(test)]
mod tests {
    use super::{hex, text_on};

    #[test]
    fn text_on_picks_the_readable_color() {
        let white = hex(0xffffff);
        let dark = hex(0x18181b);
        assert_eq!(text_on(hex(0x3b82f6)), white); // blue
        assert_eq!(text_on(hex(0xdc2626)), white); // light-mode danger
        assert_eq!(text_on(hex(0x22c55e)), dark); // green
        assert_eq!(text_on(hex(0xf97316)), dark); // orange
    }
}
