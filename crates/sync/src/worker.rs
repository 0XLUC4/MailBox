//! Background sync for one account: a worker thread executes [`Request`]s on its backend and
//! streams [`SyncEvent`]s back; an optional second connection waits in IMAP IDLE on the inbox
//! and reports [`SyncEvent::RemoteChanged`]. Blocking network IO never touches the UI thread.

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

        if let Some(path) = idle_path {
            let backend = factory()?;
            let (stop, offline) = (stop.clone(), offline.clone());
            std::thread::Builder::new()
                .name("mailbox-idle".into())
                .spawn(move || run_idle(backend, &path, event_tx, &stop, &offline))?;
        }
        Ok(Self {
            requests,
            events,
            stop,
            offline,
        })
    }

    /// Queues a request; `false` if the worker is gone.
    pub fn request(&self, request: Request) -> bool {
        self.requests.send(request).is_ok()
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
            Request::Send(message) => {
                let local_id = message.local_id;
                let result = if offline {
                    Err(SyncError::Connection("working offline".into()))
                } else {
                    send(backend.as_mut(), &message)
                };
                match result {
                    Ok(warning) => {
                        if let Some(warning) = warning {
                            emit(SyncEvent::Progress(warning));
                        }
                        emit(SyncEvent::Sent { local_id });
                    }
                    Err(err) => emit(SyncEvent::SendFailed {
                        local_id,
                        error: err.to_string(),
                    }),
                }
            }
        }
    }
    backend.disconnect();
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
    use crate::backend::MemoryBackend;
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
