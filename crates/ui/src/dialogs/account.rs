//! "Add account" wizard (email -> detected or manual IMAP/SMTP settings) and account settings.
//!
//! Passwords are not asked here: they never go to the Mailbox database (the sync backend
//! requests them and stores them in the OS keychain).

use std::cell::Cell;

use gpui::component::WindowExt as _;
use gpui::component::input::{Input, InputEvent, InputState};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::settings::{
    AccountServers, Provider, Security, ServerEndpoint, detect_provider, email_domain,
    suggest_servers,
};
use mailbox_core::{Account, Calendar, Command, Folder, FolderRole, Id};

use super::{card, field, section_title};
use crate::app_state::{AppState, Module};
use crate::components::{Button, Icon, IconName};
use crate::settings::controls::{segmented, swatches};
use crate::settings::tr;
use crate::theme::{
    ACCENTS, DEFAULT_ACCENT, ICON_SIZE, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_LG, TEXT_SM, theme,
};

/// Folders created with every new account (name, role).
const DEFAULT_FOLDERS: [(&str, FolderRole); 6] = [
    ("Inbox", FolderRole::Inbox),
    ("Drafts", FolderRole::Drafts),
    ("Sent", FolderRole::Sent),
    ("Archive", FolderRole::Archive),
    ("Junk", FolderRole::Junk),
    ("Trash", FolderRole::Trash),
];

const SECURITY: [(&str, Security); 3] = [
    ("SSL/TLS", Security::Tls),
    ("STARTTLS", Security::StartTls),
    ("None", Security::None),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Identity,
    Servers,
}

/// Inputs of one server endpoint.
struct EndpointInputs {
    host: Entity<InputState>,
    port: Entity<InputState>,
    security: Security,
}

impl EndpointInputs {
    fn new(endpoint: &ServerEndpoint, window: &mut Window, cx: &mut App) -> Self {
        Self {
            host: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("mail.example.com")
                    .default_value(endpoint.host.clone())
            }),
            port: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("993")
                    .default_value(if endpoint.port == 0 {
                        String::new()
                    } else {
                        endpoint.port.to_string()
                    })
            }),
            security: endpoint.security,
        }
    }

    fn fill(&mut self, endpoint: &ServerEndpoint, window: &mut Window, cx: &mut App) {
        self.host.update(cx, |input, cx| {
            input.set_value(endpoint.host.clone(), window, cx)
        });
        self.port.update(cx, |input, cx| {
            input.set_value(endpoint.port.to_string(), window, cx)
        });
        self.security = endpoint.security;
    }

    fn read(&self, what: &str, cx: &App) -> Result<ServerEndpoint, String> {
        let host = self.host.read(cx).value().trim().to_owned();
        if host.is_empty() || host.contains(char::is_whitespace) {
            return Err(format!("Enter the {what} server name"));
        }
        let port = self
            .port
            .read(cx)
            .value()
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|port| *port > 0)
            .ok_or_else(|| format!("Enter a valid {what} port"))?;
        Ok(ServerEndpoint::new(host, port, self.security))
    }
}

pub struct AccountDialog {
    state: Entity<AppState>,
    /// `None` while adding a new account.
    account_id: Option<Id>,
    step: Step,
    name: Entity<InputState>,
    email: Entity<InputState>,
    username: Entity<InputState>,
    imap: EndpointInputs,
    smtp: EndpointInputs,
    color: u32,
    provider: Option<&'static Provider>,
    error: Option<SharedString>,
    _subscription: Subscription,
}

