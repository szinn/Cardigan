//! `SyncService` against two `InMemoryAddressBook`s and an `InMemoryState`.
//! Cards are synthetic and PII-free.

use super::{href::mint_href, *};
use crate::{
    addressbook::Precondition,
    contact::{CANONICAL_VERSION, Href, Uid, VCard},
    repository::transaction,
    state::{CardFailureRepository, ConflictOrigin, ContactStateRepository, FailedCard, FailureOp, FailureReason, NewContactState, SideState},
    sync::SyncedCard,
    test_support::{InMemoryAddressBook, InMemoryState, Op as BookOp, Write},
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
            other => panic!("expected a dry run, got {other:?}"),
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

    async fn applied(&self) -> CycleSummary {
        match self.sync().await {
            CycleOutcome::Applied(summary) => summary,
            other => panic!("expected an applied cycle, got {other:?}"),
        }
    }

    fn token(&self, side: Side) -> Option<String> {
        self.state
            .endpoints()
            .into_iter()
            .find(|endpoint| endpoint.side == side)
            .and_then(|endpoint| endpoint.sync_token)
    }

    /// Syncs until a cycle is idle (at most three cycles).
    async fn settle(&self) {
        for _ in 0..3 {
            if matches!(self.sync().await, CycleOutcome::Idle) {
                return;
            }
        }
        panic!("did not settle");
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

fn minted(url: &str, uid: &str) -> Href {
    mint_href(url, &Uid::from(uid))
}

#[tokio::test]
async fn baseline_copies_each_unique_card_to_the_other_side() {
    let h = Harness::new(Side::ICloud);
    h.icloud.external_put("/card/jane.vcf", vcard("u1", "Jane Doe", ""));
    h.fastmail.external_put("/dav/bob.vcf", vcard("u2", "Bob Roe", ""));

    let summary = h.applied().await;

    assert_eq!((summary.to_fastmail.added, summary.to_icloud.added), (1, 1));
    let to_fastmail = minted(FASTMAIL_URL, "u1");
    assert_eq!(
        h.fastmail.writes(),
        [Write::Put {
            href: to_fastmail.clone(),
            precondition: Precondition::IfNoneMatch,
            body: vcard("u1", "Jane Doe", "").into_bytes(),
        }]
    );
    let rows = h.state.contacts();
    let uids: Vec<&str> = rows.iter().map(|row| row.uid.as_str()).collect();
    assert_eq!(uids, ["u1", "u2"]);
    assert_eq!(rows[0].fastmail.href, to_fastmail);
    assert_eq!(Some(rows[0].fastmail.etag.clone()), h.fastmail.card(&to_fastmail).map(|(etag, _)| etag));
    assert_eq!(rows[1].icloud.href, minted(ICLOUD_URL, "u2"));
    assert_eq!(h.state.endpoints().len(), 2, "discovery recorded");
}

#[tokio::test]
async fn own_writes_are_not_synced_back() {
    let h = Harness::new(Side::ICloud);
    h.icloud.external_put("/card/jane.vcf", vcard("u1", "Jane Doe", ""));
    h.applied().await;
    let writes = h.writes();

    let summary = h.applied().await;

    assert_eq!(h.writes(), writes);
    assert_eq!(summary, CycleSummary::default());
}

#[tokio::test]
async fn an_edit_is_pushed_with_if_match() {
    let h = Harness::new(Side::ICloud);
    h.icloud.external_put("/card/jane.vcf", vcard("u1", "Jane Doe", ""));
    h.applied().await;
    h.icloud.external_put("/card/jane.vcf", vcard("u1", "Jane Doe", "NOTE:new\r\n"));

    let summary = h.applied().await;

    assert_eq!(summary.to_fastmail.updated, 1);
    let target = minted(FASTMAIL_URL, "u1");
    let last = h.fastmail.writes().pop().unwrap();
    assert!(
        matches!(&last, Write::Put { href, precondition: Precondition::IfMatch(_), .. } if *href == target),
        "{last:?}"
    );
    assert_eq!(h.fastmail.card(&target).unwrap().1, vcard("u1", "Jane Doe", "NOTE:new\r\n").into_bytes());
}

#[tokio::test]
#[allow(
    clippy::assert_is_empty,
    reason = "asserting on is_empty() reads clearer than assert_eq! against an empty array literal"
)]
async fn a_deletion_is_propagated_and_a_double_deletion_forgotten() {
    let h = Harness::new(Side::ICloud);
    h.icloud.external_put("/card/jane.vcf", vcard("u1", "Jane Doe", ""));
    h.icloud.external_put("/card/bob.vcf", vcard("u2", "Bob Roe", ""));
    h.applied().await;

    h.icloud.external_delete(&href("/card/jane.vcf"));
    h.icloud.external_delete(&href("/card/bob.vcf"));
    h.fastmail.external_delete(&minted(FASTMAIL_URL, "u2"));
    let summary = h.applied().await;

    assert_eq!((summary.to_fastmail.removed, summary.forgotten), (1, 1));
    assert!(
        matches!(h.fastmail.writes().pop(), Some(Write::Delete { href, if_match: Some(_) }) if href == minted(FASTMAIL_URL, "u1")),
        "the delete is guarded by If-Match"
    );
    assert!(h.state.contacts().is_empty());
}

