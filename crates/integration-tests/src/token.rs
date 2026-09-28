//! A stored sync token the server no longer knows falls back to a full
//! listing.

use cg_core::contact::Side;

use crate::harness::{Harness, vcard};

const UNKNOWN_TOKEN: &str = "http://radicale.org/ns/sync/0000000000000000000000000000000000000000000000000000000000000000";

#[tokio::test]
#[ignore = "needs a docker/colima daemon — run via `mise run integration-tests`"]
async fn an_invalid_sync_token_falls_back_to_a_full_listing() {
    let h = Harness::start().await;
    h.put(Side::ICloud, "ann", &vcard("ann-1", "Ann Lee", "")).await;
    h.settle().await;

    h.put(Side::ICloud, "bo", &vcard("bo-1", "Bo Chen", "")).await;
    h.set_token(Side::ICloud, UNKNOWN_TOKEN).await;
    h.cycle().await.expect("an invalid token is not an error");

    assert_eq!(h.uids(Side::Fastmail).await, ["ann-1", "bo-1"], "the change was picked up by the full listing");
    let token = h.token(Side::ICloud).await.expect("a fresh token was stored");
    assert_ne!(token, UNKNOWN_TOKEN);
    h.settle().await;
    assert_eq!(h.contacts().await.len(), 2);
}
