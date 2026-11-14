//! [`MailBackend`] over IMAP (`imap` crate, rustls, TLS/STARTTLS) and SMTP (`lettre`).
//! Blocking: run it on a background thread (see [`crate::worker`]).

use std::time::Duration;

use chrono::Utc;
use imap::types::{Fetch, Flag};
use imap::{ClientBuilder, Connection, ConnectionMode, Session, TlsKind};
use lettre::Transport;
use lettre::address::Envelope;
use lettre::transport::smtp::SmtpTransport;
use lettre::transport::smtp::authentication::{Credentials, Mechanism};

use crate::backend::{
    FolderStatus, MailBackend, RawMessage, RemoteFlags, RemoteFolder, Uid, classify_folder,
};
use crate::config::{AccountConfig, AuthConfig, Security, ServerConfig, credentials};
use crate::oauth::{OAuthSettings, TokenProvider, xoauth2};
use crate::{SyncError, SyncResult};

/// UIDs per IMAP command (keeps command lines short).
const UID_BATCH: usize = 200;

enum Secret {
    Password(Option<String>),
    OAuth(Box<TokenProvider>),
}

enum Credential {
    Password(String),
    Token(String),
}

#[derive(Debug, Clone, Copy, Default)]
struct Capabilities {
    idle: bool,
    move_: bool,
    uidplus: bool,
}

pub struct ImapSmtpBackend {
    config: AccountConfig,
    secret: Secret,
    session: Option<Session<Connection>>,
    capabilities: Capabilities,
}

impl ImapSmtpBackend {
    pub fn new(config: AccountConfig) -> SyncResult<Self> {
        let secret = match &config.auth {
            AuthConfig::Password => Secret::Password(None),
            AuthConfig::OAuth2 {
                provider,
                client_id,
            } => Secret::OAuth(Box::new(TokenProvider::new(
                config.email.clone(),
                OAuthSettings::for_provider(*provider, client_id.as_deref())?,
            ))),
        };
        Ok(Self {
            config,
            secret,
            session: None,
            capabilities: Capabilities::default(),
        })
    }

    /// Uses `password` instead of the OS credential store (account setup, tests).
    pub fn with_password(config: AccountConfig, password: impl Into<String>) -> SyncResult<Self> {
        let mut backend = Self::new(config)?;
        backend.secret = Secret::Password(Some(password.into()));
        Ok(backend)
    }

    fn credential(&mut self) -> SyncResult<Credential> {
        match &mut self.secret {
            Secret::Password(cached) => {
                if cached.is_none() {
                    *cached = Some(credentials::password(&self.config.email)?);
                }
                Ok(Credential::Password(cached.clone().unwrap_or_default()))
            }
            Secret::OAuth(provider) => Ok(Credential::Token(provider.access_token()?)),
        }
    }

    /// Forgets cached secrets so the next attempt reads/refreshes them again.
    fn forget_secret(&mut self) {
        match &mut self.secret {
            Secret::Password(cached) => *cached = None,
            Secret::OAuth(provider) => provider.invalidate(),
        }
    }

    fn session(&mut self) -> SyncResult<&mut Session<Connection>> {
        self.session
            .as_mut()
            .ok_or_else(|| SyncError::Connection("not connected".into()))
    }

    /// Runs `f` on the session; a broken connection is dropped so the next sync reconnects.
    fn with_session<T>(
        &mut self,
        f: impl FnOnce(&mut Session<Connection>, Capabilities) -> imap::Result<T>,
    ) -> SyncResult<T> {
        let capabilities = self.capabilities;
        let result = f(self.session()?, capabilities).map_err(imap_error);
        if matches!(result, Err(SyncError::Connection(_))) {
            self.session = None;
        }
        result
    }

