//! Reading pane: action bar, subject, the other messages of the conversation (collapsed, click to
//! show one), then the shown message: header, attachments and its body filling the rest. HTML
//! bodies are shown exactly as sent by the system web engine on Windows (see `webview`), plain
//! text and other platforms use our renderer. Remote pictures stay blocked until "Show pictures".

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use chrono::Local;
use gpui::component::Sizable as _;
use gpui::component::button::{Button as KitButton, ButtonVariants as _};
use gpui::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::{
    Address, Attachment, Command, Folder, FolderRole, Id, Importance, Message, mail_ops,
};

use super::html::{self, Block, RenderContext};
#[cfg(any(windows, target_os = "macos"))]
use super::webview::{self, MailWebView, WebEvent};
use super::{ComposeKind, message_menu, message_window, ops, sender_card};
use crate::app_state::AppState;
use crate::components::{
    Avatar, Badge, Button, EmptyState, Icon, IconButton, IconName, avatar_color, toolbar_separator,
    tooltip_above,
};
use crate::theme::{
    ICON_SIZE_SM, RADIUS_LG, RADIUS_MD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_6, TEXT_MD,
    TEXT_SM, TEXT_XL, TEXT_XS, Theme, hex, theme,
};
#[cfg(any(windows, target_os = "macos"))]
use mailbox_window_handle::ParentWindow;

/// Which message the pane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneSource {
    /// Follows the selection in `AppState` (main window).
    Selected,
    /// A fixed message (pop-out window).
    Fixed(Id),
}

pub struct ReadingPaneView {
    state: Entity<AppState>,
    source: PaneSource,
    /// Messages whose remote pictures the user allowed.
    show_pictures: HashSet<Id>,
    /// Messages whose page asked for remote content while it was blocked (from the webview).
    #[cfg(any(windows, target_os = "macos"))]
    blocked: HashSet<Id>,
    /// Conversation message the user clicked to show instead of the selected one…
    viewing: Option<Id>,
    /// …while this message is selected.
    viewing_for: Option<Id>,
    /// Parsed bodies by message id, with the body length used to detect edits.
    bodies: HashMap<Id, (usize, Rc<Vec<Block>>)>,
    /// Attachments an HTML body shows inline (`cid:`), by message id (with the body length):
    /// not listed again above the body.
    inline_parts: HashMap<Id, (usize, Rc<HashSet<String>>)>,
    /// Whether an HTML body loads remote content, by message id (with the body length).
    #[cfg(any(windows, target_os = "macos"))]
    remote: HashMap<Id, (usize, bool)>,
    #[cfg(any(windows, target_os = "macos"))]
    web: Option<Rc<MailWebView>>,
    /// WebView2 could not start: HTML goes through our renderer.
    #[cfg(any(windows, target_os = "macos"))]
    web_failed: bool,
    #[cfg(any(windows, target_os = "macos"))]
    web_creating: bool,
    /// Zoom of the message body in the web view (1.0 = 100%), the same for every message.
    zoom: f64,
    _subscription: Subscription,
}

/// Zoom levels of the message body.
const ZOOM_LEVELS: [f64; 9] = [0.67, 0.8, 0.9, 1., 1.1, 1.25, 1.5, 1.75, 2.];

fn human_size(bytes: u64) -> String {
    match bytes {
        b if b >= 1_048_576 => format!("{:.1} MB", b as f64 / 1_048_576.),
        b if b >= 1024 => format!("{} KB", b / 1024),
        b => format!("{b} B"),
    }
}

/// Icon of an attachment's kind, from its MIME type or else its extension (mail clients often
/// send `application/octet-stream`).
fn attachment_icon(attachment: &Attachment) -> IconName {
    let mime = attachment.mime.to_ascii_lowercase();
    let extension = attachment
        .name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    let is = |mimes: &[&str], extensions: &[&str]| {
        mimes.iter().any(|m| mime.contains(m)) || extensions.contains(&extension.as_str())
    };
    if mime.starts_with("image/") || is(&[], &["png", "jpg", "jpeg", "gif", "webp", "heic"]) {
        IconName::Image
    } else if is(
        &["spreadsheet", "excel", "csv"],
        &["xls", "xlsx", "ods", "csv", "numbers"],
    ) {
        IconName::FileSpreadsheet
    } else if is(
        &["zip", "compressed", "x-7z", "x-rar", "gzip", "x-tar"],
        &["zip", "7z", "rar", "gz", "tar"],
    ) {
        IconName::FileArchive
    } else if mime.starts_with("audio/") || is(&[], &["mp3", "wav", "m4a", "ogg", "flac"]) {
        IconName::FileMusic
    } else if mime.starts_with("video/") || is(&[], &["mp4", "mov", "avi", "mkv", "webm"]) {
        IconName::FilePlay
    } else if is(&["calendar"], &["ics"]) {
        IconName::CalendarDays
    } else if mime.starts_with("text/")
        || is(
            &["pdf", "word", "document", "presentation", "rtf"],
            &[
                "pdf", "doc", "docx", "odt", "rtf", "txt", "ppt", "pptx", "key", "pages",
            ],
        )
    {
        IconName::FileText
    } else {
        IconName::File
    }
}

