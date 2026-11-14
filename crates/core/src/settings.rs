//! User preferences and per-account server settings, persisted in the `settings` key/value table.
//!
//! Reads: [`Settings::load`]. Writes go through [`Command::SetSetting`] (undoable like any command):
//! `state.run(settings.command()?, cx)`.
//! Also holds the built-in IMAP/SMTP provider table used by the "Add account" wizard.

use rusqlite::Row;
use rusqlite::types::Value;
use serde::{Deserialize, Serialize};

use crate::command::Command;
use crate::error::{Error, Result};
use crate::model::Id;
use crate::store::{Record, Store};

/// Key of the application preferences blob.
const APP_KEY: &str = "app";

/// One row of the `settings` table (value is JSON text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingEntry {
    pub id: Id,
    pub key: String,
    pub value: String,
}

impl Record for SettingEntry {
    const KIND: &'static str = "setting";
    const TABLE: &'static str = "settings";
    const COLUMNS: &'static [&'static str] = &["key", "value"];

    fn id(&self) -> Id {
        self.id
    }
    fn set_id(&mut self, id: Id) {
        self.id = id;
    }
    fn values(&self) -> Result<Vec<Value>> {
        Ok(vec![
            Value::Text(self.key.clone()),
            Value::Text(self.value.clone()),
        ])
    }
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            key: row.get(1)?,
            value: row.get(2)?,
        })
    }
}

fn entry(store: &Store, key: &str) -> Result<Option<SettingEntry>> {
    let entries: Vec<SettingEntry> = store.query("WHERE key = ?1", [key])?;
    Ok(entries.into_iter().next())
}

/// Raw value of a setting, `None` when unset.
pub fn get(store: &Store, key: &str) -> Result<Option<String>> {
    Ok(entry(store, key)?.map(|entry| entry.value))
}

/// Typed value of a JSON setting; `None` when unset.
pub fn get_json<T: serde::de::DeserializeOwned>(store: &Store, key: &str) -> Result<Option<T>> {
    get(store, key)?
        .map(|raw| serde_json::from_str::<T>(&raw).map_err(Error::from))
        .transpose()
}

/// Command that stores `value` (JSON) under `key`.
pub fn set_json_command<T: Serialize>(key: impl Into<String>, value: &T) -> Result<Command> {
    Ok(Command::SetSetting {
        key: key.into(),
        value: Some(serde_json::to_string(value)?),
    })
}

/// Applies [`Command::SetSetting`]; `None` removes the key. Returns the inverse command.
pub(crate) fn apply_set(store: &Store, key: String, value: Option<String>) -> Result<Command> {
    let previous = entry(store, &key)?;
    match value {
        Some(value) => {
            let mut row = SettingEntry {
                id: previous.as_ref().map_or(0, |entry| entry.id),
                key: key.clone(),
                value,
            };
            store.save(&mut row)?;
        }
        None => {
            if let Some(old) = &previous {
                store.delete::<SettingEntry>(old.id)?;
            }
        }
    }
    Ok(Command::SetSetting {
        key,
        value: previous.map(|entry| entry.value),
    })
}

// ---------- application preferences ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Density {
    Compact,
    #[default]
    Comfortable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RibbonMode {
    /// One row of small buttons.
    #[default]
    Simplified,
    /// Large buttons in captioned groups (three rows high).
    Classic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadingPane {
    #[default]
    Right,
    Bottom,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WeekStart {
    #[default]
    Monday,
    Sunday,
    Saturday,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    #[default]
    En,
    Fr,
}

/// Every user preference. Missing fields in the stored JSON fall back to defaults,
/// so adding a field never breaks an existing database.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemePreference,
    /// Accent colour `0xRRGGBB`.
    pub accent: u32,
    pub density: Density,
    /// Base UI font size in px.
    pub font_size: u8,
    pub ribbon: RibbonMode,
    pub reading_pane: ReadingPane,
    /// Group messages by conversation.
    pub conversations: bool,
    /// Seconds before an opened message is marked read; `None` = never automatically.
    pub mark_read_after_secs: Option<u32>,
    pub week_start: WeekStart,
    /// Work hours, `[start, end)` in local hours.
    pub work_start_hour: u8,
    pub work_end_hour: u8,
    pub notify_new_mail: bool,
    pub notify_sound: bool,
    pub notify_reminders: bool,
    pub language: Language,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemePreference::System,
            accent: 0x3b82f6,
            density: Density::Comfortable,
            font_size: 13,
            ribbon: RibbonMode::Simplified,
            reading_pane: ReadingPane::Right,
            conversations: true,
            mark_read_after_secs: Some(0),
            week_start: WeekStart::Monday,
            work_start_hour: 9,
            work_end_hour: 18,
            notify_new_mail: true,
            notify_sound: false,
            notify_reminders: true,
            language: Language::En,
        }
    }
}

