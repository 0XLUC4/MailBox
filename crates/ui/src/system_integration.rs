//! Operating-system integration: `mailto:` launches, single instance, default mail app.
//!
//! - Single instance: the first instance listens on a loopback TCP port written to
//!   `<data dir>/instance.port`. A second launch ([`forward_to_running`]) sends its `mailto:`
//!   (or just "activate") there, waits for `ok`, and exits; the running window comes forward
//!   and opens compose. Without an answer the second launch simply starts normally.
//! - macOS sends `mailto:` links as Apple Events, not arguments: [`receive_open_urls`] keeps them
//!   until the main window polls them (with the requests above).
//! - Default mail app ([`register_mail_handler`], explicit button): on Windows registers
//!   Mailbox as a `mailto:` handler under HKCU, then opens Windows' Default apps page where
//!   the user confirms (Windows does not let apps make themselves the default). On Linux,
//!   writes a desktop entry and makes it the `mailto:` handler with `xdg-mime`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use gpui::{AnyWindowHandle, App, Application, Entity};

use crate::app_state::AppState;
use crate::compose::{ComposeRequest, open_compose};
use crate::notifications::{self, Activation};

/// First token of every message, so an unrelated program on a stale port is never trusted.
const PROTOCOL: &str = "mailbox/1";
const ACTIVATE: &str = "activate";
const MAX_LINE: u64 = 16 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// URLs the OS asked the app to open (macOS Apple Events), until [`poll_requests`] takes them.
static OPENED_URLS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Keeps the URLs the OS asks `app` to open (macOS `mailto:` links, including the one a launch
/// was for); the main window opens them (see [`listen`]).
pub fn receive_open_urls(app: Application) -> Application {
    app.on_open_urls(|urls| {
        OPENED_URLS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(urls);
    });
    app
}

fn take_queued_urls() -> Vec<String> {
    std::mem::take(&mut *OPENED_URLS.lock().unwrap_or_else(PoisonError::into_inner))
}

/// The `mailto:` URL among command-line arguments (as passed by the Windows handler).
pub fn mailto_from_args<'a>(args: impl IntoIterator<Item = &'a str>) -> Option<String> {
    args.into_iter()
        .map(str::trim)
        .find(|arg| arg.len() >= 7 && arg[..7].eq_ignore_ascii_case("mailto:"))
        .map(str::to_owned)
}

fn port_file() -> PathBuf {
    crate::sync_bridge::data_dir().join("instance.port")
}

/// Hands `mailto` (or a plain "bring to front") to an already running instance.
/// Returns `true` when that instance acknowledged it: the caller should exit.
pub fn forward_to_running(mailto: Option<&str>) -> bool {
    let Ok(text) = std::fs::read_to_string(port_file()) else {
        return false;
    };
    let Ok(port) = text.trim().parse::<u16>() else {
        return false;
    };
    let payload = mailto.unwrap_or(ACTIVATE);
    if payload.contains(['\r', '\n']) {
        return false;
    }
    send_line(port, payload).unwrap_or(false)
}

fn send_line(port: u16, payload: &str) -> std::io::Result<bool> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&address, IO_TIMEOUT)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    writeln!(stream, "{PROTOCOL} {payload}")?;
    let mut answer = String::new();
    BufReader::new(stream).take(16).read_line(&mut answer)?;
    Ok(answer.trim() == "ok")
}

/// Starts listening for later launches and OS open requests. Call once, after the main window
/// opens.
pub fn listen(state: Entity<AppState>, main_window: AnyWindowHandle, cx: &mut App) {
    let (sender, received) = channel::<String>();
    match bind_instance_port() {
        Ok(listener) => {
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    if let Some(payload) = read_request(stream)
                        && sender.send(payload).is_err()
                    {
                        break;
                    }
                }
            });
        }
        Err(err) => crate::log::error(format_args!("single-instance listener failed: {err}")),
    }
    // Polled even without the listener: macOS open requests arrive through `receive_open_urls`.
    poll_requests(state, main_window, received, cx);
}

