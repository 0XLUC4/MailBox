//! HTML mail bodies shown exactly as sent, by the system web engine (WebView2 through `wry`).
//!
//! The webview is a child window of the GPUI window, so GPUI cannot draw over it: the reading
//! pane paints a [`paper`] element where the body goes, which places the webview over it every
//! frame and hides it when the paper is not painted (other module, no selection) or when a GPUI
//! overlay (menu, popover, dialog, toast) covers it.
//!
//! Locked down: JavaScript off, a strict Content-Security-Policy (no scripts, forms, frames,
//! plugins or connections), no navigation (clicked http/https/mailto links go to the app), an
//! InPrivate profile (no cookies or storage), no devtools, no browser shortcuts, no downloads.
//!
//! Remote pictures: [`prepare`] points every network reference at `/remote/<n>` on our protocol.
//! Until "Show pictures" those requests get an empty picture of the same size (like Gmail or
//! Outlook: no broken icon) and are reported ([`WebEvent::Blocked`]), which shows the bar; once
//! allowed, the app fetches them itself (no cookies, no referrer). Anything not rewritten stays
//! blocked by the CSP until allowed.

use std::borrow::Cow;
use std::cell::Cell;
use std::io::Read as _;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use gpui::*;
use mailbox_window_handle::ParentWindow;
use wry::WebViewBuilderExtWindows as _;
use wry::http::{Request, Response, StatusCode, header};

/// Custom protocol name; on Windows its pages live at `http://mbx.localhost/…`.
const SCHEME: &str = "mbx";
const ORIGIN: &str = "http://mbx.localhost/";

/// What the webview asks the app to do.
pub enum WebEvent {
    /// A clicked link (http, https or mailto only).
    OpenLink(String),
    /// The page of this message asked for remote content while it is blocked.
    Blocked(u64),
}

/// Largest remote resource fetched.
const MAX_BYTES: u64 = 15 * 1024 * 1024;
/// 1×1 transparent GIF: a blocked or failed resource of unknown size.
const EMPTY_GIF: &[u8] = &[
    0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00,
    0xff, 0xff, 0xff, 0x21, 0xf9, 0x04, 0x01, 0x00, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3b,
];

/// The document served for one message.
#[derive(Default)]
pub struct Page {
    /// The message, for [`WebEvent::Blocked`].
    pub id: u64,
    /// HTML from [`prepare`].
    pub html: String,
    /// Load remote pictures, styles and fonts ("Show pictures").
    pub remote: bool,
    /// `/inline/<n>` → (MIME type, stored file), in [`prepare`]'s `cid:` order.
    pub inline: Vec<Option<(String, PathBuf)>>,
    /// `/remote/<n>`, in [`prepare`]'s order.
    pub remote_refs: Vec<Remote>,
    /// [`WebEvent::Blocked`] was sent for this page.
    pub reported: bool,
}

pub struct MailWebView {
    webview: wry::WebView,
    page: Arc<Mutex<Page>>,
    /// Key of the page loaded in the webview.
    loaded: std::cell::RefCell<String>,
    visible: Cell<bool>,
    /// Natively shown once (see [`MailWebView::set_visible`]).
    shown: Cell<bool>,
    /// Where the paper is (last [`MailWebView::place`]).
    target: Cell<Option<Bounds<Pixels>>>,
    /// Rectangle last given to the webview (`None`: off-screen or not set).
    bounds: Cell<Option<Bounds<Pixels>>>,
    /// Bumped each time the paper is painted (see [`hide_unless_painted`]).
    paints: Cell<u64>,
    _context: wry::WebContext,
}

