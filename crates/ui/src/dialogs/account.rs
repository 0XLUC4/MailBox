//! "Add account" wizard and account settings.
//!
//! Adding: email + password (or app password) → server settings from the built-in provider
//! table, else looked up online ([`config::discover`]: only the domain is sent) → "Test
//! connection" (IMAP login + folder list, SMTP EHLO + AUTH) → the password goes to the OS
//! keychain (never the Mailbox database), the account is created with the server's folders and
//! sync starts at once. Gmail / Outlook can sign in with OAuth when a client id is configured
//! (field below, or `MAILBOX_GOOGLE_CLIENT_ID` / `MAILBOX_MS_CLIENT_ID`).
//!
//! Settings: edit name, colour and servers, enter a new password, test, remove the account.

use std::cell::Cell;

use gpui::component::WindowExt as _;
use gpui::component::input::{Input, InputEvent, InputState};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::settings::{
    self as core_settings, AccountServers, Provider, Security, ServerEndpoint, detect_provider,
    email_domain, suggest_servers,
};
use mailbox_core::{Account, Calendar, Command, Folder, FolderRole, Id, command};
use mailbox_sync::RemoteFolder;
use mailbox_sync::config::{self, AccountConfig, AuthConfig, OAuthProvider, credentials};
use mailbox_sync::imap_smtp::test_connection;
use mailbox_sync::oauth::OAuthSettings;

use super::{Confirm, card, confirm, dialog_frame, field, section_title};
use crate::app_state::{AppState, Module};
use crate::components::{Banner, Button, IconName};
use crate::settings::controls::{segmented, swatches};
use crate::settings::tr;
use crate::sync_bridge;
use crate::theme::{ACCENTS, DEFAULT_ACCENT, SPACE_1, SPACE_2, SPACE_3, TEXT_LG, TEXT_SM, theme};

/// Server source shown when the provider was not found anywhere.
const GUESSED: &str =
    "Provider not recognised: we guessed the usual settings. Check them with your provider.";

const SECURITY: [(&str, Security); 3] = [
    ("SSL/TLS", Security::Tls),
    ("STARTTLS", Security::StartTls),
    ("None", Security::None),
];

/// Setting key of the OAuth client id entered for a provider.
fn client_id_key(provider: OAuthProvider) -> &'static str {
    match provider {
        OAuthProvider::Google => "oauth.google.client_id",
        OAuthProvider::Microsoft => "oauth.microsoft.client_id",
    }
}

/// Why a provider needs an app password, and the page where it is created.
fn app_password_help(provider: &str) -> Option<(&'static str, &'static str)> {
    Some(match provider {
        "Gmail" => (
            "With two-step verification on, Gmail needs an app password instead of your Google password.",
            "https://myaccount.google.com/apppasswords",
        ),
        "Outlook.com" => (
            "With two-step verification on, Outlook needs an app password instead of your Microsoft password.",
            "https://account.live.com/proofs/AppPassword",
        ),
        "iCloud" => (
            "iCloud Mail needs an app-specific password, created in your Apple Account.",
            "https://account.apple.com/account/manage",
        ),
        "Yahoo" => (
            "Yahoo Mail needs an app password, created in your account security settings.",
            "https://login.yahoo.com/account/security/app-passwords",
        ),
        "AOL" => (
            "AOL Mail needs an app password, created in your account security settings.",
            "https://login.aol.com/account/security/app-passwords",
        ),
        _ => return None,
    })
}

