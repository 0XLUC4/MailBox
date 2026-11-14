//! Contact editor: every field of a [`ContactCard`], with add/remove rows for emails, phones
//! and addresses, a photo picker, categories and notes. Saved as one undoable command.

use chrono::NaiveDate;
use gpui::component::input::{Input, InputState, Textarea, TextareaState};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::people::{ContactCard, PostalAddress};
use mailbox_core::vcard;

use super::PeopleView;
use super::card::chip;
use crate::components::{Avatar, Button, IconButton, IconName};
use crate::theme::{
    ICON_SIZE_SM, RADIUS_FULL, RADIUS_LG, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_6, TEXT_SM,
    TEXT_XL, TEXT_XS, Theme,
};

/// Largest photo accepted (stored inline in the database and in exported vCards).
const MAX_PHOTO_BYTES: u64 = 1024 * 1024;

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
    emails: Vec<Entity<InputState>>,
    phones: Vec<Entity<InputState>>,
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
                    placeholder: &'static str,
                    window: &mut Window,
                    cx: &mut Context<PeopleView>| {
            let mut inputs: Vec<Entity<InputState>> = values
                .iter()
                .map(|value| text_input(placeholder, value, window, cx))
                .collect();
            if inputs.is_empty() {
                inputs.push(text_input(placeholder, "", window, cx));
            }
            inputs
        };
        let notes_text = contact.notes.clone();
        let notes = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Notes")
                .default_value(notes_text)
        });
        let first_name = text_input("First name", &details.first_name, window, cx);
        // Focus the first field so typing starts right away.
        first_name.update(cx, |input, cx| input.focus(window, cx));
        Self {
            display_name: text_input("Shown as (optional)", &contact.display_name, window, cx),
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
            emails: list(contact.emails.as_slice(), "name@example.com", window, cx),
            phones: list(contact.phones.as_slice(), "+1 555 0100", window, cx),
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
        let emails: Vec<String> = self
            .emails
            .iter()
            .map(|input| value(input, cx))
            .filter(|email| !email.is_empty())
            .collect();
        if let Some(invalid) = emails.iter().find(|email| !email.contains('@')) {
            return Err(format!("“{invalid}” is not an email address."));
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
        contact.phones = self
            .phones
            .iter()
            .map(|input| value(input, cx))
            .filter(|phone| !phone.is_empty())
            .collect();

        let details = &mut card.details;
        details.first_name = value(&self.first_name, cx);
        details.last_name = value(&self.last_name, cx);
        details.nickname = value(&self.nickname, cx);
        details.website = value(&self.website, cx);
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

        let email_rows = form.emails.iter().enumerate().map(|(index, input)| {
            removable(
                input,
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
        let phone_rows = form.phones.iter().enumerate().map(|(index, input)| {
            removable(
                input,
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
            .size_full()
            .overflow_y_scroll()
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
                            .child(Button::new("form-cancel").ghost().label("Cancel").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.form = None;
                                    cx.notify();
                                }),
                            ))
                            .child(
                                Button::new("form-save")
                                    .primary()
                                    .icon(IconName::Check)
                                    .label("Save")
                                    .on_click(cx.listener(|this, _, _, cx| this.save_form(cx))),
                            ),
                    )
                    .when_some(form.error.clone(), |el, error| {
                        el.child(
                            div()
                                .px(SPACE_3)
                                .py(SPACE_2)
                                .rounded(RADIUS_LG)
                                .bg(t.danger.alpha(0.12))
                                .text_color(t.danger)
                                .text_size(TEXT_SM)
                                .child(error),
                        )
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
                                    let input = text_input("name@example.com", "", window, cx);
                                    if let Some(form) = this.form.as_mut() {
                                        form.emails.push(input);
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
                                    let input = text_input("+1 555 0100", "", window, cx);
                                    if let Some(form) = this.form.as_mut() {
                                        form.phones.push(input);
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
                    .child(labeled(
                        "Categories",
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(SPACE_1)
                            .children(category_chips),
                        t,
                    ))
                    .child(labeled("Notes", Textarea::new(&form.notes).h(px(110.)), t)),
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

fn text_input(
    placeholder: &'static str,
    initial: &str,
    window: &mut Window,
    cx: &mut Context<PeopleView>,
) -> Entity<InputState> {
    let initial = initial.to_owned();
    cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(placeholder)
            .default_value(initial)
    })
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
    div().child(
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
    use super::parse_birthday;
    use chrono::NaiveDate;

    #[test]
    fn birthday_formats() {
        let expected = NaiveDate::from_ymd_opt(1990, 12, 31);
        assert_eq!(parse_birthday("1990-12-31"), expected);
        assert_eq!(parse_birthday("31/12/1990"), expected);
        assert_eq!(parse_birthday("31.12.1990"), expected);
        assert_eq!(parse_birthday("12/31/1990"), None);
    }
}
