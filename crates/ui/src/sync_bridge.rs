//! Bridge between [`AppState`] and the background sync workers (`mailbox-sync`).
//!
//! - One worker per account with server settings: the `AccountServers` saved by the account
//!   wizard (plus its `account.<id>.auth` setting), or an entry of the optional
//!   `<data dir>/sync.json` (matched by email, wins). The password/refresh token comes from the
//!   OS credential store.
//! - Every [`POLL_INTERVAL`] the bridge drains worker events and turns them into
//!   `mailbox_core::Command`s applied with `command::apply` (not the undo history: undoing
//!   "a message arrived" makes no sense), then refreshes `AppState::sync_status`.
//! - Local edits are found by diffing the store against the sync state, so changes made while
//!   offline are pushed on the next sync. Messages placed in the account's "Outbox" folder
//!   are sent (headers from compose in `<data dir>/outbox/<id>.json`: Bcc, reply threading,
//!   read receipt), then moved to Sent. A failed send stays in the Outbox with an error toast
//!   and a Retry button; it is not retried automatically until Send/Receive or Retry.
//! - Actions: [`SendReceive`] (F9) syncs now, [`ToggleWorkOffline`].
//!
//! Entry points for other modules: [`send_receive`], [`set_offline`], [`is_offline`],
//! [`request_body`], [`outbox_folder`], [`import_file`], [`export_folder`],
//! [`export_message`], [`attachment_path`], [`outgoing_headers_path`], [`reload`],
//! [`start_account`], [`remove_account`], [`retry_outbox`], [`data_dir`].

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use gpui::component::WindowExt as _;
use gpui::component::button::Button;
use gpui::component::notification::{Notification, NotificationType};
use gpui::{App, AppContext as _, Context, Entity, Global, Task};
use mailbox_core::settings::{self as core_settings, AccountServers};
use mailbox_core::{
    Account, Address, Command, Folder, FolderRole, Id, Message, Store, command, remote,
};
use mailbox_sync::config::{self, AccountConfig, AuthConfig, credentials};
use mailbox_sync::engine::{FetchedMessage, PushOp, SyncInput};
use mailbox_sync::formats::{self, MboxEntry, OutgoingHeaders};
use mailbox_sync::imap_smtp::ImapSmtpBackend;
use mailbox_sync::local::store_fetched;
use mailbox_sync::mime::preview;
use mailbox_sync::state::LocalMessage;
use mailbox_sync::thread::normalize_id;
use mailbox_sync::utf7;
use mailbox_sync::worker::BackendFactory;
use mailbox_sync::{
    AttachmentData, MailBackend, OutgoingMessage, ParsedMail, RemoteFlags, RemoteFolder, Request,
    SyncEvent, SyncResult, SyncState, WorkerHandle,
};

use crate::app_state::{AppState, SyncPhase, SyncSummary};
use crate::components::toast;
use crate::{SendReceive, ToggleWorkOffline};

/// How often worker events are applied to the store.
const POLL_INTERVAL: Duration = Duration::from_millis(300);
/// During a sync the state file is saved at most this often (and at the end of the cycle), so
/// a crash or kill loses little; the store's server identities cover the rest.
const STATE_SAVE_INTERVAL: Duration = Duration::from_secs(2);
/// Longest wait between retries after failures.
const MAX_BACKOFF: Duration = Duration::from_secs(15 * 60);
/// First sync of a folder downloads at most this many (newest) messages.
const MAX_INITIAL: usize = 5000;
/// Local folder (role `Custom`) whose messages are sent on the next Send/Receive.
pub const OUTBOX_NAME: &str = "Outbox";

struct GlobalSyncBridge(Entity<SyncBridge>);

/// Notification id type of the "Message not sent" toast (keyed by message id).
struct SendFailedToast;

impl Global for GlobalSyncBridge {}

/// Creates the bridge, starts configured accounts and registers the global actions.
pub fn init(state: Entity<AppState>, cx: &mut App) {
    let bridge = cx.new(|cx| SyncBridge::new(state, cx));
    cx.set_global(GlobalSyncBridge(bridge));
    cx.on_action(|_: &SendReceive, cx| send_receive(cx));
    cx.on_action(|_: &ToggleWorkOffline, cx| {
        let offline = !is_offline(cx);
        set_offline(offline, cx);
    });
}

pub fn bridge(cx: &App) -> Option<Entity<SyncBridge>> {
    cx.try_global::<GlobalSyncBridge>()
        .map(|global| global.0.clone())
}

fn with_bridge(cx: &mut App, f: impl FnOnce(&mut SyncBridge, &mut Context<SyncBridge>)) {
    if let Some(bridge) = bridge(cx) {
        bridge.update(cx, f);
    }
}

/// Syncs every account now and sends the Outbox ("Send/Receive").
pub fn send_receive(cx: &mut App) {
    with_bridge(cx, |bridge, cx| bridge.send_receive(cx));
}

/// Work Offline: no network until switched back; local changes are pushed afterwards.
pub fn set_offline(offline: bool, cx: &mut App) {
    with_bridge(cx, |bridge, cx| bridge.set_offline(offline, cx));
}

pub fn is_offline(cx: &App) -> bool {
    bridge(cx).is_some_and(|bridge| bridge.read(cx).offline)
}

/// Downloads the body of a headers-only message (call when opening one with an empty body).
/// Repeated calls while a download is pending are ignored.
pub fn request_body(message_id: Id, cx: &mut App) {
    with_bridge(cx, |bridge, _| bridge.request_body(message_id));
}

/// Stops all workers and starts them again from `sync.json` (after editing accounts).
pub fn reload(cx: &mut App) {
    with_bridge(cx, |bridge, cx| bridge.reload(cx));
}

/// Starts (or restarts) sync for one account right away (after adding it).
pub fn start_account(account_id: Id, cx: &mut App) {
    with_bridge(cx, |bridge, cx| bridge.start_account(account_id, cx));
}

/// Stops sync, deletes the account's local data (store rows, sync state, attachments) and
/// its keychain secrets. Not undoable: confirm first.
pub fn remove_account(account_id: Id, cx: &mut App) {
    with_bridge(cx, |bridge, cx| bridge.remove_account(account_id, cx));
}

/// Sends the Outbox again, including messages whose last attempt failed.
pub fn retry_outbox(cx: &mut App) {
    send_receive(cx);
}

