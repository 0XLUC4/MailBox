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
//! `client_id` or the `MAILBOX_GOOGLE_CLIENT_ID` / `MAILBOX_MS_CLIENT_ID` env vars.
//!
//! Accounts added in the app keep their servers in the store (`AccountServers`, see
//! [`AccountConfig::from_servers`]); `sync.json` is an optional manual override.
//! [`discover`] finds server settings for domains missing from the built-in table.

use std::path::Path;

use mailbox_core::settings::{self as core_settings, AccountServers};
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

impl OAuthProvider {
    /// The provider whose OAuth sign-in serves this IMAP host, if any.
    pub fn for_host(imap_host: &str) -> Option<Self> {
        let host = imap_host.to_lowercase();
        if host.ends_with("gmail.com") || host.ends_with("googlemail.com") {
            Some(Self::Google)
        } else if host.ends_with("office365.com") || host.ends_with("outlook.com") {
            Some(Self::Microsoft)
        } else {
            None
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Google => "Google",
            Self::Microsoft => "Microsoft",
        }
    }
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

impl From<core_settings::Security> for Security {
    fn from(security: core_settings::Security) -> Self {
        match security {
            core_settings::Security::Tls => Self::Tls,
            core_settings::Security::StartTls => Self::StartTls,
            core_settings::Security::None => Self::Plain,
        }
    }
}

impl From<&core_settings::ServerEndpoint> for ServerConfig {
    fn from(endpoint: &core_settings::ServerEndpoint) -> Self {
        Self {
            host: endpoint.host.clone(),
            port: endpoint.port,
            security: endpoint.security.into(),
        }
    }
}

/// Setting key holding an account's [`AuthConfig`] (JSON) next to its `AccountServers`.
pub fn auth_setting_key(account_id: mailbox_core::Id) -> String {
    format!("account.{account_id}.auth")
}

impl AccountConfig {
    /// Sync settings for an account configured in the app (default sync options).
    pub fn from_servers(email: &str, servers: &AccountServers, auth: AuthConfig) -> Self {
        let username = servers.username.trim();
        Self {
            email: email.to_owned(),
            username: (!username.is_empty() && !username.eq_ignore_ascii_case(email))
                .then(|| username.to_owned()),
            imap: (&servers.imap).into(),
            smtp: (&servers.smtp).into(),
            auth,
            offline_days: default_offline_days(),
            poll_minutes: default_poll_minutes(),
            idle: true,
            save_sent: None,
        }
    }

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

    /// Removes the account's password and OAuth refresh token (missing entries are fine).
    pub fn forget_account(email: &str) {
        // Best effort: an entry that was never stored cannot be deleted.
        let _ = delete(PASSWORD_SERVICE, email);
        let _ = delete(crate::oauth::REFRESH_TOKEN_SERVICE, email);
    }
}

/// Server settings lookup for domains missing from the built-in provider table, in order:
/// Mozilla ISPDB, the domain's own autoconfig file, then the ISPDB entry of the domain's
/// mail exchanger (custom domains hosted by Google, Microsoft...). Only the domain name is
/// ever sent; the address itself stays local. Blocking (network): run it off the UI thread.
pub fn discover(email: &str) -> Option<AccountServers> {
    let domain = core_settings::email_domain(email)?;
    let agent = oauth2::ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(8))
        .redirects(2)
        .build();
    let fetch = |url: String| -> Option<AccountServers> {
        let xml = agent.get(&url).call().ok()?.into_string().ok()?;
        parse_autoconfig(&xml, email)
    };
    fetch(format!("https://autoconfig.thunderbird.net/v1.1/{domain}"))
        .or_else(|| fetch(format!("https://autoconfig.{domain}/mail/config-v1.1.xml")))
        .or_else(|| {
            let exchanger = mx_base_domain(&domain)?;
            (exchanger != domain)
                .then(|| {
                    fetch(format!(
                        "https://autoconfig.thunderbird.net/v1.1/{exchanger}"
                    ))
                })
                .flatten()
        })
}

/// Reads a Thunderbird autoconfig (`clientConfig` v1.1) document: the first IMAP and SMTP
/// servers, with `%EMAILADDRESS%`-style placeholders filled in.
pub fn parse_autoconfig(xml: &str, email: &str) -> Option<AccountServers> {
    let local = email.split('@').next().unwrap_or_default();
    let domain = core_settings::email_domain(email).unwrap_or_default();
    let fill = |value: &str| {
        value
            .replace("%EMAILADDRESS%", email)
            .replace("%EMAILLOCALPART%", local)
            .replace("%EMAILDOMAIN%", &domain)
    };
    let endpoint = |block: &str| -> Option<core_settings::ServerEndpoint> {
        let security = match xml_tag(block, "socketType")?.to_ascii_uppercase().as_str() {
            "SSL" => core_settings::Security::Tls,
            "STARTTLS" => core_settings::Security::StartTls,
            _ => core_settings::Security::None,
        };
        Some(core_settings::ServerEndpoint::new(
            fill(xml_tag(block, "hostname")?),
            xml_tag(block, "port")?.parse().ok()?,
            security,
        ))
    };
    let imap = xml_block(xml, "incomingServer", "imap")?;
    let smtp = xml_block(xml, "outgoingServer", "smtp")?;
    Some(AccountServers {
        provider: xml_tag(xml, "displayShortName")
            .or_else(|| xml_tag(xml, "displayName"))
            .unwrap_or_default()
            .to_owned(),
        username: xml_tag(imap, "username").map_or_else(|| email.to_owned(), fill),
        imap: endpoint(imap)?,
        smtp: endpoint(smtp)?,
    })
}

