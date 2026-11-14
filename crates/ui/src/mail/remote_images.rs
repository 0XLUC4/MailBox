//! HTTP client GPUI uses to fetch remote pictures, only after "Show pictures" (the HTML renderer
//! never references a remote image otherwise). Blocking `ureq` inside the future is fine: GPUI
//! loads images on its background executor.

use std::future::Future;
use std::io::Read as _;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use gpui::App;
use gpui::http_client::http::{self, HeaderValue};
use gpui::http_client::{AsyncBody, HttpClient, Result, Url, anyhow};

type Fetch = Pin<Box<dyn Future<Output = Result<http::Response<AsyncBody>>> + Send + 'static>>;

/// Pictures larger than this are not shown.
const MAX_BYTES: u64 = 15 * 1024 * 1024;

struct ImageClient {
    agent: ureq::Agent,
    user_agent: HeaderValue,
}

impl HttpClient for ImageClient {
    fn user_agent(&self) -> Option<&HeaderValue> {
        Some(&self.user_agent)
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }

    fn send(&self, request: http::Request<AsyncBody>) -> Fetch {
        let agent = self.agent.clone();
        let user_agent = self.user_agent.to_str().unwrap_or("Mailbox").to_owned();
        let uri = request.uri().to_string();
        Box::pin(async move { fetch(&agent, &uri, &user_agent) })
    }
}

/// GETs `uri` (http/https only) and returns the body, status included.
fn fetch(agent: &ureq::Agent, uri: &str, user_agent: &str) -> Result<http::Response<AsyncBody>> {
    let lower = uri.to_ascii_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return Err(anyhow!("unsupported image URL"));
    }
    let response = match agent.get(uri).set("User-Agent", user_agent).call() {
        Ok(response) => response,
        Err(ureq::Error::Status(code, _)) => {
            return Ok(http::Response::builder()
                .status(code)
                .body(AsyncBody::empty())?);
        }
        Err(err) => return Err(err.into()),
    };
    let status = response.status();
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(anyhow!("image too large"));
    }
    Ok(http::Response::builder()
        .status(status)
        .body(AsyncBody::from(bytes))?)
}

/// Installs the client (idempotent).
pub fn install(cx: &mut App) {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
        .redirects(5)
        .build();
    cx.set_http_client(Arc::new(ImageClient {
        agent,
        user_agent: HeaderValue::from_static("Mailbox"),
    }));
}