/// Where compose leaves the extra headers of a queued message ([`OutgoingHeaders`] as JSON).
pub fn outgoing_headers_path(message_id: Id) -> PathBuf {
    data_dir().join("outbox").join(format!("{message_id}.json"))
}

/// The account's Outbox folder, created if missing. Compose saves messages there to send.
pub fn outbox_folder(account_id: Id, cx: &mut App) -> Option<Id> {
    bridge(cx)?.update(cx, |bridge, cx| bridge.outbox_folder(account_id, cx))
}

/// Imports an `.eml` or `.mbox` file into a folder (in the background).
pub fn import_file(path: PathBuf, folder_id: Id, cx: &mut App) {
    with_bridge(cx, |bridge, cx| bridge.import_file(path, folder_id, cx));
}

/// Exports a folder as an mbox file (in the background).
pub fn export_folder(folder_id: Id, path: PathBuf, cx: &mut App) {
    with_bridge(cx, |bridge, cx| bridge.export_folder(folder_id, path, cx));
}

/// Exports one message as an `.eml` file (in the background).
pub fn export_message(message_id: Id, path: PathBuf, cx: &mut App) {
    with_bridge(cx, |bridge, cx| bridge.export_message(message_id, path, cx));
}

/// Where an attachment of a stored message lives on disk (may not exist: headers-only mail).
pub fn attachment_path(message_id: Id, name: &str) -> PathBuf {
    attachments_dir(&data_dir(), message_id).join(file_name(name))
}

/// Per-account sync bookkeeping.
struct AccountSync {
    account_id: Id,
    config: AccountConfig,
    worker: WorkerHandle,
    sync: SyncState,
    state_path: PathBuf,
    /// `sync` changed since it was last written to `state_path`.
    dirty: bool,
    saved_at: Instant,
    next_sync: Instant,
    failures: u32,
    busy: bool,
    last_error: Option<String>,
    progress: String,
    /// Outbox messages handed to the worker.
    sending: HashSet<Id>,
    /// Outbox messages whose last send failed, with the error (kept until a retry).
    failed: HashMap<Id, String>,
    /// Bodies requested on demand.
    fetching: HashSet<Id>,
    /// Unread messages that arrived in the inbox during the current cycle.
    new_mail: Vec<Id>,
}

pub struct SyncBridge {
    state: Entity<AppState>,
    data_dir: PathBuf,
    accounts: HashMap<Id, AccountSync>,
    offline: bool,
    last_success: Option<DateTime<Local>>,
    _poll: Task<()>,
}

