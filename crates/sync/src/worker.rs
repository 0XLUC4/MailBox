//! Background sync for one account: a worker thread executes [`Request`]s on its backend and
//! streams [`SyncEvent`]s back; [`Request::Send`] goes to a separate send thread (own
//! connection) so outgoing mail never waits behind a long sync cycle; an optional third
//! connection waits in IMAP IDLE on the inbox and reports [`SyncEvent::RemoteChanged`].
//! Blocking network IO never touches the UI thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use chrono::Utc;
use mailbox_core::Id;

use crate::backend::{MailBackend, RemoteFlags, Uid};
use crate::engine::{SyncEvent, SyncInput, fetch_body, run_cycle};
use crate::{SyncError, SyncResult};

/// Re-issue IDLE regularly (servers drop idle clients after ~30 min; it also bounds stop latency).
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// Wait before reconnecting the IDLE connection after an error.
const IDLE_RETRY: Duration = Duration::from_secs(60);

/// Creates a backend connection (called once for the worker and once for IDLE).
pub type BackendFactory = Arc<dyn Fn() -> SyncResult<Box<dyn MailBackend>> + Send + Sync>;

/// A message to submit, already rendered (see [`crate::formats::build_rfc5322`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingMessage {
    pub local_id: Id,
    pub from: String,
    pub recipients: Vec<String>,
    pub raw: Vec<u8>,
    /// Server Sent folder to append a copy to (`None`: the server files it itself).
    pub sent_path: Option<String>,
}

#[derive(Debug)]
pub enum Request {
    Sync(SyncInput),
    FetchBody {
        local_id: Id,
        path: String,
        uid: Uid,
    },
    Send(OutgoingMessage),
    /// Work Offline: no network until switched back.
    SetOffline(bool),
    Stop,
}

/// Owner side of an account's worker. Dropping it stops the threads.
pub struct WorkerHandle {
    requests: Sender<Request>,
    sends: Sender<OutgoingMessage>,
    events: Receiver<SyncEvent>,
    stop: Arc<AtomicBool>,
    offline: Arc<AtomicBool>,
}

impl WorkerHandle {
    /// Starts the worker; with `idle_path`, also an IDLE watcher on that folder.
    pub fn spawn(factory: BackendFactory, idle_path: Option<String>) -> SyncResult<Self> {
        let (requests, request_rx) = mpsc::channel();
        let (event_tx, events) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let offline = Arc::new(AtomicBool::new(false));

        let backend = factory()?;
        let worker_events = event_tx.clone();
        std::thread::Builder::new()
            .name("mailbox-sync".into())
            .spawn(move || run_worker(backend, request_rx, worker_events))?;

        let (sends, send_rx) = mpsc::channel();
        let backend = factory()?;
        let (send_events, send_offline) = (event_tx.clone(), offline.clone());
        std::thread::Builder::new()
            .name("mailbox-send".into())
            .spawn(move || run_sender(backend, send_rx, send_events, &send_offline))?;

        if let Some(path) = idle_path {
            let backend = factory()?;
            let (stop, offline) = (stop.clone(), offline.clone());
            std::thread::Builder::new()
                .name("mailbox-idle".into())
                .spawn(move || run_idle(backend, &path, event_tx, &stop, &offline))?;
        }
        Ok(Self {
            requests,
            sends,
            events,
            stop,
            offline,
        })
    }

    /// Queues a request; `false` if the worker is gone.
    pub fn request(&self, request: Request) -> bool {
        match request {
            Request::Send(message) => self.sends.send(message).is_ok(),
            request => self.requests.send(request).is_ok(),
        }
    }

    pub fn set_offline(&self, offline: bool) {
        self.offline.store(offline, Ordering::Relaxed);
        self.request(Request::SetOffline(offline));
    }

    /// Events received so far (non-blocking).
    pub fn drain(&self) -> Vec<SyncEvent> {
        self.events.try_iter().collect()
    }

    /// Waits for the next event (tests, CLI).
    pub fn recv_timeout(&self, timeout: Duration) -> Option<SyncEvent> {
        self.events.recv_timeout(timeout).ok()
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.requests.send(Request::Stop);
    }
}

fn run_worker(
    mut backend: Box<dyn MailBackend>,
    requests: Receiver<Request>,
    events: Sender<SyncEvent>,
) {
    let emit = |event: SyncEvent| {
        // The owner is gone when this fails; the loop ends on the closed request channel.
        let _ = events.send(event);
    };
    let mut offline = false;
    for request in requests {
        match request {
            Request::Stop => break,
            Request::SetOffline(value) => {
                offline = value;
                if offline {
                    backend.disconnect();
                }
            }
            Request::Sync(input) => {
                let error = if offline {
                    Some("working offline".to_owned())
                } else {
                    run_cycle(backend.as_mut(), input, Utc::now(), &mut |event| {
                        emit(event)
                    })
                    .err()
                    .map(|err| {
                        backend.disconnect();
                        err.to_string()
                    })
                };
                emit(SyncEvent::Finished { error });
            }
            Request::FetchBody {
                local_id,
                path,
                uid,
            } => {
                if offline {
                    continue;
                }
                match fetch_body(backend.as_mut(), &path, uid) {
                    Ok(parsed) => emit(SyncEvent::BodyFetched {
                        local_id,
                        parsed: Box::new(parsed),
                    }),
                    Err(err) => emit(SyncEvent::Progress(format!(
                        "could not download the message: {err}"
                    ))),
                }
            }
            // Routed to the send thread by `WorkerHandle::request`.
            Request::Send(_) => {}
        }
    }
    backend.disconnect();
}

