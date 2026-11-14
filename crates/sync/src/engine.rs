//! One sync cycle over a [`MailBackend`], as pure logic: the caller passes what it knows
//! (cursors, pending local changes) and receives [`SyncEvent`]s to apply to the store.
//!
//! Order: push local changes → list folders → per folder (Inbox first): UIDVALIDITY check,
//! vanished UIDs, remote flag changes, new UIDs (headers first, full source only for mail
//! newer than `offline_days`).

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use mailbox_core::{FolderRole, Id};

use crate::backend::{MailBackend, RemoteFlags, RemoteFolder, Uid};
use crate::mime::{ParsedMail, parse_message};
use crate::{SyncError, SyncResult};

/// Messages fetched per round trip (and per [`SyncEvent::NewMessages`]).
pub const CHUNK: usize = 50;

/// A local change to replay on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushOp {
    SetFlags {
        path: String,
        uid: Uid,
        flags: RemoteFlags,
    },
    Move {
        path: String,
        uid: Uid,
        target: String,
    },
    Delete {
        path: String,
        uid: Uid,
    },
}

impl PushOp {
    pub fn path(&self) -> &str {
        match self {
            Self::SetFlags { path, .. } | Self::Move { path, .. } | Self::Delete { path, .. } => {
                path
            }
        }
    }

    pub fn uid(&self) -> Uid {
        match self {
            Self::SetFlags { uid, .. } | Self::Move { uid, .. } | Self::Delete { uid, .. } => *uid,
        }
    }
}

/// What the caller already has for a folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderCursor {
    /// 0 = never synced.
    pub uid_validity: u32,
    pub highest_uid: Uid,
    /// UIDs stored locally, with the flags last seen on the server.
    pub known: BTreeMap<Uid, RemoteFlags>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SyncInput {
    pub push: Vec<PushOp>,
    pub cursors: BTreeMap<String, FolderCursor>,
    /// Mail newer than this many days is fetched with bodies; older mail headers only.
    pub offline_days: u32,
    /// First sync of a folder fetches at most this many (newest) messages.
    pub max_initial: usize,
}

