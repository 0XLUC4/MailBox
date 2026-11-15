//! Bridge between [`AppState`] and CalDAV/CardDAV sync (`mailbox_sync::dav`).
//!
//! - Accounts live in `<data dir>/dav.json`, passwords in the OS keyring
//!   (`mailbox-dav` / login), per-account state in `<data dir>/dav/<key>.json`.
//! - Every account syncs every `poll_minutes` in the background (network on a background
//!   thread), and on Send/Receive (F9) or [`sync_now`]. Server changes are applied with
//!   `command::apply` (outside the undo history), like mail sync.
//! - Each server calendar maps to a local [`Calendar`]; address books map to the contacts of
//!   the account (new local contacts go to the first address book).
//! - Invitations: [`send_invitation`] lets the server schedule when it supports
//!   `calendar-auto-schedule`, else queues an email with a `METHOD:REQUEST` .ics in the
//!   mail Outbox.
//!
//! Entry points: [`init`], [`sync_now`], [`reload`], [`status`], [`send_invitation`],
//! [`open_add_dav_account`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use gpui::component::WindowExt as _;
use gpui::component::input::{Input, InputState};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::calendar::{self, CalendarCommand, EventDetails, EventSnapshot};
use mailbox_core::people::{self, ContactCard, PeopleCommand};
use mailbox_core::{Account, Address, Calendar, CalendarEvent, Command, Id, command};
use mailbox_sync::SyncResult;
use mailbox_sync::dav::http::{Auth, DavClient};
use mailbox_sync::dav::sync::AccountOutcome;
use mailbox_sync::dav::{
    CollectionOutcome, CollectionState, DavAccount, DavState, ItemLink, Kind, LocalItem, Provider,
    RemoteChange, config as dav_config, discovery, items,
};

use crate::SendReceive;
use crate::app_state::AppState;
use crate::components::{Banner, Button, IconName};
use crate::dialogs::{card, dialog_frame, field};
use crate::settings::controls::segmented;
use crate::settings::tr;
use crate::sync_bridge;
use crate::theme::{DEFAULT_ACCENT, SPACE_1, SPACE_2, SPACE_3, TEXT_LG, TEXT_SM, theme};

/// How often due accounts are checked.
const TICK: Duration = Duration::from_secs(20);
const MAX_BACKOFF: Duration = Duration::from_secs(60 * 60);
const CONFLICT_SUFFIX: &str = " (conflict copy)";

struct GlobalDavBridge(Entity<DavBridge>);

impl Global for GlobalDavBridge {}

/// Creates the bridge, starts periodic sync and joins Send/Receive (F9).
pub fn init(state: Entity<AppState>, cx: &mut App) {
    let bridge = cx.new(|cx| DavBridge::new(state, cx));
    cx.set_global(GlobalDavBridge(bridge));
    cx.on_action(|_: &SendReceive, cx| {
        sync_now(cx);
        // Let the mail bridge handle the same action.
        cx.propagate();
    });
}

fn with_bridge<R>(
    cx: &mut App,
    f: impl FnOnce(&mut DavBridge, &mut Context<DavBridge>) -> R,
) -> Option<R> {
    let bridge = cx.try_global::<GlobalDavBridge>()?.0.clone();
    Some(bridge.update(cx, f))
}

/// Syncs every calendar/contacts account now.
pub fn sync_now(cx: &mut App) {
    with_bridge(cx, |bridge, cx| bridge.sync_all(true, cx));
}

/// Re-reads `dav.json` (after adding an account) and syncs.
pub fn reload(cx: &mut App) {
    with_bridge(cx, |bridge, cx| {
        bridge.load_accounts(cx);
        bridge.sync_all(false, cx);
    });
}

/// One-line status of calendar/contacts sync (`None` without DAV account).
pub fn status(cx: &App) -> Option<SharedString> {
    let bridge = cx.try_global::<GlobalDavBridge>()?.0.read(cx);
    bridge.status_line().map(SharedString::from)
}

/// Local account of the first CardDAV account (where new contacts must go to be synced).
pub fn contacts_account(cx: &App) -> Option<Id> {
    let bridge = cx.try_global::<GlobalDavBridge>()?.0.read(cx);
    bridge
        .accounts
        .iter()
        .find(|account| account.config.contacts)
        .map(|account| account.account_id)
}

/// Whether a local calendar mirrors a CalDAV collection.
pub fn is_synced_calendar(calendar_id: Id, cx: &App) -> bool {
    cx.try_global::<GlobalDavBridge>().is_some_and(|global| {
        global
            .0
            .read(cx)
            .accounts
            .iter()
            .any(|account| account.state.collection_of_calendar(calendar_id).is_some())
    })
}

/// How an invitation leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvitationRoute {
    /// The CalDAV server sends it when the event is uploaded (sync started).
    Server,
    /// Queued as an email with an .ics attachment in the mail Outbox.
    Email,
}