impl SyncBridge {
    fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                if this.update(cx, |bridge, cx| bridge.poll(cx)).is_err() {
                    break;
                }
            }
        });
        let mut bridge = Self {
            state,
            data_dir: data_dir(),
            accounts: HashMap::new(),
            offline: false,
            last_success: None,
            _poll: poll,
        };
        bridge.repair_store(cx);
        bridge.start_accounts(cx);
        bridge
    }

    pub fn is_offline(&self) -> bool {
        self.offline
    }

    // ---------- lifecycle ----------

    /// `sync.json` entries (optional manual overrides).
    fn overrides(&self, cx: &mut Context<Self>) -> Vec<AccountConfig> {
        config::load_accounts(&self.data_dir.join("sync.json")).unwrap_or_else(|err| {
            self.report(format!("Cannot read sync.json: {err}"), cx);
            Vec::new()
        })
    }

    fn start_accounts(&mut self, cx: &mut Context<Self>) {
        let overrides = self.overrides(cx);
        let accounts = match self.state.read(cx).store().list_accounts() {
            Ok(accounts) => accounts,
            Err(err) => {
                self.report(err, cx);
                return;
            }
        };
        for account in accounts {
            let Some(config) = account_config(self.state.read(cx).store(), &account, &overrides)
            else {
                continue;
            };
            if let Err(err) = self.start(account.id, config) {
                self.report(
                    format!("Cannot start sync for {}: {err}", account.email),
                    cx,
                );
            }
        }
        self.refresh_status(cx);
    }

    fn start_account(&mut self, account_id: Id, cx: &mut Context<Self>) {
        let overrides = self.overrides(cx);
        let store = self.state.read(cx).store();
        let config = store
            .get::<Account>(account_id)
            .ok()
            .flatten()
            .and_then(|account| account_config(store, &account, &overrides));
        let Some(config) = config else {
            return self.report("This account has no server settings", cx);
        };
        if let Some(mut old) = self.accounts.remove(&account_id) {
            self.save_state(&mut old, cx);
        }
        let email = config.email.clone();
        if let Err(err) = self.start(account_id, config) {
            self.report(format!("Cannot start sync for {email}: {err}"), cx);
        }
        self.poll(cx);
    }

    fn remove_account(&mut self, account_id: Id, cx: &mut Context<Self>) {
        let store = self.state.read(cx).store();
        let Some(account) = store.get::<Account>(account_id).ok().flatten() else {
            return;
        };
        let message_ids: Vec<Id> = store
            .list_folders(account_id)
            .unwrap_or_default()
            .iter()
            .flat_map(|folder| store.list_messages(folder.id).unwrap_or_default())
            .map(|message| message.id)
            .collect();
        // Dropping the handle stops the worker threads.
        self.accounts.remove(&account_id);
        if self.apply(Command::DeleteAccount(account_id), cx).is_none() {
            return;
        }
        credentials::forget_account(&account.email);
        let data_dir = self.data_dir.clone();
        self.background(
            cx,
            move || {
                // Best effort: missing files are fine, the store rows are already gone.
                let _ = std::fs::remove_file(state_path(&data_dir, account_id));
                for id in message_ids {
                    let _ = std::fs::remove_dir_all(attachments_dir(&data_dir, id));
                    let _ = std::fs::remove_file(outgoing_headers_file(&data_dir, id));
                }
            },
            |_, (), _| {},
        );
        self.state.update(cx, |state, cx| {
            let inbox = state
                .store()
                .list_accounts()
                .ok()
                .and_then(|accounts| accounts.first().map(|account| account.id))
                .and_then(|id| state.store().folder_by_role(id, FolderRole::Inbox).ok())
                .flatten()
                .map(|folder| folder.id);
            state.select_folder(inbox, cx);
            state.notify_info(format!("Account {} removed", account.email), cx);
        });
        self.refresh_status(cx);
    }

    fn start(&mut self, account_id: Id, config: AccountConfig) -> SyncResult<()> {
        let state_path = state_path(&self.data_dir, account_id);
        let sync = SyncState::load(&state_path)?;
        let factory: BackendFactory = {
            let config = config.clone();
            Arc::new(move || -> SyncResult<Box<dyn MailBackend>> {
                Ok(Box::new(ImapSmtpBackend::new(config.clone())?))
            })
        };
        let idle_path = config.idle.then(|| {
            sync.path_of_role(FolderRole::Inbox)
                .unwrap_or("INBOX")
                .to_owned()
        });
        let worker = WorkerHandle::spawn(factory, idle_path)?;
        if self.offline {
            worker.set_offline(true);
        }
        self.accounts.insert(
            account_id,
            AccountSync {
                account_id,
                config,
                worker,
                sync,
                state_path,
                dirty: false,
                saved_at: Instant::now(),
                next_sync: Instant::now(),
                failures: 0,
                busy: false,
                last_error: None,
                progress: String::new(),
                sending: HashSet::new(),
                failed: HashMap::new(),
                fetching: HashSet::new(),
                new_mail: Vec::new(),
            },
        );
        Ok(())
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        for (_, mut account) in std::mem::take(&mut self.accounts) {
            self.save_state(&mut account, cx);
        }
        self.start_accounts(cx);
    }

    fn send_receive(&mut self, cx: &mut Context<Self>) {
        if self.accounts.is_empty() {
            self.info("No sync account configured", cx);
            return;
        }
        if self.offline {
            self.info("Working offline: switch it off to send and receive", cx);
            return;
        }
        let now = Instant::now();
        for account in self.accounts.values_mut() {
            account.next_sync = now;
            account.failures = 0;
            account.failed.clear();
        }
        self.poll(cx);
    }

    fn set_offline(&mut self, offline: bool, cx: &mut Context<Self>) {
        if self.offline == offline {
            return;
        }
        self.offline = offline;
        let now = Instant::now();
        for account in self.accounts.values_mut() {
            account.worker.set_offline(offline);
            account.next_sync = now;
        }
        self.refresh_status(cx);
    }

    fn request_body(&mut self, message_id: Id) {
        for account in self.accounts.values_mut() {
            let Some((path, uid, _)) = account.sync.find(message_id) else {
                continue;
            };
            if account.fetching.insert(message_id) {
                let path = path.to_owned();
                account.worker.request(Request::FetchBody {
                    local_id: message_id,
                    path,
                    uid,
                });
            }
            return;
        }
    }

    // ---------- polling ----------

    fn poll(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<Id> = self.accounts.keys().copied().collect();
        for id in ids {
            // Taken out of the map so handlers can borrow `self` freely.
            let Some(mut account) = self.accounts.remove(&id) else {
                continue;
            };
            // A drain can hold thousands of messages: save while applying, not only after.
            for event in account.worker.drain() {
                account.dirty |= !matches!(event, SyncEvent::Progress(_));
                self.apply_event(&mut account, event, cx);
                if account.dirty && account.saved_at.elapsed() >= STATE_SAVE_INTERVAL {
                    self.save_state(&mut account, cx);
                }
            }
            if !self.offline && !account.busy && Instant::now() >= account.next_sync {
                self.start_cycle(&mut account, cx);
            }
            self.accounts.insert(id, account);
        }
        self.refresh_status(cx);
    }

    fn start_cycle(&mut self, account: &mut AccountSync, cx: &mut Context<Self>) {
        let local = match self.local_snapshot(account.account_id, cx) {
            Ok(local) => local,
            Err(err) => {
                self.report(err, cx);
                account.next_sync = Instant::now() + backoff(1);
                return;
            }
        };
        // Safety net: a sync state that matches nothing in the store (database replaced or
        // reset) must not turn into "delete everything on the server". Re-download instead.
        let links = account.sync.linked_ids().count();
        if links > 0 && !account.sync.linked_ids().any(|id| local.contains_key(&id)) {
            account.sync = SyncState::default();
        }
        let input = SyncInput {
            push: account.sync.diff_local(&local),
            cursors: account.sync.cursors(),
            offline_days: account.config.offline_days,
            max_initial: MAX_INITIAL,
        };
        if account.worker.request(Request::Sync(input)) {
            account.busy = true;
            account.new_mail.clear();
            account.progress.clear();
        } else {
            account.last_error = Some("sync worker stopped (restart the app)".to_owned());
            account.next_sync = Instant::now() + MAX_BACKOFF;
            return;
        }
        self.send_outbox(account, cx);
    }

    fn local_snapshot(
        &self,
        account_id: Id,
        cx: &App,
    ) -> mailbox_core::Result<HashMap<Id, LocalMessage>> {
        let store = self.state.read(cx).store();
        let mut local = HashMap::new();
        for folder in store.list_folders(account_id)? {
            for message in store.list_messages(folder.id)? {
                local.insert(
                    message.id,
                    LocalMessage {
                        folder_id: message.folder_id,
                        flags: RemoteFlags::from(message.flags),
                    },
                );
            }
        }
        Ok(local)
    }

    fn apply_event(&mut self, account: &mut AccountSync, event: SyncEvent, cx: &mut Context<Self>) {
        match event {
            SyncEvent::Progress(text) => account.progress = text,
            SyncEvent::Folders(folders) => self.map_folders(account, folders, cx),
            SyncEvent::Pushed(op) => {
                // Moved away on the server: its old UID no longer identifies the local copy.
                if let PushOp::Move { path, uid, .. } = &op
                    && let Some(link) = account
                        .sync
                        .folders
                        .get(path)
                        .and_then(|folder| folder.messages.get(uid))
                {
                    let local_id = link.local_id;
                    self.update_store(cx, |store| store.set_message_remote(local_id, None, ""));
                }
                account.sync.apply_pushed(&op);
            }
            SyncEvent::FolderReset { path } => {
                let ids = account.sync.reset_folder(&path);
                self.delete_local(ids, cx);
            }
            SyncEvent::NewMessages {
                path,
                uid_validity,
                messages,
            } => self.store_new(account, &path, uid_validity, messages, cx),
            SyncEvent::FlagsChanged { path, changes } => {
                let updates = account.sync.remote_flags(&path, &changes);
                self.apply_remote_flags(updates, cx);
            }
            SyncEvent::Vanished { path, uids } => {
                let ids = account.sync.vanish(&path, &uids);
                self.delete_local(ids, cx);
            }
            SyncEvent::FolderSynced {
                path,
                uid_validity,
                highest_uid,
            } => account.sync.folder_synced(&path, uid_validity, highest_uid),
            SyncEvent::BodyFetched { local_id, parsed } => {
                account.fetching.remove(&local_id);
                self.store_body(account, local_id, *parsed, cx);
            }
            SyncEvent::Sent { local_id } => self.after_sent(account, local_id, cx),
            SyncEvent::SendFailed { local_id, error } => {
                account.sending.remove(&local_id);
                account.failed.insert(local_id, error.clone());
                let subject = self
                    .existing(local_id, cx)
                    .map(|message| message.subject)
                    .unwrap_or_default();
                self.notify_send_failure(local_id, subject, error, cx);
            }
            SyncEvent::RemoteChanged => {
                let soon = Instant::now() + Duration::from_secs(2);
                account.next_sync = account.next_sync.min(soon);
            }
            SyncEvent::Finished { error } => self.finish_cycle(account, error, cx),
        }
    }

    fn finish_cycle(
        &mut self,
        account: &mut AccountSync,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        account.busy = false;
        account.fetching.clear();
        match error {
            None => {
                account.failures = 0;
                account.last_error = None;
                account.next_sync = Instant::now() + poll_interval(&account.config);
                self.last_success = Some(Local::now());
            }
            Some(error) => {
                account.failures += 1;
                account.next_sync = Instant::now() + backoff(account.failures);
                account.last_error = Some(error);
            }
        }
        self.save_state(account, cx);
        if !account.new_mail.is_empty() {
            let count = account.new_mail.len();
            let noun = if count == 1 { "message" } else { "messages" };
            self.info(
                format!("{count} new {noun} for {}", account.config.email),
                cx,
            );
            self.on_new_mail(std::mem::take(&mut account.new_mail), cx);
        }
    }

    /// Runs the account's mail rules on newly arrived messages (not undoable, like sync).
    fn on_new_mail(&mut self, ids: Vec<Id>, cx: &mut Context<Self>) {
        let result = {
            let store = self.state.read(cx).store();
            let account = ids
                .first()
                .and_then(|id| store.get::<Message>(*id).ok().flatten())
                .map(|message| message.account_id);
            account.map(|account| mailbox_core::rules::run_command(store, account, &ids))
        };
        match result {
            Some(Ok((Some(command), _))) => {
                self.apply(command, cx);
            }
            Some(Err(err)) => self.report(err, cx),
            _ => {}
        }
        // Mute, automatic replies, desktop notification.
        crate::notifications::on_new_mail(&self.state, ids, cx);
    }

    // ---------- events → store ----------

    fn map_folders(
        &mut self,
        account: &mut AccountSync,
        mut folders: Vec<RemoteFolder>,
        cx: &mut Context<Self>,
    ) {
        let local = match self.state.read(cx).store().list_folders(account.account_id) {
            Ok(local) => local,
            Err(err) => return self.report(err, cx),
        };
        // Parents before children (a parent path is a prefix of its children's paths).
        folders.sort_by_key(|folder| folder.path.len());
        for (position, remote) in folders.iter().enumerate() {
            let parent_id = remote
                .parent
                .as_deref()
                .and_then(|parent| account.sync.local_folder(parent));
            let known = account
                .sync
                .local_folder(&remote.path)
                .filter(|id| local.iter().any(|folder| folder.id == *id));
            let matching = || {
                local
                    .iter()
                    .find(|folder| match remote.role {
                        FolderRole::Custom => {
                            folder.role == FolderRole::Custom
                                && folder.parent_id == parent_id
                                && folder.name.eq_ignore_ascii_case(&remote.name)
                        }
                        role => folder.role == role,
                    })
                    .map(|folder| folder.id)
            };
            let local_id = known.or_else(matching).or_else(|| {
                let folder = Folder {
                    id: 0,
                    account_id: account.account_id,
                    parent_id,
                    name: display_name(remote),
                    role: remote.role,
                    position: position as i64,
                };
                self.apply(Command::SaveFolder(Box::new(folder)), cx)
                    .and_then(|inverse| created_id(&inverse))
            });
            account.sync.folder_mut(remote).local_id = local_id;
        }
    }

    fn store_new(
        &mut self,
        account: &mut AccountSync,
        path: &str,
        uid_validity: u32,
        messages: Vec<FetchedMessage>,
        cx: &mut Context<Self>,
    ) {
        let is_inbox = account
            .sync
            .folders
            .get(path)
            .is_some_and(|folder| folder.role == FolderRole::Inbox);
        for fetched in messages {
            let account_id = account.account_id;
            let sync = &mut account.sync;
            let stored = self.update_store(cx, |store| {
                store_fetched(store, sync, account_id, path, uid_validity, fetched)
            });
            let Some(stored) = stored.flatten() else {
                continue;
            };
            if is_inbox && stored.created && stored.unread {
                account.new_mail.push(stored.local_id);
            }
            if let Some(to) = stored.receipt_to.filter(|_| is_inbox && stored.created) {
                crate::receipts::note_request(&self.state, stored.local_id, to, cx);
            }
            self.write_attachments(stored.local_id, stored.attachments, cx);
        }
    }

    fn store_body(
        &mut self,
        account: &mut AccountSync,
        local_id: Id,
        parsed: ParsedMail,
        cx: &mut Context<Self>,
    ) {
        let Some(mut message) = self.existing(local_id, cx) else {
            return;
        };
        message.preview = preview(&parsed.message.body_text);
        message.body_text = parsed.message.body_text;
        message.body_html = parsed.message.body_html;
        message.attachments = parsed.message.attachments;
        if self
            .apply(Command::SaveMessage(Box::new(message)), cx)
            .is_none()
        {
            return;
        }
        for folder in account.sync.folders.values_mut() {
            for link in folder.messages.values_mut() {
                if link.local_id == local_id {
                    link.has_body = true;
                }
            }
        }
        self.write_attachments(local_id, parsed.attachments, cx);
    }

    fn apply_remote_flags(&mut self, updates: Vec<(Id, RemoteFlags)>, cx: &mut Context<Self>) {
        let commands: Vec<Command> = updates
            .into_iter()
            .filter(|(id, _)| self.existing(*id, cx).is_some())
            .map(|(id, flags)| Command::SetMessageFlags {
                ids: vec![id],
                read: Some(flags.seen),
                flagged: Some(flags.flagged),
                pinned: None,
            })
            .collect();
        if !commands.is_empty() {
            self.apply(Command::Batch(commands), cx);
        }
    }

    fn delete_local(&mut self, ids: Vec<Id>, cx: &mut Context<Self>) {
        let ids: Vec<Id> = ids
            .into_iter()
            .filter(|id| self.existing(*id, cx).is_some())
            .collect();
        if !ids.is_empty() {
            self.apply(Command::DeleteMessages { ids }, cx);
        }
    }

    // ---------- outbox ----------

    fn outbox_folder(&mut self, account_id: Id, cx: &mut Context<Self>) -> Option<Id> {
        if let Some(folder) = self.find_outbox(account_id, cx) {
            return Some(folder.id);
        }
        let folder = Folder {
            id: 0,
            account_id,
            parent_id: None,
            name: OUTBOX_NAME.to_owned(),
            role: FolderRole::Custom,
            position: 0,
        };
        self.apply(Command::SaveFolder(Box::new(folder)), cx)
            .and_then(|inverse| created_id(&inverse))
    }

    fn find_outbox(&self, account_id: Id, cx: &App) -> Option<Folder> {
        let folders = self.state.read(cx).store().list_folders(account_id).ok()?;
        folders.into_iter().find(|folder| {
            folder.role == FolderRole::Custom
                && folder.parent_id.is_none()
                && folder.name.eq_ignore_ascii_case(OUTBOX_NAME)
        })
    }

    fn send_outbox(&mut self, account: &mut AccountSync, cx: &mut Context<Self>) {
        let Some(outbox) = self.find_outbox(account.account_id, cx) else {
            return;
        };
        let messages = match self.state.read(cx).store().list_messages(outbox.id) {
            Ok(messages) => messages,
            Err(err) => return self.report(err, cx),
        };
        let sent_path = account
            .config
            .save_sent_copy()
            .then(|| {
                account
                    .sync
                    .path_of_role(FolderRole::Sent)
                    .map(str::to_owned)
            })
            .flatten();
        // Messages that came from a server folder named "Outbox" are never (re)sent.
        let linked: HashSet<Id> = account.sync.linked_ids().collect();
        for mut message in messages {
            if account.sending.contains(&message.id)
                || account.failed.contains_key(&message.id)
                || linked.contains(&message.id)
            {
                continue;
            }
            if message.from.email.trim().is_empty() {
                message.from =
                    Address::new(message.from.name.clone(), account.config.email.clone());
            }
            let headers = outgoing_headers(&self.data_dir, &message, &account.config.email);
            let attachments = read_attachments(&self.data_dir, &message);
            let raw = match formats::build_outgoing(&message, &headers, &attachments) {
                Ok(raw) => raw,
                Err(err) => {
                    self.report(format!("Cannot send “{}”: {err}", message.subject), cx);
                    continue;
                }
            };
            // Bcc only goes to the SMTP envelope (never into the headers).
            let recipients: Vec<String> = message
                .to
                .iter()
                .chain(&message.cc)
                .chain(&headers.bcc)
                .map(|address| address.email.trim().to_owned())
                .filter(|email| !email.is_empty())
                .collect();
            let outgoing = OutgoingMessage {
                local_id: message.id,
                from: account.config.email.clone(),
                recipients,
                raw,
                sent_path: sent_path.clone(),
            };
            if account.worker.request(Request::Send(outgoing)) {
                account.sending.insert(message.id);
            }
        }
    }

    fn after_sent(&mut self, account: &mut AccountSync, local_id: Id, cx: &mut Context<Self>) {
        account.sending.remove(&local_id);
        // A "Message not sent" toast from an earlier attempt is now wrong.
        cx.defer(move |cx| {
            for window in cx.windows() {
                window
                    .update(cx, |_, window, cx| {
                        window.remove_notification1::<SendFailedToast>(local_id as u64, cx)
                    })
                    .ok();
            }
        });
        let Some(message) = self.existing(local_id, cx) else {
            return;
        };
        // When the server copy shows up in Sent (our APPEND or the provider's own), it is
        // linked to this local message instead of duplicating it.
        let headers = outgoing_headers(&self.data_dir, &message, &account.config.email);
        let message_id = normalize_id(headers.message_id.as_deref().unwrap_or_default());
        account.sync.pending_adoptions.insert(message_id, local_id);
        // Best effort: a leftover file is only read again if the message is re-queued.
        let _ = std::fs::remove_file(outgoing_headers_file(&self.data_dir, local_id));

        let sent = self
            .state
            .read(cx)
            .store()
            .folder_by_role(account.account_id, FolderRole::Sent);
        let mut commands = vec![Command::SetMessageFlags {
            ids: vec![local_id],
            read: Some(true),
            flagged: None,
            pinned: None,
        }];
        match sent {
            Ok(Some(folder)) => commands.push(Command::MoveMessages {
                ids: vec![local_id],
                folder_id: folder.id,
            }),
            Ok(None) => {}
            Err(err) => self.report(err, cx),
        }
        self.apply(Command::Batch(commands), cx);
    }

    // ---------- import / export ----------

    fn import_file(&mut self, path: PathBuf, folder_id: Id, cx: &mut Context<Self>) {
        let folder = match self.state.read(cx).store().require::<Folder>(folder_id) {
            Ok(folder) => folder,
            Err(err) => return self.report(err, cx),
        };
        self.background(
            cx,
            move || read_import(&path),
            move |bridge, result, cx| match result {
                Ok(parsed) => bridge.save_imported(parsed, &folder, cx),
                Err(err) => bridge.report(format!("Import failed: {err}"), cx),
            },
        );
    }

    fn save_imported(&mut self, parsed: Vec<ParsedMail>, folder: &Folder, cx: &mut Context<Self>) {
        let existing = self.subject_threads(folder.account_id, cx);
        let mut count = 0;
        for mut mail in parsed {
            let attachments = std::mem::take(&mut mail.attachments);
            let mut message = mail.message;
            // Messages without threading headers join the account's conversation with the
            // same subject, if any.
            if message
                .thread_id
                .starts_with(mailbox_sync::mime::SUBJECT_THREAD)
                && let Some(thread) = existing.get(&message.thread_id)
            {
                message.thread_id = thread.clone();
            }
            message.account_id = folder.account_id;
            message.folder_id = folder.id;
            message.flags.read = true;
            let id = self
                .apply(Command::SaveMessage(Box::new(message)), cx)
                .and_then(|inverse| created_id(&inverse));
            if let Some(id) = id {
                count += 1;
                self.write_attachments(id, attachments, cx);
            }
        }
        let noun = if count == 1 { "message" } else { "messages" };
        self.info(format!("Imported {count} {noun} into {}", folder.name), cx);
    }

    /// Subject thread key (see `mime::subject_thread`) → thread id of the account's newest
    /// message with that subject.
    fn subject_threads(&self, account_id: Id, cx: &App) -> HashMap<String, String> {
        let store = self.state.read(cx).store();
        let mut messages: Vec<Message> = store
            .list_folders(account_id)
            .unwrap_or_default()
            .iter()
            .flat_map(|folder| store.list_messages(folder.id).unwrap_or_default())
            .collect();
        messages.sort_by_key(|message| message.date);
        messages
            .into_iter()
            .map(|message| {
                (
                    mailbox_sync::mime::subject_thread(&message.subject),
                    message.thread_id,
                )
            })
            .collect()
    }

    fn export_folder(&mut self, folder_id: Id, path: PathBuf, cx: &mut Context<Self>) {
        let messages = match self.state.read(cx).store().list_messages(folder_id) {
            Ok(messages) => messages,
            Err(err) => return self.report(err, cx),
        };
        let data_dir = self.data_dir.clone();
        let count = messages.len();
        self.background(
            cx,
            move || write_mbox_file(&data_dir, &messages, &path),
            move |bridge, result, cx| match result {
                Ok(()) => bridge.info(format!("Exported {count} messages"), cx),
                Err(err) => bridge.report(format!("Export failed: {err}"), cx),
            },
        );
    }

    fn export_message(&mut self, message_id: Id, path: PathBuf, cx: &mut Context<Self>) {
        let Some(message) = self.existing(message_id, cx) else {
            return;
        };
        let data_dir = self.data_dir.clone();
        self.background(
            cx,
            move || -> SyncResult<()> {
                let attachments = read_attachments(&data_dir, &message);
                let raw = formats::write_eml(&message, None, &attachments)?;
                std::fs::write(&path, raw)?;
                Ok(())
            },
            |bridge, result, cx| match result {
                Ok(()) => bridge.info("Message exported", cx),
                Err(err) => bridge.report(format!("Export failed: {err}"), cx),
            },
        );
    }

    // ---------- helpers ----------

    /// Runs `work` on the background executor, then `done` on the UI thread.
    fn background<R: Send + 'static>(
        &self,
        cx: &mut Context<Self>,
        work: impl FnOnce() -> R + Send + 'static,
        done: impl FnOnce(&mut Self, R, &mut Context<Self>) + 'static,
    ) {
        let task = cx.background_executor().spawn(async move { work() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            // The bridge lives as long as the app; nothing to do if it is gone.
            let _ = this.update(cx, |bridge, cx| done(bridge, result, cx));
        })
        .detach();
    }

    fn write_attachments(
        &self,
        message_id: Id,
        attachments: Vec<AttachmentData>,
        cx: &mut Context<Self>,
    ) {
        if attachments.is_empty() {
            return;
        }
        let dir = attachments_dir(&self.data_dir, message_id);
        self.background(
            cx,
            move || write_attachment_files(&dir, &attachments),
            |bridge, result, cx| {
                if let Err(err) = result {
                    bridge.report(format!("Cannot save attachments: {err}"), cx);
                }
            },
        );
    }

    fn existing(&self, id: Id, cx: &App) -> Option<Message> {
        self.state
            .read(cx)
            .store()
            .get::<Message>(id)
            .ok()
            .flatten()
    }

    /// Applies a command outside the undo history; toasts errors. Returns the inverse.
    fn apply(&self, command: Command, cx: &mut Context<Self>) -> Option<Command> {
        self.state.update(cx, |state, cx| {
            match command::apply(state.store(), command) {
                Ok(inverse) => {
                    cx.notify();
                    Some(inverse)
                }
                Err(err) => {
                    state.report_error(err, cx);
                    None
                }
            }
        })
    }

    fn save_state(&self, account: &mut AccountSync, cx: &mut Context<Self>) {
        account.dirty = false;
        account.saved_at = Instant::now();
        if let Err(err) = account.sync.save(&account.state_path) {
            self.report(format!("Cannot save sync state: {err}"), cx);
        }
    }

    /// Runs a store write outside the undo history (sync bookkeeping); toasts errors.
    fn update_store<R>(
        &self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&Store) -> mailbox_core::Result<R>,
    ) -> Option<R> {
        self.state.update(cx, |state, cx| match f(state.store()) {
            Ok(value) => {
                cx.notify();
                Some(value)
            }
            Err(err) => {
                state.report_error(err, cx);
                None
            }
        })
    }

    /// Startup repair of what older versions left behind: the same server message stored
    /// several times (sync state lost when the app was closed mid-sync) and list previews
    /// made of a separator line. Duplicates are merged into the most complete copy; sync
    /// links are pointed at it before the merge commits (a link to a deleted id would delete
    /// the mail on the server), then the removed copies' attachment files are moved or deleted.
    fn repair_store(&mut self, cx: &mut Context<Self>) {
        let sync_dir = self.data_dir.join("sync");
        let merged = self.update_store(cx, |store| {
            store.transaction(|store| {
                store.repair_previews()?;
                decode_folder_names(store)?;
                let merged = store.merge_duplicate_messages()?;
                if !merged.is_empty() {
                    remap_sync_states(&sync_dir, &remote::merged_map(&merged))
                        .map_err(|err| mailbox_core::Error::Invalid(err.to_string()))?;
                }
                Ok(merged)
            })
        });
        let Some(merged) = merged.filter(|merged| !merged.is_empty()) else {
            return;
        };
        let data_dir = self.data_dir.clone();
        let count = merged.len();
        self.background(
            cx,
            move || {
                for merge in merged {
                    let removed = attachments_dir(&data_dir, merge.removed);
                    let kept = attachments_dir(&data_dir, merge.kept);
                    // Best effort: a leftover folder only wastes space.
                    if removed.exists() {
                        if kept.exists() {
                            let _ = std::fs::remove_dir_all(&removed);
                        } else {
                            let _ = std::fs::rename(&removed, &kept);
                        }
                    }
                }
            },
            move |bridge, (), cx| {
                let noun = if count == 1 { "copy" } else { "copies" };
                bridge.info(format!("Removed {count} duplicate message {noun}"), cx);
            },
        );
    }

    fn report(&self, err: impl std::fmt::Display, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, cx| state.report_error(err, cx));
    }

    fn info(&self, message: impl Into<String>, cx: &mut Context<Self>) {
        let message = message.into();
        self.state
            .update(cx, |state, cx| state.notify_info(message, cx));
    }

    /// "Message not sent" toast with a Retry button (the message stays in the Outbox).
    fn notify_send_failure(
        &self,
        local_id: Id,
        subject: String,
        error: String,
        cx: &mut Context<Self>,
    ) {
        let window = cx.active_window().or_else(|| cx.windows().first().copied());
        let Some(window) = window else {
            return self.report(format!("Could not send the message: {error}"), cx);
        };
        let subject = if subject.trim().is_empty() {
            "(no subject)".to_owned()
        } else {
            subject
        };
        let message = format!("“{subject}” stays in the Outbox. {error}");
        let notification = toast(
            Notification::new().with_type(NotificationType::Error),
            &message,
        )
        .id1::<SendFailedToast>(local_id as u64)
        .title("Message not sent")
        .action(move |_, _, _| {
            Button::new("retry-send")
                .label("Retry")
                .on_click(move |_, window, cx| {
                    window.remove_notification1::<SendFailedToast>(local_id as u64, cx);
                    retry_outbox(cx)
                })
        });
        // Deferred: a window may be mid-update when the event is applied.
        cx.defer(move |cx| {
            window
                .update(cx, |_, window, cx| {
                    window.push_notification(notification, cx)
                })
                .ok();
        });
    }

    fn refresh_status(&self, cx: &mut Context<Self>) {
        let unsent = self
            .accounts
            .values()
            .map(|account| account.failed.len())
            .sum();
        let busy = self.accounts.values().find(|account| account.busy);
        let error = self.accounts.values().find_map(|account| {
            account
                .last_error
                .as_ref()
                .map(|error| (&account.config.email, error))
        });
        let (phase, detail) = if self.accounts.is_empty() {
            (SyncPhase::NoAccount, String::new())
        } else if self.offline {
            (SyncPhase::Offline, "Working offline".to_owned())
        } else if let Some(account) = busy {
            let detail = if account.progress.is_empty() {
                format!("Syncing {}…", account.config.email)
            } else {
                format!("{}: {}", account.config.email, account.progress)
            };
            (SyncPhase::Syncing, detail)
        } else if let Some((email, error)) = error {
            (SyncPhase::Error, format!("{email}: {error}"))
        } else if let Some(time) = self.last_success {
            (
                SyncPhase::Idle,
                format!("Up to date · {}", time.format("%H:%M")),
            )
        } else {
            (SyncPhase::Syncing, "Waiting to sync".to_owned())
        };
        let summary = SyncSummary {
            phase,
            last_synced: self.last_success,
            detail: detail.clone().into(),
            unsent,
        };
        self.state.update(cx, |state, cx| {
            state.set_sync_status(detail, cx);
            state.set_sync_summary(summary, cx);
        });
    }
}

