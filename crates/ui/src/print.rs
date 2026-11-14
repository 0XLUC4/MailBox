//! Message and mailbox files: print (HTML page opened in the default browser, which shows its
//! print dialog), save as `.eml`, export every folder of an account as `.mbox`, import
//! `.mbox`/`.eml` files into the selected folder. Encoding and I/O run in the sync bridge.

use std::path::PathBuf;

use gpui::{App, Entity, PathPromptOptions};
use mailbox_core::{Folder, Id, Message, export};

use crate::app_state::AppState;
use crate::mail::html;
use crate::sync_bridge;

fn report(state: &Entity<AppState>, message: impl Into<String>, cx: &mut App) {
    let message = message.into();
    state.update(cx, |state, cx| state.report_error(message, cx));
}

fn info(state: &Entity<AppState>, message: impl Into<String>, cx: &mut App) {
    let message = message.into();
    state.update(cx, |state, cx| state.notify_info(message, cx));
}

fn message(state: &Entity<AppState>, id: Id, cx: &App) -> Option<Message> {
    state.read(cx).store().get::<Message>(id).ok().flatten()
}

fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// Plain text of a message (HTML bodies are flattened).
fn body_text(message: &Message) -> String {
    match &message.body_html {
        Some(body) if message.body_text.trim().is_empty() => html::to_text(&html::parse(body)),
        _ => message.body_text.clone(),
    }
}

/// Writes a printable page to the temp folder and opens it (the page calls `window.print()`).
pub fn print_message(state: &Entity<AppState>, id: Id, cx: &mut App) {
    let Some(message) = message(state, id, cx) else {
        return;
    };
    let page = export::print_html(&message, &body_text(&message));
    let path = std::env::temp_dir()
        .join("mailbox-print")
        .join(format!("message-{id}.html"));
    let written = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|_| std::fs::write(&path, page));
    match written {
        Ok(()) => cx.open_with_system(&path),
        Err(err) => report(state, format!("Cannot prepare printing: {err}"), cx),
    }
}

/// Asks for a file name, then saves the message as `.eml`.
pub fn save_as_eml(state: &Entity<AppState>, id: Id, cx: &mut App) {
    let Some(message) = message(state, id, cx) else {
        return;
    };
    let name = export::file_name(&message.subject, "eml");
    let picked = cx.prompt_for_new_path(&home_dir(), Some(&name));
    cx.spawn(async move |cx| {
        if let Ok(Ok(Some(path))) = picked.await {
            cx.update(|cx| sync_bridge::export_message(id, path, cx));
        }
    })
    .detach();
}

/// Asks for a folder, then writes one `.mbox` per folder of the current account.
pub fn export_mailbox(state: &Entity<AppState>, cx: &mut App) {
    let Some(account_id) = crate::actions::current_account(state, cx) else {
        return info(state, "Add an account first", cx);
    };
    let folders = state
        .read(cx)
        .store()
        .list_folders(account_id)
        .unwrap_or_default();
    let picked = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some("Export the mailbox here".into()),
    });
    let state = state.clone();
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(dirs))) = picked.await else {
            return;
        };
        let Some(dir) = dirs.into_iter().next() else {
            return;
        };
        cx.update(|cx| {
            for folder in &folders {
                let path = dir.join(export::file_name(&folder_path(folder, &folders), "mbox"));
                sync_bridge::export_folder(folder.id, path, cx);
            }
            info(
                &state,
                format!("Exporting {} folders to {}", folders.len(), dir.display()),
                cx,
            );
        });
    })
    .detach();
}

/// `Parent - Child` so nested folders with the same name do not overwrite each other.
fn folder_path(folder: &Folder, all: &[Folder]) -> String {
    let mut parts = vec![folder.name.clone()];
    let mut parent = folder.parent_id;
    while let Some(id) = parent {
        let Some(found) = all.iter().find(|candidate| candidate.id == id) else {
            break;
        };
        parts.push(found.name.clone());
        parent = found.parent_id;
        if parts.len() > 32 {
            break;
        }
    }
    parts.reverse();
    parts.join(" - ")
}

/// Asks for `.mbox` / `.eml` files, then imports them into the selected folder.
pub fn import_mailbox(state: &Entity<AppState>, cx: &mut App) {
    let Some(folder_id) = state.read(cx).selected_folder() else {
        return info(state, "Select the folder to import into", cx);
    };
    let picked = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: true,
        prompt: Some("Import .mbox or .eml files".into()),
    });
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(paths))) = picked.await else {
            return;
        };
        cx.update(|cx| {
            for path in paths {
                sync_bridge::import_file(path, folder_id, cx);
            }
        });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::folder_path;
    use mailbox_core::{Folder, FolderRole};

    fn folder(id: i64, parent_id: Option<i64>, name: &str) -> Folder {
        Folder {
            id,
            account_id: 1,
            parent_id,
            name: name.into(),
            role: FolderRole::Custom,
            position: 0,
        }
    }

    #[test]
    fn nested_folders_get_their_path() {
        let all = vec![folder(1, None, "Projects"), folder(2, Some(1), "Atlas")];
        assert_eq!(folder_path(&all[1], &all), "Projects - Atlas");
        assert_eq!(folder_path(&all[0], &all), "Projects");
    }
}