/// Sends the invitation of an event the user organizes.
pub fn send_invitation(event_id: Id, cx: &mut App) -> Option<InvitationRoute> {
    with_bridge(cx, |bridge, cx| bridge.send_invitation(event_id, cx)).flatten()
}

// ---------- bridge ----------

struct AccountSync {
    config: DavAccount,
    account_id: Id,
    state: DavState,
    state_path: PathBuf,
    next_sync: Instant,
    busy: bool,
    failures: u32,
    last_error: Option<String>,
}

pub struct DavBridge {
    state: Entity<AppState>,
    data_dir: PathBuf,
    accounts: Vec<AccountSync>,
    last_success: Option<DateTime<Local>>,
    _timer: Task<()>,
}

impl DavBridge {
    fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let timer = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if this
                    .update(cx, |bridge, cx| bridge.sync_all(false, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut bridge = Self {
            state,
            data_dir: data_dir(),
            accounts: Vec::new(),
            last_success: None,
            _timer: timer,
        };
        bridge.load_accounts(cx);
        bridge
    }

    fn load_accounts(&mut self, cx: &mut Context<Self>) {
        let configs = match dav_config::load_accounts(&self.data_dir.join("dav.json")) {
            Ok(configs) => configs,
            Err(err) => return self.report(format!("Cannot read calendar accounts: {err}"), cx),
        };
        let mut accounts = Vec::new();
        for config in configs {
            // Keep the running state of accounts already loaded.
            if let Some(index) = self.accounts.iter().position(|a| a.config == config) {
                accounts.push(self.accounts.swap_remove(index));
                continue;
            }
            let Some(account_id) = self.ensure_account(&config, cx) else {
                continue;
            };
            let state_path = self
                .data_dir
                .join("dav")
                .join(format!("{}.json", config.key()));
            let state = DavState::load(&state_path).unwrap_or_else(|err| {
                self.report(format!("{}: sync state reset ({err})", config.email), cx);
                DavState::default()
            });
            accounts.push(AccountSync {
                config,
                account_id,
                state,
                state_path,
                next_sync: Instant::now(),
                busy: false,
                failures: 0,
                last_error: None,
            });
        }
        self.accounts = accounts;
    }

    /// The local account with the same email, created when missing.
    fn ensure_account(&mut self, config: &DavAccount, cx: &mut Context<Self>) -> Option<Id> {
        let find = |bridge: &Self, cx: &mut Context<Self>| {
            let accounts = bridge.state.read(cx).store().list_accounts().ok()?;
            accounts
                .into_iter()
                .find(|a| a.email.eq_ignore_ascii_case(&config.email))
                .map(|a| a.id)
        };
        if let Some(id) = find(self, cx) {
            return Some(id);
        }
        let account = Account {
            id: 0,
            name: format!("{} ({})", config.email, config.provider.label()),
            email: config.email.clone(),
            color: DEFAULT_ACCENT,
        };
        self.apply(Command::SaveAccount(Box::new(account)), cx)?;
        find(self, cx)
    }

    fn sync_all(&mut self, manual: bool, cx: &mut Context<Self>) {
        if sync_bridge::is_offline(cx) {
            return;
        }
        let now = Instant::now();
        for index in 0..self.accounts.len() {
            let account = &mut self.accounts[index];
            if account.busy || (!manual && now < account.next_sync) {
                continue;
            }
            self.start(index, cx);
        }
        self.publish_status(cx);
    }

    fn start(&mut self, index: usize, cx: &mut Context<Self>) {
        self.detach_missing_calendars(index, cx);
        // An unreadable local store must not look like "everything was deleted locally".
        let local = match self.local_snapshot(&self.accounts[index], cx) {
            Ok(local) => local,
            Err(err) => {
                let account = &mut self.accounts[index];
                let message = format!("{}: cannot read local data: {err}", account.config.email);
                account.next_sync = Instant::now() + Duration::from_secs(60);
                account.last_error = Some(message.clone());
                self.report(&message, cx);
                return;
            }
        };
        let account = &mut self.accounts[index];
        account.busy = true;
        let config = account.config.clone();
        let state = account.state.clone();
        let key = config.key();
        let task = cx.background_executor().spawn(async move {
            let client = config.client()?;
            mailbox_sync::dav::sync::sync_account(
                &client,
                state,
                &config.kinds(),
                |kind| config.start_url(kind),
                &local,
            )
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |bridge, cx| bridge.finish(&key, result, cx));
        })
        .detach();
    }

