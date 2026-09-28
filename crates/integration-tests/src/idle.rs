//! A settled system stays quiet: no writes, even though Radicale rewrites
//! the cards it stores.

use cg_core::{contact::Side, service::CycleOutcome};

use crate::harness::{Harness, vcard};

#[tokio::test]
#[ignore = "needs a docker/colima daemon — run via `mise run integration-tests`"]
async fn a_settled_system_stays_idle_with_zero_writes() {
    let h = Harness::start().await;
    h.put(Side::ICloud, "ann", &vcard("ann-1", "Ann Lee", "EMAIL:ann@example.com\r\n")).await;
    h.put(Side::Fastmail, "bo", &vcard("bo-1", "Bo Chen", "TEL:+1 555 0100\r\n")).await;
    // A more realistic card: a long, foldable NOTE, a non-ASCII name and an
    // inline PHOTO, all synthetic.
    let note = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(6);
    let photo = "QUJD".repeat(150);
    let extra = format!("NOTE:{note}\r\nPHOTO;ENCODING=b;TYPE=JPEG:{photo}\r\n");
    h.put(Side::ICloud, "zoe", &vcard("zoe-1", "Zoë Müller", &extra)).await;
    h.settle().await;
    h.icloud.reset_counts();
    h.fastmail.reset_counts();

    for _ in 0..3 {
        assert!(matches!(h.cycle().await.unwrap(), CycleOutcome::Idle), "a quiet cycle is idle");
    }

    assert_eq!(h.icloud.writes() + h.fastmail.writes(), 0);
    assert_eq!(h.failures().await.len(), 0, "no failing card left");
}
