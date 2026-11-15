//! Design tokens: colours (light/dark + accent), spacing, radii, font sizes.
//!
//! Read with `let t = theme(cx).clone();` at the top of `render`, then use `t.text`, `t.border`...
//! The appearance follows the system unless the user picks Light/Dark in settings.

use std::sync::LazyLock;

use gpui::component::{Theme as ComponentTheme, ThemeMode};
use gpui::{App, Global, Hsla, Pixels, Window, WindowAppearance, hsla, px, rgb};

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
pub const TEXT_XS: Pixels = px(11.);
pub const TEXT_SM: Pixels = px(12.);
pub const TEXT_MD: Pixels = px(13.);
pub const TEXT_LG: Pixels = px(15.);
pub const TEXT_XL: Pixels = px(18.);
pub const TEXT_2XL: Pixels = px(22.);

// ---------- layout ----------
pub const RAIL_WIDTH: Pixels = px(56.);
pub const TOP_BAR_HEIGHT: Pixels = px(48.);
pub const ICON_SIZE: Pixels = px(16.);
pub const ICON_SIZE_SM: Pixels = px(14.);

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
    /// Text on an accent background.
    pub on_accent: Hsla,
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
            text_subtle: hex(0x9c9ca4),
            hover: hsla(0., 0., 0., 0.045),
            active: hsla(0., 0., 0., 0.08),
            accent,
            accent_hover: Hsla {
                l: (accent.l - 0.06).max(0.),
                ..accent
            },
            accent_subtle: accent.alpha(0.12),
            on_accent: hex(0xffffff),
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
            text_subtle: hex(0x6e6e76),
            hover: hsla(0., 0., 1., 0.05),
            active: hsla(0., 0., 1., 0.09),
            accent,
            accent_hover: Hsla {
                l: (accent.l + 0.06).min(1.),
                ..accent
            },
            accent_subtle: accent.alpha(0.2),
            on_accent: hex(0xffffff),
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
    cx.refresh_windows();
}
