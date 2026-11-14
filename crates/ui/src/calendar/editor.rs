//! Event editor, in its own window: title, location, start/end, all-day, time zone,
//! recurrence (presets + custom dialog), attendees with Find a Time, reminder, show-as,
//! private, categories, description; invitation answers for events organized by others.

use chrono::{
    DateTime, Datelike, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Utc,
    Weekday,
};
use gpui::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::calendar::{
    self as cal, BusySlot, CalendarCommand, EventDetails, Occurrence, ShowAs,
};
use mailbox_core::recurrence::{
    Frequency, RRule, WeekdayNum, month_name, ordinal_name, to_utc, weekday_long,
    weekday_position, weekday_short,
};
use mailbox_core::{Address, Attendee, Calendar, CalendarEvent, Id, ResponseStatus};

use super::sidebar::switch;
use crate::app_state::AppState;
use crate::components::{Avatar, Badge, Button, Icon, IconButton, IconName};
use crate::theme::{
    RADIUS_FULL, RADIUS_LG, RADIUS_MD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_6, TEXT_LG,
    TEXT_MD, TEXT_SM, TEXT_XL, TEXT_XS, Theme, hex, theme,
};

/// What the editor edits.
#[derive(Debug, Clone)]
pub enum EditorTarget {
    New {
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        all_day: bool,
        calendar_id: Option<Id>,
        title: String,
    },
    /// A one-off event, a whole series, or a stored modified occurrence.
    Event { id: Id },
    /// One instance of a series: saving creates a modified occurrence.
    Occurrence { occurrence: Occurrence },
}

/// Opens the editor in a new window.
pub fn open_event_editor(state: Entity<AppState>, target: EditorTarget, cx: &mut App) {
    let title = match &target {
        EditorTarget::New { .. } => "New event",
        EditorTarget::Event { .. } => "Edit event",
        EditorTarget::Occurrence { .. } => "Edit occurrence",
    };
    cx.defer(move |cx| {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(820.), px(780.)), cx)),
            window_min_size: Some(size(px(640.), px(520.))),
            titlebar: Some(TitlebarOptions {
                title: Some(title.into()),
                ..Default::default()
            }),
            focus: true,
            ..Default::default()
        };
        let for_window = state.clone();
        let opened = gpui::open_window(options, cx, move |window, cx| {
            cx.new(|cx| EventEditor::new(for_window, target, window, cx))
        });
        if let Err(err) = opened {
            state.update(cx, |state, cx| {
                state.report_error(format!("Could not open the event editor: {err}"), cx)
            });
        }
    });
}

const REMINDER_CHOICES: &[(Option<i64>, &str)] = &[
    (None, "None"),
    (Some(0), "At start"),
    (Some(5), "5 min"),
    (Some(15), "15 min"),
    (Some(30), "30 min"),
    (Some(60), "1 hour"),
    (Some(24 * 60), "1 day"),
];

/// Find a Time shows these hours.
const FIND_FROM_HOUR: i64 = 7;
const FIND_TO_HOUR: i64 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecurrenceEnd {
    Never,
    Count,
    Until,
}

struct RecurrenceDraft {
    freq: Frequency,
    weekdays: Vec<Weekday>,
    /// Monthly/yearly "on the 2nd Tuesday" instead of "on day 10".
    by_weekday: bool,
    end: RecurrenceEnd,
    interval: Entity<InputState>,
    count: Entity<InputState>,
    until: Entity<InputState>,
}

pub struct EventEditor {
    state: Entity<AppState>,
    target: EditorTarget,
    focus: FocusHandle,
    event_id: Id,
    details: EventDetails,
    calendars: Vec<Calendar>,
    my_emails: Vec<String>,
    contacts: Vec<Address>,
    category_colors: Vec<(String, u32)>,

    title: Entity<InputState>,
    location: Entity<InputState>,
    start_date: Entity<InputState>,
    start_time: Entity<InputState>,
    end_date: Entity<InputState>,
    end_time: Entity<InputState>,
    attendee_input: Entity<InputState>,
    tz_query: Entity<InputState>,
    description: Entity<TextareaState>,

    calendar_id: Id,
    all_day: bool,
    time_zone: Option<String>,
    tz_open: bool,
    recurrence: Option<RRule>,
    recurrence_dialog: Option<RecurrenceDraft>,
    attendees: Vec<Attendee>,
    attendee_query: String,
    reminder: Option<i64>,
    show_as: ShowAs,
    private: bool,
    categories: Vec<String>,
    find_time: Option<NaiveDate>,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

// ---------- parsing helpers ----------

fn parse_date(value: &str) -> Option<NaiveDate> {
    let value = value.trim();
    ["%Y-%m-%d", "%d/%m/%Y", "%d.%m.%Y", "%Y/%m/%d"]
        .iter()
        .find_map(|format| NaiveDate::parse_from_str(value, format).ok())
}

fn parse_time(value: &str) -> Option<NaiveTime> {
    let value = value.trim().to_ascii_lowercase().replace('h', ":");
    let value = value.trim_end_matches(':');
    ["%H:%M", "%H%M", "%H", "%I:%M%p", "%I:%M %p", "%I%p", "%I %p"]
        .iter()
        .find_map(|format| NaiveTime::parse_from_str(value, format).ok())
        .or_else(|| {
            // "9" or "930" without separator.
            let digits: String = value.chars().filter(char::is_ascii_digit).collect();
            match digits.len() {
                1 | 2 => NaiveTime::from_hms_opt(digits.parse().ok()?, 0, 0),
                3 | 4 => {
                    let n: u32 = digits.parse().ok()?;
                    NaiveTime::from_hms_opt(n / 100, n % 100, 0)
                }
                _ => None,
            }
        })
}

/// Wall-clock time in the event zone (`None` = system zone).
fn utc_to_zone(time: DateTime<Utc>, zone: Option<&str>) -> NaiveDateTime {
    match zone.and_then(cal::parse_tz) {
        Some(tz) => time.with_timezone(&tz).naive_local(),
        None => time.with_timezone(&Local).naive_local(),
    }
}

fn zone_to_utc(local: NaiveDateTime, zone: Option<&str>) -> DateTime<Utc> {
    match zone.and_then(cal::parse_tz) {
        Some(tz) => to_utc(&tz, local),
        None => to_utc(&Local, local),
    }
}

/// "Name <email>" or a bare email.
fn parse_address(text: &str) -> Option<Address> {
    let text = text.trim();
    let (name, email) = match (text.find('<'), text.rfind('>')) {
        (Some(open), Some(close)) if open < close => {
            (text[..open].trim().trim_matches('"'), text[open + 1..close].trim())
        }
        _ => ("", text),
    };
    let valid = email.contains('@')
        && email.rsplit('@').next().is_some_and(|domain| domain.contains('.'))
        && !email.contains(' ');
    valid.then(|| Address::new(name, email))
}

/// Recurrence presets offered as chips for a start date.
fn presets(start: NaiveDate) -> Vec<(String, Option<RRule>)> {
    let weekday = start.weekday();
    let (nth, _) = weekday_position(start);
    let ordinal = if nth >= 5 { -1 } else { nth };
    vec![
        ("Never".to_owned(), None),
        ("Daily".to_owned(), Some(RRule::new(Frequency::Daily))),
        (
            "Weekdays".to_owned(),
            Some(RRule::new(Frequency::Weekly).by_day(
                [Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Fri]
                    .map(WeekdayNum::every),
            )),
        ),
        (
            format!("Weekly on {}", weekday_short(weekday)),
            Some(RRule::new(Frequency::Weekly).by_day([WeekdayNum::every(weekday)])),
        ),
        (
            format!("Monthly on day {}", start.day()),
            Some(RRule::new(Frequency::Monthly).by_month_day([start.day() as i8])),
        ),
        (
            format!("Monthly on the {} {}", ordinal_name(ordinal), weekday_short(weekday)),
            Some(RRule::new(Frequency::Monthly).by_day([WeekdayNum::nth(ordinal, weekday)])),
        ),
        (
            format!("Yearly on {} {}", start.day(), &month_name(start.month())[..3]),
            Some(RRule::new(Frequency::Yearly)),
        ),
    ]
}

impl EventEditor {
    pub fn new(
        state: Entity<AppState>,
        target: EditorTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (event, details, calendars, my_emails, contacts, category_colors) = {
            let store = state.read(cx).store();
            let calendars = store.list_calendars().unwrap_or_default();
            let accounts = store.list_accounts().unwrap_or_default();
            let blank = |calendar_id: Id, start: DateTime<Utc>, end: DateTime<Utc>| CalendarEvent {
                id: 0,
                calendar_id,
                title: String::new(),
                location: String::new(),
                description: String::new(),
                start,
                end,
                all_day: false,
                recurrence: None,
                attendees: Vec::new(),
                reminders: vec![15],
                categories: Vec::new(),
            };
            let first_calendar = calendars.first().map_or(0, |calendar| calendar.id);
            let (event, details) = match &target {
                EditorTarget::New {
                    start,
                    end,
                    all_day,
                    calendar_id,
                    title,
                } => {
                    let mut event = blank(calendar_id.unwrap_or(first_calendar), *start, *end);
                    event.all_day = *all_day;
                    event.title = title.clone();
                    if *all_day {
                        event.reminders.clear();
                    }
                    (event, EventDetails::default_for(0))
                }
                EditorTarget::Event { id } => {
                    let event = store
                        .get::<CalendarEvent>(*id)
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| blank(first_calendar, Utc::now(), Utc::now()));
                    let details = cal::details(store, *id)
                        .unwrap_or_else(|_| EventDetails::default_for(*id));
                    (event, details)
                }
                EditorTarget::Occurrence { occurrence } => {
                    (occurrence.event.clone(), occurrence.details.clone())
                }
            };
            let contacts = store
                .list_contacts()
                .unwrap_or_default()
                .into_iter()
                .flat_map(|contact| {
                    contact
                        .emails
                        .iter()
                        .map(|email| Address::new(contact.display_name.clone(), email.clone()))
                        .collect::<Vec<_>>()
                })
                .collect();
            let categories = store
                .list_categories()
                .unwrap_or_default()
                .into_iter()
                .map(|category| (category.name, category.color))
                .collect();
            (
                event,
                details,
                calendars,
                accounts.into_iter().map(|account| account.email).collect(),
                contacts,
                categories,
            )
        };