    fn finish(&mut self, key: &str, result: SyncResult<AccountOutcome>, cx: &mut Context<Self>) {
        let Some(index) = self.accounts.iter().position(|a| a.config.key() == key) else {
            return;
        };
        let mut errors = Vec::new();
        match result {
            Ok(outcome) => {
                let account_id = self.accounts[index].account_id;
                let mut state = outcome.state;
                for (remote, result) in outcome.collections {
                    match result {
                        Ok(collection) => {
                            let applied = self.apply_collection(account_id, collection, cx);
                            state.collections.insert(remote.url, applied.0);
                            errors.extend(applied.1);
                        }
                        Err(err) => errors.push(format!("{}: {err}", remote.name)),
                    }
                }
                let account = &mut self.accounts[index];
                account.state = state;
                if let Err(err) = account.state.save(&account.state_path) {
                    errors.push(format!("cannot save sync state: {err}"));
                }
            }
            Err(err) => errors.push(err.to_string()),
        }
        let account = &mut self.accounts[index];
        account.busy = false;
        if errors.is_empty() {
            account.failures = 0;
            account.last_error = None;
            account.next_sync = Instant::now()
                + Duration::from_secs(u64::from(account.config.poll_minutes.max(1)) * 60);
            self.last_success = Some(Local::now());
        } else {
            account.failures += 1;
            let backoff = Duration::from_secs(60) * 2u32.saturating_pow(account.failures.min(6));
            account.next_sync = Instant::now() + backoff.min(MAX_BACKOFF);
            let message = format!("{}: {}", account.config.email, errors.join("; "));
            // Toast once per failure streak, not every retry.
            if account.failures == 1 {
                self.report(&message, cx);
            }
            self.accounts[index].last_error = Some(message);
        }
        self.publish_status(cx);
    }

    /// A synced calendar deleted locally (e.g. undo history, older versions) is re-downloaded
    /// from scratch instead of being read as "every event deleted here" and wiped remotely.
    fn detach_missing_calendars(&mut self, index: usize, cx: &App) {
        let store = self.state.read(cx).store();
        for collection in self.accounts[index].state.collections.values_mut() {
            let Some(calendar_id) = collection.local_id else {
                continue;
            };
            if matches!(store.get::<Calendar>(calendar_id), Ok(None)) {
                collection.local_id = None;
                collection.items.clear();
                collection.sync_token = None;
                collection.ctag = None;
            }
        }
    }

    // ---------- local -> resources ----------

    /// Local items per collection URL.
    fn local_snapshot(
        &self,
        account: &AccountSync,
        cx: &App,
    ) -> Result<HashMap<String, Vec<LocalItem>>, String> {
        let store = self.state.read(cx).store();
        let mut snapshot = HashMap::new();
        let default_book = account
            .state
            .collections
            .values()
            .find(|c| c.kind == Kind::AddressBook)
            .map(|c| c.url.clone());
        let cards: Vec<ContactCard> = people::list_contact_cards(store)
            .map_err(|err| err.to_string())?
            .into_iter()
            .filter(|card| card.contact.account_id == account.account_id)
            .collect();
        for collection in account.state.collections.values() {
            let items = match (collection.kind, collection.local_id) {
                (Kind::Calendar, Some(calendar_id)) => calendar_items(store, calendar_id)?,
                (Kind::AddressBook, _) => cards
                    .iter()
                    .filter(|card| {
                        collection.uid_of(card.id()).is_some()
                            || (Some(&collection.url) == default_book.as_ref()
                                && !account
                                    .state
                                    .collections
                                    .values()
                                    .any(|c| c.uid_of(card.id()).is_some()))
                    })
                    .map(|card| {
                        let uid = collection
                            .uid_of(card.id())
                            .map_or_else(calendar::new_uid, str::to_owned);
                        contact_item(card, uid)
                    })
                    .collect(),
                _ => Vec::new(),
            };
            snapshot.insert(collection.url.clone(), items);
        }
        Ok(snapshot)
    }

    // ---------- resources -> local ----------

