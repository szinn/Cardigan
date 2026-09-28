//! Changes made on one side reach the other, and nothing bounces back.

use cg_core::contact::Side;

use crate::harness::{Harness, vcard};

#[tokio::test]
#[ignore = "needs a docker/colima daemon — run via `mise run integration-tests`"]
async fn a_card_created_on_icloud_reaches_fastmail() {
    let h = Harness::start().await;
    h.put(Side::ICloud, "ann", &vcard("ann-1", "Ann Lee", "EMAIL:ann@example.com\r\n")).await;

    h.settle().await;

    assert_eq!(h.uids(Side::Fastmail).await, ["ann-1"]);
    assert_eq!(h.uids(Side::ICloud).await, ["ann-1"]);
    assert_eq!(h.contacts().await.len(), 1);
}
