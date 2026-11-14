//! Writes fetched server messages into the local store without ever duplicating one.
//!
//! A server message (account + folder path + UIDVALIDITY + UID) maps to at most one local
//! message (UNIQUE index, see [`mailbox_core::remote`]). Before inserting, the copy already
//! stored is looked up: a message moved or sent locally (pending adoption), the row holding
//! that server identity (an earlier run whose sync state was lost, or a sync running twice),
//! or a row stored before identities were recorded (same Message-ID or sender/subject/date).
//! That copy is updated in place instead (upsert), keeping the user's state.

use mailbox_core::remote::RemoteId;
use mailbox_core::{Address, Id, Message, Store};

use crate::engine::FetchedMessage;
use crate::mime::AttachmentData;
use crate::state::{Link, SyncState};

/// Outcome of [`store_fetched`].
#[derive(Debug)]
pub struct Stored {
    pub local_id: Id,
    /// `false`: an existing local copy was updated.
    pub created: bool,
    pub unread: bool,
    /// To write to disk (the store keeps names only).
    pub attachments: Vec<AttachmentData>,
    /// The sender asks for a read receipt.
    pub receipt_to: Option<Address>,
}

/// Saves a message fetched from server folder `path` (or updates its local copy) and links it
/// in `state`. The message and its server identity are written in one transaction.
/// `Ok(None)`: the folder has no local mirror.
pub fn store_fetched(
    store: &Store,
    state: &mut SyncState,
    account_id: Id,
    path: &str,
    uid_validity: u32,
    fetched: FetchedMessage,
) -> mailbox_core::Result<Option<Stored>> {
    let Some(folder_id) = state.local_folder(path) else {
        return Ok(None);
    };
    let FetchedMessage {
        uid,
        flags,
        mut has_body,
        mut parsed,
    } = fetched;
    parsed.assign_thread(&mut state.threads);
    let attachments = std::mem::take(&mut parsed.attachments);
    let receipt_to = parsed.receipt_to.take();
    let message_id = parsed.message_id.clone();
    let mut message = parsed.message;
    message.account_id = account_id;
    message.folder_id = folder_id;
    let remote = RemoteId {
        path: path.to_owned(),
        uid_validity,
        uid,
    };

    let existing = local_copy(store, state, &message, &message_id, &remote)?;
    let created = existing.is_none();
    if let Some(existing) = existing {
        message.id = existing.id;
        // A local move not pushed yet stays where the user put it.
        message.folder_id = existing.folder_id;
        message.categories = existing.categories;
        message.flags.read |= existing.flags.read;
        message.flags.flagged |= existing.flags.flagged;
        message.flags.pinned = existing.flags.pinned;
        if !has_body {
            has_body = !existing.body_text.is_empty() || existing.body_html.is_some();
            message.body_text = existing.body_text;
            message.body_html = existing.body_html;
            message.preview = existing.preview;
            message.attachments = existing.attachments;
        }
    }
    let local_id = store.transaction(|store| {
        let id = store.save(&mut message)?;
        store.set_message_remote(id, Some(&remote), &message_id)?;
        Ok(id)
    })?;
    state.link(
        path,
        uid,
        Link {
            local_id,
            flags,
            message_id,
            has_body,
        },
    );
    Ok(Some(Stored {
        local_id,
        created,
        unread: !message.flags.read,
        attachments,
        receipt_to,
    }))
}

/// The local message this server message already is, if any (see the module docs).
fn local_copy(
    store: &Store,
    state: &mut SyncState,
    message: &Message,
    message_id: &str,
    remote: &RemoteId,
) -> mailbox_core::Result<Option<Message>> {
    if let Some(id) = state.take_adoption(message_id)
        && let Some(existing) = store.get::<Message>(id)?
    {
        return Ok(Some(existing));
    }
    let id = match store.message_by_remote(message.account_id, remote)? {
        Some(id) => Some(id),
        // A twin still linked to another UID is a second server copy: keep both.
        None => store
            .unlinked_twin(message, message_id)?
            .filter(|id| state.find(*id).is_none()),
    };
    match id {
        Some(id) => store.get::<Message>(id),
        None => Ok(None),
    }
}