// ---------- free helpers (no UI state) ----------

/// Sync settings of a stored account: its `sync.json` override, else the servers saved by the
/// account wizard. `None` = not set up for sync.
fn account_config(
    store: &Store,
    account: &Account,
    overrides: &[AccountConfig],
) -> Option<AccountConfig> {
    if let Some(config) = overrides
        .iter()
        .find(|config| config.email.eq_ignore_ascii_case(&account.email))
    {
        return Some(config.clone());
    }
    let servers = AccountServers::load(store, account.id).ok().flatten()?;
    if servers.imap.host.trim().is_empty() || servers.smtp.host.trim().is_empty() {
        return None;
    }
    let auth: AuthConfig = core_settings::get_json(store, &config::auth_setting_key(account.id))
        .ok()
        .flatten()
        .unwrap_or_default();
    Some(AccountConfig::from_servers(&account.email, &servers, auth))
}

/// Folder names stored undecoded by older versions ("Messages envoy&AOk-s") get their real
/// name ("Messages envoyés"). Returns how many changed.
fn decode_folder_names(store: &Store) -> mailbox_core::Result<usize> {
    let mut changed = 0;
    for account in store.list_accounts()? {
        for mut folder in store.list_folders(account.id)? {
            let name = utf7::decode(&folder.name);
            if name != folder.name {
                folder.name = name;
                store.save(&mut folder)?;
                changed += 1;
            }
        }
    }
    Ok(changed)
}