impl MailWebView {
    /// Creates the (hidden) webview as a child of `parent`; events are sent to `events`.
    /// Runs a nested message loop: call it while GPUI's `App` is not borrowed (from a task).
    pub fn new(
        parent: &ParentWindow,
        events: async_channel::Sender<WebEvent>,
    ) -> Result<Rc<Self>, wry::Error> {
        let page = Arc::new(Mutex::new(Page::default()));
        let served = page.clone();
        let (navigate, new_window, blocked) = (events.clone(), events.clone(), events);
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(20))
            .redirects(5)
            .build();
        // WebView2 needs a writable folder even for an InPrivate profile.
        let mut context = wry::WebContext::new(Some(crate::data_dir().join("webview")));
        let webview = wry::WebViewBuilder::new_with_web_context(&mut context)
            .with_visible(false)
            .with_focused(false)
            .with_javascript_disabled()
            .with_incognito(true)
            .with_devtools(false)
            .with_autoplay(false)
            .with_browser_accelerator_keys(false)
            .with_theme(wry::Theme::Light)
            .with_background_color((255, 255, 255, 255))
            .with_asynchronous_custom_protocol(SCHEME.into(), move |_, request, responder| {
                let mut page = served.lock().unwrap_or_else(PoisonError::into_inner);
                let Some(remote) = request
                    .uri()
                    .path()
                    .strip_prefix("/remote/")
                    .and_then(|n| n.parse::<usize>().ok())
                    .and_then(|n| page.remote_refs.get(n).cloned())
                else {
                    return responder.respond(respond(&page, &request));
                };
                if !page.remote {
                    if !page.reported {
                        page.reported = true;
                        blocked.try_send(WebEvent::Blocked(page.id)).ok();
                    }
                    return responder.respond(placeholder(remote.size));
                }
                drop(page);
                let agent = agent.clone();
                std::thread::spawn(move || {
                    let response = match fetch(&agent, &remote.url) {
                        Some((mime, bytes)) => resource(&mime, bytes),
                        None => placeholder(remote.size),
                    };
                    responder.respond(response);
                });
            })
            .with_navigation_handler(move |url| {
                // Only the message itself (and its anchors) loads in the webview.
                if url.starts_with(&format!("{ORIGIN}m/")) || url == "about:blank" {
                    return true;
                }
                forward_link(&navigate, url);
                false
            })
            .with_new_window_req_handler(move |url, _| {
                forward_link(&new_window, url);
                wry::NewWindowResponse::Deny
            })
            .with_download_started_handler(|_, _| false)
            // `true` = handled: a dropped file must not replace the message.
            .with_drag_drop_handler(|_| true)
            .build_as_child(parent)?;
        Ok(Rc::new(Self {
            webview,
            page,
            loaded: Default::default(),
            visible: Cell::new(false),
            shown: Cell::new(false),
            target: Cell::new(None),
            bounds: Cell::new(None),
            paints: Cell::new(0),
            _context: context,
        }))
    }

    /// Shows the page built by `page` unless the page with the same `key` (URL-safe) is loaded.
    pub fn load(&self, key: String, page: impl FnOnce() -> Page) {
        if *self.loaded.borrow() == key {
            return;
        }
        *self.page.lock().unwrap_or_else(PoisonError::into_inner) = page();
        let url = format!("{ORIGIN}m/{key}");
        if self.webview.load_url(&url).is_ok() {
            *self.loaded.borrow_mut() = key;
        }
    }

    /// Hiding moves the webview off-screen instead of hiding it natively: WebView2 repaints its
    /// white background each time it is shown again, a white flash whenever a toast, menu or
    /// dialog had covered the message.
    pub fn set_visible(&self, visible: bool) {
        if self.visible.get() == visible {
            return;
        }
        if visible {
            if !self.shown.get() {
                if self.webview.set_visible(true).is_err() {
                    return;
                }
                self.shown.set(true);
            }
            if let Some(bounds) = self.target.get() {
                self.apply(bounds);
            }
        } else if self.shown.get() {
            self.move_off_screen();
            // Off-screen but still a live window: it must not keep the keyboard.
            self.focus_app();
        }
        self.visible.set(visible);
    }

    /// Gives keyboard focus back to the GPUI window. A click in the message moves the system
    /// focus into the webview, and GPUI (one native window) never takes it back by itself:
    /// keys (Esc in a menu, shortcuts, typing) would all go to the webview.
    fn focus_app(&self) {
        let _ = self.webview.focus_parent();
    }

    fn place(&self, bounds: Bounds<Pixels>) {
        self.target.set(Some(bounds));
        // Off-screen while hidden: moved back by `set_visible(true)`.
        if self.visible.get() || !self.shown.get() {
            self.apply(bounds);
        }
    }

    fn apply(&self, bounds: Bounds<Pixels>) {
        if self.bounds.get() != Some(bounds) && self.set_rect(bounds).is_ok() {
            self.bounds.set(Some(bounds));
        }
    }

    fn move_off_screen(&self) {
        let size = self.target.get().map_or(size(px(1.), px(1.)), |b| b.size);
        if self
            .set_rect(Bounds::new(point(px(-32000.), px(-32000.)), size))
            .is_ok()
        {
            self.bounds.set(None);
        }
    }

    fn set_rect(&self, bounds: Bounds<Pixels>) -> Result<(), wry::Error> {
        self.webview.set_bounds(wry::Rect {
            position: wry::dpi::LogicalPosition::new(
                f32::from(bounds.origin.x),
                f32::from(bounds.origin.y),
            )
            .into(),
            size: wry::dpi::LogicalSize::new(
                f32::from(bounds.size.width),
                f32::from(bounds.size.height),
            )
            .into(),
        })
    }
}