impl ReadingPaneView {
    pub fn new(
        state: Entity<AppState>,
        source: PaneSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.observe_in(&state, window, |_this, _, _window, cx| {
            // The pane may not be drawn any more (other module, reading pane off…).
            #[cfg(any(windows, target_os = "macos"))]
            if let Some(web) = &_this.web {
                webview::hide_unless_painted(web, _window);
            }
            cx.notify()
        });
        Self {
            state,
            source,
            show_pictures: HashSet::new(),
            #[cfg(any(windows, target_os = "macos"))]
            blocked: HashSet::new(),
            viewing: None,
            viewing_for: None,
            bodies: HashMap::new(),
            inline_parts: HashMap::new(),
            #[cfg(any(windows, target_os = "macos"))]
            remote: HashMap::new(),
            #[cfg(any(windows, target_os = "macos"))]
            web: None,
            #[cfg(any(windows, target_os = "macos"))]
            web_failed: false,
            #[cfg(any(windows, target_os = "macos"))]
            web_creating: false,
            zoom: 1.,
            _subscription: subscription,
        }
    }

    fn current_id(&self, cx: &App) -> Option<Id> {
        match self.source {
            PaneSource::Selected => self.state.read(cx).selected_message(),
            PaneSource::Fixed(id) => Some(id),
        }
    }

    fn blocks(&mut self, message: &Message) -> Rc<Vec<Block>> {
        let source = message.body_html.as_deref().unwrap_or(&message.body_text);
        if let Some((len, blocks)) = self.bodies.get(&message.id)
            && *len == source.len()
        {
            return blocks.clone();
        }
        let mut blocks = match &message.body_html {
            Some(body) if !body.trim().is_empty() => html::parse(body),
            _ => html::parse_plain(&message.body_text),
        };
        crate::attachments::resolve_inline_images(message, &mut blocks);
        let blocks = Rc::new(blocks);
        self.bodies
            .insert(message.id, (source.len(), blocks.clone()));
        blocks
    }

    fn action_bar(
        &self,
        message: &Message,
        in_role: Option<FolderRole>,
        cx: &mut Context<Self>,
    ) -> Div {
        let id = message.id;
        let flagged = message.flags.flagged;
        let read = message.flags.read;
        let compose = |kind: ComposeKind, state: Entity<AppState>| {
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                ops::compose_from(&state, id, kind, window, cx)
            }
        };
        let (s_flag, s_read, s_archive, s_delete, s_more) = (
            self.state.clone(),
            self.state.clone(),
            self.state.clone(),
            self.state.clone(),
            self.state.clone(),
        );
        let text_state = self.state.clone();
        let popout_state = self.state.clone();
        let show_popout = self.source == PaneSource::Selected;

