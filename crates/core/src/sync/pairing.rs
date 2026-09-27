use std::collections::{HashMap, HashSet};

use super::{Op, PairPass, RecreateConflict, Resource, SYNC_HASH, SyncedCard, Unsynced, UnsyncedCard};
use crate::{
    contact::{CardHash, ConflictWinner, DisplayIdentity, HashOptions, MatchKeys, Side, Uid},
    state::ConflictOrigin,
};

/// A card pairing leaves alone this cycle: it may be the same person as a
/// card on the other side, but not certainly. Re-evaluated every cycle; CG-8
/// persists the latest set in `baseline_skips`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skip {
    pub side: Side,
    pub resource: Resource,
    pub uid: Uid,
    /// `canonical_hash(SYNC_HASH)` when skipped (informational).
    pub content_hash: CardHash,
    /// 0: same name on the other side but nothing shared. 2 or more: several
    /// possible matches (the card's own, or its only match's).
    pub candidate_count: u32,
    pub identity: DisplayIdentity,
    /// The other side's unsynced cards with the same name.
    pub candidates: Vec<DisplayIdentity>,
}

/// What pairing decided this cycle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Paired {
    pub ops: Vec<Op>,
    pub skips: Vec<Skip>,
}

/// Pairs both sides' unsynced cards: pass 1 (same UID), pass 2 (same
/// content), pass 3 (identity heuristic); then copies unique cards and skips
/// ambiguous ones. Runs every cycle; on an empty state store this is the whole
/// baseline.
pub fn pair(unsynced: &Unsynced, winner: ConflictWinner) -> Paired {
    let mut pairing = Pairing {
        icloud: unsynced.icloud.iter().collect(),
        fastmail: unsynced.fastmail.iter().collect(),
        winner,
        ops: Vec::new(),
    };
    pairing.by_uid();
    pairing.by_content();
    pairing.by_identity();

    let mut paired = Paired {
        ops: pairing.ops,
        skips: Vec::new(),
    };
    let settled =
        settle_side(Side::ICloud, &pairing.icloud, &pairing.fastmail)
            .into_iter()
            .chain(settle_side(Side::Fastmail, &pairing.fastmail, &pairing.icloud));
    for outcome in settled {
        match outcome {
            Settled::Copy(op) => paired.ops.push(*op),
            Settled::Skip(skip) => paired.skips.push(skip),
        }
    }
    paired
}

enum Settled {
    Copy(Box<Op>),
    Skip(Skip),
}

/// The cards still unpaired on each side, and the ops decided so far.
struct Pairing<'a> {
    icloud: Vec<&'a UnsyncedCard>,
    fastmail: Vec<&'a UnsyncedCard>,
    winner: ConflictWinner,
    ops: Vec<Op>,
}

