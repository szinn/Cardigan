use super::{BaselineReport, KnownCards, Plan, PlanInput, Planned, Skip, guard, pairing, planner, relink};

/// One cycle's complete plan: the planner's ops for synced contacts, then
/// pairing's for unsynced cards.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CyclePlan {
    pub plan: Plan,
    /// The latest skips, for `baseline_skips` (`replace_all`).
    pub skips: Vec<Skip>,
    pub report: BaselineReport,
}

/// Runs the planner, holds cross-pair deletes (`hold_cross_deletes`), and then
/// runs pairing and relinks the groups it copies (CG-14). CG-8 calls this every
/// cycle (an empty state store makes it the initial baseline) and checks
/// `check_deletions` on the result before writing.
pub fn plan_cycle(input: &PlanInput<'_>) -> CyclePlan {
    let Planned { mut plan, unsynced } = planner::plan(input);
    guard::hold_cross_deletes(&mut plan, input.state);
    let known = KnownCards::collect(input.icloud, input.fastmail, input.state);
    let paired = pairing::pair(&unsynced, &known, input.winner);
    plan.ops.extend(paired.ops);
    let aliases = relink::aliases(&plan.ops, input.replayed);
    relink::relink_groups(&mut plan.ops, &aliases, input.fastmail);
    let mut report = BaselineReport::build(&plan.ops, &paired.skips, &plan.diagnostics);
    report.synced_duplicates = super::synced_duplicates(input.state);
    CyclePlan {
        plan,
        skips: paired.skips,
        report,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        contact::{ETag, Href, Side, VCard, VCardError},
        state::ContactState,
        sync::{
            Entry, SkipReason, Snapshot,
            fixtures::{card, card_with, fetched, row, unchanged},
        },
    };

    /// Every card fetched on side `s` at `/{s}/{uid}.vcf`, ETag `{s}-{uid}`.
    fn side(s: &str, cards: &[VCard]) -> Snapshot {
        cards
            .iter()
            .map(|c| {
                let uid = c.uid().as_str();
                (Href::from(format!("/{s}/{uid}.vcf")), fetched(&format!("{s}-{uid}"), c.clone()))
            })
            .collect()
    }

    fn cycle(icloud: &Snapshot, fastmail: &Snapshot, state: &[ContactState], winner: Side) -> CyclePlan {
        plan_cycle(&PlanInput {
            icloud,
            fastmail,
            state,
            winner,
            replayed: &[],
        })
    }

    fn render(plan: &Plan) -> String {
        if plan.ops.is_empty() && plan.diagnostics.is_empty() {
            "(nothing)".to_owned()
        } else {
            plan.to_string().trim_end().replace('\n', "; ")
        }
    }

    /// The state-absent half of the spec's matrix. With no state row a side
    /// can only be "new" or absent ("changed", "unchanged" and "deleted" are
    /// defined against state), so the other 12 labels are unreachable.
    #[test]
    fn state_absent_matrix() {
        let jane = card("u1", "Jane Doe");
        let other = card_with("u1", "Jane Doe", "NOTE:other\r\n");
        let cases: [(&str, Vec<VCard>, Vec<VCard>, Side); 6] = [
            ("new/absent", vec![jane.clone()], vec![], Side::ICloud),
            ("absent/new", vec![], vec![jane.clone()], Side::ICloud),
            ("new/new same", vec![jane.clone()], vec![jane.clone()], Side::ICloud),
            ("new/new different, icloud wins", vec![jane.clone()], vec![other.clone()], Side::ICloud),
            ("new/new different, fastmail wins", vec![jane.clone()], vec![other], Side::Fastmail),
            ("absent/absent", vec![], vec![], Side::ICloud),
        ];
        let lines: Vec<String> = cases
            .into_iter()
            .map(|(label, i, f, winner)| format!("{label}: {}", render(&cycle(&side("i", &i), &side("f", &f), &[], winner).plan)))
            .collect();
        insta::assert_snapshot!(lines.join("\n"), @r"
        new/absent: create fastmail uid=u1 from=/i/u1.vcf
        absent/new: create icloud uid=u1 from=/f/u1.vcf
        new/new same: adopt uid=u1 icloud=/i/u1.vcf@i-u1 fastmail=/f/u1.vcf@f-u1
        new/new different, icloud wins: conflict(baseline) icloud wins uid=u1 → fastmail /f/u1.vcf@f-u1
        new/new different, fastmail wins: conflict(baseline) fastmail wins uid=u1 → icloud /i/u1.vcf@i-u1
        absent/absent: (nothing)
        ");
    }

    fn baseline_icloud() -> Vec<VCard> {
        vec![
            card_with("ic-3", "Ann Lee", "EMAIL:ann@example.com\r\n"),
            card_with("ic-4", "Bo Ray", "EMAIL:bo@example.com\r\nNOTE:icloud\r\n"),
            card_with("ic-5", "Sam Poe", "EMAIL:sam@one.example\r\n"),
            card("ic-6", "Pat Kim"),
            card("u1", "Jane Doe"),
            card_with("u2", "John Roe", "NOTE:icloud\r\n"),
        ]
    }

    fn baseline_fastmail() -> Vec<VCard> {
        vec![
            card_with("fm-3", "Ann Lee", "EMAIL:ann@example.com\r\n"),
            card_with("fm-4", "Bo Ray", "EMAIL:bo@example.com\r\nNOTE:fastmail\r\n"),
            card_with("fm-5", "Sam Poe", "EMAIL:sam@two.example\r\n"),
            card("fm-7", "jane@example.org"),
            card_with(
                "fm-g",
                "Family",
                "X-ADDRESSBOOKSERVER-KIND:group\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:fm-3\r\n",
            ),
            card_with("u1", "Jane Doe", "REV:2026-09-26T00:00:00Z\r\n"),
            card_with("u2", "John Roe", "NOTE:fastmail\r\n"),
        ]
    }

    #[test]
    fn full_baseline_plan_and_report() {
        let icloud = side("i", &baseline_icloud());
        let mut fastmail = side("f", &baseline_fastmail());
        fastmail.insert(
            Href::from("/f/bad.vcf"),
            Entry::Fetched {
                etag: ETag::from("f-bad"),
                card: Err(VCardError::MissingUid),
            },
        );

        let planned = cycle(&icloud, &fastmail, &[], Side::ICloud);

        insta::assert_snapshot!(planned.plan.to_string(), @r"
        adopt uid=u1 icloud=/i/u1.vcf@i-u1 fastmail=/f/u1.vcf@f-u1
        conflict(baseline) icloud wins uid=u2 → fastmail /f/u2.vcf@f-u2
        recreate(content) uid=ic-3 fastmail /f/fm-3.vcf@f-fm-3 was fm-3
        recreate(identity) uid=ic-4 fastmail /f/fm-4.vcf@f-fm-4 was fm-4 icloud wins
        create fastmail uid=ic-6 from=/i/ic-6.vcf
        create icloud uid=fm-7 from=/f/fm-7.vcf
        copy-group icloud uid=fm-g from=/f/fm-g.vcf@f-fm-g relinked=1
        ! unreadable fastmail /f/bad.vcf@f-bad: vCard has no UID
        ");
        let report = planned.report.to_string();
        assert!(!report.contains("@example"), "PII leaked into the report: {report}");
        insta::assert_snapshot!(report, @r"
        in sync: 1, conflicts: 1, re-UID'd: 1, paired by identity: 1, skipped: 2, likely duplicates: 0, to copy: 3
        In sync (same UID, same content):
          Jane Doe uid=u1
        Conflicts (same UID, different content):
          John Roe uid=u2, icloud wins
        Re-UID'd (same content; the Fastmail card takes the iCloud UID):
          Ann Lee uid=ic-3
        Paired by identity (the Fastmail card takes the iCloud UID):
          Bo Ray uid=ic-4, icloud wins
        Skipped, never guessed (edit either card to resolve):
          icloud Sam Poe: 0 candidates (same name, nothing shared): Sam Poe
          fastmail Sam Poe: 0 candidates (same name, nothing shared): Sam Poe
        To copy:
          Pat Kim uid=ic-6 → fastmail
          <no name> uid=fm-7 → icloud (no name to match on)
          Family uid=fm-g → icloud
        Unreadable (their counterparts may be copied):
          fastmail /f/bad.vcf
        ");
        assert_eq!(planned.skips.len(), 2);
    }

    #[test]
    fn empty_report_is_the_counts_line() {
        let planned = cycle(&Snapshot::new(), &Snapshot::new(), &[], Side::ICloud);

        assert_eq!(
            planned.report.to_string(),
            "in sync: 0, conflicts: 0, re-UID'd: 0, paired by identity: 0, skipped: 0, likely duplicates: 0, to copy: 0\n"
        );
    }

    #[test]
    fn resume_after_recreate_delete_copies_the_icloud_card() {
        // Crash between DELETE of fm-3 and its recreate: only iCloud holds
        // it. Pure `sync` has no memory of the deleted Fastmail card's own
        // bytes, so pairing copies the iCloud card instead. This is the
        // fallback CG-8 hits only when its durably recorded
        // `create_fastmail` (Op::Recreate's doc, I2) is missing; normally the
        // service replays that record first (CG-16's journal) and this path
        // runs only when the server rejects the journaled bytes.
        let icloud = side("i", &[card_with("ic-3", "Ann Lee", "EMAIL:ann@example.com\r\n")]);

        let planned = cycle(&icloud, &Snapshot::new(), &[], Side::ICloud);

        assert_eq!(render(&planned.plan), "create fastmail uid=ic-3 from=/i/ic-3.vcf");
    }

    #[test]
    fn resume_after_recreate_create_adopts() {
        // Crash after the recreate, before the state write: same UID both
        // sides.
        let ann = card_with("ic-3", "Ann Lee", "EMAIL:ann@example.com\r\n");

        let planned = cycle(&side("i", std::slice::from_ref(&ann)), &side("f", &[ann]), &[], Side::ICloud);

        assert_eq!(render(&planned.plan), "adopt uid=ic-3 icloud=/i/ic-3.vcf@i-ic-3 fastmail=/f/ic-3.vcf@f-ic-3");
    }

    #[test]
    fn resume_after_fastmail_wins_icloud_put_pairs_by_content() {
        // Crash after the iCloud PUT: both hold the same content, UIDs differ.
        let body = "EMAIL:bo@example.com\r\nNOTE:fastmail\r\n";

        let planned = cycle(
            &side("i", &[card_with("ic-4", "Bo Ray", body)]),
            &side("f", &[card_with("fm-4", "Bo Ray", body)]),
            &[],
            Side::Fastmail,
        );

        assert_eq!(render(&planned.plan), "recreate(content) uid=ic-4 fastmail /f/fm-4.vcf@f-fm-4 was fm-4");
    }

    #[test]
    fn synced_contacts_and_pairing_share_a_cycle() {
        let jane = card("u1", "Jane Doe");
        let state = vec![row(1, &jane, ("/i/u1.vcf", "i1"), ("/f/u1.vcf", "f1"))];
        let ann = |uid: &str| card_with(uid, "Ann Lee", "EMAIL:ann@example.com\r\n");
        let mut icloud = side("i", &[ann("ic-3")]);
        icloud.insert(Href::from("/i/u1.vcf"), unchanged("i1"));
        let mut fastmail = side("f", &[ann("fm-3")]);
        fastmail.insert(Href::from("/f/u1.vcf"), unchanged("f1"));

        let planned = cycle(&icloud, &fastmail, &state, Side::ICloud);

        assert_eq!(render(&planned.plan), "recreate(content) uid=ic-3 fastmail /f/fm-3.vcf@f-fm-3 was fm-3");
    }

    #[test]
    fn a_merged_delete_across_two_pairs_is_held() {
        let a = card_with("a", "", "ORG:Harbor Grill\r\nTEL:+1 555 0100\r\n");
        let b = card_with("b", "Harbor Grill", "TEL:+1 555 0100\r\n");
        let state = vec![
            row(1, &a, ("/i/a.vcf", "i-a"), ("/f/a.vcf", "f-a")),
            row(2, &b, ("/i/b.vcf", "i-b"), ("/f/b.vcf", "f-b")),
        ];
        // a's iCloud card and b's Fastmail card are gone; the others are
        // unchanged.
        let mut icloud = Snapshot::new();
        icloud.insert(Href::from("/i/b.vcf"), unchanged("i-b"));
        let mut fastmail = Snapshot::new();
        fastmail.insert(Href::from("/f/a.vcf"), unchanged("f-a"));

        let planned = cycle(&icloud, &fastmail, &state, Side::ICloud);

        assert_eq!(planned.plan.ops, []);
        assert_eq!(planned.plan.diagnostics.len(), 2, "{}", planned.plan);
        let report = planned.report.to_string();
        assert!(report.contains("  Harbor Grill uid=b: delete on icloud, with uid=a"), "{report}");
        assert!(!report.contains("555"), "PII leaked into the report: {report}");
    }

    #[test]
    fn a_new_nameless_duplicate_of_a_synced_card_is_skipped() {
        // The synced card is Unchanged (not fetched): only its state row
        // makes it known.
        let kw = card_with("u1", "KW pharmacy", "TEL:+1 555 0100\r\n");
        let state = vec![row(1, &kw, ("/i/u1.vcf", "i1"), ("/f/u1.vcf", "f1"))];
        let mut icloud = Snapshot::new();
        icloud.insert(Href::from("/i/u1.vcf"), unchanged("i1"));
        let mut fastmail = Snapshot::new();
        fastmail.insert(Href::from("/f/u1.vcf"), unchanged("f1"));
        fastmail.insert(
            Href::from("/f/dup.vcf"),
            fetched("f-dup", card_with("fm-2", "", "ORG:KW pharmacy\r\nTEL:+1 555 0100\r\n")),
        );

        let planned = cycle(&icloud, &fastmail, &state, Side::ICloud);

        assert_eq!(render(&planned.plan), "(nothing)");
        let reasons: Vec<(&str, SkipReason)> = planned.skips.iter().map(|s| (s.uid.as_str(), s.reason)).collect();
        assert_eq!(reasons, [("fm-2", SkipReason::LikelyDuplicate)]);
    }

    #[test]
    fn the_report_lists_synced_nameless_duplicates() {
        let kw = card_with("u1", "KW pharmacy", "TEL:+1 555 0100\r\n");
        let dup = card_with("u2", "", "ORG:KW pharmacy\r\nTEL:+1 555 0100\r\n");
        let state = vec![
            row(1, &kw, ("/i/u1.vcf", "i1"), ("/f/u1.vcf", "f1")),
            row(2, &dup, ("/i/u2.vcf", "i2"), ("/f/u2.vcf", "f2")),
        ];
        let side = |s: &str| -> Snapshot {
            [("u1", "1"), ("u2", "2")]
                .into_iter()
                .map(|(uid, n)| (Href::from(format!("/{s}/{uid}.vcf")), unchanged(&format!("{s}{n}"))))
                .collect()
        };

        let planned = cycle(&side("i"), &side("f"), &state, Side::ICloud);

        assert_eq!(render(&planned.plan), "(nothing)");
        assert!(
            planned.report.to_string().contains("  <no name> (KW pharmacy) uid=u2: like KW pharmacy uid=u1"),
            "{}",
            planned.report
        );
    }
}
