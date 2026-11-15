//! Client-side window chrome shared by every window.
//!
//! The native title bar is hidden ([`window_options`]); views draw their own drag
//! area ([`drag_area`]) and caption buttons ([`WindowControls`]). On Windows the OS
//! hit-tests those regions (`WindowControlArea`), so snap layouts, double-click
//! maximize, Aero drag and the native resize frame keep working. Secondary windows
//! get a slim bar for free through [`open_window`].
//!
//! macOS keeps its native traffic lights (no caption buttons of ours): they are moved to the
//! vertical middle of the bar ([`traffic_lights_in`]) and views leave
//! [`MACOS_TRAFFIC_LIGHTS_WIDTH`] free on the left of it.

use std::sync::{Arc, LazyLock};

use gpui::prelude::FluentBuilder as _;
use gpui::*;

use crate::components::{Icon, IconName};
use crate::theme::{ICON_SIZE_SM, ICON_SIZE_XS, text_on, theme};

/// Height of the slim bar drawn above secondary windows.
pub const TITLE_BAR_HEIGHT: Pixels = px(36.);
/// Caption button width (Windows 11 uses 46px).
const CAPTION_WIDTH: Pixels = px(46.);
/// Room left for the macOS traffic lights (12px inset, three 14px buttons 6px apart, 10px gap).
pub const MACOS_TRAFFIC_LIGHTS_WIDTH: Pixels = px(76.);
/// Left inset of the macOS close button.
const TRAFFIC_LIGHTS_INSET: Pixels = px(12.);
/// Height of a macOS traffic light button.
const TRAFFIC_LIGHT_SIZE: Pixels = px(14.);

const IS_MACOS: bool = cfg!(target_os = "macos");
const IS_WINDOWS: bool = cfg!(target_os = "windows");

/// The app icon (`assets/icon.svg`, rendered by `cargo run -p mailbox --example make_icon`).
pub const APP_ICON_PNG: &[u8] = include_bytes!("../../../assets/png/icon-128.png");

static APP_ICON: LazyLock<Arc<Image>> =
    LazyLock::new(|| Arc::new(Image::from_bytes(ImageFormat::Png, APP_ICON_PNG.to_vec())));

/// The app icon as an element, `size` square.
pub fn app_icon(size: Pixels) -> Img {
    img(APP_ICON.clone()).size(size).flex_shrink_0()
}

/// Base options for any app window: no native title bar, client decorations.
/// `title` is only used by the taskbar / Alt-Tab, it is never drawn.
pub fn window_options(title: impl Into<SharedString>) -> WindowOptions {
    WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            appears_transparent: true,
            traffic_light_position: Some(traffic_lights_in(TITLE_BAR_HEIGHT)),
        }),
        window_decorations: Some(WindowDecorations::Client),
        app_owns_titlebar_drag: true,
        ..Default::default()
    }
}

/// [`window_options`] for a window whose own title bar is `bar_height` tall (the main window).
pub fn window_options_with_bar(
    title: impl Into<SharedString>,
    bar_height: Pixels,
) -> WindowOptions {
    let mut options = window_options(title);
    if let Some(titlebar) = options.titlebar.as_mut() {
        titlebar.traffic_light_position = Some(traffic_lights_in(bar_height));
    }
    options
}

/// macOS traffic lights position: vertically centered in a bar `bar_height` tall.
fn traffic_lights_in(bar_height: Pixels) -> Point<Pixels> {
    point(TRAFFIC_LIGHTS_INSET, (bar_height - TRAFFIC_LIGHT_SIZE) / 2.)
}

