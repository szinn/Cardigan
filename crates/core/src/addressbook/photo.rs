//! The `PhotoFetcher` driven port: downloads the bytes behind an iCloud
//! `PHOTO` URI (CG-15 R6). `cg-carddav` implements it. PII: a URI and the
//! bytes never appear in logs or errors.

use crate::{Error, contact::PhotoUri};

/// Driven port for downloading photos.
#[async_trait::async_trait]
#[cfg_attr(test, mockall::automock)]
pub trait PhotoFetcher: Send + Sync {
    /// The bytes behind an iCloud `PHOTO` URI. Errors use `AddressBookError`'s
    /// taxonomy: 401 `Unauthorized`, 429/503 `RateLimited`, 5xx/timeouts
    /// `Transient`, a refused URI or any other status `Permanent`.
    async fn fetch(&self, uri: &PhotoUri) -> Result<Vec<u8>, Error>;
}