#[tokio::test]
async fn a_missing_put_etag_is_fetched() {
    let h = Harness::new(Side::ICloud);
    h.fastmail.omit_etag_on_put(true);
    h.icloud.external_put("/card/jane.vcf", vcard("u1", "Jane Doe", ""));

    h.applied().await;

    let target = minted(FASTMAIL_URL, "u1");
    assert_eq!(
        Some(h.state.contacts()[0].fastmail.etag.clone()),
        h.fastmail.card(&target).map(|(etag, _)| etag)
    );
}

#[tokio::test]
async fn a_rejected_write_is_held_and_the_cycle_continues() {
    let h = Harness::new(Side::ICloud);
    h.icloud.external_put("/card/a.vcf", vcard("u1", "Ann Lee", ""));
    h.icloud.external_put("/card/b.vcf", vcard("u2", "Bo Ray", ""));
    h.fastmail.fail_next(BookOp::Put, AddressBookError::Permanent("400 Bad Request".into()));

    let summary = h.applied().await;

    assert_eq!((summary.to_fastmail.added, summary.to_fastmail.errors), (1, 1));
    let failures = h.state.failures();
    assert_eq!(failures.len(), 1);
    let failure = &failures[0];
    assert_eq!(
        (failure.side, failure.op, failure.reason, failure.attempts),
        (Side::ICloud, FailureOp::Create, FailureReason::Rejected, 1)
    );

    let writes = h.fastmail.writes().len();
    h.applied().await;
    assert_eq!(h.fastmail.writes().len(), writes, "held until its backoff elapses");

    h.advance(TimeDelta::seconds(61));
    assert_eq!(h.applied().await.to_fastmail.added, 1);
    assert!(h.state.failures().is_empty(), "cleared by the successful create");
    assert_eq!(h.state.contacts().len(), 2);
}

#[tokio::test]
async fn rate_limiting_aborts_the_cycle() {
    let h = Harness::new(Side::ICloud);
    h.icloud.external_put("/card/a.vcf", vcard("u1", "Ann Lee", ""));
    h.icloud.external_put("/card/b.vcf", vcard("u2", "Bo Ray", ""));
    h.fastmail.fail_next(
        BookOp::Put,
        AddressBookError::RateLimited {
            retry_after: Some(std::time::Duration::from_secs(30)),
        },
    );

    let error = h.run(CycleMode::Sync, false).await.unwrap_err();

    assert!(matches!(error, Error::AddressBook(AddressBookError::RateLimited { .. })), "{error:?}");
    assert_eq!(h.fastmail.writes().len(), 1, "nothing written after the rate limit");
    assert!(h.state.failures().is_empty(), "a create is not held on abort");
    assert_eq!(h.applied().await.to_fastmail.added, 2);
}

#[tokio::test]
async fn an_unreadable_card_is_recorded_as_a_read_failure() {
    let h = Harness::new(Side::ICloud);
    h.icloud
        .external_put("/card/bad.vcf", "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:No Uid\r\nEND:VCARD\r\n");

    let summary = h.applied().await;

    assert_eq!(summary.to_fastmail.errors, 1);
    let failure = &h.state.failures()[0];
    assert_eq!(
        (failure.op, failure.reason, failure.uid.clone()),
        (FailureOp::Read, FailureReason::MissingUid, None)
    );
}

#[tokio::test]
#[allow(
    clippy::assert_is_empty,
    reason = "asserting on is_empty() reads clearer than assert_eq! against an empty array literal"
)]
async fn a_state_write_failure_after_a_put_converges_without_a_duplicate() {
    let h = Harness::new(Side::ICloud);
    h.applied().await; // discovery recorded, so the next write is the op's
    h.icloud.external_put("/card/jane.vcf", vcard("u1", "Jane Doe", ""));
    h.state.fail_next_write();

    let summary = h.applied().await;

    assert_eq!(summary.to_fastmail.errors, 1);
    assert!(h.state.contacts().is_empty());
    assert_eq!(h.state.failures().len(), 2, "the source and the card already written are held");

    h.advance(TimeDelta::seconds(61));
    let summary = h.applied().await;

    assert_eq!(summary.adopted, 1);
    assert_eq!(h.state.contacts().len(), 1);
    assert!(h.icloud.writes().is_empty(), "never copied back to iCloud");
    assert_eq!(h.fastmail.writes().len(), 1);
    assert!(h.state.failures().is_empty());
}

