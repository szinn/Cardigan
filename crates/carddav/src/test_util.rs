//! Shared helpers for this crate's wiremock tests.

use secrecy::SecretString;
use url::Url;

use crate::config::{CardDavConfig, ProviderQuirks};

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
