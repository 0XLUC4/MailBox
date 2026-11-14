//! Settings › Accounts: mail accounts (order, edit, new password, remove) and calendar /
//! contacts accounts (CalDAV/CardDAV, stored in `<data dir>/dav.json`).
//!
//! The account order is a setting ([`ORDER_KEY`], a JSON list of ids, undoable); other views
//! can list accounts in that order with [`ordered_accounts`].

use std::cell::Cell;
use std::path::PathBuf;

use gpui::component::WindowExt as _;
use gpui::component::input::{Input, InputEvent, InputState};
use gpui::*;
use mailbox_core::settings::{self as core_settings, AccountServers};
use mailbox_core::{Account, Id, Store};
use mailbox_sync::config::{self, AccountConfig, AuthConfig, credentials};
use mailbox_sync::dav::{DavAccount, config as dav_config};
use mailbox_sync::imap_smtp::test_connection;

use super::tr;
use crate::app_state::AppState;
use crate::components::{Banner, Button, Icon, IconButton, IconName};
use crate::dialogs::{self, Confirm, confirm, dialog_frame, field, section_title};
use crate::theme::{RADIUS_FULL, SPACE_1, SPACE_2, SPACE_3, TEXT_LG, TEXT_MD, TEXT_SM, hex, theme};
use crate::{dav_bridge, sync_bridge};

/// Setting holding the account order (JSON list of account ids).
pub const ORDER_KEY: &str = "accounts.order";

/// Accounts in the user's order; accounts missing from the saved order come last, by id.
pub fn ordered_accounts(store: &Store) -> Vec<Account> {
    let mut accounts = store.list_accounts().unwrap_or_default();
    let order: Vec<Id> = core_settings::get_json(store, ORDER_KEY)
        .ok()
        .flatten()
        .unwrap_or_default();
    accounts.sort_by_key(|account| {
        order
            .iter()
            .position(|id| *id == account.id)
            .unwrap_or(usize::MAX)
    });
    accounts
}

/// The ids in `ids` with `id` moved by `step` (-1 up, +1 down), clamped to the list.
fn moved(mut ids: Vec<Id>, id: Id, step: i32) -> Vec<Id> {
    if let Some(index) = ids.iter().position(|other| *other == id) {
        let target = (index as i32 + step).clamp(0, ids.len() as i32 - 1) as usize;
        ids.swap(index, target);
    }
    ids
}

/// Moves an account up or down (undoable).
fn move_account(state: &Entity<AppState>, id: Id, step: i32, cx: &mut App) {
    state.update(cx, |state, cx| {
        let ids = ordered_accounts(state.store())
            .into_iter()
            .map(|account| account.id)
            .collect();
        match core_settings::set_json_command(ORDER_KEY, &moved(ids, id, step)) {
            Ok(command) => {
                state.run(command, cx);
            }
            Err(err) => state.report_error(err, cx),
        }
    });
}

fn dav_path() -> PathBuf {
    sync_bridge::data_dir().join("dav.json")
}

fn auth_of(store: &Store, id: Id) -> AuthConfig {
    core_settings::get_json(store, &config::auth_setting_key(id))
        .ok()
        .flatten()
        .unwrap_or_default()
}

fn avatar(color: u32, icon: IconName, cx: &App) -> Div {
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(32.))
        .rounded(RADIUS_FULL)
        .bg(hex(color))
        .text_color(theme(cx).on_accent)
        .child(Icon::new(icon).size(px(16.)))
}

fn account_row(
    icon: Div,
    title: String,
    detail: String,
    actions: Vec<AnyElement>,
    cx: &App,
) -> AnyElement {
    let t = theme(cx);
    div()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap(SPACE_3)
        .py(SPACE_3)
        .border_b_1()
        .border_color(t.border_subtle)
        .child(icon)
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(160.))
                .child(div().text_size(TEXT_MD).truncate().child(title))
                .child(
                    div()
                        .truncate()
                        .text_size(TEXT_SM)
                        .text_color(t.text_muted)
                        .child(detail),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(SPACE_1)
                .children(actions),
        )
        .into_any_element()
}

