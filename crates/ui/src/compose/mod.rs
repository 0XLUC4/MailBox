//! Compose: new mail, reply, reply all, forward, `mailto:` and draft editing.
//!
//! [`open_compose`] docks the editor over the right side of the main window (several at once, as
//! tabs); "Pop out" moves one into its own window. Drafts autosave (untracked) into Drafts;
//! Send waits 5 s with an Undo toast, then `Command::QueueSend` moves the draft into the
//! account's Outbox folder, which the sync engine sends. Body markup and recipients logic
//! live in `mailbox_core::compose` (tested there).

mod dock;
// Page and commands only (no web view): unused where the formatted editor is unavailable.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
mod editor;
mod recipients;
mod send;
mod view;

use std::borrow::Borrow;

use chrono::Utc;
use gpui::*;
use mailbox_core::compose::{self, Draft, DraftMeta, markup};
use mailbox_core::{Address, Error, FolderRole, Id, Importance, Message, MessageFlags};

use crate::app_state::AppState;

pub use dock::ComposeDock;
pub use view::ComposeView;

gpui::actions!(
    compose,
    [
        SendMessage,
        ToggleBold,
        ToggleItalic,
        ToggleUnderline,
        InsertLink
    ]
);

/// What to open the editor with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposeRequest {
    New,
    /// New message to these people (contact cards, People module).
    To(Vec<Address>),
    Reply(Id),
    ReplyAll(Id),
    Forward(Id),
    /// A `mailto:` URL (to, cc, bcc, subject, body).
    Mailto(String),
    /// Re-open a saved draft (message id).
    EditDraft(Id),
}

/// Process-wide compose state: the dock of the main window.
#[derive(Default)]
struct ComposeGlobal {
    dock: Option<Entity<ComposeDock>>,
    main_window: Option<AnyWindowHandle>,
    keys_bound: bool,
}

impl Global for ComposeGlobal {}

/// Opens a compose editor docked in `window` (the main window).
///
/// `state` may be passed as `Entity<AppState>` or `&Entity<AppState>`. Errors (missing message,
/// no account) are shown as a toast.
pub fn open_compose(
    state: impl Borrow<Entity<AppState>>,
    request: ComposeRequest,
    window: &mut Window,
    cx: &mut App,
) {
    let state = state.borrow().clone();
    bind_keys(cx);
    if let ComposeRequest::EditDraft(id) = request
        && dock::activate_draft(id, cx)
    {
        return;
    }
    let draft = match seed(state.read(cx), &request) {
        Ok(draft) => draft,
        Err(err) => {
            state.update(cx, |state, cx| state.report_error(err, cx));
            return;
        }
    };
    let view = cx.new(|cx| ComposeView::new(state.clone(), draft, view::Host::Docked, window, cx));
    dock::add(view, window, cx);
    // The shell observes the state: re-render it so the dock appears.
    state.update(cx, |_, cx| cx.notify());
}

/// Is message `id` a draft compose can re-open?
pub fn is_draft(state: &AppState, id: Id) -> bool {
    compose::load_draft(state.store(), id).is_ok_and(|draft| draft.is_some())
}

/// Re-opens draft `id` in compose (brings it to the front if already open).
pub fn edit_draft(state: &Entity<AppState>, id: Id, window: &mut Window, cx: &mut App) {
    open_compose(state, ComposeRequest::EditDraft(id), window, cx);
}

/// Shell-level handlers for New mail (ribbon, Ctrl+N), Reply (Ctrl+R), Reply all
/// (Ctrl+Shift+R) and Forward (Ctrl+Shift+F) on the selected message. A module that handles
/// these closer to the focus wins.
pub fn install<E: InteractiveElement>(element: E, state: &Entity<AppState>) -> E {
    let new = state.clone();
    let element = element.on_action(move |_: &crate::actions::NewMail, window, cx| {
        open_compose(&new, ComposeRequest::New, window, cx)
    });
    let element = on_selected::<_, crate::actions::Reply>(element, state, ComposeRequest::Reply);
    let element =
        on_selected::<_, crate::actions::ReplyAll>(element, state, ComposeRequest::ReplyAll);
    on_selected::<_, crate::actions::Forward>(element, state, ComposeRequest::Forward)
}

fn on_selected<E: InteractiveElement, A: Action>(
    element: E,
    state: &Entity<AppState>,
    request: fn(Id) -> ComposeRequest,
) -> E {
    let state = state.clone();
    element.on_action(
        move |_: &A, window, cx| match state.read(cx).selected_message() {
            Some(id) => open_compose(&state, request(id), window, cx),
            None => state.update(cx, |state, cx| {
                state.notify_info(crate::settings::tr(cx, "Select a message first"), cx)
            }),
        },
    )
}

/// Queues saved draft `message_id` for sending now (Outbox, then Send/Receive). Used by
/// "send later", automatic replies and read receipts.
pub fn queue_draft(state: &Entity<AppState>, message_id: Id, cx: &mut App) {
    send::queue(state, message_id, cx);
}

/// The compose dock to overlay on the main window content, when something is being written.
pub fn dock(cx: &App) -> Option<Entity<ComposeDock>> {
    cx.try_global::<ComposeGlobal>()
        .and_then(|global| global.dock.clone())
        .filter(|dock| !dock.read(cx).is_empty())
}

