//! Two-way sync of one collection, as pure logic over a [`DavClient`] (no store access).
//!
//! 1. Remote changes: RFC 6578 `sync-collection` when the server has sync tokens, else the
//!    `getctag` shortcut + a `getetag` listing compared with the known etags.
//! 2. Changed resources are downloaded with `calendar-multiget` / `addressbook-multiget`.
//! 3. Local changes (content hash differs from the last synced one) are pushed with
//!    `If-Match`; new items with `If-None-Match: *`; deletions with `If-Match`.
//! 4. Conflicts (changed on both sides, or `412` on push): the server wins and the local
//!    version is kept as a copy ([`CollectionOutcome::conflicts`]).
//!
//! The caller applies [`RemoteChange`]s to the store, then records links with
//! [`CollectionState::link`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use url::Url;

use super::discovery::{Home, Kind, RemoteCollection};
use super::http::{DavClient, Request, resolve, status_error};
use super::items::{LocalItem, resource_name};
use super::xml;
use crate::{SyncError, SyncResult};
use mailbox_core::Id;

/// Resources per multiget request.
const MULTIGET_CHUNK: usize = 50;

/// Persisted state of one DAV account (`<data dir>/dav/<key>.json`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DavState {
    #[serde(default)]
    pub homes: Vec<Home>,
    /// Keyed by collection URL.
    #[serde(default)]
    pub collections: BTreeMap<String, CollectionState>,
}

impl DavState {
    pub fn load(path: &Path) -> SyncResult<Self> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err.into()),
        }
    }

    /// Writes atomically (temp file + rename).
    pub fn save(&self, path: &Path) -> SyncResult<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_vec(self)?)?;
        std::fs::rename(&temp, path)?;
        Ok(())
    }

    pub fn home(&self, kind: Kind) -> Option<&Home> {
        self.homes.iter().find(|home| home.kind == kind)
    }

    /// Collection mirrored by a local calendar.
    pub fn collection_of_calendar(&self, calendar_id: Id) -> Option<&CollectionState> {
        self.collections
            .values()
            .find(|c| c.kind == Kind::Calendar && c.local_id == Some(calendar_id))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectionState {
    pub kind: Kind,
    pub url: String,
    pub name: String,
    #[serde(default)]
    pub color: Option<u32>,
    /// Local calendar mirroring it (address books map to the account's contacts).
    #[serde(default)]
    pub local_id: Option<Id>,
    #[serde(default)]
    pub ctag: Option<String>,
    #[serde(default)]
    pub sync_token: Option<String>,
    /// Keyed by resource URL.
    #[serde(default)]
    pub items: BTreeMap<String, ItemLink>,
}

/// A server resource stored locally.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemLink {
    pub local_id: Id,
    pub uid: String,
    pub etag: String,
    /// [`LocalItem::hash`] when last synced.
    pub hash: u64,
}

impl CollectionState {
    pub fn new(remote: &RemoteCollection) -> Self {
        Self {
            kind: remote.kind,
            url: remote.url.clone(),
            name: remote.name.clone(),
            color: remote.color,
            local_id: None,
            ctag: None,
            sync_token: None,
            items: BTreeMap::new(),
        }
    }

    pub fn link(&mut self, href: String, link: ItemLink) {
        self.items
            .retain(|h, l| *h == href || l.local_id != link.local_id);
        self.items.insert(href, link);
    }

    pub fn uid_of(&self, local_id: Id) -> Option<&str> {
        self.items
            .values()
            .find(|link| link.local_id == local_id)
            .map(|link| link.uid.as_str())
    }
}

/// What to do to the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteChange {
    /// Create (`local_id` None) or replace a local item with the server version.
    Upsert {
        href: String,
        etag: String,
        text: String,
        local_id: Option<Id>,
    },
    /// The server deleted it.
    Delete { local_id: Id },
}

#[derive(Debug, Clone, PartialEq)]
pub struct CollectionOutcome {
    pub state: CollectionState,
    /// Apply in order.
    pub changes: Vec<RemoteChange>,
    /// Local items to duplicate (as a "conflict copy") before applying `changes`.
    pub conflicts: Vec<Id>,
    pub pushed: usize,
    /// Non-fatal per-item failures.
    pub errors: Vec<String>,
}

/// Remote changes since the last sync.
#[derive(Debug, Default)]
struct RemoteDelta {
    /// href -> etag
    changed: BTreeMap<String, String>,
    removed: Vec<String>,
    token: Option<String>,
}

