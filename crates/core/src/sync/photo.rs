//! The photo dimension of planning (CG-15).

use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::Arc,
};

use super::{Diagnostic, Entry, Op, PhotoChange, Snapshot, SyncedCard};
use crate::{
    contact::{CardPhoto, Fit, ICLOUD_MAX_CARD_BYTES, PhotoData, PhotoHash, PhotoUri, Side, Uid, VCard, fit_photo},
    state::{FailureReason, PhotoState},
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

/// One side's current photo, resolved (R1, R3). `bytes` is known for
/// Fastmail's inline photo and for downloaded iCloud photos; `None` for an
/// iCloud photo unchanged since it was recorded, or an uncopyable one.
/// PII: `Debug` prints the hash only.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Current {
    pub(super) hash: PhotoHash,
    pub(super) bytes: Option<Arc<[u8]>>,
    pub(super) uri: Option<PhotoUri>,
}

impl fmt::Debug for Current {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Current")
            .field("hash", &self.hash)
            .field("has_bytes", &self.bytes.is_some())
            .field("has_uri", &self.uri.is_some())
            .finish()
    }
}

/// The photo could not be resolved: an iCloud URI that is neither recorded
/// nor downloaded. Never read as "no photo".
#[derive(Debug)]
pub(super) struct Unavailable;

/// `card`'s photo on `side`. `Ok(None)` is "no photo".
pub(super) fn current(side: Side, card: &VCard, recorded: Option<&PhotoState>, photos: &FetchedPhotos) -> Result<Option<Current>, Unavailable> {
    Ok(match card.photo() {
        None => None,
        Some(CardPhoto::Inline(data)) => Some(Current {
            hash: data.hash,
            bytes: Some(data.bytes),
            uri: None,
        }),
        Some(CardPhoto::Uri(uri)) if side == Side::ICloud => {
            let known = recorded
                .filter(|r| r.tracked && r.icloud_uri.as_ref() == Some(&uri))
                .and_then(|r| r.icloud_hash);
            match (known, photos.get(&uri)) {
                (Some(hash), _) => Some(Current {
                    hash,
                    bytes: None,
                    uri: Some(uri),
                }),
                (None, Some(Ok(data))) => Some(Current {
                    hash: data.hash,
                    bytes: Some(data.bytes.clone()),
                    uri: Some(uri),
                }),
                (None, _) => return Err(Unavailable),
            }
        }
        // A URI on Fastmail, or an unreadable inline photo: identified by its
        // text, never copied (R1).
        Some(CardPhoto::Uri(uri)) => Some(Current {
            hash: PhotoHash::of(uri.as_str().as_bytes()),
            bytes: None,
            uri: None,
        }),
        Some(CardPhoto::Unreadable) => Some(Current {
            hash: PhotoHash::of(card.uid().as_str().as_bytes()),
            bytes: None,
            uri: None,
        }),
    })
}

/// The recorded photo of a side that was not fetched (its ETag is
/// unchanged, so its photo is too).
pub(super) fn recorded(side: Side, state: &PhotoState) -> Option<Current> {
    match side {
        Side::ICloud => state.icloud_hash.map(|hash| Current {
            hash,
            bytes: None,
            uri: state.icloud_uri.clone(),
        }),
        Side::Fastmail => state.fastmail_hash.map(|hash| Current { hash, bytes: None, uri: None }),
    }
}

/// What a write does to one side's photo (R4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PhotoAction {
    Keep,
    /// Always carries bytes (`Current.bytes` is `Some`).
    Set(Current),
    Remove,
}

impl PhotoAction {
    pub(super) fn is_keep(&self) -> bool {
        matches!(self, Self::Keep)
    }
}

/// The photo action for each side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PhotoPlan {
    pub(super) icloud: PhotoAction,
    pub(super) fastmail: PhotoAction,
}

impl PhotoPlan {
    const KEEP: Self = Self {
        icloud: PhotoAction::Keep,
        fastmail: PhotoAction::Keep,
    };

    pub(super) fn on(&self, side: Side) -> &PhotoAction {
        match side {
            Side::ICloud => &self.icloud,
            Side::Fastmail => &self.fastmail,
        }
    }
}