    /// Applies a collection's changes; returns its state with the new links and errors.
    fn apply_collection(
        &mut self,
        account_id: Id,
        outcome: CollectionOutcome,
        cx: &mut Context<Self>,
    ) -> (CollectionState, Vec<String>) {
        let CollectionOutcome {
            mut state,
            changes,
            conflicts,
            errors,
            ..
        } = outcome;
        let mut errors: Vec<String> = errors
            .into_iter()
            .map(|e| format!("{}: {e}", state.name))
            .collect();
        let calendar_id = match state.kind {
            Kind::Calendar => match self.ensure_calendar(account_id, &state, cx) {
                Some(id) => {
                    state.local_id = Some(id);
                    Some(id)
                }
                None => return (state, errors),
            },
            Kind::AddressBook => None,
        };
        for local_id in conflicts {
            self.keep_conflict_copy(state.kind, local_id, cx);
        }
        for change in changes {
            let result = match change {
                RemoteChange::Upsert {
                    href,
                    etag,
                    text,
                    local_id,
                } => {
                    let stored = match calendar_id {
                        Some(calendar_id) => self.upsert_event(calendar_id, local_id, &text, cx),
                        None => self.upsert_contact(account_id, &state, local_id, &text, cx),
                    };
                    stored.map(|item| {
                        let link = ItemLink {
                            local_id: item.local_id,
                            hash: item.hash(),
                            uid: item.uid,
                            etag,
                        };
                        state.link(href, link);
                    })
                }
                RemoteChange::Delete { local_id } => {
                    let command = match state.kind {
                        Kind::Calendar => CalendarCommand::Delete { id: local_id }.into_command(),
                        Kind::AddressBook => {
                            Command::People(PeopleCommand::DeleteContacts(vec![local_id]))
                        }
                    };
                    // Already gone locally is fine.
                    let _ = self.try_apply(command, cx);
                    Ok(())
                }
            };
            if let Err(err) = result {
                errors.push(format!("{}: {err}", state.name));
            }
        }
        (state, errors)
    }

    /// Local calendar of a collection (created, or renamed/recoloured to match).
    fn ensure_calendar(
        &mut self,
        account_id: Id,
        state: &CollectionState,
        cx: &mut Context<Self>,
    ) -> Option<Id> {
        let existing = state.local_id.and_then(|id| {
            self.state
                .read(cx)
                .store()
                .get::<Calendar>(id)
                .ok()
                .flatten()
        });
        // A local rename/recolour sticks until the server's own value changes.
        let previous = self
            .accounts
            .iter()
            .find_map(|account| account.state.collections.get(&state.url));
        let name_changed = previous.is_none_or(|p| p.name != state.name);
        let color_changed = previous.is_none_or(|p| p.color != state.color);
        let wanted = Calendar {
            id: existing.as_ref().map_or(0, |c| c.id),
            account_id,
            name: match &existing {
                Some(calendar) if !name_changed => calendar.name.clone(),
                _ => state.name.clone(),
            },
            color: match (&existing, state.color) {
                (Some(calendar), _) if !color_changed => calendar.color,
                (_, Some(color)) => color,
                (Some(calendar), None) => calendar.color,
                (None, None) => DEFAULT_ACCENT,
            },
        };
        if existing.as_ref() == Some(&wanted) {
            return Some(wanted.id);
        }
        match self.apply(Command::SaveCalendar(Box::new(wanted.clone())), cx)? {
            Command::DeleteCalendar(id) => Some(id),
            _ => Some(wanted.id),
        }
    }

