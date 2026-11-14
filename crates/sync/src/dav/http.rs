//! Minimal blocking WebDAV client over `ureq`: custom methods, manual redirects (method and
//! body are kept, unlike ureq's own handling), Basic or Bearer auth.

use std::io::Read as _;
use std::time::Duration;

use url::Url;

use super::xml::{self, Multistatus};
use crate::{SyncError, SyncResult};

/// Largest body read (address books with photos can be big).
const MAX_BODY: u64 = 64 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

#[derive(Clone)]
pub enum Auth {
    Basic { user: String, password: String },
    Bearer(String),
}

impl std::fmt::Debug for Auth {
    // Never print secrets.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Basic { user, .. } => write!(f, "Basic({user})"),
            Self::Bearer(_) => f.write_str("Bearer"),
        }
    }
}

impl Auth {
    fn header(&self) -> String {
        match self {
            Self::Basic { user, password } => format!(
                "Basic {}",
                mailbox_core::vcard::base64_encode(format!("{user}:{password}").as_bytes())
            ),
            Self::Bearer(token) => format!("Bearer {token}"),
        }
    }
}

/// A server answer (any status).
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    /// Final URL after redirects.
    pub url: Url,
    pub etag: Option<String>,
    /// `DAV:` header (compliance classes, e.g. `calendar-auto-schedule`).
    pub dav: Option<String>,
    pub body: String,
}

impl HttpResponse {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// The body as a multistatus, or an error for any other answer.
    pub fn multistatus(&self) -> SyncResult<Multistatus> {
        if !self.is_success() {
            return Err(status_error(self.status, self.url.as_str()));
        }
        xml::parse_multistatus(&self.body)
    }
}

/// Maps an HTTP failure to a sync error.
pub fn status_error(status: u16, url: &str) -> SyncError {
    match status {
        401 | 403 => SyncError::Auth(format!("server refused access ({status}) to {url}")),
        _ => SyncError::Protocol(format!("HTTP {status} from {url}")),
    }
}

/// One request: method, URL, optional XML/text body and extra headers.
pub struct Request<'a> {
    pub method: &'a str,
    pub url: Url,
    pub depth: Option<&'a str>,
    pub body: Option<(&'a str, String)>,
    pub headers: Vec<(&'a str, String)>,
}

impl<'a> Request<'a> {
    pub fn new(method: &'a str, url: Url) -> Self {
        Self {
            method,
            url,
            depth: None,
            body: None,
            headers: Vec::new(),
        }
    }

    pub fn depth(mut self, depth: &'a str) -> Self {
        self.depth = Some(depth);
        self
    }

    pub fn xml(mut self, body: String) -> Self {
        self.body = Some(("application/xml; charset=utf-8", body));
        self
    }

    pub fn body(mut self, content_type: &'a str, body: String) -> Self {
        self.body = Some((content_type, body));
        self
    }

    pub fn header(mut self, name: &'a str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }
}

#[derive(Debug, Clone)]
pub struct DavClient {
    agent: ureq::Agent,
    auth: Option<Auth>,
}

impl DavClient {
    pub fn new(auth: Option<Auth>) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            .redirects(0)
            .user_agent(concat!("Mailbox/", env!("CARGO_PKG_VERSION")))
            .build();
        Self { agent, auth }
    }

    /// Sends a request, following redirects with the same method and body.
    pub fn send(&self, request: Request) -> SyncResult<HttpResponse> {
        let mut url = request.url.clone();
        for _ in 0..=MAX_REDIRECTS {
            let mut call = self.agent.request(request.method, url.as_str());
            // Credentials only go to the same host or over https.
            if let Some(auth) = &self.auth
                && (url.host_str() == request.url.host_str() || url.scheme() == "https")
            {
                call = call.set("Authorization", &auth.header());
            }
            if let Some(depth) = request.depth {
                call = call.set("Depth", depth);
            }
            for (name, value) in &request.headers {
                call = call.set(name, value);
            }
            let result = match &request.body {
                Some((content_type, body)) => {
                    call.set("Content-Type", content_type).send_string(body)
                }
                None => call.call(),
            };
            let response = match result {
                Ok(response) | Err(ureq::Error::Status(_, response)) => response,
                Err(ureq::Error::Transport(err)) => {
                    return Err(SyncError::Connection(err.to_string()));
                }
            };
            let status = response.status();
            if (300..400).contains(&status)
                && let Some(location) = response.header("Location")
            {
                url = url
                    .join(location)
                    .map_err(|err| SyncError::Protocol(format!("bad redirect: {err}")))?;
                continue;
            }
            let etag = response.header("ETag").map(str::to_owned);
            let dav = response.header("DAV").map(str::to_owned);
            let mut body = String::new();
            response
                .into_reader()
                .take(MAX_BODY)
                .read_to_string(&mut body)?;
            return Ok(HttpResponse {
                status,
                url,
                etag,
                dav,
                body,
            });
        }
        Err(SyncError::Protocol(format!(
            "too many redirects from {}",
            request.url
        )))
    }
}

/// Parses a user-typed server address (`https://` added when missing).
pub fn parse_url(text: &str) -> SyncResult<Url> {
    let text = text.trim();
    let text = if text.contains("://") {
        text.to_owned()
    } else {
        format!("https://{text}")
    };
    Url::parse(&text).map_err(|err| SyncError::Protocol(format!("invalid URL `{text}`: {err}")))
}

/// Resolves an href from a response against the URL it came from.
pub fn resolve(base: &Url, href: &str) -> SyncResult<Url> {
    base.join(href)
        .map_err(|err| SyncError::Protocol(format!("invalid href `{href}`: {err}")))
}