/// Listens on a loopback port and writes it to [`port_file`].
fn bind_instance_port() -> std::io::Result<TcpListener> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    let path = port_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, port.to_string())?;
    Ok(listener)
}

/// Reads one request and acknowledges it. `None` for anything that is not ours.
fn read_request(stream: TcpStream) -> Option<String> {
    stream.set_read_timeout(Some(IO_TIMEOUT)).ok()?;
    let mut line = String::new();
    BufReader::new(&stream)
        .take(MAX_LINE)
        .read_line(&mut line)
        .ok()?;
    let payload = line.trim().strip_prefix(PROTOCOL)?.trim().to_owned();
    (&stream).write_all(b"ok\n").ok()?;
    Some(payload)
}

fn poll_requests(
    state: Entity<AppState>,
    main_window: AnyWindowHandle,
    received: Receiver<String>,
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(POLL_INTERVAL).await;
            for payload in received.try_iter().chain(take_queued_urls()) {
                cx.update(|cx| {
                    notifications::focus(&state, main_window, Activation::App, cx);
                    if payload != ACTIVATE {
                        open_mailto(&state, main_window, payload, cx);
                    }
                });
            }
        }
    })
    .detach();
}

/// Opens compose for a `mailto:` URL in the main window.
pub fn open_mailto(
    state: &Entity<AppState>,
    main_window: AnyWindowHandle,
    url: String,
    cx: &mut App,
) {
    if mailto_from_args([url.as_str()]).is_none() {
        return;
    }
    let state = state.clone();
    main_window
        .update(cx, |_, window, cx| {
            open_compose(state, ComposeRequest::Mailto(url), window, cx)
        })
        .ok();
}

/// Registers Mailbox as a `mailto:` handler (HKCU) and opens the Default apps settings page.
#[cfg(windows)]
pub fn register_mail_handler(cx: &App) -> Result<(), String> {
    use windows_registry::CURRENT_USER;

    const PROG_ID: &str = "Mailbox.Url.Mailto";
    const CLIENT: &str = r"Software\Clients\Mail\Mailbox";

    fn fail(err: impl std::fmt::Display) -> String {
        format!("registry: {err}")
    }

    let exe = std::env::current_exe().map_err(|err| err.to_string())?;
    let command = format!("\"{}\" \"%1\"", exe.display());

    let prog = CURRENT_USER
        .create(format!(r"Software\Classes\{PROG_ID}"))
        .map_err(fail)?;
    prog.set_string("", "Mailbox mailto URL").map_err(fail)?;
    prog.set_string("URL Protocol", "").map_err(fail)?;
    CURRENT_USER
        .create(format!(r"Software\Classes\{PROG_ID}\DefaultIcon"))
        .and_then(|key| key.set_string("", format!("\"{}\",0", exe.display())))
        .map_err(fail)?;
    CURRENT_USER
        .create(format!(r"Software\Classes\{PROG_ID}\shell\open\command"))
        .and_then(|key| key.set_string("", &command))
        .map_err(fail)?;

    let client = CURRENT_USER.create(CLIENT).map_err(fail)?;
    client.set_string("", "Mailbox").map_err(fail)?;
    // The exe's icon resource 1 (`apps/mailbox/build.rs`), shown in Default apps.
    CURRENT_USER
        .create(format!(r"{CLIENT}\DefaultIcon"))
        .and_then(|key| key.set_string("", format!("\"{}\",0", exe.display())))
        .map_err(fail)?;
    CURRENT_USER
        .create(format!(r"{CLIENT}\shell\open\command"))
        .and_then(|key| key.set_string("", format!("\"{}\"", exe.display())))
        .map_err(fail)?;
    let capabilities = CURRENT_USER
        .create(format!(r"{CLIENT}\Capabilities"))
        .map_err(fail)?;
    capabilities
        .set_string("ApplicationName", "Mailbox")
        .and_then(|_| {
            capabilities.set_string(
                "ApplicationDescription",
                "Mail, calendar, contacts and tasks",
            )
        })
        .map_err(fail)?;
    CURRENT_USER
        .create(format!(r"{CLIENT}\Capabilities\URLAssociations"))
        .and_then(|key| key.set_string("mailto", PROG_ID))
        .map_err(fail)?;
    CURRENT_USER
        .create(r"Software\RegisteredApplications")
        .and_then(|key| key.set_string("Mailbox", format!(r"{CLIENT}\Capabilities")))
        .map_err(fail)?;

    cx.open_url("ms-settings:defaultapps?registeredAppUser=Mailbox");
    Ok(())
}

