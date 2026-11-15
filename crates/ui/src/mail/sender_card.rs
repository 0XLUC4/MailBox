//! Contact card: a click on a name in the reading pane (sender, recipients) opens a small card,
//! like Outlook's, to write to the person, save them as a contact or copy their address.

use gpui::base::Selectable;
use gpui::component::popover::Popover;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::people;
use mailbox_core::{Address, Command, Id, PeopleCommand};

use crate::app_state::AppState;
use crate::components::{Avatar, Button, Icon, IconButton, IconName};
use crate::compose::{ComposeRequest, open_compose};
use crate::settings::tr;
use crate::theme::{SPACE_0_5, SPACE_2, SPACE_3, TEXT_LG, TEXT_SM, theme};

/// `address` as a clickable name opening its card (on `background`); `bold` for the sender.
pub fn name(
    state: &Entity<AppState>,
    address: &Address,
    account_id: Id,
    bold: bool,
    id: impl Into<ElementId>,
    background: Hsla,
) -> impl IntoElement {
    let state = state.clone();
    let card_address = address.clone();
    Popover::new(id)
        .anchor(Anchor::TopLeft)
        .w(px(320.))
        .bg(background)
        .trigger(NameTrigger {
            label: address.display().to_owned().into(),
            bold,
            selected: false,
        })
        .content(move |_, _, cx| card(&state, &card_address, account_id, cx))
}

fn card(
    state: &Entity<AppState>,
    address: &Address,
    account_id: Id,
    cx: &mut Context<gpui::base::PopoverState>,
) -> AnyElement {
    let t = theme(cx).clone();
    let popover = cx.entity();
    let saved = people::contact_with_email(state.read(cx).store(), &address.email)
        .ok()
        .flatten();
    let detail = saved.as_ref().map(|contact| {
        [contact.job_title.clone(), contact.company.clone()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" - ")
    });

    let mail_state = state.clone();
    let mail_to = address.email.clone();
    let mail_popover = popover.clone();
    let write = Button::new("card-write")
        .primary()
        .icon(IconName::Mail)
        .label(tr(cx, "New email"))
        .on_click(move |_, window, cx| {
            mail_popover.update(cx, |popover, cx| popover.dismiss(window, cx));
            let request = ComposeRequest::Mailto(format!("mailto:{mail_to}"));
            open_compose(&mail_state, request, window, cx);
        });

    let save = if saved.is_some() {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(SPACE_0_5)
            .text_size(TEXT_SM)
            .text_color(t.text_muted)
            .child(Icon::new(IconName::Check).size(px(14.)))
            .child(tr(cx, "In your contacts"))
            .into_any_element()
    } else {
        let save_state = state.clone();
        let save_address = address.clone();
        let save_popover = popover.clone();
        Button::new("card-save")
            .icon(IconName::UserPlus)
            .label(tr(cx, "Add to contacts"))
            .on_click(move |_, window, cx| {
                save_popover.update(cx, |popover, cx| popover.dismiss(window, cx));
                let card = people::card_from_address(&save_address, account_id);
                let command = Command::People(PeopleCommand::SaveContact(Box::new(card)));
                let label = save_address.display().to_owned();
                save_state.update(cx, |state, cx| {
                    if state.run(command, cx) {
                        state.notify_info(format!("Saved {label} to contacts"), cx);
                    }
                });
            })
            .into_any_element()
    };

    let copy_state = state.clone();
    let copy_email = address.email.clone();
    let copy = IconButton::new("card-copy", IconName::Copy)
        .tooltip(tr(cx, "Copy address"))
        .on_click(move |_, window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy_email.clone()));
            popover.update(cx, |popover, cx| popover.dismiss(window, cx));
            copy_state.update(cx, |state, cx| state.notify_info("Address copied", cx));
        });

    div()
        .flex()
        .flex_col()
        .gap(SPACE_3)
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(SPACE_3)
                .child(Avatar::new(address.display().to_owned()).size(px(44.)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(px(0.))
                        .gap(SPACE_0_5)
                        .child(
                            div()
                                .truncate()
                                .text_size(TEXT_LG)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(t.text)
                                .child(address.display().to_owned()),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(TEXT_SM)
                                .text_color(t.text_muted)
                                .child(address.email.clone()),
                        )
                        .when_some(detail.filter(|d| !d.is_empty()), |el, detail| {
                            el.child(
                                div()
                                    .truncate()
                                    .text_size(TEXT_SM)
                                    .text_color(t.text_subtle)
                                    .child(detail),
                            )
                        }),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(SPACE_2)
                .child(write)
                .child(save)
                .child(div().flex_1())
                .child(copy),
        )
        .into_any_element()
}

/// A name that looks like text and underlines on hover.
#[derive(IntoElement)]
struct NameTrigger {
    label: SharedString,
    bold: bool,
    selected: bool,
}

impl Selectable for NameTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl RenderOnce for NameTrigger {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx).clone();
        div()
            .cursor_pointer()
            .when(self.bold, |el| el.font_weight(FontWeight::SEMIBOLD))
            .when(self.selected, |el| el.text_color(t.accent))
            .hover(|style| style.underline())
            .child(self.label)
    }
}