impl Settings {
    pub const FONT_SIZES: [u8; 5] = [12, 13, 14, 15, 16];

    /// Stored preferences, defaults when never saved.
    pub fn load(store: &Store) -> Result<Self> {
        Ok(get_json(store, APP_KEY)?.unwrap_or_default())
    }

    /// Command that persists these preferences.
    pub fn command(&self) -> Result<Command> {
        set_json_command(APP_KEY, self)
    }
}

// ---------- account servers ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Security {
    /// Implicit TLS (IMAPS 993, SMTPS 465).
    #[default]
    Tls,
    StartTls,
    None,
}

impl Security {
    pub fn label(self) -> &'static str {
        match self {
            Self::Tls => "SSL/TLS",
            Self::StartTls => "STARTTLS",
            Self::None => "None",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ServerEndpoint {
    pub host: String,
    pub port: u16,
    pub security: Security,
}

impl ServerEndpoint {
    pub fn new(host: impl Into<String>, port: u16, security: Security) -> Self {
        Self {
            host: host.into(),
            port,
            security,
        }
    }
}

/// Connection settings of an account (no secrets: passwords belong to the OS keychain).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountServers {
    /// Provider name when detected ("Gmail"), empty for manual setups.
    pub provider: String,
    pub username: String,
    pub imap: ServerEndpoint,
    pub smtp: ServerEndpoint,
}

fn servers_key(account_id: Id) -> String {
    format!("account.{account_id}.servers")
}

impl AccountServers {
    pub fn load(store: &Store, account_id: Id) -> Result<Option<Self>> {
        get_json(store, &servers_key(account_id))
    }

    pub fn command(&self, account_id: Id) -> Result<Command> {
        set_json_command(servers_key(account_id), self)
    }
}

/// A mail provider with well-known server settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Provider {
    pub name: &'static str,
    /// Email domains served by this provider (lowercase).
    pub domains: &'static [&'static str],
    pub imap: (&'static str, u16, Security),
    pub smtp: (&'static str, u16, Security),
}

impl Provider {
    pub fn servers(&self, email: &str) -> AccountServers {
        AccountServers {
            provider: self.name.to_owned(),
            username: email.trim().to_owned(),
            imap: ServerEndpoint::new(self.imap.0, self.imap.1, self.imap.2),
            smtp: ServerEndpoint::new(self.smtp.0, self.smtp.1, self.smtp.2),
        }
    }
}

/// Built-in provider table (public server settings).
pub const PROVIDERS: &[Provider] = &[
    Provider {
        name: "Gmail",
        domains: &["gmail.com", "googlemail.com"],
        imap: ("imap.gmail.com", 993, Security::Tls),
        smtp: ("smtp.gmail.com", 465, Security::Tls),
    },
    Provider {
        name: "Outlook.com",
        domains: &[
            "outlook.com",
            "outlook.fr",
            "hotmail.com",
            "hotmail.fr",
            "live.com",
            "live.fr",
            "msn.com",
        ],
        imap: ("outlook.office365.com", 993, Security::Tls),
        smtp: ("smtp.office365.com", 587, Security::StartTls),
    },
    Provider {
        name: "iCloud",
        domains: &["icloud.com", "me.com", "mac.com"],
        imap: ("imap.mail.me.com", 993, Security::Tls),
        smtp: ("smtp.mail.me.com", 587, Security::StartTls),
    },
    Provider {
        name: "Yahoo",
        domains: &["yahoo.com", "yahoo.fr", "ymail.com", "rocketmail.com"],
        imap: ("imap.mail.yahoo.com", 993, Security::Tls),
        smtp: ("smtp.mail.yahoo.com", 465, Security::Tls),
    },
    Provider {
        name: "Fastmail",
        domains: &["fastmail.com", "fastmail.fm"],
        imap: ("imap.fastmail.com", 993, Security::Tls),
        smtp: ("smtp.fastmail.com", 465, Security::Tls),
    },
    Provider {
        name: "OVHcloud",
        domains: &["ovh.com", "ovh.net"],
        imap: ("ssl0.ovh.net", 993, Security::Tls),
        smtp: ("ssl0.ovh.net", 465, Security::Tls),
    },
    Provider {
        name: "AOL",
        domains: &["aol.com", "aol.fr"],
        imap: ("imap.aol.com", 993, Security::Tls),
        smtp: ("smtp.aol.com", 465, Security::Tls),
    },
    Provider {
        name: "GMX",
        domains: &["gmx.com", "gmx.fr", "gmx.de", "gmx.net"],
        imap: ("imap.gmx.com", 993, Security::Tls),
        smtp: ("mail.gmx.com", 587, Security::StartTls),
    },
    Provider {
        name: "Zoho Mail",
        domains: &["zoho.com", "zohomail.com"],
        imap: ("imap.zoho.com", 993, Security::Tls),
        smtp: ("smtp.zoho.com", 465, Security::Tls),
    },
    Provider {
        name: "Orange",
        domains: &["orange.fr", "wanadoo.fr"],
        imap: ("imap.orange.fr", 993, Security::Tls),
        smtp: ("smtp.orange.fr", 465, Security::Tls),
    },
    Provider {
        name: "Free",
        domains: &["free.fr"],
        imap: ("imap.free.fr", 993, Security::Tls),
        smtp: ("smtp.free.fr", 465, Security::Tls),
    },
    Provider {
        name: "La Poste",
        domains: &["laposte.net"],
        imap: ("imap.laposte.net", 993, Security::Tls),
        smtp: ("smtp.laposte.net", 465, Security::Tls),
    },
];

/// Lowercase domain of a plausible email address (`a@b.c`), `None` otherwise.
pub fn email_domain(email: &str) -> Option<String> {
    let (local, domain) = email.trim().rsplit_once('@')?;
    let domain = domain.to_lowercase();
    let valid = !local.is_empty()
        && !local.contains(char::is_whitespace)
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    valid.then_some(domain)
}

/// Known provider for an email address.
pub fn detect_provider(email: &str) -> Option<&'static Provider> {
    let domain = email_domain(email)?;
    PROVIDERS
        .iter()
        .find(|provider| provider.domains.contains(&domain.as_str()))
}