fn forward_link(events: &async_channel::Sender<WebEvent>, url: String) {
    let lower = url.to_ascii_lowercase();
    if ["http://", "https://", "mailto:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
    {
        events.try_send(WebEvent::OpenLink(url)).ok();
    }
}

/// Where the webview goes: fills its parent, positions the webview over itself after each paint
/// and shows it only while nothing GPUI draws covers it.
pub fn paper(web: Rc<MailWebView>) -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |bounds, hitbox, window, _| {
            web.paints.set(web.paints.get() + 1);
            web.place(bounds);
            // Clicks in the webview never reach GPUI: any mouse press GPUI sees is on the app.
            let pressed = web.clone();
            window.on_mouse_event(move |_: &MouseDownEvent, phase, _, _| {
                if phase == DispatchPhase::Capture {
                    pressed.focus_app();
                }
            });
            // Checked once this frame is on screen: overlays (menus, dialogs, toasts) occlude
            // the hitbox, and they paint after this element.
            window.on_next_frame(move |window, _| {
                web.set_visible(uncovered(&hitbox, bounds, window));
            });
        },
    )
    .size_full()
}

fn uncovered(hitbox: &Hitbox, bounds: Bounds<Pixels>, window: &Window) -> bool {
    const STEPS: u32 = 6;
    let inset = px(2.);
    let (width, height) = (
        bounds.size.width - inset * 2.,
        bounds.size.height - inset * 2.,
    );
    (0..=STEPS).all(|i| {
        (0..=STEPS).all(|j| {
            let point = point(
                bounds.origin.x + inset + width * (i as f32 / STEPS as f32),
                bounds.origin.y + inset + height * (j as f32 / STEPS as f32),
            );
            hitbox.is_hovered_at(point, window)
        })
    })
}

/// Hides the webview if the paper is not painted in the coming frame (module switch, reading
/// pane turned off…). Call when app state changed.
pub fn hide_unless_painted(web: &Rc<MailWebView>, window: &Window) {
    let (web, paints) = (web.clone(), web.paints.get());
    // Next-frame callbacks run before that frame draws: check one frame later.
    window.on_next_frame(move |window, _| {
        window.on_next_frame(move |_, _| {
            if web.paints.get() == paints {
                web.set_visible(false);
            }
        })
    });
}

fn respond(page: &Page, request: &Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let path = request.uri().path();
    let (mime, body) = if path.starts_with("/m/") {
        (
            "text/html; charset=utf-8".to_owned(),
            Some(page.html.clone().into_bytes()),
        )
    } else if let Some((mime, file)) = path
        .strip_prefix("/inline/")
        .and_then(|n| n.parse::<usize>().ok())
        .and_then(|n| page.inline.get(n)?.as_ref())
    {
        (mime.clone(), std::fs::read(file).ok())
    } else {
        (String::new(), None)
    };
    let builder = Response::builder()
        .header(header::CACHE_CONTROL, "no-store")
        .header("Content-Security-Policy", csp(page.remote))
        .header("X-Content-Type-Options", "nosniff");
    let response = match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, mime)
            .body(Cow::Owned(body)),
        None => builder
            .status(StatusCode::NOT_FOUND)
            .body(Cow::Borrowed(&[][..])),
    };
    response.unwrap_or_else(|_| Response::new(Cow::Borrowed(&[][..])))
}

