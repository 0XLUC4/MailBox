//! Contact editor: every field of a [`ContactCard`], with add/remove rows for labelled emails,
//! phones and addresses, a photo picker, categories and notes. Saved as one undoable command.
//! Enter in any single-line field saves, Esc cancels.

use chrono::NaiveDate;
use gpui::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::compose::is_valid_email;
use mailbox_core::people::{ContactCard, PostalAddress, trim_labels};
use mailbox_core::vcard;

use super::PeopleView;
use super::card::chip;
use crate::components::{Avatar, Banner, Button, IconButton, IconName};
use crate::theme::{
    ICON_SIZE_SM, RADIUS_FULL, RADIUS_LG, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_6, TEXT_XL,
    TEXT_XS, Theme,
};

/// Largest photo accepted (stored inline in the database and in exported vCards).
const MAX_PHOTO_BYTES: u64 = 1024 * 1024;

/// A value with its label ("work", "home"...): one email or phone row.
pub(super) struct LabeledInput {
    label: Entity<InputState>,
    value: Entity<InputState>,
}

impl LabeledInput {
    fn new(
        label: &str,
        value: &str,
        placeholder: &'static str,
        window: &mut Window,
        cx: &mut Context<PeopleView>,
    ) -> Self {
        Self {
            label: text_input("Label", label, window, cx),
            value: text_input(placeholder, value, window, cx),
        }
    }
}

/// Reads labelled rows: non-empty values and their labels (trailing empty labels dropped).
fn read_labeled(rows: &[LabeledInput], cx: &App) -> (Vec<String>, Vec<String>) {
    let (values, mut labels): (Vec<String>, Vec<String>) = rows
        .iter()
        .map(|row| (value(&row.value, cx), value(&row.label, cx).to_lowercase()))
        .filter(|(value, _)| !value.is_empty())
        .unzip();
    trim_labels(&mut labels);
    (values, labels)
}

pub(super) struct AddressFields {
    label: Entity<InputState>,
    street: Entity<InputState>,
    city: Entity<InputState>,
    region: Entity<InputState>,
    postal_code: Entity<InputState>,
    country: Entity<InputState>,
}

impl AddressFields {
    fn new(address: &PostalAddress, window: &mut Window, cx: &mut Context<PeopleView>) -> Self {
        Self {
            label: text_input("Label (home, work…)", &address.label, window, cx),
            street: text_input("Street", &address.street, window, cx),
            city: text_input("City", &address.city, window, cx),
            region: text_input("Region", &address.region, window, cx),
            postal_code: text_input("Postal code", &address.postal_code, window, cx),
            country: text_input("Country", &address.country, window, cx),
        }
    }

    fn read(&self, cx: &App) -> PostalAddress {
        PostalAddress {
            label: value(&self.label, cx),
            street: value(&self.street, cx),
            city: value(&self.city, cx),
            region: value(&self.region, cx),
            postal_code: value(&self.postal_code, cx),
            country: value(&self.country, cx),
        }
    }
}

/// Editing state for one contact (new or existing).
pub(super) struct ContactForm {
    base: ContactCard,
    display_name: Entity<InputState>,
    first_name: Entity<InputState>,
    last_name: Entity<InputState>,
    nickname: Entity<InputState>,
    company: Entity<InputState>,
    job_title: Entity<InputState>,
    website: Entity<InputState>,
    birthday: Entity<InputState>,
    notes: Entity<TextareaState>,
    emails: Vec<LabeledInput>,
    phones: Vec<LabeledInput>,
    addresses: Vec<AddressFields>,
    categories: Vec<String>,
    photo: Option<String>,
    favorite: bool,
    pub(super) error: Option<SharedString>,
}

