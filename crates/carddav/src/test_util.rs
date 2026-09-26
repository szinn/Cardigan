//! Shared helpers for this crate's wiremock tests.

use secrecy::SecretString;
use url::Url;

use crate::config::{CardDavConfig, ProviderQuirks};

/// `Authorization` value for user `user`, password `pass`.
pub(crate) const BASIC: &str = "Basic dXNlcjpwYXNz";

pub(crate) fn config(entry: &str, quirks: ProviderQuirks) -> CardDavConfig {
    CardDavConfig::new(Url::parse(entry).unwrap(), "user", SecretString::from("pass"), quirks)
}