impl AccountDialog {
    fn new(
        state: Entity<AppState>,
        account_id: Option<Id>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let store = state.read(cx).store();
        let account = account_id.and_then(|id| store.get::<Account>(id).ok().flatten());
        let servers = account_id
            .and_then(|id| AccountServers::load(store, id).ok().flatten())
            .unwrap_or_default();
        let (name, email, color) = account
            .map(|account| (account.name, account.email, account.color))
            .unwrap_or_else(|| (String::new(), String::new(), DEFAULT_ACCENT));
        let provider = detect_provider(&email);

        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Work, Personal...")
                .default_value(name)
        });
        let email = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("you@example.com")
                .default_value(email)
        });
        let username = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Usually your email address")
                .default_value(servers.username.clone())
        });
        let imap = EndpointInputs::new(&servers.imap, window, cx);
        let smtp = EndpointInputs::new(&servers.smtp, window, cx);
        let subscription = cx.subscribe_in(
            &email,
            window,
            |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.provider = detect_provider(&input.read(cx).value());
                    this.error = None;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } if this.step == Step::Identity => {
                    this.next(window, cx)
                }
                _ => {}
            },
        );
        Self {
            state,
            account_id,
            step: if account_id.is_some() {
                Step::Servers
            } else {
                Step::Identity
            },
            name,
            email,
            username,
            imap,
            smtp,
            color,
            provider,
            error: None,
            _subscription: subscription,
        }
    }

    fn email_value(&self, cx: &App) -> String {
        self.email.read(cx).value().trim().to_owned()
    }

    fn fail(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.error = Some(message.into());
        cx.notify();
    }

    /// Identity step -> servers step, pre-filled from the provider table.
    fn next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let email = self.email_value(cx);
        let Some(servers) = suggest_servers(&email) else {
            return self.fail("Enter a valid email address", cx);
        };
        self.provider = detect_provider(&email);
        self.imap.fill(&servers.imap, window, cx);
        self.smtp.fill(&servers.smtp, window, cx);
        self.username.update(cx, |input, cx| {
            input.set_value(servers.username, window, cx)
        });
        if self.name.read(cx).value().trim().is_empty() {
            let name = self.provider.map_or_else(
                || email_domain(&email).unwrap_or_default(),
                |p| p.name.to_owned(),
            );
            self.name
                .update(cx, |input, cx| input.set_value(name, window, cx));
        }
        self.step = Step::Servers;
        self.error = None;
        cx.notify();
    }

    fn servers(&self, cx: &App) -> Result<AccountServers, String> {
        let username = self.username.read(cx).value().trim().to_owned();
        Ok(AccountServers {
            provider: self.provider.map(|p| p.name.to_owned()).unwrap_or_default(),
            username: if username.is_empty() {
                self.email_value(cx)
            } else {
                username
            },
            imap: self.imap.read("incoming", cx)?,
            smtp: self.smtp.read("outgoing", cx)?,
        })
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let email = self.email_value(cx);
        if email_domain(&email).is_none() {
            return self.fail("Enter a valid email address", cx);
        }
        let servers = match self.servers(cx) {
            Ok(servers) => servers,
            Err(message) => return self.fail(message, cx),
        };
        let name = match self.name.read(cx).value().trim() {
            "" => email.clone(),
            name => name.to_owned(),
        };
        let account = Account {
            id: self.account_id.unwrap_or(0),
            name,
            email,
            color: self.color,
        };
        let saved = match self.account_id {
            Some(id) => save_account(&self.state, id, account, servers, cx),
            None => create_account(&self.state, account, servers, cx),
        };
        if saved {
            window.close_dialog(cx);
        }
    }

    fn render_endpoint(&self, title: &'static str, which: Which, cx: &mut Context<Self>) -> Div {
        let inputs = match which {
            Which::Imap => &self.imap,
            Which::Smtp => &self.smtp,
        };
        let view = cx.entity().downgrade();
        let security = segmented(
            title,
            &SECURITY,
            inputs.security,
            move |security, _, cx| {
                if let Some(view) = view.upgrade() {
                    view.update(cx, |this, cx| {
                        match which {
                            Which::Imap => this.imap.security = security,
                            Which::Smtp => this.smtp.security = security,
                        }
                        cx.notify();
                    });
                }
            },
            cx,
        );
        div()
            .flex()
            .flex_col()
            .gap(SPACE_2)
            .child(section_title(title, cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(SPACE_2)
                    .child(
                        div()
                            .flex_1()
                            .child(field("Server", Input::new(&inputs.host), cx)),
                    )
                    .child(
                        div()
                            .w(px(88.))
                            .child(field("Port", Input::new(&inputs.port), cx)),
                    ),
            )
            .child(field("Security", security, cx))
    }

    fn render_identity(&self, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let view = cx.entity().downgrade();
        let detected = self.provider.map(|provider| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(SPACE_1)
                .text_size(TEXT_SM)
                .text_color(t.success)
                .child(Icon::new(IconName::ShieldCheck).size(px(14.)))
                .child(format!(
                    "{} detected: settings will be filled in",
                    provider.name
                ))
        });
        div()
            .flex()
            .flex_col()
            .gap(SPACE_3)
            .child(field("Email address", Input::new(&self.email), cx))
            .children(detected)
            .child(field("Account name", Input::new(&self.name), cx))
            .child(field(
                "Colour",
                swatches(
                    "account-color",
                    ACCENTS,
                    self.color,
                    move |color, _, cx| {
                        if let Some(view) = view.upgrade() {
                            view.update(cx, |this, cx| {
                                this.color = color;
                                cx.notify();
                            });
                        }
                    },
                    cx,
                ),
                cx,
            ))
    }

    fn render_servers(&self, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let banner = match (self.account_id, self.provider) {
            (Some(_), _) => None,
            (None, Some(provider)) => Some((
                IconName::ShieldCheck,
                t.success,
                format!(
                    "{} settings detected. Review them, then add the account.",
                    provider.name
                ),
            )),
            (None, None) => Some((
                IconName::CircleAlert,
                t.warning,
                "Provider not recognised: we guessed the usual settings. Check them with your \
                 provider."
                    .to_owned(),
            )),
        };
        div()
            .flex()
            .flex_col()
            .gap(SPACE_3)
            .when_some(banner, |el, (icon, color, text)| {
                el.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(SPACE_2)
                        .p(SPACE_2)
                        .rounded(px(6.))
                        .bg(color.alpha(0.1))
                        .text_size(TEXT_SM)
                        .child(Icon::new(icon).size(ICON_SIZE).text_color(color))
                        .child(div().flex_1().child(text)),
                )
            })
            .when(self.account_id.is_some(), |el| {
                el.child(self.render_identity(cx))
            })
            .child(
                card(cx)
                    .child(self.render_endpoint("INCOMING MAIL (IMAP)", Which::Imap, cx))
                    .child(self.render_endpoint("OUTGOING MAIL (SMTP)", Which::Smtp, cx)),
            )
            .child(field("Username", Input::new(&self.username), cx))
            .child(
                div()
                    .text_size(TEXT_SM)
                    .text_color(t.text_subtle)
                    .child("Passwords are never stored in the Mailbox database."),
            )
    }
}

