//! First-run screen, shown by the shell while no account exists: one call to action that opens
//! the "Add account" wizard, a link to calendar/contacts-only accounts and a language switch.

use gpui::*;
use mailbox_core::settings::Language;

use crate::app_state::AppState;
use crate::components::{Button, Icon, IconName};
use crate::dav_bridge::open_add_dav_account;
use crate::dialogs::account::open_add_account;
use crate::settings::controls::segmented;
use crate::settings::{prefs, tr, update_settings};
use crate::theme::{
    DIALOG_MD, ICON_SIZE, RADIUS_XL, SPACE_1, SPACE_2, SPACE_3, SPACE_6, TEXT_2XL, TEXT_MD,
    TEXT_SM, theme,
};
use crate::titlebar::app_icon;

/// What connecting an account gives, as (icon, text) rows.
const POINTS: [(IconName, &str); 3] = [
    (
        IconName::Inbox,
        "Mail syncs over IMAP and SMTP with any provider: Gmail, Outlook, iCloud, your own domain.",
    ),
    (
        IconName::ShieldCheck,
        "Your password stays in the system keychain; mail is stored on this computer only.",
    ),
    (
        IconName::WifiOff,
        "Read, sort and write offline: changes are sent when you are back online.",
    ),
];

/// The onboarding page (fills the content area).
pub fn onboarding(state: Entity<AppState>, cx: &App) -> Div {
    let t = theme(cx).clone();
    let points = POINTS.map(|(icon, text)| {
        div()
            .flex()
            .flex_row()
            .items_start()
            .gap(SPACE_3)
            .child(
                Icon::new(icon)
                    .size(ICON_SIZE)
                    .text_color(t.accent)
                    .mt(px(1.)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .text_size(TEXT_SM)
                    .text_color(t.text_muted)
                    .child(tr(cx, text)),
            )
    });
    let language_state = state.clone();
    let language = segmented(
        "onboarding-language",
        &[("English", Language::En), ("Français", Language::Fr)],
        prefs(cx).language,
        move |value, _, cx| update_settings(&language_state, cx, |s| s.language = value),
        cx,
    );
    let dav_state = state.clone();
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(t.background)
        .child(
            div()
                .w(DIALOG_MD)
                .flex()
                .flex_col()
                .gap(SPACE_6)
                .p(SPACE_6)
                .rounded(RADIUS_XL)
                .border_1()
                .border_color(t.border_subtle)
                .bg(t.elevated)
                .child(app_icon(px(56.)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(SPACE_1)
                        .child(
                            div()
                                .text_size(TEXT_2XL)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(tr(cx, "Connect your email")),
                        )
                        .child(div().text_size(TEXT_MD).text_color(t.text_muted).child(tr(
                            cx,
                            "Add an account to start. Enter your address and password: server \
                             settings are detected for you.",
                        ))),
                )
                .child(div().flex().flex_col().gap(SPACE_2).children(points))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap(SPACE_2)
                        .child(
                            Button::new("onboarding-add-account")
                                .primary()
                                .icon(IconName::Plus)
                                .label(tr(cx, "Add account"))
                                .on_click(move |_, window, cx| {
                                    open_add_account(state.clone(), window, cx)
                                }),
                        )
                        .child(
                            Button::new("onboarding-add-dav")
                                .ghost()
                                .icon(IconName::CalendarDays)
                                .label(tr(cx, "Calendar & contacts only"))
                                .on_click(move |_, window, cx| {
                                    open_add_dav_account(dav_state.clone(), window, cx)
                                }),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(SPACE_2)
                        .pt(SPACE_3)
                        .border_t_1()
                        .border_color(t.border_subtle)
                        .child(
                            Icon::new(IconName::Globe)
                                .size(ICON_SIZE)
                                .text_color(t.text_muted),
                        )
                        .child(
                            div()
                                .flex_1()
                                .text_size(TEXT_SM)
                                .text_color(t.text_muted)
                                .child(tr(cx, "Language")),
                        )
                        .child(language),
                ),
        )
}