/// Writes `<data home>/applications/mailbox.desktop` (this executable, `%u` for the URL) and
/// makes it the `mailto:` handler with `xdg-mime`.
#[cfg(target_os = "linux")]
pub fn register_mail_handler(_cx: &App) -> Result<(), String> {
    const DESKTOP_FILE: &str = "mailbox.desktop";

    let exe = std::env::current_exe().map_err(|err| err.to_string())?;
    let applications = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .ok_or("no home folder")?
        .join("applications");
    std::fs::create_dir_all(&applications).map_err(|err| err.to_string())?;
    // Desktop entry quoting: the path in double quotes with `"`, `` ` ``, `$` and `\` escaped,
    // then every `\` doubled again for the key's string escaping.
    let mut quoted = String::new();
    for c in exe.to_string_lossy().chars() {
        match c {
            '"' | '`' | '$' => quoted.push_str(&format!("\\\\{c}")),
            '\\' => quoted.push_str("\\\\\\\\"),
            c => quoted.push(c),
        }
    }
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Mailbox\nTerminal=false\n\
         Exec=\"{quoted}\" %u\nCategories=Network;Email;\nMimeType=x-scheme-handler/mailto;\n"
    );
    std::fs::write(applications.join(DESKTOP_FILE), entry).map_err(|err| err.to_string())?;
    let status = std::process::Command::new("xdg-mime")
        .args(["default", DESKTOP_FILE, "x-scheme-handler/mailto"])
        .status()
        .map_err(|err| format!("xdg-mime: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("xdg-mime failed ({status})"))
    }
}

/// macOS only lets the user pick the default mail app (from Apple Mail's settings).
#[cfg(not(any(windows, target_os = "linux")))]
pub fn register_mail_handler(cx: &App) -> Result<(), String> {
    Err(crate::settings::tr(
        cx,
        "choose Mailbox in Mail > Settings > General > Default email reader",
    )
    .into())
}

/// Settings / palette entry: registers, then explains what to confirm.
pub fn set_default_mail_app(state: &Entity<AppState>, cx: &mut App) {
    let result = register_mail_handler(cx);
    let done = crate::settings::tr(
        cx,
        if cfg!(windows) {
            "Mailbox is registered for mailto: links. Pick it under Email in the Default apps page."
        } else {
            "Mailbox now opens mailto: links."
        },
    );
    state.update(cx, |state, cx| match result {
        Ok(()) => state.notify_info(done, cx),
        Err(err) => state.report_error(
            format!(
                "{}: {err}",
                crate::settings::tr(cx, "Could not register Mailbox")
            ),
            cx,
        ),
    });
}

#[cfg(test)]
mod tests {
    use super::mailto_from_args;

    #[test]
    fn finds_mailto_arguments() {
        assert_eq!(
            mailto_from_args(["--sample", "MAILTO:a@b.co?subject=Hi"]),
            Some("MAILTO:a@b.co?subject=Hi".to_owned())
        );
        assert_eq!(mailto_from_args(["--data", "x.db"]), None);
        assert_eq!(mailto_from_args(["mail"]), None);
    }
}