#[derive(Debug, Clone, Copy)]
enum Which {
    Imap,
    Smtp,
}

impl Render for AccountDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let editing = self.account_id.is_some();
        let (heading, subtitle) = match (editing, self.step) {
            (true, _) => ("Account settings", "Name, colour and server settings."),
            (false, Step::Identity) => (
                "Add an account",
                "Enter your email address: Mailbox detects the server settings of Gmail, \
                 Outlook, iCloud, Yahoo, Fastmail, OVH and more.",
            ),
            (false, Step::Servers) => ("Server settings", "Step 2 of 2"),
        };
        let body = match (editing, self.step) {
            (false, Step::Identity) => self.render_identity(cx),
            _ => self.render_servers(cx),
        };

        let back = (!editing && self.step == Step::Servers).then(|| {
            Button::new("account-back")
                .ghost()
                .icon(IconName::ArrowLeft)
                .label("Back")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.step = Step::Identity;
                    this.error = None;
                    cx.notify();
                }))
        });
        let primary_label = match (editing, self.step) {
            (true, _) => tr(cx, "Save"),
            (false, Step::Identity) => "Continue",
            (false, Step::Servers) => tr(cx, "Add account"),
        };
        let primary = Button::new("account-primary")
            .primary()
            .label(primary_label)
            .on_click(cx.listener(|this, _, window, cx| {
                if this.account_id.is_none() && this.step == Step::Identity {
                    this.next(window, cx);
                } else {
                    this.submit(window, cx);
                }
            }));

        div()
            .flex()
            .flex_col()
            .gap(SPACE_4)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(SPACE_1)
                    .child(
                        div()
                            .text_size(TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(heading),
                    )
                    .child(
                        div()
                            .text_size(TEXT_SM)
                            .text_color(t.text_muted)
                            .child(subtitle),
                    ),
            )
            .child(
                div()
                    .id("account-body")
                    .max_h(px(460.))
                    .overflow_y_scroll()
                    .child(body),
            )
            .when_some(self.error.clone(), |el, error| {
                el.child(div().text_size(TEXT_SM).text_color(t.danger).child(error))
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(SPACE_2)
                    .children(back)
                    .child(div().flex_1())
                    .child(
                        Button::new("account-cancel")
                            .label(tr(cx, "Cancel"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(primary),
            )
    }
}

/// Creates the account, its default folders, a calendar and its server settings.
fn create_account(
    state: &Entity<AppState>,
    account: Account,
    servers: AccountServers,
    cx: &mut App,
) -> bool {
    state.update(cx, |state, cx| {
        let email = account.email.clone();
        let color = account.color;
        if !state.run(Command::SaveAccount(Box::new(account)), cx) {
            return false;
        }
        let created = state.store().list_accounts().ok().and_then(|accounts| {
            accounts
                .into_iter()
                .filter(|account| account.email == email)
                .map(|account| account.id)
                .max()
        });
        let Some(account_id) = created else {
            state.report_error("The new account could not be found", cx);
            return false;
        };
        let mut commands: Vec<Command> = DEFAULT_FOLDERS
            .iter()
            .enumerate()
            .map(|(position, (name, role))| {
                Command::SaveFolder(Box::new(Folder {
                    id: 0,
                    account_id,
                    parent_id: None,
                    name: (*name).to_owned(),
                    role: *role,
                    position: position as i64,
                }))
            })
            .collect();
        commands.push(Command::SaveCalendar(Box::new(Calendar {
            id: 0,
            account_id,
            name: "Calendar".into(),
            color,
        })));
        match servers.command(account_id) {
            Ok(command) => commands.push(command),
            Err(err) => {
                state.report_error(err, cx);
                return false;
            }
        }
        if !state.run(Command::Batch(commands), cx) {
            return false;
        }
        let inbox = state.store().folder_by_role(account_id, FolderRole::Inbox);
        if let Ok(Some(inbox)) = inbox {
            state.set_module(Module::Mail, cx);
            state.select_folder(Some(inbox.id), cx);
        }
        state.notify_info(format!("Account {email} added"), cx);
        true
    })
}

/// Updates an existing account and its server settings in one undoable step.
fn save_account(
    state: &Entity<AppState>,
    account_id: Id,
    account: Account,
    servers: AccountServers,
    cx: &mut App,
) -> bool {
    state.update(cx, |state, cx| {
        let servers = match servers.command(account_id) {
            Ok(command) => command,
            Err(err) => {
                state.report_error(err, cx);
                return false;
            }
        };
        state.run(
            Command::Batch(vec![Command::SaveAccount(Box::new(account)), servers]),
            cx,
        )
    })
}

fn open(view: Entity<AccountDialog>, window: &mut Window, cx: &mut App) {
    let focus = view.read(cx).email.clone();
    let first = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false) {
            focus.update(cx, |input, cx| input.focus(window, cx));
        }
        dialog.w(px(540.)).child(view.clone())
    });
}

/// Opens the "Add account" wizard.
pub fn open_add_account(state: Entity<AppState>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| AccountDialog::new(state, None, window, cx));
    open(view, window, cx);
}

/// Opens the settings of an existing account.
pub fn open_account_settings(
    state: Entity<AppState>,
    account_id: Id,
    window: &mut Window,
    cx: &mut App,
) {
    let view = cx.new(|cx| AccountDialog::new(state, Some(account_id), window, cx));
    open(view, window, cx);
}
