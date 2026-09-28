use std::collections::{BTreeMap, HashMap};

use super::{Diagnostic, Op, Plan};
use crate::{
    contact::{DisplayIdentity, MatchKeys, Side, Uid},
    state::ContactState,
};

/// Deletions per side always allowed, however few contacts are synced.
pub const DELETE_FLOOR: usize = 10;
/// Share of synced contacts one cycle may delete on one side.
pub const DELETE_PERCENT: usize = 20;

/// A plan that would delete more than the guard allows on one side, for
/// example after discovery picked the wrong (empty) collection. CG-8 writes
/// nothing and reports this instead.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("plan would delete {deletes} of {contacts} synced contacts on {side} (limit {limit})")]
pub struct MassDeletion {
    pub side: Side,
    pub deletes: usize,
    pub contacts: usize,
    pub limit: usize,
}

/// Checks each side's deletion count against
/// `max(DELETE_FLOOR, synced_contacts * DELETE_PERCENT / 100)`. A side's count
/// is its `Delete` ops plus every `Forget` (gone on both sides, so it counts
/// toward both tallies). `Op::Recreate`'s own DELETE of the old Fastmail card
/// is intentionally not counted here (M3): every baseline pair recreates the
/// Fastmail card (pass 2 or pass 3), so counting those would trip the guard
/// on an ordinary baseline rather than a real mass deletion.
pub fn check_deletions(plan: &Plan, synced_contacts: usize) -> Result<(), MassDeletion> {
    let limit = DELETE_FLOOR.max(synced_contacts * DELETE_PERCENT / 100);
    for side in [Side::ICloud, Side::Fastmail] {
        let deletes = plan
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Delete { on, .. } if *on == side) || matches!(op, Op::Forget { .. }))
            .count();
        if deletes > limit {
            return Err(MassDeletion {
                side,
                deletes,
                contacts: synced_contacts,
                limit,
            });
        }
    }
    Ok(())
}

/// One `Op::Delete` of a synced row, with that row's identity from
/// `last_synced_vcard` (the deleted side's card is gone, and the other side's
/// may not have been fetched).
struct Deletion {
    index: usize,
    uid: Uid,
    on: Side,
    keys: MatchKeys,
    identity: DisplayIdentity,
}

impl Deletion {
    fn held_with(&self, partner: &Self) -> Diagnostic {
        Diagnostic::DeleteHeld {
            on: self.on,
            uid: self.uid.clone(),
            with: partner.uid.clone(),
            identity: self.identity.clone(),
        }
    }
}

