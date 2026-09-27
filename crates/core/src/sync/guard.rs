use super::{Op, Plan};
use crate::contact::Side;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{contact::Uid, sync::fixtures::res};

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
}
