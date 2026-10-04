//! Applying one op: its server writes first, then its state write in one
//! transaction. A crash between them leaves state the next cycle's plan
//! converges from (echo suppression, `Adopt`, or pairing).

use chrono::{DateTime, Utc};

use super::{Collections, SyncService, href::mint_href};
use crate::{
    AddressBookError, Error,
    addressbook::Precondition,
    contact::{CANONICAL_VERSION, ETag, Href, Side, Uid, VCard},
    state::{ConflictOrigin, FailedCard, FailureOp, FailureReason, NewConflict, NewContactState, NewPendingRecreate, PhotoState, SideState},
    sync::{Op, Resource, SyncedCard},
    with_transaction,
};

/// The state write that completes an op.
#[derive(Debug)]
pub(super) struct StateWrite {
    pub(super) uid: Uid,
    pub(super) icloud: Option<Resource>,
    pub(super) fastmail: Option<Resource>,
    pub(super) synced: Option<SyncedCard>,
    /// Card failures this op resolves.
    pub(super) clear: Vec<(Side, Href)>,
    /// Completes a Recreate: removes its journal row in the same transaction.
    pub(super) recreated: bool,
    /// `None` leaves the row's photo state as it is.
    pub(super) photo: Option<PhotoState>,
}

impl StateWrite {
    /// A write naming `written` (the side the op wrote) and `source` (the
    /// other side).
    fn pushed(uid: &Uid, written: (Side, Resource), source: Resource, synced: &SyncedCard, clear: Vec<(Side, Href)>) -> Self {
        let (to, resource) = written;
        let (icloud, fastmail) = match to {
            Side::ICloud => (resource, source),
            Side::Fastmail => (source, resource),
        };
        Self {
            uid: uid.clone(),
            icloud: Some(icloud),
            fastmail: Some(fastmail),
            synced: Some(synced.clone()),
            clear,
            recreated: false,
            photo: None,
        }
    }
}

/// A card the daemon wrote this op, as soon as the server accepted it —
/// before its ETag is necessarily known (I2). Kept as `(side, href, etag)`
/// rather than a `Resource` since the ETag may still be `None`: a failure
/// recorded from this is held all the same, converging on the next cycle
/// once its group (`listing::held_hrefs`) is due.
pub(super) type Written = Vec<(Side, Href, Option<ETag>)>;

