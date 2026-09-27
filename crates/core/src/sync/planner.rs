use super::{
    Diagnostic, Op, Plan, Resource, SYNC_HASH, Snapshot, SyncedCard, Unsynced,
    sides::{Present, SideView},
};
use crate::{
    contact::{CANONICAL_VERSION, CardHash, ConflictWinner, Side, Uid, VCard},
    state::{ConflictOrigin, ContactState},
};

/// Everything one planning run needs.
#[derive(Debug, Clone, Copy)]
pub struct PlanInput<'a> {
    pub icloud: &'a Snapshot,
    pub fastmail: &'a Snapshot,
    /// Every contact state row (`ContactStateRepository::list_all`).
    pub state: &'a [ContactState],
    /// `CARDIGAN_CONFLICT_WINNER`.
    pub winner: ConflictWinner,
}

/// The planner's result: ops for synced contacts, and the cards pairing
/// (CG-7) must handle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Planned {
    pub plan: Plan,
    pub unsynced: Unsynced,
}

/// Decides every synced contact's op, in state order. Cards without a state
/// row are never acted on here: they come back in `Planned::unsynced`.
pub fn plan(input: &PlanInput<'_>) -> Planned {
    let icloud = SideView::classify(Side::ICloud, input.icloud, input.state);
    let fastmail = SideView::classify(Side::Fastmail, input.fastmail, input.state);

    let mut ops = Vec::new();
    let mut diagnostics = icloud.diagnostics;
    diagnostics.extend(fastmail.diagnostics);
    for row in input.state {
        if icloud.held.contains(&row.uid) || fastmail.held.contains(&row.uid) {
            continue;
        }
        let baseline = Baseline::of(row);
        let i = baseline.status(Side::ICloud, icloud.tracked.get(&row.uid));
        let f = baseline.status(Side::Fastmail, fastmail.tracked.get(&row.uid));
        // An uncertain side may be hiding this row's card at an href the
        // planner cannot attribute (unreadable or held): a `Deleted` status
        // there might be wrong, so emit no op rather than risk a wrong
        // delete or resurrect (Review Focus 1). Report the skip instead of
        // silently dropping the row.
        let icloud_deferred = icloud.uncertain && i.is_deleted();
        let fastmail_deferred = fastmail.uncertain && f.is_deleted();
        if icloud_deferred || fastmail_deferred {
            if icloud_deferred {
                diagnostics.push(Diagnostic::DeletionDeferred {
                    side: Side::ICloud,
                    uid: row.uid.clone(),
                });
            }
            if fastmail_deferred {
                diagnostics.push(Diagnostic::DeletionDeferred {
                    side: Side::Fastmail,
                    uid: row.uid.clone(),
                });
            }
            continue;
        }
        match decide(&baseline, i, f, input) {
            Some(Ok(op)) => ops.push(op),
            Some(Err(diagnostic)) => diagnostics.push(diagnostic),
            None => {}
        }
    }

    // A UID held on one side (duplicated, UID-changed, or unreadable at its
    // synced href) must not reach pairing from the other side, or pairing
    // would Create a copy of a card the planner is still holding here.
    let icloud_unsynced = icloud.unsynced.into_iter().filter(|c| !fastmail.held.contains(c.card.uid())).collect();
    let fastmail_unsynced = fastmail.unsynced.into_iter().filter(|c| !icloud.held.contains(c.card.uid())).collect();

    Planned {
        plan: Plan { ops, diagnostics },
        unsynced: Unsynced {
            icloud: icloud_unsynced,
            fastmail: fastmail_unsynced,
        },
    }
}

/// A synced contact's card on one side, relative to the last sync.
enum Status<'a> {
    /// Not fetched: at the stored resource.
    Unchanged(Resource),
    /// Fetched, content equal to the baseline.
    Same(Resource, &'a VCard),
    Changed(Resource, &'a VCard),
    Deleted,
}

impl Status<'_> {
    /// Whether this side has nothing at the row's tracked resource.
    fn is_deleted(&self) -> bool {
        matches!(self, Self::Deleted)
    }

    /// The new resource to record when the card was fetched but not changed.
    fn refreshed(self) -> Option<Resource> {
        match self {
            Self::Same(resource, _) => Some(resource),
            _ => None,
        }
    }
}

