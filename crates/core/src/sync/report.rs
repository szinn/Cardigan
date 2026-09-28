use std::{collections::HashSet, fmt};

use super::{Diagnostic, Op, PairPass, Skip};
use crate::{
    contact::{DisplayIdentity, Href, Side, Uid, VCard},
    state::ConflictOrigin,
};

/// A paired contact in the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportPair {
    pub uid: Uid,
    pub identity: DisplayIdentity,
    /// The configured winner, for conflicts and identity pairs.
    pub winner: Option<Side>,
}

impl ReportPair {
    fn new(uid: &Uid, card: &VCard, winner: Option<Side>) -> Self {
        Self {
            uid: uid.clone(),
            identity: card.display_identity(),
            winner,
        }
    }
}

impl fmt::Display for ReportPair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} uid={}", self.identity, self.uid)?;
        match self.winner {
            Some(winner) => write!(f, ", {winner} wins"),
            None => Ok(()),
        }
    }
}

/// A card copied to the other side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportCopy {
    pub uid: Uid,
    pub identity: DisplayIdentity,
    pub to: Side,
    /// The card has no usable name, so it could not be matched by identity.
    pub no_name: bool,
}

impl ReportCopy {
    fn new(uid: &Uid, card: &VCard, to: Side) -> Self {
        let identity = card.display_identity();
        Self {
            uid: uid.clone(),
            no_name: identity.name().is_none(),
            identity,
            to,
        }
    }
}

impl fmt::Display for ReportCopy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} uid={} → {}", self.identity, self.uid, self.to)?;
        if self.no_name {
            f.write_str(" (no name to match on)")?;
        }
        Ok(())
    }
}

/// What pairing will do this cycle, for `dry-run` and the cycle summary.
/// Built from the plan alone, so a dry run and a real run report the same
/// thing. PII-safe: names, organizations, UIDs and hrefs only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BaselineReport {
    pub in_sync: Vec<ReportPair>,
    pub conflicts: Vec<ReportPair>,
    pub reuid: Vec<ReportPair>,
    pub by_identity: Vec<ReportPair>,
    pub skipped: Vec<Skip>,
    pub copies: Vec<ReportCopy>,
    pub unreadable: Vec<(Side, Href)>,
    /// Cards CG-6 held instead of syncing: `Diagnostic::DuplicateUid` and
    /// `Diagnostic::UidChanged` entries, by side and UID (M2). Never appear
    /// in any op, so without this the user would have no way to learn a
    /// card isn't syncing.
    pub held: Vec<Diagnostic>,
    /// Deletes CG-17 held because they may remove one contact across two
    /// pairs (`Diagnostic::DeleteHeld`).
    pub held_deletes: Vec<Diagnostic>,
    /// Fastmail-only groups being copied that list a UID pairing replaced
    /// (Decision 7: fixed by hand in v1). A group here may also legitimately
    /// appear under `copies` — this list only flags which of those copies
    /// need a by-hand membership fix; membership on both sides stays stale
    /// until CG-14 rewrites it.
    pub groups_with_reuid_members: Vec<ReportCopy>,
}

impl BaselineReport {
    pub fn build(ops: &[Op], skips: &[Skip], diagnostics: &[Diagnostic]) -> Self {
        let mut report = Self {
            skipped: skips.to_vec(),
            ..Self::default()
        };
        let mut replaced_fastmail_uids: HashSet<&Uid> = HashSet::new();
        for op in ops {
            match op {
                Op::Adopt { uid, synced, .. } => report.in_sync.push(ReportPair::new(uid, &synced.card, None)),
                Op::Conflict {
                    uid,
                    origin: ConflictOrigin::Baseline,
                    winner,
                    synced,
                    ..
                } => report.conflicts.push(ReportPair::new(uid, &synced.card, Some(*winner))),
                Op::Recreate {
                    uid,
                    pass,
                    fastmail_uid,
                    synced,
                    conflict,
                    ..
                } => {
                    replaced_fastmail_uids.insert(fastmail_uid);
                    let line = ReportPair::new(uid, &synced.card, conflict.as_ref().map(|c| c.winner));
                    match pass {
                        PairPass::Content => report.reuid.push(line),
                        PairPass::Identity => report.by_identity.push(line),
                    }
                }
                Op::Create { uid, to, synced, .. } => report.copies.push(ReportCopy::new(uid, &synced.card, *to)),
                _ => {}
            }
        }
        report.groups_with_reuid_members = ops
            .iter()
            .filter_map(|op| match op {
                Op::Create {
                    uid, to: Side::ICloud, synced, ..
                } if synced.card.is_group() && synced.card.member_uids().iter().any(|m| replaced_fastmail_uids.contains(m)) => {
                    Some(ReportCopy::new(uid, &synced.card, Side::ICloud))
                }
                _ => None,
            })
            .collect();
        report.unreadable = diagnostics
            .iter()
            .filter_map(|d| match d {
                Diagnostic::Unreadable { side, href, .. } => Some((*side, href.clone())),
                _ => None,
            })
            .collect();
        report.held = diagnostics
            .iter()
            .filter(|d| matches!(d, Diagnostic::DuplicateUid { .. } | Diagnostic::UidChanged { .. }))
            .cloned()
            .collect();
        report.held_deletes = diagnostics.iter().filter(|d| matches!(d, Diagnostic::DeleteHeld { .. })).cloned().collect();
        report
    }
}