impl SyncService {
    /// Runs `op`. Every card it writes is pushed to `written` as soon as the
    /// server accepts it, so a later failure can hold it (Decision 5).
    pub(super) async fn execute(&self, op: &Op, collections: &Collections, now: DateTime<Utc>, written: &mut Written) -> Result<(), Error> {
        match op {
            Op::Create { uid, to, source, synced } | Op::Resurrect { uid, to, source, synced } => {
                let href = mint_href(&collections.get(*to).addressbook_url, uid);
                let resource = self.write_card(*to, href, synced.body(), Precondition::IfNoneMatch, written).await?;
                let clear = vec![(to.other(), source.href.clone())];
                self.write_state(StateWrite::pushed(uid, (*to, resource), source.clone(), synced, clear), now)
                    .await
            }
            Op::Update {
                uid,
                to,
                target,
                source,
                synced,
            } => {
                let precondition = Precondition::IfMatch(target.etag.clone());
                let resource = self.write_card(*to, target.href.clone(), synced.body(), precondition, written).await?;
                let clear = vec![(to.other(), source.href.clone())];
                self.write_state(StateWrite::pushed(uid, (*to, resource), source.clone(), synced, clear), now)
                    .await
            }
            Op::Delete { uid, on, target } => {
                self.book(*on).delete(&target.href, Some(&target.etag)).await?;
                self.drop_state(uid, vec![(*on, target.href.clone())]).await
            }
            Op::Conflict {
                uid,
                origin,
                winner,
                target,
                source,
                synced,
                icloud_card,
                fastmail_card,
            } => {
                self.record_conflict(NewConflict {
                    uid: uid.clone(),
                    origin: *origin,
                    winner: *winner,
                    icloud_vcard: icloud_card.as_bytes().to_vec(),
                    fastmail_vcard: fastmail_card.as_bytes().to_vec(),
                    detected_at: now,
                })
                .await?;
                let loser = winner.other();
                let precondition = Precondition::IfMatch(target.etag.clone());
                let resource = self.write_card(loser, target.href.clone(), synced.body(), precondition, written).await?;
                let clear = vec![(*winner, source.href.clone()), (loser, target.href.clone())];
                self.write_state(StateWrite::pushed(uid, (loser, resource), source.clone(), synced, clear), now)
                    .await
            }
            // Fastmail first (CG-14 triage F3): a failed iCloud create then
            // leaves an already-relinked group for the next cycle to copy.
            Op::CopyGroup {
                uid,
                source,
                rewritten,
                synced,
                ..
            } => {
                let precondition = Precondition::IfMatch(source.etag.clone());
                let fastmail_now = self.write_card(Side::Fastmail, source.href.clone(), rewritten, precondition, written).await?;
                let href = mint_href(&collections.icloud.addressbook_url, uid);
                let icloud_now = self.write_card(Side::ICloud, href, synced.body(), Precondition::IfNoneMatch, written).await?;
                let clear = vec![(Side::Fastmail, source.href.clone())];
                self.write_state(StateWrite::pushed(uid, (Side::ICloud, icloud_now), fastmail_now, synced, clear), now)
                    .await
            }
            // Op::Recreate's documented order, with the journal (CG-16)
            // before the DELETE.
            Op::Recreate {
                uid,
                icloud,
                old_fastmail,
                fastmail_uid,
                put_icloud,
                create_fastmail,
                synced,
                conflict,
                ..
            } => {
                if let Some(conflict) = conflict {
                    self.record_conflict(NewConflict {
                        uid: uid.clone(),
                        origin: ConflictOrigin::Baseline,
                        winner: conflict.winner,
                        icloud_vcard: conflict.icloud_card.as_bytes().to_vec(),
                        fastmail_vcard: conflict.fastmail_card.as_bytes().to_vec(),
                        detected_at: now,
                    })
                    .await?;
                }
                let icloud_now = match put_icloud {
                    Some(card) => {
                        let precondition = Precondition::IfMatch(icloud.etag.clone());
                        self.write_card(Side::ICloud, icloud.href.clone(), card, precondition, written).await?
                    }
                    None => icloud.clone(),
                };
                let new_href = mint_href(&collections.fastmail.addressbook_url, uid);
                self.journal_recreate(NewPendingRecreate {
                    uid: uid.clone(),
                    icloud_href: icloud.href.clone(),
                    old_fastmail_href: old_fastmail.href.clone(),
                    old_fastmail_uid: fastmail_uid.clone(),
                    new_fastmail_href: new_href.clone(),
                    card: create_fastmail.as_bytes().to_vec(),
                    created_at: now,
                })
                .await?;
                self.fastmail.delete(&old_fastmail.href, Some(&old_fastmail.etag)).await?;
                let fastmail_now = self
                    .write_card(Side::Fastmail, new_href, create_fastmail, Precondition::IfNoneMatch, written)
                    .await?;
                let write = StateWrite {
                    uid: uid.clone(),
                    icloud: Some(icloud_now),
                    fastmail: Some(fastmail_now),
                    synced: Some(synced.clone()),
                    clear: vec![(Side::ICloud, icloud.href.clone()), (Side::Fastmail, old_fastmail.href.clone())],
                    recreated: true,
                    photo: None,
                };
                self.write_state(write, now).await
            }
            Op::Adopt { uid, icloud, fastmail, synced } => {
                let write = StateWrite {
                    uid: uid.clone(),
                    icloud: Some(icloud.clone()),
                    fastmail: Some(fastmail.clone()),
                    synced: Some(synced.clone()),
                    clear: vec![(Side::ICloud, icloud.href.clone()), (Side::Fastmail, fastmail.href.clone())],
                    recreated: false,
                    photo: None,
                };
                self.write_state(write, now).await
            }
            Op::Refresh { uid, icloud, fastmail, synced } => {
                let write = StateWrite {
                    uid: uid.clone(),
                    icloud: icloud.clone(),
                    fastmail: fastmail.clone(),
                    synced: synced.clone(),
                    clear: Vec::new(),
                    recreated: false,
                    photo: None,
                };
                self.write_state(write, now).await
            }
            Op::Forget { uid } => self.drop_state(uid, Vec::new()).await,
        }
    }