impl Pairing<'_> {
    /// Pass 1: the same UID on both sides.
    fn by_uid(&mut self) {
        let fastmail: HashMap<&Uid, usize> = self.fastmail.iter().enumerate().map(|(f, c)| (c.card.uid(), f)).collect();
        let pairs: Vec<(usize, usize)> = self
            .icloud
            .iter()
            .enumerate()
            .filter_map(|(i, c)| fastmail.get(c.card.uid()).map(|&f| (i, f)))
            .collect();
        for &(i, f) in &pairs {
            let op = self.uid_pair(self.icloud[i], self.fastmail[f]);
            self.ops.push(op);
        }
        self.remove(&pairs);
    }

    /// Photos never count (Decision 12).
    fn uid_pair(&self, icloud: &UnsyncedCard, fastmail: &UnsyncedCard) -> Op {
        let uid = icloud.card.uid().clone();
        if icloud.card.canonical_hash(SYNC_HASH) == fastmail.card.canonical_hash(SYNC_HASH) {
            return Op::Adopt {
                uid,
                icloud: icloud.resource.clone(),
                fastmail: fastmail.resource.clone(),
                synced: SyncedCard::recorded(&icloud.card),
            };
        }
        let (source, target) = match self.winner {
            Side::ICloud => (icloud, fastmail),
            Side::Fastmail => (fastmail, icloud),
        };
        Op::Conflict {
            uid,
            origin: ConflictOrigin::Baseline,
            winner: self.winner,
            target: target.resource.clone(),
            source: source.resource.clone(),
            synced: SyncedCard::for_push(&source.card, Some(&target.card)),
            icloud_card: icloud.card.clone(),
            fastmail_card: fastmail.card.clone(),
        }
    }

    /// Pass 2: the same content under different UIDs (UID and photos
    /// excluded), unique both ways.
    fn by_content(&mut self) {
        let without_uid = HashOptions {
            exclude_uid: true,
            ..SYNC_HASH
        };
        let mut buckets: HashMap<CardHash, Vec<usize>> = HashMap::new();
        for (f, c) in self.fastmail.iter().enumerate() {
            buckets.entry(c.card.canonical_hash(without_uid)).or_default().push(f);
        }
        let candidates: Vec<Vec<usize>> = self
            .icloud
            .iter()
            .map(|i| buckets.get(&i.card.canonical_hash(without_uid)).cloned().unwrap_or_default())
            .collect();
        let pairs = mutual_pairs(&candidates, self.fastmail.len());
        for &(i, f) in &pairs {
            let op = Self::content_pair(self.icloud[i], self.fastmail[f]);
            self.ops.push(op);
        }
        self.remove(&pairs);
    }

    /// Pass 3: the identity heuristic, unique both ways.
    fn by_identity(&mut self) {
        let candidates = identity_candidates(&self.icloud, &self.fastmail);
        let pairs = mutual_pairs(&candidates, self.fastmail.len());
        for &(i, f) in &pairs {
            let op = self.identity_pair(self.icloud[i], self.fastmail[f]);
            self.ops.push(op);
        }
        self.remove(&pairs);
    }

    /// Decision 4: the winner's content under the iCloud UID; the conflict is
    /// recorded first.
    fn identity_pair(&self, icloud: &UnsyncedCard, fastmail: &UnsyncedCard) -> Op {
        let uid = icloud.card.uid().clone();
        let conflict = Some(RecreateConflict {
            winner: self.winner,
            icloud_card: icloud.card.clone(),
            fastmail_card: fastmail.card.clone(),
        });
        // Each overwritten or recreated card keeps its own photos (Decision
        // 12).
        let (put_icloud, create_fastmail, synced) = match self.winner {
            Side::ICloud => {
                let synced = SyncedCard::recorded(&icloud.card);
                (None, synced.card.with_photos_of(&fastmail.card), synced)
            }
            Side::Fastmail => {
                let winning = fastmail.card.with_uid(&uid);
                let synced = SyncedCard::for_push(&winning, Some(&icloud.card));
                (Some(synced.body().clone()), winning, synced)
            }
        };
        Op::Recreate {
            uid,
            pass: PairPass::Identity,
            icloud: icloud.resource.clone(),
            old_fastmail: fastmail.resource.clone(),
            fastmail_uid: fastmail.card.uid().clone(),
            put_icloud,
            create_fastmail,
            synced,
            conflict,
        }
    }

    /// Fastmail keeps its own bytes, photo included; only the UID changes.
    fn content_pair(icloud: &UnsyncedCard, fastmail: &UnsyncedCard) -> Op {
        let uid = icloud.card.uid().clone();
        Op::Recreate {
            uid: uid.clone(),
            pass: PairPass::Content,
            icloud: icloud.resource.clone(),
            old_fastmail: fastmail.resource.clone(),
            fastmail_uid: fastmail.card.uid().clone(),
            put_icloud: None,
            create_fastmail: fastmail.card.with_uid(&uid),
            synced: SyncedCard::recorded(&icloud.card),
            conflict: None,
        }
    }

    /// Drops paired cards from both sides.
    fn remove(&mut self, pairs: &[(usize, usize)]) {
        let (icloud, fastmail): (HashSet<usize>, HashSet<usize>) = pairs.iter().copied().unzip();
        self.icloud = self.icloud.iter().enumerate().filter(|(i, _)| !icloud.contains(i)).map(|(_, c)| *c).collect();
        self.fastmail = self
            .fastmail
            .iter()
            .enumerate()
            .filter(|(f, _)| !fastmail.contains(f))
            .map(|(_, c)| *c)
            .collect();
    }
}

