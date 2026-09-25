use std::fmt;

use crate::contact::{ETag, Href, string_id};

string_id!(
    /// Opaque RFC 6578 sync token, stored and replayed exactly as the server
    /// sent it. Not PII.
    SyncToken
);

/// The address book collection discovery resolved for one side. The fields
/// map onto `state::Endpoint`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    /// Absolute URL of the address book collection.
    pub addressbook_url: String,
    /// The host principal discovery resolved (iCloud's numbered `pXX-` host).
    pub discovered_host: String,
    /// The collection answers `sync-collection` REPORTs. When false, callers
    /// enumerate with `AddressBook::list_etags`.
    pub supports_sync_collection: bool,
}

/// What changed in the collection since a sync token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeSet {
    /// Created or modified resources, with their current ETags.
    pub changed: Vec<(Href, ETag)>,
    /// Deleted resources. May include hrefs the caller never saw (created
    /// and deleted between two polls); callers ignore those.
    pub removed: Vec<Href>,
    /// Token to store and pass to the next `changes_since`.
    pub token: SyncToken,
}

/// Result of `AddressBook::changes_since`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Changes {
    Delta(ChangeSet),
    /// The server rejected the token (expired or unknown). Fall back to
    /// `changes_since(None)`: the full membership plus a fresh token.
    TokenInvalid,
}

/// One card exactly as the server returned it.
#[derive(Clone, PartialEq, Eq)]
pub struct FetchedCard {
    pub href: Href,
    pub etag: ETag,
    /// Verbatim vCard bytes. Full contact data (PII): never log it.
    pub body: Vec<u8>,
}

impl fmt::Debug for FetchedCard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FetchedCard")
            .field("href", &self.href)
            .field("etag", &self.etag)
            .field("bytes", &self.body.len())
            .finish()
    }
}

/// Result of `AddressBook::multiget`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MultigetResult {
    pub found: Vec<FetchedCard>,
    /// Requested hrefs the server no longer has (deleted mid-cycle). Not an
    /// error.
    pub missing: Vec<Href>,
}

/// Guard on a `put`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Precondition {
    /// `If-Match: <etag>`: update only if the resource still has this ETag.
    IfMatch(ETag),
    /// `If-None-Match: *`: create only; fail if the resource exists.
    IfNoneMatch,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetched_card_debug_omits_body() {
        let card = FetchedCard {
            href: Href::from("/a.vcf"),
            etag: ETag::from("\"1\""),
            body: b"BEGIN:VCARD\r\nVERSION:3.0\r\nEMAIL:jane@example.com\r\nEND:VCARD\r\n".to_vec(),
        };
        let debug = format!("{card:?}");
        assert!(debug.contains("/a.vcf"), "{debug}");
        assert!(!debug.contains("jane@example.com"), "card content leaked into Debug: {debug}");
    }

    #[test]
    fn sync_token_round_trips() {
        let token = SyncToken::from("https://example.test/sync/42");
        assert_eq!(token.as_str(), "https://example.test/sync/42");
        assert_eq!(token.clone().into_string(), token.to_string());
    }
}