/// Syncs one collection. `local` lists every local item of the collection.
pub fn sync_collection(
    client: &DavClient,
    remote: &RemoteCollection,
    mut state: CollectionState,
    local: &[LocalItem],
) -> SyncResult<CollectionOutcome> {
    let base = Url::parse(&remote.url).map_err(|err| SyncError::Protocol(err.to_string()))?;
    let delta = remote_delta(client, &base, remote, &state)?;
    state.name.clone_from(&remote.name);
    state.color = remote.color;

    // Our own pushes come back with the etag we already know.
    let changed: BTreeMap<String, String> = delta
        .changed
        .into_iter()
        .filter(|(href, etag)| state.items.get(href).is_none_or(|l| &l.etag != etag))
        .collect();
    let mut downloaded = download(client, &base, remote.kind, &changed)?;
    let removed: HashSet<String> = delta.removed.into_iter().collect();

    let mut outcome = CollectionOutcome {
        state: state.clone(),
        changes: Vec::new(),
        conflicts: Vec::new(),
        pushed: 0,
        errors: Vec::new(),
    };
    let local_by_id: HashMap<Id, &LocalItem> = local.iter().map(|i| (i.local_id, i)).collect();
    let mut linked: HashSet<Id> = HashSet::new();

    for (href, link) in &state.items {
        linked.insert(link.local_id);
        let local_item = local_by_id.get(&link.local_id).copied();
        let locally_changed = local_item.is_some_and(|item| item.hash() != link.hash);
        if let Some((etag, text)) = downloaded.remove(href) {
            // Changed on the server: server wins.
            if locally_changed {
                outcome.conflicts.push(link.local_id);
            }
            outcome.changes.push(RemoteChange::Upsert {
                href: href.clone(),
                etag,
                text,
                local_id: local_item.map(|item| item.local_id),
            });
        } else if removed.contains(href) {
            outcome.state.items.remove(href);
            match local_item {
                // Edited here, deleted there: keep the local version as a new item.
                Some(item) if locally_changed => {
                    push_new(client, &base, remote.kind, item, &mut outcome)
                }
                Some(item) => outcome.changes.push(RemoteChange::Delete {
                    local_id: item.local_id,
                }),
                None => {}
            }
        } else {
            match local_item {
                None => delete_remote(client, &base, href, link, &mut outcome)?,
                Some(item) if locally_changed => {
                    update_remote(client, &base, remote.kind, href, link, item, &mut outcome)?
                }
                Some(_) => {}
            }
        }
    }
    // New on the server.
    for (href, (etag, text)) in downloaded {
        outcome.changes.push(RemoteChange::Upsert {
            href,
            etag,
            text,
            local_id: None,
        });
    }
    // New here.
    for item in local.iter().filter(|item| !linked.contains(&item.local_id)) {
        push_new(client, &base, remote.kind, item, &mut outcome);
    }
    outcome.state.sync_token = delta.token.or_else(|| remote.sync_token.clone());
    outcome.state.ctag.clone_from(&remote.ctag);
    Ok(outcome)
}

fn remote_delta(
    client: &DavClient,
    base: &Url,
    remote: &RemoteCollection,
    state: &CollectionState,
) -> SyncResult<RemoteDelta> {
    if remote.sync_token.is_some() {
        // A first sync (no token) asks for everything.
        let token = if state.items.is_empty() {
            ""
        } else {
            state.sync_token.as_deref().unwrap_or("")
        };
        match sync_report(client, base, token) {
            Ok(delta) => return Ok(delta),
            // Expired/invalid token or unsupported: full comparison below.
            Err(SyncError::Protocol(_)) => {}
            Err(err) => return Err(err),
        }
    }
    if remote.ctag.is_some() && remote.ctag == state.ctag && state.sync_token.is_none() {
        return Ok(RemoteDelta::default());
    }
    etag_listing(client, base, state)
}

fn sync_report(client: &DavClient, base: &Url, token: &str) -> SyncResult<RemoteDelta> {
    let response = client.send(
        Request::new("REPORT", base.clone())
            .depth("1")
            .xml(xml::sync_collection(token)),
    )?;
    let status = response.multistatus()?;
    let mut delta = RemoteDelta {
        token: status.sync_token,
        ..RemoteDelta::default()
    };
    for entry in status.responses {
        let href = canonical(&response.url, &entry.href)?;
        if href == base.as_str() {
            continue;
        }
        if entry.status == Some(404) {
            delta.removed.push(href);
        } else if let Some(etag) = entry.text("getetag") {
            delta.changed.insert(href, etag.to_owned());
        }
    }
    Ok(delta)
}

