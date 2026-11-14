//! Bridge between [`AppState`] and the background sync workers (`mailbox-sync`).
//!
//! - One worker per account configured in `<data dir>/sync.json` (matched by email); the
//!   password/refresh token comes from the OS credential store.
//! - Every [`POLL_INTERVAL`] the bridge drains worker events and turns them into
//!   `mailbox_core::Command`s applied with `command::apply` (not the undo history: undoing
//!   "a message arrived" makes no sense), then refreshes `AppState::sync_status`.
//! - Local edits are found by diffing the store against the sync state, so changes made while
//!   offline are pushed on the next sync. Messages placed in the account's "Outbox" folder
//!   are sent, then moved to Sent.
//! - Actions: [`SendReceive`] (F9) syncs now, [`ToggleWorkOffline`].
//!
//! Entry points for other modules: [`send_receive`], [`set_offline`], [`is_offline`],
//! [`request_body`], [`outbox_folder`], [`import_file`], [`export_folder`],
//! [`export_message`], [`attachment_path`], [`reload`].

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use gpui::{App, AppContext as _, Context, Entity, Global, Task};
use mailbox_core::{Address, Command, Folder, FolderRole, Id, Message, command};
use mailbox_sync::config::{self, AccountConfig};
use mailbox_sync::engine::{FetchedMessage, SyncInput};
use mailbox_sync::formats::{self, MboxEntry};
use mailbox_sync::imap_smtp::ImapSmtpBackend;
use mailbox_sync::mime::preview;
use mailbox_sync::state::{Link, LocalMessage};
use mailbox_sync::thread::normalize_id;
use mailbox_sync::worker::BackendFactory;
use mailbox_sync::{
    AttachmentData, MailBackend, OutgoingMessage, ParsedMail, RemoteFlags, RemoteFolder, Request,
    SyncEvent, SyncResult, SyncState, WorkerHandle,
};

use crate::app_state::AppState;
use crate::{SendReceive, ToggleWorkOffline};

/// How often worker events are applied to the store.
const POLL_INTERVAL: Duration = Duration::from_millis(300);
/// Longest wait between retries after failures.
const MAX_BACKOFF: Duration = Duration::from_secs(15 * 60);
/// First sync of a folder downloads at most this many (newest) messages.
const MAX_INITIAL: usize = 5000;
/// Local folder (role `Custom`) whose messages are sent on the next Send/Receive.
pub const OUTBOX_NAME: &str = "Outbox";

