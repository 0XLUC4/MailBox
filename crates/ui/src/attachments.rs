//! Attachments of received messages: open with the default app (from a temp copy), save,
//! save all, and inline (`cid:`) pictures for the reading pane.
//!
//! Files live where the sync bridge stores them ([`sync_bridge::attachment_path`]); a message
//! fetched headers-only has none yet, so the body is requested and the user asked to retry.
//! Dragging an attachment out to the desktop is not possible: GPUI has no outgoing OS drag.

use std::path::{Path, PathBuf};

use gpui::{App, Entity, PathPromptOptions};
use mailbox_core::{Attachment, Id, Message};

use crate::app_state::AppState;
use crate::sync_bridge;

/// Extensions that run code when opened: only "Save" is offered for these.
const RISKY: [&str; 18] = [
    "exe", "com", "bat", "cmd", "msi", "msp", "scr", "pif", "vbs", "vbe", "js", "jse", "wsf",
    "wsh", "ps1", "hta", "lnk", "reg",
];

/// Whether opening `name` directly could run code.
pub fn is_risky(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| RISKY.contains(&ext.to_ascii_lowercase().as_str()))
}

pub fn is_image(attachment: &Attachment) -> bool {
    attachment.mime.starts_with("image/")
}

/// The stored file, when it has been downloaded.
pub fn stored(message_id: Id, name: &str) -> Option<PathBuf> {
    let path = sync_bridge::attachment_path(message_id, name);
    path.is_file().then_some(path)
}

/// The stored file, or a toast + body download when it is missing.
fn require(state: &Entity<AppState>, message_id: Id, name: &str, cx: &mut App) -> Option<PathBuf> {
    let found = stored(message_id, name);
    if found.is_none() {
        sync_bridge::request_body(message_id, cx);
        info(
            state,
            "Downloading this attachment… try again in a moment",
            cx,
        );
    }
    found
}

fn info(state: &Entity<AppState>, message: impl Into<String>, cx: &mut App) {
    let message = message.into();
    state.update(cx, |state, cx| state.notify_info(message, cx));
}

fn error(state: &Entity<AppState>, message: impl Into<String>, cx: &mut App) {
    let message = message.into();
    state.update(cx, |state, cx| state.report_error(message, cx));
}

/// Opens an attachment with the system default app, from a temp copy (the store stays intact).
pub fn open(state: &Entity<AppState>, message_id: Id, name: &str, cx: &mut App) {
    if is_risky(name) {
        return info(
            state,
            "For your safety this file can't be opened directly: save it, then open it yourself",
            cx,
        );
    }
    let Some(source) = require(state, message_id, name, cx) else {
        return;
    };
    let Some(file_name) = source.file_name().map(ToOwned::to_owned) else {
        return;
    };
    let target = std::env::temp_dir()
        .join("mailbox-attachments")
        .join(message_id.to_string())
        .join(file_name);
    let copied = target
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|_| std::fs::copy(&source, &target));
    match copied {
        Ok(_) => cx.open_with_system(&target),
        Err(err) => error(state, format!("Cannot open {name}: {err}"), cx),
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// Asks where to save one attachment, then copies it there.
pub fn save(state: &Entity<AppState>, message_id: Id, name: &str, cx: &mut App) {
    let Some(source) = require(state, message_id, name, cx) else {
        return;
    };
    let suggested = source
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    let picked = cx.prompt_for_new_path(&home_dir(), suggested.as_deref());
    let state = state.clone();
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(target))) = picked.await else {
            return;
        };
        let result = std::fs::copy(&source, &target);
        cx.update(|cx| match result {
            Ok(_) => info(&state, format!("Saved {}", target.display()), cx),
            Err(err) => error(&state, format!("Save failed: {err}"), cx),
        });
    })
    .detach();
}

/// Asks for a folder, then copies every downloaded attachment of the message into it.
pub fn save_all(state: &Entity<AppState>, message_id: Id, cx: &mut App) {
    let message = state
        .read(cx)
        .store()
        .get::<Message>(message_id)
        .ok()
        .flatten();
    let Some(message) = message.filter(|message| !message.attachments.is_empty()) else {
        return info(state, "This message has no attachments", cx);
    };
    let sources: Vec<PathBuf> = message
        .attachments
        .iter()
        .filter_map(|attachment| stored(message_id, &attachment.name))
        .collect();
    if sources.len() < message.attachments.len() {
        sync_bridge::request_body(message_id, cx);
        return info(
            state,
            "Downloading the attachments… try again in a moment",
            cx,
        );
    }
    let picked = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some("Save attachments here".into()),
    });
    let state = state.clone();
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(dirs))) = picked.await else {
            return;
        };
        let Some(dir) = dirs.into_iter().next() else {
            return;
        };
        let result = copy_all(&sources, &dir);
        cx.update(|cx| match result {
            Ok(count) => info(
                &state,
                format!("Saved {count} attachment(s) to {}", dir.display()),
                cx,
            ),
            Err(err) => error(&state, format!("Save failed: {err}"), cx),
        });
    })
    .detach();
}

