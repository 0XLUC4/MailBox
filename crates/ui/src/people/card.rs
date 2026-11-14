//! Read-only contact card: identity, actions, contact info, groups, notes, recent mail and
//! upcoming meetings with this person.

use chrono::{Local, Utc};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::people::{self, ContactCard, ContactGroup};
use mailbox_core::{Address, Id, PeopleCommand};

use super::{PeopleView, decode_photo};
use crate::app_state::Module;
use crate::components::{Avatar, Badge, Button, Icon, IconButton, IconName};
use crate::theme::{
    ICON_SIZE, ICON_SIZE_SM, RADIUS_FULL, RADIUS_LG, RADIUS_MD, SPACE_1, SPACE_1_5, SPACE_2,
    SPACE_3, SPACE_4, SPACE_6, TEXT_2XL, TEXT_MD, TEXT_SM, TEXT_XS, Theme, hex,
};

/// How many recent messages / days of meetings the card shows.
const RECENT_MESSAGES: usize = 5;
const UPCOMING_DAYS: i64 = 60;

impl PeopleView {
    pub(super) fn render_card(
        &mut self,
        card: &ContactCard,
        groups: &[ContactGroup],
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let id = card.id();
        let name = card.name();
        let contact = &card.contact;
        let details = &card.details;

        let store = self.state.read(cx).store();
        let recent = people::recent_messages_with(store, &contact.emails, RECENT_MESSAGES)
            .unwrap_or_default();
        let now = Utc::now();
        let upcoming = people::upcoming_events_with(store, &contact.emails, now, UPCOMING_DAYS)
            .unwrap_or_default();
        let category_colors = store.list_categories().unwrap_or_default();

        let recipients: Vec<Address> = card
            .primary_email()
            .map(|email| Address::new(name.clone(), email))
            .into_iter()
            .collect();

        // ----- header -----
        let photo = details.photo.as_deref().and_then(|uri| self.photo(uri));
        let portrait = match photo {
            Some(image) => img(image)
                .size(px(72.))
                .rounded(RADIUS_FULL)
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            None => Avatar::new(name.clone()).size(px(72.)).into_any_element(),
        };
        let headline = [contact.job_title.trim(), contact.company.trim()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        let header = div()
            .flex()
            .items_center()
            .gap(SPACE_4)
            .child(portrait)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w(px(0.))
                    .gap(SPACE_1)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(SPACE_2)
                            .child(
                                div()
                                    .truncate()
                                    .text_size(TEXT_2XL)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(name.clone()),
                            )
                            .when(contact.favorite, |el| {
                                el.child(
                                    Icon::new(IconName::Star)
                                        .size(ICON_SIZE)
                                        .text_color(t.warning),
                                )
                            }),
                    )
                    .when(!headline.is_empty(), |el| {
                        el.child(div().text_color(t.text_muted).child(headline))
                    })
                    .when(!details.nickname.trim().is_empty(), |el| {
                        el.child(
                            div()
                                .text_size(TEXT_SM)
                                .text_color(t.text_subtle)
                                .child(format!("“{}”", details.nickname.trim())),
                        )
                    }),
            );

