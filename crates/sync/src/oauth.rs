//! OAuth2 for Gmail / Microsoft 365: authorization code + PKCE with a loopback redirect
//! (`http://127.0.0.1:<port>/`), refresh tokens in the OS credential store, access tokens
//! cached in memory. Client ids come from config or env; no secret is compiled in.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use oauth2::basic::BasicClient;
use oauth2::url::Url;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointNotSet, EndpointSet,
    PkceCodeChallenge, RedirectUrl, RefreshToken, Scope, TokenResponse, TokenUrl,
};

use crate::config::{OAuthProvider, credentials};
use crate::{SyncError, SyncResult};

/// Keyring service for refresh tokens (user = account email).
pub const REFRESH_TOKEN_SERVICE: &str = "mailbox-oauth";

/// How long the browser sign-in may take.
const AUTHORIZE_TIMEOUT: Duration = Duration::from_secs(300);

/// One interactive sign-in at a time (the sync and IDLE connections share tokens).
static AUTH_LOCK: Mutex<()> = Mutex::new(());

type Client = BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthSettings {
    pub auth_url: String,
    pub token_url: String,
    pub scopes: Vec<String>,
    pub client_id: String,
    /// Google "desktop app" clients have a (non-confidential) secret; read from env.
    pub client_secret: Option<String>,
}

impl OAuthSettings {
    pub fn for_provider(provider: OAuthProvider, client_id: Option<&str>) -> SyncResult<Self> {
        let (auth_url, token_url, scopes, prefix) = match provider {
            OAuthProvider::Google => (
                "https://accounts.google.com/o/oauth2/v2/auth",
                "https://oauth2.googleapis.com/token",
                vec!["https://mail.google.com/"],
                "MAILBOX_GOOGLE",
            ),
            OAuthProvider::Microsoft => (
                "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
                "https://login.microsoftonline.com/common/oauth2/v2.0/token",
                vec![
                    "https://outlook.office.com/IMAP.AccessAsUser.All",
                    "https://outlook.office.com/SMTP.Send",
                    "offline_access",
                ],
                "MAILBOX_MICROSOFT",
            ),
        };
        let env = |name: &str| std::env::var(format!("{prefix}_{name}")).ok();
        let client_id = client_id
            .map(str::to_owned)
            .filter(|id| !id.trim().is_empty())
            .or_else(|| env("CLIENT_ID"))
            .or_else(|| {
                (provider == OAuthProvider::Microsoft)
                    .then(|| std::env::var("MAILBOX_MS_CLIENT_ID").ok())
                    .flatten()
            })
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| {
                SyncError::Auth(format!("no OAuth client id: set {prefix}_CLIENT_ID"))
            })?;
        Ok(Self {
            auth_url: auth_url.to_owned(),
            token_url: token_url.to_owned(),
            scopes: scopes.into_iter().map(str::to_owned).collect(),
            client_id,
            client_secret: env("CLIENT_SECRET"),
        })
    }

    fn client(&self, redirect: Option<String>) -> SyncResult<Client> {
        let invalid = |err: oauth2::url::ParseError| SyncError::Auth(err.to_string());
        let client = BasicClient::new(ClientId::new(self.client_id.clone()))
            .set_auth_uri(AuthUrl::new(self.auth_url.clone()).map_err(invalid)?)
            .set_token_uri(TokenUrl::new(self.token_url.clone()).map_err(invalid)?);
        let client = match redirect {
            Some(url) => client.set_redirect_uri(RedirectUrl::new(url).map_err(invalid)?),
            None => client,
        };
        Ok(match &self.client_secret {
            Some(secret) => client.set_client_secret(ClientSecret::new(secret.clone())),
            None => client,
        })
    }
}

/// Access tokens for one account, refreshed as needed.
pub struct TokenProvider {
    email: String,
    settings: OAuthSettings,
    cached: Option<(String, Instant)>,
}

impl TokenProvider {
    pub fn new(email: impl Into<String>, settings: OAuthSettings) -> Self {
        Self {
            email: email.into(),
            settings,
            cached: None,
        }
    }

    /// A valid access token: cached, refreshed, or (first use) from a browser sign-in.
    pub fn access_token(&mut self) -> SyncResult<String> {
        if let Some((token, expires)) = &self.cached
            && Instant::now() < *expires
        {
            return Ok(token.clone());
        }
        let _guard = AUTH_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let token = match credentials::get(REFRESH_TOKEN_SERVICE, &self.email) {
            Ok(refresh) => match self.refresh(&refresh) {
                Ok(token) => token,
                // Revoked or expired refresh token: sign in again.
                Err(SyncError::Auth(_)) => self.authorize()?,
                Err(err) => return Err(err),
            },
            Err(_) => self.authorize()?,
        };
        Ok(token)
    }

    /// Forgets the cached access token (after an authentication failure).
    pub fn invalidate(&mut self) {
        self.cached = None;
    }

    fn refresh(&mut self, refresh_token: &str) -> SyncResult<String> {
        let response = self
            .settings
            .client(None)?
            .exchange_refresh_token(&RefreshToken::new(refresh_token.to_owned()))
            .request(&http_client())
            .map_err(|err| SyncError::Auth(err.to_string()))?;
        self.store(&response)
    }

