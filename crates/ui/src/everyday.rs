//! Actions for the everyday mail features (catalogued in `actions::ACTIONS`, shown in the ribbon
//! and the command palette) and their shell-level handlers.

use gpui::{Action, App, Entity, InteractiveElement, Window};
use mailbox_core::Id;

use crate::app_state::AppState;

gpui::actions!(
    mailbox,
    [
        SnoozeMessage,
        MuteConversation,
        PrintMessage,
        SaveMessageAs,
        SaveAllAttachments,
        ExportMailbox,
        ImportMailbox,
        OpenAutoReply,
        OpenReadReceipts,
        SetDefaultMailApp,
    ]
);

/// Catalog ids that need a selected message.
pub const NEEDS_MESSAGE: [&str; 5] = ["snooze", "mute", "print", "save-eml", "save-attachments"];

type Handler = fn(&Entity<AppState>, &mut Window, &mut App);

fn on<E: InteractiveElement, A: Action>(
    element: E,
    state: &Entity<AppState>,
    handler: Handler,
) -> E {
    let state = state.clone();
    element.on_action(move |_: &A, window, cx| handler(&state, window, cx))
}

/// Runs `run` with the selected message id, or explains there is none.
fn with_message(
    state: &Entity<AppState>,
    cx: &mut App,
    run: impl FnOnce(&Entity<AppState>, Id, &mut App),
) {
    match state.read(cx).selected_message() {
        Some(id) => run(state, id, cx),
        None => state.update(cx, |state, cx| {
            state.notify_info("Select a message first", cx)
        }),
    }
}

/// Attaches the handlers to the shell root element.
pub fn install<E: InteractiveElement>(element: E, state: &Entity<AppState>) -> E {
    let element = on::<_, SnoozeMessage>(element, state, |state, window, cx| {
        crate::later::open_snooze_picker(state.clone(), window, cx)
    });
    let element = on::<_, MuteConversation>(element, state, |state, _, cx| {
        crate::later::toggle_mute_selected(state, cx)
    });
    let element = on::<_, PrintMessage>(element, state, |state, _, cx| {
        with_message(state, cx, crate::print::print_message)
    });
    let element = on::<_, SaveMessageAs>(element, state, |state, _, cx| {
        with_message(state, cx, crate::print::save_as_eml)
    });
    let element = on::<_, SaveAllAttachments>(element, state, |state, _, cx| {
        with_message(state, cx, crate::attachments::save_all)
    });
    let element = on::<_, ExportMailbox>(element, state, |state, _, cx| {
        crate::print::export_mailbox(state, cx)
    });
    let element = on::<_, ImportMailbox>(element, state, |state, _, cx| {
        crate::print::import_mailbox(state, cx)
    });
    let element = on::<_, OpenAutoReply>(element, state, |state, window, cx| {
        crate::auto_reply::open(state.clone(), window, cx)
    });
    let element = on::<_, OpenReadReceipts>(element, state, |state, window, cx| {
        crate::receipts::open_policy(state.clone(), window, cx)
    });
    on::<_, SetDefaultMailApp>(element, state, |state, _, cx| {
        crate::system_integration::set_default_mail_app(state, cx)
    })
}