        let zone = details.time_zone.clone();
        let (start_local, end_local) = if event.all_day {
            let last = event.end - Duration::days(1);
            (event.start.naive_utc(), last.max(event.start).naive_utc())
        } else {
            (
                utc_to_zone(event.start, zone.as_deref()),
                utc_to_zone(event.end, zone.as_deref()),
            )
        };
        let mut text_input = |value: String, placeholder: &'static str| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        let title = text_input(event.title.clone(), "Add a title");
        let location = text_input(event.location.clone(), "Add a location or a meeting link");
        let start_date = text_input(start_local.format("%Y-%m-%d").to_string(), "YYYY-MM-DD");
        let start_time = text_input(start_local.format("%H:%M").to_string(), "HH:MM");
        let end_date = text_input(end_local.format("%Y-%m-%d").to_string(), "YYYY-MM-DD");
        let end_time = text_input(end_local.format("%H:%M").to_string(), "HH:MM");
        let attendee_input = text_input(String::new(), "Invite people: name or email, then Enter");
        let tz_query = text_input(String::new(), "Search time zones");
        let description = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(6)
                .placeholder("Add a description")
                .default_value(event.description.clone())
        });

        let subscriptions = vec![
            cx.subscribe_in(&title, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.save(window, cx);
                }
            }),
            cx.subscribe_in(
                &attendee_input,
                window,
                |this, input, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        this.attendee_query = input.read(cx).value().trim().to_owned();
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => {
                        let text = input.read(cx).value().to_string();
                        match parse_address(&text) {
                            Some(address) => {
                                this.add_attendee(address, cx);
                                input.update(cx, |input, cx| input.set_value("", window, cx));
                                this.attendee_query.clear();
                            }
                            None if !text.trim().is_empty() => {
                                // Pick the first suggestion when the text is a name.
                                if let Some(address) = this.suggestions().into_iter().next() {
                                    this.add_attendee(address, cx);
                                    input.update(cx, |input, cx| input.set_value("", window, cx));
                                    this.attendee_query.clear();
                                } else {
                                    this.error = Some("Enter a valid email address".into());
                                    cx.notify();
                                }
                            }
                            None => {}
                        }
                    }
                    _ => {}
                },
            ),
            cx.subscribe_in(&tz_query, window, |_, _, event: &InputEvent, _, cx| {
                if let InputEvent::Change = event {
                    cx.notify();
                }
            }),
        ];
        title.update(cx, |input, cx| input.focus(window, cx));

        Self {
            state,
            focus: cx.focus_handle(),
            event_id: event.id,
            calendar_id: event.calendar_id,
            all_day: event.all_day,
            time_zone: zone,
            tz_open: false,
            recurrence: event
                .recurrence
                .as_deref()
                .and_then(|rule| RRule::parse(rule).ok()),
            recurrence_dialog: None,
            attendees: event.attendees.clone(),
            attendee_query: String::new(),
            reminder: event.reminders.iter().copied().max(),
            show_as: details.show_as,
            private: details.private,
            categories: event.categories.clone(),
            find_time: None,
            error: None,
            details,
            target,
            calendars,
            my_emails,
            contacts,
            category_colors,
            title,
            location,
            start_date,
            start_time,
            end_date,
            end_time,
            attendee_input,
            tz_query,
            description,
            _subscriptions: subscriptions,
        }
    }

    fn is_occurrence(&self) -> bool {
        matches!(self.target, EditorTarget::Occurrence { .. }) || self.details.master_id.is_some()
    }

    fn is_mine(&self, email: &str) -> bool {
        self.my_emails.iter().any(|mine| mine.eq_ignore_ascii_case(email))
    }

    /// The user's own address on this event's calendar.
    fn my_address(&self, cx: &App) -> Option<Address> {
        let calendar = self.calendars.iter().find(|c| c.id == self.calendar_id)?;
        let store = self.state.read(cx).store();
        let account = store
            .list_accounts()
            .ok()?
            .into_iter()
            .find(|account| account.id == calendar.account_id)?;
        Some(Address::new(account.name, account.email))
    }

    /// Invited by someone else: (organizer, my attendee entry).
    fn invitation(&self) -> Option<(Address, Attendee)> {
        let organizer = self.details.organizer.clone()?;
        if self.is_mine(&organizer.email) {
            return None;
        }
        let me = self
            .attendees
            .iter()
            .find(|attendee| self.is_mine(&attendee.address.email))?
            .clone();
        Some((organizer, me))
    }

    fn suggestions(&self) -> Vec<Address> {
        let query = self.attendee_query.to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        self.contacts
            .iter()
            .filter(|contact| {
                contact.name.to_lowercase().contains(&query)
                    || contact.email.to_lowercase().contains(&query)
            })
            .filter(|contact| {
                !self
                    .attendees
                    .iter()
                    .any(|a| a.address.email.eq_ignore_ascii_case(&contact.email))
            })
            .take(5)
            .cloned()
            .collect()
    }

    fn add_attendee(&mut self, address: Address, cx: &mut Context<Self>) {
        if !self
            .attendees
            .iter()
            .any(|a| a.address.email.eq_ignore_ascii_case(&address.email))
        {
            self.attendees.push(Attendee {
                address,
                status: ResponseStatus::NeedsAction,
                optional: false,
            });
        }
        self.error = None;
        cx.notify();
    }

    fn value(&self, input: &Entity<InputState>, cx: &App) -> String {
        input.read(cx).value().trim().to_owned()
    }

    fn start_date_value(&self, cx: &App) -> Option<NaiveDate> {
        parse_date(&self.value(&self.start_date, cx))
    }

    /// Start/end in UTC from the inputs.
    fn times(&self, cx: &App) -> Result<(DateTime<Utc>, DateTime<Utc>), String> {
        let start_date = self
            .start_date_value(cx)
            .ok_or("Start date: use YYYY-MM-DD")?;
        let end_date =
            parse_date(&self.value(&self.end_date, cx)).ok_or("End date: use YYYY-MM-DD")?;
        let (start, end) = if self.all_day {
            (
                cal::all_day_start(start_date),
                cal::all_day_start(end_date + Duration::days(1)),
            )
        } else {
            let start_time =
                parse_time(&self.value(&self.start_time, cx)).ok_or("Start time: use HH:MM")?;
            let end_time =
                parse_time(&self.value(&self.end_time, cx)).ok_or("End time: use HH:MM")?;
            let zone = self.time_zone.as_deref();
            (
                zone_to_utc(start_date.and_time(start_time), zone),
                zone_to_utc(end_date.and_time(end_time), zone),
            )
        };
        if end < start {
            return Err("The event ends before it starts".into());
        }
        Ok((start, end))
    }

    fn build(&self, cx: &App) -> Result<(CalendarEvent, EventDetails), String> {
        let (start, end) = self.times(cx)?;
        let organizer = match (&self.details.organizer, self.attendees.is_empty()) {
            (Some(organizer), _) => Some(organizer.clone()),
            (None, false) => self.my_address(cx),
            (None, true) => None,
        };
        let event = CalendarEvent {
            id: if matches!(self.target, EditorTarget::New { .. }) {
                0
            } else {
                self.event_id
            },
            calendar_id: self.calendar_id,
            title: self.value(&self.title, cx),
            location: self.value(&self.location, cx),
            description: self.description.read(cx).value().to_string(),
            start,
            end,
            all_day: self.all_day,
            recurrence: if self.is_occurrence() {
                None
            } else {
                self.recurrence.as_ref().map(ToString::to_string)
            },
            attendees: self.attendees.clone(),
            reminders: self.reminder.into_iter().collect(),
            categories: self.categories.clone(),
        };
        let details = EventDetails {
            time_zone: if self.all_day {
                None
            } else {
                self.time_zone.clone()
            },
            show_as: self.show_as,
            private: self.private,
            organizer,
            ..self.details.clone()
        };
        Ok((event, details))
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (event, details) = match self.build(cx) {
            Ok(built) => built,
            Err(message) => {
                self.error = Some(message.into());
                cx.notify();
                return;
            }
        };
        let command = match &self.target {
            EditorTarget::Occurrence { occurrence } if !occurrence.is_exception() => {
                CalendarCommand::SaveOccurrence {
                    series_id: occurrence.event.id,
                    occurrence: occurrence.series_start.unwrap_or(occurrence.event.start),
                    event: Box::new(CalendarEvent { id: 0, ..event }),
                    details: Box::new(EventDetails {
                        exdates: Vec::new(),
                        ..details
                    }),
                }
            }
            _ => CalendarCommand::Save {
                event: Box::new(event),
                details: Box::new(details),
            },
        };
        let ok = self
            .state
            .update(cx, |state, cx| state.run(command.into_command(), cx));
        if ok {
            window.remove_window();
        }
    }

    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let command = match &self.target {
            EditorTarget::New { .. } => {
                window.remove_window();
                return;
            }
            EditorTarget::Event { id } => CalendarCommand::Delete { id: *id },
            EditorTarget::Occurrence { occurrence } if occurrence.is_exception() => {
                CalendarCommand::Delete {
                    id: occurrence.event.id,
                }
            }
            EditorTarget::Occurrence { occurrence } => CalendarCommand::SkipOccurrence {
                series_id: occurrence.event.id,
                occurrence: occurrence.series_start.unwrap_or(occurrence.event.start),
            },
        };
        let ok = self
            .state
            .update(cx, |state, cx| state.run(command.into_command(), cx));
        if ok {
            window.remove_window();
        }
    }

    fn respond(&mut self, status: ResponseStatus, cx: &mut Context<Self>) {
        let Some((_, me)) = self.invitation() else {
            return;
        };
        let id = match &self.target {
            EditorTarget::Occurrence { occurrence } => occurrence.series_id(),
            _ => self.event_id,
        };
        let ok = self.state.update(cx, |state, cx| {
            state.run(
                CalendarCommand::Respond {
                    id,
                    email: me.address.email.clone(),
                    status,
                }
                .into_command(),
                cx,
            )
        });
        if ok {
            if let Some(attendee) = self
                .attendees
                .iter_mut()
                .find(|a| a.address.email.eq_ignore_ascii_case(&me.address.email))
            {
                attendee.status = status;
            }
            self.show_as = match status {
                ResponseStatus::Tentative => ShowAs::Tentative,
                ResponseStatus::Declined => ShowAs::Free,
                _ => ShowAs::Busy,
            };
            cx.notify();
        }
    }

    /// Sets the start/end inputs (keeping them in the event zone).
    fn set_times(
        &mut self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let zone = self.time_zone.clone();
        let start = utc_to_zone(start, zone.as_deref());
        let end = utc_to_zone(end, zone.as_deref());
        let set = |input: &Entity<InputState>, value: String, window: &mut Window, cx: &mut Context<Self>| {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        };
        set(&self.start_date, start.format("%Y-%m-%d").to_string(), window, cx);
        set(&self.start_time, start.format("%H:%M").to_string(), window, cx);
        set(&self.end_date, end.format("%Y-%m-%d").to_string(), window, cx);
        set(&self.end_time, end.format("%H:%M").to_string(), window, cx);
        self.error = None;
        cx.notify();
    }

    // ---------- recurrence dialog ----------

    fn open_recurrence_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let start = self
            .start_date_value(cx)
            .unwrap_or_else(|| Local::now().date_naive());
        let rule = self.recurrence.clone().unwrap_or_else(|| {
            RRule::new(Frequency::Weekly).by_day([WeekdayNum::every(start.weekday())])
        });
        let interval = rule.interval.to_string();
        let count = rule.count.unwrap_or(10).to_string();
        let until = rule
            .until
            .map(|until| until.date_naive())
            .unwrap_or_else(|| start + Duration::days(90))
            .format("%Y-%m-%d")
            .to_string();
        let mut input = |value: String| {
            cx.new(|cx| InputState::new(window, cx).default_value(value))
        };
        let draft = RecurrenceDraft {
            freq: rule.freq,
            weekdays: if rule.by_day.is_empty() {
                vec![start.weekday()]
            } else {
                rule.by_day.iter().map(|day| day.weekday).collect()
            },
            by_weekday: rule.by_day.iter().any(|day| day.ordinal.is_some())
                || !rule.by_set_pos.is_empty(),
            end: if rule.count.is_some() {
                RecurrenceEnd::Count
            } else if rule.until.is_some() {
                RecurrenceEnd::Until
            } else {
                RecurrenceEnd::Never
            },
            interval: input(interval),
            count: input(count),
            until: input(until),
        };
        self.recurrence_dialog = Some(draft);
        cx.notify();
    }

    fn draft_rule(&self, draft: &RecurrenceDraft, cx: &App) -> Result<RRule, String> {
        let start = self.start_date_value(cx).ok_or("Set a valid start date first")?;
        let interval: u32 = self
            .value(&draft.interval, cx)
            .parse()
            .ok()
            .filter(|n| *n >= 1)
            .ok_or("Repeat every: enter a number")?;
        let (nth, _) = weekday_position(start);
        let ordinal = if nth >= 5 { -1 } else { nth };
        let mut rule = RRule::new(draft.freq).interval(interval);
        match draft.freq {
            Frequency::Daily => {}
            Frequency::Weekly => {
                let mut days = draft.weekdays.clone();
                if days.is_empty() {
                    days.push(start.weekday());
                }
                days.sort_by_key(|day| day.num_days_from_monday());
                days.dedup();
                rule = rule.by_day(days.into_iter().map(WeekdayNum::every));
            }
            Frequency::Monthly => {
                rule = if draft.by_weekday {
                    rule.by_day([WeekdayNum::nth(ordinal, start.weekday())])
                } else {
                    rule.by_month_day([start.day() as i8])
                };
            }
            Frequency::Yearly => {
                if draft.by_weekday {
                    rule = rule
                        .by_month([start.month()])
                        .by_day([WeekdayNum::nth(ordinal, start.weekday())]);
                }
            }
        }
        match draft.end {
            RecurrenceEnd::Never => {}
            RecurrenceEnd::Count => {
                let count: u32 = self
                    .value(&draft.count, cx)
                    .parse()
                    .ok()
                    .filter(|n| *n >= 1)
                    .ok_or("End after: enter a number of occurrences")?;
                rule = rule.count(count);
            }
            RecurrenceEnd::Until => {
                let date = parse_date(&self.value(&draft.until, cx)).ok_or("End by: use YYYY-MM-DD")?;
                let last = date.and_hms_opt(23, 59, 59).unwrap_or_default();
                rule = rule.until(zone_to_utc(last, self.time_zone.as_deref()));
            }
        }
        Ok(rule)
    }

    fn apply_recurrence_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.recurrence_dialog.as_ref() else {
            return;
        };
        match self.draft_rule(draft, cx) {
            Ok(rule) => {
                self.recurrence = Some(rule);
                self.recurrence_dialog = None;
                self.error = None;
            }
            Err(message) => self.error = Some(message.into()),
        }
        cx.notify();
    }
}

