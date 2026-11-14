//! CalDAV (RFC 4791) + CardDAV (RFC 6352) two-way sync.
//!
//! - [`config`]: accounts (`dav.json`), provider presets (Google, iCloud, Fastmail,
//!   Nextcloud, generic), keyring credentials, OAuth for Google.
//! - [`discovery`]: well-known URLs, principal, home sets, calendars and address books.
//! - [`sync`]: per-collection sync (sync-collection or ctag/etag), pushes with `If-Match`,
//!   conflicts (server wins, local copy kept), persisted [`sync::DavState`].
//! - [`items`]: local events/contacts <-> .ics/.vcf resources (core parsers), iMIP invites.
//! - [`http`], [`xml`]: the WebDAV client and its XML.
//!
//! Like the mail backends, nothing here touches the store: the UI bridge (`dav_bridge`)
//! builds [`items::LocalItem`]s and applies [`sync::RemoteChange`]s as commands.

pub mod config;
pub mod discovery;
pub mod http;
pub mod items;
pub mod sync;
pub mod xml;

#[cfg(test)]
mod mock;
#[cfg(test)]
mod tests;

pub use config::{DavAccount, Provider};
pub use discovery::{Kind, RemoteCollection};
pub use items::LocalItem;
pub use sync::{CollectionOutcome, CollectionState, DavState, ItemLink, RemoteChange};