/// Sends queued messages one by one, independently of sync cycles.
fn run_sender(
    mut backend: Box<dyn MailBackend>,
    messages: Receiver<OutgoingMessage>,
    events: Sender<SyncEvent>,
    offline: &AtomicBool,
) {
    for message in messages {
        let local_id = message.local_id;
        let result = if offline.load(Ordering::Relaxed) {
            Err(SyncError::Connection("working offline".into()))
        } else {
            send(backend.as_mut(), &message)
        };
        // Idle connections get dropped by servers: reconnect fresh for the next message.
        backend.disconnect();
        let reported = match result {
            Ok(warning) => {
                if let Some(warning) = warning {
                    let _ = events.send(SyncEvent::Progress(warning));
                }
                events.send(SyncEvent::Sent { local_id })
            }
            Err(err) => events.send(SyncEvent::SendFailed {
                local_id,
                error: err.to_string(),
            }),
        };
        // The owner is gone.
        if reported.is_err() {
            break;
        }
    }
}

/// SMTP submit, then a copy in Sent. Once SMTP accepted it the message counts as sent:
/// a failed copy is only a warning (never resend because of it).
fn send(backend: &mut dyn MailBackend, message: &OutgoingMessage) -> SyncResult<Option<String>> {
    backend.send(&message.from, &message.recipients, &message.raw)?;
    let Some(path) = &message.sent_path else {
        return Ok(None);
    };
    let seen = RemoteFlags {
        seen: true,
        flagged: false,
    };
    let saved = backend
        .connect()
        .and_then(|()| backend.append(path, &message.raw, seen));
    Ok(saved
        .err()
        .map(|err| format!("Sent, but no copy saved in {path}: {err}")))
}

fn run_idle(
    mut backend: Box<dyn MailBackend>,
    path: &str,
    events: Sender<SyncEvent>,
    stop: &AtomicBool,
    offline: &AtomicBool,
) {
    while !stop.load(Ordering::Relaxed) {
        if offline.load(Ordering::Relaxed) {
            backend.disconnect();
            pause(stop, Duration::from_secs(5));
            continue;
        }
        let changed = backend
            .connect()
            .and_then(|()| backend.wait_for_changes(path, IDLE_TIMEOUT));
        match changed {
            Ok(true) => {
                if events.send(SyncEvent::RemoteChanged).is_err() {
                    break;
                }
            }
            Ok(false) => {}
            Err(SyncError::Unsupported(_)) => break,
            Err(_) => {
                backend.disconnect();
                pause(stop, IDLE_RETRY);
            }
        }
    }
    backend.disconnect();
}

