//! WebDAV XML: request bodies and `207 Multi-Status` parsing (RFC 4918, 4791, 6352, 6578).
//!
//! Properties are keyed by their local name only: the few properties we read never collide
//! across the DAV, CalDAV, CardDAV, CalendarServer and Apple namespaces.

use std::collections::HashMap;

use crate::{SyncError, SyncResult};

const NAMESPACES: &str = r#"xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:card="urn:ietf:params:xml:ns:carddav" xmlns:cs="http://calendarserver.org/ns/" xmlns:ical="http://apple.com/ns/ical/""#;

/// One property of a response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Prop {
    /// Text content (trimmed), e.g. an etag or a display name.
    pub text: String,
    /// `<d:href>` children, e.g. of `calendar-home-set`.
    pub hrefs: Vec<String>,
    /// Local names of child elements, e.g. `collection`, `calendar` in `resourcetype`.
    pub children: Vec<String>,
    /// `name` attributes of children, e.g. `VEVENT` in `supported-calendar-component-set`.
    pub names: Vec<String>,
}

/// One `<d:response>`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Response {
    pub href: String,
    /// Response-level status (sync-collection reports deletions as `404`).
    pub status: Option<u16>,
    /// Properties found (`200` propstats only).
    pub props: HashMap<String, Prop>,
}

impl Response {
    pub fn text(&self, name: &str) -> Option<&str> {
        self.props
            .get(name)
            .map(|prop| prop.text.as_str())
            .filter(|text| !text.is_empty())
    }

    pub fn href_of(&self, name: &str) -> Option<&str> {
        self.props
            .get(name)
            .and_then(|prop| prop.hrefs.first())
            .map(String::as_str)
    }

    pub fn has_child(&self, name: &str, child: &str) -> bool {
        self.props
            .get(name)
            .is_some_and(|prop| prop.children.iter().any(|c| c == child))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Multistatus {
    pub responses: Vec<Response>,
    /// Top-level `<d:sync-token>` of a sync-collection report.
    pub sync_token: Option<String>,
}

/// Parses a multistatus body.
pub fn parse_multistatus(body: &str) -> SyncResult<Multistatus> {
    let doc = roxmltree::Document::parse(body)
        .map_err(|err| SyncError::Protocol(format!("invalid DAV XML: {err}")))?;
    let root = doc.root_element();
    if root.tag_name().name() != "multistatus" {
        return Err(SyncError::Protocol(format!(
            "expected multistatus, got <{}>",
            root.tag_name().name()
        )));
    }
    let mut result = Multistatus::default();
    for node in root.children().filter(roxmltree::Node::is_element) {
        match node.tag_name().name() {
            "response" => result.responses.push(parse_response(node)),
            "sync-token" => result.sync_token = Some(text_of(node)),
            _ => {}
        }
    }
    Ok(result)
}

fn parse_response(node: roxmltree::Node) -> Response {
    let mut response = Response::default();
    for child in node.children().filter(roxmltree::Node::is_element) {
        match child.tag_name().name() {
            "href" => response.href = text_of(child),
            "status" => response.status = parse_status(&text_of(child)),
            "propstat" => {
                let ok = child
                    .children()
                    .find(|n| n.tag_name().name() == "status")
                    .and_then(|n| parse_status(&text_of(n)))
                    .is_none_or(|code| (200..300).contains(&code));
                if !ok {
                    continue;
                }
                let props = child.children().filter(|n| n.tag_name().name() == "prop");
                for prop in props.flat_map(|p| p.children().filter(roxmltree::Node::is_element)) {
                    response
                        .props
                        .insert(prop.tag_name().name().to_owned(), parse_prop(prop));
                }
            }
            _ => {}
        }
    }
    response
}

fn parse_prop(node: roxmltree::Node) -> Prop {
    let mut prop = Prop {
        text: text_of(node),
        ..Prop::default()
    };
    for child in node.children().filter(roxmltree::Node::is_element) {
        let name = child.tag_name().name();
        if name == "href" {
            prop.hrefs.push(text_of(child));
        }
        if let Some(value) = child.attribute("name") {
            prop.names.push(value.to_owned());
        }
        prop.children.push(name.to_owned());
    }
    prop
}

/// Concatenated text of a node (CDATA included), trimmed.
fn text_of(node: roxmltree::Node) -> String {
    node.descendants()
        .filter(roxmltree::Node::is_text)
        .filter_map(|n| n.text())
        .collect::<String>()
        .trim()
        .to_owned()
}

/// `HTTP/1.1 200 OK` -> 200.
fn parse_status(line: &str) -> Option<u16> {
    line.split_whitespace().nth(1)?.parse().ok()
}

pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A `PROPFIND` body asking for `props` (prefixed names, e.g. `d:getetag`).
pub fn propfind(props: &[&str]) -> String {
    let props: String = props.iter().map(|p| format!("<{p}/>")).collect();
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><d:propfind {NAMESPACES}><d:prop>{props}</d:prop></d:propfind>"#
    )
}

/// Properties of a calendar or address book collection.
pub const COLLECTION_PROPS: &[&str] = &[
    "d:resourcetype",
    "d:displayname",
    "ical:calendar-color",
    "cs:getctag",
    "d:sync-token",
    "c:supported-calendar-component-set",
];

/// An RFC 6578 `sync-collection` report (empty token = initial sync).
pub fn sync_collection(token: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><d:sync-collection {NAMESPACES}><d:sync-token>{}</d:sync-token><d:sync-level>1</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>"#,
        escape(token)
    )
}

