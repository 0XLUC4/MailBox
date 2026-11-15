//! The compose editor: header, address fields, formatting toolbar, body, attachments, footer.

use std::ops::Range;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use gpui::component::WindowExt as _;
use gpui::component::input::{Input, InputEvent, InputState, Search, Textarea, TextareaState};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::compose::markup::{self, Edit, LineFormat};
use mailbox_core::compose::{self, Draft, DraftAttachment, Signature};
use mailbox_core::{Account, Address, Attachment, Command, FolderRole, Id, Importance};

use super::editor as editor_js;
use super::recipients::{RecipientEvent, RecipientField};
use super::{InsertLink, SendMessage, ToggleBold, ToggleItalic, ToggleUnderline, dock, send};
use crate::app_state::AppState;
use crate::components::{Avatar, Button, Icon, IconButton, IconName, Toolbar, toolbar_separator};
#[cfg(any(windows, target_os = "macos"))]
use crate::mail::webview::{MailWebView, WebEvent};
use crate::settings::tr;
use crate::theme::{
    ICON_SIZE_SM, RADIUS_FULL, RADIUS_LG, RADIUS_MD, SPACE_1, SPACE_1_5, SPACE_2, SPACE_3, SPACE_4,
    TEXT_LG, TEXT_MD, TEXT_SM, TEXT_XS, Theme, hex, theme,
};

const AUTOSAVE_DELAY: Duration = Duration::from_millis(1500);
/// Above this total, warn that many servers will refuse the message.
const LARGE_ATTACHMENTS: u64 = 25 * 1024 * 1024;

/// Text colours offered by the toolbar (content colours, not UI tokens).
const TEXT_COLORS: [u32; 10] = [
    0x111827, 0x6b7280, 0xdc2626, 0xea580c, 0xca8a04, 0x16a34a, 0x0d9488, 0x2563eb, 0x7c3aed,
    0xdb2777,
];
const HIGHLIGHTS: [u32; 6] = [0xfef08a, 0xbbf7d0, 0xbfdbfe, 0xfbcfe8, 0xfed7aa, 0xe5e7eb];
const SIZES: [(&str, u8); 3] = [("Small", 12), ("Large", 18), ("Huge", 24)];
const SYMBOLS: [&str; 48] = [
    "😀", "😂", "😊", "😍", "🤔", "😅", "😢", "😮", "👍", "👎", "👏", "🙏", "🎉", "🔥", "✅", "❌",
    "⭐", "❤️", "💡", "📎", "📅", "⏰", "🚀", "☕", "©", "®", "™", "€", "£", "¥", "§", "¶", "°",
    "±", "×", "÷", "≈", "≠", "≤", "≥", "→", "←", "↑", "↓", "…", "•", "—", "«",
];

/// Where the editor lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Host {
    /// In the main window's compose dock.
    Docked,
    /// Popped out in its own window.
    Window,
}

/// Inline bar under the formatting toolbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Panel {
    Link,
    TextColor,
    Highlight,
    Size,
    Symbols,
    Signature,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum SaveStatus {
    /// Nothing changed since opening.
    Clean,
    Unsaved,
    Saved(DateTime<Local>),
    Failed,
}

pub struct ComposeView {
    state: Entity<AppState>,
    host: Host,
    /// Everything not held by the inputs below (ids, account, importance, attachments...).
    draft: Draft,
    to: Entity<RecipientField>,
    cc: Entity<RecipientField>,
    bcc: Entity<RecipientField>,
    show_cc: bool,
    show_bcc: bool,
    subject: Entity<InputState>,
    body: Entity<TextareaState>,
    link_input: Entity<InputState>,
    signature_new: Entity<TextareaState>,
    signature_reply: Entity<TextareaState>,
    panel: Option<Panel>,
    from_open: bool,
    /// The formatted body editor (Windows, macOS) once created; the text area until then and
    /// where it is unavailable.
    #[cfg(any(windows, target_os = "macos"))]
    editor: Option<std::rc::Rc<MailWebView>>,
    #[cfg(any(windows, target_os = "macos"))]
    editor_failed: bool,
    status: SaveStatus,
    autosave: Option<Task<()>>,
    /// Detached from its host (closed, sent or discarded): never save again.
    closed: bool,
    _subscriptions: Vec<Subscription>,
}