        // ----- actions -----
        let favorite = contact.favorite;
        let compose_to = recipients.clone();
        let meet_with = recipients.clone();
        let first_phone = contact.phones.first().cloned();
        let actions = div()
            .flex()
            .items_center()
            .gap(SPACE_2)
            .child(
                Button::new("card-email")
                    .primary()
                    .icon(IconName::Mail)
                    .label("Email")
                    .disabled(recipients.is_empty())
                    .on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.emit_compose(compose_to.clone(), cx)
                        }),
                    ),
            )
            .child(
                Button::new("card-call")
                    .icon(IconName::Phone)
                    .label("Call")
                    .disabled(first_phone.is_none())
                    .on_click(move |_, _, cx| {
                        if let Some(phone) = &first_phone {
                            cx.open_url(&tel_url(phone));
                        }
                    }),
            )
            .child(
                Button::new("card-meeting")
                    .icon(IconName::CalendarPlus)
                    .label("Schedule meeting")
                    .disabled(recipients.is_empty())
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.emit_meeting(meet_with.clone(), cx)),
                    ),
            )
            .child(div().flex_1())
            .child(
                IconButton::new("card-favorite", IconName::Star)
                    .active(favorite)
                    .tooltip(if favorite {
                        "Remove from favorites"
                    } else {
                        "Add to favorites"
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.run(
                            PeopleCommand::SetFavorite {
                                id,
                                favorite: !favorite,
                            },
                            cx,
                        );
                    })),
            )
            .child(
                IconButton::new("card-edit", IconName::Pencil)
                    .tooltip("Edit")
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.edit_contact(id, window, cx)),
                    ),
            )
            .child(
                IconButton::new("card-export", IconName::Upload)
                    .tooltip("Export as .vcf")
                    .on_click(cx.listener(move |this, _, _, cx| this.export(Some(vec![id]), cx))),
            )
            .child(
                IconButton::new("card-delete", IconName::Trash)
                    .tooltip("Delete contact")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.delete_contacts(vec![id], window, cx)
                    })),
            );

        // ----- contact info -----
        let mut info: Vec<AnyElement> = Vec::new();
        for (index, email) in contact.emails.iter().enumerate() {
            let to = vec![Address::new(name.clone(), email.clone())];
            let label = field_label(details.email_label(index), "Email");
            info.push(
                info_row(
                    ("card-email-row", index as u64),
                    IconName::Mail,
                    email.clone(),
                    label,
                    t,
                )
                .child(self.copy_button(("copy-email", index as u64), email.clone(), cx))
                .on_click(cx.listener(move |this, _, _, cx| this.emit_compose(to.clone(), cx)))
                .into_any_element(),
            );
        }
        for (index, phone) in contact.phones.iter().enumerate() {
            let url = tel_url(phone);
            let label = field_label(details.phone_label(index), "Phone");
            info.push(
                info_row(
                    ("card-phone-row", index as u64),
                    IconName::Phone,
                    phone.clone(),
                    label,
                    t,
                )
                .child(self.copy_button(("copy-phone", index as u64), phone.clone(), cx))
                .on_click(move |_, _, cx| cx.open_url(&url))
                .into_any_element(),
            );
        }
        for (index, address) in details.addresses.iter().enumerate() {
            let label = if address.label.trim().is_empty() {
                "Address".to_owned()
            } else {
                capitalize(address.label.trim())
            };
            let value = address.one_line();
            let copied = value.clone();
            info.push(
                info_row(
                    ("card-address-row", index as u64),
                    IconName::MapPin,
                    value.clone(),
                    label,
                    t,
                )
                .child(self.copy_button(("copy-address", index as u64), value, cx))
                .on_click(cx.listener(move |this, _, _, cx| this.copy(copied.clone(), cx)))
                .into_any_element(),
            );
        }
        if let Some(birthday) = details.birthday {
            info.push(
                info_row(
                    "card-birthday",
                    IconName::Cake,
                    birthday.format("%-d %B %Y").to_string(),
                    "Birthday",
                    t,
                )
                .into_any_element(),
            );
        }
        let website = details.website.trim().to_owned();
        if !website.is_empty() {
            let url = if website.contains("://") {
                website.clone()
            } else {
                format!("https://{website}")
            };
            info.push(
                info_row("card-website", IconName::Globe, website, "Website", t)
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .into_any_element(),
            );
        }

        // ----- categories & groups -----
        let categories: Vec<AnyElement> = details
            .categories
            .iter()
            .map(|category| {
                let color = category_colors
                    .iter()
                    .find(|known| &known.name == category)
                    .map_or(t.accent, |known| hex(known.color));
                Badge::new(category.clone()).color(color).into_any_element()
            })
            .collect();
        let group_chips: Vec<AnyElement> = groups
            .iter()
            .map(|group| {
                let group_id = group.id;
                let member = group.members.contains(&id);
                chip(
                    ("card-group", group_id as u64),
                    group.name.clone(),
                    member,
                    t,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_member(group_id, id, cx)))
                .into_any_element()
            })
            .collect();

        // ----- activity -----
        let messages: Vec<AnyElement> = recent
            .into_iter()
            .map(|message| {
                let (message_id, folder_id) = (message.id, message.folder_id);
                activity_row(
                    ("card-message", message_id as u64),
                    IconName::Mail,
                    if message.subject.trim().is_empty() {
                        "(no subject)".to_owned()
                    } else {
                        message.subject.clone()
                    },
                    message
                        .date
                        .with_timezone(&Local)
                        .format("%-d %b %Y")
                        .to_string(),
                    t,
                )
                .on_click(
                    cx.listener(move |this, _, _, cx| this.open_message(message_id, folder_id, cx)),
                )
                .into_any_element()
            })
            .collect();
        let meetings: Vec<AnyElement> = upcoming
            .into_iter()
            .map(|event| {
                let start = event.start.with_timezone(&Local);
                let when = if event.recurrence.is_some() && event.start < now {
                    "Recurring".to_owned()
                } else if event.all_day {
                    start.format("%a %-d %b").to_string()
                } else {
                    start.format("%a %-d %b · %H:%M").to_string()
                };
                activity_row(
                    ("card-event", event.id as u64),
                    IconName::CalendarDays,
                    event.title.clone(),
                    when,
                    t,
                )
                .on_click(cx.listener(|this, _, _, cx| this.open_calendar(cx)))
                .into_any_element()
            })
            .collect();

        div()
            .id("contact-card")
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(SPACE_4)
                    .max_w(px(720.))
                    .px(SPACE_6)
                    .py(SPACE_6)
                    .child(header)
                    .child(actions)
                    .when(!info.is_empty(), |el| {
                        el.child(section("Contact info", t).children(info))
                    })
                    .when(!categories.is_empty(), |el| {
                        el.child(
                            section("Categories", t).child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap(SPACE_1_5)
                                    .px(SPACE_2)
                                    .children(categories),
                            ),
                        )
                    })
                    .when(!group_chips.is_empty(), |el| {
                        el.child(
                            section("Groups", t).child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap(SPACE_1_5)
                                    .px(SPACE_2)
                                    .children(group_chips),
                            ),
                        )
                    })
                    .when(!contact.notes.trim().is_empty(), |el| {
                        el.child(
                            section("Notes", t).child(
                                div()
                                    .px(SPACE_2)
                                    .text_color(t.text_muted)
                                    .whitespace_normal()
                                    .child(contact.notes.clone()),
                            ),
                        )
                    })
                    .child(section("Recent emails", t).map(|el| {
                        if messages.is_empty() {
                            el.child(empty_note("No emails with this person yet.", t))
                        } else {
                            el.children(messages)
                        }
                    }))
                    .child(section("Upcoming meetings", t).map(|el| {
                        if meetings.is_empty() {
                            el.child(empty_note("Nothing scheduled in the next 60 days.", t))
                        } else {
                            el.children(meetings)
                        }
                    })),
            )
    }

    /// Decoded photo, cached for the currently shown data URI.
    pub(super) fn photo(&mut self, uri: &str) -> Option<std::sync::Arc<Image>> {
        match &self.photo_cache {
            Some((cached, image)) if cached == uri => return Some(image.clone()),
            _ => {}
        }
        let image = decode_photo(uri)?;
        self.photo_cache = Some((uri.to_owned(), image.clone()));
        Some(image)
    }

    /// Small "Copy" button shown on row hover (does not trigger the row's own click).
    fn copy_button(
        &self,
        id: impl Into<ElementId>,
        text: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex_shrink_0()
            .opacity(0.)
            .group_hover("info-row", |style| style.opacity(1.))
            .child(
                IconButton::new(id, IconName::Copy)
                    .size(px(24.))
                    .icon_size(ICON_SIZE_SM)
                    .tooltip("Copy")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.copy(text.clone(), cx)
                    })),
            )
    }

    fn copy(&mut self, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.state
            .update(cx, |state, cx| state.notify_info("Copied to clipboard", cx));
    }

    fn open_message(&mut self, message_id: Id, folder_id: Id, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.select_folder(Some(folder_id), cx);
            state.select_message(Some(message_id), cx);
            state.set_module(Module::Mail, cx);
        });
    }

    fn open_calendar(&mut self, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, cx| state.set_module(Module::Calendar, cx));
    }
}