/// Points every sync state file in `dir` at the messages that absorbed merged duplicates.
fn remap_sync_states(dir: &Path, merged: &HashMap<Id, Id>) -> SyncResult<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            let mut state = SyncState::load(&path)?;
            if state.remap_local_ids(merged) {
                state.save(&path)?;
            }
        }
    }
    Ok(())
}

fn state_path(data_dir: &Path, account_id: Id) -> PathBuf {
    data_dir.join("sync").join(format!("{account_id}.json"))
}

fn outgoing_headers_file(data_dir: &Path, message_id: Id) -> PathBuf {
    data_dir.join("outbox").join(format!("{message_id}.json"))
}

/// Compose headers of a queued message (defaults when compose left none), with a stable
/// Message-ID so a retried send keeps the same one.
fn outgoing_headers(data_dir: &Path, message: &Message, email: &str) -> OutgoingHeaders {
    let mut headers =
        OutgoingHeaders::load(&outgoing_headers_file(data_dir, message.id)).unwrap_or_default();
    if headers
        .message_id
        .as_deref()
        .is_none_or(|id| id.trim().is_empty())
    {
        headers.message_id = Some(outgoing_message_id(message, email));
    }
    headers
}

/// `MAILBOX_DATA_DIR`, else %APPDATA%/mailbox, $XDG_DATA_HOME/mailbox or ~/.local/share/mailbox:
/// the database (`mailbox.db`), sync state, attachments and the Outbox headers live there.
pub fn data_dir() -> PathBuf {
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

fn attachments_dir(data_dir: &Path, message_id: Id) -> PathBuf {
    data_dir.join("attachments").join(message_id.to_string())
}

/// A safe file name (no path separators or reserved characters).
fn file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || r#"<>:"/\|?*"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_owned();
    if cleaned.is_empty() {
        "attachment".to_owned()
    } else {
        cleaned
    }
}