/// Detected settings, or the usual `imap.<domain>` / `smtp.<domain>` guess for a manual setup.
pub fn suggest_servers(email: &str) -> Option<AccountServers> {
    if let Some(provider) = detect_provider(email) {
        return Some(provider.servers(email));
    }
    let domain = email_domain(email)?;
    Some(AccountServers {
        provider: String::new(),
        username: email.trim().to_owned(),
        imap: ServerEndpoint::new(format!("imap.{domain}"), 993, Security::Tls),
        smtp: ServerEndpoint::new(format!("smtp.{domain}"), 587, Security::StartTls),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::History;
    use crate::store::tests::store;

    #[test]
    fn settings_default_then_roundtrip_with_undo() {
        let store = store();
        assert_eq!(Settings::load(&store).unwrap(), Settings::default());

        let mut history = History::new();
        let mut settings = Settings {
            theme: ThemePreference::Dark,
            font_size: 15,
            ..Settings::default()
        };
        history.run(&store, settings.command().unwrap()).unwrap();
        assert_eq!(Settings::load(&store).unwrap(), settings);

        settings.ribbon = RibbonMode::Classic;
        history.run(&store, settings.command().unwrap()).unwrap();
        assert_eq!(Settings::load(&store).unwrap().ribbon, RibbonMode::Classic);

        history.undo(&store).unwrap();
        assert_eq!(
            Settings::load(&store).unwrap().ribbon,
            RibbonMode::Simplified
        );
        history.undo(&store).unwrap();
        assert_eq!(Settings::load(&store).unwrap(), Settings::default());
        assert_eq!(get(&store, APP_KEY).unwrap(), None);
    }

    #[test]
    fn partial_json_uses_defaults() {
        let store = store();
        History::new()
            .run(
                &store,
                Command::SetSetting {
                    key: APP_KEY.into(),
                    value: Some(r#"{"theme":"light","language":"fr"}"#.into()),
                },
            )
            .unwrap();
        let settings = Settings::load(&store).unwrap();
        assert_eq!(settings.theme, ThemePreference::Light);
        assert_eq!(settings.language, Language::Fr);
        assert_eq!(settings.work_start_hour, 9);
    }

    #[test]
    fn providers_are_detected() {
        assert_eq!(detect_provider("ada@gmail.com").unwrap().name, "Gmail");
        assert_eq!(
            detect_provider(" Bob@Hotmail.FR ").unwrap().name,
            "Outlook.com"
        );
        assert_eq!(detect_provider("x@me.com").unwrap().name, "iCloud");
        assert!(detect_provider("x@example.org").is_none());
        assert!(detect_provider("not-an-email").is_none());

        let manual = suggest_servers("me@acme.io").unwrap();
        assert_eq!(manual.imap.host, "imap.acme.io");
        assert_eq!(manual.smtp.port, 587);
        assert!(manual.provider.is_empty());
        assert_eq!(suggest_servers("ada@gmail.com").unwrap().smtp.port, 465);
    }

    #[test]
    fn email_validation() {
        assert_eq!(email_domain("a@b.co").as_deref(), Some("b.co"));
        for bad in ["", "@b.co", "a@b", "a@.b", "a b@c.d", "a@b c.d"] {
            assert!(email_domain(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn account_servers_roundtrip() {
        let store = store();
        let servers = suggest_servers("ada@fastmail.com").unwrap();
        History::new()
            .run(&store, servers.command(7).unwrap())
            .unwrap();
        assert_eq!(AccountServers::load(&store, 7).unwrap(), Some(servers));
        assert_eq!(AccountServers::load(&store, 8).unwrap(), None);
    }
}