/// `(i, f)` for each iCloud card `i` whose only candidate is `f` while `i` is
/// also `f`'s only claimant: unique in both directions (F3).
fn mutual_pairs(candidates: &[Vec<usize>], fastmail_len: usize) -> Vec<(usize, usize)> {
    let mut claims = vec![0usize; fastmail_len];
    for list in candidates {
        for &f in list {
            claims[f] += 1;
        }
    }
    candidates
        .iter()
        .enumerate()
        .filter_map(|(i, list)| match list.as_slice() {
            [f] if claims[*f] == 1 => Some((i, *f)),
            _ => None,
        })
        .collect()
}

/// After the passes (Decision 6): unique → copy; ambiguous → skip.
fn settle_side(side: Side, cards: &[&UnsyncedCard], other: &[&UnsyncedCard]) -> Vec<Settled> {
    let own = identity_candidates(cards, other);
    let reverse = identity_candidates(other, cards);
    let other_keys: Vec<MatchKeys> = other.iter().map(|c| c.card.match_keys()).collect();
    let other_names = name_buckets(&other_keys);

    cards
        .iter()
        .zip(&own)
        .map(|(card, candidates)| {
            let keys = card.card.match_keys();
            let same_name = keys.name_key().and_then(|name| other_names.get(name)).map_or(&[][..], Vec::as_slice);
            if same_name.is_empty() {
                return Settled::Copy(Box::new(Op::Create {
                    uid: card.card.uid().clone(),
                    to: side.other(),
                    source: card.resource.clone(),
                    synced: SyncedCard::for_push(&card.card, None),
                }));
            }
            let count = match candidates.as_slice() {
                // Its only match is contested: report the match's count.
                [only] => reverse[*only].len(),
                list => list.len(),
            };
            Settled::Skip(Skip {
                side,
                resource: card.resource.clone(),
                uid: card.card.uid().clone(),
                content_hash: card.card.canonical_hash(SYNC_HASH),
                candidate_count: u32::try_from(count).unwrap_or(u32::MAX),
                identity: card.card.display_identity(),
                candidates: same_name.iter().map(|&o| other[o].card.display_identity()).collect(),
            })
        })
        .collect()
}

/// For each card in `from`, the `to` cards it matches by identity.
fn identity_candidates(from: &[&UnsyncedCard], to: &[&UnsyncedCard]) -> Vec<Vec<usize>> {
    let to_keys: Vec<MatchKeys> = to.iter().map(|c| c.card.match_keys()).collect();
    let by_name = name_buckets(&to_keys);
    from.iter()
        .map(|c| {
            let keys = c.card.match_keys();
            keys.name_key()
                .and_then(|name| by_name.get(name))
                .into_iter()
                .flatten()
                .copied()
                .filter(|&t| keys.is_match(&to_keys[t]))
                .collect()
        })
        .collect()
}