/// Rows of the Accounts section.
pub fn rows(state: &Entity<AppState>, cx: &App) -> Vec<AnyElement> {
    let mut rows = vec![
        section_title(tr(cx, "MAIL ACCOUNTS"), cx)
            .pt(SPACE_2)
            .into_any_element(),
    ];
    let store = state.read(cx).store();
    let accounts = ordered_accounts(store);
    let count = accounts.len();
    for (index, account) in accounts.into_iter().enumerate() {
        let servers = AccountServers::load(store, account.id).ok().flatten();
        let oauth = matches!(auth_of(store, account.id), AuthConfig::OAuth2 { .. });
        let detail = match &servers {
            Some(servers) if !servers.provider.is_empty() => {
                format!("{} · {}", account.email, servers.provider)
            }
            Some(servers) => format!("{} · {}", account.email, servers.imap.host),
            None => account.email.clone(),
        };
        let id = account.id;
        let key = id as u64;
        let mut actions = vec![
            {
                let state = state.clone();
                IconButton::new(("account-up", key), IconName::ArrowUp)
                    .tooltip(tr(cx, "Move up"))
                    .disabled(index == 0)
                    .on_click(move |_, _, cx| move_account(&state, id, -1, cx))
                    .into_any_element()
            },
            {
                let state = state.clone();
                IconButton::new(("account-down", key), IconName::ArrowDown)
                    .tooltip(tr(cx, "Move down"))
                    .disabled(index + 1 == count)
                    .on_click(move |_, _, cx| move_account(&state, id, 1, cx))
                    .into_any_element()
            },
        ];
        if !oauth && servers.is_some() {
            let state = state.clone();
            actions.push(
                Button::new(("account-password", key))
                    .icon(IconName::KeyRound)
                    .label(tr(cx, "Password…"))
                    .on_click(move |_, window, cx| {
                        open_password(PasswordTarget::Mail(id), state.clone(), window, cx)
                    })
                    .into_any_element(),
            );
        }
        let edit_state = state.clone();
        actions.push(
            Button::new(("account-edit", key))
                .icon(IconName::Settings)
                .label(tr(cx, "Edit"))
                .on_click(move |_, window, cx| {
                    dialogs::account::open_account_settings(edit_state.clone(), id, window, cx)
                })
                .into_any_element(),
        );
        let email = account.email.clone();
        actions.push(
            IconButton::new(("account-remove", key), IconName::Trash)
                .tooltip(tr(cx, "Remove account…"))
                .on_click(move |_, window, cx| confirm_remove_mail(id, &email, window, cx))
                .into_any_element(),
        );
        rows.push(account_row(
            avatar(account.color, IconName::AtSign, cx),
            account.name,
            detail,
            actions,
            cx,
        ));
    }
    if count == 0 {
        rows.push(muted(tr(cx, "No mail account yet."), cx));
    }
    rows.push(
        div()
            .flex()
            .pt(SPACE_3)
            .pb(SPACE_3)
            .child(
                Button::new("account-add")
                    .primary()
                    .icon(IconName::Plus)
                    .label(tr(cx, "Add account"))
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(crate::actions::OpenAddAccount), cx)
                    }),
            )
            .into_any_element(),
    );

    rows.push(
        section_title(
            tr(cx, "CALENDAR & CONTACTS ACCOUNTS (CALDAV / CARDDAV)"),
            cx,
        )
        .pt(SPACE_3)
        .into_any_element(),
    );
    let dav = dav_config::load_accounts(&dav_path()).unwrap_or_default();
    if dav.is_empty() {
        rows.push(muted(
            tr(
                cx,
                "Sync calendars and contacts with Google, iCloud, Fastmail, Nextcloud or any \
                 CalDAV/CardDAV server.",
            ),
            cx,
        ));
    }
    for (index, account) in dav.into_iter().enumerate() {
        let key = index as u64;
        let mut what = Vec::new();
        if account.calendars {
            what.push(tr(cx, "Calendar"));
        }
        if account.contacts {
            what.push(tr(cx, "People"));
        }
        let detail = format!("{} · {}", account.provider.label(), what.join(", "));
        let mut actions = Vec::new();
        if !account.provider.uses_oauth() {
            let target = PasswordTarget::Dav(account.clone());
            let state = state.clone();
            actions.push(
                Button::new(("dav-password", key))
                    .icon(IconName::KeyRound)
                    .label(tr(cx, "Password…"))
                    .on_click(move |_, window, cx| {
                        open_password(target.clone(), state.clone(), window, cx)
                    })
                    .into_any_element(),
            );
        }
        let removed = account.clone();
        actions.push(
            IconButton::new(("dav-remove", key), IconName::Trash)
                .tooltip(tr(cx, "Remove account…"))
                .on_click(move |_, window, cx| confirm_remove_dav(removed.clone(), window, cx))
                .into_any_element(),
        );
        rows.push(account_row(
            avatar(crate::theme::DEFAULT_ACCENT, IconName::CalendarDays, cx),
            account.email,
            detail,
            actions,
            cx,
        ));
    }
    let state = state.clone();
    rows.push(
        div()
            .flex()
            .pt(SPACE_3)
            .child(
                Button::new("dav-add")
                    .icon(IconName::Plus)
                    .label(tr(cx, "Add calendar & contacts account"))
                    .on_click(move |_, window, cx| {
                        dav_bridge::open_add_dav_account(state.clone(), window, cx)
                    }),
            )
            .into_any_element(),
    );
    rows
}