impl ComposeView {
    pub(crate) fn new(
        state: Entity<AppState>,
        draft: Draft,
        host: Host,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let message = &draft.message;
        let to =
            cx.new(|cx| RecipientField::new(state.clone(), message.to.clone(), "To", window, cx));
        let cc =
            cx.new(|cx| RecipientField::new(state.clone(), message.cc.clone(), "Cc", window, cx));
        let bcc = cx.new(|cx| {
            RecipientField::new(state.clone(), draft.meta.bcc.clone(), "Bcc", window, cx)
        });
        let subject = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Subject")
                .default_value(message.subject.clone())
        });
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Write your message…  **bold**  *italic*  - list  > quote")
                .searchable(true)
                .default_value(draft.meta.markup.clone())
        });
        let link_input = cx.new(|cx| InputState::new(window, cx).placeholder("https://"));
        let signature_new = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 6)
                .placeholder("Signature for new messages")
        });
        let signature_reply = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 6)
                .placeholder("Signature for replies and forwards")
        });

        let mut subscriptions = vec![
            cx.subscribe_in(
                &subject,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => this.mark_dirty(cx),
                    InputEvent::PressEnter {
                        secondary: true, ..
                    } => this.send(window, cx),
                    InputEvent::PressEnter { .. } => this.focus_body(window, cx),
                    _ => {}
                },
            ),
            cx.subscribe_in(
                &body,
                window,
                |this, body, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => this.mark_dirty(cx),
                    InputEvent::PressEnter {
                        secondary: true, ..
                    } => {
                        // The textarea already inserted a line break for Ctrl+Enter.
                        body.update(cx, |body, cx| {
                            let caret = body.selected_range().start;
                            let text = body.value();
                            if text.get(..caret).is_some_and(|t| t.ends_with('\n')) {
                                body.set_selected_range(caret - 1..caret, cx);
                                body.replace("", window, cx);
                            }
                        });
                        this.send(window, cx)
                    }
                    InputEvent::PressEnter { shift: false, .. } => this.continue_list(window, cx),
                    _ => {}
                },
            ),
            cx.subscribe_in(
                &link_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.insert_link(window, cx);
                    }
                },
            ),
        ];
        for field in [&to, &cc, &bcc] {
            subscriptions
                .push(cx.subscribe(field, |this, _, _: &RecipientEvent, cx| this.mark_dirty(cx)));
        }

        // Hidden while the compose view is not drawn (another module, a closed dock).
        #[cfg(any(windows, target_os = "macos"))]
        subscriptions.push(
            cx.observe_in(&state, window, |this: &mut Self, _, window, _| {
                if let Some(editor) = &this.editor {
                    crate::mail::webview::hide_unless_painted(editor, window);
                }
            }),
        );
        #[cfg(any(windows, target_os = "macos"))]
        {
            let focus = !message.to.is_empty();
            let source = draft.meta.markup.clone();
            cx.defer_in(window, move |this: &mut Self, window, cx| {
                this.start_editor(source, focus, window, cx)
            });
        }

        if host == Host::Window {
            let view = cx.entity().downgrade();
            window.on_window_should_close(cx, move |_, cx| {
                view.update(cx, |this, cx| this.save_if_dirty(cx)).ok();
                true
            });
        }

        if message.to.is_empty() {
            to.update(cx, |field, cx| field.focus(window, cx));
        } else {
            body.update(cx, |body, cx| body.focus(window, cx));
        }

        Self {
            show_cc: !message.cc.is_empty(),
            show_bcc: !draft.meta.bcc.is_empty(),
            state,
            host,
            draft,
            to,
            cc,
            bcc,
            subject,
            body,
            link_input,
            signature_new,
            signature_reply,
            panel: None,
            from_open: false,
            #[cfg(any(windows, target_os = "macos"))]
            editor: None,
            #[cfg(any(windows, target_os = "macos"))]
            editor_failed: false,
            status: SaveStatus::Clean,
            autosave: None,
            closed: false,
            _subscriptions: subscriptions,
        }
    }

    /// Saved draft id (0 until the first save).
    pub fn message_id(&self) -> Id {
        self.draft.message.id
    }

    /// Subject, or "New message".
    pub fn title(&self, cx: &App) -> SharedString {
        let subject = self.subject.read(cx).value();
        if subject.trim().is_empty() {
            "New message".into()
        } else {
            subject
        }
    }

    /// The draft as currently edited.
    fn collect(&self, cx: &App) -> Draft {
        let mut draft = self.draft.clone();
        let source = self.body.read(cx).value().to_string();
        let blocks = markup::parse(&source);
        let text = markup::to_plain(&blocks);
        draft.message.to = self.to.read(cx).addresses(cx);
        draft.message.cc = self.cc.read(cx).addresses(cx);
        draft.meta.bcc = self.bcc.read(cx).addresses(cx);
        draft.message.subject = self.subject.read(cx).value().trim().to_owned();
        draft.message.preview = compose::preview_text(&text);
        draft.message.body_html = Some(markup::to_html(&blocks));
        draft.message.body_text = text;
        draft.message.date = Utc::now();
        draft.message.attachments = draft
            .meta
            .attachments
            .iter()
            .map(|file| Attachment {
                name: file.name.clone(),
                mime: file.mime.clone(),
                size: file.size,
            })
            .collect();
        draft.meta.markup = source;
        draft
    }

    // ---------- saving ----------

    fn mark_dirty(&mut self, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.status = SaveStatus::Unsaved;
        self.autosave = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(AUTOSAVE_DELAY).await;
            this.update(cx, |this, cx| {
                this.save(cx);
            })
            .ok();
        }));
        cx.notify();
    }

    /// Saves into the account's Drafts folder (not undoable: autosave). Returns success.
    fn save(&mut self, cx: &mut Context<Self>) -> bool {
        if self.closed {
            return false;
        }
        if self.draft.message.folder_id == 0 {
            let drafts = self
                .state
                .read(cx)
                .store()
                .folder_by_role(self.draft.message.account_id, FolderRole::Drafts);
            match drafts {
                Ok(Some(folder)) => self.draft.message.folder_id = folder.id,
                _ => {
                    self.status = SaveStatus::Failed;
                    self.state.update(cx, |state, cx| {
                        state.report_error("This account has no Drafts folder", cx)
                    });
                    cx.notify();
                    return false;
                }
            }
        }
        let draft = self.collect(cx);
        let saved = self.state.update(cx, |state, cx| {
            state.run_untracked(Command::SaveDraft(Box::new(draft.clone())), cx)
        });
        if saved {
            let id = if draft.message.id == 0 {
                compose::draft_by_key(self.state.read(cx).store(), &draft.meta.key)
                    .ok()
                    .flatten()
                    .map_or(0, |meta| meta.message_id)
            } else {
                draft.message.id
            };
            self.draft = draft;
            self.draft.message.id = id;
            self.draft.meta.message_id = id;
            self.status = SaveStatus::Saved(Local::now());
        } else {
            self.status = SaveStatus::Failed;
        }
        cx.notify();
        saved
    }

    fn save_if_dirty(&mut self, cx: &mut Context<Self>) {
        self.autosave = None;
        if self.status == SaveStatus::Unsaved {
            self.save(cx);
        }
    }

    /// Removes the editor from its dock or closes its window. Never saves.
    fn detach(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.closed = true;
        self.autosave = None;
        // Its child window goes away with it.
        #[cfg(any(windows, target_os = "macos"))]
        {
            self.editor = None;
        }
        match self.host {
            Host::Docked => {
                dock::remove(&cx.entity(), cx);
                self.state.update(cx, |_, cx| cx.notify());
            }
            Host::Window => window.remove_window(),
        }
    }

    /// Closes the editor, keeping the draft (saved if changed).
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_if_dirty(cx);
        self.detach(window, cx);
    }

    fn discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.message_id() == 0 {
            self.detach(window, cx);
            return;
        }
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let view = view.clone();
            alert
                .title("Discard this draft?")
                .description("The draft is deleted. Undo with Ctrl+Z.")
                .ok_text("Discard")
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    view.update(cx, |this, cx| this.discard_now(window, cx))
                        .ok();
                    true
                })
        });
    }

    fn discard_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.autosave = None;
        let id = self.message_id();
        if id != 0 {
            self.state.update(cx, |state, cx| {
                state.run(Command::DeleteDraft(id), cx);
            });
        }
        self.detach(window, cx);
    }

    // ---------- sending ----------

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.send_at(None, window, cx);
    }

    /// Sends now (with the undo delay) or, with `at`, keeps the draft and sends it then.
    fn send_at(&mut self, at: Option<DateTime<Utc>>, window: &mut Window, cx: &mut Context<Self>) {
        self.send_checked(at, false, window, cx);
    }

    /// [`Self::send_at`]; unless `confirmed`, first asks about an empty subject or an attachment
    /// the text mentions but that is missing.
    fn send_checked(
        &mut self,
        at: Option<DateTime<Utc>>,
        confirmed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.closed {
            return;
        }
        let fields = [self.to.clone(), self.cc.clone(), self.bcc.clone()];
        for field in &fields {
            field.update(cx, |field, cx| field.commit_pending(window, cx));
        }
        let problems: Vec<String> = fields
            .iter()
            .flat_map(|field| field.read(cx).problems())
            .collect();
        if !problems.is_empty() {
            let message = format!(
                "Check these recipients (or use Check names): {}",
                problems.join(", ")
            );
            self.state
                .update(cx, |state, cx| state.report_error(message, cx));
            return;
        }
        if fields
            .iter()
            .all(|field| field.read(cx).addresses(cx).is_empty())
        {
            self.state.update(cx, |state, cx| {
                state.report_error("Add at least one recipient", cx)
            });
            self.to.update(cx, |field, cx| field.focus(window, cx));
            return;
        }
        if !confirmed && self.confirm_send(at, window, cx) {
            return;
        }
        self.status = SaveStatus::Unsaved;
        if !self.save(cx) || self.message_id() == 0 {
            return;
        }
        let subject = self.draft.message.subject.clone();
        let id = self.message_id();
        self.detach(window, cx);
        match at {
            Some(at) => crate::later::schedule_send(&self.state, id, at, cx),
            None => send::schedule(self.state.clone(), id, subject, cx),
        }
    }

    /// Opens "Send anyway?" when the draft has no subject or misses an attachment it mentions;
    /// true when it did (sending then waits for the answer).
    fn confirm_send(
        &mut self,
        at: Option<DateTime<Utc>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let draft = self.collect(cx);
        let attachments = draft.meta.attachments.len() + draft.message.attachments.len();
        let warnings = compose::send_warnings(
            &draft.message.subject,
            &draft.message.body_text,
            attachments,
        );
        if warnings.is_empty() {
            return false;
        }
        let description = warnings
            .iter()
            .map(|warning| match warning {
                compose::SendWarning::NoSubject => tr(cx, "This message has no subject."),
                compose::SendWarning::MissingAttachment => tr(
                    cx,
                    "The message mentions an attachment, but nothing is attached.",
                ),
            })
            .collect::<Vec<_>>()
            .join(" ");
        let this = cx.entity().downgrade();
        crate::dialogs::confirm(
            crate::dialogs::Confirm {
                title: tr(cx, "Send anyway?").into(),
                description: description.into(),
                ok: tr(cx, "Send").into(),
                danger: false,
            },
            move |window, cx| {
                this.update(cx, |this, cx| this.send_checked(at, true, window, cx))
                    .ok();
            },
            window,
            cx,
        );
        true
    }

    fn pick_send_time(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let on_pick: crate::later::OnPick = std::rc::Rc::new(move |at, window, cx| {
            this.update(cx, |this, cx| this.send_at(Some(at), window, cx))
                .ok();
        });
        crate::later::open_time_picker("Send later", on_pick, window, cx);
    }

    fn pop_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.status == SaveStatus::Unsaved && !self.save(cx) {
            return;
        }
        let draft = self.collect(cx);
        let state = self.state.clone();
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(860.), px(780.)), cx)),
            window_min_size: Some(size(px(560.), px(480.))),
            ..crate::titlebar::window_options(self.title(cx))
        };
        let opened = crate::titlebar::open_window(options, cx, move |window, cx| {
            cx.new(|cx| ComposeView::new(state, draft, Host::Window, window, cx))
        });
        match opened {
            Ok(_) => self.detach(window, cx),
            Err(err) => self.state.update(cx, |state, cx| {
                state.report_error(format!("Cannot open a window: {err}"), cx)
            }),
        }
    }

    // ---------- fields ----------

    fn focus_body(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(any(windows, target_os = "macos"))]
        if let Some(editor) = &self.editor {
            editor.focus_page();
            return;
        }
        self.body.update(cx, |body, cx| body.focus(window, cx));
        cx.notify();
    }

    fn set_account(&mut self, account: &Account, cx: &mut Context<Self>) {
        self.from_open = false;
        if account.id != self.draft.message.account_id {
            let drafts = self
                .state
                .read(cx)
                .store()
                .folder_by_role(account.id, FolderRole::Drafts)
                .ok()
                .flatten();
            let message = &mut self.draft.message;
            message.account_id = account.id;
            message.from = Address::new(account.name.clone(), account.email.clone());
            message.folder_id = drafts.map_or(0, |folder| folder.id);
            self.mark_dirty(cx);
        }
        cx.notify();
    }

    fn check_names(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut unresolved = 0;
        for field in [self.to.clone(), self.cc.clone(), self.bcc.clone()] {
            unresolved += field.update(cx, |field, cx| {
                field.commit_pending(window, cx);
                field.check_names(cx)
            });
        }
        self.state.update(cx, |state, cx| {
            if unresolved == 0 {
                state.notify_info("All names resolved", cx);
            } else {
                state.report_error(
                    format!("{unresolved} name(s) could not be resolved (shown in colour)"),
                    cx,
                );
            }
        });
    }

    fn toggle_importance(&mut self, importance: Importance, cx: &mut Context<Self>) {
        let current = &mut self.draft.message.importance;
        *current = if *current == importance {
            Importance::Normal
        } else {
            importance
        };
        self.mark_dirty(cx);
    }

    // ---------- attachments ----------

    fn pick_files(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await {
                this.update(cx, |this, cx| this.add_files(paths, cx)).ok();
            }
        })
        .detach();
    }

    fn add_files(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let mut added = false;
        for path in paths {
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            let path_text = path.to_string_lossy().into_owned();
            let known = self
                .draft
                .meta
                .attachments
                .iter()
                .any(|file| file.path == path_text);
            if !metadata.is_file() || known {
                continue;
            }
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path_text.clone());
            self.draft.meta.attachments.push(DraftAttachment {
                mime: compose::guess_mime(&name).to_owned(),
                name,
                path: path_text,
                size: metadata.len(),
            });
            added = true;
        }
        if !added {
            return;
        }
        let total: u64 = self
            .draft
            .meta
            .attachments
            .iter()
            .map(|file| file.size)
            .sum();
        if total > LARGE_ATTACHMENTS {
            self.state.update(cx, |state, cx| {
                state.notify_info(
                    format!(
                        "Attachments total {}: many servers refuse messages over 25 MB",
                        format_size(total)
                    ),
                    cx,
                )
            });
        }
        self.mark_dirty(cx);
    }

    fn remove_attachment(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.draft.meta.attachments.len() {
            self.draft.meta.attachments.remove(index);
            self.mark_dirty(cx);
        }
    }

    // ---------- body editing ----------

    /// Applies a markup edit computed from the body text and its selection.
    fn edit_body(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        make: impl FnOnce(&str, Range<usize>) -> Edit,
    ) {
        self.body.update(cx, |body, cx| {
            let text = body.value();
            let edit = make(&text, body.selected_range());
            body.set_selected_range(edit.range.clone(), cx);
            body.replace(edit.text, window, cx);
            body.set_selected_range(edit.select, cx);
            body.focus(window, cx);
        });
        cx.notify();
    }

    fn wrap(
        &mut self,
        open: String,
        close: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.run_in_editor(|| editor_js::format_command(&open)) {
            return;
        }
        self.edit_body(window, cx, move |text, selection| {
            markup::toggle_wrap(text, selection, &open, close)
        });
    }

    fn lines(&mut self, format: LineFormat, window: &mut Window, cx: &mut Context<Self>) {
        let command = match format {
            LineFormat::Bullet => "mbx.cmd('insertUnorderedList')",
            LineFormat::Numbered => "mbx.cmd('insertOrderedList')",
            LineFormat::Quote => "mbx.quote()",
        };
        if self.run_in_editor(|| Some(command.to_owned())) {
            return;
        }
        self.edit_body(window, cx, move |text, selection| {
            markup::toggle_lines(text, selection, format)
        });
    }

    fn indent(&mut self, outdent: bool, window: &mut Window, cx: &mut Context<Self>) {
        let command = if outdent {
            "mbx.cmd('outdent')"
        } else {
            "mbx.cmd('indent')"
        };
        if self.run_in_editor(|| Some(command.to_owned())) {
            return;
        }
        self.edit_body(window, cx, move |text, selection| {
            markup::indent_lines(text, selection, outdent)
        });
    }

    /// Removes inline formatting from the selection (keeps lists and quotes).
    fn clear_formatting(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.run_in_editor(|| Some("mbx.cmd('removeFormat');mbx.cmd('unlink')".to_owned())) {
            return;
        }
        self.edit_body(window, cx, |text, selection| {
            let plain = markup::plain_text(&text[selection.clone()]);
            let end = selection.start + plain.len();
            Edit {
                select: selection.start..end,
                range: selection,
                text: plain,
            }
        });
    }

    /// After Enter on a list or quote line: repeats its marker (next number), or ends the
    /// list when the item was left empty.
    fn continue_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.body.update(cx, |body, cx| {
            let text = body.value();
            let Some(edit) = list_continuation(&text, body.selected_range().start) else {
                return;
            };
            body.set_selected_range(edit.range, cx);
            body.replace(edit.text, window, cx);
            body.set_selected_range(edit.select, cx);
        });
    }

    fn insert_text(&mut self, text: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if self.run_in_editor(|| Some(editor_js::insert_text(text))) {
            return;
        }
        self.body.update(cx, |body, cx| {
            body.replace(text, window, cx);
            body.focus(window, cx);
        });
    }

    fn insert_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.link_input.read(cx).value().trim().to_owned();
        if url.is_empty() {
            return;
        }
        self.link_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.panel = None;
        if self.run_in_editor(|| editor_js::link(&url)) {
            cx.notify();
            return;
        }
        self.edit_body(window, cx, move |text, selection| {
            markup::insert_link(text, selection, &url)
        });
    }

    fn find_replace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_body(window, cx);
        window.dispatch_action(Box::new(Search), cx);
    }

    fn toggle_panel(&mut self, panel: Panel, window: &mut Window, cx: &mut Context<Self>) {
        if self.panel == Some(panel) {
            self.panel = None;
        } else {
            self.panel = Some(panel);
            match panel {
                Panel::Link => self
                    .link_input
                    .update(cx, |input, cx| input.focus(window, cx)),
                Panel::Signature => self.load_signatures(window, cx),
                _ => {}
            }
        }
        cx.notify();
    }

    // ---------- signatures ----------

    fn load_signatures(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let signature =
            compose::signature(self.state.read(cx).store(), self.draft.message.account_id)
                .ok()
                .flatten()
                .unwrap_or_default();
        self.signature_new.update(cx, |input, cx| {
            input.set_value(signature.new_message, window, cx)
        });
        self.signature_reply
            .update(cx, |input, cx| input.set_value(signature.reply, window, cx));
    }

    fn save_signatures(&mut self, cx: &mut Context<Self>) {
        let signature = Signature {
            id: 0,
            account_id: self.draft.message.account_id,
            new_message: self.signature_new.read(cx).value().trim_end().to_owned(),
            reply: self.signature_reply.read(cx).value().trim_end().to_owned(),
        };
        self.state.update(cx, |state, cx| {
            if state.run(Command::SaveSignature(Box::new(signature)), cx) {
                state.notify_info("Signatures saved", cx);
            }
        });
    }

    fn insert_signature(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let source = if self.draft.meta.reply_to.is_some() {
            &self.signature_reply
        } else {
            &self.signature_new
        };
        let signature = source.read(cx).value().trim().to_owned();
        if signature.is_empty() {
            return;
        }
        self.panel = None;
        if self.run_in_editor(|| Some(editor_js::insert_markup(&format!("\n{signature}")))) {
            cx.notify();
            return;
        }
        self.edit_body(window, cx, move |_text, selection| {
            let insert = format!("\n\n{signature}\n");
            let caret = selection.start + insert.len();
            Edit {
                range: selection,
                text: insert,
                select: caret..caret,
            }
        });
    }

    // ---------- formatted editor ----------

    /// Whether the body is the formatted editor rather than the markup text area.
    fn uses_editor(&self) -> bool {
        #[cfg(any(windows, target_os = "macos"))]
        return !self.editor_failed;
        #[cfg(not(any(windows, target_os = "macos")))]
        false
    }

    /// Runs the command `make` builds in the formatted editor; false without it (the caller
    /// then edits the markup).
    fn run_in_editor(&self, make: impl FnOnce() -> Option<String>) -> bool {
        #[cfg(any(windows, target_os = "macos"))]
        if let Some(editor) = &self.editor {
            if let Some(js) = make() {
                editor.run(&js);
            }
            // The toolbar click took the keyboard: typing goes on in the editor.
            editor.refocus();
            return true;
        }
        let _ = make;
        false
    }

    /// Creates the formatted editor (in the background) showing `source`; `focus` gives it the
    /// keyboard once its page is ready. The text area stays the body if it cannot start.
    #[cfg(any(windows, target_os = "macos"))]
    fn start_editor(
        &mut self,
        source: String,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::theme::{css_color, rgb_bytes};
        let Ok(parent) = mailbox_window_handle::ParentWindow::of(window) else {
            self.editor_failed = true;
            cx.notify();
            return;
        };
        let t = theme(cx);
        let colors = editor_js::EditorColors {
            background: css_color(t.background),
            text: css_color(t.text),
            placeholder: css_color(t.text_subtle),
            link: css_color(t.accent),
            quote: css_color(t.border),
        };
        let background = rgb_bytes(t.background);
        let html = editor_js::page(&source, tr(cx, "Write your message…"), &colors);
        cx.spawn_in(window, async move |this, cx| {
            let (events, received) = async_channel::unbounded();
            // Outside any `update`: creating the web view runs a nested message loop.
            let created = MailWebView::new_editor(&parent, events, html, background);
            let started = this
                .update(cx, |this, cx| {
                    match created {
                        Ok(editor) => this.editor = Some(editor),
                        Err(err) => {
                            crate::log::error(format_args!(
                                "formatted editor unavailable, using the text area: {err}"
                            ));
                            this.editor_failed = true;
                        }
                    }
                    cx.notify();
                    this.editor.is_some()
                })
                .unwrap_or(false);
            while started && let Ok(WebEvent::Editor(message)) = received.recv().await {
                let handled = this.update_in(cx, |this, window, cx| {
                    this.on_editor_message(&message, focus, window, cx)
                });
                if handled.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// `change\n<markup>` (stored in the body field, saved later), `send` (Ctrl+Enter) or
    /// `ready` (page loaded).
    #[cfg(any(windows, target_os = "macos"))]
    fn on_editor_message(
        &mut self,
        message: &str,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(markup) = message.strip_prefix("change\n") {
            if self.body.read(cx).value().as_ref() != markup {
                let markup = markup.to_owned();
                self.body
                    .update(cx, |body, cx| body.set_value(markup, window, cx));
                self.mark_dirty(cx);
            }
        } else if message == "send" {
            self.send(window, cx);
        } else if message == "ready"
            && focus
            && let Some(editor) = &self.editor
        {
            editor.focus_page();
        }
    }

    /// The body: the formatted editor (its theme background while it starts), else the markup
    /// text area.
    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        #[cfg(any(windows, target_os = "macos"))]
        if self.uses_editor() {
            return div()
                .flex_1()
                .min_h(px(0.))
                .when_some(self.editor.clone(), |el, editor| {
                    el.child(crate::mail::webview::paper(editor))
                })
                .into_any_element();
        }
        let _ = cx;
        div()
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .px(SPACE_2)
            .py(SPACE_1)
            .child(Textarea::new(&self.body).appearance(false).h_full())
            .into_any_element()
    }

    // ---------- rendering ----------

    fn render_header(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let status = match self.status {
            SaveStatus::Clean => None,
            SaveStatus::Unsaved => Some("Editing…".to_owned()),
            SaveStatus::Saved(at) => Some(format!("Draft saved {}", at.format("%H:%M"))),
            SaveStatus::Failed => Some("Not saved".to_owned()),
        };
        div()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .items_center()
            .gap(SPACE_1)
            .h(px(48.))
            .pl(SPACE_4)
            .pr(SPACE_2)
            .border_b_1()
            .border_color(t.border_subtle)
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .text_size(TEXT_LG)
                    .font_weight(FontWeight::SEMIBOLD)
                    .truncate()
                    .child(self.title(cx)),
            )
            .when_some(status, |el, status| {
                el.child(
                    div()
                        .flex_shrink_0()
                        .mr(SPACE_2)
                        .text_size(TEXT_XS)
                        .text_color(if self.status == SaveStatus::Failed {
                            t.danger
                        } else {
                            t.text_subtle
                        })
                        .child(status),
                )
            })
            .child(
                IconButton::new("compose-signature", IconName::Signature)
                    .tooltip("Signatures")
                    .active(self.panel == Some(Panel::Signature))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_panel(Panel::Signature, window, cx)
                    })),
            )
            .when(self.host == Host::Docked, |el| {
                el.child(
                    IconButton::new("compose-pop-out", IconName::SquareArrowOutUpRight)
                        .tooltip("Pop out")
                        .on_click(cx.listener(|this, _, window, cx| this.pop_out(window, cx))),
                )
            })
            .child(
                IconButton::new("compose-close", IconName::X)
                    .tooltip("Close (keeps the draft)")
                    .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
            )
    }

    fn render_from(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let accounts = self
            .state
            .read(cx)
            .store()
            .list_accounts()
            .unwrap_or_default();
        let from = self.draft.message.from.clone();
        let choosable = accounts.len() > 1;
        let hover = t.hover;
        let menu = (self.from_open && choosable).then(|| {
            let items = accounts
                .into_iter()
                .map(|account| {
                    let selected = account.id == self.draft.message.account_id;
                    div()
                        .id(("compose-account", account.id as u64))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(SPACE_2)
                        .px(SPACE_2)
                        .py(SPACE_1)
                        .rounded(RADIUS_MD)
                        .cursor_pointer()
                        .when(selected, |el| el.bg(t.accent_subtle))
                        .hover(move |style| style.bg(hover))
                        .child(Avatar::new(account.name.clone()).size(px(24.)))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(div().text_size(TEXT_SM).child(account.name.clone()))
                                .child(
                                    div()
                                        .text_size(TEXT_XS)
                                        .text_color(t.text_muted)
                                        .child(account.email.clone()),
                                ),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.set_account(&account, cx)))
                })
                .collect::<Vec<_>>();
            deferred(
                anchored().snap_to_window().child(
                    div()
                        .occlude()
                        .mt(SPACE_1)
                        .w(px(300.))
                        .p(SPACE_1)
                        .flex()
                        .flex_col()
                        .gap(SPACE_1)
                        .rounded(RADIUS_LG)
                        .border_1()
                        .border_color(t.border)
                        .bg(t.elevated)
                        .shadow_lg()
                        .children(items),
                ),
            )
            .with_priority(1)
        });
        field_row("From", t).child(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .id("compose-from")
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(SPACE_1_5)
                        .h(px(28.))
                        .px(SPACE_1_5)
                        .rounded(RADIUS_MD)
                        .when(choosable, |el| {
                            el.cursor_pointer()
                                .hover(move |style| style.bg(hover))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.from_open = !this.from_open;
                                    cx.notify();
                                }))
                        })
                        .child(Avatar::new(from.display().to_owned()).size(px(20.)))
                        .child(div().text_size(TEXT_SM).child(from.name.clone()))
                        .child(
                            div()
                                .text_size(TEXT_SM)
                                .text_color(t.text_muted)
                                .child(format!("<{}>", from.email)),
                        )
                        .when(choosable, |el| {
                            el.child(
                                Icon::new(IconName::ChevronDown)
                                    .size(ICON_SIZE_SM)
                                    .text_color(t.text_muted),
                            )
                        }),
                )
                .when_some(menu, |el, menu| el.child(div().h_0().child(menu))),
        )
    }

    fn render_fields(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let to_row = field_row("To", t)
            .child(self.to.clone())
            .when(!self.show_cc, |el| {
                el.child(
                    Button::new("compose-show-cc")
                        .ghost()
                        .compact()
                        .label("Cc")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.show_cc = true;
                            this.cc.update(cx, |field, cx| field.focus(window, cx));
                            cx.notify();
                        })),
                )
            })
            .when(!self.show_bcc, |el| {
                el.child(
                    Button::new("compose-show-bcc")
                        .ghost()
                        .compact()
                        .label("Bcc")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.show_bcc = true;
                            this.bcc.update(cx, |field, cx| field.focus(window, cx));
                            cx.notify();
                        })),
                )
            })
            .child(
                IconButton::new("compose-check-names", IconName::UserCheck)
                    .tooltip("Check names")
                    .on_click(cx.listener(|this, _, window, cx| this.check_names(window, cx))),
            );
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .child(self.render_from(t, cx))
            .child(to_row)
            .when(self.show_cc, |el| {
                el.child(field_row("Cc", t).child(self.cc.clone()))
            })
            .when(self.show_bcc, |el| {
                el.child(field_row("Bcc", t).child(self.bcc.clone()))
            })
            .child(
                field_row("Subject", t).child(
                    div()
                        .flex_1()
                        .text_size(TEXT_LG)
                        .font_weight(FontWeight::MEDIUM)
                        .child(Input::new(&self.subject).appearance(false)),
                ),
            )
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = self.panel;
        let tool = |id: &'static str, icon: IconName, tooltip: &'static str| {
            IconButton::new(id, icon).tooltip(tooltip)
        };
        Toolbar::new()
            .child(tool("fmt-bold", IconName::Bold, "Bold (Ctrl+B)").on_click(
                cx.listener(|this, _, window, cx| this.wrap("**".into(), "**", window, cx)),
            ))
            .child(
                tool("fmt-italic", IconName::Italic, "Italic (Ctrl+I)").on_click(
                    cx.listener(|this, _, window, cx| this.wrap("*".into(), "*", window, cx)),
                ),
            )
            .child(
                tool("fmt-underline", IconName::Underline, "Underline (Ctrl+U)").on_click(
                    cx.listener(|this, _, window, cx| this.wrap("__".into(), "__", window, cx)),
                ),
            )
            .child(
                tool("fmt-strike", IconName::Strikethrough, "Strikethrough").on_click(
                    cx.listener(|this, _, window, cx| this.wrap("~~".into(), "~~", window, cx)),
                ),
            )
            .child(toolbar_separator(cx))
            .child(
                tool("fmt-color", IconName::Baseline, "Text colour")
                    .active(panel == Some(Panel::TextColor))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_panel(Panel::TextColor, window, cx)
                    })),
            )
            .child(
                tool("fmt-highlight", IconName::Highlighter, "Highlight")
                    .active(panel == Some(Panel::Highlight))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_panel(Panel::Highlight, window, cx)
                    })),
            )
            .child(
                tool("fmt-size", IconName::ALargeSmall, "Font size")
                    .active(panel == Some(Panel::Size))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_panel(Panel::Size, window, cx)
                    })),
            )
            .child(toolbar_separator(cx))
            .child(
                tool("fmt-bullets", IconName::List, "Bulleted list").on_click(
                    cx.listener(|this, _, window, cx| this.lines(LineFormat::Bullet, window, cx)),
                ),
            )
            .child(
                tool("fmt-numbers", IconName::ListOrdered, "Numbered list").on_click(
                    cx.listener(|this, _, window, cx| this.lines(LineFormat::Numbered, window, cx)),
                ),
            )
            .child(
                tool(
                    "fmt-outdent",
                    IconName::ListIndentDecrease,
                    "Decrease indent",
                )
                .on_click(cx.listener(|this, _, window, cx| this.indent(true, window, cx))),
            )
            .child(
                tool(
                    "fmt-indent",
                    IconName::ListIndentIncrease,
                    "Increase indent",
                )
                .on_click(cx.listener(|this, _, window, cx| this.indent(false, window, cx))),
            )
            .child(tool("fmt-quote", IconName::TextQuote, "Quote").on_click(
                cx.listener(|this, _, window, cx| this.lines(LineFormat::Quote, window, cx)),
            ))
            .child(toolbar_separator(cx))
            .child(
                tool("fmt-link", IconName::Link, "Link (Ctrl+K)")
                    .active(panel == Some(Panel::Link))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_panel(Panel::Link, window, cx)
                    })),
            )
            .child(
                tool(
                    "fmt-symbols",
                    IconName::FaceSlightlySmilingPlus,
                    "Emoji and symbols",
                )
                .active(panel == Some(Panel::Symbols))
                .on_click(
                    cx.listener(|this, _, window, cx| {
                        this.toggle_panel(Panel::Symbols, window, cx)
                    }),
                ),
            )
            .child(
                tool("fmt-clear", IconName::RemoveFormatting, "Clear formatting")
                    .on_click(cx.listener(|this, _, window, cx| this.clear_formatting(window, cx))),
            )
            .child(div().flex_1())
            .when(!self.uses_editor(), |el| {
                el.child(
                    tool(
                        "fmt-find",
                        IconName::TextSearch,
                        "Find and replace (Ctrl+F / Ctrl+H)",
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.find_replace(window, cx))),
                )
            })
    }

    fn render_panel(&self, panel: Panel, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let bar = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .flex_shrink_0()
            .items_center()
            .gap(SPACE_1_5)
            .px(SPACE_4)
            .py(SPACE_2)
            .bg(t.surface)
            .border_b_1()
            .border_color(t.border_subtle)
            .text_size(TEXT_SM);
        let label = |text: &'static str| div().mr(SPACE_1).text_color(t.text_muted).child(text);
        match panel {
            Panel::Link => bar
                .child(label("Link"))
                .child(div().flex_1().child(Input::new(&self.link_input)))
                .child(
                    Button::new("link-insert")
                        .primary()
                        .compact()
                        .label("Insert")
                        .on_click(cx.listener(|this, _, window, cx| this.insert_link(window, cx))),
                )
                .into_any_element(),
            Panel::TextColor | Panel::Highlight => {
                let highlight = panel == Panel::Highlight;
                let colors: &[u32] = if highlight { &HIGHLIGHTS } else { &TEXT_COLORS };
                let swatches = colors.iter().map(|&color| {
                    div()
                        .id(("swatch", u64::from(color)))
                        .size(px(22.))
                        .rounded(RADIUS_FULL)
                        .bg(hex(color))
                        .border_1()
                        .border_color(t.border)
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let key = if highlight { "bg" } else { "color" };
                            this.wrap(format!("{{{key}=#{color:06x}}}"), "{/}", window, cx);
                        }))
                });
                bar.child(label(if highlight {
                    "Highlight"
                } else {
                    "Text colour"
                }))
                .children(swatches)
                .into_any_element()
            }
            Panel::Size => bar
                .child(label("Size"))
                .children(SIZES.iter().map(|&(name, size)| {
                    Button::new(("size", u64::from(size)))
                        .compact()
                        .label(name)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.wrap(format!("{{size={size}}}"), "{/}", window, cx);
                        }))
                }))
                .into_any_element(),
            Panel::Symbols => {
                let hover = t.hover;
                bar.children(SYMBOLS.iter().enumerate().map(|(index, &symbol)| {
                    div()
                        .id(("symbol", index as u64))
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(28.))
                        .rounded(RADIUS_MD)
                        .text_size(TEXT_LG)
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.insert_text(symbol, window, cx)
                        }))
                        .child(symbol)
                }))
                .into_any_element()
            }
            Panel::Signature => div()
                .flex()
                .flex_col()
                .flex_shrink_0()
                .gap(SPACE_2)
                .px(SPACE_4)
                .py(SPACE_3)
                .bg(t.surface)
                .border_b_1()
                .border_color(t.border_subtle)
                .text_size(TEXT_SM)
                .child(label("New messages"))
                .child(Textarea::new(&self.signature_new))
                .child(label("Replies and forwards"))
                .child(Textarea::new(&self.signature_reply))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(SPACE_2)
                        .child(
                            Button::new("signature-save")
                                .primary()
                                .compact()
                                .label("Save signatures")
                                .on_click(cx.listener(|this, _, _, cx| this.save_signatures(cx))),
                        )
                        .child(
                            Button::new("signature-insert")
                                .compact()
                                .label("Insert here")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.insert_signature(window, cx)
                                })),
                        ),
                )
                .into_any_element(),
        }
    }

    fn render_attachments(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let chips =
            self.draft
                .meta
                .attachments
                .iter()
                .enumerate()
                .map(|(index, file)| {
                    div()
                        .id(("attachment", index as u64))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(SPACE_1_5)
                        .h(px(30.))
                        .pl(SPACE_2)
                        .pr(SPACE_1)
                        .rounded(RADIUS_MD)
                        .border_1()
                        .border_color(t.border)
                        .bg(t.elevated)
                        .text_size(TEXT_SM)
                        .child(
                            Icon::new(IconName::File)
                                .size(ICON_SIZE_SM)
                                .text_color(t.text_muted),
                        )
                        .child(div().max_w(px(200.)).truncate().child(file.name.clone()))
                        .child(
                            div()
                                .text_size(TEXT_XS)
                                .text_color(t.text_subtle)
                                .child(format_size(file.size)),
                        )
                        .child(
                            IconButton::new(("attachment-remove", index as u64), IconName::X)
                                .size(px(20.))
                                .icon_size(px(12.))
                                .tooltip("Remove attachment")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.remove_attachment(index, cx)
                                })),
                        )
                })
                .collect::<Vec<_>>();
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .flex_shrink_0()
            .gap(SPACE_2)
            .px(SPACE_4)
            .py(SPACE_2)
            .border_t_1()
            .border_color(t.border_subtle)
            .children(chips)
    }

    fn render_footer(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let importance = self.draft.message.importance;
        div()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .items_center()
            .gap(SPACE_1)
            .h(px(52.))
            .px(SPACE_4)
            .border_t_1()
            .border_color(t.border_subtle)
            .child(
                Button::new("compose-send")
                    .primary()
                    .icon(IconName::SendHorizontal)
                    .label("Send")
                    .on_click(cx.listener(|this, _, window, cx| this.send(window, cx))),
            )
            .child(
                IconButton::new("compose-send-later", IconName::CalendarClock)
                    .tooltip("Send later")
                    .on_click(cx.listener(|this, _, window, cx| this.pick_send_time(window, cx))),
            )
            .child(
                div()
                    .ml(SPACE_2)
                    .text_size(TEXT_XS)
                    .text_color(t.text_subtle)
                    .child("Ctrl+Enter"),
            )
            .child(div().flex_1())
            .child(
                IconButton::new("compose-attach", IconName::Paperclip)
                    .tooltip("Attach files (or drop them here)")
                    .on_click(cx.listener(|this, _, _, cx| this.pick_files(cx))),
            )
            .child(
                IconButton::new("compose-high", IconName::ArrowUp)
                    .tooltip("High importance")
                    .active(importance == Importance::High)
                    .on_click(
                        cx.listener(|this, _, _, cx| this.toggle_importance(Importance::High, cx)),
                    ),
            )
            .child(
                IconButton::new("compose-low", IconName::ArrowDown)
                    .tooltip("Low importance")
                    .active(importance == Importance::Low)
                    .on_click(
                        cx.listener(|this, _, _, cx| this.toggle_importance(Importance::Low, cx)),
                    ),
            )
            .child(toolbar_separator(cx))
            .child(
                IconButton::new("compose-discard", IconName::Trash)
                    .tooltip("Discard draft")
                    .on_click(cx.listener(|this, _, window, cx| this.discard(window, cx))),
            )
    }
}