/// Client id from the settings, else from the environment.
fn configured_client_id(state: &AppState, provider: OAuthProvider) -> Option<String> {
    core_settings::get_json::<String>(state.store(), client_id_key(provider))
        .ok()
        .flatten()
        .filter(|id| !id.trim().is_empty())
        .or_else(|| {
            OAuthSettings::for_provider(provider, None)
                .ok()
                .map(|settings| settings.client_id)
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Identity,
    Servers,
}

/// State of the server lookup / connection test.
#[derive(Debug, Clone)]
enum Check {
    Idle,
    Running(&'static str),
    /// Succeeded with exactly these settings; the server's folders.
    Passed(Box<AccountConfig>, Vec<RemoteFolder>),
    Failed(SharedString),
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
    /// Email of the edited account when the dialog opened (its keychain entries use it).
    saved_email: Option<String>,
    step: Step,
    name: Entity<InputState>,
    email: Entity<InputState>,
    password: Entity<InputState>,
    username: Entity<InputState>,
    client_id: Entity<InputState>,
    imap: EndpointInputs,
    smtp: EndpointInputs,
    color: u32,
    provider: Option<&'static Provider>,
    /// Where the server settings came from, shown on the servers step.
    source: Option<SharedString>,
    /// Sign in with OAuth instead of a password.
    use_oauth: bool,
    check: Check,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
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
        let auth: AuthConfig = account_id
            .and_then(|id| {
                core_settings::get_json(store, &config::auth_setting_key(id))
                    .ok()
                    .flatten()
            })
            .unwrap_or_default();
        let saved_email = account.as_ref().map(|account| account.email.clone());
        let (name, email, color) = account
            .map(|account| (account.name, account.email, account.color))
            .unwrap_or_else(|| (String::new(), String::new(), DEFAULT_ACCENT));
        let provider = detect_provider(&email);
        let oauth = OAuthProvider::for_host(&servers.imap.host);
        let client_id = oauth
            .and_then(|provider| configured_client_id(state.read(cx), provider))
            .unwrap_or_default();

        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(tr(cx, "Work, Personal..."))
                .default_value(name)
        });
        let email = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("you@example.com")
                .default_value(email)
        });
        let password = cx.new(|cx| {
            InputState::new(window, cx).masked(true).placeholder(tr(
                cx,
                if account_id.is_some() {
                    "Leave empty to keep the saved password"
                } else {
                    "Password or app password"
                },
            ))
        });
        let username = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(tr(cx, "Usually your email address"))
                .default_value(servers.username.clone())
        });
        let client_id = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(tr(
                    cx,
                    "Optional: enables “Sign in with Google / Microsoft”",
                ))
                .default_value(client_id)
        });
        let imap = EndpointInputs::new(&servers.imap, window, cx);
        let smtp = EndpointInputs::new(&servers.smtp, window, cx);
        let on_enter =
            |this: &mut Self, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>| {
                if matches!(event, InputEvent::PressEnter { .. }) && this.step == Step::Identity {
                    this.next(window, cx);
                }
            };
        let subscriptions = vec![
            cx.subscribe_in(
                &email,
                window,
                move |this, input, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.provider = detect_provider(&input.read(cx).value());
                        this.error = None;
                        cx.notify();
                    }
                    on_enter(this, event, window, cx);
                },
            ),
            cx.subscribe_in(
                &password,
                window,
                move |this, _, event: &InputEvent, window, cx| on_enter(this, event, window, cx),
            ),
        ];
        Self {
            state,
            account_id,
            saved_email,
            step: if account_id.is_some() {
                Step::Servers
            } else {
                Step::Identity
            },
            name,
            email,
            password,
            username,
            client_id,
            imap,
            smtp,
            color,
            provider,
            source: None,
            use_oauth: matches!(auth, AuthConfig::OAuth2 { .. }),
            check: Check::Idle,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    fn email_value(&self, cx: &App) -> String {
        self.email.read(cx).value().trim().to_owned()
    }

    fn password_value(&self, cx: &App) -> Option<String> {
        let password = self.password.read(cx).value().to_string();
        (!password.is_empty()).then_some(password)
    }

    fn fail(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.error = Some(message.into());
        cx.notify();
    }

    /// OAuth provider of the IMAP server currently entered.
    fn oauth_provider(&self, cx: &App) -> Option<OAuthProvider> {
        OAuthProvider::for_host(self.imap.host.read(cx).value().trim())
    }

    // ---------- identity → servers ----------

    /// Identity step → servers step: settings from the provider table, else looked up online.
    fn next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.check, Check::Running(_)) {
            return;
        }
        let email = self.email_value(cx);
        let Some(guess) = suggest_servers(&email) else {
            return self.fail(tr(cx, "Enter a valid email address"), cx);
        };
        // No password check here: a custom domain hosted by Google or Microsoft only shows
        // its browser sign-in once its servers are known (next step).
        self.error = None;
        self.provider = detect_provider(&email);
        if self.name.read(cx).value().trim().is_empty() {
            let name = self.provider.map_or_else(
                || email_domain(&email).unwrap_or_default(),
                |p| p.name.to_owned(),
            );
            self.name
                .update(cx, |input, cx| input.set_value(name, window, cx));
        }
        if let Some(provider) = self.provider {
            let source = tr(cx, "{} settings detected.").replace("{}", provider.name);
            return self.show_servers(guess, source, window, cx);
        }

        self.check = Check::Running(tr(cx, "Looking up the server settings…"));
        cx.notify();
        let lookup_email = email.clone();
        let lookup = cx
            .background_executor()
            .spawn(async move { config::discover(&lookup_email) });
        cx.spawn_in(window, async move |this, cx| {
            let found = lookup.await;
            this.update_in(cx, |this, window, cx| {
                this.check = Check::Idle;
                let (servers, source) = match found {
                    Some(servers) => {
                        let source = if servers.provider.is_empty() {
                            tr(cx, "Settings found online.").to_owned()
                        } else {
                            tr(cx, "{} settings found online.").replace("{}", &servers.provider)
                        };
                        (servers, source)
                    }
                    None => (guess, tr(cx, GUESSED).to_owned()),
                };
                this.show_servers(servers, source, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn show_servers(
        &mut self,
        servers: AccountServers,
        source: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.imap.fill(&servers.imap, window, cx);
        self.smtp.fill(&servers.smtp, window, cx);
        let client_id = OAuthProvider::for_host(&servers.imap.host)
            .filter(|_| self.client_id.read(cx).value().trim().is_empty())
            .and_then(|provider| configured_client_id(self.state.read(cx), provider));
        if let Some(client_id) = client_id {
            self.client_id
                .update(cx, |input, cx| input.set_value(client_id, window, cx));
        }
        self.username.update(cx, |input, cx| {
            input.set_value(servers.username, window, cx)
        });
        self.source = Some(source.into());
        self.step = Step::Servers;
        // No password typed for a Google / Microsoft server: sign in with the browser.
        if !self.use_oauth
            && self.password_value(cx).is_none()
            && OAuthProvider::for_host(&servers.imap.host)
                .and_then(|provider| configured_client_id(self.state.read(cx), provider))
                .is_some()
        {
            self.use_oauth = true;
        }
        cx.notify();
        // Test right away: most users never need to touch the servers.
        if self.use_oauth || self.password_value(cx).is_some() {
            self.run_test(false, window, cx);
        }
    }

    // ---------- test / save ----------

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

    fn auth(&self, cx: &App) -> Result<AuthConfig, String> {
        if !self.use_oauth {
            return Ok(AuthConfig::Password);
        }
        let provider = self
            .oauth_provider(cx)
            .ok_or("Sign-in with a browser only works with Gmail and Outlook servers")?;
        let client_id = self.client_id.read(cx).value().trim().to_owned();
        if client_id.is_empty() {
            return Err(format!(
                "Enter the OAuth client id of your {} app, or use an app password",
                provider.label()
            ));
        }
        Ok(AuthConfig::OAuth2 {
            provider,
            client_id: Some(client_id),
        })
    }

    fn config(&self, cx: &App) -> Result<AccountConfig, String> {
        let email = self.email_value(cx);
        if email_domain(&email).is_none() {
            return Err("Enter a valid email address".into());
        }
        Ok(AccountConfig::from_servers(
            &email,
            &self.servers(cx)?,
            self.auth(cx)?,
        ))
    }

    /// Tests the connection in the background; with `then_save`, saves when it passes.
    fn run_test(&mut self, then_save: bool, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.check, Check::Running(_)) {
            return;
        }
        let config = match self.config(cx) {
            Ok(config) => config,
            Err(message) => return self.fail(message, cx),
        };
        let password = match config.auth {
            AuthConfig::Password => self.password_value(cx),
            AuthConfig::OAuth2 { .. } => None,
        };
        if self.account_id.is_none() && !self.use_oauth && password.is_none() {
            return self.fail(tr(cx, "Enter your password (or an app password)"), cx);
        }
        self.error = None;
        self.check = Check::Running(tr(
            cx,
            if self.use_oauth {
                "Finish signing in in your browser…"
            } else {
                "Testing the connection…"
            },
        ));
        cx.notify();
        let tested = config.clone();
        let task = cx
            .background_executor()
            .spawn(async move { test_connection(&tested, password.as_deref()) });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(folders) => {
                        this.check = Check::Passed(Box::new(config), folders);
                        if then_save {
                            this.save(window, cx);
                        }
                    }
                    Err(message) => this.check = Check::Failed(message.into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Primary button: adding requires a passed test with the current settings.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let config = match self.config(cx) {
            Ok(config) => config,
            Err(message) => return self.fail(message, cx),
        };
        let tested = matches!(&self.check, Check::Passed(passed, _) if **passed == config);
        if self.account_id.is_none() && !tested {
            return self.run_test(true, window, cx);
        }
        self.save(window, cx);
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let config = self.config(cx);
        let (config, servers) = match config.and_then(|config| Ok((config, self.servers(cx)?))) {
            Ok(values) => values,
            Err(message) => return self.fail(message, cx),
        };
        let renamed = self
            .saved_email
            .clone()
            .filter(|old| *old != config.email);
        if let Some(old) = &renamed
            && let Err(err) = move_secrets(old, &config.email)
        {
            return self.fail(
                format!("The password could not be moved to the new address: {err}"),
                cx,
            );
        }
        if let (AuthConfig::Password, Some(password)) = (&config.auth, self.password_value(cx))
            && let Err(err) = credentials::set_password(&config.email, &password)
        {
            return self.fail(
                format!("The password could not be saved in the system keychain: {err}"),
                cx,
            );
        }
        let name = match self.name.read(cx).value().trim() {
            "" => config.email.clone(),
            name => name.to_owned(),
        };
        let account = Account {
            id: self.account_id.unwrap_or(0),
            name,
            email: config.email.clone(),
            color: self.color,
        };
        let mut extra = vec![];
        if let Some(id) = self.account_id {
            match core_settings::set_json_command(config::auth_setting_key(id), &config.auth) {
                Ok(command) => extra.push(command),
                Err(err) => return self.fail(err.to_string(), cx),
            }
        }
        if let Some(provider) = self.oauth_provider(cx) {
            let client_id = self.client_id.read(cx).value().trim().to_owned();
            match core_settings::set_json_command(client_id_key(provider), &client_id) {
                Ok(command) => extra.push(command),
                Err(err) => return self.fail(err.to_string(), cx),
            }
        }
        let saved = match self.account_id {
            Some(id) => save_account(&self.state, id, account, servers, extra, cx).then_some(id),
            None => {
                let folders = match &self.check {
                    Check::Passed(_, folders) => folders.clone(),
                    _ => Vec::new(),
                };
                create_account(
                    &self.state,
                    account,
                    servers,
                    &config.auth,
                    extra,
                    &folders,
                    cx,
                )
            }
        };
        match (saved, renamed) {
            (Some(id), _) => {
                sync_bridge::start_account(id, cx);
                window.close_dialog(cx);
            }
            (None, Some(old)) => {
                let _ = move_secrets(&config.email, &old);
            }
            (None, None) => {}
        }
    }

    /// Closes this dialog and asks to confirm the removal.
    fn remove(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account_id) = self.account_id else {
            return;
        };
        let email = self.email_value(cx);
        window.close_dialog(cx);
        confirm(
            Confirm {
                title: tr(cx, "Remove {}?").replace("{}", &email).into(),
                description: tr(
                    cx,
                    "Its mail, folders and calendars are deleted from this computer and its \
                     password is removed from the system keychain. Nothing is deleted on the server.",
                )
                .into(),
                ok: tr(cx, "Remove account").into(),
                danger: true,
            },
            move |_, cx| sync_bridge::remove_account(account_id, cx),
            window,
            cx,
        );
    }

    // ---------- rendering ----------

    fn render_endpoint(&self, title: &'static str, which: Which, cx: &mut Context<Self>) -> Div {
        let inputs = match which {
            Which::Imap => &self.imap,
            Which::Smtp => &self.smtp,
        };
        let view = cx.entity().downgrade();
        let security = segmented(
            title,
            &SECURITY.map(|(label, value)| (tr(cx, label), value)),
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
                    .child(div().flex_1().child(field(
                        tr(cx, "Server"),
                        Input::new(&inputs.host),
                        cx,
                    )))
                    .child(div().w(px(88.)).child(field(
                        tr(cx, "Port"),
                        Input::new(&inputs.port),
                        cx,
                    ))),
            )
            .child(field(tr(cx, "Security"), security, cx))
    }

    /// How to get an app password (with a link that opens the provider's page), plus the
    /// OAuth sign-in button for Gmail / Outlook when a client id is configured.
    fn render_password_help(&self, cx: &mut Context<Self>) -> Option<Div> {
        let provider = self.provider?;
        let t = theme(cx).clone();
        let help = app_password_help(provider.name).map(|(text, url)| {
            div()
                .flex()
                .flex_col()
                .items_start()
                .gap(SPACE_1)
                .child(
                    div()
                        .text_size(TEXT_SM)
                        .text_color(t.text_muted)
                        .child(tr(cx, text)),
                )
                .child(
                    Button::new("account-app-password")
                        .ghost()
                        .compact()
                        .icon(IconName::ExternalLink)
                        .label(tr(cx, "Create an app password"))
                        .on_click(move |_, _, cx| cx.open_url(url)),
                )
        });
        let oauth = OAuthProvider::for_host(provider.imap.0);
        let sign_in = oauth.map(|oauth| {
            if configured_client_id(self.state.read(cx), oauth).is_some() {
                Button::new("account-oauth")
                    .icon(IconName::Globe)
                    .label(tr(cx, "Sign in with {}").replace("{}", oauth.label()))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.use_oauth = true;
                        this.next(window, cx);
                    }))
                    .into_any_element()
            } else {
                div()
                    .text_size(TEXT_SM)
                    .text_color(t.text_subtle)
                    .child(
                        tr(
                            cx,
                            "To sign in with {} in your browser instead, set an OAuth client id in the server settings or the {} environment variable.",
                        )
                        .replacen("{}", oauth.label(), 1)
                        .replacen(
                            "{}",
                            match oauth {
                                OAuthProvider::Google => "MAILBOX_GOOGLE_CLIENT_ID",
                                OAuthProvider::Microsoft => "MAILBOX_MS_CLIENT_ID",
                            },
                            1,
                        ),
                    )
                    .into_any_element()
            }
        });
        if help.is_none() && sign_in.is_none() {
            return None;
        }
        Some(card(cx).gap(SPACE_2).children(help).children(sign_in))
    }

    fn render_identity(&self, cx: &mut Context<Self>) -> Div {
        let view = cx.entity().downgrade();
        let detected = self.provider.map(|provider| {
            Banner::success(
                "account-detected",
                tr(cx, "{} detected: settings will be filled in").replace("{}", provider.name),
            )
            .icon(IconName::ShieldCheck)
            .plain()
        });
        let adding = self.account_id.is_none();
        div()
            .flex()
            .flex_col()
            .gap(SPACE_3)
            .child(field(tr(cx, "Email address"), Input::new(&self.email), cx))
            .children(detected)
            .when(adding, |el| {
                el.child(field(tr(cx, "Password"), Input::new(&self.password), cx))
                    .children(self.render_password_help(cx))
            })
            .child(field(tr(cx, "Account name"), Input::new(&self.name), cx))
            .child(field(
                tr(cx, "Colour"),
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

    fn render_check(&self, cx: &App) -> Option<Banner> {
        const ID: &str = "account-check";
        match &self.check {
            Check::Idle => None,
            Check::Running(text) => Some(Banner::info(ID, *text).icon(IconName::Clock)),
            Check::Passed(_, folders) => Some(Banner::success(
                ID,
                tr(
                    cx,
                    "Connected: sign-in works for receiving and sending ({} folders).",
                )
                .replace("{}", &folders.len().to_string()),
            )),
            Check::Failed(message) => Some(Banner::error(ID, message.clone())),
        }
    }

    fn render_servers(&self, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let editing = self.account_id.is_some();
        let source = (!editing)
            .then(|| self.source.clone())
            .flatten()
            .map(|text| {
                if self.provider.is_some() || text != tr(cx, GUESSED) {
                    Banner::success("account-source", text).icon(IconName::ShieldCheck)
                } else {
                    Banner::warning("account-source", text)
                }
            });
        let oauth = self.oauth_provider(cx);
        let auth_line = oauth.map(|provider| {
            let (text, toggle) = if self.use_oauth {
                (
                    tr(cx, "Signing in with {} in your browser.").replace("{}", provider.label()),
                    tr(cx, "Use a password instead"),
                )
            } else {
                (
                    tr(cx, "Signing in with a password or app password.").to_owned(),
                    tr(cx, "Sign in with the browser instead"),
                )
            };
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(SPACE_2)
                .text_size(TEXT_SM)
                .text_color(t.text_muted)
                .child(div().flex_1().child(text))
                .child(
                    Button::new("account-auth-toggle")
                        .ghost()
                        .compact()
                        .label(toggle)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.use_oauth = !this.use_oauth;
                            this.check = Check::Idle;
                            cx.notify();
                        })),
                )
        });
        div()
            .flex()
            .flex_col()
            .gap(SPACE_3)
            .children(source)
            .when(editing, |el| el.child(self.render_identity(cx)))
            .child(
                card(cx)
                    .child(self.render_endpoint(tr(cx, "INCOMING MAIL (IMAP)"), Which::Imap, cx))
                    .child(self.render_endpoint(tr(cx, "OUTGOING MAIL (SMTP)"), Which::Smtp, cx)),
            )
            .child(field(tr(cx, "Username"), Input::new(&self.username), cx))
            .children(auth_line)
            .when(!self.use_oauth, |el| {
                el.child(field(
                    tr(cx, if editing { "New password" } else { "Password" }),
                    Input::new(&self.password),
                    cx,
                ))
            })
            .when(oauth.is_some(), |el| {
                el.child(field(
                    tr(cx, "OAuth client id"),
                    Input::new(&self.client_id),
                    cx,
                ))
            })
            .child(div().text_size(TEXT_SM).text_color(t.text_subtle).child(tr(
                cx,
                "Passwords are kept in the system keychain, never in the Mailbox database.",
            )))
    }
}