#[tokio::test]
async fn discovery_is_recorded_once_per_process() {
    let h = Harness::new(Side::ICloud);
    h.applied().await;
    h.icloud.fail_next(BookOp::Discover, AddressBookError::Unauthorized);

    h.sync().await;

    assert_eq!(h.state.endpoints().len(), 2);
}

#[tokio::test]
async fn a_fatal_error_during_an_update_holds_the_edited_sides_source() {
    let h = Harness::new(Side::ICloud);
    h.icloud.external_put("/card/jane.vcf", vcard("u1", "Jane Doe", ""));
    h.applied().await;
    h.icloud.external_put("/card/jane.vcf", vcard("u1", "Jane Doe", "NOTE:edited\r\n"));
    h.fastmail.fail_next(BookOp::Put, AddressBookError::Transient("503".into()));

    let error = h.run(CycleMode::Sync, false).await.unwrap_err();

    assert!(matches!(error, Error::AddressBook(AddressBookError::Transient(_))), "{error:?}");
    let failures = h.state.failures();
    assert_eq!(failures.len(), 1, "an Update is held on a fatal abort (holds_on_abort)");
    let failure = &failures[0];
    assert_eq!(
        (failure.side, failure.op, failure.reason),
        (Side::ICloud, FailureOp::Update, FailureReason::Transient)
    );
}

#[tokio::test]
#[allow(
    clippy::assert_is_empty,
    reason = "asserting on is_empty() reads clearer than assert_eq! against an empty array literal"
)]
async fn held_cards_of_one_failed_op_release_together_not_by_each_cards_own_backoff() {
    let h = Harness::new(Side::ICloud);
    h.icloud.external_put("/card/jane.vcf", vcard("u1", "Jane Doe", ""));

    // Attempt 1: rejected outright. attempts=1, delay=60s.
    h.fastmail.fail_next(BookOp::Put, AddressBookError::Permanent("400".into()));
    let summary = h.applied().await;
    assert_eq!(summary.to_fastmail.errors, 1);
    h.advance(TimeDelta::seconds(61));

    // Attempt 2: rejected again (same card, same ETag: attempts accrue).
    // attempts=2, delay=120s.
    h.fastmail.fail_next(BookOp::Put, AddressBookError::Permanent("400".into()));
    let summary = h.applied().await;
    assert_eq!(summary.to_fastmail.errors, 1);
    h.advance(TimeDelta::seconds(121));

    // Attempt 3: the PUT lands, but the state write fails: the card is now
    // on Fastmail, but not yet in state. The iCloud source's attempts
    // continue (attempts=3, delay=240s); the newly-written Fastmail card is
    // its own first failure (attempts=1, delay=60s).
    h.state.fail_next_write();
    let summary = h.applied().await;
    assert_eq!(summary.to_fastmail.errors, 1);
    assert_eq!(h.state.failures().len(), 2, "the icloud source and the fastmail card the PUT actually wrote");
    let fastmail_writes = h.fastmail.writes().len();

    // The written card's own backoff (60s) elapses, but the source's
    // (240s) has not (I1): the group must stay held together, or the
    // written card would surface as unique to Fastmail and get copied
    // back to iCloud.
    h.advance(TimeDelta::seconds(61));
    let after_short_backoff = h.applied().await;
    assert_eq!(
        CycleSummary {
            persistent_failures: Vec::new(),
            ..after_short_backoff.clone()
        },
        CycleSummary::default(),
        "no copy-back while the group is still held"
    );
    assert_eq!(
        after_short_backoff.persistent_failures.len(),
        1,
        "the icloud source has now reached PERSISTENT_ATTEMPTS and is reported every cycle until it clears"
    );
    assert!(h.icloud.writes().is_empty(), "never copied back to iCloud");
    assert_eq!(h.fastmail.writes().len(), fastmail_writes, "no retry either, until the whole group is due");

    // Once every card in the group is due, it converges on a single state
    // row from the two matching cards, with no duplicate write to either
    // side.
    h.advance(TimeDelta::seconds(200));
    let converged = h.applied().await;
    assert_eq!(converged.adopted, 1);
    assert_eq!(h.state.contacts().len(), 1);
    assert!(h.icloud.writes().is_empty(), "never copied back to iCloud");
    assert_eq!(h.fastmail.writes().len(), fastmail_writes);
    assert!(h.state.failures().is_empty());
}

