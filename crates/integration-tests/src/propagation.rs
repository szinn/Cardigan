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

#[tokio::test]
#[ignore = "needs a docker/colima daemon — run via `mise run integration-tests`"]
async fn a_card_created_on_fastmail_reaches_icloud() {
    let h = Harness::start().await;
    h.put(Side::Fastmail, "bo", &vcard("bo-1", "Bo Chen", "TEL:+1 555 0100\r\n")).await;

    h.settle().await;

    assert_eq!(h.uids(Side::ICloud).await, ["bo-1"]);
    assert_eq!(h.contacts().await.len(), 1);
}

#[tokio::test]
#[ignore = "needs a docker/colima daemon — run via `mise run integration-tests`"]
async fn an_edit_on_icloud_reaches_fastmail_without_bouncing() {
    let h = Harness::start().await;
    let href = h.put(Side::ICloud, "ann", &vcard("ann-1", "Ann Lee", "EMAIL:ann@example.com\r\n")).await;
    h.settle().await;
    h.icloud.reset_counts();
    h.fastmail.reset_counts();

    h.edit(Side::ICloud, &href, &vcard("ann-1", "Ann Lee", "EMAIL:ann@new.example\r\n")).await;
    h.settle().await;

    let fastmail = h.cards(Side::Fastmail).await;
    assert_eq!(fastmail.len(), 1);
    assert!(fastmail[0].body.contains("ann@new.example"), "the edit reached Fastmail");
    assert_eq!(h.fastmail.writes(), 1, "one update to Fastmail");
    assert_eq!(h.icloud.writes(), 0, "nothing bounced back to iCloud");
}

#[tokio::test]
#[ignore = "needs a docker/colima daemon — run via `mise run integration-tests`"]
async fn an_edit_on_fastmail_reaches_icloud_without_bouncing() {
    let h = Harness::start().await;
    h.put(Side::ICloud, "ann", &vcard("ann-1", "Ann Lee", "EMAIL:ann@example.com\r\n")).await;
    h.settle().await;
    let href = h.cards(Side::Fastmail).await[0].href.clone();
    h.icloud.reset_counts();
    h.fastmail.reset_counts();

    h.edit(Side::Fastmail, &href, &vcard("ann-1", "Ann Lee", "EMAIL:ann@fastmail.example\r\n"))
        .await;
    h.settle().await;

    let icloud = h.cards(Side::ICloud).await;
    assert!(icloud[0].body.contains("ann@fastmail.example"), "the edit reached iCloud");
    assert_eq!(h.icloud.writes(), 1, "one update to iCloud");
    assert_eq!(h.fastmail.writes(), 0, "nothing bounced back to Fastmail");
}

#[tokio::test]
#[ignore = "needs a docker/colima daemon — run via `mise run integration-tests`"]
async fn a_delete_on_icloud_removes_the_fastmail_card() {
    let h = Harness::start().await;
    let href = h.put(Side::ICloud, "ann", &vcard("ann-1", "Ann Lee", "")).await;
    h.settle().await;

    h.remove(Side::ICloud, &href).await;
    h.settle().await;

    assert_eq!(h.uids(Side::Fastmail).await, Vec::<String>::new());
    assert_eq!(h.contacts().await.len(), 0);
}

#[tokio::test]
#[ignore = "needs a docker/colima daemon — run via `mise run integration-tests`"]
async fn a_delete_on_fastmail_removes_the_icloud_card() {
    let h = Harness::start().await;
    h.put(Side::ICloud, "ann", &vcard("ann-1", "Ann Lee", "")).await;
    h.settle().await;
    let href = h.cards(Side::Fastmail).await[0].href.clone();

    h.remove(Side::Fastmail, &href).await;
    h.settle().await;

    assert_eq!(h.uids(Side::ICloud).await, Vec::<String>::new());
    assert_eq!(h.contacts().await.len(), 0);
}

#[tokio::test]
#[ignore = "needs a docker/colima daemon — run via `mise run integration-tests`"]
async fn a_listing_larger_than_one_multiget_batch_syncs_every_card() {
    let h = Harness::start_with_batch(2).await;
    for i in 0..5 {
        h.put(Side::ICloud, &format!("p{i}"), &vcard(&format!("p-{i}"), &format!("Person {i}"), ""))
            .await;
    }

    h.settle().await;

    assert_eq!(h.uids(Side::Fastmail).await, ["p-0", "p-1", "p-2", "p-3", "p-4"]);
}
