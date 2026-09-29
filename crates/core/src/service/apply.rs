//! Running a planned cycle: diagnostics first, then every op in order.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};

use super::{
    Collections, PERSISTENT_ATTEMPTS, SyncService,
    executor::{Written, failed_cards, holds_on_abort, is_cycle_fatal},
    listing::{Built, Listed, Stored},
    summary::{CycleSummary, direction, target_side},
};
use crate::{
    Error,
    addressbook::SyncToken,
    contact::{CANONICAL_VERSION, ETag, Href, Side, Uid},
    state::{CardFailure, ContactState, FailedCard, FailureOp, FailureReason, NewBaselineSkip},
    sync::{CyclePlan, Diagnostic, Op, SkipReason},
    with_transaction,
};

/// One cycle's bookkeeping.
pub(super) struct Run<'a> {
    pub(super) summary: CycleSummary,
    /// Synced contacts by UID, to name ops that carry no card.
    contacts: HashMap<&'a Uid, &'a ContactState>,
    /// Every `(side, href)` a failure was recorded for this cycle.
    pub(super) failed: HashSet<(Side, Href)>,
}

impl<'a> Run<'a> {
    fn new(stored: &'a Stored, built: &Built) -> Self {
        let mut summary = CycleSummary::default();
        summary.to_fastmail.fetched = built.fetched_on(Side::ICloud);
        summary.to_icloud.fetched = built.fetched_on(Side::Fastmail);
        Self {
            summary,
            contacts: stored.contacts.iter().map(|row| (&row.uid, row)).collect(),
            failed: HashSet::new(),
        }
    }

    /// The op's name and company: the only contact data a log line carries.
    pub(super) fn identity(&self, op: &Op) -> String {
        let card = match op {
            Op::Create { synced, .. }
            | Op::Update { synced, .. }
            | Op::Conflict { synced, .. }
            | Op::Resurrect { synced, .. }
            | Op::Adopt { synced, .. }
            | Op::Recreate { synced, .. }
            | Op::Refresh { synced: Some(synced), .. } => Some(&synced.card),
            Op::Delete { .. } | Op::Refresh { synced: None, .. } | Op::Forget { .. } => self.contacts.get(op.uid()).map(|row| &row.last_synced_vcard),
        };
        card.map_or_else(|| "<unknown>".to_owned(), |card| card.display_identity().to_string())
    }

    fn applied(&mut self, op: &Op) {
        self.summary.applied(op);
        if let (Some(log_op), Some(to)) = (op.log_op(), target_side(op)) {
            tracing::info!(record = ?self.identity(op), uid = %op.uid(), direction = %direction(to), op = log_op, "synced");
        }
    }
}

/// A read failure for a card the planner could not use.
fn read_failure(side: Side, href: &Href, uid: Option<&Uid>, etag: Option<ETag>, reason: FailureReason) -> FailedCard {
    FailedCard {
        side,
        href: href.clone(),
        uid: uid.cloned(),
        op: FailureOp::Read,
        etag,
        reason,
    }
}

impl SyncService {
    /// Records the plan's diagnostics, then runs every op. A failing op is
    /// recorded and skipped; a cycle-fatal error stops the loop (Decision 6).
    pub(super) async fn apply(
        &self,
        cycle: &CyclePlan,
        stored: &Stored,
        built: &Built,
        listed: &Listed,
        collections: &Collections,
        now: DateTime<Utc>,
    ) -> Result<CycleSummary, Error> {
        let mut run = Run::new(stored, built);
        self.record_diagnostics(&cycle.plan.diagnostics, listed, &mut run, now).await?;
        for op in &cycle.plan.ops {
            let mut written = Written::new();
            let Err(error) = self.execute(op, collections, now, &mut written).await else {
                run.applied(op);
                continue;
            };
            let fatal = is_cycle_fatal(&error);
            if !fatal {
                self.record_op_failure(op, &written, &error, &mut run, now).await?;
            } else if holds_on_abort(op) {
                // Recording the held cards must not cost us the original
                // fatal error (m1): a state-store failure here would
                // otherwise replace e.g. `Unauthorized`, hiding why the
                // cycle actually aborted.
                if let Err(record_error) = self.record_op_failure(op, &written, &error, &mut run, now).await {
                    tracing::warn!(
                        reason = FailureReason::from(&record_error).as_str(),
                        "failed to record the held card; retrying next interval"
                    );
                }
            }
            if fatal {
                tracing::warn!(
                    uid = %op.uid(),
                    reason = FailureReason::from(&error).as_str(),
                    "sync cycle aborted; retrying next interval"
                );
                return Err(error);
            }
        }
        let failures = self.finish(cycle, stored, built, listed, &run.failed, now).await?;
        run.summary.skipped = cycle.skips.len();
        run.summary.persistent_failures = failures.into_iter().filter(|failure| failure.attempts >= PERSISTENT_ATTEMPTS).collect();
        log_cycle(cycle, &run.summary);
        Ok(run.summary)
    }