#[tokio::test]
#[allow(
    clippy::assert_is_empty,
    reason = "asserting on is_empty() reads clearer than assert_eq! against an empty array literal"
)]
async fn a_failed_etag_fetch_after_a_put_still_holds_the_written_card() {
    let h = Harness::new(Side::ICloud);
    h.icloud.external_put("/card/jane.vcf", vcard("u1", "Jane Doe", ""));
    h.fastmail.omit_etag_on_put(true);
    h.fastmail.fail_next(BookOp::Multiget, AddressBookError::Permanent("500".into()));

    let summary = h.applied().await;

    assert_eq!(summary.to_fastmail.errors, 1);
    let failures = h.state.failures();
    assert_eq!(failures.len(), 2, "the icloud source and the fastmail card the PUT actually wrote (I2)");
    let fastmail_failure = failures
        .iter()
        .find(|failure| failure.side == Side::Fastmail)
        .expect("the written card is held");
    assert_eq!(fastmail_failure.etag, None, "the ETag could not be recovered");
    assert!(h.state.contacts().is_empty());

    let writes = h.fastmail.writes().len();
    let next = h.applied().await;
    assert_eq!(next, CycleSummary::default(), "held until due; no copy-back");
    assert_eq!(h.fastmail.writes().len(), writes);
}

const PHOTO: &str = "PHOTO;ENCODING=b;TYPE=JPEG:QUJD\r\n";

#[tokio::test]
async fn a_sync_conflict_keeps_both_versions_and_pushes_the_winner() {
    let h = Harness::new(Side::ICloud);
    h.icloud.external_put("/card/jane.vcf", vcard("u1", "Jane Doe", ""));
    h.applied().await;
    let target = minted(FASTMAIL_URL, "u1");
    h.icloud.external_put("/card/jane.vcf", vcard("u1", "Jane Doe", "NOTE:icloud\r\n"));
    h.fastmail.external_put(target.clone(), vcard("u1", "Jane Doe", "NOTE:fastmail\r\n"));

    let summary = h.applied().await;

    assert_eq!(summary.to_fastmail.conflicts, 1);
    let conflicts = h.state.conflicts();
    assert_eq!(conflicts.len(), 1);
    assert_eq!((conflicts[0].origin, conflicts[0].winner), (ConflictOrigin::Sync, Side::ICloud));
    assert_eq!(conflicts[0].fastmail_vcard, vcard("u1", "Jane Doe", "NOTE:fastmail\r\n").into_bytes());
    assert_eq!(h.fastmail.card(&target).unwrap().1, vcard("u1", "Jane Doe", "NOTE:icloud\r\n").into_bytes());
    assert!(h.applied().await == CycleSummary::default(), "settled");
}

#[tokio::test]
async fn a_baseline_conflict_with_fastmail_winning() {
    let h = Harness::new(Side::Fastmail);
    h.icloud.external_put("/card/jane.vcf", vcard("u1", "Jane Doe", "NOTE:icloud\r\n"));
    h.fastmail.external_put("/dav/jane.vcf", vcard("u1", "Jane Doe", "NOTE:fastmail\r\n"));

    let summary = h.applied().await;

    assert_eq!(summary.to_icloud.conflicts, 1);
    assert_eq!(
        h.icloud.card(&href("/card/jane.vcf")).unwrap().1,
        vcard("u1", "Jane Doe", "NOTE:fastmail\r\n").into_bytes()
    );
    let conflicts = h.state.conflicts();
    assert_eq!((conflicts[0].origin, conflicts[0].winner), (ConflictOrigin::Baseline, Side::Fastmail));
    assert_eq!(h.state.contacts().len(), 1);
}

#[tokio::test]
#[allow(
    clippy::assert_is_empty,
    reason = "asserting on is_empty() reads clearer than assert_eq! against an empty array literal"
)]
async fn a_conflict_is_recorded_even_when_the_push_fails() {
    let h = Harness::new(Side::ICloud);
    h.icloud.external_put("/card/jane.vcf", vcard("u1", "Jane Doe", "NOTE:icloud\r\n"));
    h.fastmail.external_put("/dav/jane.vcf", vcard("u1", "Jane Doe", "NOTE:fastmail\r\n"));
    h.fastmail
        .fail_next(BookOp::Put, AddressBookError::PreconditionFailed { href: href("/dav/jane.vcf") });

    let summary = h.applied().await;

    assert_eq!(summary.to_fastmail.errors, 1);
    assert_eq!(h.state.conflicts().len(), 1, "recorded before the push");
    assert!(h.state.contacts().is_empty());
    let mut held: Vec<Side> = h.state.failures().iter().map(|failure| failure.side).collect();
    held.sort_by_key(|side| side.as_str());
    assert_eq!(held, [Side::Fastmail, Side::ICloud], "both cards of the unsynced pair are held");

    h.sync().await;
    assert!(
        h.icloud.writes().is_empty() && h.fastmail.writes().len() == 1,
        "neither card is copied while held"
    );
}