impl ContactForm {
    pub(super) fn new(
        card: ContactCard,
        window: &mut Window,
        cx: &mut Context<PeopleView>,
    ) -> Self {
        let contact = &card.contact;
        let details = &card.details;
        let list = |values: &[String],
                    labels: &[String],
                    placeholder: &'static str,
                    window: &mut Window,
                    cx: &mut Context<PeopleView>| {
            let mut rows: Vec<LabeledInput> = values
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    let label = labels.get(index).map_or("", String::as_str);
                    LabeledInput::new(label, value, placeholder, window, cx)
                })
                .collect();
            if rows.is_empty() {
                rows.push(LabeledInput::new("", "", placeholder, window, cx));
            }
            rows
        };
        let notes_text = contact.notes.clone();
        let notes = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Notes")
                .default_value(notes_text)
        });
        // A name computed from first + last is left empty so renaming updates it.
        let full_name = format!("{} {}", details.first_name.trim(), details.last_name.trim());
        let shown_as = if contact.display_name.trim() == full_name.trim() {
            String::new()
        } else {
            contact.display_name.clone()
        };
        let first_name = text_input("First name", &details.first_name, window, cx);
        // Focus the first field so typing starts right away.
        first_name.update(cx, |input, cx| input.focus(window, cx));
        Self {
            display_name: text_input("Shown as (optional)", &shown_as, window, cx),
            first_name,
            last_name: text_input("Last name", &details.last_name, window, cx),
            nickname: text_input("Nickname", &details.nickname, window, cx),
            company: text_input("Company", &contact.company, window, cx),
            job_title: text_input("Job title", &contact.job_title, window, cx),
            website: text_input("https://", &details.website, window, cx),
            birthday: text_input(
                "YYYY-MM-DD",
                &details
                    .birthday
                    .map(|date| date.format("%Y-%m-%d").to_string())
                    .unwrap_or_default(),
                window,
                cx,
            ),
            notes,
            emails: list(
                &contact.emails,
                &details.email_labels,
                EMAIL_PLACEHOLDER,
                window,
                cx,
            ),
            phones: list(
                &contact.phones,
                &details.phone_labels,
                PHONE_PLACEHOLDER,
                window,
                cx,
            ),
            addresses: details
                .addresses
                .iter()
                .map(|address| AddressFields::new(address, window, cx))
                .collect(),
            categories: details.categories.clone(),
            photo: details.photo.clone(),
            favorite: contact.favorite,
            error: None,
            base: card,
        }
    }

    /// The edited card, or a message explaining what to fix.
    pub(super) fn to_card(&self, cx: &App) -> Result<ContactCard, String> {
        let mut card = self.base.clone();
        let (emails, email_labels) = read_labeled(&self.emails, cx);
        if let Some(invalid) = emails.iter().find(|email| !is_valid_email(email)) {
            return Err(format!("“{invalid}” is not an email address."));
        }
        let (phones, phone_labels) = read_labeled(&self.phones, cx);
        if let Some(invalid) = phones.iter().find(|phone| !is_phone(phone)) {
            return Err(format!("“{invalid}” is not a phone number."));
        }
        let website = value(&self.website, cx);
        if website.chars().any(char::is_whitespace) {
            return Err("The website should be a single address, like example.com.".into());
        }
        let birthday = value(&self.birthday, cx);
        let birthday = if birthday.is_empty() {
            None
        } else {
            Some(parse_birthday(&birthday).ok_or("Birthday should look like 1990-12-31.")?)
        };

        let contact = &mut card.contact;
        contact.display_name = value(&self.display_name, cx);
        contact.company = value(&self.company, cx);
        contact.job_title = value(&self.job_title, cx);
        contact.notes = self.notes.read(cx).value().trim_end().to_owned();
        contact.favorite = self.favorite;
        contact.emails = emails;
        contact.phones = phones;

        let details = &mut card.details;
        details.first_name = value(&self.first_name, cx);
        details.last_name = value(&self.last_name, cx);
        details.nickname = value(&self.nickname, cx);
        details.website = website;
        details.email_labels = email_labels;
        details.phone_labels = phone_labels;
        details.birthday = birthday;
        details.photo = self.photo.clone();
        details.categories = self.categories.clone();
        details.addresses = self
            .addresses
            .iter()
            .map(|fields| fields.read(cx))
            .filter(|address| !address.is_empty())
            .collect();

        if card.name().is_empty() {
            return Err("Add a name or an email address.".into());
        }
        Ok(card)
    }
}