/// `source`'s photo as an action for the other side: copy it when it has
/// bytes, remove when there is none, keep when it cannot be copied.
fn from(source: Option<&Current>) -> PhotoAction {
    match source {
        Some(current) if current.bytes.is_some() => PhotoAction::Set(current.clone()),
        Some(_) => PhotoAction::Keep,
        None => PhotoAction::Remove,
    }
}

/// R4's tables. `state` is the row's record; `None` or untracked is a new
/// pair or a pre-upgrade row (Decision 2).
pub(super) fn decide(state: Option<&PhotoState>, icloud: Option<&Current>, fastmail: Option<&Current>, winner: Side) -> PhotoPlan {
    let keep = PhotoPlan::KEEP;
    let hash = |c: Option<&Current>| c.map(|c| c.hash);
    let Some(state) = state.filter(|s| s.tracked) else {
        // Untracked (Decision 2): fill a one-sided gap, keep differences.
        return match (icloud, fastmail) {
            (Some(_), None) => PhotoPlan {
                fastmail: from(icloud),
                ..keep
            },
            (None, Some(_)) => PhotoPlan {
                icloud: from(fastmail),
                ..keep
            },
            _ => keep,
        };
    };
    let icloud_changed = hash(icloud) != state.icloud_hash;
    let fastmail_changed = hash(fastmail) != state.fastmail_hash;
    match (icloud_changed, fastmail_changed) {
        (false, false) => keep,
        (true, false) => PhotoPlan {
            fastmail: from(icloud),
            ..keep
        },
        (false, true) => PhotoPlan {
            icloud: from(fastmail),
            ..keep
        },
        (true, true) if hash(icloud) == hash(fastmail) => keep,
        (true, true) => match winner {
            Side::ICloud => PhotoPlan {
                fastmail: from(icloud),
                ..keep
            },
            Side::Fastmail => PhotoPlan {
                icloud: from(fastmail),
                ..keep
            },
        },
    }
}

/// The PUT body for `to`: `content` (photo-free) with `action` applied.
/// `target` is the card being replaced, for `Keep`. Returns the body, what
/// it did to the photo, and the bytes pushed (after fitting, for iCloud).
pub(super) fn apply(to: Side, content: &VCard, target: Option<&VCard>, action: &PhotoAction) -> (VCard, PhotoChange, Option<PhotoData>) {
    match action {
        PhotoAction::Keep => {
            let body = target.map_or_else(|| content.clone(), |target| content.with_photos_of(target));
            let change = if body.photo().is_some() { PhotoChange::Kept } else { PhotoChange::None };
            (body, change, None)
        }
        PhotoAction::Remove => (content.without_photos(), PhotoChange::Removed, None),
        PhotoAction::Set(current) => {
            let data = PhotoData::new(current.bytes.as_deref().expect("Set carries bytes").to_vec());
            if to == Side::Fastmail {
                return (content.with_inline_photo(&data.bytes), PhotoChange::Set, Some(data));
            }
            match fit_photo(content, &data, ICLOUD_MAX_CARD_BYTES) {
                Fit::AsIs => (content.with_inline_photo(&data.bytes), PhotoChange::Set, Some(data)),
                Fit::Resized(resized) => (content.with_inline_photo(&resized.bytes), PhotoChange::Set, Some(resized)),
                Fit::Unfittable => (content.without_photos(), PhotoChange::Stripped, None),
            }
        }
    }
}

/// A write's photo outcome on one side: what it did, and the hash of the
/// bytes it pushed.
pub(super) type Pushed = (PhotoChange, Option<PhotoHash>);

