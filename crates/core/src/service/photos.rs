//! Downloading the iCloud photos planning needs (CG-15 R3, R6).

use std::collections::HashMap;

use super::{
    SyncService,
    executor::is_cycle_fatal,
    listing::{Built, Stored},
};
use crate::{
    AddressBookError, Error,
    contact::{CardPhoto, PhotoData},
    state::FailureReason,
    sync::{Entry, FetchedPhotos},
};

/// A photo download failing for the gateway's sake (5xx, timeout) is that
/// contact's failure (R6), unlike a CardDAV transient error: only the
/// account-wide errors end the cycle.
fn is_photo_fatal(error: &Error) -> bool {
    is_cycle_fatal(error) && !matches!(error, Error::AddressBook(AddressBookError::Transient(_)))
}

impl SyncService {
    /// Every iCloud photo URI the planner may need bytes or a hash for:
    /// a fetched iCloud card whose URI differs from its row's recorded URI,
    /// or whose row is untracked, or that has no row (pairing). Each URI is
    /// fetched once. A cycle-fatal error aborts; any other failure is kept
    /// per URI and holds that contact (`Diagnostic::PhotoUnavailable`).
    pub(super) async fn download_photos(&self, built: &Built, stored: &Stored) -> Result<FetchedPhotos, Error> {
        let rows: HashMap<_, _> = stored.contacts.iter().map(|row| (&row.uid, &row.photo)).collect();
        let mut photos = FetchedPhotos::new();
        for (_, entry) in built.icloud.entries() {
            let Entry::Fetched { card: Ok(card), .. } = entry else { continue };
            let Some(CardPhoto::Uri(uri)) = card.photo() else { continue };
            let known = rows
                .get(card.uid())
                .is_some_and(|photo| photo.tracked && photo.icloud_uri.as_ref() == Some(&uri) && photo.icloud_hash.is_some());
            if known || photos.contains_key(&uri) {
                continue;
            }
            let result = match self.photos.fetch(&uri).await {
                Ok(bytes) => Ok(PhotoData::new(bytes)),
                Err(error) if is_photo_fatal(&error) => return Err(error),
                Err(error) => Err(FailureReason::from(&error)),
            };
            photos.insert(uri, result);
        }
        Ok(photos)
    }
}
