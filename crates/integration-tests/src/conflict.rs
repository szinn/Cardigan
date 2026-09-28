//! A card that changes on the server between listing and PUT gets a real
//! 412; the next cycle resolves it as a conflict.

use cg_core::{contact::Side, service::CycleOutcome};

use crate::harness::{Harness, vcard};

#[tokio::test]
#[ignore = "needs a docker/colima daemon — run via `mise run integration-tests`"]
async fn a_card_edited_between_listing_and_put_is_resolved_as_a_conflict() {
    let h = Harness::start().await;
    let icloud_href = h.put(Side::ICloud, "ann", &vcard("ann-1", "Ann Lee", "EMAIL:ann@example.com\r\n")).await;
    h.settle().await;
    let fastmail_href = h.cards(Side::Fastmail).await[0].href.clone();

    // The user edits iCloud; while the engine is about to push that edit,
    // someone edits the same card on Fastmail, so the PUT's If-Match is stale.
    h.edit(Side::ICloud, &icloud_href, &vcard("ann-1", "Ann Lee", "EMAIL:ann@icloud.example\r\n"))
        .await;
    h.fastmail.before_next_put(h.edit_hook(
        Side::Fastmail,
        fastmail_href,
        vcard("ann-1", "Ann Lee", "EMAIL:ann@example.com\r\nNOTE:edited on fastmail\r\n"),
    ));

    let outcome = h.cycle().await.expect("a 412 is not cycle-fatal");
    assert!(matches!(outcome, CycleOutcome::Applied(_)));
    h.settle().await;

    let fastmail = h.cards(Side::Fastmail).await;
    assert_eq!(fastmail.len(), 1);
    assert!(fastmail[0].body.contains("ann@icloud.example"), "iCloud (the configured winner) won");
    assert!(!fastmail[0].body.contains("edited on fastmail"));
    assert_eq!(h.conflicts().await.len(), 1, "the losing Fastmail edit is kept in the conflict history");
    assert_eq!(h.uids(Side::ICloud).await, ["ann-1"]);
}
