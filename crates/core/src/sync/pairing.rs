use std::collections::{HashMap, HashSet};

use super::{Entry, Op, PairPass, RecreateConflict, Resource, SYNC_HASH, Snapshot, SyncedCard, Unsynced, UnsyncedCard};
use crate::{
    contact::{CardHash, ConflictWinner, DisplayIdentity, HashOptions, MatchKeys, Side, Uid, VCard},
    state::{ConflictOrigin, ContactState},
};

/// Why pairing left a card alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Several possible matches, or a same-name card with nothing shared
    /// (passes 2 and 3, and the 2026-09-27 nameless collision).
    Ambiguous,
    /// CG-18: a card with no name that shares an email or phone with a card
    /// the other side already holds (named or not; paired, synced, skipped
    /// or held). Most likely a duplicate of it, so it is not copied. Report
    /// only: `baseline_skips` does not record the reason.
    LikelyDuplicate,
}

/// A card pairing leaves alone this cycle: it may be the same person as a
/// card on the other side, but not certainly. Re-evaluated every cycle; CG-8
/// persists the latest set in `baseline_skips`.
///
/// CG-8 should log one `warn` per skip, naming only `identity` and `uid`
/// (the report already lists every skip; this is for operators watching the
/// log). This applies to every kind of skip below, including the nameless
/// email/phone match (2026-09-27 decision).
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
    /// The other side's unsynced cards the count came from: same-name
    /// candidates for a pass-3 skip, (I1) the pass-2 content matches that
    /// stayed unpaired for a card pass 2 refused as ambiguous, or (2026-09-27
    /// decision) the other nameless cards a nameless card shares an email or
    /// phone with.
    pub candidates: Vec<DisplayIdentity>,
    /// Why the card was skipped.
    pub reason: SkipReason,
}

/// What pairing decided this cycle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Paired {
    pub ops: Vec<Op>,
    pub skips: Vec<Skip>,
}

/// A card one side holds this cycle, reduced to what the likely-duplicate
/// check needs (CG-18). `keys` holds emails and phone numbers (PII): never
/// log it.
#[derive(Debug, Clone)]
pub struct KnownCard {
    pub uid: Uid,
    pub keys: MatchKeys,
    pub identity: DisplayIdentity,
}

impl KnownCard {
    pub fn of(card: &VCard) -> Self {
        Self {
            uid: card.uid().clone(),
            keys: card.match_keys(),
            identity: card.display_identity(),
        }
    }
}

/// Every card each side holds this cycle: its parsed snapshot entries, then
/// every synced contact's `last_synced_vcard` (which covers `Unchanged`
/// entries that were not fetched). `Held` and unreadable entries carry no
/// card, so they cannot count. One entry per UID; a fetched card wins over
/// its state row.
#[derive(Debug, Clone, Default)]
pub struct KnownCards {
    pub icloud: Vec<KnownCard>,
    pub fastmail: Vec<KnownCard>,
}

impl KnownCards {
    pub fn collect(icloud: &Snapshot, fastmail: &Snapshot, state: &[ContactState]) -> Self {
        Self {
            icloud: known_on(icloud, state),
            fastmail: known_on(fastmail, state),
        }
    }

    fn side(&self, side: Side) -> &[KnownCard] {
        match side {
            Side::ICloud => &self.icloud,
            Side::Fastmail => &self.fastmail,
        }
    }
}

fn known_on(snapshot: &Snapshot, state: &[ContactState]) -> Vec<KnownCard> {
    let fetched = snapshot.entries().filter_map(|(_, entry)| match entry {
        Entry::Fetched { card: Ok(card), .. } => Some(card),
        _ => None,
    });
    let mut seen: HashSet<&Uid> = HashSet::new();
    fetched
        .chain(state.iter().map(|row| &row.last_synced_vcard))
        .filter(|card| seen.insert(card.uid()))
        .map(KnownCard::of)
        .collect()
}