/// The row's photo state after the op. `pushed_icloud`/`pushed_fastmail`
/// are `apply`'s outcomes for writes to that side. The iCloud URI of a Set
/// is unknown until the executor reads the card back (R5), so it is `None`
/// here and filled in by the executor.
pub(super) fn next_state(
    state: Option<&PhotoState>,
    icloud: Option<&Current>,
    fastmail: Option<&Current>,
    plan: &PhotoPlan,
    pushed_icloud: Option<Pushed>,
    pushed_fastmail: Option<Pushed>,
) -> PhotoState {
    let prev = state.cloned().unwrap_or_default();
    let mut next = PhotoState { tracked: true, ..prev.clone() };
    match (&plan.icloud, pushed_icloud) {
        (PhotoAction::Keep, _) | (_, None) => {
            next.icloud_uri = icloud.and_then(|c| c.uri.clone());
            next.icloud_hash = icloud.map(|c| c.hash);
            if icloud.map(|c| c.hash) != prev.icloud_hash {
                next.stripped = false;
            }
        }
        (_, Some((PhotoChange::Stripped, _))) => {
            next.icloud_uri = None;
            next.icloud_hash = None;
            next.stripped = true;
        }
        (_, Some((_, hash))) => {
            next.icloud_uri = None;
            next.icloud_hash = hash;
            next.stripped = false;
        }
    }
    match (&plan.fastmail, pushed_fastmail) {
        (PhotoAction::Keep, _) | (_, None) => next.fastmail_hash = fastmail.map(|c| c.hash),
        (_, Some((_, hash))) => next.fastmail_hash = hash,
    }
    next
}

/// Pairing's plain copies (`Create`, and CG-14's `CopyGroup`) carry the
/// source's photo (R4, Decision 1); every other pairing op stays untracked
/// and is filled next cycle. Runs after relinking, so `CopyGroup`s exist.
pub(super) fn attach_to_copies(ops: &mut [Op], icloud: &Snapshot, fastmail: &Snapshot, photos: &FetchedPhotos) {
    for op in ops.iter_mut() {
        let (to, source_card, synced) = match op {
            Op::Create { to, source, synced, .. } => {
                let snapshot = match to.other() {
                    Side::ICloud => icloud,
                    Side::Fastmail => fastmail,
                };
                let Some(Entry::Fetched { card: Ok(card), .. }) = snapshot.get(&source.href) else {
                    continue;
                };
                (*to, card, synced)
            }
            Op::CopyGroup { rewritten, synced, .. } => (Side::ICloud, &*rewritten, synced),
            _ => continue,
        };
        let Ok(src) = current(to.other(), source_card, None, photos) else { continue };
        copy_photo(synced, to, src);
    }
}