impl Render for ComposeView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let drop_bg = t.accent_subtle;
        let panel = self.panel.map(|panel| self.render_panel(panel, &t, cx));
        let attachments = (!self.draft.meta.attachments.is_empty())
            .then(|| self.render_attachments(&t, cx).into_any_element());
        let editor = self.render_body(cx);

        div()
            .id("compose")
            .key_context("Compose")
            .on_action(cx.listener(|this, _: &SendMessage, window, cx| this.send(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleBold, window, cx| {
                this.wrap("**".into(), "**", window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleItalic, window, cx| {
                this.wrap("*".into(), "*", window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleUnderline, window, cx| {
                this.wrap("__".into(), "__", window, cx)
            }))
            .on_action(cx.listener(|this, _: &InsertLink, window, cx| {
                this.panel = None;
                this.toggle_panel(Panel::Link, window, cx)
            }))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.add_files(paths.paths().to_vec(), cx)
            }))
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(drop_bg))
            .flex()
            .flex_col()
            .size_full()
            .bg(t.background)
            .text_color(t.text)
            .text_size(TEXT_MD)
            .child(self.render_header(&t, cx))
            .child(self.render_fields(&t, cx))
            .child(self.render_toolbar(cx))
            .children(panel)
            .child(editor)
            .children(attachments)
            .child(self.render_footer(&t, cx))
    }
}

