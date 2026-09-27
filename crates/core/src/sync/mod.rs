//! The sync engine's pure core: both sides' snapshots plus the stored state
//! in, a plan of operations out. No I/O. CG-8 builds the snapshots and
//! executes the plan.

#[cfg(test)]
mod fixtures;
mod plan;
mod sides;
mod snapshot;

pub use plan::{Diagnostic, Op, Plan, Resource, SYNC_HASH, SyncedCard, Unsynced, UnsyncedCard};
pub use snapshot::{Entry, FetchLists, Snapshot, fetch_lists};