#[derive(Debug, Clone, Copy)]
enum Which {
    Imap,
    Smtp,
}

impl Render for AccountDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let editing = self.account_id.is_some();
        let (heading, subtitle) = match (editing, self.step) {
            (true, _) => ("Account settings", "Name, colour, servers and password."),
            (false, Step::Identity) => (
                "Add an account",
                "Enter your email address and password: Mailbox finds the server settings \
                 and checks that everything works.",
            ),
            (false, Step::Servers) => ("Server settings", "Step 2 of 2"),
        };
        let (heading, subtitle) = (tr(cx, heading), tr(cx, subtitle));
        let body = match (editing, self.step) {
            (false, Step::Identity) => self.render_identity(cx),
            _ => self.render_servers(cx),
        };
        let running = matches!(self.check, Check::Running(_));

        let back = (!editing && self.step == Step::Servers).then(|| {
            Button::new("account-back")
                .ghost()
                .icon(IconName::ArrowLeft)
                .label(tr(cx, "Back"))
                .disabled(running)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.step = Step::Identity;
                    this.use_oauth = false;
                    this.check = Check::Idle;
                    this.error = None;
                    cx.notify();
                }))
        });
        let remove = editing.then(|| {
            Button::new("account-remove")
                .ghost()
                .icon(IconName::Trash)
                .label(tr(cx, "Remove account…"))
                .on_click(cx.listener(|this, _, window, cx| this.remove(window, cx)))
        });
        let test = (self.step == Step::Servers).then(|| {
            Button::new("account-test")
                .label(tr(cx, "Test connection"))
                .disabled(running)
                .on_click(cx.listener(|this, _, window, cx| this.run_test(false, window, cx)))
        });
        let primary_label = match (editing, self.step) {
            (true, _) => tr(cx, "Save"),
            (false, Step::Identity) => tr(cx, "Continue"),
            (false, Step::Servers) => tr(cx, "Add account"),
        };
        let primary = Button::new("account-primary")
            .primary()
            .label(primary_label)
            .disabled(running)
            .on_click(cx.listener(|this, _, window, cx| {
                if this.account_id.is_none() && this.step == Step::Identity {
                    this.next(window, cx);
                } else {
                    this.submit(window, cx);
                }
            }));

        let header = div()
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
            );
        let messages = self
            .render_check(cx)
            .map(IntoElement::into_any_element)
            .into_iter()
            .chain(
                self.error
                    .clone()
                    .map(|error| Banner::error("account-error", error).into_any_element()),
            )
            .collect();
        let footer = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(SPACE_2)
            .children(back)
            .children(remove)
            .child(div().flex_1())
            .child(
                Button::new("account-cancel")
                    .label(tr(cx, "Cancel"))
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            )
            .children(test)
            .child(primary);
        dialog_frame("account-body", header, body, messages, footer, window)
    }
}