// ---------- rendering ----------

fn chip(id: impl Into<ElementId>, label: impl Into<SharedString>, active: bool, t: &Theme) -> Stateful<Div> {
    chip_base(id, active, t).child(label.into())
}

/// A chip with a leading colour dot (calendars, categories).
fn chip_with_dot(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    active: bool,
    color: Hsla,
    t: &Theme,
) -> Stateful<Div> {
    chip_base(id, active, t)
        .child(div().size(px(8.)).rounded(RADIUS_FULL).bg(color))
        .child(label.into())
}

fn chip_base(id: impl Into<ElementId>, active: bool, t: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(SPACE_1)
        .h(px(26.))
        .px(SPACE_2)
        .rounded(RADIUS_MD)
        .border_1()
        .text_size(TEXT_SM)
        .cursor_pointer()
        .when(active, |el| {
            el.border_color(t.accent)
                .bg(t.accent_subtle)
                .text_color(t.accent)
                .font_weight(FontWeight::MEDIUM)
        })
        .when(!active, |el| {
            el.border_color(t.border)
                .text_color(t.text_muted)
                .hover(|style| style.bg(t.hover).text_color(t.text))
        })
}

fn field_row(label: &'static str, icon: IconName, t: &Theme, content: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_row()
        .items_start()
        .gap(SPACE_3)
        .child(
            div()
                .flex()
                .flex_row()
                .flex_shrink_0()
                .items_center()
                .gap(SPACE_2)
                .w(px(112.))
                .h(px(30.))
                .text_size(TEXT_SM)
                .text_color(t.text_muted)
                .child(Icon::new(icon).size(px(14.)))
                .child(label),
        )
        .child(div().flex_1().min_w(px(0.)).child(content))
}