    /// PUTs `card` at `href` and returns the resource with its new ETag.
    /// Pushed to `written` as soon as the PUT is accepted (I2) — even if the
    /// server sent no ETag and the follow-up fetch to recover one then
    /// fails, so the card the daemon just wrote is still held rather than
    /// left to surface as unattributed on the next listing.
    pub(super) async fn write_card(&self, side: Side, href: Href, card: &VCard, precondition: Precondition, written: &mut Written) -> Result<Resource, Error> {
        let book = self.book(side);
        let put_etag = book.put(&href, card.as_bytes(), precondition).await?;
        written.push((side, href.clone(), put_etag.clone()));
        let etag = match put_etag {
            Some(etag) => etag,
            None => book
                .multiget(std::slice::from_ref(&href))
                .await?
                .found
                .into_iter()
                .next()
                .map(|card| card.etag)
                .ok_or_else(|| AddressBookError::Permanent(format!("{href} was gone right after it was written")))?,
        };
        if let Some(entry) = written.iter_mut().find(|(s, h, _)| *s == side && *h == href) {
            entry.2 = Some(etag.clone());
        }
        Ok(Resource { href, etag })
    }

    /// Updates the contact's state row, or adds it when there is none (which
    /// then needs both sides and the card), and clears the op's failures.
    #[allow(
        clippy::single_match_else,
        reason = "the two arms differ enough (update vs. add, with a None-field guard) that if-let/else reads worse"
    )]
    pub(super) async fn write_state(&self, write: StateWrite, now: DateTime<Utc>) -> Result<(), Error> {
        with_transaction!(self, contact_state_repository, card_failure_repository, pending_recreate_repository, |tx| {
            let StateWrite {
                uid,
                icloud,
                fastmail,
                synced,
                clear,
                recreated,
                photo,
            } = write;
            let seen = |resource: Resource| SideState {
                href: resource.href,
                etag: resource.etag,
                last_seen_at: now,
            };
            if recreated {
                pending_recreate_repository.delete(tx, &uid).await?;
            }
            match contact_state_repository.find_by_uid(tx, &uid).await? {
                Some(mut row) => {
                    if let Some(resource) = icloud {
                        row.icloud = seen(resource);
                    }
                    if let Some(resource) = fastmail {
                        row.fastmail = seen(resource);
                    }
                    if let Some(synced) = synced {
                        row.content_hash = synced.content_hash;
                        row.hash_version = CANONICAL_VERSION;
                        row.last_synced_vcard = synced.card;
                        row.last_synced_at = now;
                    }
                    if let Some(photo) = photo {
                        row.photo = photo;
                    }
                    contact_state_repository.update(tx, row).await?;
                }
                None => {
                    let (Some(icloud), Some(fastmail), Some(synced)) = (icloud, fastmail, synced) else {
                        return Err(Error::Infrastructure(format!("no state row to update for uid={uid}")));
                    };
                    let new = NewContactState {
                        uid,
                        icloud: seen(icloud),
                        fastmail: seen(fastmail),
                        content_hash: synced.content_hash,
                        hash_version: CANONICAL_VERSION,
                        photo: photo.unwrap_or_default(),
                        last_synced_vcard: synced.card,
                        last_synced_at: now,
                    };
                    contact_state_repository.add(tx, new).await?;
                }
            }
            for (side, href) in &clear {
                card_failure_repository.clear(tx, *side, href).await?;
            }
            Ok(())
        })
    }

    /// Appends to the conflict history in its own transaction, before the
    /// winner is pushed, so the losing version survives any later failure
    /// (Decision 4).
    pub(super) async fn record_conflict(&self, conflict: NewConflict) -> Result<(), Error> {
        with_transaction!(self, conflict_repository, |tx| conflict_repository.add(tx, conflict).await.map(|_| ()))
    }

    /// Durably records a Recreate's Fastmail card before its old card is
    /// deleted (CG-8 Decision 11). If this fails, the DELETE never runs.
    async fn journal_recreate(&self, new: NewPendingRecreate) -> Result<(), Error> {
        with_transaction!(self, pending_recreate_repository, |tx| pending_recreate_repository
            .upsert(tx, new)
            .await
            .map(|_| ()))
    }

    /// Drops the contact's state row and clears the op's failures.
    pub(super) async fn drop_state(&self, uid: &Uid, clear: Vec<(Side, Href)>) -> Result<(), Error> {
        let uid = uid.clone();
        with_transaction!(self, contact_state_repository, card_failure_repository, |tx| {
            contact_state_repository.delete_by_uid(tx, &uid).await?;
            for (side, href) in &clear {
                card_failure_repository.clear(tx, *side, href).await?;
            }
            Ok(())
        })
    }
}

