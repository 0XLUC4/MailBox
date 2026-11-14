//! CalDAV/CardDAV accounts (`dav.json` next to `sync.json`), provider presets and
//! credentials (OS keyring, service [`DAV_PASSWORD_SERVICE`], user = login, like mail).

use std::path::Path;

use serde::{Deserialize, Serialize};
use url::Url;

use super::discovery::Kind;
use super::http::{Auth, DavClient, parse_url};
use crate::config::{OAuthProvider, credentials};
use crate::oauth::{OAuthSettings, REFRESH_TOKEN_SERVICE, TokenProvider};
use crate::{SyncError, SyncResult};

/// Keyring service for DAV passwords (user = login). Separate from the mail password: iCloud
/// and Fastmail use app-specific passwords per protocol.
pub const DAV_PASSWORD_SERVICE: &str = "mailbox-dav";

/// Google scopes when the mail sign-in does not cover calendars/contacts.
const GOOGLE_DAV_SCOPES: [&str; 2] = [
    "https://www.googleapis.com/auth/calendar",
    "https://www.googleapis.com/auth/carddav",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Google,
    ICloud,
    Fastmail,
    Nextcloud,
    #[default]
    Generic,
}

impl Provider {
    pub const ALL: [Provider; 5] = [
        Self::Google,
        Self::ICloud,
        Self::Fastmail,
        Self::Nextcloud,
        Self::Generic,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Google => "Google",
            Self::ICloud => "iCloud",
            Self::Fastmail => "Fastmail",
            Self::Nextcloud => "Nextcloud",
            Self::Generic => "Other (CalDAV/CardDAV)",
        }
    }

    /// The user must type a server address.
    pub fn needs_url(self) -> bool {
        matches!(self, Self::Nextcloud | Self::Generic)
    }

    /// Google signs in with OAuth, the others with a password.
    pub fn uses_oauth(self) -> bool {
        self == Self::Google
    }

    pub fn help(self) -> &'static str {
        match self {
            Self::Google => "Signs in with your Google account in the browser.",
            Self::ICloud => {
                "Use an app-specific password (appleid.apple.com > Sign-In and Security)."
            }
            Self::Fastmail => {
                "Use an app password with CalDAV/CardDAV access (Settings > Privacy & Security)."
            }
            Self::Nextcloud => {
                "Server address, e.g. https://cloud.example.com. An app password is recommended."
            }
            Self::Generic => "Server address of your CalDAV/CardDAV service.",
        }
    }

    /// Discovery start URL for `kind`.
    pub fn start_url(self, kind: Kind, server: &str, login: &str) -> SyncResult<Url> {
        let fixed = match (self, kind) {
            (Self::Google, Kind::Calendar) => {
                return parse_url(&format!(
                    "https://apidata.googleusercontent.com/caldav/v2/{login}/user"
                ));
            }
            (Self::Google, Kind::AddressBook) => "https://www.googleapis.com/.well-known/carddav",
            (Self::ICloud, Kind::Calendar) => "https://caldav.icloud.com/",
            (Self::ICloud, Kind::AddressBook) => "https://contacts.icloud.com/",
            (Self::Fastmail, Kind::Calendar) => "https://caldav.fastmail.com/",
            (Self::Fastmail, Kind::AddressBook) => "https://carddav.fastmail.com/",
            (Self::Nextcloud, _) => {
                let base = parse_url(server)?;
                return base
                    .join("/remote.php/dav/")
                    .map_err(|err| SyncError::Protocol(err.to_string()));
            }
            (Self::Generic, _) => return parse_url(server),
        };
        parse_url(fixed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DavAccount {
    /// Matches `mailbox_core::Account::email` (an account is created when none matches).
    pub email: String,
    #[serde(default)]
    pub provider: Provider,
    /// Server address for Nextcloud/generic.
    #[serde(default)]
    pub url: String,
    /// Login; the email when empty.
    #[serde(default)]
    pub username: String,
    #[serde(default = "yes")]
    pub calendars: bool,
    #[serde(default = "yes")]
    pub contacts: bool,
    #[serde(default = "default_poll_minutes")]
    pub poll_minutes: u32,
}

fn yes() -> bool {
    true
}

fn default_poll_minutes() -> u32 {
    15
}

impl DavAccount {
    pub fn new(provider: Provider, email: impl Into<String>) -> Self {
        Self {
            email: email.into(),
            provider,
            url: String::new(),
            username: String::new(),
            calendars: true,
            contacts: true,
            poll_minutes: default_poll_minutes(),
        }
    }

    pub fn login(&self) -> &str {
        if self.username.trim().is_empty() {
            &self.email
        } else {
            self.username.trim()
        }
    }

    /// Stable key, used for the state file name.
    pub fn key(&self) -> String {
        let raw = format!("{:?}-{}-{}", self.provider, self.login(), self.url);
        raw.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect()
    }

    pub fn kinds(&self) -> Vec<Kind> {
        let mut kinds = Vec::new();
        if self.calendars {
            kinds.push(Kind::Calendar);
        }
        if self.contacts {
            kinds.push(Kind::AddressBook);
        }
        kinds
    }

    pub fn start_url(&self, kind: Kind) -> SyncResult<Url> {
        self.provider.start_url(kind, &self.url, self.login())
    }

    /// An authenticated client (reads the keyring; Google may open a browser sign-in).
    pub fn client(&self) -> SyncResult<DavClient> {
        let auth = if self.provider.uses_oauth() {
            Auth::Bearer(self.google_token()?)
        } else {
            Auth::Basic {
                user: self.login().to_owned(),
                password: password(self.login())?,
            }
        };
        Ok(DavClient::new(Some(auth)))
    }

    /// The mail account's token when it was granted DAV scopes, else a DAV-only sign-in
    /// (refresh token stored under `dav:<email>`).
    fn google_token(&self) -> SyncResult<String> {
        let mut settings = OAuthSettings::for_provider(OAuthProvider::Google, None)?;
        if credentials::get(REFRESH_TOKEN_SERVICE, &format!("dav:{}", self.email)).is_err()
            && credentials::get(REFRESH_TOKEN_SERVICE, &self.email).is_ok()
        {
            return TokenProvider::new(self.email.clone(), settings).access_token();
        }
        settings
            .scopes
            .extend(GOOGLE_DAV_SCOPES.iter().map(|s| (*s).to_owned()));
        TokenProvider::new(format!("dav:{}", self.email), settings).access_token()
    }
}

pub fn password(login: &str) -> SyncResult<String> {
    credentials::get(DAV_PASSWORD_SERVICE, login)
}

pub fn set_password(login: &str, password: &str) -> SyncResult<()> {
    credentials::set(DAV_PASSWORD_SERVICE, login, password)
}

/// Reads `dav.json`; a missing file means no account.
pub fn load_accounts(path: &Path) -> SyncResult<Vec<DavAccount>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(err.into()),
    }
}