fn small_input(input: &Entity<InputState>, width: f32) -> Div {
    div().w(px(width)).child(Input::new(input))
}

impl EventEditor {
    fn render_header(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let color = self
            .calendars
            .iter()
            .find(|calendar| calendar.id == self.calendar_id)
            .map_or(t.accent, |calendar| hex(calendar.color));
        let heading = match &self.target {
            EditorTarget::New { .. } => "New event",
            EditorTarget::Event { .. } if self.recurrence.is_some() => "Edit series",
            EditorTarget::Event { .. } => "Edit event",
            EditorTarget::Occurrence { .. } => "Edit occurrence",
        };
        let is_new = matches!(self.target, EditorTarget::New { .. });
        div()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .items_center()
            .gap(SPACE_2)
            .h(px(56.))
            .px(SPACE_6)
            .border_b_1()
            .border_color(t.border_subtle)
            .child(div().size(px(10.)).rounded(RADIUS_FULL).bg(color))
            .child(
                div()
                    .flex_1()
                    .text_size(TEXT_LG)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(heading),
            )
            .when(!is_new, |el| {
                el.child(
                    Button::new("editor-delete")
                        .ghost()
                        .icon(IconName::Trash)
                        .label(if self.is_occurrence() {
                            "Delete occurrence"
                        } else {
                            "Delete"
                        })
                        .on_click(cx.listener(|this, _, window, cx| this.delete(window, cx))),
                )
            })
            .child(
                Button::new("editor-cancel")
                    .label("Cancel")
                    .on_click(cx.listener(|_, _, window, _| window.remove_window())),
            )
            .child(
                Button::new("editor-save")
                    .primary()
                    .icon(IconName::Check)
                    .label("Save")
                    .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
            )
    }