/// Errors that end the cycle rather than one card's op (Decision 6): the
/// whole account or the state store is affected.
pub(super) fn is_cycle_fatal(error: &Error) -> bool {
    error.is_transient() || matches!(error, Error::AddressBook(AddressBookError::Unauthorized))
}

/// Whether to hold `op`'s cards when a cycle-fatal error aborts it: only for
/// a synced contact's ops. Holding a card of an unsynced pair would make the
/// other look unique to its side, and pairing would copy it back.
pub(super) fn holds_on_abort(op: &Op) -> bool {
    matches!(
        op,
        Op::Update { .. }
            | Op::Delete { .. }
            | Op::Resurrect { .. }
            | Op::Conflict {
                origin: ConflictOrigin::Sync,
                ..
            }
    )
}

/// The cards to hold after `op` failed (Decision 5): its source; both cards
/// for an op pairing two unsynced cards; and every card it already wrote, at
/// its new ETag. State-only ops hold nothing.
///
/// Every card is recorded under `op.uid()` — including `Recreate`'s old
/// Fastmail card, which is `op.uid()`'s counterpart even though the card
/// itself still carries `fastmail_uid` in its own bytes — so the listing
/// snapshot (`listing::held_hrefs`) groups them as one unit and releases
/// them together (I1): a card released alone, while the rest of the op's
/// cards are still held, would look unique to its side and pairing would
/// copy it back, duplicating the contact.
pub(super) fn failed_cards(op: &Op, written: &[(Side, Href, Option<ETag>)], reason: FailureReason) -> Vec<FailedCard> {
    let failure_op = match op {
        Op::Create { .. } | Op::Resurrect { .. } | Op::CopyGroup { .. } => FailureOp::Create,
        Op::Delete { .. } => FailureOp::Delete,
        _ => FailureOp::Update,
    };
    let uid = op.uid();
    let card = |side: Side, href: &Href, etag: Option<ETag>| FailedCard {
        side,
        href: href.clone(),
        uid: Some(uid.clone()),
        op: failure_op,
        etag,
        reason,
    };
    let mut cards = match op {
        Op::Create { to, source, .. } | Op::Resurrect { to, source, .. } | Op::Update { to, source, .. } => {
            vec![card(to.other(), &source.href, Some(source.etag.clone()))]
        }
        Op::CopyGroup { source, .. } => vec![card(Side::Fastmail, &source.href, Some(source.etag.clone()))],
        Op::Delete { on, target, .. } => vec![card(*on, &target.href, Some(target.etag.clone()))],
        Op::Conflict {
            origin,
            winner,
            target,
            source,
            ..
        } => {
            let mut cards = vec![card(*winner, &source.href, Some(source.etag.clone()))];
            if *origin == ConflictOrigin::Baseline {
                cards.push(card(winner.other(), &target.href, Some(target.etag.clone())));
            }
            cards
        }
        Op::Recreate { icloud, old_fastmail, .. } => vec![
            card(Side::ICloud, &icloud.href, Some(icloud.etag.clone())),
            card(Side::Fastmail, &old_fastmail.href, Some(old_fastmail.etag.clone())),
        ],
        Op::Adopt { .. } | Op::Refresh { .. } | Op::Forget { .. } => return Vec::new(),
    };
    // A card already written is held at its new ETag (or with no ETag when
    // the server omitted one and it could not be recovered — I2; its group
    // still holds it). At the old ETag its next listing would read as an
    // edit and retry at once.
    for (side, href, etag) in written {
        match cards.iter_mut().find(|held| held.side == *side && held.href == *href) {
            Some(held) => held.etag.clone_from(etag),
            None => cards.push(card(*side, href, etag.clone())),
        }
    }
    cards
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::PairPass;

    fn res(href: &str, etag: &str) -> Resource {
        Resource {
            href: Href::from(href),
            etag: ETag::from(etag),
        }
    }

    fn written(side: Side, href: &str, etag: &str) -> (Side, Href, Option<ETag>) {
        (side, Href::from(href), Some(ETag::from(etag)))
    }

    fn synced() -> SyncedCard {
        let card = VCard::parse("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:u1\r\nFN:Jane Doe\r\nEND:VCARD\r\n").unwrap();
        SyncedCard::recorded(&card)
    }

    fn keys(cards: &[FailedCard]) -> Vec<(Side, &str, Option<&str>)> {
        cards
            .iter()
            .map(|card| (card.side, card.href.as_str(), card.etag.as_ref().map(ETag::as_str)))
            .collect()
    }

    #[test]
    fn a_failed_group_copy_holds_the_fastmail_group_at_its_newest_etag() {
        let op = Op::CopyGroup {
            uid: Uid::from("g1"),
            source: res("/dav/g.vcf", "f1"),
            rewritten: synced().card,
            synced: synced(),
            relinked: 1,
        };

        let before_write = failed_cards(&op, &[], FailureReason::Rejected);
        assert_eq!(keys(&before_write), [(Side::Fastmail, "/dav/g.vcf", Some("f1"))]);
        assert_eq!(before_write[0].op, FailureOp::Create);

        let after_fastmail = failed_cards(&op, &[written(Side::Fastmail, "/dav/g.vcf", "f2")], FailureReason::Rejected);
        assert_eq!(keys(&after_fastmail), [(Side::Fastmail, "/dav/g.vcf", Some("f2"))]);
        assert!(!holds_on_abort(&op), "an unsynced group is never held on abort");
    }

    #[test]
    fn a_failed_create_holds_its_source_and_any_card_it_wrote() {
        let op = Op::Create {
            uid: Uid::from("u1"),
            to: Side::Fastmail,
            source: res("/card/a.vcf", "i1"),
            synced: synced(),
        };

        let before_write = failed_cards(&op, &[], FailureReason::Rejected);
        assert_eq!(keys(&before_write), [(Side::ICloud, "/card/a.vcf", Some("i1"))]);
        assert_eq!(before_write[0].op, FailureOp::Create);
        assert_eq!(before_write[0].uid, Some(Uid::from("u1")));

        let after_write = failed_cards(&op, &[written(Side::Fastmail, "/dav/new.vcf", "f1")], FailureReason::Internal);
        assert_eq!(
            keys(&after_write),
            [(Side::ICloud, "/card/a.vcf", Some("i1")), (Side::Fastmail, "/dav/new.vcf", Some("f1"))]
        );
        assert!(
            after_write.iter().all(|card| card.uid == Some(Uid::from("u1"))),
            "one op's cards share its uid (I1)"
        );
    }

    #[test]
    #[allow(
        clippy::assert_is_empty,
        reason = "asserting on is_empty() reads clearer than assert_eq! against an empty array literal"
    )]
    fn a_written_card_is_held_at_its_new_etag_and_state_only_ops_hold_nothing() {
        let op = Op::Update {
            uid: Uid::from("u1"),
            to: Side::ICloud,
            target: res("/card/a.vcf", "i1"),
            source: res("/dav/a.vcf", "f2"),
            synced: synced(),
        };
        let cards = failed_cards(&op, &[written(Side::ICloud, "/card/a.vcf", "i2")], FailureReason::Internal);
        assert_eq!(
            keys(&cards),
            [(Side::Fastmail, "/dav/a.vcf", Some("f2")), (Side::ICloud, "/card/a.vcf", Some("i2"))]
        );

        assert!(failed_cards(&Op::Forget { uid: Uid::from("u1") }, &[], FailureReason::Internal).is_empty());
    }

    #[test]
    fn a_written_card_with_no_etag_is_still_held() {
        let op = Op::Create {
            uid: Uid::from("u1"),
            to: Side::Fastmail,
            source: res("/card/a.vcf", "i1"),
            synced: synced(),
        };
        let cards = failed_cards(&op, &[(Side::Fastmail, Href::from("/dav/new.vcf"), None)], FailureReason::Internal);
        assert_eq!(
            keys(&cards),
            [(Side::ICloud, "/card/a.vcf", Some("i1")), (Side::Fastmail, "/dav/new.vcf", None)],
            "I2: a PUT accepted with no ETag, whose fallback fetch also failed, is still recorded and held"
        );
    }

    #[test]
    fn recreates_old_fastmail_card_is_recorded_under_the_ops_uid() {
        let op = Op::Recreate {
            uid: Uid::from("ic-1"),
            pass: PairPass::Content,
            icloud: res("/i/ic-1.vcf", "i1"),
            old_fastmail: res("/f/fm-1.vcf", "f1"),
            fastmail_uid: Uid::from("fm-1"),
            put_icloud: None,
            create_fastmail: synced().card,
            synced: synced(),
            conflict: None,
        };

        let cards = failed_cards(&op, &[], FailureReason::Internal);

        assert_eq!(
            keys(&cards),
            [(Side::ICloud, "/i/ic-1.vcf", Some("i1")), (Side::Fastmail, "/f/fm-1.vcf", Some("f1"))]
        );
        assert!(
            cards.iter().all(|card| card.uid == Some(Uid::from("ic-1"))),
            "both of a Recreate's cards are grouped under the op's (iCloud) uid, not fastmail_uid (I1): {cards:?}"
        );
    }

    #[test]
    fn only_account_wide_errors_are_fatal() {
        assert!(is_cycle_fatal(&AddressBookError::RateLimited { retry_after: None }.into()));
        assert!(is_cycle_fatal(&AddressBookError::Transient("503".into()).into()));
        assert!(is_cycle_fatal(&AddressBookError::Unauthorized.into()));
        assert!(!is_cycle_fatal(&AddressBookError::Permanent("400".into()).into()));
        assert!(!is_cycle_fatal(&AddressBookError::PreconditionFailed { href: Href::from("/a") }.into()));
    }

    #[test]
    fn holds_on_abort_only_for_a_synced_contacts_ops() {
        let uid = Uid::from("u1");
        let held = [
            Op::Update {
                uid: uid.clone(),
                to: Side::ICloud,
                target: res("/a", "i1"),
                source: res("/b", "f1"),
                synced: synced(),
            },
            Op::Delete {
                uid: uid.clone(),
                on: Side::ICloud,
                target: res("/a", "i1"),
            },
            Op::Resurrect {
                uid: uid.clone(),
                to: Side::ICloud,
                source: res("/b", "f1"),
                synced: synced(),
            },
            Op::Conflict {
                uid: uid.clone(),
                origin: ConflictOrigin::Sync,
                winner: Side::ICloud,
                target: res("/a", "i1"),
                source: res("/b", "f1"),
                synced: synced(),
                icloud_card: synced().card,
                fastmail_card: synced().card,
            },
        ];
        for op in &held {
            assert!(holds_on_abort(op), "{op:?}");
        }

        let not_held = [
            Op::Create {
                uid: uid.clone(),
                to: Side::Fastmail,
                source: res("/a", "i1"),
                synced: synced(),
            },
            Op::Conflict {
                uid: uid.clone(),
                origin: ConflictOrigin::Baseline,
                winner: Side::ICloud,
                target: res("/a", "i1"),
                source: res("/b", "f1"),
                synced: synced(),
                icloud_card: synced().card,
                fastmail_card: synced().card,
            },
            Op::Recreate {
                uid: uid.clone(),
                pass: PairPass::Identity,
                icloud: res("/a", "i1"),
                old_fastmail: res("/b", "f1"),
                fastmail_uid: Uid::from("fm1"),
                put_icloud: None,
                create_fastmail: synced().card,
                synced: synced(),
                conflict: None,
            },
            Op::Adopt {
                uid: uid.clone(),
                icloud: res("/a", "i1"),
                fastmail: res("/b", "f1"),
                synced: synced(),
            },
            Op::Refresh {
                uid: uid.clone(),
                icloud: None,
                fastmail: None,
                synced: None,
            },
            Op::Forget { uid },
        ];
        for op in &not_held {
            assert!(!holds_on_abort(op), "{op:?}");
        }
    }
}