fn write_attachment_files(dir: &Path, attachments: &[AttachmentData]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for attachment in attachments {
        std::fs::write(dir.join(file_name(&attachment.name)), &attachment.bytes)?;
    }
    Ok(())
}

/// Attachment files present on disk for a message (missing ones are skipped).
fn read_attachments(data_dir: &Path, message: &Message) -> Vec<AttachmentData> {
    let dir = attachments_dir(data_dir, message.id);
    message
        .attachments
        .iter()
        .filter_map(|attachment| {
            let bytes = std::fs::read(dir.join(file_name(&attachment.name))).ok()?;
            Some(AttachmentData {
                name: attachment.name.clone(),
                mime: attachment.mime.clone(),
                bytes,
            })
        })
        .collect()
}

fn read_import(path: &Path) -> SyncResult<Vec<ParsedMail>> {
    let bytes = std::fs::read(path)?;
    let is_mbox = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mbox"))
        || bytes.starts_with(b"From ");
    if is_mbox {
        Ok(formats::read_mbox(&bytes))
    } else {
        Ok(vec![formats::read_eml(&bytes)?])
    }
}

fn write_mbox_file(data_dir: &Path, messages: &[Message], path: &Path) -> SyncResult<()> {
    let mut entries = Vec::with_capacity(messages.len());
    for message in messages.iter().rev() {
        let attachments = read_attachments(data_dir, message);
        entries.push(MboxEntry {
            sender: message.from.email.clone(),
            date: message.date,
            raw: formats::build_rfc5322(message, None, &attachments)?,
        });
    }
    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
    formats::write_mbox(&mut file, &entries)?;
    std::io::Write::flush(&mut file)?;
    Ok(())
}