    /// Stores a server event resource in `calendar_id`; returns the stored item.
    fn upsert_event(
        &mut self,
        calendar_id: Id,
        local_id: Option<Id>,
        text: &str,
        cx: &mut Context<Self>,
    ) -> Result<LocalItem, String> {
        let snapshots = items::parse_event_resource(text).map_err(|e| e.to_string())?;
        let uid = snapshots
            .first()
            .and_then(|s| s.details.as_ref())
            .map(|d| d.uid.clone())
            .unwrap_or_default();
        let mut commands = Vec::new();
        let mut existing = rows_in_calendar(self.state.read(cx).store(), &uid, calendar_id);
        // The linked item changed its UID on the server: replace it.
        if let Some(id) = local_id
            && !existing.iter().any(|row| row.event_id == id)
            && self
                .state
                .read(cx)
                .store()
                .get::<CalendarEvent>(id)
                .ok()
                .flatten()
                .is_some()
        {
            commands.push(CalendarCommand::Delete { id }.into_command());
        }
        let mut new = Vec::new();
        for snapshot in snapshots {
            let mut event = snapshot.event;
            let mut details = snapshot
                .details
                .unwrap_or_else(|| EventDetails::default_for(0));
            match existing
                .iter()
                .position(|row| row.recurrence_id == details.recurrence_id)
            {
                Some(index) => {
                    let row = existing.remove(index);
                    event.id = row.event_id;
                    event.calendar_id = calendar_id;
                    details.event_id = row.event_id;
                    details.master_id = row.master_id;
                    if details.invitation.is_none() {
                        details.invitation = row.invitation;
                    }
                    commands.push(
                        CalendarCommand::Save {
                            event: Box::new(event),
                            details: Box::new(details),
                        }
                        .into_command(),
                    );
                }
                None => new.push(EventSnapshot {
                    event,
                    details: Some(details),
                }),
            }
        }
        // Modified occurrences removed on the server.
        for row in existing {
            commands.push(CalendarCommand::Delete { id: row.event_id }.into_command());
        }
        if !new.is_empty() {
            commands.push(
                CalendarCommand::Import {
                    calendar_id,
                    method: None,
                    events: new,
                }
                .into_command(),
            );
        }
        self.try_apply(Command::Batch(commands), cx)?;
        let store = self.state.read(cx).store();
        let master = rows_in_calendar(store, &uid, calendar_id)
            .into_iter()
            .find(|row| row.master_id.is_none())
            .ok_or_else(|| format!("event {uid} was not stored"))?;
        let event = store
            .get::<CalendarEvent>(master.event_id)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| format!("event {uid} was not stored"))?;
        event_item(store, event, master)
    }

    /// Stores a server vCard; returns the stored item.
    fn upsert_contact(
        &mut self,
        account_id: Id,
        state: &CollectionState,
        local_id: Option<Id>,
        text: &str,
        cx: &mut Context<Self>,
    ) -> Result<LocalItem, String> {
        let (mut card, uid) =
            items::parse_contact_resource(text, account_id).map_err(|e| e.to_string())?;
        let store = self.state.read(cx).store();
        let existing = local_id.and_then(|id| people::contact_card(store, id).ok().flatten());
        if let Some(existing) = &existing {
            card.contact.id = existing.id();
            card.details.id = existing.id();
            card.contact.favorite |= existing.contact.favorite;
        }
        let uid = match uid.is_empty() {
            true => local_id
                .and_then(|id| state.uid_of(id).map(str::to_owned))
                .unwrap_or_else(calendar::new_uid),
            false => uid,
        };
        let inverse = self.try_apply(
            Command::People(PeopleCommand::SaveContact(Box::new(card))),
            cx,
        )?;
        let id = match (existing, inverse) {
            (Some(existing), _) => existing.id(),
            (None, Command::People(PeopleCommand::DeleteContacts(ids))) => {
                ids.first().copied().unwrap_or_default()
            }
            _ => 0,
        };
        let stored = people::contact_card(self.state.read(cx).store(), id)
            .ok()
            .flatten()
            .ok_or_else(|| "contact was not stored".to_owned())?;
        Ok(contact_item(&stored, uid))
    }

    /// Server wins: the local version survives as a new, unlinked item (uploaded next time).
    fn keep_conflict_copy(&mut self, kind: Kind, local_id: Id, cx: &mut Context<Self>) {
        let store = self.state.read(cx).store();
        let command = match kind {
            Kind::Calendar => {
                let Ok(Some(mut event)) = store.get::<CalendarEvent>(local_id) else {
                    return;
                };
                let Ok(details) = calendar::details(store, local_id) else {
                    return;
                };
                event.id = 0;
                event.title.push_str(CONFLICT_SUFFIX);
                let details = EventDetails {
                    event_id: 0,
                    uid: calendar::new_uid(),
                    master_id: None,
                    recurrence_id: None,
                    invitation: None,
                    ..details
                };
                CalendarCommand::Save {
                    event: Box::new(event),
                    details: Box::new(details),
                }
                .into_command()
            }
            Kind::AddressBook => {
                let Ok(Some(mut card)) = people::contact_card(store, local_id) else {
                    return;
                };
                card.contact.id = 0;
                card.details.id = 0;
                let name = card.name();
                card.contact.display_name = format!("{name}{CONFLICT_SUFFIX}");
                Command::People(PeopleCommand::SaveContact(Box::new(card)))
            }
        };
        let _ = self.apply(command, cx);
    }

    // ---------- invitations ----------

    fn send_invitation(&mut self, event_id: Id, cx: &mut Context<Self>) -> Option<InvitationRoute> {
        let store = self.state.read(cx).store();
        let event = store.get::<CalendarEvent>(event_id).ok().flatten()?;
        let details = calendar::details(store, event_id).ok()?;
        let calendar = store.get::<Calendar>(event.calendar_id).ok().flatten()?;
        let server_schedules = self.accounts.iter().any(|account| {
            account.state.collection_of_calendar(calendar.id).is_some()
                && account
                    .state
                    .home(Kind::Calendar)
                    .is_some_and(|home| home.auto_schedule)
        });
        if server_schedules {
            self.sync_all(true, cx);
            return Some(InvitationRoute::Server);
        }
        self.queue_imip(calendar.account_id, &event, &details, cx)
            .map(|()| InvitationRoute::Email)
    }

    /// Puts an invitation email (iMIP) in the Outbox of the calendar's account (the first
    /// account for a local calendar); the mail sync sends it.
    fn queue_imip(
        &mut self,
        account_id: Id,
        event: &CalendarEvent,
        details: &EventDetails,
        cx: &mut Context<Self>,
    ) -> Option<()> {
        let account = crate::calendar::sending_account(self.state.read(cx).store(), account_id)?;
        let to: Vec<Address> = event
            .attendees
            .iter()
            .map(|a| a.address.clone())
            .filter(|a| !a.email.eq_ignore_ascii_case(&account.email))
            .collect();
        let when = event
            .start
            .with_timezone(&Local)
            .format("%A %e %B %Y, %H:%M");
        let body = format!(
            "{}\n{when}\n{}\n\n{}",
            event.title, event.location, event.description
        );
        let state = self.state.clone();
        let queued = crate::calendar::queue_calendar_mail(
            &state,
            &account,
            to,
            format!("Invitation: {}", event.title),
            body,
            items::invitation_attachment(event, details),
            cx,
        );
        match queued {
            Ok(()) => Some(()),
            Err(err) => {
                self.report(err, cx);
                None
            }
        }
    }

    // ---------- status / helpers ----------

    fn status_line(&self) -> Option<String> {
        if self.accounts.is_empty() {
            return None;
        }
        Some(if self.accounts.iter().any(|a| a.busy) {
            "Syncing calendars and contacts…".to_owned()
        } else if let Some(error) = self.accounts.iter().find_map(|a| a.last_error.clone()) {
            error
        } else if let Some(time) = self.last_success {
            format!(
                "Calendars and contacts up to date - {}",
                time.format("%H:%M")
            )
        } else {
            "Calendars and contacts waiting to sync".to_owned()
        })
    }

    /// `AppState::sync_status` belongs to the mail bridge (refreshed every 300 ms), so ours
    /// is read with [`status`]; observers of the app state are notified to re-render it.
    /// Errors are toasted through `AppState::report_error`.
    fn publish_status(&self, cx: &mut Context<Self>) {
        if !self.accounts.is_empty() {
            self.state.update(cx, |_, cx| cx.notify());
        }
    }

    fn try_apply(&self, command: Command, cx: &mut Context<Self>) -> Result<Command, String> {
        self.state.update(cx, |state, cx| {
            let result = command::apply(state.store(), command).map_err(|e| e.to_string());
            if result.is_ok() {
                cx.notify();
            }
            result
        })
    }

    fn apply(&self, command: Command, cx: &mut Context<Self>) -> Option<Command> {
        match self.try_apply(command, cx) {
            Ok(inverse) => Some(inverse),
            Err(err) => {
                self.report(err, cx);
                None
            }
        }
    }

    fn report(&self, err: impl std::fmt::Display, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, cx| state.report_error(err, cx));
    }
}

