//! In-app updates: the rail button (up arrow, blue dot when a release waits) and its popover.
//! Checks 10 s after start, then every 6 h; download, checksum and exe swap live in
//! `mailbox-updater`. `MAILBOX_UPDATE_FEED=<latest.json path or URL>` replaces the published
//! manifest (tests).

use std::time::Duration;

use gpui::base::Selectable;
use gpui::component::popover::Popover;
use gpui::component::tooltip::Tooltip;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_updater::{Error, Release, Version};

use crate::components::{Button, Icon, IconButton, IconName};
use crate::settings::tr;
use crate::theme::{
    DEFAULT_ACCENT, RADIUS_FULL, RADIUS_MD, SPACE_0_5, SPACE_2, SPACE_3, TEXT_MD, TEXT_SM, theme,
};

const FIRST_CHECK: Duration = Duration::from_secs(10);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
const NOTES_CHARS: usize = 160;

enum Status {
    NotChecked,
    UpToDate,
    Available(Release),
    Failed(Error),
}

/// Update state shared by the rail button and its popover.
pub struct Updater {
    status: Status,
    checking: bool,
    installing: bool,
    install_error: Option<Error>,
    _timer: Task<()>,
}

impl Updater {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let timer = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FIRST_CHECK).await;
            while this.update(cx, |updater, cx| updater.check(cx)).is_ok() {
                cx.background_executor().timer(CHECK_EVERY).await;
            }
        });
        Self {
            status: Status::NotChecked,
            checking: false,
            installing: false,
            install_error: None,
            _timer: timer,
        }
    }

    pub fn update_available(&self) -> bool {
        matches!(self.status, Status::Available(_))
    }

    /// Fetches the latest release's `latest.json` (ignored while a check or an install runs).
    pub fn check(&mut self, cx: &mut Context<Self>) {
        if self.checking || self.installing {
            return;
        }
        self.checking = true;
        cx.notify();
        let task = cx
            .background_executor()
            .spawn(async { mailbox_updater::check() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |updater, cx| {
                updater.checking = false;
                updater.install_error = None;
                updater.status = match result {
                    Ok(Some(release)) => Status::Available(release),
                    Ok(None) => Status::UpToDate,
                    Err(err) => Status::Failed(err),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Downloads, verifies and installs the available release, then quits (the new exe is
    /// already started). On failure the error shows in the popover and nothing changes.
    fn install(&mut self, cx: &mut Context<Self>) {
        let Status::Available(release) = &self.status else {
            return;
        };
        if self.installing {
            return;
        }
        let release = release.clone();
        self.installing = true;
        self.install_error = None;
        cx.notify();
        let task = cx
            .background_executor()
            .spawn(async move { mailbox_updater::install(&release) });
        cx.spawn(async move |this, cx| match task.await {
            Ok(()) => cx.update(|cx| cx.quit()),
            Err(err) => {
                eprintln!("mailbox: update failed: {err}");
                this.update(cx, |updater, cx| {
                    updater.installing = false;
                    updater.install_error = Some(err);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }
}

/// The rail entry just above Settings.
pub fn rail_button(updater: &Entity<Updater>, cx: &App) -> impl IntoElement {
    let card_updater = updater.clone();
    div().flex().w_full().justify_center().child(
        Popover::new("updates")
            .anchor(Anchor::LeftCenter)
            .w(px(300.))
            .bg(theme(cx).elevated)
            .trigger(RailTrigger {
                available: updater.read(cx).update_available(),
                tooltip: tr(cx, "Updates"),
                selected: false,
            })
            .content(move |_, _, cx| card(&card_updater, cx)),
    )
}

fn card(updater: &Entity<Updater>, cx: &App) -> AnyElement {
    let t = theme(cx).clone();
    let state = updater.read(cx);
    let current = Version::current();
    let available = match &state.status {
        Status::Available(release) => Some(release.clone()),
        _ => None,
    };
    let title = match &state.status {
        Status::Available(_) => tr(cx, "Update available"),
        Status::UpToDate => tr(cx, "You're up to date"),
        Status::NotChecked if state.checking => tr(cx, "Checking for updates…"),
        Status::NotChecked => tr(cx, "Updates"),
        Status::Failed(_) => tr(cx, "Could not check for updates"),
    };
    let error = match (&state.install_error, &state.status) {
        (Some(err), _) | (None, Status::Failed(err)) => Some(tr(cx, err.message())),
        _ => None,
    };
    let subtitle: SharedString = match (&error, &available) {
        (Some(message), _) => (*message).into(),
        (None, Some(release)) => format!("Mailbox {current} → {}", release.version).into(),
        (None, None) => format!("Mailbox {current}").into(),
    };

    let refresh_updater = updater.clone();
    let header = div()
        .flex()
        .flex_row()
        .items_start()
        .gap(SPACE_2)
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.))
                .gap(SPACE_0_5)
                .child(
                    div()
                        .text_size(TEXT_MD)
                        .font_weight(FontWeight::BOLD)
                        .text_color(t.text)
                        .child(title),
                )
                .child(
                    div()
                        .text_size(TEXT_SM)
                        .text_color(if error.is_some() {
                            t.danger
                        } else {
                            t.text_muted
                        })
                        .child(subtitle),
                ),
        )
        .child(refresh_button(
            state.checking,
            tr(cx, "Check for updates"),
            move |cx| refresh_updater.update(cx, |updater, cx| updater.check(cx)),
            cx,
        ));

    let installing = state.installing;
    div()
        .flex()
        .flex_col()
        .gap(SPACE_3)
        .child(header)
        .when_some(available, |el, release| {
            let notes = release.notes_excerpt(NOTES_CHARS);
            let page_url = release.page_url.clone();
            let install_updater = updater.clone();
            el.when(!notes.is_empty(), |el| {
                el.child(
                    div()
                        .text_size(TEXT_SM)
                        .text_color(t.text_muted)
                        .child(notes),
                )
            })
            .when(!page_url.is_empty(), |el| {
                el.child(
                    div()
                        .id("whats-new")
                        .text_size(TEXT_SM)
                        .text_color(t.accent)
                        .cursor_pointer()
                        .hover(|style| style.underline())
                        .child(tr(cx, "What's new"))
                        .on_click(move |_, _, cx| cx.open_url(&page_url)),
                )
            })
            .child(
                Button::new("update-and-restart")
                    .primary()
                    .label(if installing {
                        tr(cx, "Updating…")
                    } else {
                        tr(cx, "Update and restart")
                    })
                    .disabled(installing)
                    .on_click(move |_, _, cx| {
                        install_updater.update(cx, |updater, cx| updater.install(cx));
                    }),
            )
        })
        .into_any_element()
}

/// Small square refresh button; the icon spins while `spinning`.
fn refresh_button(
    spinning: bool,
    tooltip: &'static str,
    on_click: impl Fn(&mut App) + 'static,
    cx: &App,
) -> impl IntoElement {
    let t = theme(cx).clone();
    let icon = Icon::new(IconName::RefreshCw).size(px(14.));
    let icon = if spinning {
        icon.with_animation(
            "update-check-spin",
            Animation::new(Duration::from_millis(900)).repeat(),
            |icon, delta| icon.transform(Transformation::rotate(percentage(delta))),
        )
        .into_any_element()
    } else {
        icon.into_any_element()
    };
    let (hover_bg, hover_fg) = (t.hover, t.text);
    div()
        .id("update-check")
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(28.))
        .rounded(RADIUS_MD)
        .text_color(t.text_muted)
        .child(icon)
        .tooltip(move |window, cx| Tooltip::new(tooltip).build(window, cx))
        .when(!spinning, |el| {
            el.cursor_pointer()
                .hover(move |style| style.bg(hover_bg).text_color(hover_fg))
                .on_click(move |_, _, cx| on_click(cx))
        })
}

/// The rail's up-arrow button, with a blue dot when an update is available.
#[derive(IntoElement)]
struct RailTrigger {
    available: bool,
    tooltip: &'static str,
    selected: bool,
}

impl Selectable for RailTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl RenderOnce for RailTrigger {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx).clone();
        div()
            .relative()
            .child(
                IconButton::new("rail-updates", IconName::CircleArrowUp)
                    .size(px(36.))
                    .icon_size(px(18.))
                    .active(self.selected)
                    // No tooltip over the open popover.
                    .when(!self.selected, |button| button.tooltip(self.tooltip)),
            )
            .when(self.available, |el| {
                el.child(
                    div()
                        .absolute()
                        .top(px(6.))
                        .right(px(6.))
                        .size(px(8.))
                        .rounded(RADIUS_FULL)
                        .bg(rgb(DEFAULT_ACCENT))
                        .border_1()
                        .border_color(t.rail),
                )
            })
    }
}
