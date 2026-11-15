//! In-app updates: the rail button (up arrow, accent dot when a release waits) and its popover.
//! Checks 10 s after start, then every 6 h; download, checksum and exe swap live in
//! `mailbox-updater`. `MAILBOX_UPDATE_FEED=<latest.json path or URL>` replaces the published
//! manifest (tests).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gpui::base::Selectable;
use gpui::component::popover::Popover;
use gpui::component::tooltip::Tooltip;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use mailbox_updater::{Error, Release, Version};

use crate::components::{Icon, IconButton, IconName};
use crate::settings::tr;
use crate::theme::{
    DISABLED_OPACITY, ICON_SIZE_SM, RADIUS_FULL, RADIUS_LG, RADIUS_MD, SPACE_0_5, SPACE_1, SPACE_2,
    SPACE_3, TEXT_LG, TEXT_MD, TEXT_SM, theme,
};

const FIRST_CHECK: Duration = Duration::from_secs(10);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// Release-note lines shown in the popover.
const NOTES_LINES: usize = 6;
/// How often the popover redraws the progress bar during a download.
const PROGRESS_REDRAW: Duration = Duration::from_millis(100);

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
    /// Bytes downloaded by the running install (written by the background download).
    downloaded: Arc<AtomicU64>,
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
            downloaded: Arc::new(AtomicU64::new(0)),
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
        self.downloaded.store(0, Ordering::Relaxed);
        cx.notify();
        let downloaded = self.downloaded.clone();
        let task = cx.background_executor().spawn(async move {
            mailbox_updater::install(&release, |bytes| downloaded.store(bytes, Ordering::Relaxed))
        });
        // Redraws the progress bar until the install ends.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(PROGRESS_REDRAW).await;
                let installing = this.update(cx, |updater, cx| {
                    cx.notify();
                    updater.installing
                });
                if !matches!(installing, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |this, cx| match task.await {
            Ok(()) => cx.update(|cx| cx.quit()),
            Err(err) => {
                crate::log::error(format_args!("update failed: {err}"));
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
    match &updater.read(cx).status {
        Status::Available(release) => available_card(updater, release, cx),
        _ => status_card(updater, cx),
    }
}

/// No release waiting: status line and the refresh button.
fn status_card(updater: &Entity<Updater>, cx: &App) -> AnyElement {
    let t = theme(cx).clone();
    let state = updater.read(cx);
    let title = match &state.status {
        Status::UpToDate => tr(cx, "You're up to date"),
        Status::NotChecked if state.checking => tr(cx, "Checking for updates…"),
        Status::Failed(_) => tr(cx, "Could not check for updates"),
        _ => tr(cx, "Updates"),
    };
    let error = match &state.status {
        Status::Failed(err) => Some(tr(cx, err.message())),
        _ => None,
    };
    let subtitle: SharedString = match error {
        Some(message) => message.into(),
        None => format!("Mailbox {}", Version::current()).into(),
    };
    let refresh_updater = updater.clone();
    div()
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
        ))
        .into_any_element()
}