    fn authorize(&mut self) -> SyncResult<String> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let redirect = format!("http://127.0.0.1:{}/", listener.local_addr()?.port());
        let client = self.settings.client(Some(redirect))?;
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, csrf) = client
            .authorize_url(CsrfToken::new_random)
            .add_scopes(self.settings.scopes.iter().cloned().map(Scope::new))
            .add_extra_param("login_hint", self.email.clone())
            .set_pkce_challenge(challenge)
            .url();
        open_browser(url.as_str())?;
        let code = wait_for_code(&listener, csrf.secret(), AUTHORIZE_TIMEOUT)?;
        let response = client
            .exchange_code(AuthorizationCode::new(code))
            .set_pkce_verifier(verifier)
            .request(&http_client())
            .map_err(|err| SyncError::Auth(err.to_string()))?;
        self.store(&response)
    }

    fn store(&mut self, response: &impl TokenResponse) -> SyncResult<String> {
        if let Some(refresh) = response.refresh_token() {
            credentials::set(REFRESH_TOKEN_SERVICE, &self.email, refresh.secret())?;
        }
        let token = response.access_token().secret().clone();
        // Renew a minute early; assume one hour when the server does not say.
        let lifetime = response
            .expires_in()
            .unwrap_or(Duration::from_secs(3600))
            .saturating_sub(Duration::from_secs(60));
        self.cached = Some((token.clone(), Instant::now() + lifetime));
        Ok(token)
    }
}

fn http_client() -> oauth2::ureq::Agent {
    // No redirects: following them on token requests would be an SSRF vector.
    oauth2::ureq::AgentBuilder::new().redirects(0).build()
}

/// SASL XOAUTH2 initial response (IMAP `AUTHENTICATE XOAUTH2`, before base64).
pub fn xoauth2(user: &str, access_token: &str) -> String {
    format!("user={user}\x01auth=Bearer {access_token}\x01\x01")
}

fn open_browser(url: &str) -> SyncResult<()> {
    let mut command = if cfg!(target_os = "windows") {
        // `start` treats the first quoted argument as a window title.
        let mut command = std::process::Command::new("cmd");
        command
            .args(["/C", "start", ""])
            .arg(url.replace('&', "^&"));
        command
    } else if cfg!(target_os = "macos") {
        let mut command = std::process::Command::new("open");
        command.arg(url);
        command
    } else {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(url);
        command
    };
    command.spawn()?;
    Ok(())
}

/// Accepts redirect requests until one carries our `state`, then returns its `code`.
fn wait_for_code(listener: &TcpListener, state: &str, timeout: Duration) -> SyncResult<String> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            Err(err) => return Err(err.into()),
        };
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut request_line = String::new();
        {
            let mut reader = BufReader::new(&stream);
            reader.read_line(&mut request_line)?;
            // Drain the headers: closing with unread data would reset the connection.
            let mut header = String::new();
            while reader.read_line(&mut header).is_ok_and(|read| read > 2) {
                header.clear();
            }
        }
        let result = parse_redirect(&request_line, state);
        let body = match &result {
            Some(Ok(_)) => "Signed in. You can close this window and return to Mailbox.",
            Some(Err(_)) => "Sign-in failed. You can close this window.",
            None => "Waiting for sign-in...",
        };
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        if let Some(result) = result {
            return result;
        }
    }
    Err(SyncError::Auth("sign-in timed out".into()))
}

/// `GET /?code=..&state=.. HTTP/1.1` → the code. `None` = not our redirect (e.g. favicon).
fn parse_redirect(request_line: &str, state: &str) -> Option<SyncResult<String>> {
    let target = request_line.split_whitespace().nth(1)?;
    let url = Url::parse(&format!("http://127.0.0.1{target}")).ok()?;
    let param = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    };
    if let Some(error) = param("error") {
        return Some(Err(SyncError::Auth(error)));
    }
    let code = param("code")?;
    if param("state").as_deref() != Some(state) {
        return Some(Err(SyncError::Auth("state mismatch".into())));
    }
    Some(Ok(code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_parsing() {
        let ok = parse_redirect("GET /?code=abc%2F1&state=xyz HTTP/1.1\r\n", "xyz");
        assert_eq!(ok.unwrap().unwrap(), "abc/1");
        assert!(
            parse_redirect("GET /?code=abc&state=evil HTTP/1.1", "xyz")
                .unwrap()
                .is_err()
        );
        assert!(
            parse_redirect("GET /?error=access_denied HTTP/1.1", "xyz")
                .unwrap()
                .is_err()
        );
        assert!(parse_redirect("GET /favicon.ico HTTP/1.1", "xyz").is_none());
    }

    #[test]
    fn loopback_flow_returns_the_code() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let browser = std::thread::spawn(move || {
            let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(
                stream,
                "GET /?code=c0de&state=s HTTP/1.1\r\nHost: x\r\n\r\n"
            )
            .unwrap();
            let mut response = String::new();
            std::io::Read::read_to_string(&mut stream, &mut response).unwrap();
            response
        });
        let code = wait_for_code(&listener, "s", Duration::from_secs(10)).unwrap();
        assert_eq!(code, "c0de");
        assert!(browser.join().unwrap().starts_with("HTTP/1.1 200 OK"));
    }

    #[test]
    fn settings_need_a_client_id() {
        let settings = OAuthSettings::for_provider(OAuthProvider::Google, Some("id-1")).unwrap();
        assert_eq!(settings.client_id, "id-1");
        assert_eq!(settings.scopes, vec!["https://mail.google.com/".to_owned()]);
        assert!(OAuthSettings::for_provider(OAuthProvider::Microsoft, Some(" ")).is_err());
    }

    #[test]
    fn xoauth2_format() {
        assert_eq!(
            xoauth2("me@x.com", "tok"),
            "user=me@x.com\x01auth=Bearer tok\x01\x01"
        );
    }
}
