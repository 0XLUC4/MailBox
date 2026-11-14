//! Ribbon: tabs (Home, Send / Receive, View, Help) whose groups follow the active module.
//!
//! Two layouts (setting `ribbon`): *simplified*, one row of small buttons, and *classic*, large
//! icon-over-label buttons in captioned groups. Groups collapse to icon-only (lowest priority
//! first) and then into a "more" menu when the window is too narrow.
//! Every button dispatches a GPUI action from [`crate::actions`]; modules handle them.

use gpui::component::button::{Button as MenuButton, ButtonVariants as _};
use gpui::component::menu::DropdownMenu as _;
use gpui::component::tooltip::Tooltip;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::settings::RibbonMode;

use crate::actions::{ActionSpec, ToggleRibbonMode, is_checked, is_enabled, spec};
use crate::app_state::{AppState, Module};
use crate::components::{Icon, IconName};
use crate::settings::{prefs, tr};
use crate::theme::{
    ICON_SIZE_SM, RADIUS_MD, RAIL_WIDTH, SPACE_0_5, SPACE_1, SPACE_2, SPACE_3, TEXT_SM, TEXT_XS,
    theme,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Home,
    SendReceive,
    View,
    Help,
}

impl Tab {
    const ALL: [Tab; 4] = [Self::Home, Self::SendReceive, Self::View, Self::Help];

    fn label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::SendReceive => "Send / Receive",
            Self::View => "View",
            Self::Help => "Help",
        }
    }
}

/// A captioned set of buttons. Higher `priority` stays expanded longer.
struct Group {
    name: &'static str,
    priority: u8,
    items: &'static [&'static str],
}

