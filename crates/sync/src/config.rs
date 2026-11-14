//! Account server settings (JSON file) and secrets (OS credential store via `keyring`:
//! Windows Credential Manager, macOS Keychain, Secret Service).
//!
//! `sync.json` example:
//! ```json
//! [{
//!   "email": "me@example.com",
//!   "imap": { "host": "imap.example.com", "port": 993, "security": "tls" },
//!   "smtp": { "host": "smtp.example.com", "port": 587, "security": "starttls" },
//!   "auth": { "type": "password" }
//! }]
//! ```
//! OAuth: `"auth": { "type": "oauth2", "provider": "google" }`; the client id comes from
//! `client_id` or the `MAILBOX_GOOGLE_CLIENT_ID` / `MAILBOX_MICROSOFT_CLIENT_ID` env vars.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{SyncError, SyncResult};

/// Keyring service for account passwords (user = account email).
pub const PASSWORD_SERVICE: &str = "mailbox";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Security {
    /// Implicit TLS (IMAPS 993, SMTPS 465).
    #[default]
    Tls,
    /// Plain connection upgraded with STARTTLS (IMAP 143, submission 587).
    StartTls,
    /// No encryption (local test servers only).
    Plain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub security: Security,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OAuthProvider {
    Google,
    Microsoft,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AuthConfig {
    /// Password from the OS credential store.
    #[default]
    Password,
    OAuth2 {
        provider: OAuthProvider,
        #[serde(default)]
        client_id: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountConfig {
    /// Matches `mailbox_core::Account::email`.
    pub email: String,
    /// Login name when it differs from the email.
    #[serde(default)]
    pub username: Option<String>,
    pub imap: ServerConfig,
    pub smtp: ServerConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    /// Mail newer than this is kept fully offline; older mail is headers-only until opened.
    #[serde(default = "default_offline_days")]
    pub offline_days: u32,
    #[serde(default = "default_poll_minutes")]
    pub poll_minutes: u32,
    /// Push via IMAP IDLE on the inbox when the server supports it.
    #[serde(default = "default_true")]
    pub idle: bool,
    /// Append sent mail to the Sent folder. `None` = yes, except Gmail (which does it itself).
    #[serde(default)]
    pub save_sent: Option<bool>,
}

fn default_offline_days() -> u32 {
    30
}

fn default_poll_minutes() -> u32 {
    5
}

fn default_true() -> bool {
    true
}

impl AccountConfig {
    pub fn login(&self) -> &str {
        self.username.as_deref().unwrap_or(&self.email)
    }

    pub fn save_sent_copy(&self) -> bool {
        self.save_sent
            .unwrap_or_else(|| !self.smtp.host.to_lowercase().ends_with("gmail.com"))
    }
}

/// Reads the accounts file; a missing file means no configured account.
pub fn load_accounts(path: &Path) -> SyncResult<Vec<AccountConfig>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(err.into()),
    }
}

pub fn save_accounts(path: &Path, accounts: &[AccountConfig]) -> SyncResult<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(accounts)?)?;
    Ok(())
}

/// Secrets in the OS credential store.
pub mod credentials {
    use super::*;

    fn entry(service: &str, user: &str) -> SyncResult<keyring::Entry> {
        keyring::Entry::new(service, user).map_err(|err| SyncError::Credentials(err.to_string()))
    }

    pub fn get(service: &str, user: &str) -> SyncResult<String> {
        entry(service, user)?
            .get_password()
            .map_err(|err| SyncError::Credentials(err.to_string()))
    }

    pub fn set(service: &str, user: &str, secret: &str) -> SyncResult<()> {
        entry(service, user)?
            .set_password(secret)
            .map_err(|err| SyncError::Credentials(err.to_string()))
    }

    pub fn delete(service: &str, user: &str) -> SyncResult<()> {
        entry(service, user)?
            .delete_credential()
            .map_err(|err| SyncError::Credentials(err.to_string()))
    }

    /// Account password (set it from the account settings dialog with [`set_password`]).
    pub fn password(email: &str) -> SyncResult<String> {
        get(PASSWORD_SERVICE, email)
    }

    pub fn set_password(email: &str, password: &str) -> SyncResult<()> {
        set(PASSWORD_SERVICE, email, password)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_and_oauth_configs() {
        let json = r#"[
            {"email": "me@example.com",
             "imap": {"host": "imap.example.com", "port": 993},
             "smtp": {"host": "smtp.example.com", "port": 587, "security": "starttls"}},
            {"email": "me@gmail.com", "username": "me",
             "imap": {"host": "imap.gmail.com", "port": 993},
             "smtp": {"host": "smtp.gmail.com", "port": 465},
             "auth": {"type": "oauth2", "provider": "google"}, "offline_days": 7}
        ]"#;
        let accounts: Vec<AccountConfig> = serde_json::from_str(json).unwrap();
        let plain = &accounts[0];
        assert_eq!(plain.auth, AuthConfig::Password);
        assert_eq!(plain.imap.security, Security::Tls);
        assert_eq!(plain.smtp.security, Security::StartTls);
        assert_eq!(
            (plain.offline_days, plain.poll_minutes, plain.idle),
            (30, 5, true)
        );
        assert_eq!(plain.login(), "me@example.com");
        assert!(plain.save_sent_copy());

        let gmail = &accounts[1];
        assert_eq!(
            gmail.auth,
            AuthConfig::OAuth2 {
                provider: OAuthProvider::Google,
                client_id: None
            }
        );
        assert_eq!(gmail.login(), "me");
        assert_eq!(gmail.offline_days, 7);
        assert!(!gmail.save_sent_copy());
    }

    #[test]
    fn missing_file_is_empty_and_roundtrips() {
        let dir = std::env::temp_dir().join(format!("mailbox-sync-config-{}", std::process::id()));
        let path = dir.join("sync.json");
        assert!(load_accounts(&path).unwrap().is_empty());
        let account = AccountConfig {
            email: "a@b.c".into(),
            username: None,
            imap: ServerConfig {
                host: "localhost".into(),
                port: 143,
                security: Security::Plain,
            },
            smtp: ServerConfig {
                host: "localhost".into(),
                port: 25,
                security: Security::Plain,
            },
            auth: AuthConfig::Password,
            offline_days: 30,
            poll_minutes: 5,
            idle: true,
            save_sent: Some(true),
        };
        save_accounts(&path, std::slice::from_ref(&account)).unwrap();
        assert_eq!(load_accounts(&path).unwrap(), vec![account]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
