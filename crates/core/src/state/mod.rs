//! Sync state: what the daemon last synced, per contact and per endpoint,
//! plus the conflict history. These traits are the spec's sync-state port;
//! `cg-database` implements them.

mod conflict;
mod contact_state;
mod endpoint;

#[cfg(test)]
pub(crate) use conflict::MockConflictRepository;
pub use conflict::{Conflict, ConflictId, ConflictOrigin, ConflictRepository, NewConflict};
#[cfg(test)]
pub(crate) use contact_state::MockContactStateRepository;
pub use contact_state::{ContactState, ContactStateId, ContactStateRepository, NewContactState, SideState};
#[cfg(test)]
pub(crate) use endpoint::MockEndpointRepository;
pub use endpoint::{Endpoint, EndpointRepository};
