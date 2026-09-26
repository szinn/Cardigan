//! Shared helpers for this crate's wiremock tests.

use cg_core::addressbook::AddressBook;
use secrecy::SecretString;
use url::Url;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, method, path},
};

use crate::{
    adapter::CardDavAddressBook,
    config::{CardDavConfig, ProviderQuirks},
};

/// `Authorization` value for user `user`, password `pass`.
pub(crate) const BASIC: &str = "Basic dXNlcjpwYXNz";

pub(crate) fn config(entry: &str, quirks: ProviderQuirks) -> CardDavConfig {
    CardDavConfig::new(Url::parse(entry).unwrap(), "user", SecretString::from("pass"), quirks)
}

/// Wraps response elements in a multistatus using the `d:` / `card:`
/// prefixes.
pub(crate) fn multistatus(inner: &str) -> String {
    format!(r#"<?xml version="1.0" encoding="utf-8"?><d:multistatus xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav">{inner}</d:multistatus>"#)
}

fn ok_propstat(props: &str) -> String {
    format!("<d:propstat><d:prop>{props}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>")
}

/// 207 answering a principal lookup at any path with `principal`.
pub(crate) fn principal_body(principal: &str) -> String {
    multistatus(&format!(
        "<d:response><d:href>/</d:href>{}</d:response>",
        ok_propstat(&format!("<d:current-user-principal><d:href>{principal}</d:href></d:current-user-principal>"))
    ))
}

/// 207 answering a home-set lookup with `home` (relative or absolute).
pub(crate) fn home_set_body(principal: &str, home: &str) -> String {
    multistatus(&format!(
        "<d:response><d:href>{principal}</d:href>{}</d:response>",
        ok_propstat(&format!("<card:addressbook-home-set><d:href>{home}</d:href></card:addressbook-home-set>"))
    ))
}

/// One `DAV:response` for an address book collection.
pub(crate) fn addressbook_entry(href: &str, supports_sync: bool) -> String {
    let reports = if supports_sync {
        "<d:supported-report-set><d:supported-report><d:report><d:sync-collection/></d:report></d:supported-report></d:supported-report-set>"
    } else {
        "<d:supported-report-set/>"
    };
    format!(
        "<d:response><d:href>{href}</d:href>{}</d:response>",
        ok_propstat(&format!(
            "<d:resourcetype><d:collection/><card:addressbook/></d:resourcetype><d:displayname>Contacts</d:displayname>{reports}"
        ))
    )
}

/// One `DAV:response` for a plain (non-address-book) collection.
pub(crate) fn plain_collection_entry(href: &str) -> String {
    format!(
        "<d:response><d:href>{href}</d:href>{}</d:response>",
        ok_propstat("<d:resourcetype><d:collection/></d:resourcetype>")
    )
}

/// Mounts principal `/p/`, home set `/home/` and one address book
/// `/home/card/` on `server`.
pub(crate) async fn mount_discovery(server: &MockServer, supports_sync: bool) {
    Mock::given(method("PROPFIND"))
        .and(path("/"))
        .and(body_string_contains("current-user-principal"))
        .respond_with(ResponseTemplate::new(207).set_body_string(principal_body("/p/")))
        .mount(server)
        .await;
    Mock::given(method("PROPFIND"))
        .and(path("/p/"))
        .and(body_string_contains("addressbook-home-set"))
        .respond_with(ResponseTemplate::new(207).set_body_string(home_set_body("/p/", "/home/")))
        .mount(server)
        .await;
    Mock::given(method("PROPFIND"))
        .and(path("/home/"))
        .and(body_string_contains("supported-report-set"))
        .respond_with(ResponseTemplate::new(207).set_body_string(multistatus(&format!(
            "{}{}",
            plain_collection_entry("/home/"),
            addressbook_entry("/home/card/", supports_sync)
        ))))
        .mount(server)
        .await;
}

/// An adapter bound to `/home/card/` (sync-collection supported, default
/// quirks).
pub(crate) async fn discovered(server: &MockServer) -> CardDavAddressBook {
    discovered_with(server, true, ProviderQuirks::default()).await
}

pub(crate) async fn discovered_with(server: &MockServer, supports_sync: bool, quirks: ProviderQuirks) -> CardDavAddressBook {
    mount_discovery(server, supports_sync).await;
    let adapter = CardDavAddressBook::new(config(&server.uri(), quirks)).unwrap();
    adapter.discover().await.unwrap();
    adapter
}