/// A copy to `to`, a side that has no card yet (`Create`, `Resurrect`,
/// `CopyGroup`): the untracked table copies `src`, the source's photo, or
/// does nothing when there is none (R4). Sets the PUT body, the change and
/// the row's photo state.
///
/// A source photo without bytes (an iCloud URI unchanged since recorded, so
/// not downloaded; or an uncopyable one) is not copied, and the row stays
/// untracked: recording it as settled would leave `to` without the photo
/// for good. The next cycle's untracked handling downloads and fills it.
pub(super) fn copy_photo(synced: &mut SyncedCard, to: Side, src: Option<Current>) {
    if src.as_ref().is_some_and(|src| src.bytes.is_none()) {
        synced.photo = PhotoChange::None;
        synced.put_with_photo = None;
        synced.photos = PhotoState::default();
        return;
    }
    let (icloud, fastmail) = match to {
        Side::ICloud => (None, src),
        Side::Fastmail => (src, None),
    };
    let plan = decide(None, icloud.as_ref(), fastmail.as_ref(), Side::ICloud);
    let (body, change, pushed) = apply(to, &synced.card, None, plan.on(to));
    let pushed = Some((change, pushed.map(|p| p.hash)));
    let (pushed_icloud, pushed_fastmail) = match to {
        Side::ICloud => (pushed, None),
        Side::Fastmail => (None, pushed),
    };
    synced.photos = next_state(None, icloud.as_ref(), fastmail.as_ref(), &plan, pushed_icloud, pushed_fastmail);
    synced.photo = change;
    synced.put_with_photo = (body != synced.card).then_some(body);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::fixtures::{card_with, fetched, res, snapshot};

    const URI: &str = "https://gateway.icloud.com/p1";
    const URI2: &str = "https://gateway.icloud.com/p2";

    fn uri(u: &str) -> PhotoUri {
        PhotoUri::from(u.to_owned())
    }
    fn icloud_card(photo_uri: Option<&str>) -> VCard {
        card_with("u1", "Jane", &photo_uri.map_or_default(|u| format!("PHOTO;VALUE=uri:{u}\r\n")))
    }
    fn fastmail_card(bytes: Option<&[u8]>) -> VCard {
        let c = card_with("u1", "Jane", "");
        bytes.map_or(c.clone(), |b| c.with_inline_photo(b))
    }
    fn tracked(icloud: Option<(&str, &[u8])>, fastmail: Option<&[u8]>) -> PhotoState {
        PhotoState {
            icloud_uri: icloud.map(|(u, _)| uri(u)),
            icloud_hash: icloud.map(|(_, b)| PhotoHash::of(b)),
            fastmail_hash: fastmail.map(PhotoHash::of),
            stripped: false,
            tracked: true,
        }
    }

    #[test]
    fn a_new_uri_with_the_same_bytes_only_refreshes() {
        let state = tracked(Some((URI, b"A")), Some(b"A"));
        let photos: FetchedPhotos = [(uri(URI2), Ok(PhotoData::new(b"A".to_vec())))].into();
        let i = current(Side::ICloud, &icloud_card(Some(URI2)), Some(&state), &photos).unwrap();
        let f = current(Side::Fastmail, &fastmail_card(Some(b"A")), Some(&state), &photos).unwrap();

        let plan = decide(Some(&state), i.as_ref(), f.as_ref(), Side::ICloud);

        assert_eq!((plan.icloud.is_keep(), plan.fastmail.is_keep()), (true, true));
        let next = next_state(Some(&state), i.as_ref(), f.as_ref(), &plan, None, None);
        assert_eq!(next.icloud_uri, Some(uri(URI2)), "the new URI is recorded");
    }

    #[test]
    fn one_sided_changes_go_across_and_removals_sync() {
        let state = tracked(Some((URI, b"A")), Some(b"A"));
        let photos: FetchedPhotos = [(uri(URI2), Ok(PhotoData::new(b"B".to_vec())))].into();
        let i_new = current(Side::ICloud, &icloud_card(Some(URI2)), Some(&state), &photos).unwrap();
        let i_same = current(Side::ICloud, &icloud_card(Some(URI)), Some(&state), &photos).unwrap();
        let f_same = current(Side::Fastmail, &fastmail_card(Some(b"A")), Some(&state), &photos).unwrap();
        let f_gone = current(Side::Fastmail, &fastmail_card(None), Some(&state), &photos).unwrap();

        assert!(matches!(
            decide(Some(&state), i_new.as_ref(), f_same.as_ref(), Side::ICloud).fastmail,
            PhotoAction::Set(_)
        ));
        assert!(matches!(
            decide(Some(&state), i_same.as_ref(), f_gone.as_ref(), Side::ICloud).icloud,
            PhotoAction::Remove
        ));
    }

    #[test]
    fn both_sides_changed_goes_to_the_winner() {
        let state = tracked(Some((URI, b"A")), Some(b"A"));
        let photos: FetchedPhotos = [(uri(URI2), Ok(PhotoData::new(b"B".to_vec())))].into();
        let i = current(Side::ICloud, &icloud_card(Some(URI2)), Some(&state), &photos).unwrap();
        let f = current(Side::Fastmail, &fastmail_card(Some(b"C")), Some(&state), &photos).unwrap();

        assert!(matches!(
            decide(Some(&state), i.as_ref(), f.as_ref(), Side::ICloud).fastmail,
            PhotoAction::Set(_)
        ));
        assert!(matches!(
            decide(Some(&state), i.as_ref(), f.as_ref(), Side::Fastmail).icloud,
            PhotoAction::Set(_)
        ));
    }

    #[test]
    fn both_sides_changed_to_the_same_bytes_only_records() {
        let state = tracked(Some((URI, b"A")), Some(b"A"));
        let photos: FetchedPhotos = [(uri(URI2), Ok(PhotoData::new(b"B".to_vec())))].into();
        let i = current(Side::ICloud, &icloud_card(Some(URI2)), Some(&state), &photos).unwrap();
        let f = current(Side::Fastmail, &fastmail_card(Some(b"B")), Some(&state), &photos).unwrap();

        let plan = decide(Some(&state), i.as_ref(), f.as_ref(), Side::ICloud);

        assert!(plan.icloud.is_keep() && plan.fastmail.is_keep());
        let next = next_state(Some(&state), i.as_ref(), f.as_ref(), &plan, None, None);
        assert_eq!((next.icloud_hash, next.fastmail_hash), (Some(PhotoHash::of(b"B")), Some(PhotoHash::of(b"B"))));
    }

    #[test]
    fn untracked_rows_fill_gaps_and_keep_differences() {
        let photos: FetchedPhotos = [(uri(URI), Ok(PhotoData::new(b"A".to_vec())))].into();
        let untracked = PhotoState::default();
        let i = current(Side::ICloud, &icloud_card(Some(URI)), Some(&untracked), &photos).unwrap();
        let f_none = current(Side::Fastmail, &fastmail_card(None), Some(&untracked), &photos).unwrap();
        let f_other = current(Side::Fastmail, &fastmail_card(Some(b"Z")), Some(&untracked), &photos).unwrap();

        assert!(matches!(
            decide(Some(&untracked), i.as_ref(), f_none.as_ref(), Side::ICloud).fastmail,
            PhotoAction::Set(_)
        ));
        let both = decide(Some(&untracked), i.as_ref(), f_other.as_ref(), Side::ICloud);
        assert!(both.icloud.is_keep() && both.fastmail.is_keep());
        assert!(next_state(Some(&untracked), i.as_ref(), f_other.as_ref(), &both, None, None).tracked);
    }

    #[test]
    fn an_unavailable_photo_holds_the_row() {
        let state = tracked(Some((URI, b"A")), None);
        let photos = FetchedPhotos::new();
        assert!(
            current(Side::ICloud, &icloud_card(Some(URI2)), Some(&state), &photos).is_err(),
            "never read as removed"
        );
    }

    #[test]
    fn a_stripped_row_is_not_retried() {
        // Fastmail's photo could not fit on iCloud: iCloud has none on purpose.
        let state = PhotoState {
            stripped: true,
            ..tracked(None, Some(b"F"))
        };
        let i = current(Side::ICloud, &icloud_card(None), Some(&state), &FetchedPhotos::new()).unwrap();
        let f = current(Side::Fastmail, &fastmail_card(Some(b"F")), Some(&state), &FetchedPhotos::new()).unwrap();

        let plan = decide(Some(&state), i.as_ref(), f.as_ref(), Side::Fastmail);

        assert!(plan.icloud.is_keep() && plan.fastmail.is_keep());
        assert_eq!(next_state(Some(&state), i.as_ref(), f.as_ref(), &plan, None, None), state);
    }

    #[test]
    fn current_debug_hides_the_bytes_and_uri() {
        let current = Current {
            hash: PhotoHash::of(b"secret-bytes"),
            bytes: Some(Arc::from(&b"secret-bytes"[..])),
            uri: Some(uri(URI)),
        };
        let debug = format!("{:?}", PhotoAction::Set(current));
        assert!(!debug.contains("115") && !debug.contains("gateway"), "{debug}");
    }

    #[test]
    fn apply_fits_for_icloud_and_inlines_for_fastmail() {
        let content = card_with("u1", "Jane", "");
        let data = PhotoData::new(vec![0xFF, 0xD8, 0xFF, 9]);
        let set = PhotoAction::Set(Current {
            hash: data.hash,
            bytes: Some(data.bytes.clone()),
            uri: None,
        });

        let (body, change, pushed) = apply(Side::Fastmail, &content, None, &set);
        assert_eq!((change, pushed.map(|p| p.hash)), (PhotoChange::Set, Some(data.hash)));
        assert!(matches!(body.photo(), Some(CardPhoto::Inline(_))));

        let (body, change, _) = apply(Side::ICloud, &content, None, &PhotoAction::Remove);
        assert_eq!((change, body.photo()), (PhotoChange::Removed, None));
    }

    #[test]
    fn an_unfittable_photo_is_stripped_for_icloud() {
        let content = card_with("u1", "Jane", "");
        // Not an image, and far larger than iCloud's limit.
        let data = PhotoData::new(vec![7; ICLOUD_MAX_CARD_BYTES]);
        let set = PhotoAction::Set(Current {
            hash: data.hash,
            bytes: Some(data.bytes.clone()),
            uri: None,
        });

        let (body, change, pushed) = apply(Side::ICloud, &content, None, &set);

        assert_eq!((change, body.photo(), pushed), (PhotoChange::Stripped, None, None));
        let fastmail = Current {
            hash: data.hash,
            bytes: Some(data.bytes.clone()),
            uri: None,
        };
        let next = next_state(
            None,
            None,
            Some(&fastmail),
            &PhotoPlan {
                icloud: set,
                fastmail: PhotoAction::Keep,
            },
            Some((change, None)),
            None,
        );
        assert!(next.stripped && next.icloud_hash.is_none() && next.fastmail_hash == Some(data.hash));
    }

    #[test]
    fn copies_carry_the_source_photo() {
        let icloud = snapshot([("/i/u1.vcf", fetched("i1", icloud_card(Some(URI))))]);
        let fastmail = snapshot([("/f/u2.vcf", fetched("f1", card_with("u2", "Bo", "")))]);
        let photos: FetchedPhotos = [(uri(URI), Ok(PhotoData::new(vec![0xFF, 0xD8, 0xFF, 1])))].into();
        let mut ops = vec![
            Op::Create {
                uid: "u1".into(),
                to: Side::Fastmail,
                source: res("/i/u1.vcf", "i1"),
                synced: SyncedCard::for_push(&icloud_card(Some(URI)), None),
            },
            Op::Create {
                uid: "u2".into(),
                to: Side::ICloud,
                source: res("/f/u2.vcf", "f1"),
                synced: SyncedCard::for_push(&card_with("u2", "Bo", ""), None),
            },
        ];

        attach_to_copies(&mut ops, &icloud, &fastmail, &photos);

        let [Op::Create { synced: with, .. }, Op::Create { synced: without, .. }] = ops.as_slice() else {
            unreachable!()
        };
        assert_eq!(with.photo, PhotoChange::Set);
        assert!(matches!(with.body().photo(), Some(CardPhoto::Inline(_))));
        assert_eq!(with.photos.icloud_uri, Some(uri(URI)));
        assert_eq!(with.photos.fastmail_hash, with.photos.icloud_hash);
        assert!(with.photos.tracked && with.photos.fastmail_hash.is_some());
        // No source photo: nothing to remove on a new card.
        assert_eq!((without.photo, &without.put_with_photo), (PhotoChange::None, &None));
        assert!(
            without.photos.tracked
                && without.photos
                    == PhotoState {
                        tracked: true,
                        ..PhotoState::default()
                    }
        );
        assert_eq!(ops[0].to_string(), "create fastmail uid=u1 from=/i/u1.vcf photo=set");
    }

    #[test]
    fn a_copied_group_carries_the_fastmail_photo_to_icloud() {
        let group = card_with("g1", "Family", "X-ADDRESSBOOKSERVER-KIND:group\r\n").with_inline_photo(&[0xFF, 0xD8, 0xFF, 2]);
        let mut ops = vec![Op::CopyGroup {
            uid: "g1".into(),
            source: res("/f/g1.vcf", "f1"),
            synced: SyncedCard::recorded(&group),
            rewritten: group.clone(),
            relinked: 1,
        }];

        attach_to_copies(&mut ops, &snapshot([]), &snapshot([]), &FetchedPhotos::new());

        let [Op::CopyGroup { synced, .. }] = ops.as_slice() else { unreachable!() };
        assert_eq!(synced.photo, PhotoChange::Set);
        assert!(matches!(synced.body().photo(), Some(CardPhoto::Inline(_))));
        assert_eq!(synced.photos.fastmail_hash, Some(PhotoHash::of(&[0xFF, 0xD8, 0xFF, 2])));
        assert_eq!(synced.photos.icloud_hash, synced.photos.fastmail_hash, "the hash of the bytes sent (R5)");
        assert_eq!(synced.photos.icloud_uri, None, "known only after the read-back");
    }

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
