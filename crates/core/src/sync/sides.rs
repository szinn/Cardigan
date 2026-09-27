use std::collections::{BTreeMap, HashMap, HashSet};

use super::{Diagnostic, Entry, Resource, Snapshot, UnsyncedCard};
use crate::{
    contact::{Href, Side, Uid, VCard},
    state::ContactState,
};

/// How a synced contact's card shows up on one side this cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Present {
    /// At its stored href with its stored ETag: not fetched.
    Unchanged,
    /// Fetched this cycle: a new ETag, or a new href (moved).
    Fetched { resource: Resource, card: VCard },
}

/// One side's snapshot, classified against state.
#[derive(Debug, Default)]
pub(crate) struct SideView {
    /// Synced contacts present on this side, by UID.
    pub(crate) tracked: HashMap<Uid, Present>,
    /// UIDs to leave alone this cycle on both sides: held, unreadable at
    /// their synced href, UID changed, or duplicated.
    pub(crate) held: HashSet<Uid>,
    /// Parsed cards whose UID has no state row, in UID order.
    pub(crate) unsynced: Vec<UnsyncedCard>,
    pub(crate) diagnostics: Vec<Diagnostic>,
}

impl SideView {
    pub(crate) fn classify(side: Side, snapshot: &Snapshot, state: &[ContactState]) -> Self {
        let synced_at: HashMap<&Href, &Uid> = state.iter().map(|row| (&row.side(side).href, &row.uid)).collect();
        let in_state: HashSet<&Uid> = state.iter().map(|row| &row.uid).collect();
        let mut view = Self::default();
        // Every appearance of each UID, in href order.
        let mut found: BTreeMap<Uid, Vec<(Resource, Option<VCard>)>> = BTreeMap::new();

        for (href, entry) in snapshot.entries() {
            let synced_uid = synced_at.get(href).copied();
            match entry {
                Entry::Held(_) => {
                    if let Some(uid) = synced_uid {
                        view.held.insert(uid.clone());
                    }
                }
                Entry::Unchanged(etag) => {
                    // CG-8 marks only state hrefs unchanged; anything else is
                    // ignored.
                    if let Some(uid) = synced_uid {
                        let resource = Resource {
                            href: href.clone(),
                            etag: etag.clone(),
                        };
                        found.entry(uid.clone()).or_default().push((resource, None));
                    }
                }
                Entry::Fetched { etag, card: Err(error) } => {
                    view.diagnostics.push(Diagnostic::Unreadable {
                        side,
                        href: href.clone(),
                        etag: etag.clone(),
                        error: error.clone(),
                    });
                    if let Some(uid) = synced_uid {
                        view.held.insert(uid.clone());
                    }
                }
                Entry::Fetched { etag, card: Ok(card) } => match synced_uid {
                    Some(stored) if stored != card.uid() => {
                        view.diagnostics.push(Diagnostic::UidChanged {
                            side,
                            href: href.clone(),
                            etag: etag.clone(),
                            stored: stored.clone(),
                            found: card.uid().clone(),
                        });
                        view.held.insert(stored.clone());
                        view.held.insert(card.uid().clone());
                    }
                    _ => {
                        let resource = Resource {
                            href: href.clone(),
                            etag: etag.clone(),
                        };
                        found.entry(card.uid().clone()).or_default().push((resource, Some(card.clone())));
                    }
                },
            }
        }

        for (uid, mut appearances) in found {
            if view.held.contains(&uid) {
                continue;
            }
            if appearances.len() > 1 {
                view.diagnostics.push(Diagnostic::DuplicateUid {
                    side,
                    uid: uid.clone(),
                    hrefs: appearances.iter().map(|(resource, _)| resource.href.clone()).collect(),
                });
                view.held.insert(uid);
                continue;
            }
            let (resource, card) = appearances.pop().expect("a found UID has at least one appearance");
            match (in_state.contains(&uid), card) {
                (true, None) => {
                    view.tracked.insert(uid, Present::Unchanged);
                }
                (true, Some(card)) => {
                    view.tracked.insert(uid, Present::Fetched { resource, card });
                }
                (false, Some(card)) => view.unsynced.push(UnsyncedCard { resource, card }),
                // Unchanged entries only come from state hrefs.
                (false, None) => {}
            }
        }
        view
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        contact::{ETag, VCardError},
        sync::fixtures::{card, card_with, fetched, res, row, snapshot, unchanged},
    };

    fn state() -> Vec<ContactState> {
        vec![row(1, &card("u1", "Jane Doe"), ("/i/u1.vcf", "i1"), ("/f/u1.vcf", "f1"))]
    }

    fn u(uid: &str) -> Uid {
        Uid::from(uid)
    }

    fn classify(snapshot: &Snapshot) -> SideView {
        SideView::classify(Side::ICloud, snapshot, &state())
    }

