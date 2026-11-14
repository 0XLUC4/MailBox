//! Window root: left rail, top bar (ribbon + search) and the active module.

use gpui::component::WindowExt as _;
use gpui::component::notification::{Notification, NotificationType};
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use crate::app_state::{AppEvent, AppState, Module};
use crate::calendar::CalendarView;
use crate::components::{IconButton, toast};
use crate::mail::MailView;
use crate::people::{PeopleEvent, PeopleView};
use crate::ribbon::RibbonView;
use crate::search::SearchView;
use crate::settings::SettingsView;
use crate::tasks::TasksView;
use crate::theme::{
    RAIL_WIDTH, SPACE_1, SPACE_3, SPACE_4, TEXT_LG, TEXT_MD, TOP_BAR_HEIGHT, Theme, theme,
};
use crate::titlebar;
use crate::{GoToCalendar, GoToMail, GoToPeople, GoToSettings, GoToTasks, Redo, Undo};

pub struct Shell {
    state: Entity<AppState>,
    focus_handle: FocusHandle,
    ribbon: Entity<RibbonView>,
    search: Entity<SearchView>,
    mail: Entity<MailView>,
    calendar: Entity<CalendarView>,
    people: Entity<PeopleView>,
    tasks: Entity<TasksView>,
    settings: Entity<SettingsView>,
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &state,
                window,
                |_, _, event: &AppEvent, window, cx| match event {
                    AppEvent::Error(message) => window.push_notification(
                        toast(
                            Notification::new().with_type(NotificationType::Error),
                            message,
                        ),
                        cx,
                    ),
                    AppEvent::Info(message) => window.push_notification(
                        toast(
                            Notification::new().with_type(NotificationType::Info),
                            message,
                        ),
                        cx,
                    ),
                },
            ),
            cx.observe_window_appearance(window, |_, window, cx| crate::theme::refresh(window, cx)),
        ];
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);

        // People actions open compose / the event editor.
        let people = cx.new(|cx| PeopleView::new(state.clone(), window, cx));
        subscriptions.push(cx.subscribe_in(
            &people,
            window,
            |this, _, event: &PeopleEvent, window, cx| match event {
                PeopleEvent::Compose(recipients) => crate::compose::open_compose(
                    &this.state,
                    crate::compose::ComposeRequest::To(recipients.clone()),
                    window,
                    cx,
                ),
                PeopleEvent::ScheduleMeeting(attendees) => {
                    crate::calendar::open_meeting_editor(this.state.clone(), attendees.clone(), cx)
                }
            },
        ));

        Self {
            ribbon: cx.new(|cx| RibbonView::new(state.clone(), window, cx)),
            search: cx.new(|cx| SearchView::new(state.clone(), window, cx)),
            mail: cx.new(|cx| MailView::new(state.clone(), window, cx)),
            calendar: cx.new(|cx| CalendarView::new(state.clone(), window, cx)),
            people,
            tasks: cx.new(|cx| TasksView::new(state.clone(), window, cx)),
            settings: cx.new(|cx| SettingsView::new(state.clone(), window, cx)),
            state,
            focus_handle,
            _subscriptions: subscriptions,
        }
    }

    fn go_to(&mut self, module: Module, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, cx| state.set_module(module, cx));
    }

    fn render_rail(&self, active: Module, t: &Theme) -> impl IntoElement {
        let rail_item = |module: Module| {
            let state = self.state.clone();
            let is_active = module == active;
            div()
                .relative()
                .flex()
                .w_full()
                .justify_center()
                .when(is_active, |el| {
                    el.child(
                        div()
                            .absolute()
                            .left(px(0.))
                            .top(px(8.))
                            .w(px(3.))
                            .h(px(20.))
                            .rounded(px(2.))
                            .bg(t.accent),
                    )
                })
                .child(
                    IconButton::new(("rail", module as u64), module.icon())
                        .size(px(36.))
                        .icon_size(px(18.))
                        .active(is_active)
                        .tooltip(module.label())
                        .on_click(move |_, _, cx| {
                            state.update(cx, |state, cx| state.set_module(module, cx));
                        }),
                )
        };

        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .items_center()
            .w(RAIL_WIDTH)
            .h_full()
            .py(SPACE_3)
            // Clear the native traffic lights.
            .when(cfg!(target_os = "macos"), |el| el.pt(px(40.)))
            .gap(SPACE_1)
            .bg(t.rail)
            .border_r_1()
            .border_color(t.border_subtle)
            .children(Module::MAIN.map(&rail_item))
            .child(div().flex_1())
            .child(rail_item(Module::Settings))
    }

    /// Top bar doubling as the window title bar: empty space drags the window,
    /// caption buttons sit flush right.
    fn render_top_bar(&self, active: Module, t: &Theme) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .items_center()
            .h(TOP_BAR_HEIGHT)
            .pl(SPACE_3)
            .bg(t.surface)
            .border_b_1()
            .border_color(t.border_subtle)
            .child(titlebar::drag_area(
                div()
                    .id("top-bar-drag")
                    .flex()
                    .flex_1()
                    .h_full()
                    .items_center()
                    .min_w(px(0.))
                    .text_size(TEXT_LG)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(active.label()),
            ))
            .child(div().flex_shrink_0().w(px(420.)).child(self.search.clone()))
            .child(titlebar::drag_area(
                div().id("top-bar-gap").flex_shrink_0().w(SPACE_4).h_full(),
            ))
            .child(titlebar::WindowControls)
    }
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let active = self.state.read(cx).module();
        // First run: no account yet → onboarding (Settings stays reachable).
        let has_account = self
            .state
            .read(cx)
            .store()
            .list_accounts()
            .is_ok_and(|accounts| !accounts.is_empty());
        let content = match active {
            _ if !has_account && active != Module::Settings => {
                crate::onboarding::onboarding(self.state.clone(), cx).into_any_element()
            }
            Module::Mail => self.mail.clone().into_any_element(),
            Module::Calendar => self.calendar.clone().into_any_element(),
            Module::People => self.people.clone().into_any_element(),
            Module::Tasks => self.tasks.clone().into_any_element(),
            Module::Settings => self.settings.clone().into_any_element(),
        };

        div()
            .id("shell")
            .key_context("Shell")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Undo, _, cx| {
                this.state.update(cx, |state, cx| state.undo(cx));
            }))
            .on_action(cx.listener(|this, _: &Redo, _, cx| {
                this.state.update(cx, |state, cx| state.redo(cx));
            }))
            .on_action(cx.listener(|this, _: &GoToMail, _, cx| this.go_to(Module::Mail, cx)))
            .on_action(
                cx.listener(|this, _: &GoToCalendar, _, cx| this.go_to(Module::Calendar, cx)),
            )
            .on_action(cx.listener(|this, _: &GoToPeople, _, cx| this.go_to(Module::People, cx)))
            .on_action(cx.listener(|this, _: &GoToTasks, _, cx| this.go_to(Module::Tasks, cx)))
            .on_action(
                cx.listener(|this, _: &GoToSettings, _, cx| this.go_to(Module::Settings, cx)),
            )
            .map(|el| crate::actions::install(el, self.state.clone(), self.search.clone()))
            .flex()
            .flex_row()
            .size_full()
            .bg(t.background)
            .text_color(t.text)
            .text_size(TEXT_MD)
            .child(self.render_rail(active, &t))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .child(self.render_top_bar(active, &t))
                    .child(self.ribbon.clone())
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h(px(0.))
                            .child(content)
                            .children(crate::compose::dock(cx)),
                    ),
            )
    }
}