fn muted(text: &'static str, cx: &App) -> AnyElement {
    div()
        .py(SPACE_2)
        .text_size(TEXT_SM)
        .text_color(theme(cx).text_muted)
        .child(text)
        .into_any_element()
}

fn confirm_remove_mail(id: Id, email: &str, window: &mut Window, cx: &mut App) {
    confirm(
        Confirm {
            title: tr(cx, "Remove {}?").replace("{}", email).into(),
            description: tr(
                cx,
                "Its mail, folders and calendars are deleted from this computer and its \
                 password is removed from the system keychain. Nothing is deleted on the server.",
            )
            .into(),
            ok: tr(cx, "Remove account").into(),
            danger: true,
        },
        move |_, cx| sync_bridge::remove_account(id, cx),
        window,
        cx,
    );
}

fn confirm_remove_dav(account: DavAccount, window: &mut Window, cx: &mut App) {
    confirm(
        Confirm {
            title: tr(cx, "Remove {}?").replace("{}", &account.email).into(),
            description: tr(
                cx,
                "Calendars and contacts stop syncing with this server. Nothing is deleted on \
                 the server.",
            )
            .into(),
            ok: tr(cx, "Remove account").into(),
            danger: true,
        },
        move |_, cx| {
            let path = dav_path();
            let result = dav_config::load_accounts(&path).and_then(|accounts| {
                let kept: Vec<DavAccount> =
                    accounts.into_iter().filter(|a| *a != account).collect();
                dav_config::save_accounts(&path, &kept)
            });
            match result {
                Ok(()) => {
                    // Best effort: the password may never have been stored.
                    let _ = credentials::delete(dav_config::DAV_PASSWORD_SERVICE, account.login());
                    dav_bridge::reload(cx);
                }
                Err(err) => {
                    crate::log::error(format_args!("cannot update {}: {err}", path.display()))
                }
            }
            cx.refresh_windows();
        },
        window,
        cx,
    );
}

// ---------- new password ----------

#[derive(Debug, Clone)]
enum PasswordTarget {
    Mail(Id),
    Dav(DavAccount),
}

/// "Enter the password again": a mail password is tested before it is saved.
struct PasswordDialog {
    state: Entity<AppState>,
    target: PasswordTarget,
    email: String,
    password: Entity<InputState>,
    running: bool,
    error: Option<SharedString>,
    _subscription: Subscription,
}