    fn fetch(&mut self, path: &str, uids: &[Uid], query: &str) -> SyncResult<Vec<RawMessage>> {
        let headers_only = query.contains("HEADER");
        self.with_session(|session, _| {
            session.select(path)?;
            let mut messages = Vec::with_capacity(uids.len());
            for chunk in uids.chunks(UID_BATCH) {
                let fetches = session.uid_fetch(uid_set(chunk), query)?;
                messages.extend(fetches.iter().filter_map(|fetch| {
                    let data = if headers_only {
                        fetch.header()
                    } else {
                        fetch.body()
                    }?;
                    Some(RawMessage {
                        uid: fetch.uid?,
                        flags: flags_of(fetch),
                        internal_date: fetch.internal_date().map(|date| date.with_timezone(&Utc)),
                        data: data.to_vec(),
                    })
                }));
            }
            Ok(messages)
        })
    }

    fn smtp_transport(&mut self) -> SyncResult<SmtpTransport> {
        let smtp = self.config.smtp.clone();
        let builder = match smtp.security {
            Security::Tls => SmtpTransport::relay(&smtp.host),
            Security::StartTls => SmtpTransport::starttls_relay(&smtp.host),
            Security::Plain => Ok(SmtpTransport::builder_dangerous(&smtp.host)),
        }
        .map_err(|err| SyncError::Connection(err.to_string()))?
        .port(smtp.port)
        .timeout(Some(Duration::from_secs(60)));
        let user = self.config.login().to_owned();
        let builder = match self.credential()? {
            Credential::Password(password) => builder.credentials(Credentials::new(user, password)),
            Credential::Token(token) => builder
                .credentials(Credentials::new(user, token))
                .authentication(vec![Mechanism::Xoauth2]),
        };
        Ok(builder.build())
    }
}

struct XOAuth2(String);

impl imap::Authenticator for XOAuth2 {
    type Response = String;

    fn process(&self, _challenge: &[u8]) -> Self::Response {
        self.0.clone()
    }
}

impl MailBackend for ImapSmtpBackend {
    fn connect(&mut self) -> SyncResult<()> {
        if self.session.is_some() {
            return Ok(());
        }
        let imap = &self.config.imap;
        let mode = match imap.security {
            Security::Tls => ConnectionMode::Tls,
            Security::StartTls => ConnectionMode::StartTls,
            Security::Plain => ConnectionMode::Plaintext,
        };
        let client = ClientBuilder::new(imap.host.as_str(), imap.port)
            .mode(mode)
            .tls_kind(TlsKind::Rust)
            .connect()
            .map_err(|err| SyncError::Connection(err.to_string()))?;
        let user = self.config.login().to_owned();
        let login = match self.credential()? {
            Credential::Password(password) => client.login(&user, &password),
            Credential::Token(token) => {
                client.authenticate("XOAUTH2", &XOAuth2(xoauth2(&user, &token)))
            }
        };
        let mut session = match login {
            Ok(session) => session,
            Err((err, _client)) => {
                self.forget_secret();
                return Err(SyncError::Auth(err.to_string()));
            }
        };
        let capabilities = session.capabilities().map_err(imap_error)?;
        self.capabilities = Capabilities {
            idle: capabilities.has_str("IDLE"),
            move_: capabilities.has_str("MOVE"),
            uidplus: capabilities.has_str("UIDPLUS"),
        };
        self.session = Some(session);
        Ok(())
    }

    fn disconnect(&mut self) {
        if let Some(mut session) = self.session.take() {
            // Best effort: the connection is dropped either way.
            let _ = session.logout();
        }
    }

    fn list_folders(&mut self) -> SyncResult<Vec<RemoteFolder>> {
        self.with_session(|session, _| {
            let names = session.list(None, Some("*"))?;
            Ok(names
                .iter()
                .filter_map(|name| {
                    let attributes: Vec<String> = name
                        .attributes()
                        .iter()
                        .map(|attribute| format!("{attribute:?}"))
                        .collect();
                    let role = classify_folder(name.name(), &attributes)?;
                    Some(RemoteFolder::new(name.name(), name.delimiter(), role))
                })
                .collect())
        })
    }

    fn status(&mut self, path: &str) -> SyncResult<FolderStatus> {
        let mailbox = self.with_session(|session, _| session.select(path))?;
        Ok(FolderStatus {
            uid_validity: mailbox.uid_validity.unwrap_or(0),
            uid_next: mailbox.uid_next.unwrap_or(0),
        })
    }