const fn group(name: &'static str, priority: u8, items: &'static [&'static str]) -> Group {
    Group {
        name,
        priority,
        items,
    }
}

const UNDO: Group = group("Undo", 6, &["undo", "redo"]);
const SYNC: Group = group("Sync", 9, &["send-receive"]);
const ACCOUNTS: Group = group("Accounts", 5, &["add-account", "account-settings"]);
const THEME: Group = group("Theme", 4, &["dark-mode", "density"]);
const RIBBON: Group = group("Ribbon", 3, &["ribbon-mode"]);
const SUPPORT: Group = group("Support", 9, &["palette", "search", "shortcuts"]);
const ABOUT: Group = group("About", 5, &["about"]);
const CALENDAR_ARRANGE: Group = group("Arrange", 7, &["day-view", "week-view", "month-view"]);

const MAIL_HOME: &[Group] = &[
    group("New", 10, &["new-mail"]),
    group("Delete", 8, &["delete", "archive"]),
    group("Respond", 9, &["reply", "reply-all", "forward"]),
    group("Move", 5, &["move", "rules", "run-rules"]),
    group(
        "Tags",
        7,
        &["categorize", "flag", "mark-read", "mark-unread"],
    ),
    group("Later", 6, &["snooze", "mute"]),
    group("Message", 4, &["print", "save-eml", "save-attachments"]),
    UNDO,
];
const CALENDAR_HOME: &[Group] = &[
    group("New", 10, &["new-event"]),
    group("Go to", 8, &["today"]),
    CALENDAR_ARRANGE,
    UNDO,
];
const PEOPLE_HOME: &[Group] = &[
    group("New", 10, &["new-contact"]),
    group("Delete", 8, &["delete"]),
    UNDO,
];
const TASKS_HOME: &[Group] = &[
    group("New", 10, &["new-task"]),
    group("Manage", 8, &["complete-task", "delete"]),
    UNDO,
];
const SETTINGS_HOME: &[Group] = &[
    ACCOUNTS,
    group("Rules", 7, &["rules"]),
    group("Automatic", 6, &["auto-reply", "read-receipts"]),
    group("System", 4, &["default-mail-app"]),
    UNDO,
];
const MAIL_SEND_RECEIVE: &[Group] = &[
    SYNC,
    group("Folder", 6, &["mark-all-read"]),
    group(
        "Mailbox",
        5,
        &["export-mailbox", "import-mailbox", "auto-reply"],
    ),
    ACCOUNTS,
];
const SEND_RECEIVE: &[Group] = &[SYNC, ACCOUNTS];
const MAIL_VIEW: &[Group] = &[
    group("Layout", 8, &["reading-pane", "conversations"]),
    THEME,
    RIBBON,
];
const CALENDAR_VIEW: &[Group] = &[CALENDAR_ARRANGE, THEME, RIBBON];
const VIEW: &[Group] = &[THEME, RIBBON];
const HELP: &[Group] = &[SUPPORT, ABOUT];

fn groups(module: Module, tab: Tab) -> &'static [Group] {
    match (tab, module) {
        (Tab::Home, Module::Mail) => MAIL_HOME,
        (Tab::Home, Module::Calendar) => CALENDAR_HOME,
        (Tab::Home, Module::People) => PEOPLE_HOME,
        (Tab::Home, Module::Tasks) => TASKS_HOME,
        (Tab::Home, Module::Settings) => SETTINGS_HOME,
        (Tab::SendReceive, Module::Mail) => MAIL_SEND_RECEIVE,
        (Tab::SendReceive, _) => SEND_RECEIVE,
        (Tab::View, Module::Mail) => MAIL_VIEW,
        (Tab::View, Module::Calendar) => CALENDAR_VIEW,
        (Tab::View, _) => VIEW,
        (Tab::Help, _) => HELP,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fit {
    Full,
    /// Simplified: icon only. Classic: small buttons in rows of three.
    Compact,
    /// In the "more" menu.
    Hidden,
}

/// Approximate rendered width of a group (no text measurement needed at this precision).
fn group_width(group: &Group, fit: Fit, classic: bool, cx: &App) -> f32 {
    let label_width =
        |id: &str| spec(id).map_or(0., |spec| tr(cx, spec.label).chars().count() as f32 * 6.6);
    let count = group.items.len() as f32;
    let separator = 9.;
    match (fit, classic) {
        (Fit::Hidden, _) => 0.,
        (Fit::Compact, false) => count * 28. + (count - 1.) * 4. + separator,
        (Fit::Full, false) => {
            group
                .items
                .iter()
                .map(|id| label_width(id) + 36.)
                .sum::<f32>()
                + (count - 1.) * 4.
                + separator
        }
        (Fit::Full, true) => {
            group
                .items
                .iter()
                .map(|id| (label_width(id) * 0.9 + 16.).max(56.))
                .sum::<f32>()
                + separator
                + 8.
        }
        (Fit::Compact, true) => {
            let widest = group
                .items
                .iter()
                .map(|id| label_width(id) + 36.)
                .fold(0., f32::max);
            (count / 3.).ceil() * widest + separator + 8.
        }
    }
}

/// Collapses groups (lowest priority first) until they fit in `available` px.
fn fit_groups(groups: &[Group], available: f32, classic: bool, cx: &App) -> Vec<Fit> {
    let mut fits = vec![Fit::Full; groups.len()];
    let mut order: Vec<usize> = (0..groups.len()).collect();
    order.sort_by_key(|&index| groups[index].priority);
    let total = |fits: &[Fit]| -> f32 {
        groups
            .iter()
            .zip(fits)
            .map(|(group, fit)| group_width(group, *fit, classic, cx))
            .sum()
    };
    for step in [Fit::Compact, Fit::Hidden] {
        for &index in &order {
            if total(&fits) <= available {
                return fits;
            }
            fits[index] = step;
        }
    }
    fits
}

/// Dispatches from the focused element, or from the ribbon when nothing has focus,
/// so the shell handlers always receive it.
fn dispatch(action: Box<dyn Action>, ribbon: &FocusHandle, window: &mut Window, cx: &mut App) {
    if window.focused(cx).is_some() {
        window.dispatch_action(action, cx);
    } else {
        ribbon.dispatch_action(action.as_ref(), window, cx);
    }
}

pub struct RibbonView {
    state: Entity<AppState>,
    tab: Tab,
    focus_handle: FocusHandle,
    _subscription: Subscription,
}

impl RibbonView {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&state, |_, _, cx| cx.notify());
        Self {
            state,
            tab: Tab::Home,
            focus_handle: cx.focus_handle(),
            _subscription: subscription,
        }
    }

    fn tooltip_text(spec: &ActionSpec, cx: &App) -> SharedString {
        match spec.shortcut_text() {
            Some(keys) => format!("{}  ({keys})", tr(cx, spec.label)).into(),
            None => tr(cx, spec.label).into(),
        }
    }

    /// One ribbon button in the given style.
    fn button(&self, spec: &'static ActionSpec, fit: Fit, classic: bool, cx: &App) -> AnyElement {
        let t = theme(cx).clone();
        let enabled = is_enabled(spec.id, self.state.read(cx));
        let checked = is_checked(spec.id, cx);
        let label = tr(cx, spec.label);
        let tooltip = Self::tooltip_text(spec, cx);
        let focus = self.focus_handle.clone();
        let (fg, bg) = if checked {
            (t.accent, t.accent_subtle)
        } else {
            (t.text, t.background.alpha(0.))
        };
        let large = classic && fit == Fit::Full;
        let icon_only = !classic && fit == Fit::Compact;

        div()
            .id(SharedString::from(format!("ribbon-{}", spec.id)))
            .flex()
            .flex_shrink_0()
            .items_center()
            .rounded(RADIUS_MD)
            .bg(bg)
            .text_color(fg)
            .map(|el| {
                if large {
                    el.flex_col()
                        .justify_center()
                        .gap(SPACE_1)
                        .min_w(px(56.))
                        .h(px(62.))
                        .px(SPACE_1)
                        .child(Icon::new(spec.icon).size(px(22.)))
                        .child(
                            div()
                                .text_size(TEXT_XS)
                                .text_center()
                                .line_height(px(13.))
                                .child(label),
                        )
                } else if icon_only {
                    el.justify_center()
                        .size(px(28.))
                        .child(Icon::new(spec.icon).size(ICON_SIZE_SM))
                } else {
                    el.flex_row()
                        .gap(SPACE_1)
                        .h(px(if classic { 20. } else { 28. }))
                        .px(SPACE_2)
                        .text_size(TEXT_SM)
                        .child(Icon::new(spec.icon).size(ICON_SIZE_SM))
                        .child(label)
                }
            })
            .when(!enabled, |el| el.opacity(0.4))
            .when(enabled, |el| {
                el.cursor_pointer()
                    .hover(move |style| style.bg(if checked { t.accent_subtle } else { t.hover }))
                    .active(move |style| style.bg(t.active))
                    .on_click(move |_, window, cx| dispatch(spec.action(), &focus, window, cx))
            })
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .into_any_element()
    }

    fn render_group(&self, group: &Group, fit: Fit, classic: bool, cx: &App) -> AnyElement {
        let t = theme(cx).clone();
        let buttons: Vec<AnyElement> = group
            .items
            .iter()
            .filter_map(|id| spec(id))
            .map(|spec| self.button(spec, fit, classic, cx))
            .collect();
        let separator = div()
            .flex_shrink_0()
            .w(px(1.))
            .mx(SPACE_1)
            .bg(t.border_subtle);
        if !classic {
            return div()
                .flex()
                .flex_row()
                .flex_shrink_0()
                .items_center()
                .gap(SPACE_1)
                .children(buttons)
                .child(separator.h(px(18.)))
                .into_any_element();
        }
        let body = if fit == Fit::Full {
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap(SPACE_0_5)
                .children(buttons)
        } else {
            // Classic compact: small buttons stacked three per column.
            let mut columns = Vec::new();
            let mut buttons = buttons.into_iter().peekable();
            while buttons.peek().is_some() {
                columns.push(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .children(buttons.by_ref().take(3)),
                );
            }
            div().flex().flex_row().gap(SPACE_1).children(columns)
        };
        div()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .h_full()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .h_full()
                    .px(SPACE_1)
                    .child(div().flex_1().flex().items_center().child(body))
                    .child(
                        div()
                            .text_center()
                            .text_size(TEXT_XS)
                            .text_color(t.text_subtle)
                            .child(tr(cx, group.name)),
                    ),
            )
            .child(separator.my(SPACE_1))
            .into_any_element()
    }

    /// "More" menu with the groups that did not fit.
    fn render_overflow(
        &self,
        hidden: Vec<&'static Group>,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let context = window
            .focused(cx)
            .unwrap_or_else(|| self.focus_handle.clone());
        let labels: Vec<Vec<(&'static ActionSpec, SharedString)>> = hidden
            .iter()
            .map(|group| {
                group
                    .items
                    .iter()
                    .filter_map(|id| spec(id))
                    .map(|spec| (spec, SharedString::from(tr(cx, spec.label))))
                    .collect()
            })
            .collect();
        MenuButton::new("ribbon-more")
            .ghost()
            .icon(IconName::Ellipsis)
            .tooltip("More commands")
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.action_context(context.clone());
                for (index, group) in labels.iter().enumerate() {
                    if index > 0 {
                        menu = menu.separator();
                    }
                    for (spec, label) in group {
                        menu = menu.menu_with_icon(label.clone(), spec.icon, spec.action());
                    }
                }
                menu
            })
            .into_any_element()
    }
}