/// Details rows of a resource (series + exceptions) stored in `calendar_id`.
fn rows_in_calendar(store: &mailbox_core::Store, uid: &str, calendar_id: Id) -> Vec<EventDetails> {
    calendar::details_by_uid(store, uid)
        .unwrap_or_default()
        .into_iter()
        .filter(|row| {
            store
                .get::<CalendarEvent>(row.event_id)
                .ok()
                .flatten()
                .is_some_and(|event| event.calendar_id == calendar_id)
        })
        .collect()
}

/// Every series/one-off of a calendar as a resource.
/// Read errors are returned (never an empty list), so a failed read cannot delete remotely.
fn calendar_items(store: &mailbox_core::Store, calendar_id: Id) -> Result<Vec<LocalItem>, String> {
    let events: Vec<CalendarEvent> = store
        .query("WHERE calendar_id = ?1 ORDER BY id", [calendar_id])
        .map_err(|err| err.to_string())?;
    let mut items = Vec::new();
    for event in events {
        let details = calendar::details(store, event.id).map_err(|err| err.to_string())?;
        if details.master_id.is_none() {
            items.push(event_item(store, event, details)?);
        }
    }
    Ok(items)
}

/// A series (or one-off) with its modified occurrences.
fn event_item(
    store: &mailbox_core::Store,
    event: CalendarEvent,
    details: EventDetails,
) -> Result<LocalItem, String> {
    let mut exceptions = Vec::new();
    for row in calendar::exceptions_of(store, event.id).map_err(|err| err.to_string())? {
        // An orphan details row (event already deleted) is skipped, not fatal.
        if let Some(occurrence) = store
            .get::<CalendarEvent>(row.event_id)
            .map_err(|err| err.to_string())?
        {
            exceptions.push((occurrence, row));
        }
    }
    let local_id = event.id;
    let uid = details.uid.clone();
    let master = (event, details);
    Ok(LocalItem {
        local_id,
        uid,
        text: items::event_resource(&master, &exceptions),
    })
}

fn contact_item(card: &ContactCard, uid: String) -> LocalItem {
    LocalItem {
        local_id: card.id(),
        text: items::contact_resource(card, &uid),
        uid,
    }
}

/// Same location as the mail sync (`sync_bridge`): `MAILBOX_DATA_DIR` or the user data dir.
fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("MAILBOX_DATA_DIR") {
        return PathBuf::from(dir);
    }
    std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("XDG_DATA_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("mailbox")
}

// ---------- "Add calendar/contacts account" dialog ----------