    /// Persists what the cycle learned, in one transaction, once every op
    /// ran: the latest skips, when each synced card was last listed, failures
    /// that no longer apply (Decision 8), and the listing's sync tokens.
    /// Returns every card failure, for the summary.
    ///
    /// Decision 8's sweep clears a pre-cycle failure row when the planner
    /// saw the card this cycle and it did not fail again: either its href is
    /// no longer listed on its side, or it was not held this cycle
    /// (`!built.held`, fetched or unchanged both count — a synced card can
    /// sit unchanged at its own href forever and must still clear once its
    /// group is released, or `idle` would never see it as resolved).
    async fn finish(
        &self,
        cycle: &CyclePlan,
        stored: &Stored,
        built: &Built,
        listed: &Listed,
        failed: &HashSet<(Side, Href)>,
        now: DateTime<Utc>,
    ) -> Result<Vec<CardFailure>, Error> {
        let skips: Vec<NewBaselineSkip> = cycle
            .skips
            .iter()
            .map(|skip| NewBaselineSkip {
                side: skip.side,
                href: skip.resource.href.clone(),
                uid: skip.uid.clone(),
                content_hash: skip.content_hash,
                hash_version: CANONICAL_VERSION,
                candidate_count: skip.candidate_count,
                skipped_at: now,
            })
            .collect();
        let seen = |side: Side| -> Vec<Uid> {
            let listing: HashSet<&Href> = listed.side(side).entries.iter().map(|(href, _)| href).collect();
            stored
                .contacts
                .iter()
                .filter(|row| listing.contains(&row.side(side).href))
                .map(|row| row.uid.clone())
                .collect()
        };
        let (seen_icloud, seen_fastmail) = (seen(Side::ICloud), seen(Side::Fastmail));
        let resolved: Vec<(Side, Href)> = stored
            .failures
            .iter()
            .map(|failure| (failure.side, failure.href.clone()))
            .filter(|key| !failed.contains(key) && (listed.side(key.0).etag(&key.1).is_none() || !built.held.contains(key)))
            .collect();
        let tokens = [
            (Side::ICloud, listed.icloud.token.clone().map(SyncToken::into_string)),
            (Side::Fastmail, listed.fastmail.token.clone().map(SyncToken::into_string)),
        ];
        with_transaction!(
            self,
            baseline_skip_repository,
            contact_state_repository,
            card_failure_repository,
            endpoint_repository,
            |tx| {
                baseline_skip_repository.replace_all(tx, skips).await?;
                contact_state_repository.mark_seen(tx, Side::ICloud, &seen_icloud, now).await?;
                contact_state_repository.mark_seen(tx, Side::Fastmail, &seen_fastmail, now).await?;
                for (side, href) in &resolved {
                    card_failure_repository.clear(tx, *side, href).await?;
                }
                for (side, token) in tokens {
                    endpoint_repository.set_sync_token(tx, side, token).await?;
                }
                card_failure_repository.list_all(tx).await
            }
        )
    }

    async fn record_op_failure(&self, op: &Op, written: &Written, error: &Error, run: &mut Run<'_>, now: DateTime<Utc>) -> Result<(), Error> {
        let reason = FailureReason::from(error);
        let record = run.identity(op);
        run.summary.failed(target_side(op));
        let cards = failed_cards(op, written, reason);
        if cards.is_empty() {
            tracing::warn!(record = ?record, uid = %op.uid(), reason = reason.as_str(), "state update failed; the next cycle re-plans it");
            return Ok(());
        }
        run.failed.extend(cards.iter().map(|card| (card.side, card.href.clone())));
        let policy = self.backoff;
        let attempts = with_transaction!(self, card_failure_repository, |tx| {
            let mut attempts = 0;
            for card in cards {
                attempts = attempts.max(card_failure_repository.record_failure(tx, card, now, &policy).await?.attempts);
            }
            Ok(attempts)
        })?;
        tracing::warn!(record = ?record, uid = %op.uid(), reason = reason.as_str(), attempts, "sync failed; retrying with backoff");
        Ok(())
    }