/// Holds both deletes when one row is being deleted on Fastmail (its iCloud
/// card is gone) and another on iCloud (its Fastmail card is gone), and the
/// two look like the same contact (`MatchKeys::may_be_same_contact`). A
/// client that shows both accounts merges cards by name, so deleting one
/// merged entry can remove a different pair's card on each side, and
/// propagating both deletes every copy (CG-17). Removes the held ops and
/// appends one `Diagnostic::DeleteHeld` per row, in op order. Keeps no
/// state: each non-idle cycle lists in full, so the hold recurs until the
/// user restores a copy (a `Resurrect`) or deletes the rest (`Forget`s).
pub fn hold_cross_deletes(plan: &mut Plan, state: &[ContactState]) {
    let rows: HashMap<&Uid, &ContactState> = state.iter().map(|row| (&row.uid, row)).collect();
    let deletions: Vec<Deletion> = plan
        .ops
        .iter()
        .enumerate()
        .filter_map(|(index, op)| match op {
            Op::Delete { uid, on, .. } => rows.get(uid).map(|row| Deletion {
                index,
                uid: uid.clone(),
                on: *on,
                keys: row.last_synced_vcard.match_keys(),
                identity: row.last_synced_vcard.display_identity(),
            }),
            _ => None,
        })
        .collect();
    let mut held: BTreeMap<usize, Diagnostic> = BTreeMap::new();
    for fastmail in deletions.iter().filter(|d| d.on == Side::Fastmail) {
        for icloud in deletions.iter().filter(|d| d.on == Side::ICloud) {
            if fastmail.keys.may_be_same_contact(&icloud.keys) {
                held.entry(fastmail.index).or_insert_with(|| fastmail.held_with(icloud));
                held.entry(icloud.index).or_insert_with(|| icloud.held_with(fastmail));
            }
        }
    }
    if held.is_empty() {
        return;
    }
    let mut index = 0;
    plan.ops.retain(|_| {
        let keep = !held.contains_key(&index);
        index += 1;
        keep
    });
    plan.diagnostics.extend(held.into_values());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        contact::VCard,
        sync::fixtures::{card_with, res, row},
    };

    fn deletes(icloud: usize, fastmail: usize) -> Plan {
        let delete = |side: Side, i: usize| Op::Delete {
            uid: Uid::from(format!("u{i}")),
            on: side,
            target: res(&format!("/{side}/{i}.vcf"), "e"),
        };
        Plan {
            ops: (0..icloud)
                .map(|i| delete(Side::ICloud, i))
                .chain((0..fastmail).map(|i| delete(Side::Fastmail, i)))
                .collect(),
            diagnostics: vec![],
        }
    }

    #[test]
    fn the_floor_applies_to_small_address_books() {
        assert_eq!(check_deletions(&deletes(10, 0), 12), Ok(()));
        assert_eq!(
            check_deletions(&deletes(11, 0), 12),
            Err(MassDeletion {
                side: Side::ICloud,
                deletes: 11,
                contacts: 12,
                limit: 10
            })
        );
    }

    #[test]
    fn the_percentage_applies_to_large_address_books() {
        assert_eq!(check_deletions(&deletes(0, 200), 1000), Ok(()));
        assert_eq!(
            check_deletions(&deletes(0, 201), 1000),
            Err(MassDeletion {
                side: Side::Fastmail,
                deletes: 201,
                contacts: 1000,
                limit: 200
            })
        );
    }

    #[test]
    fn sides_are_counted_separately() {
        assert_eq!(check_deletions(&deletes(8, 8), 12), Ok(()));
    }

    #[test]
    fn forgets_count_toward_both_sides() {
        let forget = |i: usize| Op::Forget {
            uid: Uid::from(format!("u{i}")),
        };
        let plan = Plan {
            ops: (0..11).map(forget).collect(),
            diagnostics: vec![],
        };
        assert_eq!(
            check_deletions(&plan, 12),
            Err(MassDeletion {
                side: Side::ICloud,
                deletes: 11,
                contacts: 12,
                limit: 10
            })
        );
    }

    #[test]
    fn message_names_counts_only() {
        let error = check_deletions(&deletes(11, 0), 12).unwrap_err();
        assert_eq!(error.to_string(), "plan would delete 11 of 12 synced contacts on icloud (limit 10)");
    }

    fn delete_on(uid: &str, on: Side) -> Op {
        Op::Delete {
            uid: Uid::from(uid),
            on,
            target: res(&format!("/{on}/{uid}.vcf"), "e"),
        }
    }

    fn rows(cards: &[VCard]) -> Vec<ContactState> {
        (1..)
            .zip(cards)
            .map(|(id, card)| {
                let uid = card.uid().as_str();
                let (icloud, fastmail) = (format!("/i/{uid}.vcf"), format!("/f/{uid}.vcf"));
                row(id, card, (icloud.as_str(), "i"), (fastmail.as_str(), "f"))
            })
            .collect()
    }

    fn held(plan: &Plan) -> Vec<String> {
        plan.diagnostics.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn the_incident_holds_both_deletes_and_keeps_the_forget() {
        // Row a: the nameless ORG-only duplicate lost its iCloud card. Row b:
        // the named card lost its Fastmail card. Row c: gone from both.
        let state = rows(&[
            card_with("a", "", "ORG:Harbor Grill\r\nTEL:+1 555 0100\r\n"),
            card_with("b", "Harbor Grill", "TEL:+1 555 0100\r\n"),
            card_with("c", "Harbor Grill", ""),
        ]);
        let mut plan = Plan {
            ops: vec![delete_on("a", Side::Fastmail), delete_on("b", Side::ICloud), Op::Forget { uid: Uid::from("c") }],
            diagnostics: vec![],
        };

        hold_cross_deletes(&mut plan, &state);

        assert_eq!(plan.ops, vec![Op::Forget { uid: Uid::from("c") }]);
        assert_eq!(
            held(&plan),
            [
                "delete held on fastmail uid=a: may be the same contact as uid=b",
                "delete held on icloud uid=b: may be the same contact as uid=a",
            ]
        );
        assert!(matches!(&plan.diagnostics[1], Diagnostic::DeleteHeld { identity, .. } if identity.to_string() == "Harbor Grill"));
    }

    #[test]
    fn a_lone_delete_is_not_held() {
        let state = rows(&[card_with("a", "Harbor Grill", ""), card_with("b", "Harbor Grill", "")]);
        let mut plan = Plan {
            ops: vec![delete_on("a", Side::Fastmail)],
            diagnostics: vec![],
        };

        hold_cross_deletes(&mut plan, &state);

        assert_eq!(plan.ops, vec![delete_on("a", Side::Fastmail)]);
        assert_eq!(plan.diagnostics, []);
    }

    #[test]
    fn unrelated_deletes_on_opposite_sides_proceed() {
        let state = rows(&[
            card_with("a", "Ann Lee", "EMAIL:ann@example.com\r\n"),
            card_with("b", "Bo Ray", "EMAIL:bo@example.com\r\n"),
        ]);
        let ops = vec![delete_on("a", Side::Fastmail), delete_on("b", Side::ICloud)];
        let mut plan = Plan {
            ops: ops.clone(),
            diagnostics: vec![],
        };

        hold_cross_deletes(&mut plan, &state);

        assert_eq!(plan.ops, ops);
        assert_eq!(plan.diagnostics, []);
    }

    #[test]
    fn same_identity_deletes_on_one_side_proceed() {
        let state = rows(&[card_with("a", "Harbor Grill", ""), card_with("b", "Harbor Grill", "")]);
        let ops = vec![delete_on("a", Side::Fastmail), delete_on("b", Side::Fastmail)];
        let mut plan = Plan {
            ops: ops.clone(),
            diagnostics: vec![],
        };

        hold_cross_deletes(&mut plan, &state);

        assert_eq!(plan.ops, ops);
    }

    #[test]
    fn a_delete_without_a_state_row_is_left_alone() {
        let state = rows(&[card_with("a", "Harbor Grill", "")]);
        let ops = vec![delete_on("a", Side::Fastmail), delete_on("ghost", Side::ICloud)];
        let mut plan = Plan {
            ops: ops.clone(),
            diagnostics: vec![],
        };

        hold_cross_deletes(&mut plan, &state);

        assert_eq!(plan.ops, ops);
    }

    #[test]
    fn existing_diagnostics_are_kept_ahead_of_held_deletes() {
        let state = rows(&[card_with("a", "Harbor Grill", ""), card_with("b", "Harbor Grill", "")]);
        let deferred = Diagnostic::DeletionDeferred {
            side: Side::ICloud,
            uid: Uid::from("z"),
        };
        let mut plan = Plan {
            ops: vec![delete_on("a", Side::Fastmail), delete_on("b", Side::ICloud)],
            diagnostics: vec![deferred.clone()],
        };

        hold_cross_deletes(&mut plan, &state);

        assert_eq!(plan.diagnostics.len(), 3);
        assert_eq!(plan.diagnostics[0], deferred);
    }
}
