//! The photo dimension of planning (CG-15).

use std::collections::{HashMap, HashSet};

use super::{Diagnostic, Entry, Snapshot};
use crate::{
    contact::{CardPhoto, PhotoData, PhotoUri, Uid},
    state::FailureReason,
};

/// iCloud photos downloaded before planning, by URI.
pub type FetchedPhotos = HashMap<PhotoUri, Result<PhotoData, FailureReason>>;

/// UIDs of iCloud cards whose photo download failed, with a diagnostic each.
pub(super) fn blocked_photos(icloud: &Snapshot, photos: &FetchedPhotos) -> (HashSet<Uid>, Vec<Diagnostic>) {
    let mut blocked = HashSet::new();
    let mut diagnostics = Vec::new();
    for (href, entry) in icloud.entries() {
        let Entry::Fetched { etag, card: Ok(card) } = entry else { continue };
        let Some(CardPhoto::Uri(uri)) = card.photo() else { continue };
        if let Some(Err(reason)) = photos.get(&uri) {
            blocked.insert(card.uid().clone());
            diagnostics.push(Diagnostic::PhotoUnavailable {
                href: href.clone(),
                etag: etag.clone(),
                uid: card.uid().clone(),
                reason: *reason,
            });
        }
    }
    (blocked, diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::fixtures::{card_with, fetched, snapshot};

    #[test]
    fn a_failed_download_blocks_the_card_and_reports_it() {
        let uri = "https://gateway.icloud.com/p1";
        let icloud = snapshot([("/i/u1.vcf", fetched("i2", card_with("u1", "Jane", &format!("PHOTO;VALUE=uri:{uri}\r\n"))))]);
        let photos: FetchedPhotos = [(PhotoUri::from(uri.to_owned()), Err(FailureReason::Transient))].into();

        let (blocked, diagnostics) = blocked_photos(&icloud, &photos);

        assert!(blocked.contains(&Uid::from("u1")));
        assert!(matches!(
            diagnostics.as_slice(),
            [Diagnostic::PhotoUnavailable { uid, reason: FailureReason::Transient, .. }] if uid.as_str() == "u1"
        ));
    }
}
