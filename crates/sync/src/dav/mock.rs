//! In-process CalDAV/CardDAV server for tests (std TcpListener, one request per connection).
//!
//! Layout: `/.well-known/caldav|carddav` -> 301 `/dav/` (principal `/principals/me/`),
//! calendar home `/cal/me/` with `work/` (events) and `todo/` (tasks only), address book
//! home `/card/me/` with `contacts/`. `/` answers 404 so discovery uses the well-known URL.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, MutexGuard};

pub const USER: &str = "me";
pub const PASSWORD: &str = "secret";

#[derive(Default)]
pub struct Server {
    /// path -> (etag, body)
    pub resources: BTreeMap<String, (String, String)>,
    pub version: u64,
    /// (version, path) of every change, for sync-collection.
    pub history: Vec<(u64, String)>,
    pub sync_tokens: bool,
    pub auto_schedule: bool,
    /// "METHOD path" of every request.
    pub requests: Vec<String>,
}

impl Server {
    /// A change made by another client.
    pub fn put(&mut self, path: &str, body: &str) -> String {
        self.version += 1;
        let etag = format!("\"v{}\"", self.version);
        self.resources
            .insert(path.to_owned(), (etag.clone(), body.to_owned()));
        self.history.push((self.version, path.to_owned()));
        etag
    }

    pub fn delete(&mut self, path: &str) -> bool {
        let existed = self.resources.remove(path).is_some();
        if existed {
            self.version += 1;
            self.history.push((self.version, path.to_owned()));
        }
        existed
    }

    pub fn count(&self, method: &str) -> usize {
        self.requests
            .iter()
            .filter(|r| r.starts_with(&format!("{method} ")))
            .count()
    }
}

pub struct MockDav {
    pub base: String,
    server: Arc<Mutex<Server>>,
}

impl MockDav {
    pub fn start(sync_tokens: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = Arc::new(Mutex::new(Server {
            sync_tokens,
            ..Server::default()
        }));
        let shared = server.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                handle(stream, &shared);
            }
        });
        Self { base, server }
    }

    pub fn server(&self) -> MutexGuard<'_, Server> {
        self.server.lock().unwrap()
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }
}

struct HttpRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl HttpRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

struct Reply {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: String,
}

fn reply(status: u16) -> Reply {
    Reply {
        status,
        headers: Vec::new(),
        body: String::new(),
    }
}

fn multistatus(responses: &str, extra: &str) -> Reply {
    Reply {
        status: 207,
        headers: vec![("Content-Type", "application/xml".into())],
        body: format!(
            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:card="urn:ietf:params:xml:ns:carddav" xmlns:cs="http://calendarserver.org/ns/" xmlns:ical="http://apple.com/ns/ical/">{responses}{extra}</d:multistatus>"#
        ),
    }
}

fn ok(href: &str, props: &str) -> String {
    format!(
        "<d:response><d:href>{href}</d:href><d:propstat><d:prop>{props}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
    )
}

fn handle(stream: TcpStream, shared: &Arc<Mutex<Server>>) {
    let mut reader = BufReader::new(stream);
    let Some(request) = read_request(&mut reader) else {
        return;
    };
    let reply = {
        let mut server = shared.lock().unwrap();
        server
            .requests
            .push(format!("{} {}", request.method, request.path));
        route(&mut server, &request)
    };
    let mut stream = reader.into_inner();
    let mut head = format!(
        "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.status,
        reply.body.len()
    );
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(reply.body.as_bytes());
}

fn read_request(reader: &mut BufReader<TcpStream>) -> Option<HttpRequest> {
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_owned();
    let path = parts.next()?.to_owned();
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.push((name.trim().to_owned(), value.trim().to_owned()));
        }
    }
    let length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    Some(HttpRequest {
        method,
        path,
        headers,
        body: String::from_utf8(body).ok()?,
    })
}

fn route(server: &mut Server, request: &HttpRequest) -> Reply {
    let expected = format!(
        "Basic {}",
        mailbox_core::vcard::base64_encode(format!("{USER}:{PASSWORD}").as_bytes())
    );
    if request.header("Authorization") != Some(expected.as_str()) {
        return reply(401);
    }
    let path = request.path.as_str();
    match request.method.as_str() {
        "OPTIONS" => {
            let mut dav = "1, 2, calendar-access".to_owned();
            if server.auto_schedule {
                dav.push_str(", calendar-auto-schedule");
            }
            Reply {
                headers: vec![("DAV", dav)],
                ..reply(200)
            }
        }
        "PROPFIND" => propfind(server, request),
        "REPORT" if request.body.contains("sync-collection") => sync_report(server, request),
        "REPORT" => multiget(server, request),
        "GET" => match server.resources.get(path) {
            Some((etag, body)) => Reply {
                status: 200,
                headers: vec![("ETag", etag.clone())],
                body: body.clone(),
            },
            None => reply(404),
        },
        "PUT" => {
            let current = server.resources.get(path).map(|(etag, _)| etag.clone());
            let refused = match (request.header("If-None-Match"), request.header("If-Match")) {
                (Some("*"), _) => current.is_some(),
                (_, Some(wanted)) => current.as_deref() != Some(wanted),
                _ => false,
            };
            if refused {
                return reply(412);
            }
            let etag = server.put(path, &request.body);
            Reply {
                headers: vec![("ETag", etag)],
                ..reply(if current.is_some() { 204 } else { 201 })
            }
        }
        "DELETE" => {
            let Some((etag, _)) = server.resources.get(path) else {
                return reply(404);
            };
            if request
                .header("If-Match")
                .is_some_and(|wanted| wanted != etag)
            {
                return reply(412);
            }
            server.delete(path);
            reply(204)
        }
        _ => reply(405),
    }
}

