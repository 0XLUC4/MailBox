//! Service discovery (RFC 6764 + 4791/6352): start URL or `/.well-known/caldav|carddav`,
//! `current-user-principal`, `calendar-home-set` / `addressbook-home-set`, then the
//! calendars and address books of the home.

use serde::{Deserialize, Serialize};
use url::Url;

use super::http::{DavClient, HttpResponse, Request, resolve, status_error};
use super::xml::{self, COLLECTION_PROPS, Response};
use crate::{SyncError, SyncResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Calendar,
    AddressBook,
}

impl Kind {
    fn well_known(self) -> &'static str {
        match self {
            Self::Calendar => "/.well-known/caldav",
            Self::AddressBook => "/.well-known/carddav",
        }
    }

    fn home_prop(self) -> (&'static str, &'static str) {
        match self {
            Self::Calendar => ("c:calendar-home-set", "calendar-home-set"),
            Self::AddressBook => ("card:addressbook-home-set", "addressbook-home-set"),
        }
    }

    /// File extension of a resource.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Calendar => "ics",
            Self::AddressBook => "vcf",
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Self::Calendar => "text/calendar; charset=utf-8",
            Self::AddressBook => "text/vcard; charset=utf-8",
        }
    }
}

/// A calendar or address book on the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteCollection {
    pub kind: Kind,
    /// Absolute URL of the collection.
    pub url: String,
    pub name: String,
    /// `0xRRGGBB` from `calendar-color`.
    pub color: Option<u32>,
    pub ctag: Option<String>,
    /// Present when the server supports RFC 6578 sync-collection.
    pub sync_token: Option<String>,
}

/// Home collection of one kind, found once and cached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Home {
    pub kind: Kind,
    pub url: String,
    /// Server handles scheduling (RFC 6638 `calendar-auto-schedule`): invitations are sent
    /// by the server when an event with attendees is saved.
    #[serde(default)]
    pub auto_schedule: bool,
}

/// Finds the home of `kind` starting from `start` (a server root, principal or home URL).
pub fn find_home(client: &DavClient, start: &Url, kind: Kind) -> SyncResult<Home> {
    let principal = find_principal(client, start, kind)?;
    let (prop, name) = kind.home_prop();
    let response = client.send(
        Request::new("PROPFIND", principal.clone())
            .depth("0")
            .xml(xml::propfind(&[prop])),
    )?;
    let status = response.multistatus()?;
    let href = status
        .responses
        .iter()
        .find_map(|r| r.href_of(name))
        .ok_or_else(|| SyncError::Protocol(format!("server has no {name}")))?;
    let url = resolve(&response.url, href)?;
    let auto_schedule = kind == Kind::Calendar && supports_auto_schedule(client, &url);
    Ok(Home {
        kind,
        url: url.into(),
        auto_schedule,
    })
}

/// `current-user-principal` from the start URL, else from the well-known URL of its host.
fn find_principal(client: &DavClient, start: &Url, kind: Kind) -> SyncResult<Url> {
    let mut last_error = None;
    let well_known = resolve(start, kind.well_known())?;
    for url in [start.clone(), well_known] {
        match principal_at(client, &url) {
            Ok(Some(principal)) => return Ok(principal),
            Ok(None) => {}
            // Wrong password: no point trying another URL.
            Err(err @ SyncError::Auth(_)) => return Err(err),
            Err(err) => last_error = Some(err),
        }
    }
    Err(last_error
        .unwrap_or_else(|| SyncError::Protocol(format!("no DAV principal found at {start}"))))
}

fn principal_at(client: &DavClient, url: &Url) -> SyncResult<Option<Url>> {
    let response = client.send(
        Request::new("PROPFIND", url.clone())
            .depth("0")
            .xml(xml::propfind(&["d:current-user-principal"])),
    )?;
    let status = response.multistatus()?;
    status
        .responses
        .iter()
        .find_map(|r| r.href_of("current-user-principal"))
        .map(|href| resolve(&response.url, href))
        .transpose()
}

fn supports_auto_schedule(client: &DavClient, url: &Url) -> bool {
    client
        .send(Request::new("OPTIONS", url.clone()))
        .ok()
        .and_then(|response: HttpResponse| response.dav)
        .is_some_and(|dav| dav.contains("calendar-auto-schedule"))
}

/// Calendars (with events) or address books directly under the home.
pub fn list_collections(client: &DavClient, home: &Home) -> SyncResult<Vec<RemoteCollection>> {
    let url = Url::parse(&home.url).map_err(|err| SyncError::Protocol(err.to_string()))?;
    let response = client.send(
        Request::new("PROPFIND", url)
            .depth("1")
            .xml(xml::propfind(COLLECTION_PROPS)),
    )?;
    if !response.is_success() {
        return Err(status_error(response.status, &home.url));
    }
    let status = response.multistatus()?;
    let mut found = Vec::new();
    for entry in &status.responses {
        if is_collection_of(entry, home.kind) {
            let url = resolve(&response.url, &entry.href)?;
            found.push(collection(entry, home.kind, url));
        }
    }
    Ok(found)
}

fn is_collection_of(entry: &Response, kind: Kind) -> bool {
    match kind {
        Kind::Calendar => {
            entry.has_child("resourcetype", "calendar")
                && entry
                    .props
                    .get("supported-calendar-component-set")
                    .is_none_or(|set| {
                        set.names.is_empty() || set.names.iter().any(|n| n == "VEVENT")
                    })
        }
        Kind::AddressBook => entry.has_child("resourcetype", "addressbook"),
    }
}

fn collection(entry: &Response, kind: Kind, mut url: Url) -> RemoteCollection {
    // New resources are created relative to it.
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    let fallback = url
        .path_segments()
        .and_then(|mut segments| segments.rfind(|s| !s.is_empty()))
        .unwrap_or("Calendar")
        .to_owned();
    RemoteCollection {
        kind,
        url: url.into(),
        name: entry.text("displayname").map_or(fallback, str::to_owned),
        color: entry.text("calendar-color").and_then(parse_color),
        ctag: entry.text("getctag").map(str::to_owned),
        sync_token: entry.text("sync-token").map(str::to_owned),
    }
}

/// `#RRGGBB` or `#RRGGBBAA` -> `0xRRGGBB`.
pub fn parse_color(value: &str) -> Option<u32> {
    let hex = value.trim().strip_prefix('#')?;
    let rgb = hex.get(..6)?;
    u32::from_str_radix(rgb, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        assert_eq!(parse_color("#FF8800FF"), Some(0xFF8800));
        assert_eq!(parse_color("#0a0B0c"), Some(0x0A0B0C));
        assert_eq!(parse_color("red"), None);
    }
}
