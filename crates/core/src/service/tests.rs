//! `SyncService` against two `InMemoryAddressBook`s and an `InMemoryState`.
//! Cards are synthetic and PII-free.

use super::*;
use crate::{
    contact::{CANONICAL_VERSION, Href, Uid, VCard},
    repository::transaction,
    state::{CardFailureRepository, ContactStateRepository, FailedCard, FailureOp, FailureReason, NewContactState, SideState},
    sync::SyncedCard,
    test_support::{InMemoryAddressBook, InMemoryState, Op as BookOp},
};

const ICLOUD_URL: &str = "https://icloud.test/card/";
const FASTMAIL_URL: &str = "https://fastmail.test/dav/";

struct TestClock(Mutex<DateTime<Utc>>);

impl Clock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}

struct Harness {
    icloud: Arc<InMemoryAddressBook>,
    fastmail: Arc<InMemoryAddressBook>,
    state: Arc<InMemoryState>,
    clock: Arc<TestClock>,
    config: SyncConfig,
    service: SyncService,
}

fn book(url: &str, host: &str) -> Arc<InMemoryAddressBook> {
    Arc::new(InMemoryAddressBook::new(Collection {
        addressbook_url: url.to_owned(),
        discovered_host: host.to_owned(),
        supports_sync_collection: true,
    }))
}

/// A PII-free test card; `extra` lines each end in `\r\n`.
fn vcard(uid: &str, name: &str, extra: &str) -> String {
    format!("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:{name}\r\n{extra}END:VCARD\r\n")
}

fn href(path: &str) -> Href {
    Href::from(path)
}

impl Harness {
    fn new(winner: Side) -> Self {
        let icloud = book(ICLOUD_URL, "icloud.test");
        let fastmail = book(FASTMAIL_URL, "fastmail.test");
        let state = InMemoryState::new();
        let clock = Arc::new(TestClock(Mutex::new("2026-09-27T00:00:00Z".parse().unwrap())));
        let config = SyncConfig {
            winner,
            poll_interval: TimeDelta::seconds(60),
        };
        let service = SyncService::new(icloud.clone(), fastmail.clone(), state.repository_service(), config, clock.clone());
        Self {
            icloud,
            fastmail,
            state,
            clock,
            config,
            service,
        }
    }

    fn book(&self, side: Side) -> &InMemoryAddressBook {
        match side {
            Side::ICloud => &self.icloud,
            Side::Fastmail => &self.fastmail,
        }
    }

    fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    fn advance(&self, by: TimeDelta) {
        *self.clock.0.lock().unwrap() += by;
    }

    fn writes(&self) -> usize {
        self.icloud.writes().len() + self.fastmail.writes().len()
    }

    async fn run(&self, mode: CycleMode, reset: bool) -> Result<CycleOutcome, Error> {
        self.service.run_cycle(CycleRequest { mode, reset }).await
    }

    async fn sync(&self) -> CycleOutcome {
        self.run(CycleMode::Sync, false).await.expect("cycle succeeds")
    }

    async fn dry_run(&self) -> (CyclePlan, Option<MassDeletion>) {
        match self.run(CycleMode::DryRun, false).await.expect("dry run succeeds") {
            CycleOutcome::DryRun { cycle, blocked } => (cycle, blocked),
            other @ CycleOutcome::Blocked(_) => panic!("expected a dry run, got {other:?}"),
        }
    }

    /// `uid` on both sides (`/card/{uid}.vcf`, `/dav/{uid}.vcf`) with its
    /// state row, as after a completed sync.
    async fn seed_synced(&self, uid: &str, name: &str) {
        let body = vcard(uid, name, "");
        let (icloud_href, fastmail_href) = (href(&format!("/card/{uid}.vcf")), href(&format!("/dav/{uid}.vcf")));
        let icloud_etag = self.icloud.external_put(icloud_href.clone(), body.clone());
        let fastmail_etag = self.fastmail.external_put(fastmail_href.clone(), body.clone());
        let synced = SyncedCard::recorded(&VCard::parse(body).unwrap());
        let now = self.now();
        let new = NewContactState {
            uid: Uid::from(uid),
            icloud: SideState {
                href: icloud_href,
                etag: icloud_etag,
                last_seen_at: now,
            },
            fastmail: SideState {
                href: fastmail_href,
                etag: fastmail_etag,
                last_seen_at: now,
            },
            content_hash: synced.content_hash,
            hash_version: CANONICAL_VERSION,
            photo_stripped: false,
            last_synced_vcard: synced.card,
            last_synced_at: now,
        };
        let store = self.state.clone();
        transaction(&*self.state, |tx| Box::pin(async move { ContactStateRepository::add(&*store, tx, new).await }))
            .await
            .unwrap();
    }