fn bind_keys(cx: &mut App) {
    if cx.default_global::<ComposeGlobal>().keys_bound {
        return;
    }
    cx.default_global::<ComposeGlobal>().keys_bound = true;
    // KeyBinding needs `&'static str`: pick the platform modifier at compile time.
    macro_rules! primary {
        ($key:literal) => {
            if cfg!(target_os = "macos") {
                concat!("cmd-", $key)
            } else {
                concat!("ctrl-", $key)
            }
        };
    }
    let context = Some("Compose");
    cx.bind_keys([
        KeyBinding::new(primary!("enter"), SendMessage, context),
        KeyBinding::new(primary!("b"), ToggleBold, context),
        KeyBinding::new(primary!("i"), ToggleItalic, context),
        KeyBinding::new(primary!("u"), ToggleUnderline, context),
        KeyBinding::new(primary!("k"), InsertLink, context),
    ]);
}

/// Builds the initial draft for a request (recipients, subject, quote, signature).
fn seed(state: &AppState, request: &ComposeRequest) -> mailbox_core::Result<Draft> {
    let store = state.store();
    if let ComposeRequest::EditDraft(id) = request {
        return compose::load_draft(store, *id)?.ok_or(Error::NotFound {
            kind: "draft",
            id: *id,
        });
    }
    let original = match request {
        ComposeRequest::Reply(id) | ComposeRequest::ReplyAll(id) | ComposeRequest::Forward(id) => {
            Some(store.require::<Message>(*id)?)
        }
        _ => None,
    };
    let accounts = store.list_accounts()?;
    let current_account = state
        .selected_folder()
        .and_then(|id| store.get::<mailbox_core::Folder>(id).ok().flatten())
        .map(|folder| folder.account_id);
    let account = original
        .as_ref()
        .map(|message| message.account_id)
        .or(current_account)
        .and_then(|id| accounts.iter().find(|account| account.id == id))
        .or(accounts.first())
        .ok_or_else(|| Error::Invalid("Add an account before writing mail".into()))?;
    let drafts = store.folder_by_role(account.id, FolderRole::Drafts)?;
    let signature = compose::signature(store, account.id)?.unwrap_or_default();
    let my_emails: Vec<String> = accounts.iter().map(|a| a.email.clone()).collect();
    let now = Utc::now();
    let key = compose::new_message_id(&account.email, now);

    let mut message = Message {
        id: 0,
        account_id: account.id,
        folder_id: drafts.map_or(0, |folder| folder.id),
        thread_id: key.trim_matches(['<', '>']).to_owned(),
        from: Address::new(account.name.clone(), account.email.clone()),
        to: vec![],
        cc: vec![],
        subject: String::new(),
        date: now,
        preview: String::new(),
        body_text: String::new(),
        body_html: None,
        flags: MessageFlags {
            read: true,
            ..MessageFlags::default()
        },
        importance: Importance::Normal,
        categories: vec![],
        attachments: vec![],
    };
    let mut meta = DraftMeta {
        id: 0,
        message_id: 0,
        key,
        bcc: vec![],
        read_receipt: false,
        reply_to: None,
        in_reply_to: None,
        references: vec![],
        attachments: vec![],
        markup: String::new(),
    };
    // Body = empty first line (caret), then the signature, then the quoted original.
    let body = |parts: &[&str]| {
        std::iter::once("")
            .chain(parts.iter().copied().filter(|part| !part.trim().is_empty()))
            .collect::<Vec<_>>()
            .join("\n\n")
    };

    match (request, &original) {
        (ComposeRequest::To(addresses), _) => {
            message.to = addresses.clone();
            meta.markup = body(&[&signature.new_message]);
        }
        (ComposeRequest::Mailto(url), _) => {
            let mailto = compose::parse_mailto(url);
            message.to = mailto.to;
            message.cc = mailto.cc;
            meta.bcc = mailto.bcc;
            message.subject = mailto.subject;
            let text = markup::escape_markup(&mailto.body);
            meta.markup = if text.is_empty() {
                body(&[&signature.new_message])
            } else {
                format!("{text}\n\n{}", signature.new_message)
                    .trim_end()
                    .to_owned()
            };
        }
        (ComposeRequest::Reply(_) | ComposeRequest::ReplyAll(_), Some(original)) => {
            let reply_all = matches!(request, ComposeRequest::ReplyAll(_));
            let (to, cc) = compose::reply_recipients(original, &my_emails, reply_all);
            message.to = to;
            message.cc = cc;
            message.subject = compose::reply_subject(&original.subject);
            message.thread_id = original.thread_id.clone();
            meta.reply_to = Some(original.id);
            meta.in_reply_to = compose::parent_message_id(original);
            meta.references = meta.in_reply_to.iter().cloned().collect();
            meta.markup = body(&[&signature.reply, &compose::quote_for_reply(original)]);
        }
        (ComposeRequest::Forward(_), Some(original)) => {
            message.subject = compose::forward_subject(&original.subject);
            meta.reply_to = Some(original.id);
            meta.markup = body(&[&signature.reply, &compose::quote_for_forward(original)]);
            // Forwards carry the original's files (those downloaded; headers-only mail has none).
            meta.attachments = original
                .attachments
                .iter()
                .filter_map(|attachment| {
                    let path = crate::sync_bridge::attachment_path(original.id, &attachment.name);
                    let size = std::fs::metadata(&path).ok()?.len();
                    Some(compose::DraftAttachment {
                        path: path.to_string_lossy().into_owned(),
                        name: attachment.name.clone(),
                        mime: attachment.mime.clone(),
                        size,
                    })
                })
                .collect();
        }
        _ => meta.markup = body(&[&signature.new_message]),
    }
    Ok(Draft { message, meta })
}
