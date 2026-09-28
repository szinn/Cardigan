//! Sync state: what the daemon last synced, per contact and per endpoint,
//! plus the conflict history, failing cards, ambiguous baseline cards and the
//! Recreate journal. These traits are the spec's sync-state port;
//! `cg-database` implements them.

mod baseline_skip;
mod card_failure;
mod conflict;
mod contact_state;
mod endpoint;
mod pending_recreate;

pub use baseline_skip::{BaselineSkip, BaselineSkipId, BaselineSkipRepository, NewBaselineSkip};
pub use card_failure::{BackoffPolicy, CardFailure, CardFailureId, CardFailureRepository, FailedCard, FailureOp, FailureReason};
pub use conflict::{Conflict, ConflictId, ConflictOrigin, ConflictRepository, NewConflict};
pub use contact_state::{ContactState, ContactStateId, ContactStateRepository, NewContactState, SideState};
pub use endpoint::{Endpoint, EndpointRepository};
pub use pending_recreate::{NewPendingRecreate, PendingRecreate, PendingRecreateId, PendingRecreateRepository};
