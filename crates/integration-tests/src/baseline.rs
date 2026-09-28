//! The first cycle pairs what already exists on both sides.

use cg_core::contact::Side;

use crate::harness::{Harness, vcard};

#[tokio::test]
#[ignore = "needs a docker/colima daemon — run via `mise run integration-tests`"]
async fn the_same_card_on_both_sides_is_adopted_without_writes() {
    let h = Harness::start().await;
    let card = vcard("ann-1", "Ann Lee", "EMAIL:ann@example.com\r\n");
    h.put(Side::ICloud, "ann", &card).await;
    h.put(Side::Fastmail, "ann", &card).await;

    h.settle().await;

    assert_eq!(h.uids(Side::ICloud).await, ["ann-1"]);
    assert_eq!(h.uids(Side::Fastmail).await, ["ann-1"]);
    assert_eq!(h.icloud.writes() + h.fastmail.writes(), 0, "adopting writes nothing");
    assert_eq!(h.contacts().await.len(), 1);
}

#[tokio::test]
#[ignore = "needs a docker/colima daemon — run via `mise run integration-tests`"]
async fn a_content_pair_is_recreated_under_the_icloud_uid() {
    let h = Harness::start().await;
    h.put(Side::ICloud, "ann", &vcard("ic-1", "Ann Lee", "EMAIL:ann@example.com\r\n")).await;
    h.put(Side::Fastmail, "ann", &vcard("fm-1", "Ann Lee", "EMAIL:ann@example.com\r\n")).await;

    h.settle().await;

    assert_eq!(h.uids(Side::ICloud).await, ["ic-1"]);
    assert_eq!(h.uids(Side::Fastmail).await, ["ic-1"], "Fastmail's card now carries the iCloud UID");
    assert_eq!(h.pending().await.len(), 0);
    assert_eq!(h.contacts().await.len(), 1);
}