    #[test]
    fn unchanged_and_fetched_cards_are_tracked() {
        let view = classify(&snapshot([("/i/u1.vcf", unchanged("i1"))]));
        assert_eq!(view.tracked.get(&u("u1")), Some(&Present::Unchanged));

        let edited = card_with("u1", "Jane Doe", "NOTE:edit\r\n");
        let view = classify(&snapshot([("/i/u1.vcf", fetched("i2", edited.clone()))]));
        assert_eq!(
            view.tracked.get(&u("u1")),
            Some(&Present::Fetched {
                resource: res("/i/u1.vcf", "i2"),
                card: edited
            })
        );
        assert!(view.held.is_empty() && view.unsynced.is_empty() && view.diagnostics.is_empty());
    }

    #[test]
    fn missing_card_is_not_tracked() {
        let view = classify(&Snapshot::new());
        assert!(view.tracked.is_empty());
        assert!(view.held.is_empty());
    }

    #[test]
    fn moved_card_is_tracked_at_its_new_href() {
        let view = classify(&snapshot([("/i/moved.vcf", fetched("i2", card("u1", "Jane Doe")))]));
        assert_eq!(
            view.tracked.get(&u("u1")),
            Some(&Present::Fetched {
                resource: res("/i/moved.vcf", "i2"),
                card: card("u1", "Jane Doe")
            })
        );
    }

    #[test]
    fn card_without_state_row_is_unsynced() {
        let view = classify(&snapshot([
            ("/i/u9.vcf", fetched("i9", card("u9", "Sam Poe"))),
            ("/i/u5.vcf", fetched("i5", card("u5", "Ann Lee"))),
        ]));
        assert!(view.tracked.is_empty());
        let uids: Vec<&str> = view.unsynced.iter().map(|c| c.card.uid().as_str()).collect();
        assert_eq!(uids, ["u5", "u9"]);
        assert_eq!(view.unsynced[0].resource, res("/i/u5.vcf", "i5"));
    }

    #[test]
    fn held_entry_holds_its_synced_uid_only() {
        let view = classify(&snapshot([
            ("/i/u1.vcf", Entry::Held(ETag::from("i2"))),
            ("/i/other.vcf", Entry::Held(ETag::from("x"))),
        ]));
        assert_eq!(view.held, HashSet::from([u("u1")]));
        assert!(view.tracked.is_empty() && view.unsynced.is_empty());
    }

    #[test]
    fn unreadable_card_is_reported_and_holds_its_synced_uid() {
        let unreadable = |etag: &str| Entry::Fetched {
            etag: ETag::from(etag),
            card: Err(VCardError::MissingUid),
        };
        let view = classify(&snapshot([("/i/u1.vcf", unreadable("i2")), ("/i/new.vcf", unreadable("n1"))]));
        assert_eq!(view.held, HashSet::from([u("u1")]));
        assert_eq!(
            view.diagnostics,
            [
                Diagnostic::Unreadable {
                    side: Side::ICloud,
                    href: Href::from("/i/new.vcf"),
                    etag: ETag::from("n1"),
                    error: VCardError::MissingUid
                },
                Diagnostic::Unreadable {
                    side: Side::ICloud,
                    href: Href::from("/i/u1.vcf"),
                    etag: ETag::from("i2"),
                    error: VCardError::MissingUid
                },
            ]
        );
    }

    #[test]
    fn uid_change_at_synced_href_holds_both_uids() {
        let view = classify(&snapshot([("/i/u1.vcf", fetched("i2", card("u4", "Jane Doe")))]));
        assert_eq!(view.held, HashSet::from([u("u1"), u("u4")]));
        assert!(view.tracked.is_empty() && view.unsynced.is_empty());
        assert_eq!(
            view.diagnostics,
            [Diagnostic::UidChanged {
                side: Side::ICloud,
                href: Href::from("/i/u1.vcf"),
                etag: ETag::from("i2"),
                stored: u("u1"),
                found: u("u4")
            }]
        );
    }

    #[test]
    fn duplicate_uid_is_held_and_never_unsynced() {
        let view = classify(&snapshot([
            ("/i/u1.vcf", unchanged("i1")),
            ("/i/copy.vcf", fetched("c1", card("u1", "Jane Doe"))),
            ("/i/a.vcf", fetched("a1", card("u9", "Sam Poe"))),
            ("/i/b.vcf", fetched("b1", card("u9", "Sam Poe"))),
        ]));
        assert_eq!(view.held, HashSet::from([u("u1"), u("u9")]));
        assert!(view.tracked.is_empty() && view.unsynced.is_empty());
        assert_eq!(
            view.diagnostics,
            [
                Diagnostic::DuplicateUid {
                    side: Side::ICloud,
                    uid: u("u1"),
                    hrefs: vec![Href::from("/i/copy.vcf"), Href::from("/i/u1.vcf")]
                },
                Diagnostic::DuplicateUid {
                    side: Side::ICloud,
                    uid: u("u9"),
                    hrefs: vec![Href::from("/i/a.vcf"), Href::from("/i/b.vcf")]
                },
            ]
        );
    }
}