fn etag_listing(
    client: &DavClient,
    base: &Url,
    state: &CollectionState,
) -> SyncResult<RemoteDelta> {
    let response = client.send(
        Request::new("PROPFIND", base.clone())
            .depth("1")
            .xml(xml::propfind(&["d:getetag", "d:resourcetype"])),
    )?;
    let status = response.multistatus()?;
    let mut delta = RemoteDelta::default();
    for entry in status.responses {
        let href = canonical(&response.url, &entry.href)?;
        if href == base.as_str() || entry.has_child("resourcetype", "collection") {
            continue;
        }
        if let Some(etag) = entry.text("getetag") {
            delta.changed.insert(href, etag.to_owned());
        }
    }
    delta.removed = state
        .items
        .keys()
        .filter(|href| !delta.changed.contains_key(*href))
        .cloned()
        .collect();
    Ok(delta)
}

/// Downloads resources: href -> (etag, text).
fn download(
    client: &DavClient,
    base: &Url,
    kind: Kind,
    wanted: &BTreeMap<String, String>,
) -> SyncResult<BTreeMap<String, (String, String)>> {
    let data_prop = match kind {
        Kind::Calendar => "calendar-data",
        Kind::AddressBook => "address-data",
    };
    let hrefs: Vec<String> = wanted.keys().cloned().collect();
    let mut found = BTreeMap::new();
    for chunk in hrefs.chunks(MULTIGET_CHUNK) {
        let paths: Vec<String> = chunk
            .iter()
            .map(|href| Url::parse(href).map_or_else(|_| href.clone(), |u| u.path().to_owned()))
            .collect();
        let response = client.send(
            Request::new("REPORT", base.clone())
                .depth("1")
                .xml(xml::multiget(kind == Kind::Calendar, &paths)),
        )?;
        for entry in response.multistatus()?.responses {
            let href = canonical(&response.url, &entry.href)?;
            if let Some(text) = entry.text(data_prop) {
                let etag = entry
                    .text("getetag")
                    .map(str::to_owned)
                    .or_else(|| wanted.get(&href).cloned())
                    .unwrap_or_default();
                found.insert(href, (etag, text.to_owned()));
            }
        }
    }
    Ok(found)
}

fn delete_remote(
    client: &DavClient,
    base: &Url,
    href: &str,
    link: &ItemLink,
    outcome: &mut CollectionOutcome,
) -> SyncResult<()> {
    let url = resolve(base, href)?;
    let response =
        client.send(Request::new("DELETE", url).header("If-Match", link.etag.clone()))?;
    match response.status {
        200..=299 | 404 | 410 => {
            outcome.state.items.remove(href);
            outcome.pushed += 1;
        }
        // Changed on the server since: server wins, bring it back.
        412 => refetch(client, base, href, None, outcome)?,
        status => outcome.errors.push(status_error(status, href).to_string()),
    }
    Ok(())
}

fn update_remote(
    client: &DavClient,
    base: &Url,
    kind: Kind,
    href: &str,
    link: &ItemLink,
    item: &LocalItem,
    outcome: &mut CollectionOutcome,
) -> SyncResult<()> {
    let url = resolve(base, href)?;
    let response = client.send(
        Request::new("PUT", url.clone())
            .header("If-Match", link.etag.clone())
            .body(kind.content_type(), item.text.clone()),
    )?;
    match response.status {
        200..=299 => {
            let etag = match response.etag {
                Some(etag) => etag,
                None => fetch_etag(client, &url)?,
            };
            outcome.state.link(
                href.to_owned(),
                ItemLink {
                    etag,
                    hash: item.hash(),
                    ..link.clone()
                },
            );
            outcome.pushed += 1;
        }
        412 => {
            outcome.conflicts.push(item.local_id);
            refetch(client, base, href, Some(item.local_id), outcome)?;
        }
        status => outcome.errors.push(status_error(status, href).to_string()),
    }
    Ok(())
}

