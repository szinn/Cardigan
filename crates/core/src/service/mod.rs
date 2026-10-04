//! The sync cycle: list both address books, plan with `sync::plan_cycle`,
//! then apply the plan one contact at a time. This is the only part of
//! cg-core that does I/O; `sync` stays pure.

mod apply;
mod executor;
mod href;
mod listing;
mod replay;
mod summary;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex};

use chrono::{DateTime, TimeDelta, Utc};

use self::listing::Stored;
pub use self::summary::{CycleSummary, DirectionCounts};
use crate::{
    AddressBookError, Error,
    addressbook::{AddressBook, Collection, PhotoFetcher},
    contact::{ConflictWinner, Side},
    repository::RepositoryService,
    state::BackoffPolicy,
    sync::{CyclePlan, MassDeletion, PlanInput, check_deletions, plan_cycle},
    with_transaction,
};

/// Longest wait before a failing card is retried.
pub const BACKOFF_CAP: TimeDelta = TimeDelta::hours(24);
/// A card that has failed this many times in a row is listed in every cycle
/// summary.
pub const PERSISTENT_ATTEMPTS: u32 = 3;

const POISONED: &str = "SyncService collections lock poisoned";

/// The time source; tests substitute a settable clock.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncConfig {
    /// `CARDIGAN_CONFLICT_WINNER`.
    pub winner: ConflictWinner,
    /// The daemon's poll interval, which is also the first retry delay for a
    /// failing card.
    pub poll_interval: TimeDelta,
}

impl SyncConfig {
    /// Base = the poll interval (at most the cap), cap = `BACKOFF_CAP`.
    #[must_use]
    pub fn backoff(&self) -> BackoffPolicy {
        BackoffPolicy {
            base: self.poll_interval.min(BACKOFF_CAP),
            cap: BACKOFF_CAP,
        }
    }
}

/// Whether a cycle may write to the servers and the state store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleMode {
    Sync,
    /// Plan only: no server writes, no state writes (not even discovery).
    DryRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CycleRequest {
    pub mode: CycleMode,
    /// Sync: clear state (keeping conflicts) and re-baseline. Dry-run: plan
    /// as if the store were empty, clearing nothing.
    pub reset: bool,
}

/// What a cycle did.
#[derive(Debug)]
#[allow(clippy::large_enum_variant, reason = "DryRun is the common case; boxing CyclePlan would only add an indirection")]
pub enum CycleOutcome {
    /// Nothing changed on either side and no failing card was due: nothing
    /// was listed, planned or written.
    Idle,
    /// Dry-run: the plan a sync would apply. `blocked` is set when the
    /// mass-deletion guard would stop it.
    DryRun { cycle: CyclePlan, blocked: Option<MassDeletion> },
    /// The mass-deletion guard stopped the cycle before any write. CG-9
    /// decides whether to offer an override.
    Blocked(MassDeletion),
    /// A sync cycle ran; what it did.
    Applied(CycleSummary),
}

/// Both sides' address book collections, as discovered.
#[derive(Debug, Clone)]
struct Collections {
    icloud: Collection,
    fastmail: Collection,
}

impl Collections {
    fn get(&self, side: Side) -> &Collection {
        match side {
            Side::ICloud => &self.icloud,
            Side::Fastmail => &self.fastmail,
        }
    }
}

/// Runs sync cycles between iCloud and Fastmail. One cycle at a time: the
/// daemon loop never overlaps them.
pub struct SyncService {
    icloud: Arc<dyn AddressBook>,
    fastmail: Arc<dyn AddressBook>,
    /// Downloads iCloud photos before planning (CG-15 R6).
    #[allow(dead_code, reason = "Task 5 wires the downloads in")]
    photos: Arc<dyn PhotoFetcher>,
    repository_service: Arc<RepositoryService>,
    winner: ConflictWinner,
    backoff: BackoffPolicy,
    clock: Arc<dyn Clock>,
    /// Both collections once discovered (and recorded) by a sync cycle.
    /// Cleared to force re-discovery.
    collections: Mutex<Option<Collections>>,
}