struct GlobalSyncBridge(Entity<SyncBridge>);

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
    next_sync: Instant,
    failures: u32,
    busy: bool,
    last_error: Option<String>,
    progress: String,
    /// Outbox messages handed to the worker.
    sending: HashSet<Id>,
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
        bridge.start_accounts(cx);
        bridge
    }

    pub fn is_offline(&self) -> bool {
        self.offline
    }

    // ---------- lifecycle ----------

    fn start_accounts(&mut self, cx: &mut Context<Self>) {
        let configs = match config::load_accounts(&self.data_dir.join("sync.json")) {
            Ok(configs) => configs,
            Err(err) => {
                self.report(format!("Cannot read sync settings: {err}"), cx);
                return;
            }
        };
        let accounts = match self.state.read(cx).store().list_accounts() {
            Ok(accounts) => accounts,
            Err(err) => {
                self.report(err, cx);
                return;
            }
        };
        for account in accounts {
            let Some(config) = configs
                .iter()
                .find(|config| config.email.eq_ignore_ascii_case(&account.email))
            else {
                continue;
            };
            if let Err(err) = self.start(account.id, config.clone()) {
                self.report(
                    format!("Cannot start sync for {}: {err}", account.email),
                    cx,
                );
            }
        }
        self.refresh_status(cx);
    }

    fn start(&mut self, account_id: Id, config: AccountConfig) -> SyncResult<()> {
        let state_path = self
            .data_dir
            .join("sync")
            .join(format!("{account_id}.json"));
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
                next_sync: Instant::now(),
                failures: 0,
                busy: false,
                last_error: None,
                progress: String::new(),
                sending: HashSet::new(),
                fetching: HashSet::new(),
                new_mail: Vec::new(),
            },
        );
        Ok(())
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        for account in self.accounts.values() {
            self.save_state(account, cx);
        }
        self.accounts.clear();
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
            for event in account.worker.drain() {
                self.apply_event(&mut account, event, cx);
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
            SyncEvent::Pushed(op) => account.sync.apply_pushed(&op),
            SyncEvent::FolderReset { path } => {
                let ids = account.sync.reset_folder(&path);
                self.delete_local(ids, cx);
            }
            SyncEvent::NewMessages { path, messages } => {
                self.store_new(account, &path, messages, cx);
            }
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
                self.report(format!("Could not send the message: {error}"), cx);
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
        messages: Vec<FetchedMessage>,
        cx: &mut Context<Self>,
    ) {
        let Some(folder_id) = account.sync.local_folder(path) else {
            return;
        };
        let is_inbox = account
            .sync
            .folders
            .get(path)
            .is_some_and(|folder| folder.role == FolderRole::Inbox);
        for fetched in messages {
            let FetchedMessage {
                uid,
                flags,
                has_body,
                mut parsed,
            } = fetched;
            parsed.assign_thread(&mut account.sync.threads);
            let attachments = std::mem::take(&mut parsed.attachments);
            let message_id = parsed.message_id.clone();
            let receipt_to = parsed.receipt_to.take();
            let mut message = parsed.message;
            message.account_id = account.account_id;
            message.folder_id = folder_id;

            let adopted = account
                .sync
                .take_adoption(&message_id)
                .and_then(|id| self.existing(id, cx));
            let local_id = match adopted {
                Some(existing) => {
                    message.id = existing.id;
                    message.categories = existing.categories;
                    message.flags.pinned = existing.flags.pinned;
                    if !has_body {
                        message.body_text = existing.body_text;
                        message.body_html = existing.body_html;
                        message.preview = existing.preview;
                        message.attachments = existing.attachments;
                    }
                    self.apply(Command::SaveMessage(Box::new(message)), cx)
                        .map(|_| existing.id)
                }
                None => {
                    let unread = !message.flags.read;
                    let id = self
                        .apply(Command::SaveMessage(Box::new(message)), cx)
                        .and_then(|inverse| created_id(&inverse));
                    if let Some(id) = id
                        && is_inbox
                        && unread
                    {
                        account.new_mail.push(id);
                    }
                    id
                }
            };
            let Some(local_id) = local_id else {
                continue;
            };
            if let Some(to) = receipt_to.filter(|_| is_inbox) {
                crate::receipts::note_request(&self.state, local_id, to, cx);
            }
            account.sync.link(
                path,
                uid,
                Link {
                    local_id,
                    flags,
                    message_id,
                    has_body,
                },
            );
            self.write_attachments(local_id, attachments, cx);
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
            if account.sending.contains(&message.id) || linked.contains(&message.id) {
                continue;
            }
            if message.from.email.trim().is_empty() {
                message.from =
                    Address::new(message.from.name.clone(), account.config.email.clone());
            }
            let message_id = outgoing_message_id(&message, &account.config.email);
            let attachments = read_attachments(&self.data_dir, &message);
            let raw = match formats::build_rfc5322(&message, Some(&message_id), &attachments) {
                Ok(raw) => raw,
                Err(err) => {
                    self.report(format!("Cannot send “{}”: {err}", message.subject), cx);
                    continue;
                }
            };
            let recipients: Vec<String> = message
                .to
                .iter()
                .chain(&message.cc)
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
        let Some(message) = self.existing(local_id, cx) else {
            return;
        };
        // When the server copy shows up in Sent (our APPEND or the provider's own), it is
        // linked to this local message instead of duplicating it.
        let message_id = normalize_id(&outgoing_message_id(&message, &account.config.email));
        account.sync.pending_adoptions.insert(message_id, local_id);

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
        let mut count = 0;
        for mut mail in parsed {
            let attachments = std::mem::take(&mut mail.attachments);
            let mut message = mail.message;
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
        self.info(
            format!("Imported {count} messages into {}", folder.name),
            cx,
        );
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

    fn save_state(&self, account: &AccountSync, cx: &mut Context<Self>) {
        if let Err(err) = account.sync.save(&account.state_path) {
            self.report(format!("Cannot save sync state: {err}"), cx);
        }
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

    fn refresh_status(&self, cx: &mut Context<Self>) {
        let status = if self.accounts.is_empty() {
            String::new()
        } else if self.offline {
            "Working offline".to_owned()
        } else if let Some(account) = self.accounts.values().find(|account| account.busy) {
            if account.progress.is_empty() {
                format!("Syncing {}…", account.config.email)
            } else {
                format!("{}: {}", account.config.email, account.progress)
            }
        } else if let Some((email, error)) = self.accounts.values().find_map(|account| {
            account
                .last_error
                .as_ref()
                .map(|error| (&account.config.email, error))
        }) {
            format!("{email}: {error}")
        } else if let Some(time) = self.last_success {
            format!("Up to date · {}", time.format("%H:%M"))
        } else {
            "Waiting to sync".to_owned()
        };
        self.state
            .update(cx, |state, cx| state.set_sync_status(status, cx));
    }
}

// ---------- free helpers (no UI state) ----------

/// `MAILBOX_DATA_DIR`, else %APPDATA%/mailbox, $XDG_DATA_HOME/mailbox or ~/.local/share/mailbox.
pub(crate) fn data_dir() -> PathBuf {
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
    fn outgoing_ids_are_stable() {
        let message = Message {
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
        };
        assert_eq!(
            outgoing_message_id(&message, "me@example.com"),
            "<mailbox.3.1700000000@example.com>"
        );
    }
}
