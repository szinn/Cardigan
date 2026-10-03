//! Finishing a Recreate that a crash or cycle-fatal error interrupted after
//! it deleted the old Fastmail card (CG-8 Decision 11, CG-16).

use std::collections::HashSet;

use super::{Collections, SyncService, executor::is_cycle_fatal, listing::list_side};
use crate::{
    Error,
    addressbook::Precondition,
    contact::{Href, Uid, VCard},
    state::{FailureReason, PendingRecreate},
    with_transaction,
};

impl SyncService {
    /// For each journaled Recreate, in id order:
    /// - new card present: it finished; drop the row (pairing adopts it);
    /// - old card present: the DELETE never happened; drop the row (pairing
    ///   plans the Recreate again);
    /// - neither present, and the iCloud card behind the recreate is gone too:
    ///   the user deleted it during the crash window; drop the row without a
    ///   PUT, so replay does not resurrect it on Fastmail;
    /// - neither present, but the iCloud card is still there: PUT the journaled
    ///   Fastmail bytes at the new href, then drop the row. Without this,
    ///   pairing would copy the iCloud card and lose Fastmail's own content,
    ///   photo included.
    ///
    /// A cycle-fatal error aborts the cycle and keeps the row. Any other PUT
    /// error keeps the row with a warning; pairing may then copy the iCloud
    /// card to the same href, and the next replay drops the row.
    ///
    /// Returns (old Fastmail UID, iCloud UID) for every row whose old
    /// Fastmail card is gone: that UID is dead and the iCloud UID is the
    /// contact's only name, whichever way the row ended. `plan_cycle` relinks
    /// groups with it (CG-14).
    pub(super) async fn replay(&self, pending: &[PendingRecreate], collections: &Collections) -> Result<Vec<(Uid, Uid)>, Error> {
        let fastmail_listing = list_side(&*self.fastmail, &collections.fastmail)
            .await
            .inspect_err(|error| self.after_listing_error(error))?;
        let icloud_listing = list_side(&*self.icloud, &collections.icloud)
            .await
            .inspect_err(|error| self.after_listing_error(error))?;
        let listed: HashSet<&Href> = fastmail_listing.entries.iter().map(|(href, _)| href).collect();
        let icloud_listed: HashSet<&Href> = icloud_listing.entries.iter().map(|(href, _)| href).collect();
        let mut replayed = Vec::new();
        for entry in pending {
            if !listed.contains(&entry.old_fastmail_href) {
                replayed.push((entry.old_fastmail_uid.clone(), entry.uid.clone()));
            }
            let recreate_finished = listed.contains(&entry.new_fastmail_href) || listed.contains(&entry.old_fastmail_href);
            if !recreate_finished {
                if icloud_listed.contains(&entry.icloud_href) {
                    let record = VCard::parse(entry.card.clone()).map_or_else(|_| "<unknown>".to_owned(), |card| card.display_identity().to_string());
                    match self.fastmail.put(&entry.new_fastmail_href, &entry.card, Precondition::IfNoneMatch).await {
                        Ok(_) => tracing::info!(record = ?record, uid = %entry.uid, "finished an interrupted recreate"),
                        Err(error) if is_cycle_fatal(&error) => return Err(error),
                        Err(error) => {
                            tracing::warn!(
                                record = ?record,
                                uid = %entry.uid,
                                reason = FailureReason::from(&error).as_str(),
                                "could not finish an interrupted recreate; retrying next cycle"
                            );
                            continue;
                        }
                    }
                } else {
                    tracing::info!(uid = %entry.uid, "dropped an interrupted recreate: the contact was deleted on iCloud");
                }
            }
            let uid = entry.uid.clone();
            with_transaction!(self, pending_recreate_repository, |tx| pending_recreate_repository
                .delete(tx, &uid)
                .await
                .map(|_| ()))?;
        }
        Ok(replayed)
    }
}