/// A `calendar-multiget` (`calendar` true) or `addressbook-multiget` report.
pub fn multiget(calendar: bool, hrefs: &[String]) -> String {
    let (root, data) = if calendar {
        ("c:calendar-multiget", "c:calendar-data")
    } else {
        ("card:addressbook-multiget", "card:address-data")
    };
    let hrefs: String = hrefs
        .iter()
        .map(|href| format!("<d:href>{}</d:href>", escape(href)))
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><{root} {NAMESPACES}><d:prop><d:getetag/><{data}/></d:prop>{hrefs}</{root}>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_collections_and_sync_reports() {
        let body = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:A="http://apple.com/ns/ical/">
  <d:response>
    <d:href>/cal/user/work/</d:href>
    <d:propstat>
      <d:prop>
        <d:resourcetype><d:collection/><C:calendar/></d:resourcetype>
        <d:displayname>Work &amp; co</d:displayname>
        <A:calendar-color>#FF8800FF</A:calendar-color>
        <C:supported-calendar-component-set><C:comp name="VEVENT"/></C:supported-calendar-component-set>
        <C:calendar-data><![CDATA[BEGIN:VCALENDAR
END:VCALENDAR]]></C:calendar-data>
      </d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
    <d:propstat>
      <d:prop><d:sync-token/></d:prop>
      <d:status>HTTP/1.1 404 Not Found</d:status>
    </d:propstat>
  </d:response>
  <d:response><d:href>/cal/user/work/gone.ics</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>
  <d:sync-token>http://x/sync/5</d:sync-token>
</d:multistatus>"#;
        let status = parse_multistatus(body).unwrap();
        assert_eq!(status.sync_token.as_deref(), Some("http://x/sync/5"));
        let work = &status.responses[0];
        assert_eq!(work.href, "/cal/user/work/");
        assert!(work.has_child("resourcetype", "calendar"));
        assert_eq!(work.text("displayname"), Some("Work & co"));
        assert_eq!(work.text("calendar-color"), Some("#FF8800FF"));
        assert_eq!(
            work.props["supported-calendar-component-set"].names,
            vec!["VEVENT"]
        );
        assert!(
            work.text("calendar-data")
                .unwrap()
                .starts_with("BEGIN:VCALENDAR")
        );
        assert!(!work.props.contains_key("sync-token"));
        assert_eq!(status.responses[1].status, Some(404));
    }

    #[test]
    fn builds_escaped_bodies() {
        assert!(sync_collection("a&b").contains("<d:sync-token>a&amp;b</d:sync-token>"));
        let body = multiget(false, &["/ab/1.vcf".into()]);
        assert!(body.contains("card:addressbook-multiget"));
        assert!(body.contains("<d:href>/ab/1.vcf</d:href>"));
        assert!(roxmltree::Document::parse(&body).is_ok());
        assert!(roxmltree::Document::parse(&propfind(COLLECTION_PROPS)).is_ok());
    }

    #[test]
    fn rejects_non_multistatus() {
        assert!(parse_multistatus("<html/>").is_err());
        assert!(parse_multistatus("not xml").is_err());
    }
}
