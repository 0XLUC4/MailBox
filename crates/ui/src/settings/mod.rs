//! Settings module: appearance, mail, calendar, notifications, accounts, rules, shortcuts,
//! language, about. Preferences persist in the store (`mailbox_core::settings`) through undoable
//! commands; this view also keeps the [`prefs`] global and the theme in sync with the store.

pub mod controls;
pub mod i18n;
mod prefs;

use gpui::component::switch::Switch;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_core::settings::{
    AccountServers, Density, Language, ReadingPane, RibbonMode, Settings, ThemePreference,
    WeekStart,
};

use crate::actions::{OpenAbout, OpenAddAccount, OpenRules, OpenSignatures};
use crate::app_state::AppState;
use crate::components::{Button, Icon, IconName};
use crate::dialogs;
use crate::theme::{
    ACCENTS, RADIUS_FULL, RADIUS_MD, SPACE_0_5, SPACE_1, SPACE_2, SPACE_3, SPACE_6, SPACE_8,
    TEXT_LG, TEXT_MD, TEXT_SM, TEXT_XL, hex, theme,
};
use controls::{segmented, setting_row, stepper, swatches};
pub use i18n::tr;
pub use prefs::{prefs, update_settings};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Appearance,
    Mail,
    Calendar,
    Notifications,
    Accounts,
    Rules,
    Shortcuts,
    Language,
    About,
}

impl Section {
    const ALL: [Section; 9] = [
        Self::Appearance,
        Self::Mail,
        Self::Calendar,
        Self::Notifications,
        Self::Accounts,
        Self::Rules,
        Self::Shortcuts,
        Self::Language,
        Self::About,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Mail => "Mail",
            Self::Calendar => "Calendar",
            Self::Notifications => "Notifications",
            Self::Accounts => "Accounts",
            Self::Rules => "Rules",
            Self::Shortcuts => "Keyboard shortcuts",
            Self::Language => "Language",
            Self::About => "About",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Appearance => IconName::Palette,
            Self::Mail => IconName::Mail,
            Self::Calendar => IconName::CalendarDays,
            Self::Notifications => IconName::Bell,
            Self::Accounts => IconName::AtSign,
            Self::Rules => IconName::ListFilter,
            Self::Shortcuts => IconName::Keyboard,
            Self::Language => IconName::Globe,
            Self::About => IconName::Info,
        }
    }
}

pub struct SettingsView {
    state: Entity<AppState>,
    section: Section,
    _subscription: Subscription,
}