    /// Records a read failure for the card at `path` on `side`, at its current
    /// ETag, due one poll interval from now.
    async fn record_failure(&self, side: Side, path: &str) {
        let failed = FailedCard {
            side,
            href: href(path),
            uid: None,
            op: FailureOp::Read,
            etag: self.book(side).card(&href(path)).map(|(etag, _)| etag),
            reason: FailureReason::InvalidCard,
        };
        let (store, now, policy) = (self.state.clone(), self.now(), self.config.backoff());
        transaction(&*self.state, |tx| {
            Box::pin(async move { CardFailureRepository::record_failure(&*store, tx, failed, now, &policy).await })
        })
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn dry_run_plans_the_baseline_and_writes_nothing() {
    let h = Harness::new(Side::ICloud);
    h.icloud.external_put("/card/jane.vcf", vcard("u1", "Jane Doe", ""));
    h.fastmail.external_put("/dav/bob.vcf", vcard("u2", "Bob Roe", ""));

    let (cycle, blocked) = h.dry_run().await;

    let plan = cycle.plan.to_string();
    assert_eq!(cycle.plan.ops.len(), 2, "{plan}");
    assert!(plan.contains("create fastmail uid=u1 from=/card/jane.vcf"), "{plan}");
    assert!(plan.contains("create icloud uid=u2 from=/dav/bob.vcf"), "{plan}");
    assert_eq!(blocked, None);
    assert_eq!(h.writes(), 0);
    assert!(h.state.endpoints().is_empty(), "dry-run records no discovery");
}

#[tokio::test]
async fn listing_uses_propfind_without_sync_collection() {
    let h = Harness::new(Side::ICloud);
    h.fastmail.set_supports_sync_collection(false);
    h.fastmail.external_put("/dav/bob.vcf", vcard("u2", "Bob Roe", ""));

    let (cycle, _) = h.dry_run().await;

    assert_eq!(cycle.plan.to_string(), "create icloud uid=u2 from=/dav/bob.vcf\n");
}

#[tokio::test]
async fn a_failing_card_is_held_until_due_or_edited() {
    let h = Harness::new(Side::ICloud);
    h.icloud.external_put("/card/jane.vcf", vcard("u1", "Jane Doe", ""));
    h.record_failure(Side::ICloud, "/card/jane.vcf").await;

    assert!(h.dry_run().await.0.plan.ops.is_empty(), "held until due");

    h.advance(TimeDelta::seconds(61));
    assert_eq!(h.dry_run().await.0.plan.to_string(), "create fastmail uid=u1 from=/card/jane.vcf\n");

    h.record_failure(Side::ICloud, "/card/jane.vcf").await;
    assert!(h.dry_run().await.0.plan.ops.is_empty(), "held again");
    h.icloud.external_put("/card/jane.vcf", vcard("u1", "Jane Doe", "NOTE:edited\r\n"));
    assert_eq!(h.dry_run().await.0.plan.ops.len(), 1, "an edit retries at once");
}

#[tokio::test]
async fn mass_deletion_is_flagged_and_blocks_a_sync() {
    let h = Harness::new(Side::ICloud);
    for i in 0..11 {
        h.seed_synced(&format!("u{i}"), &format!("Person {i}")).await;
        h.icloud.external_delete(&href(&format!("/card/u{i}.vcf")));
    }

    let (_, blocked) = h.dry_run().await;
    assert_eq!(blocked.map(|b| (b.side, b.deletes)), Some((Side::Fastmail, 11)));

    assert!(matches!(h.sync().await, CycleOutcome::Blocked(_)));
    assert_eq!(h.writes(), 0);
    assert_eq!(h.state.contacts().len(), 11);
}

#[tokio::test]
#[allow(
    clippy::assert_is_empty,
    reason = "asserting on ops.is_empty() reads clearer than assert_eq! against an empty array literal"
)]
async fn dry_run_reset_plans_against_an_empty_store_and_clears_nothing() {
    let h = Harness::new(Side::ICloud);
    h.seed_synced("u1", "Jane Doe").await;
    assert!(h.dry_run().await.0.plan.ops.is_empty());

    let CycleOutcome::DryRun { cycle, .. } = h.run(CycleMode::DryRun, true).await.unwrap() else {
        panic!("expected a dry run");
    };

    assert!(cycle.plan.to_string().starts_with("adopt uid=u1 "), "{}", cycle.plan);
    assert_eq!(h.state.contacts().len(), 1);
}

#[tokio::test]
async fn a_failed_fetch_aborts_before_planning() {
    let h = Harness::new(Side::ICloud);
    h.icloud.external_put("/card/jane.vcf", vcard("u1", "Jane Doe", ""));
    h.icloud.fail_next(BookOp::Multiget, AddressBookError::Transient("timeout".into()));

    let error = h.run(CycleMode::DryRun, false).await.unwrap_err();

    assert!(matches!(error, Error::AddressBook(AddressBookError::Transient(_))), "{error:?}");
}

#[tokio::test]
async fn a_moved_collection_is_rediscovered_after_a_listing_error() {
    let h = Harness::new(Side::ICloud);
    h.icloud.fail_next(BookOp::ChangesSince, AddressBookError::Permanent("404 Not Found".into()));
    h.run(CycleMode::Sync, false).await.unwrap_err();

    // Discovery runs again: its injected failure is what the next cycle hits.
    h.icloud.fail_next(BookOp::Discover, AddressBookError::Unauthorized);
    let error = h.run(CycleMode::Sync, false).await.unwrap_err();

    assert!(matches!(error, Error::AddressBook(AddressBookError::Unauthorized)), "{error:?}");
}
