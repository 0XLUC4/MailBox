//! Recipient field: address chips, typed text, autocomplete (contacts, then past correspondents).

use gpui::component::input::{Input, InputEvent, InputState};
use gpui::component::tooltip::Tooltip;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::compose::{self, Resolution};
use mailbox_core::{Address, compose::display_recipient};

use crate::app_state::AppState;
use crate::components::{Avatar, IconButton, IconName};
use crate::settings::tr;
use crate::theme::{
    ICON_SIZE_XS, RADIUS_FULL, RADIUS_LG, RADIUS_MD, SPACE_0_5, SPACE_1, SPACE_2, TEXT_SM, TEXT_XS,
    theme,
};

const MAX_SUGGESTIONS: usize = 6;

/// Emitted when the committed recipients change.
pub enum RecipientEvent {
    Changed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChipState {
    Valid,
    /// Not an email address and not resolved by Check Names.
    Invalid,
    /// Check Names found several matches.
    Ambiguous,
}

struct Chip {
    address: Address,
    state: ChipState,
}

impl Chip {
    fn new(address: Address) -> Self {
        let state = if compose::is_valid_email(&address.email) {
            ChipState::Valid
        } else {
            ChipState::Invalid
        };
        Self { address, state }
    }
}

pub struct RecipientField {
    state: Entity<AppState>,
    input: Entity<InputState>,
    chips: Vec<Chip>,
    suggestions: Vec<Address>,
    highlighted: usize,
    _subscription: Subscription,
}

impl EventEmitter<RecipientEvent> for RecipientField {}

impl RecipientField {
    pub fn new(
        state: Entity<AppState>,
        addresses: Vec<Address>,
        placeholder: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subscription = cx.subscribe_in(&input, window, Self::on_input_event);
        Self {
            state,
            input,
            chips: addresses.into_iter().map(Chip::new).collect(),
            suggestions: Vec::new(),
            highlighted: 0,
            _subscription: subscription,
        }
    }

    /// Committed chips plus any text still being typed (so autosave never loses it).
    pub fn addresses(&self, cx: &App) -> Vec<Address> {
        let pending = compose::parse_recipients(&self.input.read(cx).value());
        self.chips
            .iter()
            .map(|chip| chip.address.clone())
            .chain(pending)
            .collect()
    }

    /// Entries that are not valid addresses (shown red / amber).
    pub fn problems(&self) -> Vec<String> {
        self.chips
            .iter()
            .filter(|chip| chip.state != ChipState::Valid)
            .map(|chip| display_recipient(&chip.address))
            .collect()
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Turns the typed text into chips (before sending or checking names).
    pub fn commit_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        self.commit_text(&text, window, cx);
    }

    /// Resolves every entry against contacts and history. Returns the number left unresolved.
    pub fn check_names(&mut self, cx: &mut Context<Self>) -> usize {
        let mut unresolved = 0;
        {
            let store = self.state.read(cx).store();
            for chip in &mut self.chips {
                match compose::resolve_recipient(store, &chip.address) {
                    Ok(Resolution::Resolved(address)) => *chip = Chip::new(address),
                    Ok(Resolution::Ambiguous(_)) => {
                        chip.state = ChipState::Ambiguous;
                        unresolved += 1;
                    }
                    Ok(Resolution::NotFound) | Err(_) => {
                        chip.state = ChipState::Invalid;
                        unresolved += 1;
                    }
                }
            }
        }
        cx.emit(RecipientEvent::Changed);
        cx.notify();
        unresolved
    }

    fn on_input_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = input.read(cx).value().to_string();
        match event {
            InputEvent::Change => {
                if text.contains([',', ';']) {
                    self.commit_text(&text, window, cx);
                } else {
                    self.refresh_suggestions(&text, cx);
                }
            }
            InputEvent::PressEnter {
                secondary: false, ..
            } => match self.suggestions.get(self.highlighted).cloned() {
                Some(address) => self.pick(address, window, cx),
                None => self.commit_text(&text, window, cx),
            },
            InputEvent::Blur => {
                self.commit_text(&text, window, cx);
                self.close_suggestions(cx);
            }
            _ => {}
        }
    }

    fn refresh_suggestions(&mut self, text: &str, cx: &mut Context<Self>) {
        let taken: Vec<String> = self
            .chips
            .iter()
            .map(|chip| chip.address.email.to_lowercase())
            .collect();
        let found = compose::suggest_recipients(
            self.state.read(cx).store(),
            text,
            MAX_SUGGESTIONS + taken.len(),
        )
        .unwrap_or_default();
        self.suggestions = found
            .into_iter()
            // Past drafts may hold half-typed names: only offer real addresses.
            .filter(|address| {
                compose::is_valid_email(&address.email)
                    && !taken.contains(&address.email.to_lowercase())
            })
            .take(MAX_SUGGESTIONS)
            .collect();
        self.highlighted = 0;
        cx.notify();
    }

    fn close_suggestions(&mut self, cx: &mut Context<Self>) {
        if !self.suggestions.is_empty() {
            self.suggestions.clear();
            cx.notify();
        }
    }

