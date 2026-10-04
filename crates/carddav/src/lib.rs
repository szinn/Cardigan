//! CardDAV implementation of cg-core's `AddressBook` port: discovery,
//! `sync-collection`, `addressbook-multiget` and conditional writes, over
//! reqwest with rustls. Card bodies are full contact data (PII): this crate
//! never logs them.
mod adapter;
mod client;
mod config;
mod discovery;
mod error;
mod href;
mod multiget;
mod photo;
mod sync;
#[cfg(test)]
mod test_util;
mod write;
mod xml;

pub use adapter::CardDavAddressBook;
pub use config::{CardDavConfig, DEFAULT_CONNECT_TIMEOUT, DEFAULT_MULTIGET_BATCH, DEFAULT_REQUEST_TIMEOUT, ProviderQuirks};
pub use photo::IcloudPhotoFetcher;
