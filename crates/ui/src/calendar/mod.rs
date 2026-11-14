//! Calendar module: Day / Work week / Week / Month / Agenda views, mini-month navigator,
//! calendar list, drag & drop editing, quick create, event editor window and reminders.
//!
//! Data: occurrences come from `mailbox_core::calendar::occurrences` (recurrences expanded,
//! exceptions applied). Every change is a `CalendarCommand` run through `AppState::run`, so
//! it is undoable with Ctrl+Z.

mod agenda;
mod calendars;
mod editor;
mod invitations;
mod layout;
mod month;
mod reminders;
mod sidebar;
mod time_grid;

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Local, NaiveDate, NaiveDateTime, Utc};
use gpui::component::input::{Input, InputEvent, InputState};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::calendar::{self as cal, CalendarCommand, EventDetails, Occurrence, ShowAs};
use mailbox_core::{Account, Calendar, CalendarEvent, Id, ResponseStatus};

use crate::actions;
use crate::app_state::{AppState, Module};
use crate::components::{Button, Icon, IconButton, IconName, toolbar_separator};
use crate::theme::{
    ICON_SIZE_SM, RADIUS_LG, RADIUS_MD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_6, TEXT_LG,
    TEXT_MD, TEXT_SM, Theme, hex, theme,
};

pub use editor::{EditorTarget, EventEditor, open_event_editor, open_meeting_editor};
pub use invitations::{
    IncomingInvitation, incoming_invitation, invitation_bar, queue_calendar_mail,
    respond_to_invitation, sending_account,
};
use layout::ViewKind;
pub use reminders::ReminderService;

/// Handles a ribbon/palette action while `module` is open (app-level listener, registered
/// after the "not available yet" fallbacks so it runs first); otherwise lets it through.
pub(crate) fn module_action<A: Action, V: 'static>(
    view: &WeakEntity<V>,
    state: &Entity<AppState>,
    module: Module,
    cx: &mut App,
    handler: fn(&mut V, &mut Context<V>),
) {
    let view = view.clone();
    let state = state.downgrade();
    cx.on_action(move |_: &A, cx| {
        let active = state
            .upgrade()
            .is_some_and(|state| state.read(cx).module() == module);
        match view.upgrade() {
            Some(view) if active => view.update(cx, handler),
            _ => cx.propagate(),
        }
    });
}

/// Height of one hour in the time grid.
pub(crate) const HOUR_HEIGHT: f32 = 52.;

/// What the mouse is over: a day column (time grid) or a day cell (month).
#[derive(Debug, Clone, Copy)]
struct HitArea {
    day: NaiveDate,
    calendar: Option<Id>,
    bounds: Bounds<Pixels>,
    /// Time grid column (y maps to minutes) vs month cell.
    timed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragKind {
    Create,
    Move,
    ResizeStart,
    ResizeEnd,
}

#[derive(Debug, Clone)]
struct Drag {
    kind: DragKind,
    occurrence: Option<Occurrence>,
    origin: (NaiveDate, i64),
    current: (NaiveDate, i64),
    calendar: Option<Id>,
    timed: bool,
    moved: bool,
    copy: bool,
}

impl Drag {
    /// New start/end of the dragged occurrence (move/resize).
    fn preview(&self) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
        let occurrence = self.occurrence.as_ref()?;
        let (start, end) = (occurrence.event.start, occurrence.event.end);
        let days = (self.current.0 - self.origin.0).num_days();
        let minutes = if self.timed {
            snap_delta(self.current.1 - self.origin.1)
        } else {
            0
        };
        let min_len = Duration::minutes(layout::SNAP_MINUTES);
        Some(match self.kind {
            DragKind::Move | DragKind::Create => {
                let delta = Duration::days(days) + Duration::minutes(minutes);
                (start + delta, end + delta)
            }
            DragKind::ResizeEnd => {
                let new_end = layout::local_to_utc(self.current.0, layout::snap(self.current.1));
                (start, new_end.max(start + min_len))
            }
            DragKind::ResizeStart => {
                let new_start = layout::local_to_utc(self.current.0, layout::snap(self.current.1));
                (new_start.min(end - min_len), end)
            }
        })
    }

    /// The occurrence as it would look after the drop.
    fn ghost(&self) -> Option<Occurrence> {
        let (start, end) = self.preview()?;
        let mut ghost = self.occurrence.clone()?;
        ghost.event.start = start;
        ghost.event.end = end;
        Some(ghost)
    }

    /// Day and minute range of a drag-to-create.
    fn create_range(&self) -> (NaiveDate, i64, i64) {
        let (a, b) = (self.origin.1, self.current.1);
        let start = layout::snap_down(a.min(b));
        let end = if self.moved {
            layout::snap(a.max(b)).max(start + layout::SNAP_MINUTES)
        } else {
            start + 30
        };
        (self.origin.0, start, end.min(layout::MINUTES_PER_DAY))
    }
}