#[tokio::test]
#[allow(
    clippy::assert_is_empty,
    reason = "asserting on is_empty() reads clearer than assert_eq! against an empty array literal"
)]
async fn a_content_pair_recreates_the_fastmail_card_under_the_icloud_uid() {
    let h = Harness::new(Side::ICloud);
    h.icloud.external_put("/card/ann.vcf", vcard("ic-1", "Ann Lee", "EMAIL:ann@example.com\r\n"));
    h.fastmail
        .external_put("/dav/ann.vcf", vcard("fm-1", "Ann Lee", &format!("EMAIL:ann@example.com\r\n{PHOTO}")));

    let summary = h.applied().await;

    assert_eq!(summary.to_fastmail.updated, 1);
    let target = minted(FASTMAIL_URL, "ic-1");
    let recreated = vcard("ic-1", "Ann Lee", &format!("EMAIL:ann@example.com\r\n{PHOTO}")).into_bytes();
    let writes = h.fastmail.writes();
    assert!(
        matches!(
            writes.as_slice(),
            [
                Write::Delete { href: deleted, if_match: Some(_) },
                Write::Put { href: created, precondition: Precondition::IfNoneMatch, body },
            ] if deleted.as_str() == "/dav/ann.vcf" && *created == target && *body == recreated
        ),
        "{writes:?}"
    );
    assert!(h.icloud.writes().is_empty());
    let rows = h.state.contacts();
    assert_eq!((rows[0].uid.as_str(), &rows[0].fastmail.href), ("ic-1", &target));
}

#[tokio::test]
async fn an_identity_pair_with_fastmail_winning_updates_icloud_first() {
    let h = Harness::new(Side::Fastmail);
    h.icloud
        .external_put("/card/bo.vcf", vcard("ic-4", "Bo Ray", "EMAIL:bo@example.com\r\nNOTE:icloud\r\n"));
    h.fastmail
        .external_put("/dav/bo.vcf", vcard("fm-4", "Bo Ray", "EMAIL:bo@example.com\r\nNOTE:fastmail\r\n"));

    let summary = h.applied().await;

    assert_eq!((summary.to_fastmail.updated, summary.to_icloud.updated), (1, 1));
    assert!(matches!(
        h.icloud.writes().as_slice(),
        [Write::Put {
            precondition: Precondition::IfMatch(_),
            ..
        }]
    ));
    assert_eq!(
        h.icloud.card(&href("/card/bo.vcf")).unwrap().1,
        vcard("ic-4", "Bo Ray", "EMAIL:bo@example.com\r\nNOTE:fastmail\r\n").into_bytes()
    );
    assert!(matches!(h.fastmail.writes().as_slice(), [Write::Delete { .. }, Write::Put { .. }]));
    let conflicts = h.state.conflicts();
    assert_eq!((conflicts[0].origin, conflicts[0].winner), (ConflictOrigin::Baseline, Side::Fastmail));
    assert_eq!(h.state.contacts()[0].uid.as_str(), "ic-4");
}

#[tokio::test]
async fn a_quiet_cycle_is_idle() {
    let h = Harness::new(Side::ICloud);
    h.icloud.external_put("/card/jane.vcf", vcard("u1", "Jane Doe", ""));
    h.applied().await;
    h.applied().await; // lists the daemon's own write once, writes nothing
    let writes = h.writes();

    assert!(matches!(h.sync().await, CycleOutcome::Idle));
    assert_eq!(h.writes(), writes);
}

#[tokio::test]
async fn an_aborted_cycle_keeps_the_previous_tokens() {
    let h = Harness::new(Side::ICloud);
    h.icloud.external_put("/card/jane.vcf", vcard("u1", "Jane Doe", ""));
    h.settle().await;
    let token = h.token(Side::ICloud);
    h.icloud.external_put("/card/bob.vcf", vcard("u2", "Bob Roe", ""));
    h.fastmail.fail_next(BookOp::Put, AddressBookError::Transient("reset".into()));

    h.run(CycleMode::Sync, false).await.unwrap_err();

    assert_eq!(h.token(Side::ICloud), token);
    assert_eq!(h.applied().await.to_fastmail.added, 1, "the next cycle still sees the new card");
}

#[tokio::test]
async fn an_expired_token_falls_back_to_a_full_listing() {
    let h = Harness::new(Side::ICloud);
    h.icloud.external_put("/card/jane.vcf", vcard("u1", "Jane Doe", ""));
    h.settle().await;
    h.icloud.expire_tokens();

    assert_eq!(h.applied().await, CycleSummary::default());
    assert!(matches!(h.sync().await, CycleOutcome::Idle));
}