    fn fetch_flags(&mut self, path: &str) -> SyncResult<Vec<(Uid, RemoteFlags)>> {
        self.with_session(|session, _| {
            if session.select(path)?.exists == 0 {
                return Ok(Vec::new());
            }
            let fetches = session.uid_fetch("1:*", "(UID FLAGS)")?;
            Ok(fetches
                .iter()
                .filter_map(|fetch| Some((fetch.uid?, flags_of(fetch))))
                .collect())
        })
    }

    fn fetch_headers(&mut self, path: &str, uids: &[Uid]) -> SyncResult<Vec<RawMessage>> {
        self.fetch(path, uids, "(UID FLAGS INTERNALDATE BODY.PEEK[HEADER])")
    }

    fn fetch_full(&mut self, path: &str, uids: &[Uid]) -> SyncResult<Vec<RawMessage>> {
        self.fetch(path, uids, "(UID FLAGS INTERNALDATE BODY.PEEK[])")
    }

    fn set_flags(&mut self, path: &str, uids: &[Uid], flags: RemoteFlags) -> SyncResult<()> {
        self.with_session(|session, _| {
            session.select(path)?;
            for chunk in uids.chunks(UID_BATCH) {
                let set = uid_set(chunk);
                for (flag, on) in [("\\Seen", flags.seen), ("\\Flagged", flags.flagged)] {
                    let sign = if on { '+' } else { '-' };
                    session.uid_store(&set, format!("{sign}FLAGS.SILENT ({flag})"))?;
                }
            }
            Ok(())
        })
    }

    fn move_messages(&mut self, path: &str, uids: &[Uid], target: &str) -> SyncResult<()> {
        self.with_session(|session, capabilities| {
            session.select(path)?;
            for chunk in uids.chunks(UID_BATCH) {
                let set = uid_set(chunk);
                if capabilities.move_ {
                    session.uid_mv(&set, target)?;
                } else {
                    session.uid_copy(&set, quote(target))?;
                    delete_uids(session, &set, capabilities)?;
                }
            }
            Ok(())
        })
    }

    fn delete_messages(&mut self, path: &str, uids: &[Uid]) -> SyncResult<()> {
        self.with_session(|session, capabilities| {
            session.select(path)?;
            for chunk in uids.chunks(UID_BATCH) {
                delete_uids(session, &uid_set(chunk), capabilities)?;
            }
            Ok(())
        })
    }

    fn append(&mut self, path: &str, raw: &[u8], flags: RemoteFlags) -> SyncResult<()> {
        self.with_session(|session, _| {
            let mut append = session.append(path, raw);
            if flags.seen {
                append.flag(Flag::Seen);
            }
            if flags.flagged {
                append.flag(Flag::Flagged);
            }
            append.finish().map(|_| ())
        })
    }

    fn send(&mut self, from: &str, recipients: &[String], raw: &[u8]) -> SyncResult<()> {
        let invalid = |err: lettre::address::AddressError| SyncError::Protocol(err.to_string());
        let from = from.parse().map_err(invalid)?;
        let to = recipients
            .iter()
            .map(|address| address.parse())
            .collect::<Result<Vec<_>, _>>()
            .map_err(invalid)?;
        let envelope =
            Envelope::new(Some(from), to).map_err(|err| SyncError::Protocol(err.to_string()))?;
        let transport = self.smtp_transport()?;
        transport.send_raw(&envelope, raw).map_err(smtp_error)?;
        Ok(())
    }

    fn wait_for_changes(&mut self, path: &str, timeout: Duration) -> SyncResult<bool> {
        if !self.capabilities.idle {
            return Err(SyncError::Unsupported("IDLE"));
        }
        self.with_session(|session, _| {
            session.select(path)?;
            let mut idle = session.idle();
            idle.timeout(timeout);
            let outcome = idle.wait_while(imap::extensions::idle::stop_on_any)?;
            Ok(outcome == imap::extensions::idle::WaitOutcome::MailboxChanged)
        })
    }
}