/// An empty picture taking the place of a blocked (or unreachable) one: an `<img>` keeps its
/// size (a missing dimension collapses to 0 rather than a big empty square).
fn placeholder(size: Option<(u32, u32)>) -> Response<Cow<'static, [u8]>> {
    match size {
        Some((width, height)) => resource(
            "image/svg+xml",
            format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}"/>"#
            )
            .into_bytes(),
        ),
        None => resource("image/gif", EMPTY_GIF.to_vec()),
    }
}

fn resource(mime: &str, bytes: Vec<u8>) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .header(header::CONTENT_TYPE, mime)
        .header(header::CACHE_CONTROL, "no-store")
        .header("X-Content-Type-Options", "nosniff")
        .body(Cow::Owned(bytes))
        .unwrap_or_else(|_| Response::new(Cow::Borrowed(&[][..])))
}

/// GETs an http(s) resource for the page (no cookies, no referrer); `None` on any failure.
fn fetch(agent: &ureq::Agent, url: &str) -> Option<(String, Vec<u8>)> {
    let lower = url.to_ascii_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return None;
    }
    let response = agent.get(url).set("User-Agent", "Mailbox").call().ok()?;
    let mime = response.content_type().to_owned();
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MAX_BYTES).then_some((mime, bytes))
}

/// Nothing runs, submits, frames or connects; only the message's own parts load unless `remote`.
pub fn csp(remote: bool) -> String {
    let net = if remote { " https: http:" } else { "" };
    format!(
        "default-src 'none'; img-src 'self' data:{net}; style-src 'self' 'unsafe-inline'{net}; \
         font-src 'self' data:{net}; script-src 'none'; object-src 'none'; frame-src 'none'; \
         child-src 'none'; connect-src 'none'; media-src 'none'; form-action 'none'; \
         base-uri 'none'"
    )
}

/// A network resource the message would load (picture, background, font), proxied by the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub url: String,
    /// `width`/`height` of the `<img>` showing it (0 when not given), so a blocked picture keeps
    /// its box; `None` for backgrounds and fonts.
    pub size: Option<(u32, u32)>,
}

/// The message HTML as served, from [`prepare`].
#[derive(Debug, Default)]
pub struct Prepared {
    pub html: String,
    /// `cid:` references, in `/inline/<n>` order.
    pub cids: Vec<String>,
    /// Remote references, in `/remote/<n>` order.
    pub remote: Vec<Remote>,
}

/// The message HTML as served: what the page would load — `src`, `background` and CSS `url()`
/// values (attributes, `style`, `<style>`) — points at our protocol: `cid:` parts at
/// `/inline/<n>`, network resources at `/remote/<n>` (blocked or proxied, see [`MailWebView`]);
/// `srcset` is dropped (`src` is used), `http-equiv` neutralized (no `<meta refresh>`), and the
/// page asks for a light colour scheme like the sender designed it. The doctype is kept.
pub fn prepare(html: &str) -> Prepared {
    let lower = html.to_ascii_lowercase();
    let mut prepared = Prepared::default();
    let mut out = String::with_capacity(html.len() + 128);
    let mut rest = 0;
    for (start, end, in_src) in url_values(&lower) {
        let value = &html[start..end];
        let value_lower = &lower[start..end];
        let replacement = if let Some(cid) = value_lower.strip_prefix("cid:").map(|_| &value[4..]) {
            prepared.cids.push(cid.to_owned());
            format!("/inline/{}", prepared.cids.len() - 1)
        } else if ["http://", "https://", "//"]
            .iter()
            .any(|scheme| value_lower.starts_with(scheme))
        {
            let url = value.replace("&amp;", "&");
            let url = if url.starts_with("//") {
                format!("https:{url}")
            } else {
                url
            };
            let size = in_src.then(|| img_size(&lower, start)).flatten();
            prepared.remote.push(Remote { url, size });
            format!("/remote/{}", prepared.remote.len() - 1)
        } else {
            continue;
        };
        out.push_str(&html[rest..start]);
        out.push_str(&replacement);
        rest = end;
    }
    out.push_str(&html[rest..]);
    let out = replace_ignore_case(&out, "srcset", "data-srcset");
    let out = replace_ignore_case(&out, "http-equiv", "data-http-equiv");

    const HEAD: &str = r#"<meta name="color-scheme" content="light only">"#;
    let trimmed = out.trim_start();
    let doctype_end = trimmed
        .get(..9)
        .filter(|start| start.eq_ignore_ascii_case("<!doctype"))
        .and_then(|_| trimmed.find('>'))
        .map(|end| out.len() - trimmed.len() + end + 1);
    prepared.html = match doctype_end {
        Some(end) => format!("{}{HEAD}{}", &out[..end], &out[end..]),
        None => format!("{HEAD}{out}"),
    };
    prepared
}

