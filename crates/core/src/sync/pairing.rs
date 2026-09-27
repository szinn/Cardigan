use std::collections::{HashMap, HashSet};

use super::{Op, PairPass, SYNC_HASH, SyncedCard, Unsynced, UnsyncedCard};
use crate::{
    contact::{CardHash, ConflictWinner, HashOptions, Side, Uid},
    state::ConflictOrigin,
};

/// What pairing decided this cycle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Paired {
    pub ops: Vec<Op>,
}

/// Pairs both sides' unsynced cards: pass 1 (same UID), then pass 2 (same
/// content, different UIDs). Runs every cycle; on an empty state store this
/// is the whole baseline.
pub fn pair(unsynced: &Unsynced, winner: ConflictWinner) -> Paired {
    let mut pairing = Pairing {
        icloud: unsynced.icloud.iter().collect(),
        fastmail: unsynced.fastmail.iter().collect(),
        winner,
        ops: Vec::new(),
    };
    pairing.by_uid();
    pairing.by_content();
    Paired { ops: pairing.ops }
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
}
