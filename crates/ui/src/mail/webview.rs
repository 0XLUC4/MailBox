//! HTML mail bodies shown exactly as sent, by the system web engine (WebView2 through `wry`).
//!
//! The webview is a child window of the GPUI window, so GPUI cannot draw over it: the reading
//! pane paints a [`paper`] element where the body goes, which places the webview over it every
//! frame and hides it when the paper is not painted (other module, no selection) or when a GPUI
//! overlay (menu, popover, dialog, toast) covers it.
//!
//! Locked down: JavaScript off, a strict Content-Security-Policy (no scripts, forms, frames,
//! plugins or connections; pictures, styles and fonts only from the message itself until "Show
//! pictures"), no navigation (clicked http/https/mailto links go to the app), an InPrivate
//! profile (no cookies or storage), no devtools, no browser shortcuts, no downloads.

use std::borrow::Cow;
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};

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
}

/// The document served for one message.
#[derive(Default)]
pub struct Page {
    /// HTML from [`prepare`].
    pub html: String,
    /// Load remote pictures, styles and fonts ("Show pictures").
    pub remote: bool,
    /// `/inline/<n>` → (MIME type, stored file), in [`prepare`]'s `cid:` order.
    pub inline: Vec<Option<(String, PathBuf)>>,
}

pub struct MailWebView {
    webview: wry::WebView,
    page: Arc<Mutex<Page>>,
    /// Key of the page loaded in the webview.
    loaded: std::cell::RefCell<String>,
    visible: Cell<bool>,
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
        let (navigate, new_window) = (events.clone(), events);
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
            .with_custom_protocol(SCHEME.into(), move |_, request| {
                let page = served.lock().unwrap_or_else(PoisonError::into_inner);
                respond(&page, &request)
            })
            .with_navigation_handler(move |url| {
                if url.starts_with(ORIGIN) || url == "about:blank" {
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

    pub fn set_visible(&self, visible: bool) {
        if self.visible.get() != visible && self.webview.set_visible(visible).is_ok() {
            self.visible.set(visible);
        }
    }

    fn place(&self, bounds: Bounds<Pixels>) {
        if self.bounds.get() == Some(bounds) {
            return;
        }
        let rect = wry::Rect {
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
        };
        if self.webview.set_bounds(rect).is_ok() {
            self.bounds.set(Some(bounds));
        }
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

/// The message HTML as served: `cid:` references point at `/inline/<n>` (returned in order),
/// `http-equiv` is neutralized (no `<meta refresh>` redirect), and the page asks for a light
/// colour scheme like the sender designed it. Doctype (and so the rendering mode) is kept.
pub fn prepare(html: &str) -> (String, Vec<String>) {
    let mut out = String::with_capacity(html.len() + 128);
    let mut cids = Vec::new();
    let lower = html.to_ascii_lowercase();
    let mut rest = 0;
    let mut search = 0;
    while let Some(found) = lower[search..].find("cid:") {
        let start = search + found;
        search = start + 4;
        let quoted = lower[..start].trim_end().ends_with(['"', '\'', '=', '(']);
        if !quoted {
            continue;
        }
        let end = html[search..]
            .find(|c: char| matches!(c, '"' | '\'' | ')' | '>') || c.is_whitespace())
            .map_or(html.len(), |len| search + len);
        out.push_str(&html[rest..start]);
        out.push_str(&format!("/inline/{}", cids.len()));
        cids.push(html[search..end].to_owned());
        rest = end;
        search = end;
    }
    out.push_str(&html[rest..]);
    let out = replace_ignore_case(&out, "http-equiv", "data-http-equiv");

    const HEAD: &str = r#"<meta name="color-scheme" content="light only">"#;
    let trimmed = out.trim_start();
    let doctype_end = trimmed
        .get(..9)
        .filter(|start| start.eq_ignore_ascii_case("<!doctype"))
        .and_then(|_| trimmed.find('>'))
        .map(|end| out.len() - trimmed.len() + end + 1);
    let page = match doctype_end {
        Some(end) => format!("{}{HEAD}{}", &out[..end], &out[end..]),
        None => format!("{HEAD}{out}"),
    };
    (page, cids)
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
    use super::{csp, has_remote_content, prepare};

    #[test]
    fn rewrites_cid_references_in_order() {
        let (page, cids) = prepare(
            r#"<!DOCTYPE html><img src="cid:logo@x"><div style="background:url(cid:bg.png)">cid: text</div><img src=CID:hero>"#,
        );
        assert_eq!(cids, ["logo@x", "bg.png", "hero"]);
        assert!(page.starts_with("<!DOCTYPE html><meta name=\"color-scheme\""));
        assert!(page.contains(r#"src="/inline/0""#));
        assert!(page.contains("url(/inline/1)"));
        assert!(page.contains("src=/inline/2>"));
        assert!(page.contains("cid: text"), "plain text is left alone");
    }

    #[test]
    fn neutralizes_meta_refresh() {
        let (page, _) =
            prepare(r#"<meta HTTP-EQUIV="refresh" content="0;url=https://evil.example">"#);
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