fn snap_delta(minutes: i64) -> i64 {
    let step = layout::SNAP_MINUTES as f64;
    ((minutes as f64 / step).round() * step) as i64
}

/// Inline "type a title and press Enter" block.
struct QuickCreate {
    day: NaiveDate,
    start: i64,
    end: i64,
    all_day: bool,
    calendar_id: Id,
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// What to do once the user picked "this occurrence" or "entire series".
#[derive(Debug, Clone)]
enum SeriesAction {
    Open,
    Delete,
    Move {
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    },
}

#[derive(Debug, Clone)]
struct SeriesPrompt {
    occurrence: Occurrence,
    action: SeriesAction,
}

/// Everything a render needs, loaded once per frame.
pub(crate) struct ViewData {
    accounts: Vec<Account>,
    calendars: Vec<Calendar>,
    /// Visible (checked) calendars.
    visible: Vec<Calendar>,
    days: Vec<NaiveDate>,
    occurrences: Vec<Occurrence>,
    today: NaiveDate,
    now: NaiveDateTime,
    my_emails: Vec<String>,
}

impl ViewData {
    fn color(&self, calendar_id: Id) -> Hsla {
        self.calendars
            .iter()
            .find(|calendar| calendar.id == calendar_id)
            .map_or(hex(0x3b82f6), |calendar| hex(calendar.color))
    }

