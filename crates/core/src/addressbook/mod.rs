//! The `AddressBook` driven port: how the sync engine talks to one side's
//! CardDAV address book. `cg-carddav` implements it.

mod model;
mod port;

pub use model::{ChangeSet, Changes, Collection, FetchedCard, MultigetResult, Precondition, SyncToken};
pub use port::AddressBook;
#[cfg(any(test, feature = "test-support"))]
pub use port::MockAddressBook;
