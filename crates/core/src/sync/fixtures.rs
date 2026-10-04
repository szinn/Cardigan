//! Test fixtures for the sync engine. Cards are tiny and PII-free.

use chrono::{DateTime, Utc};

use super::{Entry, Resource, SYNC_HASH, Snapshot, UnsyncedCard};
use crate::{
    contact::{CANONICAL_VERSION, ETag, Href, VCard},
    state::{ContactState, PhotoState, SideState},
};

/// An embedded photo line, as Fastmail stores photos.
pub(crate) const EMBEDDED_PHOTO: &str = "PHOTO;ENCODING=b;TYPE=JPEG:QUJD\r\n";
/// A URI photo line, as iCloud stores photos.
pub(crate) const URI_PHOTO: &str = "PHOTO;VALUE=uri:https://p1-contacts.icloud.com/photo/abc\r\n";

pub(crate) fn card(uid: &str, name: &str) -> VCard {
    card_with(uid, name, "")
}

/// A card with extra content lines (each ending in `\r\n`) before `END`.
pub(crate) fn card_with(uid: &str, name: &str, body: &str) -> VCard {
    VCard::parse(format!("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:{name}\r\n{body}END:VCARD\r\n")).expect("fixture card parses")
}

pub(crate) fn res(href: &str, etag: &str) -> Resource {
    Resource {
        href: Href::from(href),
        etag: ETag::from(etag),
    }
}

/// No photos downloaded: every card is photo-less or already known.
pub(crate) static NO_PHOTOS: std::sync::LazyLock<crate::sync::FetchedPhotos> = std::sync::LazyLock::new(crate::sync::FetchedPhotos::new);

/// A state row for `synced` (photo-less fixture cards), current hash version.
pub(crate) fn row(id: u64, synced: &VCard, icloud: (&str, &str), fastmail: (&str, &str)) -> ContactState {
    let at = DateTime::<Utc>::UNIX_EPOCH;
    let side = |(href, etag): (&str, &str)| SideState {
        href: Href::from(href),
        etag: ETag::from(etag),
        last_seen_at: at,
    };
    ContactState {
        id,
        version: 1,
        uid: synced.uid().clone(),
        icloud: side(icloud),
        fastmail: side(fastmail),
        content_hash: synced.canonical_hash(SYNC_HASH),
        hash_version: CANONICAL_VERSION,
        photo: PhotoState {
            tracked: true,
            ..PhotoState::default()
        },
        last_synced_vcard: synced.clone(),
        last_synced_at: at,
        created_at: at,
        updated_at: at,
    }
}

pub(crate) fn unchanged(etag: &str) -> Entry {
    Entry::Unchanged(ETag::from(etag))
}

pub(crate) fn fetched(etag: &str, card: VCard) -> Entry {
    Entry::Fetched {
        etag: ETag::from(etag),
        card: Ok(card),
    }
}

pub(crate) fn snapshot<const N: usize>(entries: [(&str, Entry); N]) -> Snapshot {
    entries.into_iter().map(|(href, entry)| (Href::from(href), entry)).collect()
}

/// An unsynced iCloud card at `/i/{uid}.vcf` with ETag `i-{uid}`.
pub(crate) fn on_icloud(card: VCard) -> UnsyncedCard {
    let uid = card.uid().as_str().to_owned();
    UnsyncedCard {
        resource: res(&format!("/i/{uid}.vcf"), &format!("i-{uid}")),
        card,
    }
}

/// An unsynced Fastmail card at `/f/{uid}.vcf` with ETag `f-{uid}`.
pub(crate) fn on_fastmail(card: VCard) -> UnsyncedCard {
    let uid = card.uid().as_str().to_owned();
    UnsyncedCard {
        resource: res(&format!("/f/{uid}.vcf"), &format!("f-{uid}")),
        card,
    }
}