impl SettingsView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        sync(&state, window, cx);
        // Re-sync after every store change (settings commands, undo/redo).
        let subscription = cx.observe_in(&state, window, |_, state, window, cx| {
            sync(&state, window, cx);
            cx.notify();
        });
        Self {
            state,
            section: Section::Appearance,
            _subscription: subscription,
        }
    }

    fn render_nav(&self, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(px(220.))
            .h_full()
            .p(SPACE_3)
            .gap(SPACE_0_5)
            .bg(t.surface)
            .border_r_1()
            .border_color(t.border_subtle)
            .child(
                div()
                    .px(SPACE_2)
                    .pb(SPACE_3)
                    .text_size(TEXT_XL)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr(cx, "Settings")),
            )
            .children(Section::ALL.map(|section| {
                let selected = section == self.section;
                div()
                    .id(("settings-section", section as u64))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(SPACE_2)
                    .h(px(30.))
                    .px(SPACE_2)
                    .rounded(RADIUS_MD)
                    .text_size(TEXT_MD)
                    .cursor_pointer()
                    .when(selected, |el| {
                        el.bg(t.accent_subtle)
                            .text_color(t.accent)
                            .font_weight(FontWeight::MEDIUM)
                    })
                    .when(!selected, |el| {
                        el.text_color(t.text_muted)
                            .hover(|style| style.bg(t.hover).text_color(t.text))
                    })
                    .child(Icon::new(section.icon()).size(px(15.)))
                    .child(tr(cx, section.label()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.section = section;
                        cx.notify();
                    }))
            }))
    }

    fn render_appearance(&self, settings: &Settings, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let state = self.state.clone();
        let theme_choice = segmented(
            "theme",
            &[
                (tr(cx, "System"), ThemePreference::System),
                (tr(cx, "Light"), ThemePreference::Light),
                (tr(cx, "Dark"), ThemePreference::Dark),
            ],
            settings.theme,
            move |value, _, cx| update_settings(&state, cx, |s| s.theme = value),
            cx,
        );
        let state = self.state.clone();
        let accent = swatches(
            "accent",
            ACCENTS,
            settings.accent,
            move |value, _, cx| update_settings(&state, cx, |s| s.accent = value),
            cx,
        );
        let state = self.state.clone();
        let density = segmented(
            "density",
            &[
                (tr(cx, "Comfortable"), Density::Comfortable),
                (tr(cx, "Compact"), Density::Compact),
            ],
            settings.density,
            move |value, _, cx| update_settings(&state, cx, |s| s.density = value),
            cx,
        );
        let state = self.state.clone();
        let sizes = Settings::FONT_SIZES;
        let size_index = sizes.iter().position(|size| *size == settings.font_size);
        let font = stepper(
            "font-size",
            format!("{} px", settings.font_size),
            size_index.is_none_or(|index| index > 0),
            size_index.is_none_or(|index| index + 1 < sizes.len()),
            move |step, _, cx| {
                update_settings(&state, cx, |s| {
                    let index = sizes
                        .iter()
                        .position(|size| *size == s.font_size)
                        .unwrap_or(1) as i32;
                    let next = (index + step).clamp(0, sizes.len() as i32 - 1) as usize;
                    s.font_size = sizes[next];
                })
            },
            cx,
        );
        let state = self.state.clone();
        let ribbon = segmented(
            "ribbon",
            &[
                (tr(cx, "Simplified"), RibbonMode::Simplified),
                (tr(cx, "Classic"), RibbonMode::Classic),
            ],
            settings.ribbon,
            move |value, _, cx| update_settings(&state, cx, |s| s.ribbon = value),
            cx,
        );
        vec![
            setting_row(tr(cx, "Theme"), None, theme_choice, cx).into_any_element(),
            setting_row(tr(cx, "Accent colour"), None, accent, cx).into_any_element(),
            setting_row(
                tr(cx, "Density"),
                Some("Row height in lists.".into()),
                density,
                cx,
            )
            .into_any_element(),
            setting_row(tr(cx, "Font size"), None, font, cx).into_any_element(),
            setting_row(
                tr(cx, "Ribbon layout"),
                Some("One compact row, or large buttons in labelled groups.".into()),
                ribbon,
                cx,
            )
            .into_any_element(),
        ]
    }

    fn render_mail(&self, settings: &Settings, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let state = self.state.clone();
        let pane = segmented(
            "reading-pane",
            &[
                (tr(cx, "Right"), ReadingPane::Right),
                (tr(cx, "Bottom"), ReadingPane::Bottom),
                (tr(cx, "Off"), ReadingPane::Off),
            ],
            settings.reading_pane,
            move |value, _, cx| update_settings(&state, cx, |s| s.reading_pane = value),
            cx,
        );
        let state = self.state.clone();
        let conversations = Switch::new("conversations")
            .checked(settings.conversations)
            .on_click(move |checked, _, cx| {
                let checked = *checked;
                update_settings(&state, cx, |s| s.conversations = checked)
            });
        let state = self.state.clone();
        let mark_read = segmented(
            "mark-read",
            &[
                (tr(cx, "Immediately"), Some(0)),
                (tr(cx, "After 5 s"), Some(5)),
                (tr(cx, "Never"), None),
            ],
            settings.mark_read_after_secs,
            move |value, _, cx| update_settings(&state, cx, |s| s.mark_read_after_secs = value),
            cx,
        );
        let signatures = Button::new("signatures")
            .icon(IconName::Pencil)
            .label(tr(cx, "Edit"))
            .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSignatures), cx));
        vec![
            setting_row(tr(cx, "Reading pane"), None, pane, cx).into_any_element(),
            setting_row(
                tr(cx, "Conversations"),
                Some(tr(cx, "Group messages by conversation").into()),
                conversations,
                cx,
            )
            .into_any_element(),
            setting_row(tr(cx, "Mark as read when opened"), None, mark_read, cx).into_any_element(),
            setting_row(
                tr(cx, "Signatures"),
                Some("Added to new messages and replies.".into()),
                signatures,
                cx,
            )
            .into_any_element(),
        ]
    }

    fn render_calendar(&self, settings: &Settings, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let state = self.state.clone();
        let week = segmented(
            "week-start",
            &[
                (tr(cx, "Monday"), WeekStart::Monday),
                (tr(cx, "Sunday"), WeekStart::Sunday),
                (tr(cx, "Saturday"), WeekStart::Saturday),
            ],
            settings.week_start,
            move |value, _, cx| update_settings(&state, cx, |s| s.week_start = value),
            cx,
        );
        let (start, end) = (settings.work_start_hour, settings.work_end_hour);
        let state = self.state.clone();
        let start_stepper = stepper(
            "work-start",
            format!("{start:02}:00"),
            start > 0,
            start + 1 < end,
            move |step, _, cx| {
                update_settings(&state, cx, |s| {
                    s.work_start_hour = shift_hour(s.work_start_hour, step, 0, s.work_end_hour - 1)
                })
            },
            cx,
        );
        let state = self.state.clone();
        let end_stepper = stepper(
            "work-end",
            format!("{end:02}:00"),
            end > start + 1,
            end < 24,
            move |step, _, cx| {
                update_settings(&state, cx, |s| {
                    s.work_end_hour = shift_hour(s.work_end_hour, step, s.work_start_hour + 1, 24)
                })
            },
            cx,
        );
        vec![
            setting_row(tr(cx, "Week starts on"), None, week, cx).into_any_element(),
            setting_row(
                tr(cx, "Work hours"),
                Some("Highlighted in day and week views.".into()),
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(SPACE_2)
                    .child(start_stepper)
                    .child(div().text_color(theme(cx).text_subtle).child("–"))
                    .child(end_stepper),
                cx,
            )
            .into_any_element(),
        ]
    }

    fn render_notifications(&self, settings: &Settings, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let toggle = |id: &'static str, checked: bool, set: fn(&mut Settings, bool)| {
            let state = self.state.clone();
            Switch::new(id)
                .checked(checked)
                .on_click(move |checked, _, cx| {
                    let checked = *checked;
                    update_settings(&state, cx, |s| set(s, checked))
                })
        };
        vec![
            setting_row(
                tr(cx, "New mail alerts"),
                Some("Desktop notification when mail arrives.".into()),
                toggle("notify-mail", settings.notify_new_mail, |s, v| {
                    s.notify_new_mail = v
                }),
                cx,
            )
            .into_any_element(),
            setting_row(
                tr(cx, "Play a sound"),
                None,
                toggle("notify-sound", settings.notify_sound, |s, v| {
                    s.notify_sound = v
                }),
                cx,
            )
            .into_any_element(),
            setting_row(
                tr(cx, "Event reminders"),
                None,
                toggle("notify-reminders", settings.notify_reminders, |s, v| {
                    s.notify_reminders = v
                }),
                cx,
            )
            .into_any_element(),
        ]
    }

    fn render_accounts(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = theme(cx).clone();
        let store = self.state.read(cx).store();
        let accounts: Vec<_> = store
            .list_accounts()
            .unwrap_or_default()
            .into_iter()
            .map(|account| {
                let servers = AccountServers::load(store, account.id).ok().flatten();
                (account, servers)
            })
            .collect();
        let mut rows: Vec<AnyElement> = accounts
            .into_iter()
            .map(|(account, servers)| {
                let detail = match servers {
                    Some(servers) if !servers.provider.is_empty() => {
                        format!("{} · {}", account.email, servers.provider)
                    }
                    Some(servers) => format!("{} · {}", account.email, servers.imap.host),
                    None => account.email.clone(),
                };
                let state = self.state.clone();
                let account_id = account.id;
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(SPACE_3)
                    .py(SPACE_3)
                    .border_b_1()
                    .border_color(t.border_subtle)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(32.))
                            .rounded(RADIUS_FULL)
                            .bg(hex(account.color))
                            .text_color(t.on_accent)
                            .child(Icon::new(IconName::AtSign).size(px(16.))),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .child(div().text_size(TEXT_MD).child(account.name.clone()))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(TEXT_SM)
                                    .text_color(t.text_muted)
                                    .child(detail),
                            ),
                    )
                    .child(
                        Button::new(("account-edit", account_id as u64))
                            .icon(IconName::Settings)
                            .label(tr(cx, "Edit"))
                            .on_click(move |_, window, cx| {
                                dialogs::account::open_account_settings(
                                    state.clone(),
                                    account_id,
                                    window,
                                    cx,
                                )
                            }),
                    )
                    .into_any_element()
            })
            .collect();
        rows.push(
            div()
                .pt(SPACE_3)
                .child(
                    Button::new("account-add")
                        .primary()
                        .icon(IconName::Plus)
                        .label(tr(cx, "Add account"))
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(OpenAddAccount), cx)
                        }),
                )
                .into_any_element(),
        );
        rows
    }

    fn render_rules(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let count = {
            let store = self.state.read(cx).store();
            store
                .list_accounts()
                .unwrap_or_default()
                .iter()
                .map(|account| store.list_rules(account.id).map_or(0, |rules| rules.len()))
                .sum::<usize>()
        };
        let state = self.state.clone();
        vec![
            setting_row(
                tr(cx, "Rules"),
                Some(
                    format!(
                        "{count} rule(s). Rules run on new mail, in order, and can be run on a \
                         folder at any time."
                    )
                    .into(),
                ),
                Button::new("manage-rules")
                    .icon(IconName::ListFilter)
                    .label(tr(cx, "Manage rules"))
                    .on_click(move |_, window, cx| {
                        dialogs::rules::open(state.clone(), window, cx);
                    }),
                cx,
            )
            .into_any_element(),
        ]
    }

    fn render_shortcuts(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        dialogs::shortcut_rows(cx)
    }

    fn render_language(&self, settings: &Settings, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let state = self.state.clone();
        let language = segmented(
            "language",
            &[("English", Language::En), ("Français", Language::Fr)],
            settings.language,
            move |value, _, cx| update_settings(&state, cx, |s| s.language = value),
            cx,
        );
        vec![
            setting_row(
                tr(cx, "Language"),
                Some("Menus, ribbon and settings.".into()),
                language,
                cx,
            )
            .into_any_element(),
        ]
    }

    fn render_about(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![
            setting_row(
                "Mailbox",
                Some(format!("Version {}", env!("CARGO_PKG_VERSION")).into()),
                Button::new("about")
                    .icon(IconName::Info)
                    .label(tr(cx, "About Mailbox"))
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenAbout), cx)),
                cx,
            )
            .into_any_element(),
        ]
    }
}