impl SyncService {
    #[must_use]
    pub fn new(
        icloud: Arc<dyn AddressBook>,
        fastmail: Arc<dyn AddressBook>,
        photos: Arc<dyn PhotoFetcher>,
        repository_service: Arc<RepositoryService>,
        config: SyncConfig,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            icloud,
            fastmail,
            photos,
            repository_service,
            winner: config.winner,
            backoff: config.backoff(),
            clock,
            collections: Mutex::new(None),
        }
    }

    /// Runs one cycle. An `Err` is cycle-fatal (listing failed, rate limited,
    /// unauthorized, transient, or the state store is unreachable); ops that
    /// completed before it stay applied, and the caller retries next
    /// interval.
    pub async fn run_cycle(&self, request: CycleRequest) -> Result<CycleOutcome, Error> {
        let now = self.clock.now();
        let dry_run = request.mode == CycleMode::DryRun;
        if request.reset && !dry_run {
            self.reset().await?;
        }
        let collections = self.collections(!dry_run).await?;
        // `dry-run --reset` previews a re-baseline: plan as if the store were
        // empty.
        let stored = if request.reset && dry_run { Stored::default() } else { self.load().await? };
        // An interrupted Recreate must be finished before pairing sees an
        // iCloud-only card and copies it (CG-16). Dry-run never replays.
        let replayed = if !dry_run && !stored.pending.is_empty() {
            self.replay(&stored.pending, &collections).await?
        } else {
            Vec::new()
        };
        if !dry_run && self.idle(&stored, &collections, now).await? {
            return Ok(CycleOutcome::Idle);
        }
        let listed = self.list(&collections).await?;
        let built = self.build(&listed, &stored, now).await?;
        let cycle = plan_cycle(&PlanInput {
            icloud: &built.icloud,
            fastmail: &built.fastmail,
            state: &stored.contacts,
            winner: self.winner,
            replayed: &replayed,
        });
        let blocked = check_deletions(&cycle.plan, stored.contacts.len()).err();
        if dry_run {
            return Ok(CycleOutcome::DryRun { cycle, blocked });
        }
        if let Some(blocked) = blocked {
            tracing::warn!("{blocked}; wrote nothing this cycle");
            return Ok(CycleOutcome::Blocked(blocked));
        }
        let summary = self.apply(&cycle, &stored, &built, &listed, &collections, now).await?;
        Ok(CycleOutcome::Applied(summary))
    }

    fn book(&self, side: Side) -> &dyn AddressBook {
        match side {
            Side::ICloud => &*self.icloud,
            Side::Fastmail => &*self.fastmail,
        }
    }

    /// Both collections: cached, or discovered now. With `record`, a fresh
    /// discovery is written to the endpoints table and cached; dry-run does
    /// neither.
    async fn collections(&self, record: bool) -> Result<Collections, Error> {
        if let Some(known) = self.collections.lock().expect(POISONED).clone() {
            return Ok(known);
        }
        let found = Collections {
            icloud: self.icloud.discover().await?,
            fastmail: self.fastmail.discover().await?,
        };
        if record {
            let (icloud, fastmail) = (found.icloud.clone(), found.fastmail.clone());
            with_transaction!(self, endpoint_repository, |tx| {
                endpoint_repository
                    .upsert_discovery(tx, Side::ICloud, &icloud.addressbook_url, &icloud.discovered_host)
                    .await?;
                endpoint_repository
                    .upsert_discovery(tx, Side::Fastmail, &fastmail.addressbook_url, &fastmail.discovered_host)
                    .await?;
                Ok(())
            })?;
            *self.collections.lock().expect(POISONED) = Some(found.clone());
        }
        Ok(found)
    }

    fn forget_collections(&self) {
        *self.collections.lock().expect(POISONED) = None;
    }

    /// `--reset`: clears contacts, endpoints, card failures and baseline
    /// skips, keeps the conflict history, and forces re-discovery so the
    /// endpoints are recorded again.
    async fn reset(&self) -> Result<(), Error> {
        let contacts = with_transaction!(
            self,
            contact_state_repository,
            endpoint_repository,
            card_failure_repository,
            baseline_skip_repository,
            |tx| {
                let contacts = contact_state_repository.delete_all(tx).await?;
                endpoint_repository.delete_all(tx).await?;
                card_failure_repository.delete_all(tx).await?;
                baseline_skip_repository.delete_all(tx).await?;
                Ok(contacts)
            }
        )?;
        self.forget_collections();
        tracing::info!(contacts, "state reset; re-baselining (conflict history kept)");
        Ok(())
    }

    /// A 404/410 or a transport failure while listing may mean the
    /// collection moved (iCloud's numbered host): discover again next cycle.
    fn after_listing_error(&self, error: &Error) {
        if matches!(error, Error::AddressBook(AddressBookError::Permanent(_) | AddressBookError::Transient(_))) {
            self.forget_collections();
        }
    }
}