/// The last-synced content of a row, hashed under the current
/// `CANONICAL_VERSION`.
struct Baseline<'a> {
    row: &'a ContactState,
    hash: CardHash,
    stale: bool,
}

impl<'a> Baseline<'a> {
    fn of(row: &'a ContactState) -> Self {
        let stale = row.hash_version != CANONICAL_VERSION;
        let hash = if stale {
            row.last_synced_vcard.canonical_hash(SYNC_HASH)
        } else {
            row.content_hash
        };
        Self { row, hash, stale }
    }

    fn stored(&self, side: Side) -> Resource {
        let stored = self.row.side(side);
        Resource {
            href: stored.href.clone(),
            etag: stored.etag.clone(),
        }
    }

    /// Photos never count (Decision 7): a card differs from the baseline only
    /// if its `SYNC_HASH` does.
    fn status<'b>(&self, side: Side, present: Option<&'b Present>) -> Status<'b> {
        match present {
            None => Status::Deleted,
            Some(Present::Unchanged) => Status::Unchanged(self.stored(side)),
            Some(Present::Fetched { resource, card }) => {
                if card.canonical_hash(SYNC_HASH) == self.hash {
                    Status::Same(resource.clone(), card)
                } else {
                    Status::Changed(resource.clone(), card)
                }
            }
        }
    }

    /// For a stale hash version: the last-synced card under the current hash.
    fn rehash(&self) -> Option<SyncedCard> {
        self.stale.then(|| SyncedCard::recorded(&self.row.last_synced_vcard))
    }
}

/// Decision 4's table. `Err` is an update the planner refuses because its
/// target was not read.
fn decide(baseline: &Baseline<'_>, icloud: Status<'_>, fastmail: Status<'_>, input: &PlanInput<'_>) -> Option<Result<Op, Diagnostic>> {
    let uid = baseline.row.uid.clone();
    match (icloud, fastmail) {
        (Status::Deleted, Status::Deleted) => Some(Ok(Op::Forget { uid })),
        (Status::Changed(i_resource, i_card), Status::Changed(f_resource, f_card)) => {
            if i_card.canonical_hash(SYNC_HASH) == f_card.canonical_hash(SYNC_HASH) {
                // The same edit landed on both sides.
                return Some(Ok(Op::Refresh {
                    uid,
                    icloud: Some(i_resource),
                    fastmail: Some(f_resource),
                    synced: Some(SyncedCard::recorded(i_card)),
                }));
            }
            let winner = input.winner;
            let (source, target, card, loser) = match winner {
                Side::ICloud => (i_resource, f_resource, i_card, f_card),
                Side::Fastmail => (f_resource, i_resource, f_card, i_card),
            };
            Some(Ok(Op::Conflict {
                uid,
                origin: ConflictOrigin::Sync,
                winner,
                target,
                source,
                synced: SyncedCard::for_push(card, Some(loser)),
                icloud_card: i_card.clone(),
                fastmail_card: f_card.clone(),
            }))
        }
        (Status::Changed(source, card), other) => Some(one_sided(uid, Side::Fastmail, source, card, other)),
        (other, Status::Changed(source, card)) => Some(one_sided(uid, Side::ICloud, source, card, other)),
        // `Changed` on either side is already matched above, so the only
        // cases left here are `Unchanged`/`Same` against `Deleted`: the
        // target is bound directly, with no need for a total-but-panicking
        // accessor on `Status`.
        (Status::Deleted, Status::Unchanged(target) | Status::Same(target, _)) => Some(Ok(Op::Delete {
            uid,
            on: Side::Fastmail,
            target,
        })),
        (Status::Unchanged(target) | Status::Same(target, _), Status::Deleted) => Some(Ok(Op::Delete { uid, on: Side::ICloud, target })),
        (i, f) => {
            let (icloud, fastmail, synced) = (i.refreshed(), f.refreshed(), baseline.rehash());
            (icloud.is_some() || fastmail.is_some() || synced.is_some()).then_some(Ok(Op::Refresh { uid, icloud, fastmail, synced }))
        }
    }
}

