//! The sync engine's pure core: both sides' snapshots plus the stored state
//! in, a plan of operations out. No I/O. CG-8 builds the snapshots and
//! executes the plan.

mod cycle;
#[cfg(test)]
mod fixtures;
mod guard;
mod pairing;
mod photo;
mod plan;
mod planner;
mod relink;
mod report;
mod sides;
mod snapshot;

pub use cycle::{CyclePlan, plan_cycle};
pub use guard::{DELETE_FLOOR, DELETE_PERCENT, MassDeletion, check_deletions, hold_cross_deletes};
pub use pairing::{KnownCard, KnownCards, Paired, Skip, SkipReason, pair};
pub use photo::FetchedPhotos;
pub use plan::{Diagnostic, Op, PairPass, Plan, RecreateConflict, Resource, SYNC_HASH, SyncedCard, Unsynced, UnsyncedCard};
pub use planner::{PlanInput, Planned, plan};
pub use report::{BaselineReport, ReportCopy, ReportDuplicate, ReportPair, synced_duplicates};
pub use snapshot::{Entry, FetchLists, Snapshot, fetch_lists};