impl PeopleView {
    pub(super) fn render_form(
        &mut self,
        t: &Theme,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let photo_uri = self.form.as_ref().and_then(|form| form.photo.clone());
        let photo = photo_uri.as_deref().and_then(|uri| self.photo(uri));
        let categories = self
            .state
            .read(cx)
            .store()
            .list_categories()
            .unwrap_or_default();
        let Some(form) = self.form.as_ref() else {
            return div().into_any_element();
        };
        let is_new = form.base.id() == 0;
        let preview_name = form.base.name();

        let portrait = match photo {
            Some(image) => img(image)
                .size(px(64.))
                .rounded(RADIUS_FULL)
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            None => Avatar::new(if preview_name.is_empty() {
                "?".to_owned()
            } else {
                preview_name
            })
            .size(px(64.))
            .into_any_element(),
        };

        let email_rows = form.emails.iter().enumerate().map(|(index, row)| {
            labeled_row(
                row,
                ("remove-email", index as u64),
                cx.listener(move |this, _, _, cx| {
                    if let Some(form) = this.form.as_mut().filter(|form| index < form.emails.len())
                    {
                        form.emails.remove(index);
                    }
                    cx.notify();
                }),
            )
        });
        let phone_rows = form.phones.iter().enumerate().map(|(index, row)| {
            labeled_row(
                row,
                ("remove-phone", index as u64),
                cx.listener(move |this, _, _, cx| {
                    if let Some(form) = this.form.as_mut().filter(|form| index < form.phones.len())
                    {
                        form.phones.remove(index);
                    }
                    cx.notify();
                }),
            )
        });
        let address_rows = form.addresses.iter().enumerate().map(|(index, fields)| {
            div()
                .flex()
                .flex_col()
                .gap(SPACE_2)
                .p(SPACE_3)
                .rounded(RADIUS_LG)
                .border_1()
                .border_color(t.border_subtle)
                .child(removable(
                    &fields.label,
                    ("remove-address", index as u64),
                    cx.listener(move |this, _, _, cx| {
                        if let Some(form) = this
                            .form
                            .as_mut()
                            .filter(|form| index < form.addresses.len())
                        {
                            form.addresses.remove(index);
                        }
                        cx.notify();
                    }),
                ))
                .child(Input::new(&fields.street))
                .child(pair(
                    Input::new(&fields.postal_code),
                    Input::new(&fields.city),
                ))
                .child(pair(
                    Input::new(&fields.region),
                    Input::new(&fields.country),
                ))
        });

        let mut category_names: Vec<String> = categories
            .iter()
            .map(|category| category.name.clone())
            .collect();
        for extra in &form.categories {
            if !category_names.contains(extra) {
                category_names.push(extra.clone());
            }
        }
        let has_categories = !category_names.is_empty();
        let category_chips = category_names.into_iter().enumerate().map(|(index, name)| {
            let active = form.categories.contains(&name);
            let toggled = name.clone();
            chip(("form-category", index as u64), name, active, t).on_click(cx.listener(
                move |this, _, _, cx| {
                    if let Some(form) = this.form.as_mut() {
                        if form.categories.contains(&toggled) {
                            form.categories.retain(|category| *category != toggled);
                        } else {
                            form.categories.push(toggled.clone());
                        }
                    }
                    cx.notify();
                },
            ))
        });

        div()
            .id("contact-form")
            .track_focus(&self.form_focus)
            .size_full()
            .overflow_y_scroll()
            // Capture phase: runs before the focused input, which would otherwise eat Esc.
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.cancel_form(cx);
                }
            }))
            // Enter with focus outside a text field (after picking a photo, toggling a chip).
            // Inputs save through `PressEnter`; Notes keeps Enter for new lines.
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let in_notes = this
                    .form
                    .as_ref()
                    .is_some_and(|form| form.notes.focus_handle(cx).is_focused(window));
                if event.keystroke.key == "enter"
                    && !event.keystroke.modifiers.modified()
                    && !in_notes
                {
                    this.save_form(cx);
                }
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(SPACE_4)
                    .max_w(px(640.))
                    .px(SPACE_6)
                    .py(SPACE_6)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(SPACE_2)
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(TEXT_XL)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(if is_new {
                                        "New contact"
                                    } else {
                                        "Edit contact"
                                    }),
                            )
                            .child(
                                Button::new("form-cancel")
                                    .ghost()
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| this.cancel_form(cx))),
                            )
                            .child(
                                Button::new("form-save")
                                    .primary()
                                    .icon(IconName::Check)
                                    .label("Save")
                                    .on_click(cx.listener(|this, _, _, cx| this.save_form(cx))),
                            ),
                    )
                    .when_some(form.error.clone(), |el, error| {
                        el.child(Banner::error("form-error", error))
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(SPACE_3)
                            .child(portrait)
                            .child(
                                Button::new("form-photo")
                                    .icon(IconName::Camera)
                                    .label(if form.photo.is_some() {
                                        "Change photo"
                                    } else {
                                        "Add photo"
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| this.pick_photo(cx))),
                            )
                            .when(form.photo.is_some(), |el| {
                                el.child(
                                    Button::new("form-photo-remove")
                                        .ghost()
                                        .label("Remove")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            if let Some(form) = this.form.as_mut() {
                                                form.photo = None;
                                            }
                                            cx.notify();
                                        })),
                                )
                            })
                            .child(div().flex_1())
                            .child(
                                chip("form-favorite", "Favorite", form.favorite, t).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        if let Some(form) = this.form.as_mut() {
                                            form.favorite = !form.favorite;
                                        }
                                        cx.notify();
                                    }),
                                ),
                            ),
                    )
                    .child(pair(
                        labeled("First name", Input::new(&form.first_name), t),
                        labeled("Last name", Input::new(&form.last_name), t),
                    ))
                    .child(pair(
                        labeled("Display name", Input::new(&form.display_name), t),
                        labeled("Nickname", Input::new(&form.nickname), t),
                    ))
                    .child(pair(
                        labeled("Company", Input::new(&form.company), t),
                        labeled("Job title", Input::new(&form.job_title), t),
                    ))
                    .child(labeled(
                        "Email",
                        div()
                            .flex()
                            .flex_col()
                            .gap(SPACE_2)
                            .children(email_rows)
                            .child(add_button(
                                "add-email",
                                "Add email",
                                cx.listener(|this, _, window, cx| {
                                    let row =
                                        LabeledInput::new("", "", EMAIL_PLACEHOLDER, window, cx);
                                    row.value.update(cx, |input, cx| input.focus(window, cx));
                                    if let Some(form) = this.form.as_mut() {
                                        form.emails.push(row);
                                    }
                                    cx.notify();
                                }),
                            )),
                        t,
                    ))
                    .child(labeled(
                        "Phone",
                        div()
                            .flex()
                            .flex_col()
                            .gap(SPACE_2)
                            .children(phone_rows)
                            .child(add_button(
                                "add-phone",
                                "Add phone",
                                cx.listener(|this, _, window, cx| {
                                    let row =
                                        LabeledInput::new("", "", PHONE_PLACEHOLDER, window, cx);
                                    row.value.update(cx, |input, cx| input.focus(window, cx));
                                    if let Some(form) = this.form.as_mut() {
                                        form.phones.push(row);
                                    }
                                    cx.notify();
                                }),
                            )),
                        t,
                    ))
                    .child(labeled(
                        "Addresses",
                        div()
                            .flex()
                            .flex_col()
                            .gap(SPACE_2)
                            .children(address_rows)
                            .child(add_button(
                                "add-address",
                                "Add address",
                                cx.listener(|this, _, window, cx| {
                                    let fields =
                                        AddressFields::new(&PostalAddress::default(), window, cx);
                                    if let Some(form) = this.form.as_mut() {
                                        form.addresses.push(fields);
                                    }
                                    cx.notify();
                                }),
                            )),
                        t,
                    ))
                    .child(pair(
                        labeled("Birthday", Input::new(&form.birthday), t),
                        labeled("Website", Input::new(&form.website), t),
                    ))
                    .when(has_categories, |el| {
                        el.child(labeled(
                            "Categories",
                            div()
                                .flex()
                                .flex_wrap()
                                .gap(SPACE_1)
                                .children(category_chips),
                            t,
                        ))
                    })
                    .child(labeled("Notes", Textarea::new(&form.notes).h(px(110.)), t))
                    .child(
                        div()
                            .text_size(TEXT_XS)
                            .text_color(t.text_subtle)
                            .child("Enter saves - Esc cancels"),
                    ),
            )
            .into_any_element()
    }

    fn pick_photo(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose a photo".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let mime = path
                .extension()
                .and_then(|extension| extension.to_str())
                .and_then(vcard::image_mime);
            let result = match mime {
                None => Err("Choose a PNG, JPEG, GIF, WebP or BMP image.".to_owned()),
                Some(mime) => match std::fs::metadata(&path) {
                    Ok(meta) if meta.len() > MAX_PHOTO_BYTES => {
                        Err("That photo is larger than 1 MB.".to_owned())
                    }
                    Ok(_) => std::fs::read(&path)
                        .map(|bytes| vcard::photo_data_uri(mime, &bytes))
                        .map_err(|err| format!("Could not read the photo: {err}")),
                    Err(err) => Err(format!("Could not read the photo: {err}")),
                },
            };
            let _ = this.update(cx, |this, cx| {
                if let Some(form) = this.form.as_mut() {
                    match result {
                        Ok(uri) => {
                            form.photo = Some(uri);
                            form.error = None;
                        }
                        Err(message) => form.error = Some(message.into()),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

const EMAIL_PLACEHOLDER: &str = "name@example.com";
const PHONE_PLACEHOLDER: &str = "+1 555 0100";

/// Single-line field; Enter saves the form.
fn text_input(
    placeholder: &'static str,
    initial: &str,
    window: &mut Window,
    cx: &mut Context<PeopleView>,
) -> Entity<InputState> {
    let initial = initial.to_owned();
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(placeholder)
            .default_value(initial)
    });
    // Detached: ends when the input is dropped with the form.
    cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
        if let InputEvent::PressEnter { .. } = event {
            this.save_form(cx);
        }
    })
    .detach();
    input
}

/// At least 3 digits; otherwise only spaces, `+ ( ) - . / #` and an "ext"/"x" extension.
fn is_phone(text: &str) -> bool {
    let digits = text.chars().filter(char::is_ascii_digit).count();
    let rest = text.to_lowercase().replace("ext", "").replace('x', "");
    digits >= 3
        && rest
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_whitespace() || "+()-./#".contains(c))
}

