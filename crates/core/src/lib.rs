pub mod addressbook;
pub mod contact;
pub mod error;
pub mod repository;
pub mod state;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

use std::sync::Arc;

use derive_builder::Builder;
pub use error::{AddressBookError, Error, ErrorKind, RepositoryError};

use crate::repository::RepositoryService;

/// All externally-provided adapter implementations required by `CoreServices`.
///
/// Use `ExternalServicesBuilder` to construct — all fields are required and
/// `.build()` returns an error if any are missing.
#[derive(Builder)]
#[builder(pattern = "owned")]
pub struct ExternalServices {
    pub(crate) repository_service: Arc<RepositoryService>,
}

pub struct CoreServices {}

impl CoreServices {
    pub(crate) fn new(external: ExternalServices) -> Self {
        let ExternalServices { repository_service } = external;

        Self {}
    }
}

pub fn create_services(external: ExternalServices) -> Result<Arc<CoreServices>, Error> {
    Ok(Arc::new(CoreServices::new(external)))
}
