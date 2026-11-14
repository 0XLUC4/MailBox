//! First-run screen, shown by the shell while no account exists: one call to action that opens
//! the "Add account" wizard.

use gpui::*;

use crate::app_state::AppState;
use crate::components::{Button, Icon, IconName};
use crate::dialogs::account::open_add_account;
use crate::theme::{
    ICON_SIZE, RADIUS_XL, SPACE_1, SPACE_2, SPACE_3, SPACE_6, TEXT_2XL, TEXT_MD, TEXT_SM, theme,
};

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
                    .text_size(TEXT_SM)
                    .text_color(t.text_muted)
                    .child(text),
            )
    });
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(t.background)
        .child(
            div()
                .w(px(440.))
                .flex()
                .flex_col()
                .gap(SPACE_6)
                .p(SPACE_6)
                .rounded(RADIUS_XL)
                .border_1()
                .border_color(t.border_subtle)
                .bg(t.elevated)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(48.))
                        .rounded(RADIUS_XL)
                        .bg(t.accent_subtle)
                        .text_color(t.accent)
                        .child(Icon::new(IconName::MailPlus).size(px(24.))),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(SPACE_1)
                        .child(
                            div()
                                .text_size(TEXT_2XL)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Connect your email"),
                        )
                        .child(div().text_size(TEXT_MD).text_color(t.text_muted).child(
                            "Add an account to start. Enter your address and password: \
                                     server settings are detected for you.",
                        )),
                )
                .child(div().flex().flex_col().gap(SPACE_2).children(points))
                .child(
                    div().flex().flex_row().child(
                        Button::new("onboarding-add-account")
                            .primary()
                            .icon(IconName::Plus)
                            .label("Add account")
                            .on_click(move |_, window, cx| {
                                open_add_account(state.clone(), window, cx)
                            }),
                    ),
                ),
        )
}