#[tokio::test]
#[allow(
    clippy::assert_is_empty,
    reason = "asserting on is_empty() reads clearer than assert_eq! against an empty array literal"
)]
async fn skips_are_stored_every_cycle() {
    let h = Harness::new(Side::ICloud);
    h.icloud.external_put("/card/sam.vcf", vcard("ic-5", "Sam Poe", "EMAIL:sam@one.example\r\n"));
    h.fastmail.external_put("/dav/sam.vcf", vcard("fm-5", "Sam Poe", "EMAIL:sam@two.example\r\n"));

    let summary = h.applied().await;

    assert_eq!(summary.skipped, 2);
    assert_eq!(h.state.skips().len(), 2);
    assert_eq!(h.writes(), 0, "never guessed");

    h.fastmail.external_delete(&href("/dav/sam.vcf"));
    let summary = h.applied().await;
    assert!(h.state.skips().is_empty());
    assert_eq!(summary.to_fastmail.added, 1, "now unique, so copied");
}

#[tokio::test]
#[allow(
    clippy::assert_is_empty,
    reason = "asserting on is_empty() reads clearer than assert_eq! against an empty array literal"
)]
async fn a_failure_for_a_card_that_is_gone_is_cleared() {
    let h = Harness::new(Side::ICloud);
    h.icloud
        .external_put("/card/bad.vcf", "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:No Uid\r\nEND:VCARD\r\n");
    h.applied().await;
    assert_eq!(h.state.failures().len(), 1);

    h.icloud.external_delete(&href("/card/bad.vcf"));
    h.applied().await;

    assert!(h.state.failures().is_empty());
}

#[tokio::test]
#[allow(
    clippy::assert_is_empty,
    reason = "asserting on is_empty() reads clearer than assert_eq! against an empty array literal"
)]
async fn a_synced_cards_row_clears_once_its_duplicate_is_gone() {
    let h = Harness::new(Side::ICloud);
    h.seed_synced("u1", "Jane Doe").await;
    h.icloud.external_put("/card/u1-dup.vcf", vcard("u1", "Jane Doe", ""));

    h.applied().await;
    let failures = h.state.failures();
    assert_eq!(failures.len(), 2, "the synced href and the duplicate are both held");
    assert!(
        failures
            .iter()
            .all(|failure| failure.side == Side::ICloud && failure.uid.as_ref().map(Uid::as_str) == Some("u1")),
        "{failures:?}"
    );

    h.icloud.external_delete(&href("/card/u1-dup.vcf"));
    h.advance(TimeDelta::seconds(61));
    h.applied().await;

    assert!(
        h.state.failures().is_empty(),
        "the synced href sits unchanged at its own href forever; it must still clear once the group is released"
    );
    h.settle().await;
}

#[tokio::test]
#[allow(
    clippy::assert_is_empty,
    reason = "asserting on is_empty() reads clearer than assert_eq! against an empty array literal"
)]
async fn a_card_failing_three_times_is_persistent() {
    let h = Harness::new(Side::ICloud);
    h.icloud.external_put("/card/jane.vcf", vcard("u1", "Jane Doe", ""));
    for _ in 0..3 {
        h.fastmail.fail_next(BookOp::Put, AddressBookError::Permanent("400".into()));
    }

    assert!(h.applied().await.persistent_failures.is_empty());
    h.advance(TimeDelta::seconds(61));
    assert!(h.applied().await.persistent_failures.is_empty());
    h.advance(TimeDelta::seconds(121));
    let summary = h.applied().await;

    assert_eq!(summary.persistent_failures.len(), 1);
    assert_eq!(summary.persistent_failures[0].attempts, PERSISTENT_ATTEMPTS);
}

#[tokio::test]
async fn reset_rebaselines_and_keeps_the_conflict_history() {
    let h = Harness::new(Side::ICloud);
    h.icloud.external_put("/card/jane.vcf", vcard("u1", "Jane Doe", "NOTE:icloud\r\n"));
    h.fastmail.external_put("/dav/jane.vcf", vcard("u1", "Jane Doe", "NOTE:fastmail\r\n"));
    h.applied().await;
    let writes = h.writes();

    let CycleOutcome::Applied(summary) = h.run(CycleMode::Sync, true).await.unwrap() else {
        panic!("expected an applied cycle");
    };

    assert_eq!(summary.adopted, 1, "both sides already agree");
    assert_eq!(h.writes(), writes);
    assert_eq!(h.state.contacts().len(), 1);
    assert_eq!(h.state.conflicts().len(), 1, "conflict history survives --reset");
    assert_eq!(h.state.endpoints().len(), 2, "discovery recorded again");
}