impl Render for RibbonView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let module = self.state.read(cx).module();
        let classic = prefs(cx).ribbon == RibbonMode::Classic;
        let groups = groups(module, self.tab);
        // Window minus rail, paddings and the "more" button.
        let available = f32::from(window.viewport_size().width) - f32::from(RAIL_WIDTH) - 72.;
        let fits = fit_groups(groups, available, classic, cx);
        let hidden: Vec<&'static Group> = groups
            .iter()
            .zip(&fits)
            .filter(|(_, fit)| **fit == Fit::Hidden)
            .map(|(group, _)| group)
            .collect();
        let visible: Vec<AnyElement> = groups
            .iter()
            .zip(&fits)
            .filter(|(_, fit)| **fit != Fit::Hidden)
            .map(|(group, fit)| self.render_group(group, *fit, classic, cx))
            .collect();
        let overflow = (!hidden.is_empty()).then(|| self.render_overflow(hidden, window, cx));

        let tabs = Tab::ALL.map(|tab| {
            let selected = tab == self.tab;
            div()
                .id(("ribbon-tab", tab as u64))
                .relative()
                .flex()
                .items_center()
                .h_full()
                .px(SPACE_2)
                .text_size(TEXT_SM)
                .cursor_pointer()
                .when(selected, |el| {
                    el.text_color(t.text).font_weight(FontWeight::MEDIUM).child(
                        div()
                            .absolute()
                            .bottom(px(0.))
                            .left(SPACE_2)
                            .right(SPACE_2)
                            .h(px(2.))
                            .rounded(px(1.))
                            .bg(t.accent),
                    )
                })
                .when(!selected, |el| {
                    el.text_color(t.text_muted)
                        .hover(|style| style.text_color(t.text))
                })
                .child(tr(cx, tab.label()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.tab = tab;
                    cx.notify();
                }))
        });
        let focus = self.focus_handle.clone();
        let mode_toggle = div()
            .id("ribbon-mode")
            .flex()
            .items_center()
            .justify_center()
            .size(px(22.))
            .rounded(RADIUS_MD)
            .text_color(t.text_muted)
            .cursor_pointer()
            .hover(|style| style.bg(t.hover).text_color(t.text))
            .child(
                Icon::new(if classic {
                    IconName::ChevronUp
                } else {
                    IconName::ChevronDown
                })
                .size(ICON_SIZE_SM),
            )
            .tooltip(move |window, cx| {
                Tooltip::new(if classic {
                    "Simplified ribbon"
                } else {
                    "Classic ribbon"
                })
                .build(window, cx)
            })
            .on_click(move |_, window, cx| {
                dispatch(Box::new(ToggleRibbonMode), &focus, window, cx)
            });

        div()
            .id("ribbon")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w_full()
            .bg(t.surface)
            .border_b_1()
            .border_color(t.border_subtle)
            // Clicking a button must not move the focus to the ribbon: actions dispatch from
            // the focused view (e.g. the message list acts on its whole selection).
            .child(
                div()
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(30.))
                    .px(SPACE_2)
                    .gap(SPACE_1)
                    .children(tabs)
                    .child(div().flex_1())
                    .child(mode_toggle),
            )
            .child(
                div()
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(SPACE_1)
                    .px(SPACE_3)
                    .h(px(if classic { 86. } else { 40. }))
                    .when(classic, |el| el.py(SPACE_1))
                    .overflow_hidden()
                    .children(visible)
                    .children(overflow),
            )
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: the parent's `gpui::*` would shadow `#[test]`.
    use super::{Tab, groups};
    use crate::actions::{ACTIONS, spec};
    use crate::app_state::Module;

    #[test]
    fn every_ribbon_item_is_in_the_catalog() {
        let modules = [
            Module::Mail,
            Module::Calendar,
            Module::People,
            Module::Tasks,
            Module::Settings,
        ];
        for module in modules {
            for tab in Tab::ALL {
                for group in groups(module, tab) {
                    for id in group.items {
                        assert!(spec(id).is_some(), "unknown ribbon item {id}");
                    }
                }
            }
        }
        let mut ids: Vec<_> = ACTIONS.iter().map(|spec| spec.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), ACTIONS.len(), "duplicate action ids");
    }
}