fn value(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().trim().to_owned()
}

/// `1990-12-31`, `31/12/1990` or `31.12.1990`.
fn parse_birthday(text: &str) -> Option<NaiveDate> {
    ["%Y-%m-%d", "%d/%m/%Y", "%d.%m.%Y"]
        .into_iter()
        .find_map(|format| NaiveDate::parse_from_str(text, format).ok())
}

fn labeled(label: &'static str, field: impl IntoElement, t: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w(px(0.))
        .gap(SPACE_1)
        .child(
            div()
                .text_size(TEXT_XS)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text_subtle)
                .child(label.to_uppercase()),
        )
        .child(field)
}

fn pair(left: impl IntoElement, right: impl IntoElement) -> Div {
    div()
        .flex()
        .gap(SPACE_3)
        .child(div().flex_1().min_w(px(0.)).child(left))
        .child(div().flex_1().min_w(px(0.)).child(right))
}

/// Label + value inputs with a trailing remove button.
fn labeled_row(
    row: &LabeledInput,
    id: impl Into<ElementId>,
    on_remove: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap(SPACE_1)
        .child(
            div()
                .w(px(96.))
                .flex_shrink_0()
                .child(Input::new(&row.label)),
        )
        .child(div().flex_1().min_w(px(0.)).child(Input::new(&row.value)))
        .child(
            IconButton::new(id, IconName::X)
                .icon_size(ICON_SIZE_SM)
                .tooltip("Remove")
                .on_click(on_remove),
        )
}