    fn render_invitation(&self, t: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (organizer, me) = self.invitation()?;
        let answer = |status: ResponseStatus, label: &'static str, icon: IconName| {
            let active = me.status == status;
            chip(("respond", status as u64), label, active, t)
                .child(Icon::new(icon).size(px(12.)))
                .on_click(cx.listener(move |this, _, _, cx| this.respond(status, cx)))
        };
        Some(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(SPACE_2)
                .mx(SPACE_6)
                .mt(SPACE_4)
                .p(SPACE_3)
                .rounded(RADIUS_LG)
                .bg(t.accent_subtle)
                .child(Avatar::new(organizer.display().to_owned()).size(px(28.)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .child(
                            div()
                                .text_size(TEXT_MD)
                                .font_weight(FontWeight::MEDIUM)
                                .child(format!("{} invited you", organizer.display())),
                        )
                        .child(
                            div()
                                .text_size(TEXT_XS)
                                .text_color(t.text_muted)
                                .child(match me.status {
                                    ResponseStatus::NeedsAction => "You have not answered yet",
                                    ResponseStatus::Accepted => "You accepted",
                                    ResponseStatus::Tentative => "You tentatively accepted",
                                    ResponseStatus::Declined => "You declined",
                                }),
                        ),
                )
                .child(answer(ResponseStatus::Accepted, "Accept", IconName::Check))
                .child(answer(ResponseStatus::Tentative, "Tentative", IconName::CircleQuestionMark))
                .child(answer(ResponseStatus::Declined, "Decline", IconName::X)),
        )
    }