/// The keychain keys secrets by email: after an address change, the saved password and OAuth
/// token follow it.
fn move_secrets(old: &str, new: &str) -> Result<(), String> {
    for service in [
        config::PASSWORD_SERVICE,
        mailbox_sync::oauth::REFRESH_TOKEN_SERVICE,
    ] {
        if let Ok(secret) = credentials::get(service, old) {
            credentials::set(service, new, &secret).map_err(|err| err.to_string())?;
            let _ = credentials::delete(service, old);
        }
    }
    Ok(())
}

/// Folder position: well-known folders first, in the usual order.
fn role_position(role: FolderRole) -> i64 {
    match role {
        FolderRole::Inbox => 0,
        FolderRole::Drafts => 1,
        FolderRole::Sent => 2,
        FolderRole::Archive => 3,
        FolderRole::Junk => 4,
        FolderRole::Trash => 5,
        _ => 10,
    }
}

/// Creates the account with the server's folders (parents first), a calendar, its server
/// settings and auth method. Returns the new account id.
fn create_account(
    state: &Entity<AppState>,
    account: Account,
    servers: AccountServers,
    auth: &AuthConfig,
    extra: Vec<Command>,
    folders: &[RemoteFolder],
    cx: &mut App,
) -> Option<Id> {
    state.update(cx, |state, cx| {
        let email = account.email.clone();
        let color = account.color;
        if !state.run_untracked(Command::SaveAccount(Box::new(account)), cx) {
            return None;
        }
        let account_id = state.store().list_accounts().ok().and_then(|accounts| {
            accounts
                .into_iter()
                .filter(|account| account.email == email)
                .map(|account| account.id)
                .max()
        });
        let Some(account_id) = account_id else {
            state.report_error("The new account could not be found", cx);
            return None;
        };

        // Folder ids are needed for children, so each folder is its own command.
        let mut folders = folders.to_vec();
        if !folders
            .iter()
            .any(|folder| folder.role == FolderRole::Inbox)
        {
            folders.push(RemoteFolder::new("INBOX", None, FolderRole::Inbox));
        }
        folders.sort_by_key(|folder| folder.path.len());
        let mut created: Vec<(String, Id)> = Vec::new();
        for remote in &folders {
            let parent_id = remote.parent.as_ref().and_then(|parent| {
                created
                    .iter()
                    .find(|(path, _)| path == parent)
                    .map(|(_, id)| *id)
            });
            let folder = Folder {
                id: 0,
                account_id,
                parent_id,
                name: if remote.role == FolderRole::Inbox {
                    "Inbox".to_owned()
                } else {
                    remote.name.clone()
                },
                role: remote.role,
                position: role_position(remote.role),
            };
            match command::apply(state.store(), Command::SaveFolder(Box::new(folder))) {
                Ok(Command::DeleteFolder(id)) => created.push((remote.path.clone(), id)),
                Ok(_) => {}
                Err(err) => state.report_error(err, cx),
            }
        }

        let mut commands = vec![Command::SaveCalendar(Box::new(Calendar {
            id: 0,
            account_id,
            name: "Calendar".into(),
            color,
        }))];
        let settings = servers.command(account_id).and_then(|servers| {
            let auth = core_settings::set_json_command(config::auth_setting_key(account_id), auth)?;
            Ok([servers, auth])
        });
        match settings {
            Ok(settings) => commands.extend(settings),
            Err(err) => {
                state.report_error(err, cx);
                return None;
            }
        }
        commands.extend(extra);
        if !state.run_untracked(Command::Batch(commands), cx) {
            return None;
        }
        let inbox = state.store().folder_by_role(account_id, FolderRole::Inbox);
        if let Ok(Some(inbox)) = inbox {
            state.set_module(Module::Mail, cx);
            state.select_folder(Some(inbox.id), cx);
        }
        state.notify_info(
            tr(cx, "Account {} added: syncing…").replace("{}", &email),
            cx,
        );
        Some(account_id)
    })
}

/// Updates an existing account, its server settings and auth in one undoable step.
fn save_account(
    state: &Entity<AppState>,
    account_id: Id,
    account: Account,
    servers: AccountServers,
    extra: Vec<Command>,
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
        let mut commands = vec![Command::SaveAccount(Box::new(account)), servers];
        commands.extend(extra);
        state.run(Command::Batch(commands), cx)
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