/// Input with a trailing remove button.
fn removable(
    input: &Entity<InputState>,
    id: impl Into<ElementId>,
    on_remove: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap(SPACE_1)
        .child(div().flex_1().min_w(px(0.)).child(Input::new(input)))
        .child(
            IconButton::new(id, IconName::X)
                .icon_size(ICON_SIZE_SM)
                .tooltip("Remove")
                .on_click(on_remove),
        )
}

fn add_button(
    id: &'static str,
    label: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div().flex().child(
        Button::new(id)
            .ghost()
            .compact()
            .icon(IconName::Plus)
            .label(label)
            .on_click(on_click),
    )
}

#[cfg(test)]
mod tests {
    use super::{is_phone, parse_birthday};
    use chrono::NaiveDate;

    #[test]
    fn phone_validation() {
        assert!(is_phone("+33 6 12 34 56 78"));
        assert!(is_phone("(555) 010-0100 ext 12"));
        assert!(!is_phone("call me"));
        assert!(!is_phone("12"));
    }

    #[test]
    fn birthday_formats() {
        let expected = NaiveDate::from_ymd_opt(1990, 12, 31);
        assert_eq!(parse_birthday("1990-12-31"), expected);
        assert_eq!(parse_birthday("31/12/1990"), expected);
        assert_eq!(parse_birthday("31.12.1990"), expected);
        assert_eq!(parse_birthday("12/31/1990"), None);
    }
}