/// Inner text of the first `<name ...>` element (trimmed).
fn xml_tag<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = xml
        .find(&format!("<{name}>"))
        .or_else(|| xml.find(&format!("<{name} ")))?;
    let start = open + xml[open..].find('>')? + 1;
    let end = start + xml[start..].find(&format!("</{name}>"))?;
    Some(xml[start..end].trim())
}

/// Content of the first `<name type="kind">` element.
fn xml_block<'a>(xml: &'a str, name: &str, kind: &str) -> Option<&'a str> {
    let close = format!("</{name}>");
    let wanted = format!("type=\"{kind}\"");
    let mut rest = xml;
    while let Some(open) = rest.find(&format!("<{name} ")) {
        let tag_end = open + rest[open..].find('>')?;
        let end = tag_end + rest[tag_end..].find(&close)?;
        if rest[open..tag_end].contains(&wanted) {
            return Some(&rest[tag_end + 1..end]);
        }
        rest = &rest[end + close.len()..];
    }
    None
}

/// Registrable-looking domain (last two labels) of the first MX host, via `nslookup`.
fn mx_base_domain(domain: &str) -> Option<String> {
    let mut command = std::process::Command::new("nslookup");
    command.args(["-type=mx", domain]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let output = command.output().ok()?;
    let host = parse_mx_host(&String::from_utf8_lossy(&output.stdout))?;
    let labels: Vec<&str> = host.split('.').filter(|label| !label.is_empty()).collect();
    (labels.len() >= 2).then(|| labels[labels.len() - 2..].join(".").to_lowercase())
}

/// First `mail exchanger = [pref] host` of `nslookup` output (Windows and Unix formats).
fn parse_mx_host(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let (_, rest) = line.split_once("mail exchanger = ")?;
        let host = rest.split_whitespace().last()?.trim_end_matches('.');
        (!host.is_empty()).then(|| host.to_owned())
    })
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

    const ISPDB: &str = r#"<?xml version="1.0"?>
<clientConfig version="1.1">
  <emailProvider id="example.org">
    <domain>example.org</domain>
    <displayName>Example Mail Service</displayName>
    <displayShortName>Example</displayShortName>
    <incomingServer type="pop3">
      <hostname>pop.example.org</hostname><port>995</port><socketType>SSL</socketType>
    </incomingServer>
    <incomingServer type="imap">
      <hostname>imap.example.org</hostname>
      <port>993</port>
      <socketType>SSL</socketType>
      <authentication>password-cleartext</authentication>
      <username>%EMAILLOCALPART%</username>
    </incomingServer>
    <outgoingServer type="smtp">
      <hostname>smtp.%EMAILDOMAIN%</hostname>
      <port>587</port>
      <socketType>STARTTLS</socketType>
      <username>%EMAILADDRESS%</username>
    </outgoingServer>
  </emailProvider>
</clientConfig>"#;

    #[test]
    fn parses_thunderbird_autoconfig() {
        use core_settings::{Security as S, ServerEndpoint};
        let servers = parse_autoconfig(ISPDB, "jo@example.org").unwrap();
        assert_eq!(servers.provider, "Example");
        assert_eq!(servers.username, "jo");
        assert_eq!(
            servers.imap,
            ServerEndpoint::new("imap.example.org", 993, S::Tls)
        );
        assert_eq!(
            servers.smtp,
            ServerEndpoint::new("smtp.example.org", 587, S::StartTls)
        );
        assert!(parse_autoconfig("<clientConfig/>", "jo@example.org").is_none());
    }

    #[test]
    fn parses_nslookup_mx_output() {
        let windows = "Server:  router\nAddress:  192.168.1.1\n\n\
                       example.org\tMX preference = 5, mail exchanger = aspmx.l.google.com\n";
        assert_eq!(
            parse_mx_host(windows).as_deref(),
            Some("aspmx.l.google.com")
        );
        let unix = "example.org\tmail exchanger = 10 mx1.mail.protection.outlook.com.\n";
        assert_eq!(
            parse_mx_host(unix).as_deref(),
            Some("mx1.mail.protection.outlook.com")
        );
        assert_eq!(parse_mx_host("no answer"), None);
    }

    #[test]
    fn config_from_app_servers() {
        use core_settings::{Security as S, ServerEndpoint};
        let servers = AccountServers {
            provider: "Gmail".into(),
            username: "me@gmail.com".into(),
            imap: ServerEndpoint::new("imap.gmail.com", 993, S::Tls),
            smtp: ServerEndpoint::new("localhost", 25, S::None),
        };
        let config = AccountConfig::from_servers("me@gmail.com", &servers, AuthConfig::Password);
        assert_eq!(config.username, None);
        assert_eq!(config.smtp.security, Security::Plain);
        assert_eq!(
            OAuthProvider::for_host(&config.imap.host),
            Some(OAuthProvider::Google)
        );
        assert_eq!(
            OAuthProvider::for_host("outlook.office365.com"),
            Some(OAuthProvider::Microsoft)
        );
        assert_eq!(OAuthProvider::for_host("imap.example.org"), None);
    }
}