/// Account setup check: IMAP login + folder list, then SMTP EHLO + AUTH. Returns the server's
/// folders. Errors are sentences for the user. `password: None` = OAuth sign-in (or the
/// stored password).
pub fn test_connection(
    config: &AccountConfig,
    password: Option<&str>,
) -> Result<Vec<RemoteFolder>, String> {
    let incoming = |err: SyncError| explain("Incoming mail (IMAP)", &config.imap, &err);
    let outgoing = |err: SyncError| explain("Outgoing mail (SMTP)", &config.smtp, &err);
    let mut backend = match password {
        Some(password) => ImapSmtpBackend::with_password(config.clone(), password),
        None => ImapSmtpBackend::new(config.clone()),
    }
    .map_err(incoming)?;
    let folders = backend
        .connect()
        .and_then(|()| backend.list_folders())
        .map_err(incoming)?;
    backend.disconnect();
    let transport = backend.smtp_transport().map_err(outgoing)?;
    match transport.test_connection() {
        Ok(true) => Ok(folders),
        Ok(false) => Err(outgoing(SyncError::Connection(
            "the server closed the connection".into(),
        ))),
        Err(err) => Err(outgoing(smtp_error(err))),
    }
}

/// A user-facing explanation of a connection test failure.
fn explain(stage: &str, server: &ServerConfig, err: &SyncError) -> String {
    let hint = match err {
        SyncError::Auth(_) => "the server rejected the user name or password. If the account \
                               uses two-step verification (Gmail, Outlook, iCloud, Yahoo...), \
                               create an app password and use it here."
            .to_owned(),
        SyncError::Connection(_) | SyncError::Io(_) => format!(
            "cannot reach {}:{}. Check the server name, the port, the security setting and \
             your internet connection.",
            server.host, server.port
        ),
        SyncError::Credentials(_) => {
            "the password could not be read from the system keychain.".to_owned()
        }
        _ => "the server answered with an error.".to_owned(),
    };
    format!("{stage}: {hint} ({err})")
}

/// SMTP failures: 535 & co. are authentication errors, other permanent ones protocol errors,
/// the rest (network, 4xx) are retryable connection errors.
fn smtp_error(err: lettre::transport::smtp::Error) -> SyncError {
    let text = err.to_string();
    let lower = text.to_lowercase();
    if lower.contains("535") || lower.contains("authentication") {
        SyncError::Auth(text)
    } else if err.is_permanent() {
        SyncError::Protocol(text)
    } else {
        SyncError::Connection(text)
    }
}

fn delete_uids(
    session: &mut Session<Connection>,
    set: &str,
    capabilities: Capabilities,
) -> imap::Result<()> {
    session.uid_store(set, "+FLAGS.SILENT (\\Deleted)")?;
    if capabilities.uidplus {
        session.uid_expunge(set)?;
    } else {
        session.expunge()?;
    }
    Ok(())
}

fn flags_of(fetch: &Fetch<'_>) -> RemoteFlags {
    RemoteFlags {
        seen: fetch.flags().contains(&Flag::Seen),
        flagged: fetch.flags().contains(&Flag::Flagged),
    }
}

/// `1,2,3` (servers accept plain lists; ranges are an optimisation we skip).
fn uid_set(uids: &[Uid]) -> String {
    uids.iter()
        .map(Uid::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// IMAP quoted string for commands the `imap` crate does not quote itself.
fn quote(mailbox: &str) -> String {
    format!("\"{}\"", mailbox.replace('\\', "\\\\").replace('"', "\\\""))
}

fn imap_error(err: imap::Error) -> SyncError {
    let text = err.to_string();
    match err {
        imap::Error::Io(_) | imap::Error::ConnectionLost | imap::Error::Bye(_) => {
            SyncError::Connection(text)
        }
        _ => SyncError::Protocol(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(uid_set(&[1, 5, 9]), "1,5,9");
        assert_eq!(quote(r#"Odd "name"\x"#), r#""Odd \"name\"\\x""#);
    }
}