/// Pairs both sides' unsynced cards: pass 1 (same UID), pass 2 (same
/// content), pass 3 (identity heuristic); then copies unique cards and skips
/// ambiguous ones and likely duplicates of a card in `known` (CG-18). Runs
/// every cycle; on an empty state store this is the whole baseline.
pub fn pair(unsynced: &Unsynced, known: &KnownCards, winner: ConflictWinner) -> Paired {
    let mut pairing = Pairing {
        icloud: unsynced.icloud.iter().collect(),
        fastmail: unsynced.fastmail.iter().collect(),
        winner,
        ops: Vec::new(),
        pass2: Pass2Candidates::default(),
    };
    pairing.by_uid();
    pairing.by_content();
    pairing.by_identity();

    let mut paired = Paired {
        ops: pairing.ops,
        skips: Vec::new(),
    };
    let settled = settle_side(
        Side::ICloud,
        &pairing.icloud,
        &pairing.fastmail,
        &pairing.pass2.icloud,
        known.side(Side::Fastmail),
    )
    .into_iter()
    .chain(settle_side(
        Side::Fastmail,
        &pairing.fastmail,
        &pairing.icloud,
        &pairing.pass2.fastmail,
        known.side(Side::ICloud),
    ));
    for outcome in settled {
        match outcome {
            Settled::Copy(op) => paired.ops.push(*op),
            Settled::Skip(skip) => paired.skips.push(skip),
        }
    }
    paired
}

/// One card's fate after the three passes: copied to the other side under
/// its own UID, or left as an ambiguous `Skip`.
enum Settled {
    Copy(Box<Op>),
    Skip(Skip),
}

/// Pass-2 (content) candidate lists for cards that stayed unpaired: keyed by
/// the card's own UID, each entry is `(candidate_count, candidates)` using
/// the same counting convention as pass 3 (I1). Consulted by `settle_side`
/// so a card pass 2 refused as ambiguous is skipped, never copied, even when
/// it has no usable name for pass 3's name-collision check.
#[derive(Debug, Clone, Default)]
struct Pass2Candidates {
    icloud: HashMap<Uid, (u32, Vec<DisplayIdentity>)>,
    fastmail: HashMap<Uid, (u32, Vec<DisplayIdentity>)>,
}

/// The cards still unpaired on each side, and the ops decided so far.
struct Pairing<'a> {
    icloud: Vec<&'a UnsyncedCard>,
    fastmail: Vec<&'a UnsyncedCard>,
    winner: ConflictWinner,
    ops: Vec<Op>,
    pass2: Pass2Candidates,
}