fn copy_all(sources: &[PathBuf], dir: &Path) -> std::io::Result<usize> {
    for source in sources {
        if let Some(name) = source.file_name() {
            std::fs::copy(source, unique_path(&dir.join(name)))?;
        }
    }
    Ok(sources.len())
}

/// `path`, or `name (2).ext`… when it already exists.
fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_owned();
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (2..1000)
        .map(|n| path.with_file_name(format!("{stem} ({n}){ext}")))
        .find(|candidate| !candidate.exists())
        .unwrap_or_else(|| path.to_owned())
}

/// Whether `src` is a stored attachment file (what [`resolve_inline_images`] produces). Anything
/// else that looks like a path (e.g. `\\host\share\x.png` in a hostile mail) is never loaded.
pub fn is_inline_file(src: &str) -> bool {
    let path = Path::new(src);
    let root = sync_bridge::data_dir().join("attachments");
    path.is_absolute()
        && path.starts_with(&root)
        && !path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        && path.is_file()
}

/// The attachment a `cid:` picture refers to. Parts are stored by file name only, so match the
/// Content-ID's local part against names (`image001.png@01D…` → `image001.png`), else take the
/// `nth` image attachment (pictures usually appear in attachment order).
pub fn cid_attachment<'a>(
    attachments: &'a [Attachment],
    cid: &str,
    nth: usize,
) -> Option<&'a Attachment> {
    let id = cid
        .trim()
        .trim_start_matches("cid:")
        .trim_matches(['<', '>']);
    let local = id.split('@').next().unwrap_or(id);
    attachments
        .iter()
        .find(|attachment| {
            attachment.name.eq_ignore_ascii_case(id) || attachment.name.eq_ignore_ascii_case(local)
        })
        .or_else(|| {
            attachments
                .iter()
                .filter(|attachment| is_image(attachment))
                .nth(nth)
        })
}

/// Rewrites `cid:` picture sources of a parsed body to the stored files (shown inline).
pub fn resolve_inline_images(message: &Message, blocks: &mut [crate::mail::html::Block]) {
    let mut nth = 0;
    resolve_in(message, blocks, &mut nth);
}

fn resolve_in(message: &Message, blocks: &mut [crate::mail::html::Block], nth: &mut usize) {
    use crate::mail::html::Block;
    for block in blocks {
        match block {
            Block::Image { src, .. } if src.to_ascii_lowercase().starts_with("cid:") => {
                let path = cid_attachment(&message.attachments, src, *nth)
                    .and_then(|attachment| stored(message.id, &attachment.name));
                *nth += 1;
                if let Some(path) = path {
                    *src = path.to_string_lossy().into_owned();
                }
            }
            Block::Quote(inner) => resolve_in(message, inner, nth),
            Block::Table(rows) => {
                for cell in rows.iter_mut().flatten() {
                    resolve_in(message, cell, nth);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{cid_attachment, is_risky};
    use mailbox_core::Attachment;

    fn attachment(name: &str, mime: &str) -> Attachment {
        Attachment {
            name: name.into(),
            mime: mime.into(),
            size: 1,
        }
    }

    #[test]
    fn matches_content_ids_to_attachments() {
        let list = [
            attachment("report.pdf", "application/pdf"),
            attachment("image001.png", "image/png"),
            attachment("logo.gif", "image/gif"),
        ];
        let found = |cid, nth| cid_attachment(&list, cid, nth).map(|a| a.name.as_str());
        assert_eq!(found("cid:image001.png@01D9.5A", 5), Some("image001.png"));
        assert_eq!(found("cid:<part1.abc@x>", 1), Some("logo.gif"));
        assert_eq!(found("cid:part1", 0), Some("image001.png"));
        assert_eq!(found("cid:part9", 2), None);
    }

    #[test]
    fn flags_executables() {
        assert!(is_risky("setup.EXE"));
        assert!(is_risky("run.ps1"));
        assert!(!is_risky("photo.jpg"));
        assert!(!is_risky("README"));
    }
}