    /// Unreadable, duplicate and UID-changed cards become card failures;
    /// deferred rows and held deletes are only counted and warned about.
    /// Recorded diagnostics are logged with structured fields, never the
    /// `Diagnostic`'s own `Display` (Decision 12): `Unreadable`
    /// interpolates the `VCardError`, and `UnsupportedVersion` carries the
    /// card's own `VERSION` text (I3).
    async fn record_diagnostics(&self, diagnostics: &[Diagnostic], listed: &Listed, run: &mut Run<'_>, now: DateTime<Utc>) -> Result<(), Error> {
        let mut cards = Vec::new();
        for diagnostic in diagnostics {
            match diagnostic {
                Diagnostic::Unreadable { side, href, etag, error } => {
                    let reason = FailureReason::from(&Error::VCard(error.clone()));
                    tracing::warn!(side = %side, href = %href, reason = reason.as_str(), "card not synced: unreadable");
                    cards.push(read_failure(*side, href, None, Some(etag.clone()), reason));
                    run.summary.failed(Some(side.other()));
                }
                Diagnostic::DuplicateUid { side, uid, hrefs } => {
                    let listing = listed.side(*side);
                    tracing::warn!(
                        side = %side,
                        uid = %uid,
                        count = hrefs.len(),
                        reason = FailureReason::DuplicateUid.as_str(),
                        "card not synced: duplicate uid"
                    );
                    cards.extend(
                        hrefs
                            .iter()
                            .map(|href| read_failure(*side, href, Some(uid), listing.etag(href), FailureReason::DuplicateUid)),
                    );
                    run.summary.failed(Some(side.other()));
                }
                Diagnostic::UidChanged { side, href, etag, found, .. } => {
                    tracing::warn!(side = %side, href = %href, uid = %found, reason = FailureReason::UidChanged.as_str(), "card not synced: uid changed");
                    cards.push(read_failure(*side, href, Some(found), Some(etag.clone()), FailureReason::UidChanged));
                    run.summary.failed(Some(side.other()));
                }
                Diagnostic::UnreadTarget { .. } | Diagnostic::DeletionDeferred { .. } => {
                    // No error text or card content in these variants'
                    // `Display` (Decision 12), so it may be logged directly.
                    run.summary.deferred += 1;
                    tracing::warn!("deferred to a later cycle: {diagnostic}");
                }
                Diagnostic::DeleteHeld { .. } => {
                    // UIDs and sides only in `Display` (Decision 12); the
                    // row stays synced, so no card failure.
                    run.summary.held_deletes += 1;
                    tracing::warn!(
                        "{diagnostic}; edit the copy you want to keep, or delete every remaining copy (deleting only one lets the other held delete go \
                         through); run `cardigan dry-run` to see which contact is held"
                    );
                }
            }
        }
        if cards.is_empty() {
            return Ok(());
        }
        run.failed.extend(cards.iter().map(|card| (card.side, card.href.clone())));
        let policy = self.backoff;
        with_transaction!(self, card_failure_repository, |tx| {
            for card in cards {
                card_failure_repository.record_failure(tx, card, now, &policy).await?;
            }
            Ok(())
        })
    }
}

/// The cycle's log lines beyond the per-op ones (Decision 12).
fn log_cycle(cycle: &CyclePlan, summary: &CycleSummary) {
    let report = &cycle.report;
    let paired =
        !(report.in_sync.is_empty() && report.conflicts.is_empty() && report.reuid.is_empty() && report.by_identity.is_empty() && report.copies.is_empty());
    if paired {
        tracing::info!("baseline pairing:\n{report}");
    }
    for skip in &cycle.skips {
        let record = skip.identity.to_string();
        if skip.reason == SkipReason::LikelyDuplicate {
            tracing::warn!(
                record = ?record,
                uid = %skip.uid,
                side = %skip.side,
                "not synced: a contact with no name shares an email or phone with a contact on the other side, so it is likely a duplicate; delete it, or name it to sync it"
            );
        } else if skip.identity.name().is_none() && !skip.candidates.is_empty() {
            tracing::warn!(
                record = ?record,
                uid = %skip.uid,
                side = %skip.side,
                "not synced: a contact with no name shares an email or phone with a contact with no name on the other side; name either to sync it"
            );
        } else {
            tracing::warn!(
                record = ?record,
                uid = %skip.uid,
                side = %skip.side,
                candidates = skip.candidate_count,
                "not synced: ambiguous match, never guessed; edit either card to resolve"
            );
        }
    }
    for failure in &summary.persistent_failures {
        tracing::warn!(
            side = %failure.side,
            href = %failure.href,
            uid = ?failure.uid.as_ref().map(Uid::as_str),
            op = failure.op.as_str(),
            reason = failure.reason.as_str(),
            attempts = failure.attempts,
            "card keeps failing"
        );
    }
    tracing::info!("sync cycle: {summary}");
}
