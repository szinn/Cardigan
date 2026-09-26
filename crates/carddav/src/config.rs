use std::time::Duration;

use secrecy::SecretString;
use url::Url;

/// Hrefs per `addressbook-multiget` REPORT. Keeps each request well under
/// iCloud's rate limits and bounds the response size (cards may carry photos).
pub const DEFAULT_MULTIGET_BATCH: usize = 50;
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Generous: a multiget of cards with photos can be several megabytes.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Provider-specific behaviour. The composition root picks it; the adapter
/// never sniffs the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderQuirks {
    /// Last path segment of the address book to pick when the home set holds
    /// several.
    pub default_collection: Option<String>,
    /// Maximum hrefs per `addressbook-multiget` REPORT.
    pub multiget_batch: usize,
}

impl ProviderQuirks {
    #[must_use]
    pub fn icloud() -> Self {
        Self {
            default_collection: Some("card".to_owned()),
            multiget_batch: DEFAULT_MULTIGET_BATCH,
        }
    }

    #[must_use]
    pub fn fastmail() -> Self {
        Self {
            default_collection: Some("Default".to_owned()),
            multiget_batch: DEFAULT_MULTIGET_BATCH,
        }
    }
}

impl Default for ProviderQuirks {
    fn default() -> Self {
        Self {
            default_collection: None,
            multiget_batch: DEFAULT_MULTIGET_BATCH,
        }
    }
}

/// Everything one adapter instance needs. `Debug` redacts the password
/// (secrecy's `SecretString`).
#[derive(Debug)]
pub struct CardDavConfig {
    /// Discovery entry point, e.g. `https://contacts.icloud.com`.
    pub entry_url: Url,
    pub username: String,
    /// App-specific password. Never logged.
    pub password: SecretString,
    pub quirks: ProviderQuirks,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
}

impl CardDavConfig {
    /// A config with the default timeouts.
    pub fn new(entry_url: Url, username: impl Into<String>, password: SecretString, quirks: ProviderQuirks) -> Self {
        Self {
            entry_url,
            username: username.into(),
            password,
            quirks,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_password() {
        let config = CardDavConfig::new(
            Url::parse("https://contacts.icloud.com").unwrap(),
            "jane@icloud.com",
            SecretString::from("abcd-efgh-ijkl-mnop"),
            ProviderQuirks::icloud(),
        );
        let debug = format!("{config:?}");
        assert!(!debug.contains("abcd-efgh-ijkl-mnop"), "password leaked into Debug: {debug}");
        assert!(debug.contains("REDACTED"), "{debug}");
    }

    #[test]
    fn new_uses_default_timeouts_and_provider_quirks() {
        let config = CardDavConfig::new(
            Url::parse("https://carddav.fastmail.com").unwrap(),
            "jane@fastmail.com",
            SecretString::from("secret"),
            ProviderQuirks::fastmail(),
        );
        assert_eq!(config.connect_timeout, DEFAULT_CONNECT_TIMEOUT);
        assert_eq!(config.request_timeout, DEFAULT_REQUEST_TIMEOUT);
        assert_eq!(config.quirks.default_collection.as_deref(), Some("Default"));
        assert_eq!(ProviderQuirks::icloud().default_collection.as_deref(), Some("card"));
        assert_eq!(ProviderQuirks::default().default_collection, None);
        assert_eq!(ProviderQuirks::default().multiget_batch, DEFAULT_MULTIGET_BATCH);
    }
}