/// Titled card section.
fn section(title: &'static str, t: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(SPACE_1)
        .p(SPACE_2)
        .rounded(RADIUS_LG)
        .border_1()
        .border_color(t.border_subtle)
        .bg(t.elevated)
        .child(
            div()
                .px(SPACE_2)
                .pt(SPACE_1)
                .pb(SPACE_1)
                .text_size(TEXT_XS)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text_subtle)
                .child(title.to_uppercase()),
        )
}

/// Icon + value with a small label underneath.
fn info_row(
    id: impl Into<ElementId>,
    icon: IconName,
    value: String,
    label: impl Into<SharedString>,
    t: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .group("info-row")
        .flex()
        .items_center()
        .gap(SPACE_3)
        .px(SPACE_2)
        .py(SPACE_1_5)
        .rounded(RADIUS_MD)
        .cursor_pointer()
        .hover(|s| s.bg(t.hover))
        .child(Icon::new(icon).size(ICON_SIZE).text_color(t.text_muted))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.))
                .child(div().truncate().text_size(TEXT_MD).child(value))
                .child(
                    div()
                        .text_size(TEXT_XS)
                        .text_color(t.text_subtle)
                        .child(label.into()),
                ),
        )
}

/// One message or event line.
fn activity_row(
    id: impl Into<ElementId>,
    icon: IconName,
    title: String,
    meta: String,
    t: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(SPACE_3)
        .px(SPACE_2)
        .py(SPACE_1_5)
        .rounded(RADIUS_MD)
        .cursor_pointer()
        .hover(|s| s.bg(t.hover))
        .child(Icon::new(icon).size(ICON_SIZE_SM).text_color(t.text_subtle))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .truncate()
                .text_size(TEXT_MD)
                .child(title),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_size(TEXT_SM)
                .text_color(t.text_subtle)
                .child(meta),
        )
}