fn propfind(server: &Server, request: &HttpRequest) -> Reply {
    let body = &request.body;
    let path = request.path.as_str();
    let collection = |href: &str, kind: &str, name: &str, comps: &str| {
        let token = if server.sync_tokens {
            format!("<d:sync-token>tok-{}</d:sync-token>", server.version)
        } else {
            String::new()
        };
        ok(
            href,
            &format!(
                "<d:resourcetype><d:collection/>{kind}</d:resourcetype><d:displayname>{name}</d:displayname><ical:calendar-color>#3366CCFF</ical:calendar-color><cs:getctag>ctag-{}</cs:getctag>{token}{comps}",
                server.version
            ),
        )
    };
    match path {
        "/.well-known/caldav" | "/.well-known/carddav" => Reply {
            headers: vec![("Location", "/dav/".into())],
            ..reply(301)
        },
        "/dav/" | "/principals/me/" if body.contains("current-user-principal") => multistatus(
            &ok(path, "<d:current-user-principal><d:href>/principals/me/</d:href></d:current-user-principal>"),
            "",
        ),
        "/principals/me/" if body.contains("calendar-home-set") => multistatus(
            &ok(path, "<c:calendar-home-set><d:href>/cal/me/</d:href></c:calendar-home-set>"),
            "",
        ),
        "/principals/me/" if body.contains("addressbook-home-set") => multistatus(
            &ok(path, "<card:addressbook-home-set><d:href>/card/me/</d:href></card:addressbook-home-set>"),
            "",
        ),
        "/cal/me/" => multistatus(
            &[
                ok("/cal/me/", "<d:resourcetype><d:collection/></d:resourcetype>"),
                collection("/cal/me/work/", "<c:calendar/>", "Work", "<c:supported-calendar-component-set><c:comp name=\"VEVENT\"/></c:supported-calendar-component-set>"),
                collection("/cal/me/todo/", "<c:calendar/>", "Tasks", "<c:supported-calendar-component-set><c:comp name=\"VTODO\"/></c:supported-calendar-component-set>"),
            ]
            .concat(),
            "",
        ),
        "/card/me/" => multistatus(
            &[
                ok("/card/me/", "<d:resourcetype><d:collection/></d:resourcetype>"),
                collection("/card/me/contacts/", "<card:addressbook/>", "Contacts", ""),
            ]
            .concat(),
            "",
        ),
        _ if path.ends_with('/') => {
            let mut responses = ok(path, "<d:resourcetype><d:collection/></d:resourcetype>");
            for (href, (etag, _)) in server.resources.range(path.to_owned()..) {
                if !href.starts_with(path) {
                    break;
                }
                responses.push_str(&ok(href, &format!("<d:getetag>{etag}</d:getetag><d:resourcetype/>")));
            }
            multistatus(&responses, "")
        }
        _ => match server.resources.get(path) {
            Some((etag, _)) => multistatus(&ok(path, &format!("<d:getetag>{etag}</d:getetag>")), ""),
            None => reply(404),
        },
    }
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Vec<&'a str> {
    text.split(open)
        .skip(1)
        .filter_map(|part| part.split(close).next())
        .collect()
}

fn sync_report(server: &Server, request: &HttpRequest) -> Reply {
    if !server.sync_tokens {
        return reply(501);
    }
    let token = between(&request.body, "<d:sync-token>", "</d:sync-token>")
        .first()
        .copied()
        .unwrap_or("");
    let since = match token {
        "" => None,
        token => match token
            .strip_prefix("tok-")
            .and_then(|n| n.parse::<u64>().ok())
        {
            Some(n) => Some(n),
            None => return reply(403),
        },
    };
    let path = request.path.as_str();
    let mut changed: Vec<&String> = match since {
        None => server.resources.keys().collect(),
        Some(n) => server
            .history
            .iter()
            .filter(|(version, _)| *version > n)
            .map(|(_, path)| path)
            .collect(),
    };
    changed.retain(|href| href.starts_with(path));
    changed.sort();
    changed.dedup();
    let responses: String = changed
        .into_iter()
        .map(|href| match server.resources.get(href) {
            Some((etag, _)) => ok(href, &format!("<d:getetag>{etag}</d:getetag>")),
            None => format!(
                "<d:response><d:href>{href}</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>"
            ),
        })
        .collect();
    multistatus(
        &responses,
        &format!("<d:sync-token>tok-{}</d:sync-token>", server.version),
    )
}

fn multiget(server: &Server, request: &HttpRequest) -> Reply {
    let data = if request.body.contains("calendar-multiget") {
        "c:calendar-data"
    } else {
        "card:address-data"
    };
    let responses: String = between(&request.body, "<d:href>", "</d:href>")
        .into_iter()
        .filter_map(|href| {
            let (etag, body) = server.resources.get(href)?;
            Some(ok(
                href,
                &format!(
                    "<d:getetag>{etag}</d:getetag><{data}>{}</{data}>",
                    super::xml::escape(body)
                ),
            ))
        })
        .collect();
    multistatus(&responses, "")
}