/// Card indices by normalized name; nameless cards are left out.
fn name_buckets(keys: &[MatchKeys]) -> HashMap<&str, Vec<usize>> {
    let mut buckets: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, key) in keys.iter().enumerate() {
        if let Some(name) = key.name_key() {
            buckets.entry(name).or_default().push(index);
        }
    }
    buckets
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        contact::VCard,
        sync::fixtures::{EMBEDDED_PHOTO, URI_PHOTO, card, card_with, on_fastmail, on_icloud},
    };

    fn run(icloud: Vec<VCard>, fastmail: Vec<VCard>, winner: Side) -> Paired {
        let unsynced = Unsynced {
            icloud: icloud.into_iter().map(on_icloud).collect(),
            fastmail: fastmail.into_iter().map(on_fastmail).collect(),
        };
        pair(&unsynced, winner)
    }

    fn render(ops: &[Op]) -> String {
        ops.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")
    }

    const REWRITTEN: &str = "REV:2026-09-26T00:00:00Z\r\n";

    #[test]
    fn same_uid_same_content_is_adopted() {
        let icloud = card("u1", "Jane Doe");

        let paired = run(vec![icloud.clone()], vec![card_with("u1", "Jane Doe", REWRITTEN)], Side::ICloud);

        assert_eq!(render(&paired.ops), "adopt uid=u1 icloud=/i/u1.vcf@i-u1 fastmail=/f/u1.vcf@f-u1");
        match &*paired.ops {
            [Op::Adopt { synced, .. }] => assert_eq!(*synced, SyncedCard::recorded(&icloud)),
            other => panic!("expected one adopt, got {other:?}"),
        }
    }

    #[test]
    fn same_uid_different_content_is_a_baseline_conflict() {
        let icloud = card_with("u1", "Jane Doe", "NOTE:icloud\r\n");
        let fastmail = card_with("u1", "Jane Doe", "NOTE:fastmail\r\n");

        let paired = run(vec![icloud.clone()], vec![fastmail.clone()], Side::ICloud);
        assert_eq!(render(&paired.ops), "conflict(baseline) icloud wins uid=u1 → fastmail /f/u1.vcf@f-u1");

        let paired = run(vec![icloud.clone()], vec![fastmail.clone()], Side::Fastmail);
        assert_eq!(render(&paired.ops), "conflict(baseline) fastmail wins uid=u1 → icloud /i/u1.vcf@i-u1");
        match &*paired.ops {
            [
                Op::Conflict {
                    origin,
                    synced,
                    icloud_card,
                    fastmail_card,
                    ..
                },
            ] => {
                assert_eq!(*origin, ConflictOrigin::Baseline);
                assert_eq!(*synced, SyncedCard::for_push(&fastmail, Some(&icloud)));
                assert_eq!(*icloud_card, icloud);
                assert_eq!(*fastmail_card, fastmail);
            }
            other => panic!("expected one conflict, got {other:?}"),
        }
    }

    #[test]
    fn same_uid_with_different_photos_is_adopted() {
        let icloud = card_with("u1", "Jane Doe", URI_PHOTO);

        let paired = run(vec![icloud.clone()], vec![card_with("u1", "Jane Doe", EMBEDDED_PHOTO)], Side::ICloud);

        assert_eq!(render(&paired.ops), "adopt uid=u1 icloud=/i/u1.vcf@i-u1 fastmail=/f/u1.vcf@f-u1");
        match &*paired.ops {
            [Op::Adopt { synced, .. }] => assert_eq!(synced.card, card("u1", "Jane Doe")),
            other => panic!("expected one adopt, got {other:?}"),
        }
    }

    #[test]
    fn baseline_conflict_keeps_the_losers_photo() {
        let icloud = card_with("u1", "Jane Doe", &format!("NOTE:icloud\r\n{URI_PHOTO}"));
        let fastmail = card_with("u1", "Jane Doe", &format!("NOTE:fastmail\r\n{EMBEDDED_PHOTO}"));

        let paired = run(vec![icloud], vec![fastmail.clone()], Side::ICloud);

        assert_eq!(
            render(&paired.ops),
            "conflict(baseline) icloud wins uid=u1 → fastmail /f/u1.vcf@f-u1 photo-kept"
        );
        let winner = card_with("u1", "Jane Doe", "NOTE:icloud\r\n");
        match &*paired.ops {
            [Op::Conflict { synced, .. }] => assert_eq!(synced.body(), &winner.with_photos_of(&fastmail)),
            other => panic!("expected one conflict, got {other:?}"),
        }
    }

    #[test]
    fn same_content_under_different_uids_recreates_the_fastmail_card() {
        let icloud = card_with("ic-1", "Ann Lee", "EMAIL:ann@example.com\r\n");
        let fastmail = card_with("fm-1", "Ann Lee", "EMAIL:ann@example.com\r\n");

        let paired = run(vec![icloud.clone()], vec![fastmail.clone()], Side::ICloud);

        assert_eq!(render(&paired.ops), "recreate(content) uid=ic-1 fastmail /f/fm-1.vcf@f-fm-1 was fm-1");
        match &*paired.ops {
            [
                Op::Recreate {
                    put_icloud,
                    create_fastmail,
                    synced,
                    conflict,
                    ..
                },
            ] => {
                assert_eq!(*put_icloud, None);
                assert_eq!(*create_fastmail, fastmail.with_uid(&Uid::from("ic-1")));
                assert_eq!(*synced, SyncedCard::recorded(&icloud));
                assert_eq!(*conflict, None);
            }
            other => panic!("expected one recreate, got {other:?}"),
        }
    }

    #[test]
    fn content_pairing_must_be_mutual() {
        let body = "EMAIL:ann@example.com\r\n";
        let paired = run(
            vec![card_with("ic-1", "Ann Lee", body), card_with("ic-2", "Ann Lee", body)],
            vec![card_with("fm-1", "Ann Lee", body)],
            Side::ICloud,
        );

        assert!(paired.ops.is_empty(), "{}", render(&paired.ops));
    }

    #[test]
    fn photo_difference_still_pairs_by_content() {
        let icloud = card_with("ic-1", "Ann Lee", &format!("EMAIL:ann@example.com\r\n{URI_PHOTO}"));
        let fastmail = card_with("fm-1", "Ann Lee", &format!("EMAIL:ann@example.com\r\n{EMBEDDED_PHOTO}"));

        let paired = run(vec![icloud], vec![fastmail.clone()], Side::ICloud);

        assert_eq!(render(&paired.ops), "recreate(content) uid=ic-1 fastmail /f/fm-1.vcf@f-fm-1 was fm-1");
        match &*paired.ops {
            // Fastmail's own bytes, photo included, under the iCloud UID.
            [Op::Recreate { create_fastmail, .. }] => assert_eq!(*create_fastmail, fastmail.with_uid(&Uid::from("ic-1"))),
            other => panic!("expected one recreate, got {other:?}"),
        }
    }

    const JANE: &str = "EMAIL:jane@example.com\r\n";

    #[test]
    fn identity_pair_with_icloud_winning() {
        let icloud = card_with("ic-1", "Jane Doe", &format!("{JANE}NOTE:icloud\r\n{URI_PHOTO}"));
        let fastmail = card_with("fm-1", "Jane Doe", &format!("{JANE}NOTE:fastmail\r\n{EMBEDDED_PHOTO}"));

        let paired = run(vec![icloud.clone()], vec![fastmail.clone()], Side::ICloud);

        assert_eq!(
            render(&paired.ops),
            "recreate(identity) uid=ic-1 fastmail /f/fm-1.vcf@f-fm-1 was fm-1 icloud wins"
        );
        match &*paired.ops {
            [
                Op::Recreate {
                    put_icloud,
                    create_fastmail,
                    synced,
                    conflict,
                    ..
                },
            ] => {
                assert_eq!(*put_icloud, None);
                // iCloud's content without its photo, plus Fastmail's own
                // photo.
                assert_eq!(*create_fastmail, icloud.without_photos().with_photos_of(&fastmail));
                assert_eq!(*synced, SyncedCard::recorded(&icloud));
                assert_eq!(
                    *conflict,
                    Some(RecreateConflict {
                        winner: Side::ICloud,
                        icloud_card: icloud,
                        fastmail_card: fastmail
                    })
                );
            }
            other => panic!("expected one recreate, got {other:?}"),
        }
    }

    #[test]
    fn identity_pair_with_fastmail_winning() {
        let icloud = card_with("ic-1", "Jane Doe", &format!("{JANE}NOTE:icloud\r\n{URI_PHOTO}"));
        let fastmail = card_with("fm-1", "Jane Doe", &format!("{JANE}NOTE:fastmail\r\n{EMBEDDED_PHOTO}"));

        let paired = run(vec![icloud.clone()], vec![fastmail.clone()], Side::Fastmail);

        assert_eq!(
            render(&paired.ops),
            "recreate(identity) uid=ic-1 fastmail /f/fm-1.vcf@f-fm-1 was fm-1 put icloud /i/ic-1.vcf@i-ic-1 fastmail wins"
        );
        let winning = fastmail.with_uid(&Uid::from("ic-1"));
        match &*paired.ops {
            [
                Op::Recreate {
                    put_icloud,
                    create_fastmail,
                    synced,
                    ..
                },
            ] => {
                // iCloud gets the winning content with its own photo kept;
                // Fastmail keeps its own bytes, photo included.
                assert_eq!(*put_icloud, Some(winning.without_photos().with_photos_of(&icloud)));
                assert_eq!(*create_fastmail, winning);
                assert_eq!(*synced, SyncedCard::for_push(&winning, Some(&icloud)));
            }
            other => panic!("expected one recreate, got {other:?}"),
        }
    }

    #[test]
    fn identity_ambiguity_skips_every_card_involved() {
        let paired = run(
            vec![
                card_with("ic-1", "Kim Wu", &format!("{JANE}NOTE:one\r\n")),
                card_with("ic-2", "Kim Wu", &format!("{JANE}NOTE:two\r\n")),
            ],
            vec![card_with("fm-1", "Kim Wu", &format!("{JANE}NOTE:three\r\n"))],
            Side::ICloud,
        );

        assert!(paired.ops.is_empty(), "{}", render(&paired.ops));
        let counts: Vec<(Side, &str, u32)> = paired.skips.iter().map(|s| (s.side, s.uid.as_str(), s.candidate_count)).collect();
        assert_eq!(counts, [(Side::ICloud, "ic-1", 2), (Side::ICloud, "ic-2", 2), (Side::Fastmail, "fm-1", 2)]);
    }

    #[test]
    fn name_collision_is_skipped_with_zero_candidates() {
        let paired = run(
            vec![card_with("ic-1", "Sam Poe", "EMAIL:sam@one.example\r\n")],
            vec![card_with("fm-1", "Sam Poe", "EMAIL:sam@two.example\r\n")],
            Side::ICloud,
        );

        assert!(paired.ops.is_empty(), "{}", render(&paired.ops));
        let counts: Vec<(Side, u32)> = paired.skips.iter().map(|s| (s.side, s.candidate_count)).collect();
        assert_eq!(counts, [(Side::ICloud, 0), (Side::Fastmail, 0)]);
    }

    #[test]
    fn skip_names_the_card_and_its_same_name_candidates() {
        let icloud = card_with("ic-1", "Sam Poe", "EMAIL:sam@one.example\r\nORG:Acme\r\n");
        let fastmail = card_with("fm-1", "Sam Poe", "EMAIL:sam@two.example\r\nORG:Globex\r\n");

        let paired = run(vec![icloud.clone()], vec![fastmail.clone()], Side::ICloud);

        assert_eq!(
            paired.skips[0],
            Skip {
                side: Side::ICloud,
                resource: crate::sync::fixtures::res("/i/ic-1.vcf", "i-ic-1"),
                uid: Uid::from("ic-1"),
                content_hash: icloud.canonical_hash(SYNC_HASH),
                candidate_count: 0,
                identity: icloud.display_identity(),
                candidates: vec![fastmail.display_identity()],
            }
        );
    }

    #[test]
    fn unique_cards_are_copied() {
        let paired = run(vec![card("ic-1", "Ann Lee")], vec![card("fm-1", "Bo Ray")], Side::ICloud);

        assert_eq!(
            render(&paired.ops),
            "create fastmail uid=ic-1 from=/i/ic-1.vcf\ncreate icloud uid=fm-1 from=/f/fm-1.vcf"
        );
        assert_eq!(paired.skips, []);
    }

    #[test]
    fn nameless_cards_are_copied() {
        // An email address used as FN is treated as no name.
        // The bodies differ so the cards can't pair by content in pass 2.
        let paired = run(
            vec![card_with("ic-1", "jane@example.com", "NOTE:one\r\n")],
            vec![card_with("fm-1", "jane@example.com", "NOTE:two\r\n")],
            Side::ICloud,
        );

        assert_eq!(
            render(&paired.ops),
            "create fastmail uid=ic-1 from=/i/ic-1.vcf\ncreate icloud uid=fm-1 from=/f/fm-1.vcf"
        );
    }

    #[test]
    fn copies_carry_no_photo() {
        let paired = run(vec![card_with("ic-1", "Ann Lee", URI_PHOTO)], vec![], Side::ICloud);

        assert_eq!(render(&paired.ops), "create fastmail uid=ic-1 from=/i/ic-1.vcf");
        match &*paired.ops {
            [Op::Create { synced, .. }] => assert_eq!(synced.body(), &card("ic-1", "Ann Lee")),
            other => panic!("expected one create, got {other:?}"),
        }
    }

    #[test]
    fn cards_paired_earlier_never_count_as_candidates() {
        // ic `u1` pairs by UID with fm `u1` (a conflict); fm `fm-2` has
        // ic u1's exact content but must not be compared with it again.
        let icloud = card_with("u1", "Jane Doe", "NOTE:x\r\n");
        let paired = run(
            vec![icloud],
            vec![card_with("u1", "Jane Doe", "NOTE:y\r\n"), card_with("fm-2", "Jane Doe", "NOTE:x\r\n")],
            Side::ICloud,
        );

        assert_eq!(
            render(&paired.ops),
            "conflict(baseline) icloud wins uid=u1 → fastmail /f/u1.vcf@f-u1\ncreate icloud uid=fm-2 from=/f/fm-2.vcf"
        );
    }
}