/// Toggleable pill.
pub(super) fn chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    active: bool,
    t: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(SPACE_1)
        .h(px(24.))
        .px(SPACE_2)
        .rounded(RADIUS_FULL)
        .border_1()
        .cursor_pointer()
        .text_size(TEXT_SM)
        .when(active, |el| {
            el.bg(t.accent_subtle)
                .border_color(t.accent)
                .text_color(t.accent)
                .child(Icon::new(IconName::Check).size(px(12.)))
        })
        .when(!active, |el| {
            el.border_color(t.border)
                .text_color(t.text_muted)
                .hover(|s| s.bg(t.hover))
        })
        .child(label.into())
}

fn empty_note(text: &'static str, t: &Theme) -> impl IntoElement {
    div()
        .px(SPACE_2)
        .pb(SPACE_1)
        .text_size(TEXT_SM)
        .text_color(t.text_subtle)
        .child(text)
}

/// "work" -> "Work"; `fallback` when there is no label.
fn field_label(label: &str, fallback: &str) -> String {
    if label.is_empty() {
        fallback.to_owned()
    } else {
        capitalize(label)
    }
}

/// `tel:` URL with only the characters dialers understand.
fn tel_url(phone: &str) -> String {
    let number: String = phone
        .chars()
        .filter(|c| c.is_ascii_digit() || matches!(c, '+' | '*' | '#'))
        .collect();
    format!("tel:{number}")
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}