impl Pairing<'_> {
    /// Pass 1: the same UID on both sides. Duplicate UIDs are held before
    /// they ever reach `Unsynced` (CG-6), so a UID never repeats within a
    /// side and a `HashMap` keyed by UID is safe here.
    fn by_uid(&mut self) {
        let fastmail: HashMap<&Uid, usize> = self.fastmail.iter().enumerate().map(|(f, c)| (c.card.uid(), f)).collect();
        debug_assert_eq!(
            fastmail.len(),
            self.fastmail.len(),
            "duplicate UID reached pairing on fastmail (CG-6 should hold it)"
        );
        debug_assert_eq!(
            self.icloud.iter().map(|c| c.card.uid()).collect::<HashSet<_>>().len(),
            self.icloud.len(),
            "duplicate UID reached pairing on icloud (CG-6 should hold it)"
        );
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
        // I1: a card with a pass-2 candidate that stays unpaired (refused as
        // not mutual) must become a skip, never a copy, however settle_side
        // would otherwise classify it — nameless cards included. Recorded
        // before removing the pairs, over the cards pass 2 actually saw.
        self.record_pass2_leftovers(&candidates, &pairs);
        for &(i, f) in &pairs {
            let op = Self::content_pair(self.icloud[i], self.fastmail[f]);
            self.ops.push(op);
        }
        self.remove(&pairs);
    }

    fn record_pass2_leftovers(&mut self, candidates: &[Vec<usize>], pairs: &[(usize, usize)]) {
        let reverse = invert(candidates, self.fastmail.len());
        let paired_icloud: HashSet<usize> = pairs.iter().map(|&(i, _)| i).collect();
        let paired_fastmail: HashSet<usize> = pairs.iter().map(|&(_, f)| f).collect();
        for (i, list) in candidates.iter().enumerate() {
            if list.is_empty() || paired_icloud.contains(&i) {
                continue;
            }
            let count = candidate_count(list, &reverse);
            let identities = list.iter().map(|&f| self.fastmail[f].card.display_identity()).collect();
            self.pass2.icloud.insert(self.icloud[i].card.uid().clone(), (count, identities));
        }
        for (f, list) in reverse.iter().enumerate() {
            if list.is_empty() || paired_fastmail.contains(&f) {
                continue;
            }
            let count = candidate_count(list, candidates);
            let identities = list.iter().map(|&i| self.icloud[i].card.display_identity()).collect();
            self.pass2.fastmail.insert(self.fastmail[f].card.uid().clone(), (count, identities));
        }
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

/// After the passes (Decision 6): unique → copy; ambiguous → skip, checked
/// in this order: `pass2` (I1) takes priority over everything else — a card
/// pass 2 could not pair mutually is always a skip, even when it has no
/// usable name; then a nameless card (2026-09-27 decision) is a skip when
/// another nameless card on the other side shares an email or phone with it,
/// and a copy otherwise; a named card falls back to the name-collision
/// check. A nameless card with no such collision is also a skip
/// (`LikelyDuplicate`, CG-18) when it shares an email or phone with any card
/// in `known`, the other side's cards this cycle; otherwise it is copied.
fn settle_side(
    side: Side,
    cards: &[&UnsyncedCard],
    other: &[&UnsyncedCard],
    pass2: &HashMap<Uid, (u32, Vec<DisplayIdentity>)>,
    known: &[KnownCard],
) -> Vec<Settled> {
    let own = identity_candidates(cards, other);
    let reverse = identity_candidates(other, cards);
    let other_keys: Vec<MatchKeys> = other.iter().map(|c| c.card.match_keys()).collect();
    let other_names = name_buckets(&other_keys);
    // 2026-09-27 decision: two nameless cards that share an email or phone
    // are plausibly the same person, so neither is copied.
    let nameless_own = nameless_candidates(cards, other);
    let nameless_reverse = nameless_candidates(other, cards);

    cards
        .iter()
        .zip(&own)
        .enumerate()
        .map(|(index, (card, candidates))| {
            let uid = card.card.uid();
            if let Some((count, pass2_candidates)) = pass2.get(uid) {
                return Settled::Skip(Skip {
                    side,
                    resource: card.resource.clone(),
                    uid: uid.clone(),
                    content_hash: card.card.canonical_hash(SYNC_HASH),
                    candidate_count: *count,
                    identity: card.card.display_identity(),
                    candidates: pass2_candidates.clone(),
                    reason: SkipReason::Ambiguous,
                });
            }
            let keys = card.card.match_keys();
            if keys.name_key().is_none() {
                let matches = &nameless_own[index];
                if matches.is_empty() {
                    // CG-18: a duplicate of a card the other side already
                    // holds is skipped; a shared ORG alone never counts.
                    let duplicates: Vec<&KnownCard> = known.iter().filter(|k| keys.shares_contact_point(&k.keys)).collect();
                    if !duplicates.is_empty() {
                        return Settled::Skip(Skip {
                            side,
                            resource: card.resource.clone(),
                            uid: uid.clone(),
                            content_hash: card.card.canonical_hash(SYNC_HASH),
                            candidate_count: u32::try_from(duplicates.len()).unwrap_or(u32::MAX),
                            identity: card.card.display_identity(),
                            candidates: duplicates.iter().map(|k| k.identity.clone()).collect(),
                            reason: SkipReason::LikelyDuplicate,
                        });
                    }
                    return Settled::Copy(Box::new(Op::Create {
                        uid: uid.clone(),
                        to: side.other(),
                        source: card.resource.clone(),
                        synced: SyncedCard::for_push(&card.card, None),
                    }));
                }
                return Settled::Skip(Skip {
                    side,
                    resource: card.resource.clone(),
                    uid: uid.clone(),
                    content_hash: card.card.canonical_hash(SYNC_HASH),
                    // Same contested-singleton convention as pass 2/3.
                    candidate_count: candidate_count(matches, &nameless_reverse),
                    identity: card.card.display_identity(),
                    candidates: matches.iter().map(|&o| other[o].card.display_identity()).collect(),
                    reason: SkipReason::Ambiguous,
                });
            }
            let same_name = keys.name_key().and_then(|name| other_names.get(name)).map_or(&[][..], Vec::as_slice);
            if same_name.is_empty() {
                return Settled::Copy(Box::new(Op::Create {
                    uid: uid.clone(),
                    to: side.other(),
                    source: card.resource.clone(),
                    synced: SyncedCard::for_push(&card.card, None),
                }));
            }
            Settled::Skip(Skip {
                side,
                resource: card.resource.clone(),
                uid: uid.clone(),
                content_hash: card.card.canonical_hash(SYNC_HASH),
                candidate_count: candidate_count(candidates, &reverse),
                identity: card.card.display_identity(),
                candidates: same_name.iter().map(|&o| other[o].card.display_identity()).collect(),
                reason: SkipReason::Ambiguous,
            })
        })
        .collect()
}

/// For each card in `from` with no usable name, the `to` cards that also
/// have no usable name and share an email or phone with it (`from` cards
/// with a name get an empty list here; pass 3's `identity_candidates`
/// already covers them). 2026-09-27 decision: nameless cards this plausibly
/// identifies as the same person are skipped, never both copied.
fn nameless_candidates(from: &[&UnsyncedCard], to: &[&UnsyncedCard]) -> Vec<Vec<usize>> {
    let to_keys: Vec<MatchKeys> = to.iter().map(|c| c.card.match_keys()).collect();
    from.iter()
        .map(|c| {
            let keys = c.card.match_keys();
            if keys.name_key().is_some() {
                return Vec::new();
            }
            to_keys
                .iter()
                .enumerate()
                .filter(|(_, other_keys)| other_keys.name_key().is_none() && keys.shares_contact_point(other_keys))
                .map(|(index, _)| index)
                .collect()
        })
        .collect()
}

/// A card's candidate count under the pass 2/3 convention: no match is 0;
/// several matches report their own count; a single match reports that
/// match's own claimant count instead (a singleton with exactly one mutual
/// claimant would already have been paired, so this only fires when it's
/// contested).
fn candidate_count(list: &[usize], reverse: &[Vec<usize>]) -> u32 {
    let count = match list {
        [only] => reverse[*only].len(),
        list => list.len(),
    };
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// Inverts a `from → [to]` candidate map into `to → [from]`.
fn invert(candidates: &[Vec<usize>], to_len: usize) -> Vec<Vec<usize>> {
    let mut inverted = vec![Vec::new(); to_len];
    for (from, list) in candidates.iter().enumerate() {
        for &to in list {
            inverted[to].push(from);
        }
    }
    inverted
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
        contact::{ETag, Href, VCardError},
        sync::fixtures::{EMBEDDED_PHOTO, URI_PHOTO, card, card_with, fetched, on_fastmail, on_icloud, row},
    };

    fn run(icloud: Vec<VCard>, fastmail: Vec<VCard>, winner: Side) -> Paired {
        run_with(icloud, fastmail, &[], &[], winner)
    }

    /// `run` plus extra cards each side already holds (synced or paired
    /// elsewhere), which only feed `KnownCards`.
    fn run_with(icloud: Vec<VCard>, fastmail: Vec<VCard>, icloud_known: &[VCard], fastmail_known: &[VCard], winner: Side) -> Paired {
        let unsynced = Unsynced {
            icloud: icloud.into_iter().map(on_icloud).collect(),
            fastmail: fastmail.into_iter().map(on_fastmail).collect(),
        };
        let known = KnownCards {
            icloud: unsynced.icloud.iter().map(|c| &c.card).chain(icloud_known).map(KnownCard::of).collect(),
            fastmail: unsynced.fastmail.iter().map(|c| &c.card).chain(fastmail_known).map(KnownCard::of).collect(),
        };
        pair(&unsynced, &known, winner)
    }

    fn skips(paired: &Paired) -> Vec<(Side, &str, SkipReason)> {
        paired.skips.iter().map(|s| (s.side, s.uid.as_str(), s.reason)).collect()
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
                reason: SkipReason::Ambiguous,
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
    fn nameless_cards_sharing_an_email_are_skipped_not_copied() {
        // 2026-09-27 decision: nameless cards on both sides that share an
        // email are plausibly the same person, so neither is copied.
        let icloud = card_with("ic-1", "x@example.com", "EMAIL:x@example.com\r\nNOTE:one\r\n");
        let fastmail = card_with("fm-1", "x@example.com", "EMAIL:x@example.com\r\nNOTE:two\r\n");

        let paired = run(vec![icloud], vec![fastmail], Side::ICloud);

        assert!(paired.ops.is_empty(), "{}", render(&paired.ops));
        let counts: Vec<(Side, &str, u32)> = paired.skips.iter().map(|s| (s.side, s.uid.as_str(), s.candidate_count)).collect();
        assert_eq!(counts, [(Side::ICloud, "ic-1", 1), (Side::Fastmail, "fm-1", 1)]);
    }

    #[test]
    fn nameless_cards_sharing_a_phone_are_skipped_not_copied() {
        let icloud = card_with("ic-1", "+15550100100", "TEL:+15550100100\r\nNOTE:one\r\n");
        let fastmail = card_with("fm-1", "+15550100100", "TEL:+15550100100\r\nNOTE:two\r\n");

        let paired = run(vec![icloud], vec![fastmail], Side::ICloud);

        assert!(paired.ops.is_empty(), "{}", render(&paired.ops));
        let counts: Vec<(Side, &str, u32)> = paired.skips.iter().map(|s| (s.side, s.uid.as_str(), s.candidate_count)).collect();
        assert_eq!(counts, [(Side::ICloud, "ic-1", 1), (Side::Fastmail, "fm-1", 1)]);
    }

    #[test]
    fn nameless_collision_skip_appears_in_the_report_without_pii() {
        let icloud = card_with("ic-1", "x@example.com", "EMAIL:x@example.com\r\nNOTE:one\r\n");
        let fastmail = card_with("fm-1", "x@example.com", "EMAIL:x@example.com\r\nNOTE:two\r\n");

        let paired = run(vec![icloud], vec![fastmail], Side::ICloud);
        let report = crate::sync::BaselineReport::build(&paired.ops, &paired.skips, &[]);
        let rendered = report.to_string();

        assert!(!rendered.contains('@'), "PII leaked into the report: {rendered}");
        insta::assert_snapshot!(rendered, @r"
        in sync: 0, conflicts: 0, re-UID'd: 0, paired by identity: 0, skipped: 2, likely duplicates: 0, to copy: 0
        Skipped, never guessed (edit either card to resolve):
          icloud <no name>: 1 candidates: <no name>
          fastmail <no name>: 1 candidates: <no name>
        ");
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
    fn nameless_pass2_candidates_that_stay_unpaired_are_skipped() {
        // I1: two identical nameless iCloud cards vs one identical Fastmail
        // card. Pass 2 refuses both (not mutual); pass 3 ignores nameless
        // cards. Every one of the three must become a skip, never a copy.
        let body = "NOTE:x\r\n";
        let paired = run(
            vec![card_with("ic-1", "jane@example.com", body), card_with("ic-2", "jane@example.com", body)],
            vec![card_with("fm-1", "jane@example.com", body)],
            Side::ICloud,
        );

        assert!(paired.ops.is_empty(), "{}", render(&paired.ops));
        let counts: Vec<(Side, &str, u32)> = paired.skips.iter().map(|s| (s.side, s.uid.as_str(), s.candidate_count)).collect();
        assert_eq!(counts, [(Side::ICloud, "ic-1", 2), (Side::ICloud, "ic-2", 2), (Side::Fastmail, "fm-1", 2)]);
    }

    #[test]
    fn nameless_pass2_candidates_mirrored_on_fastmail_side() {
        // Mirrored: one iCloud card vs two identical nameless Fastmail cards.
        let body = "NOTE:x\r\n";
        let paired = run(
            vec![card_with("ic-1", "jane@example.com", body)],
            vec![card_with("fm-1", "jane@example.com", body), card_with("fm-2", "jane@example.com", body)],
            Side::ICloud,
        );

        assert!(paired.ops.is_empty(), "{}", render(&paired.ops));
        let counts: Vec<(Side, &str, u32)> = paired.skips.iter().map(|s| (s.side, s.uid.as_str(), s.candidate_count)).collect();
        assert_eq!(counts, [(Side::ICloud, "ic-1", 2), (Side::Fastmail, "fm-1", 2), (Side::Fastmail, "fm-2", 2)]);
    }

    #[test]
    fn identity_pair_leaves_the_other_side_free_for_a_same_name_leftover() {
        // Spec rule pinned: cards paired this cycle never count as
        // collisions, so a same-name leftover on one side is still copied.
        let icloud1 = card_with("ic-1", "Jane Doe", &format!("{JANE}NOTE:one\r\n"));
        let fastmail1 = card_with("fm-1", "Jane Doe", &format!("{JANE}NOTE:two\r\n"));
        let icloud2 = card_with("ic-2", "Jane Doe", "EMAIL:jane2@example.com\r\n");

        let paired = run(vec![icloud1, icloud2], vec![fastmail1], Side::ICloud);

        assert_eq!(
            render(&paired.ops),
            "recreate(identity) uid=ic-1 fastmail /f/fm-1.vcf@f-fm-1 was fm-1 icloud wins\ncreate fastmail uid=ic-2 from=/i/ic-2.vcf"
        );
        assert_eq!(paired.skips, []);
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

    #[test]
    fn a_nameless_duplicate_of_a_card_paired_by_uid_is_skipped() {
        // The CG-18 incident: KW pharmacy pairs by UID; Fastmail's nameless
        // ORG-only duplicate shares its phone and must not be copied.
        let kw = card_with("u1", "KW pharmacy", "TEL:+1 555 0100\r\n");
        let duplicate = card_with("fm-2", "", "ORG:KW pharmacy\r\nTEL:+1 (555) 0100\r\n");

        let paired = run(vec![kw.clone()], vec![kw, duplicate], Side::ICloud);

        assert_eq!(render(&paired.ops), "adopt uid=u1 icloud=/i/u1.vcf@i-u1 fastmail=/f/u1.vcf@f-u1");
        assert_eq!(skips(&paired), [(Side::Fastmail, "fm-2", SkipReason::LikelyDuplicate)]);
        let candidates: Vec<String> = paired.skips[0].candidates.iter().map(ToString::to_string).collect();
        assert_eq!(candidates, ["KW pharmacy"]);
        assert_eq!(paired.skips[0].candidate_count, 1);
    }

    #[test]
    fn a_nameless_duplicate_on_icloud_is_skipped_too() {
        let kw = card_with("u1", "KW pharmacy", "TEL:+1 555 0100\r\n");
        let duplicate = card_with("ic-2", "", "ORG:KW pharmacy\r\nTEL:+1 555 0100\r\n");

        let paired = run(vec![kw.clone(), duplicate], vec![kw], Side::ICloud);

        assert_eq!(render(&paired.ops), "adopt uid=u1 icloud=/i/u1.vcf@i-u1 fastmail=/f/u1.vcf@f-u1");
        assert_eq!(skips(&paired), [(Side::ICloud, "ic-2", SkipReason::LikelyDuplicate)]);
    }

    #[test]
    fn a_nameless_card_sharing_an_email_with_an_unpaired_named_card_is_skipped() {
        let paired = run(
            vec![card_with("ic-1", "Ann Lee", "EMAIL:ann@example.com\r\n")],
            vec![card_with("fm-1", "", "EMAIL:ANN@example.com\r\nNOTE:x\r\n")],
            Side::ICloud,
        );

        assert_eq!(render(&paired.ops), "create fastmail uid=ic-1 from=/i/ic-1.vcf");
        assert_eq!(skips(&paired), [(Side::Fastmail, "fm-1", SkipReason::LikelyDuplicate)]);
    }

    #[test]
    fn a_nameless_duplicate_of_an_already_synced_card_is_skipped() {
        // Steady state: the named card is synced, so it is known but never
        // reaches pairing.
        let paired = run_with(
            vec![],
            vec![card_with("fm-2", "", "ORG:KW pharmacy\r\nTEL:+1 555 0100\r\n")],
            &[card_with("u1", "KW pharmacy", "TEL:+1 555 0100\r\n")],
            &[],
            Side::ICloud,
        );

        assert_eq!(render(&paired.ops), "");
        assert_eq!(skips(&paired), [(Side::Fastmail, "fm-2", SkipReason::LikelyDuplicate)]);
    }

    #[test]
    fn a_nameless_card_sharing_only_an_org_is_still_copied() {
        let paired = run(
            vec![card_with("ic-1", "KW pharmacy", "ORG:KW pharmacy\r\n")],
            vec![card_with("fm-1", "", "ORG:KW pharmacy\r\nNOTE:x\r\n")],
            Side::ICloud,
        );

        assert_eq!(
            render(&paired.ops),
            "create fastmail uid=ic-1 from=/i/ic-1.vcf\ncreate icloud uid=fm-1 from=/f/fm-1.vcf"
        );
        assert_eq!(paired.skips, []);
    }

    #[test]
    fn a_nameless_card_with_an_unrelated_phone_is_copied() {
        let paired = run_with(
            vec![],
            vec![card_with("fm-2", "", "ORG:Other Shop\r\nTEL:+1 555 0199\r\n")],
            &[card_with("u1", "KW pharmacy", "TEL:+1 555 0100\r\n")],
            &[],
            Side::ICloud,
        );

        assert_eq!(render(&paired.ops), "create icloud uid=fm-2 from=/f/fm-2.vcf");
        assert_eq!(paired.skips, []);
    }

    #[test]
    fn nameless_collisions_keep_their_ambiguous_reason() {
        let icloud = card_with("ic-1", "x@example.com", "EMAIL:x@example.com\r\nNOTE:one\r\n");
        let fastmail = card_with("fm-1", "x@example.com", "EMAIL:x@example.com\r\nNOTE:two\r\n");

        let paired = run(vec![icloud], vec![fastmail], Side::ICloud);

        assert_eq!(
            skips(&paired),
            [(Side::ICloud, "ic-1", SkipReason::Ambiguous), (Side::Fastmail, "fm-1", SkipReason::Ambiguous)]
        );
    }

    #[test]
    fn known_cards_take_fetched_cards_then_state_rows_once_each() {
        let fresh = card_with("a", "Ann New", "TEL:+1 555 0100\r\n");
        let stale = card_with("a", "Ann Old", "TEL:+1 555 0100\r\n");
        let bob = card_with("b", "Bob Roe", "");
        let snapshot: Snapshot = [
            (Href::from("/i/a.vcf"), fetched("i-a", fresh)),
            (
                Href::from("/i/bad.vcf"),
                Entry::Fetched {
                    etag: ETag::from("i-bad"),
                    card: Err(VCardError::MissingUid),
                },
            ),
            (Href::from("/i/held.vcf"), Entry::Held(ETag::from("i-held"))),
        ]
        .into_iter()
        .collect();
        let rows = vec![
            row(1, &stale, ("/i/a.vcf", "i-a0"), ("/f/a.vcf", "f-a")),
            row(2, &bob, ("/i/b.vcf", "i-b"), ("/f/b.vcf", "f-b")),
        ];

        let known = KnownCards::collect(&snapshot, &Snapshot::new(), &rows);

        let icloud: Vec<(&str, String)> = known.icloud.iter().map(|k| (k.uid.as_str(), k.identity.to_string())).collect();
        assert_eq!(icloud, [("a", "Ann New".to_owned()), ("b", "Bob Roe".to_owned())]);
        let fastmail: Vec<&str> = known.fastmail.iter().map(|k| k.uid.as_str()).collect();
        assert_eq!(fastmail, ["a", "b"], "synced contacts are known on both sides");
    }
}