pub fn save_accounts(path: &Path, accounts: &[DavAccount]) -> SyncResult<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(accounts)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_urls() {
        let url = |p: Provider, k, server| p.start_url(k, server, "me@x.com").unwrap().to_string();
        assert_eq!(
            url(Provider::Google, Kind::Calendar, ""),
            "https://apidata.googleusercontent.com/caldav/v2/me@x.com/user"
        );
        assert_eq!(
            url(Provider::ICloud, Kind::AddressBook, ""),
            "https://contacts.icloud.com/"
        );
        assert_eq!(
            url(Provider::Nextcloud, Kind::Calendar, "cloud.example.com/"),
            "https://cloud.example.com/remote.php/dav/"
        );
        assert_eq!(
            url(Provider::Generic, Kind::Calendar, "http://localhost:5232/"),
            "http://localhost:5232/"
        );
        assert!(
            Provider::Generic
                .start_url(Kind::Calendar, "", "x")
                .is_err()
        );
    }

    #[test]
    fn accounts_roundtrip_with_defaults() {
        let parsed: Vec<DavAccount> =
            serde_json::from_str(r#"[{"email":"me@icloud.com","provider":"icloud"}]"#).unwrap();
        let account = &parsed[0];
        assert_eq!(account.login(), "me@icloud.com");
        assert_eq!(account.kinds(), vec![Kind::Calendar, Kind::AddressBook]);
        assert_eq!(account.poll_minutes, 15);
        assert!(!account.key().contains('@'));

        let dir = std::env::temp_dir().join(format!("mailbox-dav-config-{}", std::process::id()));
        let path = dir.join("dav.json");
        assert!(load_accounts(&path).unwrap().is_empty());
        save_accounts(&path, &parsed).unwrap();
        assert_eq!(load_accounts(&path).unwrap(), parsed);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