    /// The user's answer to an occurrence (accepted when they organize it).
    fn my_status(&self, occurrence: &Occurrence) -> ResponseStatus {
        let mine = |email: &str| self.my_emails.iter().any(|m| m.eq_ignore_ascii_case(email));
        if occurrence
            .details
            .organizer
            .as_ref()
            .is_none_or(|organizer| mine(&organizer.email))
        {
            return ResponseStatus::Accepted;
        }
        occurrence
            .event
            .attendees
            .iter()
            .find(|attendee| mine(&attendee.address.email))
            .map_or(ResponseStatus::Accepted, |attendee| attendee.status)
    }
}

/// Colours of an event chip.
pub(crate) struct ChipStyle {
    bg: Hsla,
    fg: Hsla,
    muted: Hsla,
    bar: Hsla,
    border: Option<Hsla>,
    strike: bool,
}

pub(crate) fn chip_style(
    color: Hsla,
    t: &Theme,
    selected: bool,
    status: ResponseStatus,
    show_as: ShowAs,
) -> ChipStyle {
    let fg = if t.is_dark {
        Hsla {
            l: 0.86,
            s: color.s.min(0.8),
            ..color
        }
    } else {
        Hsla {
            l: (color.l * 0.5).min(0.32),
            ..color
        }
    };
    if selected {
        return ChipStyle {
            bg: color,
            fg: t.on_accent,
            muted: t.on_accent.alpha(0.8),
            bar: Hsla {
                l: (color.l - 0.12).max(0.),
                ..color
            },
            border: None,
            strike: false,
        };
    }
    let soft = color.alpha(if t.is_dark { 0.26 } else { 0.15 });
    let mut style = ChipStyle {
        bg: soft,
        fg,
        muted: fg.alpha(0.75),
        bar: color,
        border: None,
        strike: false,
    };
    match status {
        ResponseStatus::NeedsAction => {
            style.bg = t.background;
            style.border = Some(color.alpha(0.7));
        }
        ResponseStatus::Tentative => {
            style.bg = color.alpha(0.07);
            style.border = Some(color.alpha(0.5));
        }
        ResponseStatus::Declined => {
            style.bg = t.background.alpha(0.);
            style.fg = t.text_subtle;
            style.muted = t.text_subtle;
            style.bar = color.alpha(0.4);
            style.strike = true;
        }
        ResponseStatus::Accepted => {}
    }
    if show_as == ShowAs::Free && status == ResponseStatus::Accepted {
        style.bg = color.alpha(0.06);
        style.border = Some(color.alpha(0.35));
    }
    style
}

pub struct CalendarView {
    state: Entity<AppState>,
    focus: FocusHandle,
    kind: ViewKind,
    anchor: NaiveDate,
    sidebar_month: NaiveDate,
    /// Month shown by the toolbar date picker, `Some` while it is open.
    picker: Option<NaiveDate>,
    selected: Option<(Id, i64)>,
    hidden: HashSet<Id>,
    side_by_side: bool,
    scroll: ScrollHandle,
    hits: Rc<RefCell<Vec<HitArea>>>,
    drag: Option<Drag>,
    quick: Option<QuickCreate>,
    prompt: Option<SeriesPrompt>,
    calendar_edit: Option<calendars::CalendarEdit>,
    /// Focus the view on its next render (the module was just opened).
    needs_focus: bool,
    _reminders: Entity<ReminderService>,
    _tick: Task<()>,
    _subscription: Subscription,
}

impl CalendarView {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&state, |this: &mut Self, state, cx| {
            if state.read(cx).module() != Module::Calendar {
                this.needs_focus = true;
            }
            cx.notify()
        });
        calendars::ensure_default_calendar(&state, cx);
        let today = Local::now().date_naive();
        let scroll = ScrollHandle::new();
        // Open the day at 07:30 so 08:00 is visible with a little context above.
        scroll.set_offset(point(px(0.), px(-HOUR_HEIGHT * 7.5)));
        // Re-render every 30 s so the current-time line moves.
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(StdDuration::from_secs(30))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        let reminders = cx.new(|cx| ReminderService::new(state.clone(), cx));
        let view = cx.entity().downgrade();
        module_action::<actions::NewEvent, _>(&view, &state, Module::Calendar, cx, |this, cx| {
            this.new_event(cx)
        });
        module_action::<actions::CalendarToday, _>(
            &view,
            &state,
            Module::Calendar,
            cx,
            |this, cx| this.today(cx),
        );
        module_action::<actions::CalendarDayView, _>(
            &view,
            &state,
            Module::Calendar,
            cx,
            |this, cx| this.set_kind(ViewKind::Day, cx),
        );
        module_action::<actions::CalendarWeekView, _>(
            &view,
            &state,
            Module::Calendar,
            cx,
            |this, cx| this.set_kind(ViewKind::Week, cx),
        );
        module_action::<actions::CalendarMonthView, _>(
            &view,
            &state,
            Module::Calendar,
            cx,
            |this, cx| this.set_kind(ViewKind::Month, cx),
        );
        Self {
            state,
            focus: cx.focus_handle(),
            kind: ViewKind::Week,
            anchor: today,
            sidebar_month: layout::first_of_month(today),
            picker: None,
            selected: None,
            hidden: HashSet::new(),
            side_by_side: false,
            scroll,
            hits: Rc::new(RefCell::new(Vec::new())),
            drag: None,
            quick: None,
            prompt: None,
            calendar_edit: None,
            needs_focus: true,
            _reminders: reminders,
            _tick: tick,
            _subscription: subscription,
        }
    }

    // ---------- navigation ----------

    fn set_kind(&mut self, kind: ViewKind, cx: &mut Context<Self>) {
        self.kind = kind;
        self.quick = None;
        self.drag = None;
        cx.notify();
    }

    fn go_to(&mut self, day: NaiveDate, cx: &mut Context<Self>) {
        self.anchor = day;
        self.sidebar_month = layout::first_of_month(day);
        self.quick = None;
        cx.notify();
    }

    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let day = layout::step(self.kind, self.anchor, forward);
        self.go_to(day, cx);
    }

    fn today(&mut self, cx: &mut Context<Self>) {
        self.go_to(Local::now().date_naive(), cx);
        self.scroll
            .set_offset(point(px(0.), px(-HOUR_HEIGHT * 7.5)));
    }

    // ---------- data ----------

    fn load(&self, cx: &App) -> ViewData {
        let store = self.state.read(cx).store();
        let accounts = store.list_accounts().unwrap_or_default();
        let calendars = store.list_calendars().unwrap_or_default();
        let visible: Vec<Calendar> = calendars
            .iter()
            .filter(|calendar| !self.hidden.contains(&calendar.id))
            .cloned()
            .collect();
        let days = layout::visible_days(self.kind, self.anchor);
        let occurrences = self.occurrences_between(cx, &days, &visible);
        let now = Local::now().naive_local();
        ViewData {
            my_emails: accounts
                .iter()
                .map(|account| account.email.clone())
                .collect(),
            accounts,
            calendars,
            visible,
            days,
            occurrences,
            today: now.date(),
            now,
        }
    }

    /// Occurrences of visible calendars touching `days` (first..=last).
    fn occurrences_between(
        &self,
        cx: &App,
        days: &[NaiveDate],
        visible: &[Calendar],
    ) -> Vec<Occurrence> {
        let (Some(first), Some(last)) = (days.first(), days.last()) else {
            return Vec::new();
        };
        let (from, to) = layout::utc_range(*first, *last);
        let store = self.state.read(cx).store();
        cal::occurrences(store, from, to)
            .unwrap_or_default()
            .into_iter()
            .filter(|occurrence| {
                visible
                    .iter()
                    .any(|calendar| calendar.id == occurrence.event.calendar_id)
            })
            .filter(|occurrence| {
                let (start, end) = layout::day_span(occurrence);
                end >= *first && start <= *last
            })
            .collect()
    }

    fn default_calendar(&self, cx: &App) -> Option<Id> {
        let calendars = self
            .state
            .read(cx)
            .store()
            .list_calendars()
            .unwrap_or_default();
        calendars
            .iter()
            .find(|calendar| !self.hidden.contains(&calendar.id))
            .or(calendars.first())
            .map(|calendar| calendar.id)
    }

    fn find_selected(&self, data: &ViewData) -> Option<Occurrence> {
        let key = self.selected?;
        data.occurrences
            .iter()
            .find(|occurrence| occurrence.key() == key)
            .cloned()
    }

    // ---------- commands ----------

    fn run(&mut self, command: CalendarCommand, cx: &mut Context<Self>) -> bool {
        self.state
            .update(cx, |state, cx| state.run(command.into_command(), cx))
    }

    /// Moves/resizes, asking about the series for a recurring instance.
    fn apply_move(
        &mut self,
        occurrence: Occurrence,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        cx: &mut Context<Self>,
    ) {
        if start == occurrence.event.start && end == occurrence.event.end {
            return;
        }
        if occurrence.is_recurring_instance() {
            self.prompt = Some(SeriesPrompt {
                occurrence,
                action: SeriesAction::Move { start, end },
            });
            cx.notify();
            return;
        }
        let id = occurrence.event.id;
        self.selected = Some((id, start.timestamp()));
        self.run(CalendarCommand::Move { id, start, end }, cx);
    }

    fn copy_to(
        &mut self,
        occurrence: &Occurrence,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        cx: &mut Context<Self>,
    ) {
        self.run(
            CalendarCommand::Copy {
                id: occurrence.event.id,
                start,
                end,
            },
            cx,
        );
    }

    fn delete_occurrence(&mut self, occurrence: Occurrence, cx: &mut Context<Self>) {
        if occurrence.is_recurring_instance() {
            self.prompt = Some(SeriesPrompt {
                occurrence,
                action: SeriesAction::Delete,
            });
            cx.notify();
            return;
        }
        self.selected = None;
        self.run(
            CalendarCommand::Delete {
                id: occurrence.event.id,
            },
            cx,
        );
    }

    fn open_occurrence(&mut self, occurrence: Occurrence, cx: &mut Context<Self>) {
        if occurrence.is_recurring_instance() {
            self.prompt = Some(SeriesPrompt {
                occurrence,
                action: SeriesAction::Open,
            });
            cx.notify();
            return;
        }
        open_event_editor(
            self.state.clone(),
            EditorTarget::Event {
                id: occurrence.event.id,
            },
            cx,
        );
    }

    fn open_new(
        &mut self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        all_day: bool,
        calendar_id: Option<Id>,
        title: String,
        cx: &mut Context<Self>,
    ) {
        let calendar_id = calendar_id.or_else(|| self.default_calendar(cx));
        open_event_editor(
            self.state.clone(),
            EditorTarget::New {
                start,
                end,
                all_day,
                calendar_id,
                title,
            },
            cx,
        );
    }

    /// New event at the next full hour of the anchor day.
    fn new_event(&mut self, cx: &mut Context<Self>) {
        let now = Local::now().naive_local();
        let hour = if self.anchor == now.date() {
            (i64::from(chrono::Timelike::hour(&now)) + 1).min(23)
        } else {
            9
        };
        let start = layout::local_to_utc(self.anchor, hour * 60);
        self.open_new(
            start,
            start + Duration::minutes(30),
            false,
            None,
            String::new(),
            cx,
        );
    }

    fn resolve_prompt(&mut self, whole_series: bool, cx: &mut Context<Self>) {
        let Some(SeriesPrompt { occurrence, action }) = self.prompt.take() else {
            return;
        };
        cx.notify();
        let series_id = occurrence.series_id();
        let original = occurrence.series_start.unwrap_or(occurrence.event.start);
        match (action, whole_series) {
            (SeriesAction::Open, false) => open_event_editor(
                self.state.clone(),
                EditorTarget::Occurrence {
                    occurrence: Box::new(occurrence),
                },
                cx,
            ),
            (SeriesAction::Open, true) => open_event_editor(
                self.state.clone(),
                EditorTarget::Event { id: series_id },
                cx,
            ),
            (SeriesAction::Delete, false) => {
                self.selected = None;
                self.run(
                    CalendarCommand::SkipOccurrence {
                        series_id,
                        occurrence: original,
                    },
                    cx,
                );
            }
            (SeriesAction::Delete, true) => {
                self.selected = None;
                self.run(CalendarCommand::Delete { id: series_id }, cx);
            }
            (SeriesAction::Move { start, end }, false) => {
                let mut event = occurrence.event.clone();
                event.id = 0;
                event.start = start;
                event.end = end;
                event.recurrence = None;
                let details = EventDetails {
                    exdates: Vec::new(),
                    ..occurrence.details.clone()
                };
                self.selected = None;
                self.run(
                    CalendarCommand::SaveOccurrence {
                        series_id,
                        occurrence: original,
                        event: Box::new(event),
                        details: Box::new(details),
                    },
                    cx,
                );
            }
            (SeriesAction::Move { start, end }, true) => {
                let master = self
                    .state
                    .read(cx)
                    .store()
                    .get::<CalendarEvent>(series_id)
                    .ok()
                    .flatten();
                if let Some(master) = master {
                    let start = master.start + (start - occurrence.event.start);
                    let end = master.end + (end - occurrence.event.end);
                    self.selected = None;
                    self.run(
                        CalendarCommand::Move {
                            id: series_id,
                            start,
                            end,
                        },
                        cx,
                    );
                }
            }
        }
    }

    // ---------- quick create ----------

    #[allow(clippy::too_many_arguments)]
    fn start_quick_create(
        &mut self,
        day: NaiveDate,
        start: i64,
        end: i64,
        all_day: bool,
        calendar: Option<Id>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(calendar_id) = calendar.or_else(|| self.default_calendar(cx)) else {
            self.state.update(cx, |state, cx| {
                state.report_error("Create a calendar first", cx)
            });
            return;
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("New event"));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, input, event: &InputEvent, _window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let title = input.read(cx).value().trim().to_owned();
                    this.commit_quick_create(title, cx);
                }
                InputEvent::Blur => {
                    let title = input.read(cx).value().trim().to_owned();
                    if title.is_empty() {
                        this.quick = None;
                        cx.notify();
                    } else {
                        this.commit_quick_create(title, cx);
                    }
                }
                _ => {}
            },
        );
        input.update(cx, |input, cx| input.focus(window, cx));
        self.selected = None;
        self.quick = Some(QuickCreate {
            day,
            start,
            end,
            all_day,
            calendar_id,
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn commit_quick_create(&mut self, title: String, cx: &mut Context<Self>) {
        let Some(quick) = self.quick.take() else {
            return;
        };
        cx.notify();
        if title.is_empty() {
            return;
        }
        let (start, end) = if quick.all_day {
            let start = cal::all_day_start(quick.day);
            (start, start + Duration::days(1))
        } else {
            (
                layout::local_to_utc(quick.day, quick.start),
                layout::local_to_utc(quick.day, quick.end),
            )
        };
        let event = CalendarEvent {
            id: 0,
            calendar_id: quick.calendar_id,
            title,
            location: String::new(),
            description: String::new(),
            start,
            end,
            all_day: quick.all_day,
            recurrence: None,
            attendees: Vec::new(),
            reminders: if quick.all_day { Vec::new() } else { vec![15] },
            categories: Vec::new(),
        };
        self.run(
            CalendarCommand::Save {
                event: Box::new(event),
                details: Box::new(EventDetails::default_for(0)),
            },
            cx,
        );
    }

    fn render_quick_input(&self, quick: &QuickCreate, t: &Theme, cx: &mut Context<Self>) -> Div {
        div()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.quick = None;
                    this.focus.focus(window, cx);
                    cx.notify();
                }
            }))
            .text_size(TEXT_SM)
            .text_color(t.text)
            .child(Input::new(&quick.input).appearance(false))
    }

    // ---------- mouse ----------

    /// Area and minute-of-day under `position`; for time columns only x must match.
    fn hit(&self, position: Point<Pixels>) -> Option<(HitArea, i64)> {
        let hits = self.hits.borrow();
        let area = hits
            .iter()
            .find(|area| area.bounds.contains(&position))
            .or_else(|| {
                hits.iter().find(|area| {
                    area.timed
                        && position.x >= area.bounds.left()
                        && position.x < area.bounds.right()
                })
            })
            .copied()?;
        let minutes = if area.timed {
            let y = (position.y - area.bounds.origin.y) / px(HOUR_HEIGHT);
            ((y * 60.) as i64).clamp(0, layout::MINUTES_PER_DAY)
        } else {
            0
        };
        Some((area, minutes))
    }

    fn on_empty_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus.focus(window, cx);
        self.picker = None;
        let Some((area, minutes)) = self.hit(event.position) else {
            return;
        };
        if event.click_count >= 2 {
            self.quick = None;
            self.drag = None;
            let (start, end, all_day) = if area.timed {
                let start = layout::local_to_utc(area.day, layout::snap_down(minutes));
                (start, start + Duration::minutes(30), false)
            } else {
                let start = cal::all_day_start(area.day);
                (start, start + Duration::days(1), true)
            };
            self.open_new(start, end, all_day, area.calendar, String::new(), cx);
            return;
        }
        if !area.timed {
            // Month cell: select the day; dragging creates nothing.
            self.selected = None;
            self.anchor = area.day;
            cx.notify();
            return;
        }
        self.quick = None;
        self.selected = None;
        self.drag = Some(Drag {
            kind: DragKind::Create,
            occurrence: None,
            origin: (area.day, minutes),
            current: (area.day, minutes),
            calendar: area.calendar,
            timed: true,
            moved: false,
            copy: false,
        });
        cx.notify();
    }

    fn on_event_mouse_down(
        &mut self,
        occurrence: &Occurrence,
        kind: DragKind,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        self.focus.focus(window, cx);
        self.picker = None;
        self.quick = None;
        self.selected = Some(occurrence.key());
        if event.click_count >= 2 {
            self.drag = None;
            self.open_occurrence(occurrence.clone(), cx);
            return;
        }
        let (origin, timed) = match self.hit(event.position) {
            Some((area, minutes)) => ((area.day, minutes), area.timed),
            None => ((self.anchor, 0), false),
        };
        self.drag = Some(Drag {
            kind,
            occurrence: Some(occurrence.clone()),
            origin,
            current: origin,
            calendar: None,
            timed,
            moved: false,
            copy: event.modifiers.control || event.modifiers.platform,
        });
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if self.drag.is_none() {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            // Released outside the window: drop the drag.
            self.drag = None;
            cx.notify();
            return;
        }
        let hit = self.hit(event.position);
        let Some(drag) = self.drag.as_mut() else {
            return;
        };
        if let Some((area, minutes)) = hit {
            let current = (area.day, minutes);
            let threshold = if drag.timed { 5 } else { 0 };
            if current.0 != drag.origin.0 || (current.1 - drag.origin.1).abs() > threshold {
                drag.moved = true;
            }
            if drag.current != current {
                drag.current = current;
                drag.copy = event.modifiers.control || event.modifiers.platform;
                cx.notify();
            }
        }
    }

    fn on_mouse_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        cx.notify();
        match drag.kind {
            DragKind::Create => {
                let (day, start, end) = drag.create_range();
                self.start_quick_create(day, start, end, false, drag.calendar, window, cx);
            }
            DragKind::Move | DragKind::ResizeStart | DragKind::ResizeEnd => {
                if !drag.moved {
                    return;
                }
                let (Some(occurrence), Some((start, end))) =
                    (drag.occurrence.clone(), drag.preview())
                else {
                    return;
                };
                if drag.copy && drag.kind == DragKind::Move {
                    self.copy_to(&occurrence, start, end, cx);
                } else {
                    self.apply_move(occurrence, start, end, cx);
                }
            }
        }
    }

    // ---------- keyboard ----------

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window)
            || (self.prompt.is_some() && event.keystroke.key != "escape")
        {
            return;
        }
        let modifiers = event.keystroke.modifiers;
        if modifiers.control || modifiers.platform || modifiers.alt {
            return;
        }
        let data_needed = matches!(
            event.keystroke.key.as_str(),
            "delete" | "backspace" | "enter"
        );
        let selected = if data_needed {
            let data = self.load(cx);
            self.find_selected(&data)
        } else {
            None
        };
        match event.keystroke.key.as_str() {
            "left" => self.go_to(self.anchor - Duration::days(1), cx),
            "right" => self.go_to(self.anchor + Duration::days(1), cx),
            "up" => self.go_to(self.anchor - Duration::weeks(1), cx),
            "down" => self.go_to(self.anchor + Duration::weeks(1), cx),
            "pageup" => self.step(false, cx),
            "pagedown" => self.step(true, cx),
            "t" => self.today(cx),
            "d" => self.set_kind(ViewKind::Day, cx),
            "x" => self.set_kind(ViewKind::WorkWeek, cx),
            "w" => self.set_kind(ViewKind::Week, cx),
            "m" => self.set_kind(ViewKind::Month, cx),
            "a" => self.set_kind(ViewKind::Agenda, cx),
            "n" => self.new_event(cx),
            "delete" | "backspace" => {
                if let Some(occurrence) = selected {
                    self.delete_occurrence(occurrence, cx);
                }
            }
            "enter" => {
                if let Some(occurrence) = selected {
                    self.open_occurrence(occurrence, cx);
                }
            }
            "escape" => {
                self.prompt = None;
                self.drag = None;
                self.quick = None;
                self.picker = None;
                self.selected = None;
                cx.notify();
            }
            _ => {}
        }
    }

    // ---------- import / export ----------

    fn import_ics(&mut self, cx: &mut Context<Self>) {
        let Some(calendar_id) = self.default_calendar(cx) else {
            return;
        };
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let parsed = std::fs::read_to_string(&path)
                .map_err(|err| err.to_string())
                .and_then(|text| mailbox_core::ics::parse(&text).map_err(|err| err.to_string()));
            let _ = this.update(cx, |this, cx| match parsed {
                Ok(calendar) if calendar.events.is_empty() => {
                    this.state.update(cx, |state, cx| {
                        state.notify_info("No events in this file", cx)
                    });
                }
                Ok(calendar) => {
                    let count = calendar.events.len();
                    let ok = this.run(
                        CalendarCommand::Import {
                            calendar_id,
                            method: calendar.method,
                            events: calendar.events,
                        },
                        cx,
                    );
                    if ok {
                        this.state.update(cx, |state, cx| {
                            state.notify_info(
                                format!(
                                    "Imported {count} event{}",
                                    if count == 1 { "" } else { "s" }
                                ),
                                cx,
                            )
                        });
                    }
                }
                Err(err) => this.state.update(cx, |state, cx| {
                    state.report_error(format!("Import failed: {err}"), cx)
                }),
            });
        })
        .detach();
    }

    /// Exports the visible calendars (all their events) to an .ics file.
    fn export_ics(&mut self, cx: &mut Context<Self>) {
        let text = {
            let store = self.state.read(cx).store();
            let visible: HashSet<Id> = store
                .list_calendars()
                .unwrap_or_default()
                .into_iter()
                .filter(|calendar| !self.hidden.contains(&calendar.id))
                .map(|calendar| calendar.id)
                .collect();
            let events: Vec<(CalendarEvent, EventDetails)> = store
                .query::<CalendarEvent>("ORDER BY start_at", [])
                .unwrap_or_default()
                .into_iter()
                .filter(|event| visible.contains(&event.calendar_id))
                .map(|event| {
                    let details = cal::details(store, event.id)
                        .unwrap_or_else(|_| EventDetails::default_for(event.id));
                    (event, details)
                })
                .collect();
            mailbox_core::ics::export(&events, None)
        };
        let directory = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let path = cx.prompt_for_new_path(&directory, Some("calendar.ics"));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = path.await else {
                return;
            };
            let result = std::fs::write(&path, text);
            let _ = this.update(cx, |this, cx| {
                this.state.update(cx, |state, cx| match result {
                    Ok(()) => state.notify_info(format!("Exported to {}", path.display()), cx),
                    Err(err) => state.report_error(format!("Export failed: {err}"), cx),
                })
            });
        })
        .detach();
    }

    // ---------- toolbar ----------

    fn render_toolbar(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let title = layout::title(self.kind, self.anchor);
        let picker_open = self.picker.is_some();
        let segmented = div()
            .flex()
            .flex_row()
            .items_center()
            .p(px(2.))
            .gap(px(2.))
            .rounded(RADIUS_MD)
            .bg(t.hover)
            .children(ViewKind::ALL.map(|kind| {
                let active = kind == self.kind;
                div()
                    .id(("view-kind", kind as u64))
                    .flex()
                    .items_center()
                    .h(px(24.))
                    .px(SPACE_3)
                    .rounded(px(5.))
                    .text_size(TEXT_SM)
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .when(active, |el| {
                        el.bg(t.elevated).text_color(t.text).shadow_sm()
                    })
                    .when(!active, |el| {
                        el.text_color(t.text_muted)
                            .hover(|style| style.text_color(t.text))
                    })
                    .tooltip(move |window, cx| {
                        gpui::component::tooltip::Tooltip::new(format!(
                            "{} ({})",
                            kind.label(),
                            kind.shortcut()
                        ))
                        .build(window, cx)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.set_kind(kind, cx)))
                    .child(kind.label())
            }));

        div()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .items_center()
            .gap(SPACE_2)
            .h(px(52.))
            .px(SPACE_4)
            .border_b_1()
            .border_color(t.border_subtle)
            .child(
                Button::new("cal-today")
                    .label("Today")
                    .on_click(cx.listener(|this, _, _, cx| this.today(cx))),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .child(
                        IconButton::new("cal-prev", IconName::ChevronLeft)
                            .tooltip("Previous (Page Up)")
                            .on_click(cx.listener(|this, _, _, cx| this.step(false, cx))),
                    )
                    .child(
                        IconButton::new("cal-next", IconName::ChevronRight)
                            .tooltip("Next (Page Down)")
                            .on_click(cx.listener(|this, _, _, cx| this.step(true, cx))),
                    ),
            )
            .child(
                div().relative().child(
                    div()
                        .id("cal-title")
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(SPACE_1)
                        .px(SPACE_2)
                        .h(px(30.))
                        .rounded(RADIUS_MD)
                        .cursor_pointer()
                        .hover(|style| style.bg(t.hover))
                        .when(picker_open, |el| el.bg(t.active))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.picker = match this.picker {
                                Some(_) => None,
                                None => Some(layout::first_of_month(this.anchor)),
                            };
                            cx.notify();
                        }))
                        .child(
                            div()
                                .text_size(TEXT_LG)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .child(
                            Icon::new(IconName::ChevronDown)
                                .size(ICON_SIZE_SM)
                                .text_color(t.text_muted),
                        ),
                ),
            )
            .child(div().flex_1())
            .child(segmented)
            .child(toolbar_separator(cx))
            .child(
                IconButton::new("cal-import", IconName::Download)
                    .tooltip("Import .ics")
                    .on_click(cx.listener(|this, _, _, cx| this.import_ics(cx))),
            )
            .child(
                IconButton::new("cal-export", IconName::Upload)
                    .tooltip("Export visible calendars to .ics")
                    .on_click(cx.listener(|this, _, _, cx| this.export_ics(cx))),
            )
    }

    /// Date picker popover under the toolbar title, with a click-away backdrop.
    fn render_picker(
        &self,
        month: NaiveDate,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .id("cal-picker-backdrop")
            .absolute()
            .inset_0()
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.picker = None;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .id("cal-date-picker")
                    .absolute()
                    .top(px(48.))
                    .left(px(sidebar::SIDEBAR_WIDTH + 140.))
                    .w(px(260.))
                    .p(SPACE_3)
                    .rounded(RADIUS_LG)
                    .border_1()
                    .border_color(t.border)
                    .bg(t.elevated)
                    .shadow_lg()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(self.render_mini_month(month, true, t, cx)),
            )
    }

    fn render_prompt(
        &self,
        prompt: &SeriesPrompt,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let (title, body, this_label, all_label) = match prompt.action {
            SeriesAction::Open => (
                "Open recurring event",
                "Do you want to open just this occurrence or the whole series?",
                "Just this one",
                "The entire series",
            ),
            SeriesAction::Delete => (
                "Delete recurring event",
                "Delete only this occurrence, or every occurrence of the series?",
                "Delete this occurrence",
                "Delete the series",
            ),
            SeriesAction::Move { .. } => (
                "Change recurring event",
                "Apply the new time to this occurrence only, or shift the entire series?",
                "This occurrence",
                "Entire series",
            ),
        };
        let destructive = matches!(prompt.action, SeriesAction::Delete);
        div()
            .id("cal-series-prompt")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(hsla(0., 0., 0., if t.is_dark { 0.45 } else { 0.25 }))
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.prompt = None;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .id("cal-series-card")
                    .w(px(400.))
                    .p(SPACE_6)
                    .flex()
                    .flex_col()
                    .gap(SPACE_3)
                    .rounded(px(12.))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.elevated)
                    .shadow_lg()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(SPACE_2)
                            .child(
                                Icon::new(IconName::Repeat)
                                    .size(px(16.))
                                    .text_color(t.accent),
                            )
                            .child(
                                div()
                                    .text_size(TEXT_LG)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            ),
                    )
                    .child(
                        div()
                            .text_size(TEXT_MD)
                            .text_color(t.text_muted)
                            .child(format!("“{}” — {body}", prompt.occurrence.event.title)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_end()
                            .gap(SPACE_2)
                            .pt(SPACE_2)
                            .child(
                                Button::new("prompt-cancel")
                                    .ghost()
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.prompt = None;
                                        cx.notify();
                                    })),
                            )
                            .child(Button::new("prompt-series").label(all_label).on_click(
                                cx.listener(|this, _, _, cx| this.resolve_prompt(true, cx)),
                            ))
                            .child(
                                Button::new("prompt-one")
                                    .variant(if destructive {
                                        crate::components::ButtonVariant::Danger
                                    } else {
                                        crate::components::ButtonVariant::Primary
                                    })
                                    .label(this_label)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.resolve_prompt(false, cx)
                                        }),
                                    ),
                            ),
                    ),
            )
    }
}