/// Moves an hour by `step`, kept within `[min, max]`.
fn shift_hour(hour: u8, step: i32, min: u8, max: u8) -> u8 {
    (i32::from(hour) + step).clamp(i32::from(min), i32::from(max)) as u8
}

/// Copies the stored settings into the [`prefs`] global and applies the theme when they changed.
fn sync(state: &Entity<AppState>, window: &mut Window, cx: &mut App) {
    let stored = Settings::load(state.read(cx).store()).unwrap_or_default();
    if !prefs::loaded(cx) || *prefs(cx) != stored {
        prefs::apply(&stored, window, cx);
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let settings = prefs(cx).clone();
        let section = self.section;
        let rows = match section {
            Section::Appearance => self.render_appearance(&settings, cx),
            Section::Mail => self.render_mail(&settings, cx),
            Section::Calendar => self.render_calendar(&settings, cx),
            Section::Notifications => self.render_notifications(&settings, cx),
            Section::Accounts => self.render_accounts(cx),
            Section::Rules => self.render_rules(cx),
            Section::Shortcuts => self.render_shortcuts(cx),
            Section::Language => self.render_language(&settings, cx),
            Section::About => self.render_about(cx),
        };
        let open_rules = (section == Section::Rules).then(|| {
            Button::new("open-rules-shortcut")
                .ghost()
                .icon(IconName::ListFilter)
                .label("Open rules dialog")
                .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenRules), cx))
        });

        div()
            .flex()
            .flex_row()
            .size_full()
            .bg(t.background)
            .child(self.render_nav(cx))
            .child(
                div()
                    .id("settings-content")
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .max_w(px(720.))
                            .px(SPACE_8)
                            .py(SPACE_6)
                            .gap(SPACE_1)
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(SPACE_2)
                                    .pb(SPACE_2)
                                    .child(
                                        div()
                                            .flex_1()
                                            .text_size(TEXT_LG)
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(tr(cx, section.label())),
                                    )
                                    .children(open_rules),
                            )
                            .children(rows),
                    ),
            )
    }
}