impl fmt::Display for BaselineReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "in sync: {}, conflicts: {}, re-UID'd: {}, paired by identity: {}, skipped: {}, to copy: {}",
            self.in_sync.len(),
            self.conflicts.len(),
            self.reuid.len(),
            self.by_identity.len(),
            self.skipped.len(),
            self.copies.len()
        )?;
        section(f, "In sync (same UID, same content):", &self.in_sync)?;
        section(f, "Conflicts (same UID, different content):", &self.conflicts)?;
        section(f, "Re-UID'd (same content; the Fastmail card takes the iCloud UID):", &self.reuid)?;
        section(f, "Paired by identity (the Fastmail card takes the iCloud UID):", &self.by_identity)?;
        if !self.skipped.is_empty() {
            writeln!(f, "Skipped, never guessed (edit either card to resolve):")?;
            for skip in &self.skipped {
                write!(f, "  {} {}: {} candidates", skip.side, skip.identity, skip.candidate_count)?;
                if skip.candidate_count == 0 {
                    f.write_str(" (same name, nothing shared)")?;
                }
                let names: Vec<String> = skip.candidates.iter().map(ToString::to_string).collect();
                writeln!(f, ": {}", names.join("; "))?;
            }
        }
        section(f, "To copy:", &self.copies)?;
        if !self.unreadable.is_empty() {
            writeln!(f, "Unreadable (their counterparts may be copied):")?;
            for (side, href) in &self.unreadable {
                writeln!(f, "  {side} {href}")?;
            }
        }
        if !self.held.is_empty() {
            writeln!(f, "Held (not synced):")?;
            for diagnostic in &self.held {
                writeln!(f, "  {diagnostic}")?;
            }
        }
        if !self.held_deletes.is_empty() {
            writeln!(
                f,
                "Held deletes (may be one contact deleted across two pairs; restore a copy to keep it, or delete every remaining copy):"
            )?;
            for diagnostic in &self.held_deletes {
                if let Diagnostic::DeleteHeld { on, uid, with, identity } = diagnostic {
                    writeln!(f, "  {identity} uid={uid}: delete on {on}, with uid={with}")?;
                }
            }
        }
        section(
            f,
            "Groups listing re-UID'd members (membership stale until CG-14):",
            &self.groups_with_reuid_members,
        )
    }
}

fn section<T: fmt::Display>(f: &mut fmt::Formatter<'_>, title: &str, lines: &[T]) -> fmt::Result {
    if lines.is_empty() {
        return Ok(());
    }
    writeln!(f, "{title}")?;
    for line in lines {
        writeln!(f, "  {line}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        contact::{ETag, VCardError},
        sync::fixtures::card_with,
    };

    #[test]
    fn held_section_lists_duplicate_and_reuid_diagnostics() {
        let diagnostics = vec![
            Diagnostic::DuplicateUid {
                side: Side::ICloud,
                uid: Uid::from("u1"),
                hrefs: vec![Href::from("/i/a.vcf"), Href::from("/i/b.vcf")],
            },
            Diagnostic::UidChanged {
                side: Side::Fastmail,
                href: Href::from("/f/u2.vcf"),
                etag: ETag::from("f2"),
                stored: Uid::from("u2"),
                found: Uid::from("u3"),
            },
            Diagnostic::Unreadable {
                side: Side::Fastmail,
                href: Href::from("/f/bad.vcf"),
                etag: ETag::from("b1"),
                error: VCardError::MissingUid,
            },
        ];

        let report = BaselineReport::build(&[], &[], &diagnostics);

        assert_eq!(report.held.len(), 2);
        insta::assert_snapshot!(report.to_string(), @r"
        in sync: 0, conflicts: 0, re-UID'd: 0, paired by identity: 0, skipped: 0, to copy: 0
        Unreadable (their counterparts may be copied):
          fastmail /f/bad.vcf
        Held (not synced):
          duplicate uid=u1 on icloud: /i/a.vcf, /i/b.vcf
          uid changed on fastmail /f/u2.vcf@f2: u2 → u3
        ");
    }

    #[test]
    fn no_held_diagnostics_means_no_held_section() {
        let report = BaselineReport::build(&[], &[], &[]);

        assert_eq!(report.held, []);
        assert!(!report.to_string().contains("Held"));
    }

    #[test]
    fn held_deletes_section_names_the_contact_and_both_uids() {
        let identity = card_with("b", "Harbor Grill", "EMAIL:hg@example.com\r\n").display_identity();
        let diagnostics = vec![
            Diagnostic::DeleteHeld {
                on: Side::Fastmail,
                uid: Uid::from("a"),
                with: Uid::from("b"),
                identity: card_with("a", "", "ORG:Harbor Grill\r\n").display_identity(),
            },
            Diagnostic::DeleteHeld {
                on: Side::ICloud,
                uid: Uid::from("b"),
                with: Uid::from("a"),
                identity,
            },
        ];

        let report = BaselineReport::build(&[], &[], &diagnostics);

        assert_eq!(report.held_deletes.len(), 2);
        assert_eq!(report.held, [], "held deletes are not in the Held section");
        insta::assert_snapshot!(report.to_string(), @r"
        in sync: 0, conflicts: 0, re-UID'd: 0, paired by identity: 0, skipped: 0, to copy: 0
        Held deletes (may be one contact deleted across two pairs; restore a copy to keep it, or delete every remaining copy):
          <no name> (Harbor Grill) uid=a: delete on fastmail, with uid=b
          Harbor Grill uid=b: delete on icloud, with uid=a
        ");
    }
}
