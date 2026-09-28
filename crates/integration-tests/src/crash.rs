//! A crash right after a server write, then a restart on the same state
//! file: every case converges with one card per UID on each side.

use cg_core::contact::Side;

use crate::{
    faulty::{Fault, Write},
    harness::{Harness, vcard},
};

const PHOTO: &str = "PHOTO;ENCODING=b;TYPE=JPEG:QUJD\r\n";

async fn assert_converged(h: &Harness, uid: &str) {
    assert_eq!(h.uids(Side::ICloud).await, [uid], "one iCloud card");
    assert_eq!(h.uids(Side::Fastmail).await, [uid], "one Fastmail card");
    assert_eq!(h.contacts().await.len(), 1);
    assert_eq!(h.pending().await.len(), 0, "no journal row left");
}

#[tokio::test]
#[ignore = "needs a docker/colima daemon — run via `mise run integration-tests`"]
async fn a_crash_after_a_create_converges_without_a_duplicate() {
    let mut h = Harness::start().await;
    h.put(Side::ICloud, "ann", &vcard("ann-1", "Ann Lee", "EMAIL:ann@example.com\r\n")).await;
    h.fastmail.fault_after_next(Write::Put, Fault::Crash);

    h.crash_cycle().await;
    assert_eq!(h.uids(Side::Fastmail).await, ["ann-1"], "the create landed before the crash");
    h.restart().await;
    h.settle().await;

    assert_converged(&h, "ann-1").await;
}

#[tokio::test]
#[ignore = "needs a docker/colima daemon — run via `mise run integration-tests`"]
async fn a_crash_after_an_update_converges() {
    let mut h = Harness::start().await;
    let href = h.put(Side::ICloud, "ann", &vcard("ann-1", "Ann Lee", "EMAIL:ann@example.com\r\n")).await;
    h.settle().await;
    h.edit(Side::ICloud, &href, &vcard("ann-1", "Ann Lee", "EMAIL:ann@new.example\r\n")).await;
    h.fastmail.fault_after_next(Write::Put, Fault::Crash);

    h.crash_cycle().await;
    h.restart().await;
    h.settle().await;

    assert_converged(&h, "ann-1").await;
    assert!(h.cards(Side::Fastmail).await[0].body.contains("ann@new.example"));
}

#[tokio::test]
#[ignore = "needs a docker/colima daemon — run via `mise run integration-tests`"]
async fn a_crash_after_the_recreate_delete_is_finished_from_the_journal() {
    let mut h = Harness::start().await;
    h.put(Side::ICloud, "ann", &vcard("ic-1", "Ann Lee", "EMAIL:ann@example.com\r\n")).await;
    h.put(Side::Fastmail, "ann", &vcard("fm-1", "Ann Lee", &format!("EMAIL:ann@example.com\r\n{PHOTO}")))
        .await;
    h.fastmail.fault_after_next(Write::Delete, Fault::Crash);

    h.crash_cycle().await;
    assert_eq!(h.uids(Side::Fastmail).await, Vec::<String>::new(), "the old card was deleted");
    assert_eq!(h.pending().await.len(), 1, "the journal holds Fastmail's card");
    h.restart().await;
    h.settle().await;

    assert_converged(&h, "ic-1").await;
    assert!(
        h.cards(Side::Fastmail).await[0].body.contains("PHOTO"),
        "Fastmail kept its own card, photo included"
    );
}

#[tokio::test]
#[ignore = "needs a docker/colima daemon — run via `mise run integration-tests`"]
async fn a_crash_after_the_recreate_put_clears_the_journal_and_adopts() {
    let mut h = Harness::start().await;
    h.put(Side::ICloud, "ann", &vcard("ic-1", "Ann Lee", "EMAIL:ann@example.com\r\n")).await;
    h.put(Side::Fastmail, "ann", &vcard("fm-1", "Ann Lee", "EMAIL:ann@example.com\r\n")).await;
    h.fastmail.fault_after_next(Write::Put, Fault::Crash);

    h.crash_cycle().await;
    assert_eq!(h.uids(Side::Fastmail).await, ["ic-1"], "the new card landed before the crash");
    assert_eq!(h.pending().await.len(), 1);
    h.restart().await;
    h.settle().await;

    assert_converged(&h, "ic-1").await;
}

#[tokio::test]
#[ignore = "needs a docker/colima daemon — run via `mise run integration-tests`"]
async fn a_lost_response_after_a_create_converges_without_a_duplicate() {
    let h = Harness::start().await;
    h.put(Side::ICloud, "ann", &vcard("ann-1", "Ann Lee", "")).await;
    h.fastmail.fault_after_next(Write::Put, Fault::LostResponse);

    h.cycle().await.expect_err("a lost response aborts the cycle");
    h.settle().await;

    assert_converged(&h, "ann-1").await;
}