#[tokio::test]
async fn the_summary_counts_each_direction_and_marks_contacts_seen() {
    let h = Harness::new(Side::ICloud);
    h.icloud.external_put("/card/a.vcf", vcard("u1", "Ann Lee", ""));
    h.icloud.external_put("/card/b.vcf", vcard("u2", "Bo Ray", ""));
    h.fastmail.external_put("/dav/c.vcf", vcard("u3", "Cy Doe", ""));

    let summary = h.applied().await;

    assert_eq!(
        summary.to_string(),
        "icloud→fastmail: fetched 2, added 2, updated 0, removed 0, conflicts 0, errors 0; fastmail→icloud: fetched 1, added 1, updated 0, removed 0, \
         conflicts 0, errors 0; adopted 0, refreshed 0, forgotten 0, state errors 0, deferred 0, skipped 0, persistent failures 0"
    );

    h.advance(TimeDelta::seconds(600));
    h.applied().await; // a full cycle: its listing includes the creates
    assert!(
        h.state
            .contacts()
            .iter()
            .all(|row| row.icloud.last_seen_at == h.now() && row.fastmail.last_seen_at == h.now())
    );
}

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn logs_carry_names_but_never_card_content() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let h = Harness::new(Side::ICloud);
    let secrets = "ORG:Acme\r\nEMAIL:jane@example.com\r\nTEL:+1 555 0100\r\nNOTE:NOTE-TEXT-XYZ\r\n";
    h.icloud.external_put("/card/jane.vcf", vcard("u1", "Jane Doe", secrets));
    h.icloud.external_put(
        "/card/bad.vcf",
        "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:No Uid\r\nEMAIL:bad@example.com\r\nEND:VCARD\r\n",
    );
    h.icloud.external_put("/card/x.vcf", vcard("ic-9", "", "EMAIL:x@example.com\r\n"));
    h.fastmail
        .external_put("/dav/x.vcf", vcard("fm-9", "", "EMAIL:x@example.com\r\nTEL:+1 555 0199\r\n"));
    h.applied().await;
    h.icloud
        .external_put("/card/jane.vcf", vcard("u1", "Jane Doe", &format!("{secrets}NOTE:icloud-edit\r\n")));
    h.fastmail
        .external_put(minted(FASTMAIL_URL, "u1"), vcard("u1", "Jane Doe", &format!("{secrets}NOTE:fastmail-edit\r\n")));
    h.applied().await;

    let logs = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains(r#"record="Jane Doe (Acme)" uid=u1 direction=icloud→fastmail op="add""#), "{logs}");
    assert!(
        logs.contains(r#"record="Jane Doe (Acme)" uid=u1 direction=icloud→fastmail op="update""#),
        "the conflict-resolution synced line never fired:\n{logs}"
    );
    assert!(logs.contains("conflicts 1"), "the conflict path never ran:\n{logs}");
    assert!(
        logs.contains("a contact with no name shares an email or phone with a contact with no name on the other side"),
        "the nameless-skip warning never fired:\n{logs}"
    );
    assert!(logs.contains("sync cycle:"), "{logs}");
    for secret in ["example.com", "555 01", "NOTE-TEXT-XYZ", "icloud-edit", "fastmail-edit"] {
        assert!(!logs.contains(secret), "{secret} leaked into the logs:\n{logs}");
    }
}

#[tokio::test]
async fn planned_counts_match_what_the_sync_applies() {
    let h = Harness::new(Side::ICloud);
    h.icloud.external_put(href("/card/a.vcf"), vcard("a", "Alpha", ""));
    h.fastmail.external_put(href("/dav/b.vcf"), vcard("b", "Bravo", ""));

    let (cycle, blocked) = h.dry_run().await;
    assert!(blocked.is_none());
    let planned = CycleSummary::planned(&cycle.plan.ops);
    assert_eq!((planned.to_fastmail.added, planned.to_icloud.added), (1, 1));

    let CycleOutcome::Applied(applied) = h.sync().await else {
        panic!("expected an applied cycle");
    };
    assert_eq!(planned.to_fastmail.added, applied.to_fastmail.added);
    assert_eq!(planned.to_icloud.added, applied.to_icloud.added);
}

/// Ann Lee on both sides with different UIDs and the same content: a pass-2
/// pair, so the first sync Recreates the Fastmail card under `ic-1`.
fn seed_content_pair(h: &Harness, fastmail_extra: &str) {
    h.icloud.external_put("/card/ann.vcf", vcard("ic-1", "Ann Lee", "EMAIL:ann@example.com\r\n"));
    h.fastmail
        .external_put("/dav/ann.vcf", vcard("fm-1", "Ann Lee", &format!("EMAIL:ann@example.com\r\n{fastmail_extra}")));
}

/// Leaves one journal row: the old Fastmail card deleted, the new one never
/// written (a transient error on the PUT aborts the cycle).
async fn interrupt_after_the_delete(h: &Harness) {
    h.fastmail.fail_next(BookOp::Put, AddressBookError::Transient("connection reset".into()));
    h.run(CycleMode::Sync, false).await.unwrap_err();
    assert!(h.fastmail.card(&href("/dav/ann.vcf")).is_none(), "the old card was deleted");
    assert_eq!(h.state.pending_recreates().len(), 1);
}

#[tokio::test]
async fn a_recreate_interrupted_after_the_delete_is_finished_from_the_journal() {
    let h = Harness::new(Side::ICloud);
    seed_content_pair(&h, PHOTO);
    interrupt_after_the_delete(&h).await;

    let summary = h.applied().await;

    let fastmail_body = format!("EMAIL:ann@example.com\r\n{PHOTO}");
    assert_eq!(
        h.fastmail.card(&minted(FASTMAIL_URL, "ic-1")).unwrap().1,
        vcard("ic-1", "Ann Lee", &fastmail_body).into_bytes(),
        "Fastmail keeps its own bytes, photo included"
    );
    assert_eq!(summary.adopted, 1);
    assert_eq!(h.state.pending_recreates(), []);
    assert_eq!(h.state.contacts().len(), 1);
    assert!(h.icloud.writes().is_empty(), "the iCloud card was never copied");
}

#[tokio::test]
async fn a_journal_entry_is_dropped_when_the_delete_never_happened() {
    let h = Harness::new(Side::ICloud);
    seed_content_pair(&h, "");
    h.fastmail.fail_next(BookOp::Delete, AddressBookError::Transient("timeout".into()));
    h.run(CycleMode::Sync, false).await.unwrap_err();
    assert_eq!(h.state.pending_recreates().len(), 1);

    h.applied().await;

    assert_eq!(h.state.pending_recreates(), []);
    assert!(h.fastmail.card(&href("/dav/ann.vcf")).is_none());
    assert!(h.fastmail.card(&minted(FASTMAIL_URL, "ic-1")).is_some());
    assert_eq!(h.state.contacts().len(), 1);
}

#[tokio::test]
async fn a_completed_recreate_leaves_no_journal() {
    let h = Harness::new(Side::ICloud);
    seed_content_pair(&h, "");

    h.applied().await;

    assert_eq!(h.state.pending_recreates(), []);
    assert_eq!(h.state.contacts().len(), 1);
}

#[tokio::test]
async fn a_failed_journal_write_never_deletes_the_old_card() {
    let h = Harness::new(Side::ICloud);
    // Discover (and record endpoints) first, so the journal is the next
    // state write.
    h.applied().await;
    seed_content_pair(&h, "");
    h.state.fail_next_write();

    h.sync().await;

    assert!(h.fastmail.card(&href("/dav/ann.vcf")).is_some(), "the old card must survive");
    assert!(
        !h.fastmail.writes().iter().any(|write| matches!(write, Write::Delete { .. })),
        "no DELETE without a journal row"
    );
    assert_eq!(h.state.pending_recreates(), []);
}

#[tokio::test]
async fn a_replay_the_server_rejects_falls_back_to_the_icloud_copy() {
    let h = Harness::new(Side::ICloud);
    seed_content_pair(&h, PHOTO);
    interrupt_after_the_delete(&h).await;
    h.fastmail.fail_next(BookOp::Put, AddressBookError::Permanent("400 bad request".into()));

    // Replay's PUT is rejected (non-fatal): the cycle continues and pairing
    // copies the iCloud card to the same minted href.
    h.applied().await;
    assert_eq!(h.state.pending_recreates().len(), 1, "the row is kept this cycle");
    let target = minted(FASTMAIL_URL, "ic-1");
    let (_, body) = h.fastmail.card(&target).expect("fallback: pairing copied the iCloud card to the minted href");
    let body = String::from_utf8(body).unwrap();
    assert!(
        body.contains("UID:ic-1") && !body.contains("PHOTO"),
        "the iCloud copy, not Fastmail's bytes: {body}"
    );

    // Next cycle: the new href exists, so the row is dropped.
    h.sync().await;
    assert_eq!(h.state.pending_recreates(), []);
}

#[tokio::test]
async fn reset_keeps_and_replays_the_journal() {
    let h = Harness::new(Side::ICloud);
    seed_content_pair(&h, PHOTO);
    interrupt_after_the_delete(&h).await;

    let outcome = h.run(CycleMode::Sync, true).await.unwrap();

    assert!(matches!(outcome, CycleOutcome::Applied(_)), "{outcome:?}");
    let fastmail_body = format!("EMAIL:ann@example.com\r\n{PHOTO}");
    assert_eq!(
        h.fastmail.card(&minted(FASTMAIL_URL, "ic-1")).unwrap().1,
        vcard("ic-1", "Ann Lee", &fastmail_body).into_bytes()
    );
    assert_eq!(h.state.pending_recreates(), []);
}

#[tokio::test]
async fn dry_run_leaves_the_journal_alone() {
    let h = Harness::new(Side::ICloud);
    seed_content_pair(&h, "");
    interrupt_after_the_delete(&h).await;
    let writes_before = h.fastmail.writes().len();

    h.dry_run().await;

    assert_eq!(h.fastmail.writes().len(), writes_before, "dry-run writes nothing");
    assert_eq!(h.state.pending_recreates().len(), 1);
}
