//! What a cycle did, for its summary line. PII-free.

use std::fmt;

use crate::{contact::Side, state::CardFailure, sync::Op};

/// Counts for one direction: cards fetched from its source side and writes
/// to its destination side.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DirectionCounts {
    pub fetched: usize,
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub conflicts: usize,
    pub errors: usize,
}

impl fmt::Display for DirectionCounts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "fetched {}, added {}, updated {}, removed {}, conflicts {}, errors {}",
            self.fetched, self.added, self.updated, self.removed, self.conflicts, self.errors
        )
    }
}

/// One sync cycle's outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CycleSummary {
    /// iCloud → Fastmail.
    pub to_fastmail: DirectionCounts,
    /// Fastmail → iCloud.
    pub to_icloud: DirectionCounts,
    /// State-only bookkeeping (counted, never logged per record).
    pub adopted: usize,
    pub refreshed: usize,
    pub forgotten: usize,
    /// State-only ops that failed (a state-store error).
    pub state_errors: usize,
    /// Rows the planner left for a later cycle (`UnreadTarget`,
    /// `DeletionDeferred`).
    pub deferred: usize,
    /// Deletes held because they may remove one contact across two pairs
    /// (`DeleteHeld`, CG-17).
    pub held_deletes: usize,
    /// Ambiguous baseline cards left unsynced this cycle.
    pub skipped: usize,
    /// Cards that failed `PERSISTENT_ATTEMPTS` or more times in a row.
    pub persistent_failures: Vec<CardFailure>,
}

impl CycleSummary {
    /// The counts for the direction that writes to `to`.
    #[must_use]
    pub fn toward(&self, to: Side) -> &DirectionCounts {
        match to {
            Side::ICloud => &self.to_icloud,
            Side::Fastmail => &self.to_fastmail,
        }
    }

    /// What a sync would count if every op in `ops` succeeded. Dry-run
    /// prints this; `fetched` and the error counts stay zero.
    #[must_use]
    pub fn planned(ops: &[Op]) -> Self {
        let mut summary = Self::default();
        for op in ops {
            summary.applied(op);
        }
        summary
    }

    fn toward_mut(&mut self, to: Side) -> &mut DirectionCounts {
        match to {
            Side::ICloud => &mut self.to_icloud,
            Side::Fastmail => &mut self.to_fastmail,
        }
    }

    /// Counts an op that completed.
    pub(super) fn applied(&mut self, op: &Op) {
        match op {
            Op::Create { to, .. } | Op::Resurrect { to, .. } => self.toward_mut(*to).added += 1,
            Op::CopyGroup { .. } => {
                self.to_icloud.added += 1;
                self.to_fastmail.updated += 1;
            }
            Op::Update { to, .. } => self.toward_mut(*to).updated += 1,
            Op::Delete { on, .. } => self.toward_mut(*on).removed += 1,
            Op::Conflict { winner, .. } => self.toward_mut(winner.other()).conflicts += 1,
            Op::Recreate { put_icloud, conflict, .. } => {
                self.to_fastmail.updated += 1;
                if put_icloud.is_some() {
                    self.to_icloud.updated += 1;
                }
                if let Some(conflict) = conflict {
                    self.toward_mut(conflict.winner.other()).conflicts += 1;
                }
            }
            Op::Adopt { .. } => self.adopted += 1,
            Op::Refresh { .. } => self.refreshed += 1,
            Op::Forget { .. } => self.forgotten += 1,
        }
    }

    /// Counts a failed op or an unsyncable card, by the side it would have
    /// written to; `None` for a state-only op.
    pub(super) fn failed(&mut self, to: Option<Side>) {
        match to {
            Some(to) => self.toward_mut(to).errors += 1,
            None => self.state_errors += 1,
        }
    }
}

impl fmt::Display for CycleSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "icloud→fastmail: {}; fastmail→icloud: {}; adopted {}, refreshed {}, forgotten {}, state errors {}, deferred {}, held deletes {}, skipped {}, \
             persistent failures {}",
            self.to_fastmail,
            self.to_icloud,
            self.adopted,
            self.refreshed,
            self.forgotten,
            self.state_errors,
            self.deferred,
            self.held_deletes,
            self.skipped,
            self.persistent_failures.len()
        )
    }
}

/// The side an op writes to; `None` for a state-only op.
pub(super) fn target_side(op: &Op) -> Option<Side> {
    match op {
        Op::Create { to, .. } | Op::Update { to, .. } | Op::Resurrect { to, .. } => Some(*to),
        Op::Delete { on, .. } => Some(*on),
        Op::Conflict { winner, .. } => Some(winner.other()),
        Op::CopyGroup { .. } => Some(Side::ICloud),
        Op::Recreate { .. } => Some(Side::Fastmail),
        Op::Adopt { .. } | Op::Refresh { .. } | Op::Forget { .. } => None,
    }
}

/// `icloud→fastmail` for a write to Fastmail.
pub(super) fn direction(to: Side) -> String {
    format!("{}→{to}", to.other())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_renders_one_line() {
        let summary = CycleSummary {
            to_fastmail: DirectionCounts {
                added: 2,
                ..DirectionCounts::default()
            },
            to_icloud: DirectionCounts {
                errors: 1,
                ..DirectionCounts::default()
            },
            adopted: 1,
            ..CycleSummary::default()
        };

        insta::assert_snapshot!(summary.to_string(), @"icloud→fastmail: fetched 0, added 2, updated 0, removed 0, conflicts 0, errors 0; fastmail→icloud: fetched 0, added 0, updated 0, removed 0, conflicts 0, errors 1; adopted 1, refreshed 0, forgotten 0, state errors 0, deferred 0, held deletes 0, skipped 0, persistent failures 0");
        assert_eq!(direction(Side::ICloud), "fastmail→icloud");
    }

    #[test]
    fn a_group_copy_counts_as_an_icloud_add_and_a_fastmail_update() {
        let card = crate::contact::VCard::parse("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:g1\r\nFN:Team\r\nEND:VCARD\r\n").unwrap();
        let op = Op::CopyGroup {
            uid: crate::contact::Uid::from("g1"),
            source: crate::sync::Resource {
                href: crate::contact::Href::from("/dav/g.vcf"),
                etag: crate::contact::ETag::from("f1"),
            },
            rewritten: card.clone(),
            synced: crate::sync::SyncedCard::recorded(&card),
            relinked: 1,
        };

        let summary = CycleSummary::planned(std::slice::from_ref(&op));

        assert_eq!((summary.to_icloud.added, summary.to_fastmail.updated), (1, 1));
        assert_eq!(target_side(&op), Some(Side::ICloud));
    }
}
