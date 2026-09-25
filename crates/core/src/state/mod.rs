//! Sync state: what the daemon last synced, per contact and per endpoint.
//! These traits are the spec's sync-state port; `cg-database` implements
//! them.

mod contact_state;

#[cfg(test)]
pub(crate) use contact_state::MockContactStateRepository;
pub use contact_state::{ContactState, ContactStateId, ContactStateRepository, NewContactState, SideState};