impl Render for CalendarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        // Ribbon actions (New event, Today, views, Delete) dispatch from the focused element:
        // take focus each time the module is opened.
        if std::mem::take(&mut self.needs_focus) {
            self.focus.focus(window, cx);
        }
        self.hits.borrow_mut().clear();
        let data = self.load(cx);
        let body = match self.kind {
            ViewKind::Day | ViewKind::WorkWeek | ViewKind::Week => self
                .render_time_grid(&data, &t, window, cx)
                .into_any_element(),
            ViewKind::Month => self.render_month(&data, &t, cx).into_any_element(),
            ViewKind::Agenda => self.render_agenda(&data, &t, cx).into_any_element(),
        };
        let prompt = self
            .prompt
            .clone()
            .map(|prompt| self.render_prompt(&prompt, &t, cx));
        let picker = self.picker.map(|month| self.render_picker(month, &t, cx));
        let calendar_edit = self.render_calendar_edit(&t, cx);

        div()
            .id("calendar")
            .key_context("Calendar")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.on_key_down(event, window, cx)
            }))
            // The shell binds Delete to a mail action; here it deletes the selected event.
            .on_action(cx.listener(|this, _: &actions::DeleteSelection, _, cx| {
                let data = this.load(cx);
                if let Some(occurrence) = this.find_selected(&data) {
                    this.delete_occurrence(occurrence, cx);
                }
            }))
            .relative()
            .size_full()
            .flex()
            .flex_row()
            .bg(t.background)
            .child(self.render_sidebar(&data, &t, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .child(self.render_toolbar(&t, cx))
                    .child(div().flex_1().min_h(px(0.)).child(body)),
            )
            .when_some(picker, |el, picker| el.child(picker))
            .when_some(prompt, |el, prompt| el.child(prompt))
            .when_some(calendar_edit, |el, edit| el.child(edit))
    }
}

/// Ribbon / command palette entry points (routed by `crate::actions`).
impl CalendarView {
    pub(crate) fn action_today(&mut self, cx: &mut Context<Self>) {
        self.today(cx);
    }

    /// Deletes the selected event (asks first for a series); false when none is selected.
    pub(crate) fn action_delete(&mut self, cx: &mut Context<Self>) -> bool {
        let data = self.load(cx);
        let Some(occurrence) = self.find_selected(&data) else {
            return false;
        };
        self.delete_occurrence(occurrence, cx);
        true
    }

    pub(crate) fn action_new_event(&mut self, cx: &mut Context<Self>) {
        self.new_event(cx);
    }

    /// `"day"`, `"week"` or `"month"`.
    pub(crate) fn action_show(&mut self, view: &str, cx: &mut Context<Self>) {
        let kind = match view {
            "day" => ViewKind::Day,
            "month" => ViewKind::Month,
            _ => ViewKind::Week,
        };
        self.set_kind(kind, cx);
    }
}