/// A release waits: "Mailbox X is available", its highlights and one large button.
fn available_card(updater: &Entity<Updater>, release: &Release, cx: &App) -> AnyElement {
    let t = theme(cx).clone();
    let state = updater.read(cx);
    let installing = state.installing;
    let downloaded = state.downloaded.load(Ordering::Relaxed);
    let title = tr(cx, "Mailbox {} is available").replace("{}", &release.version.to_string());
    let current = tr(cx, "You have {}.").replace("{}", &Version::current().to_string());
    let error = state.install_error.as_ref().map(|err| error_text(err, cx));
    let notes = release.highlights(NOTES_LINES);

    let action = if mailbox_updater::SUPPORTED {
        let install_updater = updater.clone();
        let label = if installing {
            tr(cx, "Updating…")
        } else {
            tr(cx, "Update and restart")
        };
        big_button(
            "update-and-restart",
            label,
            installing,
            move |cx| install_updater.update(cx, |updater, cx| updater.install(cx)),
            cx,
        )
    } else {
        // No in-place install for this build: the release page has the download.
        let page_url = release.page_url.clone();
        big_button(
            "download-update",
            tr(cx, "Download"),
            false,
            move |cx| cx.open_url(&page_url),
            cx,
        )
    };
    let footer = if mailbox_updater::SUPPORTED {
        tr(cx, "Mailbox restarts to finish the update.")
    } else {
        tr(cx, "Opens the release page to download it.")
    };

    div()
        .flex()
        .flex_col()
        .gap(SPACE_3)
        .child(
            div()
                .flex()
                .flex_col()
                .gap(SPACE_1)
                .child(
                    div()
                        .text_size(TEXT_LG)
                        .font_weight(FontWeight::BOLD)
                        .text_color(t.text)
                        .child(title),
                )
                .child(
                    div()
                        .text_size(TEXT_SM)
                        .text_color(t.text_muted)
                        .child(current),
                ),
        )
        .when(!notes.is_empty(), |el| {
            el.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(SPACE_1)
                    .text_size(TEXT_SM)
                    .text_color(t.text)
                    .children(notes.into_iter().map(|line| div().child(line))),
            )
        })
        .when_some(error, |el, message| {
            el.child(div().text_size(TEXT_SM).text_color(t.danger).child(message))
        })
        .child(action)
        .when(installing, |el| {
            el.child(progress_bar(downloaded, release.size, t.border, t.accent))
        })
        .child(
            div()
                .text_size(TEXT_SM)
                .text_color(t.text_muted)
                .child(footer),
        )
        .into_any_element()
}

/// The translated error, with the detail of a failed install (e.g. a read-only folder).
fn error_text(err: &Error, cx: &App) -> SharedString {
    match err {
        Error::Install(detail) => format!("{}: {detail}", tr(cx, err.message())).into(),
        other => tr(cx, other.message()).into(),
    }
}

/// Full-width accent button (the popover's one action).
fn big_button(
    id: &'static str,
    label: &'static str,
    disabled: bool,
    on_click: impl Fn(&mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let t = theme(cx);
    let hover_bg = t.accent_hover;
    div()
        .id(id)
        .flex()
        .w_full()
        .h(px(40.))
        .items_center()
        .justify_center()
        .rounded(RADIUS_LG)
        .bg(t.accent)
        .text_size(TEXT_LG)
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.on_accent)
        .map(|el| {
            if disabled {
                el.opacity(DISABLED_OPACITY)
            } else {
                el.cursor_pointer()
                    .hover(move |style| style.bg(hover_bg))
                    .on_click(move |_, _, cx| on_click(cx))
            }
        })
        .child(label)
        .into_any_element()
}

/// Thin accent bar filled to `done / total`.
fn progress_bar(done: u64, total: u64, track: Hsla, fill: Hsla) -> impl IntoElement {
    let fraction = if total == 0 {
        0.
    } else {
        (done as f32 / total as f32).clamp(0., 1.)
    };
    div()
        .w_full()
        .h(px(4.))
        .rounded(RADIUS_FULL)
        .bg(track)
        .overflow_hidden()
        .child(
            div()
                .h_full()
                .w(relative(fraction))
                .rounded(RADIUS_FULL)
                .bg(fill),
        )
}

/// Small square refresh button; the icon spins while `spinning`.
fn refresh_button(
    spinning: bool,
    tooltip: &'static str,
    on_click: impl Fn(&mut App) + 'static,
    cx: &App,
) -> impl IntoElement {
    let t = theme(cx).clone();
    let icon = Icon::new(IconName::RefreshCw).size(ICON_SIZE_SM);
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

/// The rail's up-arrow button, with an accent dot when an update is available.
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
                    .size(crate::shell::RAIL_BUTTON_SIZE)
                    .icon_size(crate::shell::RAIL_ICON_SIZE)
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
                        .bg(t.accent)
                        .border_1()
                        .border_color(t.rail),
                )
            })
    }
}