fn push_new(
    client: &DavClient,
    base: &Url,
    kind: Kind,
    item: &LocalItem,
    outcome: &mut CollectionOutcome,
) {
    let result = (|| -> SyncResult<()> {
        let url = resolve(base, &resource_name(&item.uid, kind.extension()))?;
        let response = client.send(
            Request::new("PUT", url.clone())
                .header("If-None-Match", "*")
                .body(kind.content_type(), item.text.clone()),
        )?;
        if !response.is_success() {
            return Err(status_error(response.status, url.as_str()));
        }
        let etag = match response.etag {
            Some(etag) => etag,
            None => fetch_etag(client, &url)?,
        };
        outcome.state.link(
            url.into(),
            ItemLink {
                local_id: item.local_id,
                uid: item.uid.clone(),
                etag,
                hash: item.hash(),
            },
        );
        outcome.pushed += 1;
        Ok(())
    })();
    if let Err(err) = result {
        outcome.errors.push(err.to_string());
    }
}

/// Downloads one resource again (after a `412`) and queues it as the server version.
fn refetch(
    client: &DavClient,
    base: &Url,
    href: &str,
    local_id: Option<Id>,
    outcome: &mut CollectionOutcome,
) -> SyncResult<()> {
    let response = client.send(Request::new("GET", resolve(base, href)?))?;
    if response.status == 404 {
        outcome.state.items.remove(href);
        return Ok(());
    }
    if !response.is_success() {
        return Err(status_error(response.status, href));
    }
    outcome.changes.push(RemoteChange::Upsert {
        href: href.to_owned(),
        etag: response.etag.unwrap_or_default(),
        text: response.body,
        local_id,
    });
    Ok(())
}

fn fetch_etag(client: &DavClient, url: &Url) -> SyncResult<String> {
    let response = client.send(
        Request::new("PROPFIND", url.clone())
            .depth("0")
            .xml(xml::propfind(&["d:getetag"])),
    )?;
    Ok(response
        .multistatus()?
        .responses
        .iter()
        .find_map(|r| r.text("getetag").map(str::to_owned))
        .unwrap_or_default())
}

/// Absolute URL of an href (one spelling per resource in the state).
fn canonical(base: &Url, href: &str) -> SyncResult<String> {
    resolve(base, href).map(String::from)
}

/// Finds the homes (cached in `state`) and lists the server collections of `kinds`.
pub fn refresh_collections(
    client: &DavClient,
    state: &mut DavState,
    kinds: &[Kind],
    start_url: impl Fn(Kind) -> SyncResult<Url>,
) -> SyncResult<Vec<RemoteCollection>> {
    let mut collections = Vec::new();
    for &kind in kinds {
        let home = match state.home(kind) {
            Some(home) => home.clone(),
            None => {
                let home = super::discovery::find_home(client, &start_url(kind)?, kind)?;
                state.homes.push(home.clone());
                home
            }
        };
        match super::discovery::list_collections(client, &home) {
            Ok(found) => collections.extend(found),
            Err(err) => {
                // The home may have moved: discover again next time.
                state.homes.retain(|h| h.kind != kind);
                return Err(err);
            }
        }
    }
    Ok(collections)
}

/// Result of one account cycle.
#[derive(Debug)]
pub struct AccountOutcome {
    /// Homes and collections found; collection states are updated by the caller from
    /// each [`CollectionOutcome::state`] once its changes are applied.
    pub state: DavState,
    pub collections: Vec<(RemoteCollection, SyncResult<CollectionOutcome>)>,
}

/// One cycle: lists collections, then syncs each with its local items (keyed by
/// collection URL; a missing entry means no local item yet).
pub fn sync_account(
    client: &DavClient,
    mut state: DavState,
    kinds: &[Kind],
    start_url: impl Fn(Kind) -> SyncResult<Url>,
    local: &HashMap<String, Vec<LocalItem>>,
) -> SyncResult<AccountOutcome> {
    let remotes = refresh_collections(client, &mut state, kinds, start_url)?;
    // Collections deleted on the server are forgotten (local data is left alone).
    state
        .collections
        .retain(|url, _| remotes.iter().any(|remote| &remote.url == url));
    let collections = remotes
        .into_iter()
        .map(|remote| {
            let collection = state
                .collections
                .get(&remote.url)
                .cloned()
                .unwrap_or_else(|| CollectionState::new(&remote));
            let items = local.get(&remote.url).map_or(&[][..], Vec::as_slice);
            let result = sync_collection(client, &remote, collection, items);
            (remote, result)
        })
        .collect();
    Ok(AccountOutcome { state, collections })
}