/// A new server message. `parsed.message` has flags set and ids 0.
#[derive(Debug, Clone, PartialEq)]
pub struct FetchedMessage {
    pub uid: Uid,
    pub flags: RemoteFlags,
    /// `false` when only headers were fetched (body on demand).
    pub has_body: bool,
    pub parsed: ParsedMail,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SyncEvent {
    /// Human-readable progress or a non-fatal problem.
    Progress(String),
    Folders(Vec<RemoteFolder>),
    /// A local change reached the server.
    Pushed(PushOp),
    /// UIDVALIDITY changed: every local copy of this folder is stale.
    FolderReset {
        path: String,
    },
    NewMessages {
        path: String,
        messages: Vec<FetchedMessage>,
    },
    FlagsChanged {
        path: String,
        changes: Vec<(Uid, RemoteFlags)>,
    },
    /// Deleted or moved away on the server.
    Vanished {
        path: String,
        uids: Vec<Uid>,
    },
    /// The folder is up to date: store the new cursor.
    FolderSynced {
        path: String,
        uid_validity: u32,
        highest_uid: Uid,
    },
    BodyFetched {
        local_id: Id,
        parsed: Box<ParsedMail>,
    },
    Sent {
        local_id: Id,
    },
    SendFailed {
        local_id: Id,
        error: String,
    },
    /// The IDLE connection saw a change: sync soon.
    RemoteChanged,
    /// End of a [`crate::worker::Request::Sync`], with the error that stopped it, if any.
    Finished {
        error: Option<String>,
    },
}

/// Runs one full cycle. Connection errors abort (retry later); per-message problems are
/// reported as [`SyncEvent::Progress`] and skipped.
pub fn run_cycle(
    backend: &mut dyn MailBackend,
    mut input: SyncInput,
    now: DateTime<Utc>,
    emit: &mut dyn FnMut(SyncEvent),
) -> SyncResult<()> {
    backend.connect()?;

    for op in std::mem::take(&mut input.push) {
        let result = match &op {
            PushOp::SetFlags { path, uid, flags } => backend.set_flags(path, &[*uid], *flags),
            PushOp::Move { path, uid, target } => backend.move_messages(path, &[*uid], target),
            PushOp::Delete { path, uid } => backend.delete_messages(path, &[*uid]),
        };
        match result {
            Ok(()) => {
                if let Some(cursor) = input.cursors.get_mut(op.path()) {
                    match &op {
                        PushOp::SetFlags { uid, flags, .. } => {
                            cursor.known.insert(*uid, *flags);
                        }
                        PushOp::Move { uid, .. } | PushOp::Delete { uid, .. } => {
                            cursor.known.remove(uid);
                        }
                    }
                }
                emit(SyncEvent::Pushed(op));
            }
            Err(err @ (SyncError::Connection(_) | SyncError::Io(_))) => return Err(err),
            Err(err) => emit(SyncEvent::Progress(format!(
                "could not update message {} in {}: {err}",
                op.uid(),
                op.path()
            ))),
        }
    }

    let mut folders = backend.list_folders()?;
    folders.sort_by_key(|folder| (folder.role != FolderRole::Inbox, folder.path.clone()));
    emit(SyncEvent::Folders(folders.clone()));

    let cutoff = now - Duration::days(i64::from(input.offline_days));
    for folder in &folders {
        let cursor = input.cursors.remove(&folder.path).unwrap_or_default();
        emit(SyncEvent::Progress(format!("Checking {}", folder.name)));
        sync_folder(
            backend,
            &folder.path,
            cursor,
            cutoff,
            input.max_initial,
            emit,
        )?;
    }
    Ok(())
}

fn sync_folder(
    backend: &mut dyn MailBackend,
    path: &str,
    mut cursor: FolderCursor,
    cutoff: DateTime<Utc>,
    max_initial: usize,
    emit: &mut dyn FnMut(SyncEvent),
) -> SyncResult<()> {
    let status = backend.status(path)?;
    if cursor.uid_validity != 0 && cursor.uid_validity != status.uid_validity {
        emit(SyncEvent::FolderReset {
            path: path.to_owned(),
        });
        cursor = FolderCursor::default();
    }

    let remote: BTreeMap<Uid, RemoteFlags> = backend.fetch_flags(path)?.into_iter().collect();

    let vanished: Vec<Uid> = cursor
        .known
        .keys()
        .filter(|uid| !remote.contains_key(uid))
        .copied()
        .collect();
    if !vanished.is_empty() {
        emit(SyncEvent::Vanished {
            path: path.to_owned(),
            uids: vanished,
        });
    }

    let changes: Vec<(Uid, RemoteFlags)> = cursor
        .known
        .iter()
        .filter_map(|(uid, known)| {
            let now = remote.get(uid)?;
            (now != known).then_some((*uid, *now))
        })
        .collect();
    if !changes.is_empty() {
        emit(SyncEvent::FlagsChanged {
            path: path.to_owned(),
            changes,
        });
    }

    let mut new_uids: Vec<Uid> = remote
        .keys()
        .copied()
        .filter(|uid| *uid > cursor.highest_uid && !cursor.known.contains_key(uid))
        .collect();
    if cursor.highest_uid == 0 && new_uids.len() > max_initial {
        new_uids = new_uids.split_off(new_uids.len() - max_initial);
    }
    // Newest first so recent mail shows up before the backlog.
    for chunk in new_uids.rchunks(CHUNK) {
        let messages = fetch_new(backend, path, chunk, cutoff, emit)?;
        if !messages.is_empty() {
            emit(SyncEvent::NewMessages {
                path: path.to_owned(),
                messages,
            });
        }
    }

    let highest_uid = remote
        .keys()
        .next_back()
        .copied()
        .unwrap_or(0)
        .max(cursor.highest_uid);
    emit(SyncEvent::FolderSynced {
        path: path.to_owned(),
        uid_validity: status.uid_validity,
        highest_uid,
    });
    Ok(())
}

fn fetch_new(
    backend: &mut dyn MailBackend,
    path: &str,
    uids: &[Uid],
    cutoff: DateTime<Utc>,
    emit: &mut dyn FnMut(SyncEvent),
) -> SyncResult<Vec<FetchedMessage>> {
    let mut messages = Vec::with_capacity(uids.len());
    for raw in backend.fetch_headers(path, uids)? {
        match parse_message(&raw.data, raw.internal_date) {
            Ok(parsed) => messages.push(FetchedMessage {
                uid: raw.uid,
                flags: raw.flags,
                has_body: false,
                parsed,
            }),
            Err(err) => emit(SyncEvent::Progress(format!(
                "skipped message {} in {path}: {err}",
                raw.uid
            ))),
        }
    }

    let recent: Vec<Uid> = messages
        .iter()
        .filter(|message| message.parsed.message.date >= cutoff)
        .map(|message| message.uid)
        .collect();
    if !recent.is_empty() {
        for raw in backend.fetch_full(path, &recent)? {
            let Some(message) = messages.iter_mut().find(|message| message.uid == raw.uid) else {
                continue;
            };
            if let Ok(parsed) = parse_message(&raw.data, raw.internal_date) {
                message.parsed = parsed;
                message.has_body = true;
            }
        }
    }

    for message in &mut messages {
        message.parsed.message.flags.read = message.flags.seen;
        message.parsed.message.flags.flagged = message.flags.flagged;
    }
    Ok(messages)
}

/// Fetches and parses the full source of one message (body on demand).
pub fn fetch_body(backend: &mut dyn MailBackend, path: &str, uid: Uid) -> SyncResult<ParsedMail> {
    backend.connect()?;
    let raw = backend
        .fetch_full(path, &[uid])?
        .into_iter()
        .next()
        .ok_or_else(|| SyncError::Protocol(format!("message {uid} not found in {path}")))?;
    parse_message(&raw.data, raw.internal_date)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::MemoryBackend;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 14, 12, 0, 0).unwrap()
    }

    fn mail(id: &str, date: &str) -> Vec<u8> {
        format!(
            "From: a@example.com\r\nTo: b@example.com\r\nSubject: {id}\r\nDate: {date}\r\n\
             Message-ID: <{id}@example.com>\r\n\r\nBody of {id}\r\n"
        )
        .into_bytes()
    }

    const RECENT: &str = "Fri, 13 Nov 2026 10:00:00 +0000";
    const OLD: &str = "Mon, 01 Jan 2024 10:00:00 +0000";

    fn server() -> MemoryBackend {
        let mut server = MemoryBackend::default();
        server.add_folder("Archive", FolderRole::Archive);
        server.add_folder("INBOX", FolderRole::Inbox);
        server.connected = true;
        server
            .deliver("INBOX", &mail("one", RECENT), RemoteFlags::default())
            .unwrap();
        server
            .deliver(
                "INBOX",
                &mail("old", OLD),
                RemoteFlags {
                    seen: true,
                    flagged: false,
                },
            )
            .unwrap();
        server.connected = false;
        server
    }

    fn input(cursors: BTreeMap<String, FolderCursor>) -> SyncInput {
        SyncInput {
            push: vec![],
            cursors,
            offline_days: 30,
            max_initial: 1000,
        }
    }

    fn run(server: &mut MemoryBackend, input: SyncInput) -> Vec<SyncEvent> {
        let mut events = Vec::new();
        run_cycle(server, input, now(), &mut |event| events.push(event)).unwrap();
        events
    }

    fn new_messages(events: &[SyncEvent]) -> Vec<&FetchedMessage> {
        events
            .iter()
            .flat_map(|event| match event {
                SyncEvent::NewMessages { messages, .. } => messages.iter().collect(),
                _ => vec![],
            })
            .collect()
    }

    /// The cursor a caller would hold after applying `events` for INBOX.
    fn inbox_cursor(events: &[SyncEvent]) -> FolderCursor {
        let mut cursor = FolderCursor::default();
        for event in events {
            match event {
                SyncEvent::NewMessages { path, messages } if path == "INBOX" => {
                    for message in messages {
                        cursor.known.insert(message.uid, message.flags);
                    }
                }
                SyncEvent::FolderSynced {
                    path,
                    uid_validity,
                    highest_uid,
                } if path == "INBOX" => {
                    cursor.uid_validity = *uid_validity;
                    cursor.highest_uid = *highest_uid;
                }
                _ => {}
            }
        }
        cursor
    }

    #[test]
    fn first_sync_lists_folders_inbox_first_and_fetches_everything() {
        let mut server = server();
        let events = run(&mut server, input(BTreeMap::new()));
        let SyncEvent::Folders(folders) = &events[0] else {
            panic!("folders first, got {events:?}");
        };
        assert_eq!(folders[0].path, "INBOX");

        let messages = new_messages(&events);
        assert_eq!(messages.len(), 2);
        let one = messages
            .iter()
            .find(|m| m.parsed.message.subject == "one")
            .unwrap();
        let old = messages
            .iter()
            .find(|m| m.parsed.message.subject == "old")
            .unwrap();
        assert!(one.has_body);
        assert_eq!(one.parsed.message.body_text.trim(), "Body of one");
        assert!(!old.has_body, "older than offline_days: headers only");
        assert!(old.parsed.message.flags.read);
        assert_eq!(inbox_cursor(&events).highest_uid, 2);
    }

    #[test]
    fn incremental_sync_only_fetches_new_uids() {
        let mut server = server();
        let first = run(&mut server, input(BTreeMap::new()));
        let cursor = inbox_cursor(&first);

        server.connected = true;
        server
            .deliver("INBOX", &mail("two", RECENT), RemoteFlags::default())
            .unwrap();
        let events = run(
            &mut server,
            input(BTreeMap::from([("INBOX".into(), cursor)])),
        );
        let messages = new_messages(&events);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].uid, 3);
        assert_eq!(messages[0].parsed.message_id, "two@example.com");
    }

    #[test]
    fn detects_remote_flag_changes_and_vanished_messages() {
        let mut server = server();
        let cursor = inbox_cursor(&run(&mut server, input(BTreeMap::new())));

        let inbox = server.folders.get_mut("INBOX").unwrap();
        inbox.messages.get_mut(&1).unwrap().0.flagged = true;
        inbox.messages.remove(&2);
        let events = run(
            &mut server,
            input(BTreeMap::from([("INBOX".into(), cursor)])),
        );
        assert!(events.contains(&SyncEvent::FlagsChanged {
            path: "INBOX".into(),
            changes: vec![(
                1,
                RemoteFlags {
                    seen: false,
                    flagged: true
                }
            )],
        }));
        assert!(events.contains(&SyncEvent::Vanished {
            path: "INBOX".into(),
            uids: vec![2],
        }));
        assert!(new_messages(&events).is_empty());
    }

    #[test]
    fn uid_validity_change_resets_the_folder() {
        let mut server = server();
        let cursor = inbox_cursor(&run(&mut server, input(BTreeMap::new())));
        server.folders.get_mut("INBOX").unwrap().uid_validity = 99;

        let events = run(
            &mut server,
            input(BTreeMap::from([("INBOX".into(), cursor)])),
        );
        assert!(events.contains(&SyncEvent::FolderReset {
            path: "INBOX".into()
        }));
        assert_eq!(
            new_messages(&events).len(),
            2,
            "everything is fetched again"
        );
        assert_eq!(inbox_cursor(&events).uid_validity, 99);
    }

    #[test]
    fn pushes_local_changes_before_fetching() {
        let mut server = server();
        let cursor = inbox_cursor(&run(&mut server, input(BTreeMap::new())));
        let mut sync = input(BTreeMap::from([("INBOX".into(), cursor)]));
        let read = RemoteFlags {
            seen: true,
            flagged: true,
        };
        sync.push = vec![
            PushOp::SetFlags {
                path: "INBOX".into(),
                uid: 1,
                flags: read,
            },
            PushOp::Move {
                path: "INBOX".into(),
                uid: 2,
                target: "Archive".into(),
            },
        ];
        let events = run(&mut server, sync);

        assert_eq!(server.folders["INBOX"].messages[&1].0, read);
        assert!(!server.folders["INBOX"].messages.contains_key(&2));
        assert_eq!(server.folders["Archive"].messages.len(), 1);
        let pushed = events
            .iter()
            .filter(|e| matches!(e, SyncEvent::Pushed(_)))
            .count();
        assert_eq!(pushed, 2);
        // Our own changes are not reported back as remote changes.
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SyncEvent::FlagsChanged { .. }))
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SyncEvent::Vanished { .. }))
        );
        // The moved message appears in Archive as a new message (the caller adopts it).
        let moved = new_messages(&events);
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].parsed.message_id, "old@example.com");
    }

    #[test]
    fn first_sync_is_capped_to_the_newest_messages() {
        let mut server = server();
        let mut sync = input(BTreeMap::new());
        sync.max_initial = 1;
        let events = run(&mut server, sync);
        let messages = new_messages(&events);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].uid, 2);
        assert_eq!(inbox_cursor(&events).highest_uid, 2);
    }

    #[test]
    fn unreachable_server_is_an_error() {
        let mut server = server();
        server.offline = true;
        let result = run_cycle(&mut server, input(BTreeMap::new()), now(), &mut |_| {});
        assert!(matches!(result, Err(SyncError::Connection(_))));
    }

    #[test]
    fn body_on_demand() {
        let mut server = server();
        let parsed = fetch_body(&mut server, "INBOX", 2).unwrap();
        assert_eq!(parsed.message.body_text.trim(), "Body of old");
        assert!(fetch_body(&mut server, "INBOX", 42).is_err());
    }
}