/// Opens a secondary window whose content sits under a slim, empty title bar
/// (drag area + caption buttons). Use [`window_options`] as the base of `options`.
pub fn open_window<V: Render>(
    options: WindowOptions,
    cx: &mut App,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> Result<AnyWindowHandle> {
    gpui::open_window(options, cx, |window, cx| {
        let content: AnyView = build(window, cx).into();
        cx.new(|_| WindowFrame { content })
    })
    .map(|(handle, _)| handle)
}

/// Makes `element` move the window when dragged and maximize on double click.
pub fn drag_area<E: InteractiveElement>(element: E) -> E {
    if cfg!(target_os = "linux") {
        element.on_mouse_down(MouseButton::Left, |event, window, _| {
            if event.click_count >= 2 {
                window.zoom_window();
            } else {
                window.start_window_move();
            }
        })
    } else if IS_MACOS {
        element.on_mouse_down(MouseButton::Left, |event, window, _| {
            if event.click_count >= 2 {
                window.titlebar_double_click();
            } else {
                window.start_window_move();
            }
        })
    } else {
        element.window_control_area(WindowControlArea::Drag)
    }
}

/// `content` as a window root. Linux: client decorations get no frame from the system, so this
/// adds the resize edges and a 1px border (no shadow: the window is opaque).
pub fn resizable_frame(content: impl IntoElement) -> AnyElement {
    if cfg!(target_os = "linux") {
        gpui::component::window_border()
            .shadow_size(px(0.))
            .child(content)
            .into_any_element()
    } else {
        content.into_any_element()
    }
}

/// Root of a secondary window: slim bar on top, content below.
struct WindowFrame {
    content: AnyView,
}

impl Render for WindowFrame {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        resizable_frame(
            div()
                .flex()
                .flex_col()
                .size_full()
                .bg(t.background)
                .text_color(t.text)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_shrink_0()
                        .h(TITLE_BAR_HEIGHT)
                        .bg(t.surface)
                        .border_b_1()
                        .border_color(t.border_subtle)
                        .items_center()
                        .when(IS_MACOS, |el| el.pl(MACOS_TRAFFIC_LIGHTS_WIDTH))
                        .child(drag_area(div().id("title-bar-drag").flex_1().h_full()))
                        .child(WindowControls),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h(px(0.))
                        .child(self.content.clone()),
                ),
        )
    }
}

/// Minimize / maximize-restore / close, full height of the parent bar.
/// Renders nothing on macOS (native traffic lights).
#[derive(IntoElement)]
pub struct WindowControls;

impl RenderOnce for WindowControls {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let maximized = window.is_maximized();
        div()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .h_full()
            .when(!IS_MACOS, |el| {
                el.child(CaptionButton::Minimize)
                    .child(if maximized {
                        CaptionButton::Restore
                    } else {
                        CaptionButton::Maximize
                    })
                    .child(CaptionButton::Close)
            })
    }
}

#[derive(IntoElement, Clone, Copy, PartialEq)]
enum CaptionButton {
    Minimize,
    Maximize,
    Restore,
    Close,
}

impl CaptionButton {
    fn icon(self) -> IconName {
        match self {
            Self::Minimize => IconName::Minus,
            Self::Maximize => IconName::Square,
            Self::Restore => IconName::Copy,
            Self::Close => IconName::X,
        }
    }

    fn area(self) -> WindowControlArea {
        match self {
            Self::Minimize => WindowControlArea::Min,
            Self::Maximize | Self::Restore => WindowControlArea::Max,
            Self::Close => WindowControlArea::Close,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Minimize => "caption-minimize",
            Self::Maximize => "caption-maximize",
            Self::Restore => "caption-restore",
            Self::Close => "caption-close",
        }
    }
}

impl RenderOnce for CaptionButton {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx).clone();
        let (hover_bg, hover_fg, active_bg) = if self == Self::Close {
            (t.danger, text_on(t.danger), t.danger.opacity(0.85))
        } else {
            (t.hover, t.text, t.active)
        };
        // Restore's two overlapping squares read better slightly smaller.
        let icon_size = if self == Self::Restore {
            ICON_SIZE_XS
        } else {
            ICON_SIZE_SM
        };

        div()
            .id(self.id())
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .w(CAPTION_WIDTH)
            .h_full()
            .text_color(t.text_muted)
            .hover(move |style| style.bg(hover_bg).text_color(hover_fg))
            .active(move |style| style.bg(active_bg).text_color(hover_fg))
            .child(Icon::new(self.icon()).size(icon_size))
            // Windows handles the click itself through the hit-test code.
            .when(IS_WINDOWS, |el| el.window_control_area(self.area()))
            .when(!IS_WINDOWS, |el| {
                el.on_mouse_down(MouseButton::Left, |_, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                })
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    match self {
                        Self::Minimize => window.minimize_window(),
                        Self::Maximize | Self::Restore => window.zoom_window(),
                        Self::Close => window.remove_window(),
                    }
                })
            })
    }
}