    fn clear_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
    }

    fn commit_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let parsed = compose::parse_recipients(text);
        if parsed.is_empty() {
            if !text.is_empty() {
                self.clear_input(window, cx);
            }
            return;
        }
        for address in parsed {
            self.push(address);
        }
        self.clear_input(window, cx);
        self.close_suggestions(cx);
        cx.emit(RecipientEvent::Changed);
        cx.notify();
    }

    fn push(&mut self, address: Address) {
        let duplicate = self
            .chips
            .iter()
            .any(|chip| chip.address.email.eq_ignore_ascii_case(&address.email));
        if !duplicate {
            self.chips.push(Chip::new(address));
        }
    }

    fn pick(&mut self, address: Address, window: &mut Window, cx: &mut Context<Self>) {
        self.push(address);
        self.clear_input(window, cx);
        self.close_suggestions(cx);
        self.focus(window, cx);
        cx.emit(RecipientEvent::Changed);
        cx.notify();
    }

    fn remove(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.chips.len() {
            self.chips.remove(index);
            cx.emit(RecipientEvent::Changed);
            cx.notify();
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let open = !self.suggestions.is_empty();
        match event.keystroke.key.as_str() {
            "down" if open => {
                self.highlighted = (self.highlighted + 1) % self.suggestions.len();
            }
            "up" if open => {
                let count = self.suggestions.len();
                self.highlighted = (self.highlighted + count - 1) % count;
            }
            "tab" if open => {
                if let Some(address) = self.suggestions.get(self.highlighted).cloned() {
                    self.pick(address, window, cx);
                }
            }
            "escape" if open => self.suggestions.clear(),
            "backspace" if self.input.read(cx).value().is_empty() && !self.chips.is_empty() => {
                self.chips.pop();
                cx.emit(RecipientEvent::Changed);
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn render_chip(&self, index: usize, chip: &Chip, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let (bg, fg) = match chip.state {
            ChipState::Valid => (t.active, t.text),
            ChipState::Invalid => (t.danger.alpha(0.14), t.danger),
            ChipState::Ambiguous => (t.warning.alpha(0.16), t.warning),
        };
        let tooltip: SharedString = match chip.state {
            ChipState::Valid => chip.address.email.clone().into(),
            ChipState::Invalid => tr(cx, "“{}” is not a valid address")
                .replace("{}", &chip.address.email)
                .into(),
            ChipState::Ambiguous => tr(cx, "Several people match: type more of the name").into(),
        };
        div()
            .id(("recipient-chip", index as u64))
            .flex()
            .flex_row()
            .flex_shrink_0()
            .items_center()
            .gap(SPACE_1)
            .h(px(24.))
            .pl(SPACE_0_5)
            .pr(SPACE_0_5)
            .rounded(RADIUS_FULL)
            .bg(bg)
            .text_color(fg)
            .text_size(TEXT_SM)
            .child(Avatar::new(chip.address.display().to_owned()).size(px(20.)))
            .child(chip.address.display().to_owned())
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .child(
                IconButton::new(("recipient-remove", index as u64), IconName::X)
                    .size(px(20.))
                    .icon_size(ICON_SIZE_XS)
                    .tooltip(tr(cx, "Remove"))
                    .on_click(cx.listener(move |this, _, _, cx| this.remove(index, cx))),
            )
    }

    fn render_suggestions(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let (hover, selected) = (t.hover, t.accent_subtle);
        let items = self
            .suggestions
            .iter()
            .enumerate()
            .map(|(index, address)| {
                let picked = address.clone();
                div()
                    .id(("recipient-suggestion", index as u64))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(SPACE_2)
                    .px(SPACE_2)
                    .py(SPACE_1)
                    .rounded(RADIUS_MD)
                    .cursor_pointer()
                    .when(index == self.highlighted, |el| el.bg(selected))
                    .hover(move |style| style.bg(hover))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.pick(picked.clone(), window, cx);
                        }),
                    )
                    .child(Avatar::new(address.display().to_owned()).size(px(24.)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .text_size(TEXT_SM)
                                    .truncate()
                                    .child(address.display().to_owned()),
                            )
                            .when(!address.name.is_empty(), |el| {
                                el.child(
                                    div()
                                        .text_size(TEXT_XS)
                                        .text_color(t.text_muted)
                                        .truncate()
                                        .child(address.email.clone()),
                                )
                            }),
                    )
            })
            .collect::<Vec<_>>();
        deferred(
            anchored().snap_to_window().child(
                div()
                    .occlude()
                    .mt(SPACE_1)
                    .w(px(320.))
                    .p(SPACE_1)
                    .flex()
                    .flex_col()
                    .gap(SPACE_0_5)
                    .rounded(RADIUS_LG)
                    .border_1()
                    .border_color(t.border)
                    .bg(t.elevated)
                    .shadow_lg()
                    .children(items),
            ),
        )
        .with_priority(1)
    }
}

impl Render for RecipientField {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let chips = self
            .chips
            .iter()
            .enumerate()
            .map(|(index, chip)| self.render_chip(index, chip, cx).into_any_element())
            .collect::<Vec<_>>();
        let suggestions =
            (!self.suggestions.is_empty()).then(|| self.render_suggestions(cx).into_any_element());
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .capture_key_down(cx.listener(Self::on_key_down))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(SPACE_1)
                    .py(SPACE_1)
                    .children(chips)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(140.))
                            .child(Input::new(&self.input).appearance(false)),
                    ),
            )
            // `anchored` is absolutely positioned at its parent's origin: a zero-height
            // wrapper below the input row puts the list under the field instead of over it.
            .when_some(suggestions, |el, suggestions| {
                el.child(div().h_0().child(suggestions))
            })
    }
}