const PROVIDERS: [(&str, Provider); 5] = [
    ("Google", Provider::Google),
    ("iCloud", Provider::ICloud),
    ("Fastmail", Provider::Fastmail),
    ("Nextcloud", Provider::Nextcloud),
    ("Other", Provider::Generic),
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum TestState {
    Idle,
    Running,
    Passed(String),
    Failed(String),
}

pub struct AddDavAccountDialog {
    provider: Provider,
    email: Entity<InputState>,
    url: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    test: TestState,
    error: Option<SharedString>,
}

/// Opens the "Add calendar/contacts account" dialog (provider, URL, user, password, Test).
pub fn open_add_dav_account(_state: Entity<AppState>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| AddDavAccountDialog::new(window, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog.w(px(540.)).child(view.clone())
    });
}

impl AddDavAccountDialog {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        Self {
            provider: Provider::ICloud,
            email: input("you@example.com", window, cx),
            url: input("https://cloud.example.com", window, cx),
            username: input("Optional: when it differs from the email", window, cx),
            password: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("App password")
                    .masked(true)
            }),
            test: TestState::Idle,
            error: None,
        }
    }

    fn read(&self, cx: &App) -> Result<(DavAccount, String), String> {
        let value = |input: &Entity<InputState>| input.read(cx).value().trim().to_owned();
        let email = value(&self.email);
        if !email.contains('@') {
            return Err("Enter your email address".into());
        }
        let mut account = DavAccount::new(self.provider, email);
        account.url = value(&self.url);
        account.username = value(&self.username);
        if self.provider.needs_url() && account.url.is_empty() {
            return Err("Enter the server address".into());
        }
        let password = self.password.read(cx).value().to_string();
        if !self.provider.uses_oauth() && password.is_empty() {
            return Err("Enter the password".into());
        }
        Ok((account, password))
    }

    /// Discovers calendars and address books with the typed credentials.
    fn run_test(&mut self, cx: &mut Context<Self>) {
        let (account, password) = match self.read(cx) {
            Ok(values) => values,
            Err(message) => {
                self.error = Some(message.into());
                return cx.notify();
            }
        };
        self.error = None;
        self.test = TestState::Running;
        cx.notify();
        let task = cx
            .background_executor()
            .spawn(async move { test_account(&account, &password) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.test = match result {
                    Ok(found) => TestState::Passed(found),
                    Err(err) => TestState::Failed(err.to_string()),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (account, password) = match self.read(cx) {
            Ok(values) => values,
            Err(message) => {
                self.error = Some(message.into());
                return cx.notify();
            }
        };
        match save_account(&account, &password) {
            Ok(()) => {
                window.close_dialog(cx);
                reload(cx);
            }
            Err(err) => {
                self.error = Some(err.to_string().into());
                cx.notify();
            }
        }
    }
}

/// "Found 2 calendars, 1 address book".
fn test_account(account: &DavAccount, password: &str) -> SyncResult<String> {
    let client = if account.provider.uses_oauth() {
        account.client()?
    } else {
        DavClient::new(Some(Auth::Basic {
            user: account.login().to_owned(),
            password: password.to_owned(),
        }))
    };
    let mut parts = Vec::new();
    for kind in account.kinds() {
        let home = discovery::find_home(&client, &account.start_url(kind)?, kind)?;
        let count = discovery::list_collections(&client, &home)?.len();
        let noun = match (kind, count) {
            (Kind::Calendar, 1) => "calendar",
            (Kind::Calendar, _) => "calendars",
            (Kind::AddressBook, 1) => "address book",
            (Kind::AddressBook, _) => "address books",
        };
        parts.push(format!("{count} {noun}"));
    }
    Ok(format!("Found {}", parts.join(", ")))
}

/// Stores the password in the keyring and appends the account to `dav.json`.
fn save_account(account: &DavAccount, password: &str) -> SyncResult<()> {
    if !account.provider.uses_oauth() {
        dav_config::set_password(account.login(), password)?;
    }
    let path = data_dir().join("dav.json");
    let mut accounts = dav_config::load_accounts(&path)?;
    accounts.retain(|a| a.key() != account.key());
    accounts.push(account.clone());
    dav_config::save_accounts(&path, &accounts)
}

impl Render for AddDavAccountDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let view = cx.entity().downgrade();
        let providers = segmented(
            "dav-provider",
            &PROVIDERS,
            self.provider,
            move |provider, _, cx| {
                if let Some(view) = view.upgrade() {
                    view.update(cx, |this, cx| {
                        this.provider = provider;
                        this.test = TestState::Idle;
                        cx.notify();
                    });
                }
            },
            cx,
        );
        const TEST_ID: &str = "dav-test-result";
        let test_line = match &self.test {
            TestState::Idle => None,
            TestState::Running => Some(Banner::info(TEST_ID, "Connecting…").icon(IconName::Clock)),
            TestState::Passed(found) => Some(Banner::success(TEST_ID, found.clone())),
            TestState::Failed(error) => Some(Banner::error(TEST_ID, error.clone())),
        };
        let header = div()
            .flex()
            .flex_col()
            .gap(SPACE_1)
            .child(
                div()
                    .text_size(TEXT_LG)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Add calendar/contacts account"),
            )
            .child(
                div()
                    .text_size(TEXT_SM)
                    .text_color(t.text_muted)
                    .child("Two-way sync of calendars (CalDAV) and contacts (CardDAV)."),
            );
        let messages = test_line
            .map(IntoElement::into_any_element)
            .into_iter()
            .chain(
                self.error
                    .clone()
                    .map(|error| Banner::error("dav-error", error).into_any_element()),
            )
            .collect();
        let body = card(cx)
            .gap(SPACE_3)
            .child(field("Provider", providers, cx))
            .child(
                div()
                    .text_size(TEXT_SM)
                    .text_color(t.text_subtle)
                    .child(self.provider.help()),
            )
            .child(field("Email address", Input::new(&self.email), cx))
            .when(self.provider.needs_url(), |el| {
                el.child(field("Server address", Input::new(&self.url), cx))
            })
            .when(!self.provider.uses_oauth(), |el| {
                el.child(field("Username", Input::new(&self.username), cx))
                    .child(field("Password", Input::new(&self.password), cx))
            });
        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(SPACE_2)
            .child(
                Button::new("dav-test")
                    .ghost()
                    .label("Test")
                    .on_click(cx.listener(|this, _, _, cx| this.run_test(cx))),
            )
            .child(div().flex_1())
            .child(
                Button::new("dav-cancel")
                    .label(tr(cx, "Cancel"))
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            )
            .child(
                Button::new("dav-add")
                    .primary()
                    .label(tr(cx, "Add account"))
                    .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
            );
        dialog_frame("dav-body", header, body, messages, footer, window)
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone, Utc};
    use mailbox_core::calendar::{CalendarCommand, EventDetails, LOCAL_ACCOUNT};
    use mailbox_core::{Calendar, CalendarEvent, History, Store};

    use super::{calendar_items, rows_in_calendar};

    fn event(calendar_id: i64, title: &str, recurrence: Option<&str>) -> CalendarEvent {
        let start = Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap();
        CalendarEvent {
            id: 0,
            calendar_id,
            title: title.into(),
            location: String::new(),
            description: String::new(),
            start,
            end: start + Duration::hours(1),
            all_day: false,
            recurrence: recurrence.map(str::to_owned),
            attendees: Vec::new(),
            reminders: Vec::new(),
            categories: Vec::new(),
        }
    }

    /// What a CalDAV sync pushes for a calendar: one resource per series (its modified
    /// occurrences inside), never the occurrences on their own.
    #[test]
    fn calendar_snapshot_groups_series_and_exceptions() {
        let store = Store::open_in_memory().unwrap();
        let mut calendar = Calendar {
            id: 0,
            account_id: LOCAL_ACCOUNT,
            name: "Calendar".into(),
            color: 0x3b82f6,
        };
        store.save(&mut calendar).unwrap();
        let mut history = History::new();
        let mut save = |event: CalendarEvent| {
            history
                .run(
                    &store,
                    CalendarCommand::Save {
                        event: Box::new(event),
                        details: Box::new(EventDetails::default_for(0)),
                    }
                    .into_command(),
                )
                .unwrap();
        };
        save(event(calendar.id, "Standup", Some("FREQ=DAILY;COUNT=5")));
        save(event(calendar.id, "Lunch", None));
        let series = store
            .query::<CalendarEvent>("WHERE title = 'Standup'", [])
            .unwrap()
            .remove(0);
        let moved = series.start + Duration::days(1);
        history
            .run(
                &store,
                CalendarCommand::SaveOccurrence {
                    series_id: series.id,
                    occurrence: moved,
                    event: Box::new(CalendarEvent {
                        id: 0,
                        start: moved + Duration::hours(2),
                        end: moved + Duration::hours(3),
                        recurrence: None,
                        ..series.clone()
                    }),
                    details: Box::new(EventDetails::default_for(0)),
                }
                .into_command(),
            )
            .unwrap();

        let items = calendar_items(&store, calendar.id).unwrap();
        assert_eq!(items.len(), 2, "series + one-off, exception folded in");
        let standup = items.iter().find(|i| i.local_id == series.id).unwrap();
        assert_eq!(standup.text.matches("BEGIN:VEVENT").count(), 2);
        assert!(standup.text.contains("RECURRENCE-ID"));
        assert_eq!(rows_in_calendar(&store, &standup.uid, calendar.id).len(), 2);
        assert!(rows_in_calendar(&store, &standup.uid, calendar.id + 1).is_empty());
    }
}