/// A labelled row of the address block.
fn field_row(label: &'static str, t: &Theme) -> Div {
    div()
        .flex()
        .flex_row()
        .flex_shrink_0()
        .items_center()
        .gap(SPACE_2)
        .min_h(px(40.))
        .px(SPACE_4)
        .border_b_1()
        .border_color(t.border_subtle)
        .child(
            div()
                .flex_shrink_0()
                .w(px(56.))
                .text_size(TEXT_SM)
                .text_color(t.text_muted)
                .child(label),
        )
}

/// The edit for Enter pressed at `caret` (just after the new line break) when the previous
/// line is a list item or quote: `- `, `* `, `1. ` (numbered on) or `> `, indent kept.
fn list_continuation(text: &str, caret: usize) -> Option<Edit> {
    let before = text.get(..caret)?.strip_suffix('\n')?;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let line = &before[line_start..];
    let indent = line.len() - line.trim_start().len();
    let rest = &line[indent..];
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    let (marker_len, next) = if ["- ", "* ", "> "].iter().any(|m| rest.starts_with(m)) {
        (2, rest[..2].to_owned())
    } else if digits > 0 && rest[digits..].starts_with(". ") {
        let number: u64 = rest[..digits].parse().ok()?;
        (digits + 2, format!("{}. ", number + 1))
    } else {
        return None;
    };
    if rest[marker_len..].trim().is_empty() {
        // Empty item: drop it and the line break, ending the list.
        return Some(Edit {
            range: line_start..caret,
            text: String::new(),
            select: line_start..line_start,
        });
    }
    let insert = format!("{}{next}", &line[..indent]);
    let end = caret + insert.len();
    Some(Edit {
        range: caret..caret,
        text: insert,
        select: end..end,
    })
}

/// "980 B", "12 KB", "3.4 MB".
fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.;
    let size = bytes as f64;
    if size < KB {
        format!("{bytes} B")
    } else if size < KB * KB {
        format!("{:.0} KB", size / KB)
    } else {
        format!("{:.1} MB", size / (KB * KB))
    }
}

#[cfg(test)]
mod tests {
    use super::list_continuation;

    fn enter(text: &str) -> Option<String> {
        let edit = list_continuation(text, text.len())?;
        let mut out = text.to_owned();
        out.replace_range(edit.range, &edit.text);
        Some(out)
    }

    #[test]
    fn lists_continue_and_end_on_an_empty_item() {
        assert_eq!(enter("- agenda\n").as_deref(), Some("- agenda\n- "));
        assert_eq!(
            enter("x\n  9. nine\n").as_deref(),
            Some("x\n  9. nine\n  10. ")
        );
        assert_eq!(enter("> quoted\n").as_deref(), Some("> quoted\n> "));
        assert_eq!(enter("- a\n- \n").as_deref(), Some("- a\n"));
        assert_eq!(enter("plain\n"), None);
        assert_eq!(enter("-dash\n"), None);
        assert_eq!(enter("no break"), None);
    }
}