    fn render_when(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let zone_label = match &self.time_zone {
            Some(zone) => format!("{zone} ({})", cal::utc_offset_label(zone, Utc::now())),
            None => format!("System time ({})", Local::now().format("UTC%:z")),
        };
        let all_day = self.all_day;
        let zones: Vec<&'static str> = if self.tz_open {
            let query = self.tz_query.read(cx).value().to_lowercase().replace(' ', "_");
            cal::time_zone_names()
                .into_iter()
                .filter(|name| name.to_lowercase().contains(&query))
                .take(60)
                .collect()
        } else {
            Vec::new()
        };
        let current_zone = self.time_zone.clone();
        let zone_rows: Vec<AnyElement> = std::iter::once(None)
            .chain(zones.into_iter().map(Some))
            .enumerate()
            .map(|(index, zone)| {
                let active = current_zone.as_deref() == zone;
                let label = zone.map_or_else(
                    || "System time".to_owned(),
                    |name| format!("{name}  ·  {}", cal::utc_offset_label(name, Utc::now())),
                );
                let value = zone.map(str::to_owned);
                div()
                    .id(("tz", index as u64))
                    .px(SPACE_2)
                    .py(px(5.))
                    .rounded(RADIUS_MD)
                    .text_size(TEXT_SM)
                    .cursor_pointer()
                    .when(active, |el| el.bg(t.accent_subtle).text_color(t.accent))
                    .when(!active, |el| el.hover(|style| style.bg(t.hover)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.time_zone = value.clone();
                        this.tz_open = false;
                        cx.notify();
                    }))
                    .child(label)
                    .into_any_element()
            })
            .collect();

        div()
            .flex()
            .flex_col()
            .gap(SPACE_2)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(SPACE_2)
                    .child(small_input(&self.start_date, 120.))
                    .when(!all_day, |el| el.child(small_input(&self.start_time, 76.)))
                    .child(Icon::new(IconName::ArrowRight).size(px(14.)).text_color(t.text_subtle))
                    .child(small_input(&self.end_date, 120.))
                    .when(!all_day, |el| el.child(small_input(&self.end_time, 76.)))
                    .child(
                        div()
                            .id("all-day-toggle")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(SPACE_2)
                            .ml(SPACE_2)
                            .px(SPACE_2)
                            .h(px(28.))
                            .rounded(RADIUS_MD)
                            .cursor_pointer()
                            .hover(|style| style.bg(t.hover))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.all_day = !this.all_day;
                                cx.notify();
                            }))
                            .child(switch(all_day, t))
                            .child(div().text_size(TEXT_SM).child("All day")),
                    ),
            )
            .when(!all_day, |el| {
                el.child(
                    div()
                        .id("tz-toggle")
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(SPACE_1)
                        .w_full()
                        .text_size(TEXT_XS)
                        .text_color(t.text_muted)
                        .cursor_pointer()
                        .hover(|style| style.text_color(t.text))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.tz_open = !this.tz_open;
                            cx.notify();
                        }))
                        .child(Icon::new(IconName::Globe).size(px(12.)))
                        .child(zone_label)
                        .child(Icon::new(IconName::ChevronDown).size(px(12.))),
                )
            })
            .when(self.tz_open && !all_day, |el| {
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(SPACE_1)
                        .p(SPACE_2)
                        .rounded(RADIUS_LG)
                        .border_1()
                        .border_color(t.border)
                        .bg(t.elevated)
                        .child(Input::new(&self.tz_query))
                        .child(
                            div()
                                .id("tz-list")
                                .flex()
                                .flex_col()
                                .h(px(180.))
                                .overflow_y_scroll()
                                .children(zone_rows),
                        ),
                )
            })
    }

    fn render_repeat(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let start = self
            .start_date_value(cx)
            .unwrap_or_else(|| Local::now().date_naive());
        let options = presets(start);
        let matched = options
            .iter()
            .any(|(_, rule)| *rule == self.recurrence);
        let chips: Vec<AnyElement> = options
            .into_iter()
            .enumerate()
            .map(|(index, (label, rule))| {
                let active = rule == self.recurrence;
                chip(("repeat", index as u64), label, active, t)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.recurrence = rule.clone();
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .gap(SPACE_2)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap(SPACE_1)
                    .children(chips)
                    .child(
                        chip("repeat-custom", "Custom…", !matched, t)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_recurrence_dialog(window, cx)
                            })),
                    ),
            )
            .when_some(self.recurrence.as_ref(), |el, rule| {
                el.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(SPACE_1)
                        .text_size(TEXT_XS)
                        .text_color(t.text_muted)
                        .child(Icon::new(IconName::Repeat).size(px(12.)))
                        .child(rule.describe()),
                )
            })
    }

    fn render_attendees(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let suggestions: Vec<AnyElement> = self
            .suggestions()
            .into_iter()
            .enumerate()
            .map(|(index, address)| {
                let label = format!("{} <{}>", address.display(), address.email);
                div()
                    .id(("suggestion", index as u64))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(SPACE_2)
                    .px(SPACE_2)
                    .py(px(4.))
                    .rounded(RADIUS_MD)
                    .cursor_pointer()
                    .hover(|style| style.bg(t.hover))
                    .child(Avatar::new(address.display().to_owned()).size(px(20.)))
                    .child(div().text_size(TEXT_SM).child(label))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.add_attendee(address.clone(), cx);
                        this.attendee_query.clear();
                        this.attendee_input
                            .update(cx, |input, cx| input.set_value("", window, cx));
                    }))
                    .into_any_element()
            })
            .collect();
        let rows: Vec<AnyElement> = self
            .attendees
            .iter()
            .enumerate()
            .map(|(index, attendee)| {
                let email = attendee.address.email.clone();
                let toggle_email = email.clone();
                let (status_label, status_color) = match attendee.status {
                    ResponseStatus::Accepted => ("Accepted", t.success),
                    ResponseStatus::Tentative => ("Tentative", t.warning),
                    ResponseStatus::Declined => ("Declined", t.danger),
                    ResponseStatus::NeedsAction => ("Awaiting", t.text_muted),
                };
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(SPACE_2)
                    .py(px(3.))
                    .child(Avatar::new(attendee.address.display().to_owned()).size(px(24.)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(TEXT_SM)
                                    .child(attendee.address.display().to_owned()),
                            )
                            .when(!attendee.address.name.is_empty(), |el| {
                                el.child(
                                    div()
                                        .truncate()
                                        .text_size(TEXT_XS)
                                        .text_color(t.text_muted)
                                        .child(email.clone()),
                                )
                            }),
                    )
                    .child(Badge::new(status_label).color(status_color))
                    .child(
                        chip(("optional", index as u64), if attendee.optional { "Optional" } else { "Required" }, attendee.optional, t)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(a) = this
                                    .attendees
                                    .iter_mut()
                                    .find(|a| a.address.email == toggle_email)
                                {
                                    a.optional = !a.optional;
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        IconButton::new(("remove-attendee", index as u64), IconName::X)
                            .size(px(24.))
                            .icon_size(px(12.))
                            .tooltip("Remove")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.attendees.retain(|a| a.address.email != email);
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        let find_open = self.find_time.is_some();

        div()
            .flex()
            .flex_col()
            .gap(SPACE_2)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(SPACE_2)
                    .child(div().flex_1().child(Input::new(&self.attendee_input)))
                    .child(
                        Button::new("find-time")
                            .icon(IconName::CalendarClock)
                            .label("Find a time")
                            .variant(if find_open {
                                crate::components::ButtonVariant::Primary
                            } else {
                                crate::components::ButtonVariant::Secondary
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.find_time = match this.find_time {
                                    Some(_) => None,
                                    None => Some(
                                        this.start_date_value(cx)
                                            .unwrap_or_else(|| Local::now().date_naive()),
                                    ),
                                };
                                cx.notify();
                            })),
                    ),
            )
            .when(!suggestions.is_empty(), |el| {
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .p(SPACE_1)
                        .rounded(RADIUS_LG)
                        .border_1()
                        .border_color(t.border)
                        .bg(t.elevated)
                        .shadow_md()
                        .children(suggestions),
                )
            })
            .children(rows)
    }

    fn render_find_time(&self, day: NaiveDate, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let store = self.state.read(cx).store();
        let from = to_utc(&Local, day.and_time(NaiveTime::MIN));
        let to = from + Duration::days(1);
        let editing = self.event_id;
        let others = |list: Vec<Occurrence>| -> Vec<Occurrence> {
            list.into_iter()
                .filter(|o| editing == 0 || (o.event.id != editing && o.series_id() != editing))
                .collect()
        };
        let day_list = others(cal::occurrences(store, from, to).unwrap_or_default());
        let me = self
            .my_address(cx)
            .unwrap_or_else(|| Address::new("You", self.my_emails.first().cloned().unwrap_or_default()));
        let mut people: Vec<Address> = vec![Address::new("You", me.email.clone())];
        people.extend(
            self.attendees
                .iter()
                .filter(|a| !self.is_mine(&a.address.email))
                .map(|a| a.address.clone()),
        );
        let busy: Vec<Vec<BusySlot>> = people
            .iter()
            .map(|person| cal::free_busy(&day_list, &person.email, &self.my_emails))
            .collect();

        // Suggestions over the next 5 days, for the current duration.
        let duration = self
            .times(cx)
            .map(|(start, end)| end - start)
            .ok()
            .filter(|d| *d > Duration::zero() && *d <= Duration::hours(8))
            .unwrap_or(Duration::minutes(30));
        let window_start = from.max(Utc::now());
        let week_list = others(
            cal::occurrences(store, window_start, from + Duration::days(5)).unwrap_or_default(),
        );
        let week_busy: Vec<Vec<BusySlot>> = people
            .iter()
            .map(|person| cal::free_busy(&week_list, &person.email, &self.my_emails))
            .collect();
        let aligned = {
            let minutes = window_start.with_timezone(&Local).minute() as i64;
            window_start + Duration::minutes((30 - minutes % 30) % 30)
                - Duration::seconds(window_start.second() as i64)
        };
        let slots = cal::suggest_slots(
            &week_busy,
            aligned,
            from + Duration::days(5),
            duration,
            Duration::minutes(30),
            (8, 18),
            &Local,
            8,
        );
        // Only outline this event when it falls on the displayed day.
        let current = self
            .times(cx)
            .ok()
            .filter(|(start, _)| start.with_timezone(&Local).date_naive() == day);

        let span = ((FIND_TO_HOUR - FIND_FROM_HOUR) * 60) as f32;
        let position = |time: DateTime<Utc>| {
            let local = time.with_timezone(&Local).naive_local();
            let minutes = (local - day.and_time(NaiveTime::MIN)).num_minutes() - FIND_FROM_HOUR * 60;
            (minutes as f32 / span).clamp(0., 1.)
        };
        let hour_labels = div().flex().flex_row().ml(px(140.)).children(
            (FIND_FROM_HOUR..FIND_TO_HOUR).map(|hour| {
                div()
                    .flex_1()
                    .text_size(px(10.))
                    .text_color(t.text_subtle)
                    .child(format!("{hour:02}"))
            }),
        );
        let rows: Vec<AnyElement> = people
            .iter()
            .zip(&busy)
            .map(|(person, slots)| {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(28.))
                    .child(
                        div()
                            .w(px(140.))
                            .flex_shrink_0()
                            .pr(SPACE_2)
                            .truncate()
                            .text_size(TEXT_SM)
                            .child(person.display().to_owned()),
                    )
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .h(px(20.))
                            .rounded(RADIUS_MD)
                            .bg(t.hover)
                            .children(slots.iter().map(|slot| {
                                let left = position(slot.start);
                                let right = position(slot.end);
                                let color = match slot.show_as {
                                    ShowAs::Tentative => t.warning,
                                    ShowAs::OutOfOffice => hsla(0.8, 0.5, 0.5, 1.),
                                    _ => t.accent,
                                };
                                div()
                                    .absolute()
                                    .top_0()
                                    .h_full()
                                    .left(relative(left))
                                    .w(relative((right - left).max(0.)))
                                    .rounded(px(3.))
                                    .bg(color.alpha(0.7))
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        let highlight = current.map(|(start, end)| {
            let (left, right) = (position(start), position(end));
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(relative(left))
                .w(relative((right - left).max(0.005)))
                .border_2()
                .border_color(t.success)
                .rounded(RADIUS_MD)
        });
        let slot_chips: Vec<AnyElement> = slots
            .into_iter()
            .enumerate()
            .map(|(index, start)| {
                let label = start.with_timezone(&Local).format("%a %-d · %H:%M").to_string();
                chip(("slot", index as u64), label, false, t)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_times(start, start + duration, window, cx);
                        this.find_time = Some(start.with_timezone(&Local).date_naive());
                    }))
                    .into_any_element()
            })
            .collect();

        div()
            .flex()
            .flex_col()
            .gap(SPACE_2)
            .p(SPACE_3)
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(t.border)
            .bg(t.surface)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(SPACE_2)
                    .child(
                        IconButton::new("find-prev", IconName::ChevronLeft)
                            .size(px(24.))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.find_time = Some(day - Duration::days(1));
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .text_size(TEXT_MD)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(day.format("%A %-d %B").to_string()),
                    )
                    .child(
                        IconButton::new("find-next", IconName::ChevronRight)
                            .size(px(24.))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.find_time = Some(day + Duration::days(1));
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(SPACE_3)
                            .text_size(TEXT_XS)
                            .text_color(t.text_muted)
                            .child(legend(t.accent, "Busy"))
                            .child(legend(t.warning, "Tentative"))
                            .child(legend(t.success, "This event")),
                    ),
            )
            .child(hour_labels)
            .child(
                div()
                    .relative()
                    .child(div().flex().flex_col().children(rows))
                    .when_some(highlight, |el, highlight| {
                        el.child(
                            div()
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .left(px(140.))
                                .right_0()
                                .child(highlight),
                        )
                    }),
            )
            .child(
                div()
                    .text_size(TEXT_XS)
                    .text_color(t.text_muted)
                    .child("Suggested times when everyone is free (working hours):"),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap(SPACE_1)
                    .when(slot_chips.is_empty(), |el| {
                        el.child(
                            div()
                                .text_size(TEXT_SM)
                                .text_color(t.text_subtle)
                                .child("No common free slot in the next 5 days."),
                        )
                    })
                    .children(slot_chips),
            )
    }

    fn render_options(&self, t: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let calendars: Vec<AnyElement> = self
            .calendars
            .iter()
            .map(|calendar| {
                let id = calendar.id;
                chip_with_dot(
                    ("calendar", id as u64),
                    calendar.name.clone(),
                    id == self.calendar_id,
                    hex(calendar.color),
                    t,
                )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.calendar_id = id;
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
        let reminders: Vec<AnyElement> = REMINDER_CHOICES
            .iter()
            .enumerate()
            .map(|(index, (minutes, label))| {
                let minutes = *minutes;
                chip(("reminder", index as u64), *label, self.reminder == minutes, t)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.reminder = minutes;
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
        let show_as: Vec<AnyElement> = ShowAs::ALL
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let value = *value;
                chip(("show-as", index as u64), value.label(), self.show_as == value, t)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.show_as = value;
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
        let categories: Vec<AnyElement> = self
            .category_colors
            .iter()
            .enumerate()
            .map(|(index, (name, color))| {
                let active = self.categories.contains(name);
                let name = name.clone();
                chip_with_dot(("category", index as u64), name.clone(), active, hex(*color), t)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.categories.contains(&name) {
                            this.categories.push(name.clone());
                        } else {
                            this.categories.retain(|c| *c != name);
                        }
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
        let wrap = |children: Vec<AnyElement>| div().flex().flex_row().flex_wrap().gap(SPACE_1).children(children);
        let private = self.private;

        vec![
            field_row("Calendar", IconName::Calendar, t, wrap(calendars)).into_any_element(),
            field_row("Reminder", IconName::Bell, t, wrap(reminders)).into_any_element(),
            field_row("Show as", IconName::Clock, t, wrap(show_as)).into_any_element(),
            field_row(
                "Private",
                IconName::Lock,
                t,
                div()
                    .id("private-toggle")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(SPACE_2)
                    .h(px(30.))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.private = !this.private;
                        cx.notify();
                    }))
                    .child(switch(private, t))
                    .child(
                        div()
                            .text_size(TEXT_SM)
                            .text_color(t.text_muted)
                            .child("Others only see that you are busy"),
                    ),
            )
            .into_any_element(),
            field_row("Categories", IconName::Tag, t, wrap(categories)).into_any_element(),
        ]
    }

    fn render_recurrence_dialog(
        &self,
        draft: &RecurrenceDraft,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let start = self
            .start_date_value(cx)
            .unwrap_or_else(|| Local::now().date_naive());
        let (nth, _) = weekday_position(start);
        let ordinal = if nth >= 5 { -1 } else { nth };
        let preview = self
            .draft_rule(draft, cx)
            .map(|rule| rule.describe())
            .unwrap_or_else(|message| message);

        let frequencies: Vec<AnyElement> = Frequency::ALL
            .iter()
            .enumerate()
            .map(|(index, freq)| {
                let freq = *freq;
                let label = match freq {
                    Frequency::Daily => "Day",
                    Frequency::Weekly => "Week",
                    Frequency::Monthly => "Month",
                    Frequency::Yearly => "Year",
                };
                chip(("freq", index as u64), label, draft.freq == freq, t)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(draft) = this.recurrence_dialog.as_mut() {
                            draft.freq = freq;
                        }
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
        let weekdays: Vec<AnyElement> = [
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
            Weekday::Sat,
            Weekday::Sun,
        ]
        .into_iter()
        .map(|day| {
            let active = draft.weekdays.contains(&day);
            div()
                .id(("weekday", day.num_days_from_monday() as u64))
                .flex()
                .items_center()
                .justify_center()
                .size(px(30.))
                .rounded(RADIUS_FULL)
                .text_size(TEXT_SM)
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .when(active, |el| el.bg(t.accent).text_color(t.on_accent))
                .when(!active, |el| {
                    el.bg(t.hover)
                        .text_color(t.text_muted)
                        .hover(|style| style.bg(t.active))
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(draft) = this.recurrence_dialog.as_mut() {
                        if draft.weekdays.contains(&day) {
                            draft.weekdays.retain(|d| *d != day);
                        } else {
                            draft.weekdays.push(day);
                        }
                    }
                    cx.notify();
                }))
                .child(weekday_short(day)[..2].to_owned())
                .into_any_element()
        })
        .collect();
        let by_option = |id: &'static str, by_weekday: bool, label: String| {
            chip(id, label, draft.by_weekday == by_weekday, t).on_click(cx.listener(
                move |this, _, _, cx| {
                    if let Some(draft) = this.recurrence_dialog.as_mut() {
                        draft.by_weekday = by_weekday;
                    }
                    cx.notify();
                },
            ))
        };
        let end_option = |id: &'static str, end: RecurrenceEnd, label: &'static str| {
            chip(id, label, draft.end == end, t).on_click(cx.listener(move |this, _, _, cx| {
                if let Some(draft) = this.recurrence_dialog.as_mut() {
                    draft.end = end;
                }
                cx.notify();
            }))
        };
        let row = |label: &'static str| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(SPACE_2)
                .child(
                    div()
                        .w(px(90.))
                        .text_size(TEXT_SM)
                        .text_color(t.text_muted)
                        .child(label),
                )
        };
        let month_label = &month_name(start.month())[..3];

        div()
            .id("recurrence-backdrop")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(hsla(0., 0., 0., if t.is_dark { 0.45 } else { 0.25 }))
            .occlude()
            .child(
                div()
                    .id("recurrence-dialog")
                    .w(px(500.))
                    .flex()
                    .flex_col()
                    .gap(SPACE_3)
                    .p(SPACE_6)
                    .rounded(px(12.))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.elevated)
                    .shadow_lg()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(SPACE_2)
                            .child(Icon::new(IconName::Repeat).size(px(16.)).text_color(t.accent))
                            .child(
                                div()
                                    .text_size(TEXT_LG)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Custom recurrence"),
                            ),
                    )
                    .child(
                        row("Repeat every")
                            .child(small_input(&draft.interval, 56.))
                            .children(frequencies),
                    )
                    .when(draft.freq == Frequency::Weekly, |el| {
                        el.child(row("On").children(weekdays))
                    })
                    .when(draft.freq == Frequency::Monthly, |el| {
                        el.child(
                            row("On")
                                .child(by_option("by-date", false, format!("Day {}", start.day())))
                                .child(by_option(
                                    "by-weekday",
                                    true,
                                    format!("The {} {}", ordinal_name(ordinal), weekday_long(start.weekday())),
                                )),
                        )
                    })
                    .when(draft.freq == Frequency::Yearly, |el| {
                        el.child(
                            row("On")
                                .child(by_option(
                                    "by-date",
                                    false,
                                    format!("{} {month_label}", start.day()),
                                ))
                                .child(by_option(
                                    "by-weekday",
                                    true,
                                    format!(
                                        "The {} {} of {month_label}",
                                        ordinal_name(ordinal),
                                        weekday_short(start.weekday())
                                    ),
                                )),
                        )
                    })
                    .child(
                        row("Ends")
                            .child(end_option("end-never", RecurrenceEnd::Never, "Never"))
                            .child(end_option("end-count", RecurrenceEnd::Count, "After"))
                            .when(draft.end == RecurrenceEnd::Count, |el| {
                                el.child(small_input(&draft.count, 56.)).child(
                                    div().text_size(TEXT_SM).text_color(t.text_muted).child("times"),
                                )
                            })
                            .child(end_option("end-until", RecurrenceEnd::Until, "On"))
                            .when(draft.end == RecurrenceEnd::Until, |el| {
                                el.child(small_input(&draft.until, 110.))
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(SPACE_2)
                            .p(SPACE_2)
                            .rounded(RADIUS_MD)
                            .bg(t.hover)
                            .text_size(TEXT_SM)
                            .child(Icon::new(IconName::Info).size(px(14.)).text_color(t.text_muted))
                            .child(preview),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(SPACE_2)
                            .pt(SPACE_2)
                            .when(self.recurrence.is_some(), |el| {
                                el.child(Button::new("recurrence-remove").ghost().label("Don't repeat").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.recurrence = None;
                                        this.recurrence_dialog = None;
                                        cx.notify();
                                    }),
                                ))
                            })
                            .child(div().flex_1())
                            .child(Button::new("recurrence-cancel").label("Cancel").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.recurrence_dialog = None;
                                    cx.notify();
                                }),
                            ))
                            .child(
                                Button::new("recurrence-done")
                                    .primary()
                                    .label("Done")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.apply_recurrence_dialog(cx)
                                    })),
                            ),
                    ),
            )
    }
}

