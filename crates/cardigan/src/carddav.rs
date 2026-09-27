//! Builds the cg-carddav adapter for one side from the loaded configuration.

use anyhow::Context;
use cg_carddav::{CardDavAddressBook, CardDavConfig, ProviderQuirks};
use cg_core::contact::Side;
use secrecy::SecretString;
use url::Url;

use crate::{config::Config, error::Error};

/// A `CardDavAddressBook` for `side`, with that provider's quirks. Builds the
/// HTTP client only; nothing is sent until `discover()`.
pub fn build_address_book(side: Side, config: &Config) -> anyhow::Result<CardDavAddressBook> {
    let (endpoint, quirks, variable) = match side {
        Side::ICloud => (&config.icloud, ProviderQuirks::icloud(), "CARDIGAN_ICLOUD_URL"),
        Side::Fastmail => (&config.fastmail, ProviderQuirks::fastmail(), "CARDIGAN_FASTMAIL_URL"),
    };
    let entry_url = Url::parse(&endpoint.url).map_err(|e| Error::InvalidValue {
        variable,
        reason: e.to_string(),
    })?;
    let password = SecretString::from(endpoint.password.expose().to_owned());
    CardDavAddressBook::new(CardDavConfig::new(entry_url, endpoint.username.clone(), password, quirks))
        .with_context(|| format!("Couldn't build the {side} CardDAV client"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_config as config;

    #[test]
    fn builds_both_sides_from_default_urls() {
        let config = config("https://contacts.icloud.com", "https://carddav.fastmail.com");
        build_address_book(Side::ICloud, &config).unwrap();
        build_address_book(Side::Fastmail, &config).unwrap();
    }

    fn error_message(side: Side, config: &Config) -> String {
        let error = build_address_book(side, config).err().expect("expected an error");
        format!("{error:#}")
    }

    #[test]
    fn unparseable_url_names_the_sides_variable() {
        let message = error_message(Side::ICloud, &config("not a url", "https://carddav.fastmail.com"));
        assert!(message.contains("CARDIGAN_ICLOUD_URL"), "{message}");

        let message = error_message(Side::Fastmail, &config("https://contacts.icloud.com", "::"));
        assert!(message.contains("CARDIGAN_FASTMAIL_URL"), "{message}");
    }

    #[test]
    fn unsupported_scheme_is_rejected_by_the_adapter() {
        let message = error_message(Side::ICloud, &config("ftp://contacts.icloud.com", "https://carddav.fastmail.com"));
        assert!(message.contains("icloud"), "{message}");
        assert!(!message.contains("icloud-secret-pw"), "password leaked: {message}");
    }
}
