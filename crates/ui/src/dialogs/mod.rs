//! Shared dialogs: confirmation, About, keyboard shortcuts, plus the account wizard ([`account`]) and the rules editor ([`rules`]).
//!
//! All use `gpui::component::WindowExt::open_dialog`; persistent entities (inputs) are created
//! before opening because the dialog builder may re-run.

pub mod account;
pub mod rules;

use std::rc::Rc;

use gpui::component::WindowExt as _;
use gpui::component::button::ButtonVariant;
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use crate::actions::ACTIONS;
use crate::components::Icon;
use crate::settings::tr;
use crate::theme::{
    ICON_SIZE_SM, RADIUS_LG, RADIUS_MD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_LG, TEXT_SM,
    TEXT_XS, theme,
};

/// Text of a confirmation dialog.
pub struct Confirm {
    pub title: SharedString,
    pub description: SharedString,
    /// Label of the confirm button ("Delete").
    pub ok: SharedString,
    /// Red confirm button for destructive actions.
    pub danger: bool,
}

/// Asks for confirmation, then runs `on_confirm`.
pub fn confirm(
    confirm: Confirm,
    on_confirm: impl Fn(&mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let on_confirm = Rc::new(on_confirm);
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let on_confirm = on_confirm.clone();
        alert
            .cancel_text(tr(cx, "Cancel"))
            .title(confirm.title.clone())
            .description(confirm.description.clone())
            .show_cancel(true)
            .ok_text(confirm.ok.clone())
            .ok_variant(if confirm.danger {
                ButtonVariant::Danger
            } else {
                ButtonVariant::Primary
            })
            .on_ok(move |_, window, cx| {
                on_confirm(window, cx);
                true
            })
    });
}

/// Product name, version and credits.
pub fn open_about(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |dialog, _, cx| {
        let t = theme(cx).clone();
        dialog.w(px(380.)).child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(SPACE_2)
                .py(SPACE_4)
                .child(crate::titlebar::app_icon(px(64.)).mb(SPACE_2))
                .child(
                    div()
                        .text_size(TEXT_LG)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Mailbox"),
                )
                .child(
                    div()
                        .text_size(TEXT_SM)
                        .text_color(t.text_muted)
                        .child(tr(cx, "Version {}").replace("{}", env!("CARGO_PKG_VERSION"))),
                )
                .child(
                    div()
                        .mt(SPACE_2)
                        .max_w(px(300.))
                        .text_center()
                        .text_size(TEXT_SM)
                        .text_color(t.text_muted)
                        .child(tr(
                            cx,
                            "Mail, calendar, contacts and tasks in one fast native app. \
                             Built in Rust with GPUI.",
                        )),
                )
                .child(
                    div()
                        .mt(SPACE_2)
                        .text_size(TEXT_XS)
                        .text_color(t.text_subtle)
                        .child(tr(cx, "Icons by Lucide (ISC)")),
                ),
        )
    });
}

/// Rows of every action that has a keyboard shortcut.
pub fn shortcut_rows(cx: &App) -> Vec<AnyElement> {
    let t = theme(cx).clone();
    ACTIONS
        .iter()
        .filter_map(|spec| Some((spec, spec.shortcut_text()?)))
        .map(|(spec, keys)| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(SPACE_3)
                .py(SPACE_1)
                .child(
                    Icon::new(spec.icon)
                        .size(ICON_SIZE_SM)
                        .text_color(t.text_muted),
                )
                .child(div().flex_1().text_size(TEXT_SM).child(tr(cx, spec.label)))
                .child(kbd(&keys, cx))
                .into_any_element()
        })
        .collect()
}

/// Keyboard shortcut chip.
pub fn kbd(keys: &str, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .flex_shrink_0()
        .px(SPACE_1)
        .py(px(1.))
        .rounded(RADIUS_MD)
        .border_1()
        .border_color(t.border)
        .bg(t.surface)
        .text_size(TEXT_XS)
        .text_color(t.text_muted)
        .child(keys.to_owned())
}

pub fn open_shortcuts(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |dialog, _, cx| {
        dialog
            .w(px(460.))
            .title(tr(cx, "Keyboard shortcuts"))
            .child(
                div()
                    .id("shortcuts")
                    .max_h(px(420.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(shortcut_rows(cx)),
            )
    });
}

/// Section title used inside dialogs and settings.
pub fn section_title(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_size(TEXT_XS)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme(cx).text_subtle)
        .child(text.into())
}

/// Labelled form field: caption above the control.
pub fn field(label: impl Into<SharedString>, control: impl IntoElement, cx: &App) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(SPACE_1)
        .child(
            div()
                .text_size(TEXT_SM)
                .text_color(theme(cx).text_muted)
                .child(label.into()),
        )
        .child(control)
}

/// Bordered card container.
pub fn card(cx: &App) -> Div {
    let t = theme(cx);
    div()
        .flex()
        .flex_col()
        .gap(SPACE_3)
        .p(SPACE_3)
        .rounded(RADIUS_LG)
        .border_1()
        .border_color(t.border_subtle)
        .bg(t.surface)
}

/// Tallest a dialog's own content may be so the card fits the window: the dialog surface
/// keeps a margin around it and adds its padding.
pub fn dialog_max_height(window: &Window) -> Pixels {
    (window.viewport_size().height - px(112.)).max(px(200.))
}

/// Dialog layout for views that draw their own header and footer: only `body` scrolls, so
/// the header, the messages and the footer buttons stay visible and the card never grows
/// taller than the window.
pub fn dialog_frame(
    id: impl Into<ElementId>,
    header: impl IntoElement,
    body: impl IntoElement,
    messages: Vec<AnyElement>,
    footer: impl IntoElement,
    window: &Window,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(SPACE_4)
        .w_full()
        .min_w_0()
        .max_h(dialog_max_height(window))
        .child(div().flex_shrink_0().child(header))
        .child(
            div()
                .id(id)
                .min_h_0()
                .min_w_0()
                .overflow_y_scroll()
                .child(body),
        )
        .when(!messages.is_empty(), |el| {
            el.child(
                div()
                    .flex()
                    .flex_col()
                    .flex_shrink_0()
                    .gap(SPACE_2)
                    .min_w_0()
                    .children(messages),
            )
        })
        .child(div().flex_shrink_0().child(footer))
}