fn legend(color: Hsla, label: &'static str) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(SPACE_1)
        .child(div().size(px(8.)).rounded(px(2.)).bg(color.alpha(0.7)))
        .child(label)
}

impl Render for EventEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let header = self.render_header(&t, cx);
        let invitation = self.render_invitation(&t, cx);
        let when = self.render_when(&t, cx);
        let repeat = (!self.is_occurrence()).then(|| self.render_repeat(&t, cx));
        let attendees = self.render_attendees(&t, cx);
        let find_time = self.find_time.map(|day| self.render_find_time(day, &t, cx));
        let options = self.render_options(&t, cx);
        let dialog = self
            .recurrence_dialog
            .as_ref()
            .map(|draft| self.render_recurrence_dialog(draft, &t, cx));

        div()
            .id("event-editor")
            .key_context("EventEditor")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let keystroke = &event.keystroke;
                let primary = keystroke.modifiers.control || keystroke.modifiers.platform;
                match keystroke.key.as_str() {
                    "escape" if this.recurrence_dialog.is_some() => {
                        this.recurrence_dialog = None;
                        cx.notify();
                    }
                    "escape" if this.tz_open => {
                        this.tz_open = false;
                        cx.notify();
                    }
                    "escape" => window.remove_window(),
                    "s" | "enter" if primary => this.save(window, cx),
                    _ => {}
                }
            }))
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(t.background)
            .text_color(t.text)
            .text_size(TEXT_MD)
            .child(header)
            .when_some(invitation, |el, invitation| el.child(invitation))
            .when_some(self.error.clone(), |el, error| {
                el.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(SPACE_2)
                        .mx(SPACE_6)
                        .mt(SPACE_3)
                        .px(SPACE_3)
                        .py(SPACE_2)
                        .rounded(RADIUS_MD)
                        .bg(t.danger.alpha(0.12))
                        .text_color(t.danger)
                        .text_size(TEXT_SM)
                        .child(Icon::new(IconName::CircleX).size(px(14.)))
                        .child(error),
                )
            })
            .child(
                div()
                    .id("editor-body")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .px(SPACE_6)
                    .py(SPACE_4)
                    .gap(SPACE_4)
                    .child(
                        div()
                            .text_size(TEXT_XL)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(Input::new(&self.title)),
                    )
                    .child(field_row("When", IconName::Clock, &t, when))
                    .when_some(repeat, |el, repeat| {
                        el.child(field_row("Repeat", IconName::Repeat, &t, repeat))
                    })
                    .child(field_row(
                        "Location",
                        IconName::MapPin,
                        &t,
                        Input::new(&self.location),
                    ))
                    .child(field_row("People", IconName::Users, &t, attendees))
                    .when_some(find_time, |el, find_time| el.child(find_time))
                    .children(options)
                    .child(field_row(
                        "Description",
                        IconName::TextAlignStart,
                        &t,
                        Textarea::new(&self.description),
                    )),
            )
            .when_some(dialog, |el, dialog| el.child(dialog))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dates_and_times() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        assert_eq!(parse_date("2026-11-16"), Some(date));
        assert_eq!(parse_date("16/11/2026"), Some(date));
        assert_eq!(parse_date("nope"), None);
        let time = |h, m| NaiveTime::from_hms_opt(h, m, 0);
        assert_eq!(parse_time("09:30"), time(9, 30));
        assert_eq!(parse_time("9h30"), time(9, 30));
        assert_eq!(parse_time("930"), time(9, 30));
        assert_eq!(parse_time("14"), time(14, 0));
        assert_eq!(parse_time("2:15pm"), time(14, 15));
        assert_eq!(parse_time("25:00"), None);
    }

    #[test]
    fn parses_addresses() {
        assert_eq!(
            parse_address("Ada Lovelace <ada@example.com>"),
            Some(Address::new("Ada Lovelace", "ada@example.com"))
        );
        assert_eq!(
            parse_address(" bob@example.org "),
            Some(Address::new("", "bob@example.org"))
        );
        assert_eq!(parse_address("bob"), None);
        assert_eq!(parse_address("bob@localhost"), None);
    }

    #[test]
    fn presets_follow_the_start_date() {
        // Tuesday 10 November 2026 = second Tuesday.
        let start = NaiveDate::from_ymd_opt(2026, 11, 10).unwrap();
        let labels: Vec<String> = presets(start).into_iter().map(|(label, _)| label).collect();
        assert!(labels.contains(&"Weekly on Tue".to_owned()));
        assert!(labels.contains(&"Monthly on day 10".to_owned()));
        assert!(labels.contains(&"Monthly on the second Tue".to_owned()));
        assert!(labels.contains(&"Yearly on 10 Nov".to_owned()));
        let monthly = presets(start)
            .into_iter()
            .find(|(label, _)| label.starts_with("Monthly on the"))
            .and_then(|(_, rule)| rule)
            .unwrap();
        assert_eq!(monthly.to_string(), "FREQ=MONTHLY;BYDAY=2TU");
    }
}