/// Byte ranges of the values of `src=` / `background=` attributes and CSS `url(…)`, in order,
/// with "is a `src` attribute".
fn url_values(lower: &str) -> Vec<(usize, usize, bool)> {
    let bytes = lower.as_bytes();
    let skip_spaces = |mut at: usize| {
        while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        at
    };
    let mut values = Vec::new();
    for (needle, attribute) in [("src", true), ("background", true), ("url(", false)] {
        for (at, _) in lower.match_indices(needle) {
            let mut start = skip_spaces(at + needle.len());
            if attribute {
                // A whole attribute name (not `data-src`, `lowsrc`, `background-color`…).
                let boundary = at
                    .checked_sub(1)
                    .and_then(|before| bytes.get(before))
                    .is_some_and(|c| c.is_ascii_whitespace() || matches!(c, b'"' | b'\'' | b'/'));
                if !boundary || bytes.get(start) != Some(&b'=') {
                    continue;
                }
                start = skip_spaces(start + 1);
            }
            let quote = bytes
                .get(start)
                .copied()
                .filter(|c| matches!(c, b'"' | b'\''));
            if quote.is_some() {
                start += 1;
            }
            let end = lower[start..]
                .find(|c: char| match quote {
                    Some(quote) => c == quote as char,
                    None => c.is_whitespace() || c == '>' || (!attribute && c == ')'),
                })
                .map_or(lower.len(), |len| start + len);
            // `url( "x" )`: the closing quote ends it; trailing spaces are not part of it.
            let end = start + lower[start..end].trim_end().len();
            values.push((start, end, needle == "src"));
        }
    }
    values.sort_unstable();
    values.dedup_by(|next, previous| next.0 < previous.1);
    values
}

/// `width`/`height` (attributes or CSS pixels, 0 if absent) of the `<img>` whose attribute
/// starts at `at`.
fn img_size(lower: &str, at: usize) -> Option<(u32, u32)> {
    let open = lower[..at].rfind('<')?;
    let close = at + lower[at..].find('>')?;
    let tag = &lower[open..close];
    if !tag.starts_with("<img") {
        return None;
    }
    Some((
        dimension(tag, "width").unwrap_or(0),
        dimension(tag, "height").unwrap_or(0),
    ))
}

/// First `name="12"` / `name:12px` in a tag (not `max-width`, `line-height`, percentages…).
fn dimension(tag: &str, name: &str) -> Option<u32> {
    tag.match_indices(name).find_map(|(at, _)| {
        let before = tag[..at].chars().next_back();
        if before.is_some_and(|c| c == '-' || c.is_ascii_alphanumeric()) {
            return None;
        }
        let value = tag[at + name.len()..].trim_start();
        let value = value.strip_prefix(['=', ':'])?.trim_start();
        let value = value.trim_start_matches(['"', '\'']);
        let digits = value.len() - value.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 || value[digits..].starts_with(['%', '.']) {
            return None;
        }
        value[..digits].parse().ok().filter(|size| *size > 0)
    })
}

fn replace_ignore_case(text: &str, from: &str, to: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut rest = 0;
    for (start, _) in lower.match_indices(from) {
        out.push_str(&text[rest..start]);
        out.push_str(to);
        rest = start + from.len();
    }
    out.push_str(&text[rest..]);
    out
}

/// Whether the HTML would load anything from the network (pictures, backgrounds, styles, fonts).
pub fn has_remote_content(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    let compact: String = lower.split_whitespace().collect();
    ["src=", "background=", "url(", "srcset=", "href="]
        .iter()
        .any(|attr| {
            compact.match_indices(attr).any(|(at, _)| {
                let value = compact[at + attr.len()..].trim_start_matches(['"', '\'']);
                let remote = value.starts_with("http:")
                    || value.starts_with("https:")
                    || value.starts_with("//");
                // `href` only loads something on a stylesheet `<link>`.
                remote && (*attr != "href=" || is_link_tag(&compact[..at]))
            })
        })
}