        div()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .items_center()
            .gap(SPACE_1)
            .h(px(44.))
            .px(SPACE_3)
            .border_b_1()
            .border_color(theme(cx).border_subtle)
            .when(in_role == Some(FolderRole::Drafts), |el| {
                let state = self.state.clone();
                el.child(
                    Button::new("edit-draft")
                        .primary()
                        .compact()
                        .icon(IconName::SquarePen)
                        .label("Edit draft")
                        .on_click(move |_, window, cx| {
                            crate::compose::edit_draft(&state, id, window, cx)
                        }),
                )
            })
            .child(
                Button::new("reply")
                    .ghost()
                    .compact()
                    .icon(IconName::Reply)
                    .label("Reply")
                    .on_click(compose(ComposeKind::Reply, self.state.clone())),
            )
            .child(
                Button::new("reply-all")
                    .ghost()
                    .compact()
                    .icon(IconName::ReplyAll)
                    .label("Reply all")
                    .on_click(compose(ComposeKind::ReplyAll, self.state.clone())),
            )
            .child(
                Button::new("forward")
                    .ghost()
                    .compact()
                    .icon(IconName::Forward)
                    .label("Forward")
                    .on_click(compose(ComposeKind::Forward, self.state.clone())),
            )
            .child(toolbar_separator(cx))
            .when(in_role != Some(FolderRole::Archive), |el| {
                el.child(
                    IconButton::new("pane-archive", IconName::Archive)
                        .tooltip_above()
                        .tooltip("Archive")
                        .on_click(move |_, _, cx| {
                            ops::move_to_role(&s_archive, vec![id], FolderRole::Archive, cx)
                        }),
                )
            })
            .child(
                IconButton::new("pane-delete", IconName::Trash)
                    .tooltip_above()
                    .tooltip(if in_role == Some(FolderRole::Trash) {
                        "Delete permanently"
                    } else {
                        "Delete"
                    })
                    .on_click(move |_, window, cx| ops::delete(&s_delete, vec![id], window, cx)),
            )
            .child(
                IconButton::new("pane-flag", IconName::Flag)
                    .active(flagged)
                    .tooltip_above()
                    .tooltip(if flagged { "Clear flag" } else { "Flag" })
                    .on_click(move |_, _, cx| ops::set_flag(&s_flag, vec![id], !flagged, cx)),
            )
            .child(
                IconButton::new(
                    "pane-read",
                    if read {
                        IconName::Mail
                    } else {
                        IconName::MailOpen
                    },
                )
                .tooltip_above()
                .tooltip(if read {
                    "Mark as unread"
                } else {
                    "Mark as read"
                })
                .on_click(move |_, _, cx| ops::set_read(&s_read, vec![id], !read, cx)),
            )
            .child(
                KitButton::new("pane-more")
                    .ghost()
                    .small()
                    .icon(IconName::Ellipsis)
                    .tooltip("More actions")
                    .dropdown_menu(move |menu, window, cx| {
                        let state = text_state.clone();
                        let menu = menu.item(
                            PopupMenuItem::new("Copy text")
                                .icon(IconName::Copy)
                                .on_click(move |_, _, cx| {
                                    let text = state
                                        .read(cx)
                                        .store()
                                        .get::<Message>(id)
                                        .ok()
                                        .flatten()
                                        .map(|message| match &message.body_html {
                                            Some(body) => html::to_text(&html::parse(body)),
                                            None => message.body_text,
                                        });
                                    if let Some(text) = text {
                                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                                    }
                                }),
                        );
                        message_menu::build(menu, &s_more, vec![id], Some(id), window, cx)
                    }),
            )
            .child(div().flex_1())
            .when(cfg!(any(windows, target_os = "macos")), |el| {
                el.child(self.zoom_control(cx))
            })
            .when(show_popout, |el| {
                el.child(
                    IconButton::new("pane-popout", IconName::SquareArrowOutUpRight)
                        .tooltip_above()
                        .tooltip("Open in new window")
                        .on_click(move |_, _, cx| {
                            message_window::open(popout_state.clone(), id, cx)
                        }),
                )
            })
    }

    /// Zoom out, current zoom (click: 100%), zoom in.
    fn zoom_control(&self, cx: &mut Context<Self>) -> Div {
        let percent = format!("{}%", (self.zoom * 100.).round());
        div()
            .flex()
            .flex_row()
            .items_center()
            .child(
                IconButton::new("zoom-out", IconName::ZoomOut)
                    .tooltip_above()
                    .tooltip("Zoom out")
                    .on_click(cx.listener(|this, _, _, cx| this.step_zoom(-1, cx))),
            )
            .child(
                Button::new("zoom-reset")
                    .ghost()
                    .compact()
                    .label(percent)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.zoom = 1.;
                        cx.notify();
                    })),
            )
            .child(
                IconButton::new("zoom-in", IconName::ZoomIn)
                    .tooltip_above()
                    .tooltip("Zoom in")
                    .on_click(cx.listener(|this, _, _, cx| this.step_zoom(1, cx))),
            )
    }

    /// Moves to the next (`1`) or previous (`-1`) zoom level.
    fn step_zoom(&mut self, step: i32, cx: &mut Context<Self>) {
        let current = ZOOM_LEVELS
            .iter()
            .position(|level| (level - self.zoom).abs() < 0.001)
            .unwrap_or(3) as i32;
        let next = (current + step).clamp(0, ZOOM_LEVELS.len() as i32 - 1);
        self.zoom = ZOOM_LEVELS[next as usize];
        cx.notify();
    }

    fn subject_header(&self, message: &Message, t: &Theme, cx: &App) -> Div {
        let store = self.state.read(cx).store();
        let colors: HashMap<String, u32> = store
            .list_categories()
            .unwrap_or_default()
            .into_iter()
            .map(|c| (c.name, c.color))
            .collect();
        let due = mail_ops::flag_due(store, message.id).ok().flatten();
        div()
            .flex()
            .flex_col()
            .gap(SPACE_2)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(SPACE_2)
                    .when(message.importance == Importance::High, |el| {
                        el.child(
                            div().pt(px(5.)).child(
                                Icon::new(IconName::CircleAlert)
                                    .size(px(16.))
                                    .text_color(t.danger),
                            ),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .text_size(TEXT_XL)
                            .line_height(px(26.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(if message.subject.is_empty() {
                                "(no subject)".to_owned()
                            } else {
                                message.subject.clone()
                            }),
                    ),
            )
            .when(
                !message.categories.is_empty() || message.flags.flagged || message.flags.pinned,
                |el| {
                    el.child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .items_center()
                            .gap(SPACE_1)
                            .when(message.flags.pinned, |el| {
                                el.child(Badge::new("Pinned").color(t.accent))
                            })
                            .when(message.flags.flagged, |el| {
                                let label = match due {
                                    Some(due) => format!("Follow up - {}", due.format("%a %d %b")),
                                    None => "Follow up".into(),
                                };
                                el.child(Badge::new(label).color(t.danger))
                            })
                            .children(message.categories.iter().map(|name| {
                                let color = colors
                                    .get(name)
                                    .map_or_else(|| avatar_color(name), |c| hex(*c));
                                Badge::new(name.clone()).color(color)
                            })),
                    )
                },
            )
    }

    fn pictures_bar(&self, id: Id, sender: &str, t: &Theme, cx: &mut Context<Self>) -> Div {
        let (state, key) = (self.state.clone(), pictures_key(sender));
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(SPACE_2)
            .px(SPACE_3)
            .py(SPACE_2)
            .rounded(RADIUS_MD)
            .bg(t.warning.alpha(if t.is_dark { 0.14 } else { 0.1 }))
            .border_1()
            .border_color(t.warning.alpha(0.3))
            .text_size(TEXT_SM)
            .child(
                Icon::new(IconName::ShieldAlert)
                    .size(ICON_SIZE_SM)
                    .text_color(t.warning),
            )
            .child(
                div()
                    .flex_1()
                    .text_color(t.text_muted)
                    .child("Pictures were blocked to protect your privacy."),
            )
            .child(
                Button::new(("show-pictures", id as u64))
                    .compact()
                    .icon(IconName::Image)
                    .label("Show pictures")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.show_pictures.insert(id);
                        cx.notify();
                    })),
            )
            .child(
                Button::new(("always-pictures", id as u64))
                    .compact()
                    .ghost()
                    .label("Always from this sender")
                    .on_click(move |_, _, cx| {
                        let command = Command::SetSetting {
                            key: key.clone(),
                            value: Some("true".into()),
                        };
                        state.update(cx, |state, cx| state.run(command, cx));
                    }),
            )
    }

    fn attachments(&self, message: &Message, t: &Theme) -> Div {
        let message_id = message.id;
        let all_state = self.state.clone();
        // Downloaded pictures are previewed above the chips (click opens them).
        let previews = message
            .attachments
            .iter()
            .enumerate()
            .filter(|(_, attachment)| crate::attachments::is_image(attachment))
            .filter_map(|(index, attachment)| {
                let path = crate::attachments::stored(message_id, &attachment.name)?;
                let (state, name) = (self.state.clone(), attachment.name.clone());
                let tooltip: SharedString = format!("Open {}", attachment.name).into();
                let accent = t.accent;
                Some(
                    div()
                        .id(("att-preview", (message_id as u64) * 1000 + index as u64))
                        .relative()
                        .cursor_pointer()
                        .rounded(RADIUS_MD)
                        .border_1()
                        .border_color(t.border_subtle)
                        .hover(move |style| style.border_color(accent))
                        .tooltip(move |window, cx| tooltip_above(tooltip.clone(), window, cx))
                        .overflow_hidden()
                        .w(px(160.))
                        .h(px(96.))
                        .bg(t.surface)
                        .child({
                            let subtle = t.text_subtle;
                            img(path)
                                .size_full()
                                .object_fit(ObjectFit::Cover)
                                .with_fallback(move || {
                                    div()
                                        .size_full()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            Icon::new(IconName::ImageOff)
                                                .size(px(20.))
                                                .text_color(subtle),
                                        )
                                        .into_any_element()
                                })
                        })
                        // The file name over the bottom of the picture: what a click opens.
                        .child(
                            div()
                                .absolute()
                                .bottom_0()
                                .left_0()
                                .right_0()
                                .px(SPACE_2)
                                .py(px(3.))
                                .bg(black().opacity(0.55))
                                .text_size(TEXT_XS)
                                .text_color(white())
                                .truncate()
                                .child(attachment.name.clone()),
                        )
                        .on_click(move |_, _, cx| {
                            crate::attachments::open(&state, message_id, &name, cx)
                        }),
                )
            })
            .collect::<Vec<_>>();
        let chips = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(SPACE_2)
            .children(
                message
                    .attachments
                    .iter()
                    .enumerate()
                    .map(|(index, attachment)| {
                        let key = (message.id as u64) * 1000 + index as u64;
                        let (open_state, save_state) = (self.state.clone(), self.state.clone());
                        let (open_name, save_name) =
                            (attachment.name.clone(), attachment.name.clone());
                        let (preview_state, preview_name) =
                            (self.state.clone(), attachment.name.clone());
                        let tooltip: SharedString = format!(
                            "{} ({}) - double-click to open",
                            attachment.name,
                            human_size(attachment.size)
                        )
                        .into();
                        let accent = t.accent;
                        div()
                            .id(("att-chip", key))
                            .cursor_pointer()
                            .hover(move |style| style.border_color(accent))
                            .tooltip(move |window, cx| tooltip_above(tooltip.clone(), window, cx))
                            // Double-click opens, like a file in Explorer.
                            .on_click(move |event, _, cx| {
                                if event.click_count() >= 2 {
                                    crate::attachments::open(
                                        &preview_state,
                                        message_id,
                                        &preview_name,
                                        cx,
                                    )
                                }
                            })
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(SPACE_2)
                            .h(px(44.))
                            .pl(SPACE_2)
                            .pr(SPACE_1)
                            .rounded(RADIUS_LG)
                            .border_1()
                            .border_color(t.border)
                            .bg(t.elevated)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(28.))
                                    .rounded(RADIUS_MD)
                                    .bg(t.accent_subtle)
                                    .text_color(t.accent)
                                    .child(
                                        Icon::new(attachment_icon(attachment)).size(ICON_SIZE_SM),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .max_w(px(180.))
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(TEXT_SM)
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(attachment.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_size(TEXT_XS)
                                            .text_color(t.text_subtle)
                                            .child(human_size(attachment.size)),
                                    ),
                            )
                            .child(
                                IconButton::new(("att-open", key), IconName::ExternalLink)
                                    .size(px(24.))
                                    .icon_size(px(13.))
                                    .tooltip("Open")
                                    .on_click(move |_, _, cx| {
                                        crate::attachments::open(
                                            &open_state,
                                            message_id,
                                            &open_name,
                                            cx,
                                        )
                                    }),
                            )
                            .child(
                                IconButton::new(("att-save", key), IconName::Download)
                                    .size(px(24.))
                                    .icon_size(px(13.))
                                    .tooltip("Save")
                                    .on_click(move |_, _, cx| {
                                        crate::attachments::save(
                                            &save_state,
                                            message_id,
                                            &save_name,
                                            cx,
                                        )
                                    }),
                            )
                    }),
            );
        let chips = if message.attachments.len() > 1 {
            chips.child(
                Button::new(("att-save-all", message_id as u64))
                    .compact()
                    .icon(IconName::Download)
                    .label("Save all")
                    .on_click(move |_, _, cx| {
                        crate::attachments::save_all(&all_state, message_id, cx)
                    }),
            )
        } else {
            chips
        };
        div()
            .flex()
            .flex_col()
            .gap(SPACE_2)
            .when(!previews.is_empty(), |el| {
                el.child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap(SPACE_2)
                        .children(previews),
                )
            })
            .child(chips)
    }

    /// `label` then each address as a name opening its contact card, comma-separated.
    fn recipients(
        &self,
        label: &'static str,
        list: &[Address],
        message: &Message,
        key: &'static str,
        t: &Theme,
    ) -> Div {
        div()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .gap(SPACE_1)
            .child(label)
            .children(list.iter().enumerate().map(|(index, address)| {
                let separator = if index + 1 < list.len() { "," } else { "" };
                div()
                    .flex()
                    .flex_row()
                    .child(sender_card::name(
                        &self.state,
                        address,
                        message.account_id,
                        false,
                        SharedString::from(format!("{key}-card-{}-{index}", message.id)),
                        t.elevated,
                    ))
                    .child(separator)
            }))
    }

    /// Sender, recipients (or the preview when `compact`) and date of a message.
    fn message_header(&self, message: &Message, compact: bool, t: &Theme) -> Div {
        let date = message
            .date
            .with_timezone(&Local)
            .format("%a %d %b %Y, %H:%M")
            .to_string();
        // Expanded: every name opens its contact card. Collapsed rows expand on click instead.
        let details = if compact {
            div().truncate().child(message.preview.clone())
        } else {
            div()
                .flex()
                .flex_row()
                .overflow_hidden()
                .whitespace_nowrap()
                .gap(SPACE_1)
                .child(self.recipients("To:", &message.to, message, "to", t))
                .when(!message.cc.is_empty(), |el| {
                    el.child(div().w(SPACE_2)).child(self.recipients(
                        "Cc:",
                        &message.cc,
                        message,
                        "cc",
                        t,
                    ))
                })
        };
        let sender = if compact {
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(message.from.display().to_owned())
                .into_any_element()
        } else {
            sender_card::name(
                &self.state,
                &message.from,
                message.account_id,
                true,
                ("sender-card", message.id as u64),
                t.elevated,
            )
            .into_any_element()
        };
        div()
            .flex()
            .flex_row()
            .items_start()
            .gap(SPACE_3)
            .child(Avatar::new(message.from.display().to_owned()).size(px(36.)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .gap(px(2.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(SPACE_2)
                            .text_size(TEXT_MD)
                            .child(sender)
                            .when(!message.from.name.is_empty(), |el| {
                                el.child(
                                    div()
                                        .min_w(px(0.))
                                        .truncate()
                                        .text_size(TEXT_SM)
                                        .text_color(t.text_subtle)
                                        .child(message.from.email.clone()),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_size(TEXT_SM)
                            .text_color(t.text_muted)
                            .child(details),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_shrink_0()
                    .items_center()
                    .gap(SPACE_1)
                    .text_size(TEXT_SM)
                    .text_color(t.text_subtle)
                    .when(!message.attachments.is_empty(), |el| {
                        el.child(Icon::new(IconName::Paperclip).size(px(12.)))
                    })
                    .child(date),
            )
    }

    /// Another message of the conversation, collapsed; clicking shows it in full below.
    fn other_message(&self, message: &Message, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let id = message.id;
        let hover = t.hover;
        div()
            .id(("card-header", id as u64))
            .cursor_pointer()
            .p(SPACE_3)
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(t.border_subtle)
            .bg(t.background)
            .hover(move |el| el.bg(hover))
            .child(self.message_header(message, true, t))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.viewing = Some(id);
                cx.notify();
            }))
            .into_any_element()
    }

    /// Names of the attachments the HTML body shows inline (referenced by `cid:`).
    fn inline_parts(&mut self, message: &Message) -> Rc<HashSet<String>> {
        let html = message.body_html.as_deref().unwrap_or_default();
        if let Some((len, names)) = self.inline_parts.get(&message.id)
            && *len == html.len()
        {
            return names.clone();
        }
        let names: HashSet<String> = html::cid_references(html)
            .iter()
            .enumerate()
            .filter_map(|(nth, cid)| {
                crate::attachments::cid_attachment(&message.attachments, cid, nth)
                    .map(|attachment| attachment.name.clone())
            })
            .collect();
        let names = Rc::new(names);
        self.inline_parts
            .insert(message.id, (html.len(), names.clone()));
        names
    }

    /// Whether the user chose to always load pictures from this sender.
    fn sender_allowed(&self, email: &str, cx: &App) -> bool {
        mailbox_core::settings::get(self.state.read(cx).store(), &pictures_key(email))
            .ok()
            .flatten()
            .is_some()
    }

    /// The message body: HTML in the system web engine, exactly as sent;
    /// otherwise our renderer in the app theme. Also returns "has remote content".
    fn body(
        &mut self,
        message: &Message,
        allowed: bool,
        window: &mut Window,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> (AnyElement, bool) {
        #[cfg(any(windows, target_os = "macos"))]
        if let Some(html) = html::sent_html(message.body_html.as_deref())
            && !self.web_failed
        {
            let web = self.web(window, cx);
            let zoom = self.zoom;
            let remote = match self.remote.get(&message.id) {
                Some((len, remote)) if *len == html.len() => *remote,
                _ => {
                    let remote = webview::has_remote_content(html);
                    self.remote.insert(message.id, (html.len(), remote));
                    remote
                }
            };
            // Nothing painted under the webview: until it exists (created in the background), or
            // if the system fails to show it, the app background shows, not a white void.
            let paper = div().size_full().when_some(web, |el, web| {
                let key = format!("{}-{}-{}", message.id, u8::from(allowed), html.len());
                web.load(key, (255, 255, 255), || web_page(message, html, allowed));
                web.set_zoom(zoom);
                el.child(webview::paper(web))
            });
            let paper = div()
                .flex_1()
                .min_h(px(0.))
                .mx(SPACE_4)
                .mb(SPACE_4)
                .border_1()
                .border_color(t.border)
                .child(paper);
            let remote = remote || self.blocked.contains(&message.id);
            return (paper.into_any_element(), remote);
        }
        // Plain text in the web view too, in the app's colors: its text can be selected.
        #[cfg(any(windows, target_os = "macos"))]
        if !self.web_failed && !message.body_text.trim().is_empty() {
            let web = self.web(window, cx);
            let zoom = self.zoom;
            let colors = page_colors(t);
            let paper = div().size_full().when_some(web, |el, web| {
                // The colors are in the key: a theme change reloads the page.
                let key = format!(
                    "{}-text-{}-{}{}",
                    message.id,
                    message.body_text.len(),
                    colors.background.trim_start_matches('#'),
                    colors.text.trim_start_matches('#'),
                );
                let page = || plain_web_page(message, &colors);
                web.load(key, crate::theme::rgb_bytes(t.background), page);
                web.set_zoom(zoom);
                el.child(webview::paper(web))
            });
            let paper = div().flex_1().min_h(px(0.)).mx(SPACE_2).child(paper);
            return (paper.into_any_element(), false);
        }
        #[cfg(any(windows, target_os = "macos"))]
        if let Some(web) = &self.web {
            web.set_visible(false);
        }
        let _ = (window, cx);

        let blocks = self.blocks(message);
        let link_state = self.state.clone();
        let ctx = RenderContext {
            theme: t.clone(),
            show_images: allowed,
            on_link: Rc::new(move |href, window, cx| ops::open_link(&link_state, href, window, cx)),
        };
        let body = div()
            .id(("reading-body", message.id as u64))
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .px(SPACE_6)
            .pb(SPACE_4)
            .gap(SPACE_3)
            .text_color(t.text)
            .children(html::render_blocks(
                &format!("body-{}", message.id),
                &blocks,
                &ctx,
            ));
        (body.into_any_element(), html::has_remote_images(&blocks))
    }

    /// The webview once created; the first call starts creating it in the background
    /// (`web_failed` without a system web engine: our renderer is used then).
    #[cfg(any(windows, target_os = "macos"))]
    fn web(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Rc<MailWebView>> {
        if self.web.is_some() || self.web_creating || self.web_failed {
            return self.web.clone();
        }
        let Ok(parent) = ParentWindow::of(window) else {
            self.web_failed = true;
            return None;
        };
        self.web_creating = true;
        let state = self.state.clone();
        cx.spawn_in(window, async move |this, cx| {
            // Outside any `update`: creating the webview runs GPUI tasks in a nested loop.
            let (events, received) = async_channel::unbounded();
            let web = MailWebView::new(&parent, events);
            let created = this
                .update(cx, |this, cx| {
                    this.web_creating = false;
                    match web {
                        Ok(web) => this.web = Some(web),
                        Err(err) => {
                            crate::log::error(format_args!(
                                "mail webview unavailable, using the built-in renderer: {err}"
                            ));
                            this.web_failed = true;
                        }
                    }
                    cx.notify();
                    this.web.is_some()
                })
                .unwrap_or(false);
            // Links clicked in the message.
            while created && let Ok(event) = received.recv().await {
                match event {
                    WebEvent::OpenLink(url) => cx
                        .update(|window, cx| ops::open_link(&state, &url, window, cx))
                        .ok(),
                    WebEvent::Editor(_) => None,
                    // Shows the "Pictures were blocked" bar for what the page really asked for.
                    WebEvent::Blocked(id) => this
                        .update(cx, |this, cx| {
                            if this.blocked.insert(id as Id) {
                                cx.notify();
                            }
                        })
                        .ok(),
                };
            }
        })
        .detach();
        None
    }
}

/// Settings key: always load remote pictures from this sender.
fn pictures_key(email: &str) -> String {
    format!("mail.pictures.sender.{}", email.trim().to_lowercase())
}

#[cfg(any(windows, target_os = "macos"))]
fn page_colors(t: &Theme) -> html::PageColors {
    use crate::theme::css_color;
    html::PageColors {
        background: css_color(t.background),
        text: css_color(t.text),
        muted: css_color(t.text_muted),
        link: css_color(t.accent),
        border: css_color(t.border),
    }
}

/// A plain-text body as a web page (no remote content, no inline parts).
#[cfg(any(windows, target_os = "macos"))]
fn plain_web_page(message: &Message, colors: &html::PageColors) -> webview::Page {
    webview::Page {
        id: message.id as u64,
        html: html::plain_page(&message.body_text, colors),
        ..Default::default()
    }
}

/// The page served to the webview: `cid:` pictures map to the stored inline parts.
#[cfg(any(windows, target_os = "macos"))]
fn web_page(message: &Message, html: &str, remote: bool) -> webview::Page {
    let prepared = webview::prepare(html);
    let inline = prepared
        .cids
        .iter()
        .enumerate()
        .map(|(nth, cid)| {
            let attachment = crate::attachments::cid_attachment(&message.attachments, cid, nth)?;
            let path = crate::attachments::stored(message.id, &attachment.name)?;
            Some((attachment.mime.clone(), path))
        })
        .collect();
    webview::Page {
        id: message.id as u64,
        html: prepared.html,
        remote,
        inline,
        remote_refs: prepared.remote,
        reported: false,
    }
}

impl Render for ReadingPaneView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let id = self.current_id(cx);
        if self.viewing_for != id {
            self.viewing_for = id;
            self.viewing = None;
        }
        let store = self.state.read(cx).store();
        let message = id.and_then(|id| store.get::<Message>(id).ok().flatten());
        let Some(message) = message else {
            #[cfg(any(windows, target_os = "macos"))]
            if let Some(web) = &self.web {
                web.set_visible(false);
            }
            let (title, description) = match self.source {
                PaneSource::Selected => (
                    "No message selected",
                    "Pick a message in the list to read it here.",
                ),
                PaneSource::Fixed(_) => ("Message unavailable", "It was deleted or moved away."),
            };
            return div()
                .size_full()
                .bg(t.background)
                .child(EmptyState::new(IconName::MailOpen, title).description(description))
                .into_any_element();
        };
        let role_of = |folder_id: Id| {
            store
                .get::<Folder>(folder_id)
                .ok()
                .flatten()
                .map(|folder| folder.role)
        };
        let in_role = role_of(message.folder_id);
        // Conversation: newest first, deleted/junk messages only when viewing those folders.
        let mut thread: Vec<Message> = store
            .list_thread(&message.thread_id)
            .unwrap_or_default()
            .into_iter()
            .filter(|m| {
                m.id == message.id
                    || !matches!(
                        role_of(m.folder_id),
                        Some(FolderRole::Trash | FolderRole::Junk)
                    )
                    || role_of(m.folder_id) == in_role
            })
            .collect();
        thread.reverse();
        if !thread.iter().any(|m| m.id == message.id) {
            thread.insert(0, message.clone());
        }
        // Shown in full: the selected message, or the conversation message the user clicked.
        let shown = self
            .viewing
            .and_then(|viewing| thread.iter().find(|m| m.id == viewing))
            .cloned()
            .unwrap_or(message);
        let shown_role = role_of(shown.folder_id);
        let allowed =
            self.show_pictures.contains(&shown.id) || self.sender_allowed(&shown.from.email, cx);

        let action_bar = self.action_bar(&shown, shown_role, cx);
        let subject = self.subject_header(&shown, &t, cx);
        // Meeting invitation: Accept / Tentative / Decline (calendar module).
        let invitation = crate::calendar::invitation_bar(&self.state, shown.id, cx);
        let others: Vec<AnyElement> = thread
            .iter()
            .filter(|m| m.id != shown.id)
            .map(|m| self.other_message(m, &t, cx))
            .collect();
        let (body, remote) = self.body(&shown, allowed, window, &t, cx);
        // Real attachments only: pictures shown inside the body are not listed again.
        let inline = self.inline_parts(&shown);
        let listed = Message {
            attachments: shown
                .attachments
                .iter()
                .filter(|attachment| !inline.contains(&attachment.name))
                .cloned()
                .collect(),
            ..shown.clone()
        };

        let header = div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .px(SPACE_6)
            .pt(SPACE_4)
            .pb(SPACE_3)
            .gap(SPACE_3)
            .child(subject)
            .children(invitation)
            .when(!others.is_empty(), |el| {
                el.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(SPACE_1)
                        .text_size(TEXT_XS)
                        .text_color(t.text_subtle)
                        .child(Icon::new(IconName::MessagesSquare).size(px(12.)))
                        .child(format!(
                            "{} messages in this conversation",
                            others.len() + 1
                        )),
                )
                .child(
                    div()
                        .id(("conversation", shown.id as u64))
                        .flex()
                        .flex_col()
                        .gap(SPACE_2)
                        .max_h(px(150.))
                        .overflow_y_scroll()
                        .children(others),
                )
            })
            .child(self.message_header(&shown, false, &t))
            .when(remote && !allowed, |el| {
                el.child(self.pictures_bar(shown.id, &shown.from.email, &t, cx))
            })
            .when(!listed.attachments.is_empty(), |el| {
                el.child(self.attachments(&listed, &t))
            });

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(t.surface)
            .child(action_bar)
            .child(header)
            .child(body)
            .into_any_element()
    }
}
