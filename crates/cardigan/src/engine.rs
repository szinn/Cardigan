//! Builds the sync engine from the loaded configuration.

use std::sync::Arc;

use anyhow::Context;
use cg_core::{
    addressbook::AddressBook,
    contact::Side,
    repository::RepositoryService,
    service::{SyncConfig, SyncService, SystemClock},
};
use chrono::TimeDelta;

use crate::{carddav::build_address_book, config::Config};

/// The `SyncService` for both configured address books. Builds the HTTP
/// clients only; nothing is sent until the first cycle discovers the
/// collections.
pub fn build_sync_service(config: &Config, repository_service: Arc<RepositoryService>) -> anyhow::Result<SyncService> {
    let icloud: Arc<dyn AddressBook> = Arc::new(build_address_book(Side::ICloud, config)?);
    let fastmail: Arc<dyn AddressBook> = Arc::new(build_address_book(Side::Fastmail, config)?);
    let sync_config = SyncConfig {
        winner: config.conflict_winner,
        poll_interval: TimeDelta::from_std(config.poll_interval).context("CARDIGAN_POLL_INTERVAL_SECS is too large")?,
    };
    Ok(SyncService::new(icloud, fastmail, repository_service, sync_config, Arc::new(SystemClock)))
}

#[cfg(test)]
mod tests {
    use cg_core::test_support::InMemoryState;

    use super::*;
    use crate::config::test_config;

    #[test]
    fn builds_from_default_urls() {
        let config = test_config("https://contacts.icloud.com", "https://carddav.fastmail.com");
        build_sync_service(&config, InMemoryState::new().repository_service()).unwrap();
    }

    #[test]
    fn bad_url_error_names_variable_without_password() {
        let config = test_config("https://contacts.icloud.com", "not a url");
        let message = format!("{:#}", build_sync_service(&config, InMemoryState::new().repository_service()).err().unwrap());
        assert!(message.contains("CARDIGAN_FASTMAIL_URL"), "{message}");
        assert!(!message.contains("secret-pw"), "password leaked: {message}");
    }
}