fn is_link_tag(before: &str) -> bool {
    before
        .rfind('<')
        .is_some_and(|open| before[open..].starts_with("<link"))
}

#[cfg(test)]
mod tests {
    use super::{Remote, csp, has_remote_content, prepare};

    #[test]
    fn rewrites_cid_references_in_order() {
        let prepared = prepare(
            r#"<!DOCTYPE html><img src="cid:logo@x"><div style="background:url(cid:bg.png)">cid: text</div><img src=CID:hero>"#,
        );
        let page = prepared.html;
        assert_eq!(prepared.cids, ["logo@x", "bg.png", "hero"]);
        assert!(page.starts_with("<!DOCTYPE html><meta name=\"color-scheme\""));
        assert!(page.contains(r#"src="/inline/0""#));
        assert!(page.contains("url(/inline/1)"));
        assert!(page.contains("src=/inline/2>"));
        assert!(page.contains("cid: text"), "plain text is left alone");
    }

    #[test]
    fn rewrites_everything_remote_the_page_would_load() {
        let prepared = prepare(concat!(
            r#"<style>.h { background: url( "https://x.example/bg.png" ) }</style>"#,
            r#"<img width="74" height="24" src='https://x.example/logo.png?a=1&amp;b=2'>"#,
            r#"<img src=//x.example/pixel.gif width=1 height=1>"#,
            r#"<img style="width:20px;height:20px;max-width:100%" src="http://x.example/a.png">"#,
            r#"<img src="https://x.example/hero.png" width="100%" srcset="https://x.example/2x.png 2x">"#,
            r#"<td background="https://x.example/td.gif" style="background-image:url(https://x.example/s.png)">"#,
            r#"<a href="https://x.example/link">link</a><div data-src="https://x.example/no">"#,
        ));
        let urls: Vec<_> = prepared.remote.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://x.example/bg.png",
                "https://x.example/logo.png?a=1&b=2",
                "https://x.example/pixel.gif",
                "http://x.example/a.png",
                "https://x.example/hero.png",
                "https://x.example/td.gif",
                "https://x.example/s.png",
            ]
        );
        let sizes: Vec<_> = prepared.remote.iter().map(|r| r.size).collect();
        assert_eq!(
            sizes,
            [
                None,
                Some((74, 24)),
                Some((1, 1)),
                Some((20, 20)),
                Some((0, 0)),
                None,
                None
            ]
        );
        let page = prepared.html;
        assert!(page.contains(r#"url( "/remote/0" )"#));
        assert!(page.contains("src='/remote/1'"));
        assert!(page.contains("data-srcset="));
        assert!(
            page.contains(r#"href="https://x.example/link""#),
            "links stay"
        );
        assert!(page.contains(r#"data-src="https://x.example/no""#));
        assert_eq!(
            prepared.remote[2],
            Remote {
                url: "https://x.example/pixel.gif".into(),
                size: Some((1, 1))
            }
        );
    }

    #[test]
    fn neutralizes_meta_refresh() {
        let page =
            prepare(r#"<meta HTTP-EQUIV="refresh" content="0;url=https://evil.example">"#).html;
        assert!(!page.to_ascii_lowercase().contains(" http-equiv"));
        assert!(page.contains("data-http-equiv"));
    }

    #[test]
    fn detects_remote_content() {
        assert!(has_remote_content(r#"<img src="https://x.example/a.png">"#));
        assert!(has_remote_content(
            r#"<td background='http://x.example/b.gif'>"#
        ));
        assert!(has_remote_content(
            r#"<div style="background: url( //x.example/c.png )">"#
        ));
        assert!(has_remote_content(
            r#"<link rel="stylesheet" href="https://x.example/s.css">"#
        ));
        assert!(!has_remote_content(
            r#"<a href="https://x.example">link</a>"#
        ));
        assert!(!has_remote_content(
            r#"<img src="cid:a"><img src="data:image/png;base64,AA">"#
        ));
    }

    #[test]
    fn policy_blocks_remote_until_allowed() {
        assert!(csp(false).contains("img-src 'self' data:;"));
        assert!(csp(false).contains("script-src 'none'"));
        assert!(csp(true).contains("img-src 'self' data: https: http:;"));
        assert!(csp(true).contains("form-action 'none'"));
    }
}
