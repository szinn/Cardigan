//! The sync engine's pure core: both sides' snapshots plus the stored state
//! in, a plan of operations out. No I/O. CG-8 builds the snapshots and
//! executes the plan.

#[cfg(test)]
mod fixtures;
mod guard;
mod plan;
mod planner;
mod sides;
mod snapshot;

pub use guard::{DELETE_FLOOR, DELETE_PERCENT, MassDeletion, check_deletions};
pub use plan::{Diagnostic, Op, Plan, Resource, SYNC_HASH, SyncedCard, Unsynced, UnsyncedCard};
pub use planner::{PlanInput, Planned, plan};
pub use snapshot::{Entry, FetchLists, Snapshot, fetch_lists};