/// Sleeps up to `duration`, waking early when `stop` is set.
fn pause(stop: &AtomicBool, duration: Duration) {
    let step = Duration::from_millis(250);
    let mut waited = Duration::ZERO;
    while waited < duration && !stop.load(Ordering::Relaxed) {
        std::thread::sleep(step);
        waited += step;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{FolderStatus, MemoryBackend, RawMessage, RemoteFolder};
    use mailbox_core::FolderRole;
    use std::collections::BTreeMap;

    fn factory(server: MemoryBackend) -> BackendFactory {
        Arc::new(move || -> SyncResult<Box<dyn MailBackend>> { Ok(Box::new(server.clone())) })
    }

    fn server() -> MemoryBackend {
        let mut server = MemoryBackend::default();
        server.add_folder("INBOX", FolderRole::Inbox);
        server.add_folder("Sent", FolderRole::Sent);
        server.connected = true;
        server
            .deliver(
                "INBOX",
                b"Subject: hi\r\nMessage-ID: <hi@x>\r\n\r\nhello\r\n",
                RemoteFlags::default(),
            )
            .unwrap();
        server.connected = false;
        server
    }

    fn sync_input() -> SyncInput {
        SyncInput {
            push: vec![],
            cursors: BTreeMap::new(),
            offline_days: 30,
            max_initial: 100,
        }
    }

    /// Collects events until `Finished`, `Sent` or `SendFailed`.
    fn until_done(worker: &WorkerHandle) -> Vec<SyncEvent> {
        let mut events = Vec::new();
        while let Some(event) = worker.recv_timeout(Duration::from_secs(10)) {
            let done = matches!(
                event,
                SyncEvent::Finished { .. } | SyncEvent::Sent { .. } | SyncEvent::SendFailed { .. }
            );
            events.push(event);
            if done {
                break;
            }
        }
        events
    }

    #[test]
    fn sync_send_and_offline_round_trip() {
        let worker = WorkerHandle::spawn(factory(server()), None).unwrap();

        assert!(worker.request(Request::Sync(sync_input())));
        let events = until_done(&worker);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SyncEvent::NewMessages { .. }))
        );
        assert_eq!(events.last(), Some(&SyncEvent::Finished { error: None }));

        let outgoing = OutgoingMessage {
            local_id: 7,
            from: "me@x.com".into(),
            recipients: vec!["you@x.com".into()],
            raw: b"Subject: out\r\n\r\nbody\r\n".to_vec(),
            sent_path: Some("Sent".into()),
        };
        assert!(worker.request(Request::Send(outgoing.clone())));
        assert_eq!(
            until_done(&worker).last(),
            Some(&SyncEvent::Sent { local_id: 7 })
        );

        worker.set_offline(true);
        worker.request(Request::Sync(sync_input()));
        assert_eq!(
            until_done(&worker).last(),
            Some(&SyncEvent::Finished {
                error: Some("working offline".into())
            })
        );
        worker.request(Request::Send(outgoing));
        assert!(matches!(
            until_done(&worker).last(),
            Some(SyncEvent::SendFailed { local_id: 7, .. })
        ));
    }

    /// A server whose folder listing (the start of every sync) hangs until `release`.
    struct StuckSync {
        server: MemoryBackend,
        release: Arc<AtomicBool>,
    }

    impl MailBackend for StuckSync {
        fn connect(&mut self) -> SyncResult<()> {
            self.server.connect()
        }
        fn list_folders(&mut self) -> SyncResult<Vec<RemoteFolder>> {
            while !self.release.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(20));
            }
            self.server.list_folders()
        }
        fn status(&mut self, path: &str) -> SyncResult<FolderStatus> {
            self.server.status(path)
        }
        fn fetch_flags(&mut self, path: &str) -> SyncResult<Vec<(Uid, RemoteFlags)>> {
            self.server.fetch_flags(path)
        }
        fn fetch_headers(&mut self, path: &str, uids: &[Uid]) -> SyncResult<Vec<RawMessage>> {
            self.server.fetch_headers(path, uids)
        }
        fn fetch_full(&mut self, path: &str, uids: &[Uid]) -> SyncResult<Vec<RawMessage>> {
            self.server.fetch_full(path, uids)
        }
        fn set_flags(&mut self, path: &str, uids: &[Uid], flags: RemoteFlags) -> SyncResult<()> {
            self.server.set_flags(path, uids, flags)
        }
        fn move_messages(&mut self, path: &str, uids: &[Uid], target: &str) -> SyncResult<()> {
            self.server.move_messages(path, uids, target)
        }
        fn delete_messages(&mut self, path: &str, uids: &[Uid]) -> SyncResult<()> {
            self.server.delete_messages(path, uids)
        }
        fn append(&mut self, path: &str, raw: &[u8], flags: RemoteFlags) -> SyncResult<()> {
            self.server.append(path, raw, flags)
        }
        fn send(&mut self, from: &str, recipients: &[String], raw: &[u8]) -> SyncResult<()> {
            self.server.send(from, recipients, raw)
        }
    }

    /// Regression: a message queued during a long sync (first sync of a big mailbox) sat in
    /// the Outbox until that sync ended, i.e. never when the app was closed before.
    #[test]
    fn send_does_not_wait_for_a_running_sync() {
        let release = Arc::new(AtomicBool::new(false));
        let stuck = release.clone();
        let factory: BackendFactory = Arc::new(move || -> SyncResult<Box<dyn MailBackend>> {
            Ok(Box::new(StuckSync {
                server: server(),
                release: stuck.clone(),
            }))
        });
        let worker = WorkerHandle::spawn(factory, None).unwrap();
        assert!(worker.request(Request::Sync(sync_input())));
        assert!(worker.request(Request::Send(OutgoingMessage {
            local_id: 9,
            from: "me@x.com".into(),
            recipients: vec!["you@x.com".into()],
            raw: b"Subject: out\r\n\r\nbody\r\n".to_vec(),
            sent_path: Some("Sent".into()),
        })));
        let events = until_done(&worker);
        release.store(true, Ordering::Relaxed);
        assert_eq!(events.last(), Some(&SyncEvent::Sent { local_id: 9 }));
    }

    #[test]
    fn unreachable_server_finishes_with_error() {
        let mut down = server();
        down.offline = true;
        let worker = WorkerHandle::spawn(factory(down), None).unwrap();
        worker.request(Request::Sync(sync_input()));
        let events = until_done(&worker);
        assert!(matches!(
            events.last(),
            Some(SyncEvent::Finished { error: Some(_) })
        ));
    }

    #[test]
    fn send_reports_a_failed_sent_copy_as_warning() {
        let mut backend = server();
        let message = OutgoingMessage {
            local_id: 1,
            from: "me@x.com".into(),
            recipients: vec!["you@x.com".into()],
            raw: b"Subject: out\r\n\r\nbody\r\n".to_vec(),
            sent_path: Some("Missing".into()),
        };
        let warning = send(&mut backend, &message).unwrap();
        assert!(warning.unwrap().contains("Missing"));
        assert_eq!(backend.sent.len(), 1);
    }
}