impl PasswordDialog {
    fn new(
        target: PasswordTarget,
        state: Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let email = match &target {
            PasswordTarget::Mail(id) => state
                .read(cx)
                .store()
                .get::<Account>(*id)
                .ok()
                .flatten()
                .map(|account| account.email)
                .unwrap_or_default(),
            PasswordTarget::Dav(account) => account.login().to_owned(),
        };
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(tr(cx, "Password or app password"))
        });
        let subscription = cx.subscribe_in(&password, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.save(window, cx);
            }
        });
        Self {
            state,
            target,
            email,
            password,
            running: false,
            error: None,
            _subscription: subscription,
        }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let password = self.password.read(cx).value().to_string();
        if password.is_empty() {
            self.error = Some(tr(cx, "Enter your password (or an app password)").into());
            return cx.notify();
        }
        match self.target.clone() {
            PasswordTarget::Dav(account) => {
                match dav_config::set_password(account.login(), &password) {
                    Ok(()) => {
                        dav_bridge::sync_now(cx);
                        window.close_dialog(cx);
                    }
                    Err(err) => self.error = Some(err.to_string().into()),
                }
            }
            PasswordTarget::Mail(id) => {
                let servers = AccountServers::load(self.state.read(cx).store(), id)
                    .ok()
                    .flatten();
                let Some(servers) = servers else {
                    self.error = Some(tr(cx, "This account has no server settings").into());
                    return cx.notify();
                };
                let config =
                    AccountConfig::from_servers(&self.email, &servers, AuthConfig::Password);
                self.running = true;
                self.error = None;
                let task = cx.background_executor().spawn(async move {
                    test_connection(&config, Some(&password)).and_then(|_| {
                        credentials::set_password(&config.email, &password)
                            .map_err(|err| err.to_string())
                    })
                });
                cx.spawn_in(window, async move |this, cx| {
                    let result = task.await;
                    this.update_in(cx, |this, window, cx| {
                        this.running = false;
                        match result {
                            Ok(()) => {
                                sync_bridge::start_account(id, cx);
                                this.state.update(cx, |state, cx| {
                                    state.notify_info(tr(cx, "Password updated: syncing…"), cx)
                                });
                                window.close_dialog(cx);
                            }
                            Err(message) => this.error = Some(message.into()),
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        }
        cx.notify();
    }
}

impl Render for PasswordDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let header = div()
            .flex()
            .flex_col()
            .gap(SPACE_1)
            .child(
                div()
                    .text_size(TEXT_LG)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr(cx, "Enter the password again")),
            )
            .child(
                div()
                    .text_size(TEXT_SM)
                    .text_color(t.text_muted)
                    .child(self.email.clone()),
            );
        let body = field(tr(cx, "Password"), Input::new(&self.password), cx);
        let mut messages = Vec::new();
        if self.running {
            messages.push(
                Banner::info("password-check", tr(cx, "Testing the connection…"))
                    .icon(IconName::Clock)
                    .into_any_element(),
            );
        }
        if let Some(error) = self.error.clone() {
            messages.push(Banner::error("password-error", error).into_any_element());
        }
        let footer = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap(SPACE_2)
            .child(
                Button::new("password-cancel")
                    .label(tr(cx, "Cancel"))
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            )
            .child(
                Button::new("password-save")
                    .primary()
                    .label(tr(cx, "Save"))
                    .disabled(self.running)
                    .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
            );
        dialog_frame("password-body", header, body, messages, footer, window)
    }
}

fn open_password(
    target: PasswordTarget,
    state: Entity<AppState>,
    window: &mut Window,
    cx: &mut App,
) {
    let view = cx.new(|cx| PasswordDialog::new(target, state, window, cx));
    let focus = view.read(cx).password.clone();
    let first = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false) {
            focus.update(cx, |input, cx| input.focus(window, cx));
        }
        dialog.w(px(420.)).child(view.clone())
    });
}

#[cfg(test)]
mod tests {
    use super::moved;

    #[test]
    fn moves_within_bounds() {
        assert_eq!(moved(vec![1, 2, 3], 2, -1), vec![2, 1, 3]);
        assert_eq!(moved(vec![1, 2, 3], 2, 1), vec![1, 3, 2]);
        assert_eq!(moved(vec![1, 2, 3], 1, -1), vec![1, 2, 3]);
        assert_eq!(moved(vec![1, 2, 3], 3, 1), vec![1, 2, 3]);
        assert_eq!(moved(vec![1, 2, 3], 9, 1), vec![1, 2, 3]);
    }
}