/// Stable per message, so a retried send keeps the same Message-ID.
fn outgoing_message_id(message: &Message, email: &str) -> String {
    let domain = email
        .rsplit_once('@')
        .map_or("mailbox.local", |(_, domain)| domain);
    format!(
        "<mailbox.{}.{}@{domain}>",
        message.id,
        message.date.timestamp()
    )
}

fn display_name(folder: &RemoteFolder) -> String {
    if folder.role == FolderRole::Inbox {
        "Inbox".to_owned()
    } else {
        folder.name.clone()
    }
}

/// Id assigned by a `Save*` command with id 0 (its inverse deletes that id).
fn created_id(inverse: &Command) -> Option<Id> {
    match inverse {
        Command::DeleteMessages { ids } => ids.first().copied(),
        Command::DeleteFolder(id) => Some(*id),
        _ => None,
    }
}

fn poll_interval(config: &AccountConfig) -> Duration {
    Duration::from_secs(u64::from(config.poll_minutes.max(1)) * 60)
}

/// 30 s, 1 min, 2 min... capped at [`MAX_BACKOFF`].
fn backoff(failures: u32) -> Duration {
    let factor = 1u32 << failures.saturating_sub(1).min(5);
    (Duration::from_secs(30) * factor).min(MAX_BACKOFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outgoing_headers_fall_back_to_a_stable_id() {
        let dir = std::env::temp_dir().join(format!("mailbox-outgoing-{}", std::process::id()));
        let mut message = test_message();
        message.id = 42;
        let headers = outgoing_headers(&dir, &message, "me@example.com");
        assert_eq!(
            headers.message_id.as_deref(),
            Some("<mailbox.42.1700000000@example.com>")
        );
        assert!(headers.bcc.is_empty());

        let saved = OutgoingHeaders {
            message_id: Some("<draft@example.com>".into()),
            bcc: vec![Address::new("", "hidden@example.com")],
            ..OutgoingHeaders::default()
        };
        saved.save(&outgoing_headers_file(&dir, 42)).unwrap();
        assert_eq!(outgoing_headers(&dir, &message, "me@example.com"), saved);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff(1), Duration::from_secs(30));
        assert_eq!(backoff(2), Duration::from_secs(60));
        assert_eq!(backoff(20), MAX_BACKOFF);
    }

    #[test]
    fn file_names_are_sanitized() {
        assert_eq!(file_name("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(file_name("a:b?.pdf"), "a_b_.pdf");
        assert_eq!(file_name("  "), "attachment");
    }

    #[test]
    fn created_ids() {
        assert_eq!(
            created_id(&Command::DeleteMessages { ids: vec![4] }),
            Some(4)
        );
        assert_eq!(created_id(&Command::DeleteFolder(9)), Some(9));
        assert_eq!(created_id(&Command::Batch(vec![])), None);
    }

    #[test]
    fn undecoded_folder_names_are_renamed_once() {
        let store = Store::open_in_memory().unwrap();
        let mut account = Account {
            id: 0,
            name: "Me".into(),
            email: "me@example.com".into(),
            color: 0,
        };
        store.save(&mut account).unwrap();
        for (name, role) in [
            ("Messages envoy&AOk-s", FolderRole::Sent),
            ("R&D", FolderRole::Custom),
            ("Projets", FolderRole::Custom),
        ] {
            let mut folder = Folder {
                id: 0,
                account_id: account.id,
                parent_id: None,
                name: name.into(),
                role,
                position: 0,
            };
            store.save(&mut folder).unwrap();
        }
        assert_eq!(decode_folder_names(&store).unwrap(), 1);
        let names: Vec<String> = store
            .list_folders(account.id)
            .unwrap()
            .into_iter()
            .map(|folder| folder.name)
            .collect();
        assert!(names.contains(&"Messages envoyés".to_owned()), "{names:?}");
        assert!(names.contains(&"R&D".to_owned()));
        assert_eq!(decode_folder_names(&store).unwrap(), 0);
    }

    fn test_message() -> Message {
        Message {
            id: 3,
            account_id: 1,
            folder_id: 1,
            thread_id: String::new(),
            from: Address::default(),
            to: vec![],
            cc: vec![],
            subject: String::new(),
            date: DateTime::from_timestamp(1_700_000_000, 0).unwrap_or_default(),
            preview: String::new(),
            body_text: String::new(),
            body_html: None,
            flags: Default::default(),
            importance: Default::default(),
            categories: vec![],
            attachments: vec![],
        }
    }

    #[test]
    fn outgoing_ids_are_stable() {
        let message = test_message();
        assert_eq!(
            outgoing_message_id(&message, "me@example.com"),
            "<mailbox.3.1700000000@example.com>"
        );
    }
}