/// Changed on one side only: bring `to` up to date, keeping its photo, or
/// bring it back if it was deleted there (the edit wins; no photo). An
/// update needs the card it replaces (Decision 7).
fn one_sided(uid: Uid, to: Side, source: Resource, card: &VCard, other: Status<'_>) -> Result<Op, Diagnostic> {
    match other {
        Status::Deleted => Ok(Op::Resurrect {
            uid,
            to,
            source,
            synced: SyncedCard::for_push(card, None),
        }),
        Status::Unchanged(target) => Err(Diagnostic::UnreadTarget { side: to, uid, target }),
        Status::Same(target, current) | Status::Changed(target, current) => Ok(Op::Update {
            uid,
            to,
            target,
            source,
            synced: SyncedCard::for_push(card, Some(current)),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        contact::{ETag, VCardError},
        sync::{
            Entry,
            fixtures::{EMBEDDED_PHOTO, URI_PHOTO, card, card_with, fetched, res, row, snapshot, unchanged},
        },
    };

    fn input<'a>(icloud: &'a Snapshot, fastmail: &'a Snapshot, state: &'a [ContactState], winner: Side) -> PlanInput<'a> {
        PlanInput {
            icloud,
            fastmail,
            state,
            winner,
        }
    }

    fn synced() -> VCard {
        card("u1", "Jane Doe")
    }

    fn state() -> Vec<ContactState> {
        vec![row(1, &synced(), ("/i/u1.vcf", "i1"), ("/f/u1.vcf", "f1"))]
    }

    fn render(plan: &Plan) -> String {
        if plan.ops.is_empty() && plan.diagnostics.is_empty() {
            "(nothing)".to_owned()
        } else {
            plan.to_string().trim_end().replace('\n', "; ")
        }
    }

    #[derive(Clone, Copy)]
    enum Cell {
        Unchanged,
        Same,
        Changed,
        Deleted,
    }

    impl Cell {
        const ALL: [Self; 4] = [Self::Unchanged, Self::Same, Self::Changed, Self::Deleted];

        fn label(self) -> &'static str {
            match self {
                Self::Unchanged => "unchanged",
                Self::Same => "same",
                Self::Changed => "changed",
                Self::Deleted => "deleted",
            }
        }

        /// Contact u1 on side `s` (`i` or `f`): href `/{s}/u1.vcf`, stored
        /// ETag `{s}1`, a fetched ETag `{s}2`.
        fn snapshot(self, s: &str) -> Snapshot {
            let href = format!("/{s}/u1.vcf");
            match self {
                Self::Unchanged => snapshot([(&href, unchanged(&format!("{s}1")))]),
                // A server rewrite: only a volatile property differs.
                Self::Same => snapshot([(&href, fetched(&format!("{s}2"), card_with("u1", "Jane Doe", "REV:2026-09-26T00:00:00Z\r\n")))]),
                Self::Changed => snapshot([(
                    &href,
                    fetched(&format!("{s}2"), card_with("u1", "Jane Doe", &format!("NOTE:edited on {s}\r\n"))),
                )]),
                Self::Deleted => Snapshot::new(),
            }
        }
    }

    /// The state-present half of the spec's matrix: {unchanged, same,
    /// changed, deleted} on each side. ("new" cannot occur with a state row;
    /// the state-absent half is pairing's, tested in CG-7's plan_cycle.)
    /// Changed against Unchanged never happens in a real cycle, because
    /// `fetch_lists` fetches both sides of a touched row; the planner refuses
    /// to replace the unread card.
    #[test]
    fn state_matrix() {
        let state = state();
        let mut lines = Vec::new();
        for i in Cell::ALL {
            for f in Cell::ALL {
                let (icloud, fastmail) = (i.snapshot("i"), f.snapshot("f"));
                let planned = plan(&input(&icloud, &fastmail, &state, Side::ICloud));
                assert_eq!(planned.unsynced, Unsynced::default());
                lines.push(format!("{}/{}: {}", i.label(), f.label(), render(&planned.plan)));
            }
        }
        insta::assert_snapshot!(lines.join("\n"), @r"
        unchanged/unchanged: (nothing)
        unchanged/same: refresh uid=u1 fastmail=/f/u1.vcf@f2
        unchanged/changed: ! unread target icloud uid=u1 /i/u1.vcf@i1
        unchanged/deleted: delete icloud uid=u1 /i/u1.vcf@i1
        same/unchanged: refresh uid=u1 icloud=/i/u1.vcf@i2
        same/same: refresh uid=u1 icloud=/i/u1.vcf@i2 fastmail=/f/u1.vcf@f2
        same/changed: update icloud uid=u1 /i/u1.vcf@i2
        same/deleted: delete icloud uid=u1 /i/u1.vcf@i2
        changed/unchanged: ! unread target fastmail uid=u1 /f/u1.vcf@f1
        changed/same: update fastmail uid=u1 /f/u1.vcf@f2
        changed/changed: conflict(sync) icloud wins uid=u1 → fastmail /f/u1.vcf@f2
        changed/deleted: resurrect fastmail uid=u1 from=/i/u1.vcf
        deleted/unchanged: delete fastmail uid=u1 /f/u1.vcf@f1
        deleted/same: delete fastmail uid=u1 /f/u1.vcf@f2
        deleted/changed: resurrect icloud uid=u1 from=/f/u1.vcf
        deleted/deleted: forget uid=u1
        ");
    }

    #[test]
    fn update_carries_the_changed_card() {
        let edited = card_with("u1", "Jane Doe", "NOTE:new\r\n");
        let icloud = snapshot([("/i/u1.vcf", fetched("i2", edited.clone()))]);
        let fastmail = snapshot([("/f/u1.vcf", fetched("f1", synced()))]);

        let planned = plan(&input(&icloud, &fastmail, &state(), Side::ICloud));

        assert_eq!(
            planned.plan.ops,
            [Op::Update {
                uid: Uid::from("u1"),
                to: Side::Fastmail,
                target: res("/f/u1.vcf", "f1"),
                source: res("/i/u1.vcf", "i2"),
                synced: SyncedCard::for_push(&edited, Some(&synced())),
            }]
        );
    }

    #[test]
    fn conflict_follows_the_configured_winner_and_keeps_both_cards() {
        let i_edit = card_with("u1", "Jane Doe", "NOTE:icloud\r\n");
        let f_edit = card_with("u1", "Jane Doe", "NOTE:fastmail\r\n");
        let icloud = snapshot([("/i/u1.vcf", fetched("i2", i_edit.clone()))]);
        let fastmail = snapshot([("/f/u1.vcf", fetched("f2", f_edit.clone()))]);

        let planned = plan(&input(&icloud, &fastmail, &state(), Side::Fastmail));

        assert_eq!(render(&planned.plan), "conflict(sync) fastmail wins uid=u1 → icloud /i/u1.vcf@i2");
        assert_eq!(
            planned.plan.ops,
            [Op::Conflict {
                uid: Uid::from("u1"),
                origin: ConflictOrigin::Sync,
                winner: Side::Fastmail,
                target: res("/i/u1.vcf", "i2"),
                source: res("/f/u1.vcf", "f2"),
                synced: SyncedCard::for_push(&f_edit, Some(&i_edit)),
                icloud_card: i_edit,
                fastmail_card: f_edit,
            }]
        );
    }

    #[test]
    fn same_edit_on_both_sides_is_a_content_refresh() {
        let i_edit = card_with("u1", "Jane Doe", "NOTE:same\r\n");
        let f_edit = card_with("u1", "Jane Doe", "NOTE:same\r\nREV:2026-09-26T00:00:00Z\r\n");
        let icloud = snapshot([("/i/u1.vcf", fetched("i2", i_edit.clone()))]);
        let fastmail = snapshot([("/f/u1.vcf", fetched("f2", f_edit))]);

        let planned = plan(&input(&icloud, &fastmail, &state(), Side::ICloud));

        assert_eq!(
            planned.plan.ops,
            [Op::Refresh {
                uid: Uid::from("u1"),
                icloud: Some(res("/i/u1.vcf", "i2")),
                fastmail: Some(res("/f/u1.vcf", "f2")),
                synced: Some(SyncedCard::recorded(&i_edit)),
            }]
        );
    }

    #[test]
    fn moved_card_is_a_refresh_not_a_delete() {
        let icloud = snapshot([("/i/moved.vcf", fetched("i2", synced()))]);
        let fastmail = snapshot([("/f/u1.vcf", unchanged("f1"))]);

        let planned = plan(&input(&icloud, &fastmail, &state(), Side::ICloud));

        assert_eq!(render(&planned.plan), "refresh uid=u1 icloud=/i/moved.vcf@i2");
    }

    #[test]
    fn moved_and_edited_card_updates_the_other_side_from_its_new_href() {
        let edited = card_with("u1", "Jane Doe", "NOTE:new\r\n");
        let icloud = snapshot([("/i/moved.vcf", fetched("i2", edited))]);
        let fastmail = snapshot([("/f/u1.vcf", fetched("f1", synced()))]);

        let planned = plan(&input(&icloud, &fastmail, &state(), Side::ICloud));

        match &*planned.plan.ops {
            [Op::Update { to, source, .. }] => {
                assert_eq!(*to, Side::Fastmail);
                assert_eq!(*source, res("/i/moved.vcf", "i2"));
            }
            other => panic!("expected one update, got {other:?}"),
        }
    }

    #[test]
    fn stale_hash_version_is_rehashed_not_synced() {
        let mut state = state();
        state[0].hash_version = 0;
        state[0].content_hash = card("zz", "Someone Else").canonical_hash(SYNC_HASH);

        let icloud = snapshot([("/i/u1.vcf", unchanged("i1"))]);
        let fastmail = snapshot([("/f/u1.vcf", unchanged("f1"))]);
        let planned = plan(&input(&icloud, &fastmail, &state, Side::ICloud));
        assert_eq!(
            planned.plan.ops,
            [Op::Refresh {
                uid: Uid::from("u1"),
                icloud: None,
                fastmail: None,
                synced: Some(SyncedCard::recorded(&synced())),
            }]
        );

        // A server rewrite on a stale row is still only a refresh.
        let fastmail = snapshot([("/f/u1.vcf", fetched("f2", card_with("u1", "Jane Doe", "REV:2026-09-26T00:00:00Z\r\n")))]);
        let planned = plan(&input(&icloud, &fastmail, &state, Side::ICloud));
        assert_eq!(render(&planned.plan), "refresh uid=u1 fastmail=/f/u1.vcf@f2 content");
    }

    #[test]
    fn photo_only_change_is_not_a_change() {
        // iCloud added a photo (as a URI); nothing else changed.
        let icloud = snapshot([("/i/u1.vcf", fetched("i2", card_with("u1", "Jane Doe", URI_PHOTO)))]);
        let fastmail = snapshot([("/f/u1.vcf", unchanged("f1"))]);

        let planned = plan(&input(&icloud, &fastmail, &state(), Side::ICloud));

        assert_eq!(render(&planned.plan), "refresh uid=u1 icloud=/i/u1.vcf@i2");
    }

    #[test]
    fn update_keeps_the_targets_photo() {
        // iCloud edits a card that has a URI photo; Fastmail holds its own
        // inline photo, which is not a change there.
        let i_edit = card_with("u1", "Jane Doe", &format!("NOTE:new\r\n{URI_PHOTO}"));
        let f_card = card_with("u1", "Jane Doe", EMBEDDED_PHOTO);
        let icloud = snapshot([("/i/u1.vcf", fetched("i2", i_edit))]);
        let fastmail = snapshot([("/f/u1.vcf", fetched("f1", f_card.clone()))]);

        let planned = plan(&input(&icloud, &fastmail, &state(), Side::ICloud));

        assert_eq!(render(&planned.plan), "update fastmail uid=u1 /f/u1.vcf@f1 photo-kept");
        let recorded = card_with("u1", "Jane Doe", "NOTE:new\r\n");
        match &*planned.plan.ops {
            [Op::Update { synced, .. }] => {
                assert_eq!(synced.card, recorded);
                assert_eq!(synced.body(), &recorded.with_photos_of(&f_card));
            }
            other => panic!("expected one update, got {other:?}"),
        }
    }

    #[test]
    fn conflict_keeps_the_losers_photo() {
        let i_edit = card_with("u1", "Jane Doe", &format!("NOTE:icloud\r\n{URI_PHOTO}"));
        let f_edit = card_with("u1", "Jane Doe", &format!("NOTE:fastmail\r\n{EMBEDDED_PHOTO}"));
        let icloud = snapshot([("/i/u1.vcf", fetched("i2", i_edit.clone()))]);
        let fastmail = snapshot([("/f/u1.vcf", fetched("f2", f_edit))]);

        let planned = plan(&input(&icloud, &fastmail, &state(), Side::Fastmail));

        assert_eq!(render(&planned.plan), "conflict(sync) fastmail wins uid=u1 → icloud /i/u1.vcf@i2 photo-kept");
        let winner = card_with("u1", "Jane Doe", "NOTE:fastmail\r\n");
        match &*planned.plan.ops {
            [Op::Conflict { synced, .. }] => assert_eq!(synced.body(), &winner.with_photos_of(&i_edit)),
            other => panic!("expected one conflict, got {other:?}"),
        }
    }

    #[test]
    fn update_never_replaces_an_unread_card() {
        let icloud = snapshot([("/i/u1.vcf", fetched("i2", card_with("u1", "Jane Doe", "NOTE:new\r\n")))]);
        let fastmail = snapshot([("/f/u1.vcf", unchanged("f1"))]);

        let planned = plan(&input(&icloud, &fastmail, &state(), Side::ICloud));

        assert_eq!(render(&planned.plan), "! unread target fastmail uid=u1 /f/u1.vcf@f1");
    }

    #[test]
    fn resurrect_carries_no_photo() {
        let i_edit = card_with("u1", "Jane Doe", &format!("NOTE:new\r\n{URI_PHOTO}"));
        let icloud = snapshot([("/i/u1.vcf", fetched("i2", i_edit))]);

        let planned = plan(&input(&icloud, &Snapshot::new(), &state(), Side::ICloud));

        match &*planned.plan.ops {
            [Op::Resurrect { synced, .. }] => {
                assert_eq!(synced.body(), &card_with("u1", "Jane Doe", "NOTE:new\r\n"));
                assert_eq!(synced.put_with_photo, None);
            }
            other => panic!("expected one resurrect, got {other:?}"),
        }
    }

    #[test]
    fn held_card_blocks_every_op() {
        let icloud = snapshot([("/i/u1.vcf", Entry::Held(ETag::from("i2")))]);
        let fastmail = snapshot([("/f/u1.vcf", fetched("f2", card_with("u1", "Jane Doe", "NOTE:x\r\n")))]);

        let planned = plan(&input(&icloud, &fastmail, &state(), Side::ICloud));

        assert!(planned.plan.ops.is_empty(), "{}", planned.plan);
    }

    #[test]
    fn unreadable_synced_card_is_never_deleted() {
        let icloud = snapshot([(
            "/i/u1.vcf",
            Entry::Fetched {
                etag: ETag::from("i2"),
                card: Err(VCardError::UnsupportedVersion { version: "4.0".into() }),
            },
        )]);
        let fastmail = snapshot([("/f/u1.vcf", unchanged("f1"))]);

        let planned = plan(&input(&icloud, &fastmail, &state(), Side::ICloud));

        assert_eq!(render(&planned.plan), "! unreadable icloud /i/u1.vcf@i2: unsupported vCard version 4.0");
    }

    #[test]
    fn uid_change_at_same_href_is_held() {
        let icloud = snapshot([("/i/u1.vcf", fetched("i2", card("u4", "Jane Doe")))]);
        let fastmail = snapshot([("/f/u1.vcf", unchanged("f1"))]);

        let planned = plan(&input(&icloud, &fastmail, &state(), Side::ICloud));

        assert_eq!(render(&planned.plan), "! uid changed on icloud /i/u1.vcf@i2: u1 → u4");
        assert_eq!(planned.unsynced, Unsynced::default());
    }

    #[test]
    fn duplicate_uid_is_held() {
        let icloud = snapshot([("/i/u1.vcf", unchanged("i1")), ("/i/copy.vcf", fetched("c1", synced()))]);
        let fastmail = Snapshot::new();

        let planned = plan(&input(&icloud, &fastmail, &state(), Side::ICloud));

        assert_eq!(render(&planned.plan), "! duplicate uid=u1 on icloud: /i/copy.vcf, /i/u1.vcf");
    }

    #[test]
    fn held_uid_is_dropped_from_the_other_sides_unsynced() {
        // iCloud's u1 is duplicated (held); Fastmail's single u1 has no state
        // row. Fastmail's copy must not reach pairing, or pairing would
        // Create a third copy on iCloud.
        let icloud = snapshot([("/i/u1.vcf", fetched("i1", synced())), ("/i/copy.vcf", fetched("c1", synced()))]);
        let fastmail = snapshot([("/f/u1.vcf", fetched("f1", synced()))]);

        let planned = plan(&input(&icloud, &fastmail, &[], Side::ICloud));

        assert_eq!(planned.unsynced.fastmail, Vec::new());
        assert_eq!(render(&planned.plan), "! duplicate uid=u1 on icloud: /i/copy.vcf, /i/u1.vcf");
    }

    #[test]
    fn unsynced_cards_get_no_ops() {
        let icloud = snapshot([("/i/u1.vcf", unchanged("i1")), ("/i/u5.vcf", fetched("i5", card("u5", "Ann Lee")))]);
        let fastmail = snapshot([("/f/u1.vcf", unchanged("f1")), ("/f/u6.vcf", fetched("f6", card("u6", "Sam Poe")))]);

        let planned = plan(&input(&icloud, &fastmail, &state(), Side::ICloud));

        assert!(planned.plan.ops.is_empty(), "{}", planned.plan);
        assert_eq!(planned.unsynced.icloud.len(), 1);
        assert_eq!(planned.unsynced.icloud[0].card.uid().as_str(), "u5");
        assert_eq!(planned.unsynced.fastmail[0].card.uid().as_str(), "u6");

        // With no state at all, nothing is ever deleted either.
        let planned = plan(&input(&icloud, &fastmail, &[], Side::ICloud));
        assert!(planned.plan.ops.is_empty(), "{}", planned.plan);
    }

    #[test]
    fn moved_unreadable_card_is_never_deleted() {
        // iCloud's copy of u1 moved and, at its new href, fails to parse: the
        // planner cannot know it is still u1, so it must not delete the
        // Fastmail copy.
        let icloud = snapshot([(
            "/i/moved.vcf",
            Entry::Fetched {
                etag: ETag::from("m1"),
                card: Err(VCardError::MissingUid),
            },
        )]);
        let fastmail = snapshot([("/f/u1.vcf", unchanged("f1"))]);

        let planned = plan(&input(&icloud, &fastmail, &state(), Side::ICloud));

        assert_eq!(
            render(&planned.plan),
            "! unreadable icloud /i/moved.vcf@m1: vCard has no UID; ! deletion deferred on icloud uid=u1: unreadable card on that side"
        );
    }

    #[test]
    fn moved_held_card_is_never_deleted() {
        // Same blind spot with a Held entry at an unknown href: no card
        // failure diagnostic is emitted for the Held entry itself (it has its
        // own), but the row it might be hiding is still reported as deferred
        // rather than silently skipped.
        let icloud = snapshot([("/i/moved.vcf", Entry::Held(ETag::from("m1")))]);
        let fastmail = snapshot([("/f/u1.vcf", unchanged("f1"))]);

        let planned = plan(&input(&icloud, &fastmail, &state(), Side::ICloud));

        assert_eq!(render(&planned.plan), "! deletion deferred on icloud uid=u1: unreadable card on that side");
    }

    #[test]
    fn uncertain_side_does_not_block_unrelated_rows() {
        let state = vec![
            row(1, &card("u1", "Jane Doe"), ("/i/u1.vcf", "i1"), ("/f/u1.vcf", "f1")),
            row(2, &card("u2", "Bo"), ("/i/u2.vcf", "i1"), ("/f/u2.vcf", "f1")),
        ];
        // iCloud is uncertain (a Held card at an unknown href), but that must
        // not stop u2's own, unrelated update from being planned.
        let icloud = snapshot([
            ("/i/moved.vcf", Entry::Held(ETag::from("m1"))),
            ("/i/u2.vcf", fetched("i2", card_with("u2", "Bo", "NOTE:new\r\n"))),
        ]);
        let fastmail = snapshot([("/f/u1.vcf", unchanged("f1")), ("/f/u2.vcf", fetched("f1", card("u2", "Bo")))]);

        let planned = plan(&input(&icloud, &fastmail, &state, Side::ICloud));

        // u1's own row is skipped (iCloud is uncertain and u1 looks deleted
        // there) and reported, but u2 is still planned normally.
        assert_eq!(
            render(&planned.plan),
            "update fastmail uid=u2 /f/u2.vcf@f1; ! deletion deferred on icloud uid=u1: unreadable card on that side"
        );
    }

    #[test]
    fn ops_follow_state_order() {
        let state = vec![
            row(1, &card("u2", "Bo"), ("/i/u2.vcf", "i1"), ("/f/u2.vcf", "f1")),
            row(2, &card("u1", "Al"), ("/i/u1.vcf", "i1"), ("/f/u1.vcf", "f1")),
        ];

        let planned = plan(&input(&Snapshot::new(), &Snapshot::new(), &state, Side::ICloud));

        assert_eq!(render(&planned.plan), "forget uid=u2; forget uid=u1");
    }
}
