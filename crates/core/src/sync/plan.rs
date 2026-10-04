use std::fmt::{self, Write as _};

use crate::{
    contact::{CardHash, DisplayIdentity, ETag, HashOptions, Href, Side, Uid, VCard, VCardError},
    state::{ConflictOrigin, FailureReason, PhotoState},
};

/// The hash options for every content comparison in the sync engine: photos
/// are a separate dimension (CG-15), so no `PHOTO` property (URI or embedded)
/// ever counts as content.
pub const SYNC_HASH: HashOptions = HashOptions {
    exclude_photo: true,
    exclude_uid: false,
};

/// One resource on one side: where it is, and the ETag that guards writes to
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    pub href: Href,
    pub etag: ETag,
}

impl fmt::Display for Resource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.href, self.etag)
    }
}

/// What a write does to its target's photo (CG-15 R4, R8).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PhotoChange {
    /// No photo on either the body or the target.
    #[default]
    None,
    /// The target's own `PHOTO` lines are written back byte-for-byte.
    Kept,
    /// The other side's photo replaces the target's.
    Set,
    /// Every `PHOTO` line is dropped.
    Removed,
    /// The photo could not fit on iCloud: the card goes without one
    /// (Decision 1).
    Stripped,
}

/// CG-15 Decision 4: a photo going to the side the op's main write does not
/// write. PUT `body` over `target` (`If-Match`) on that side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterWrite {
    pub side: Side,
    pub target: Resource,
    /// Full contact data (PII): never log it.
    pub body: VCard,
    pub change: PhotoChange,
}

/// The card both sides hold once an op completes, in the form the state row
/// records it: without photos, which are a separate dimension (CG-15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncedCard {
    /// The card as the state row records it (`last_synced_vcard`), with every
    /// `PHOTO` removed. For a push, also the bytes to PUT unless
    /// `put_with_photo` is set; see `body()`. Full contact data (PII): never
    /// log it.
    pub card: VCard,
    /// `card.canonical_hash(SYNC_HASH)` under `CANONICAL_VERSION`: the state
    /// row's `content_hash`.
    pub content_hash: CardHash,
    /// The exact PUT body when it differs from `card`: `card` with the
    /// target's own photo kept, or with the photo being set. The state row
    /// still records `card`.
    pub put_with_photo: Option<VCard>,
    /// What the main write does to the target's photo.
    pub photo: PhotoChange,
    /// The row's photo state after the op (default: untracked, so a path
    /// that never sets it fills gaps next cycle, Decision 5).
    pub photos: PhotoState,
    /// The other side's photo write, when the photo goes against the main
    /// write's direction.
    pub counter: Option<CounterWrite>,
}

impl SyncedCard {
    /// `source` as it goes to the other side, without its photos. When
    /// `target` (the card being replaced) has photos, the PUT keeps them.
    pub fn for_push(source: &VCard, target: Option<&VCard>) -> Self {
        let mut synced = Self::recorded(source);
        synced.put_with_photo = target.map(|target| synced.card.with_photos_of(target)).filter(|put| *put != synced.card);
        if synced.put_with_photo.is_some() {
            synced.photo = PhotoChange::Kept;
        }
        synced
    }

    /// `card` as the state row records it: photos removed, hashed with
    /// `SYNC_HASH`.
    pub fn recorded(card: &VCard) -> Self {
        let card = card.without_photos();
        let content_hash = card.canonical_hash(SYNC_HASH);
        Self {
            card,
            content_hash,
            put_with_photo: None,
            photo: PhotoChange::None,
            photos: PhotoState::default(),
            counter: None,
        }
    }

    /// With the row's photo state after the op.
    #[must_use]
    pub fn with_photos(self, photos: PhotoState) -> Self {
        Self { photos, ..self }
    }

    /// The bytes to PUT.
    pub fn body(&self) -> &VCard {
        self.put_with_photo.as_ref().unwrap_or(&self.card)
    }
}

/// Which pairing pass gave a Fastmail card the iCloud UID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairPass {
    /// Pass 2: the same content under different UIDs.
    Content,
    /// Pass 3: the identity heuristic.
    Identity,
}

impl PairPass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Content => "content",
            Self::Identity => "identity",
        }
    }
}

/// A pass-3 pair's two originals, recorded in the conflict history (origin
/// `baseline`) before any write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecreateConflict {
    pub winner: Side,
    pub icloud_card: VCard,
    pub fastmail_card: VCard,
}

/// One step of a sync cycle. Every server write names the side it goes to and
/// the ETag guarding it; state-only ops (`Adopt`, `Refresh`, `Forget`) write
/// no server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// New on `to.other()` and not in state: PUT `synced.body()` to `to` at a
    /// new href (`If-None-Match: *`), then add the state row. (Pairing stage,
    /// CG-7.)
    Create { uid: Uid, to: Side, source: Resource, synced: SyncedCard },
    /// Changed on the other side: PUT `synced.body()` to `to` over `target`
    /// (`If-Match: target.etag`), then update the row.
    Update {
        uid: Uid,
        to: Side,
        target: Resource,
        source: Resource,
        synced: SyncedCard,
    },
    /// Gone from the other side: DELETE `target` on `on` (`If-Match`), then
    /// drop the row.
    Delete { uid: Uid, on: Side, target: Resource },
    /// Different content on both sides. Record both cards in the conflict
    /// history first, then PUT `synced.body()` (the winner's card, keeping the
    /// loser's photo) to the loser over
    /// `target`. `origin` is `Sync` for a contact with a state row (update it)
    /// and `Baseline` for one without (pairing pass 1: add the row).
    Conflict {
        uid: Uid,
        origin: ConflictOrigin,
        winner: Side,
        target: Resource,
        source: Resource,
        synced: SyncedCard,
        icloud_card: VCard,
        fastmail_card: VCard,
    },
    /// Deleted on `to`, edited on the other side: the edit wins. PUT
    /// `synced.body()` to `to` at a new href (`If-None-Match: *`), then update
    /// the row.
    Resurrect { uid: Uid, to: Side, source: Resource, synced: SyncedCard },
    /// CG-14: a Fastmail-only Apple group being copied to iCloud whose
    /// members include Fastmail UIDs that pairing replaced. PUT `rewritten`
    /// (the Fastmail card with those members renamed, its photo kept) over
    /// `source` (`If-Match`), then PUT `synced.body()` to iCloud at a new
    /// href (`If-None-Match: *`), then add the state row. Fastmail goes
    /// first: if the iCloud create then fails, the next cycle copies a group
    /// that is already right; the other order could push the stale members
    /// back.
    CopyGroup {
        uid: Uid,
        source: Resource,
        rewritten: VCard,
        synced: SyncedCard,
        /// Member lines rewritten, for the op line.
        relinked: usize,
    },
    /// The same card on both sides with no state row (crash recovery, or
    /// pairing pass 1): add the row. No server write.
    Adopt {
        uid: Uid,
        icloud: Resource,
        fastmail: Resource,
        synced: SyncedCard,
    },
    /// Pairing passes 2 and 3: the Fastmail card takes the iCloud card's UID.
    /// CG-8 runs these steps in order; a crash between most pairs of steps
    /// converges on the next cycle's pairing instead of duplicating:
    /// 1. record `conflict` in the conflict history (pass 3 only);
    /// 2. PUT `put_icloud` over `icloud` (`If-Match`), when set (pass 3,
    ///    Fastmail wins);
    /// 3. DELETE `old_fastmail` (`If-Match`);
    /// 4. PUT `create_fastmail` to Fastmail at a new href (`If-None-Match: *`);
    /// 5. add the state row for `uid` with `synced`.
    ///
    /// A crash between step 1 and step 2 re-records the same pass-3 conflict
    /// on the next cycle, since pairing re-evaluates from scratch: CG-8
    /// should dedupe identical conflict rows, or accept the duplicate (M1).
    ///
    /// A crash between step 3 and step 4 is not self-healing: the old
    /// Fastmail card is already gone, and nothing in pure `sync` remembers
    /// its bytes, so the next cycle's pairing can only copy the iCloud card
    /// under a new UID — losing `create_fastmail`'s own content, photo
    /// included, exactly what pass 2/3 exist to keep (I2). Pure `core`
    /// cannot persist, so recovering from this gap is CG-8's obligation:
    /// before step 3, durably record `create_fastmail` (the Fastmail card's
    /// own bytes, under the new UID) for every pass; then, on the next
    /// cycle, if that record is still pending (its old card gone, its new
    /// card not yet present), create it from the record instead of letting
    /// pairing copy the iCloud card.
    Recreate {
        uid: Uid,
        pass: PairPass,
        icloud: Resource,
        old_fastmail: Resource,
        /// The UID the Fastmail card had before.
        fastmail_uid: Uid,
        put_icloud: Option<VCard>,
        create_fastmail: VCard,
        synced: SyncedCard,
        conflict: Option<RecreateConflict>,
    },
    /// State only: record a side's new href/ETag (a server rewrite, the
    /// daemon's own write coming back, or a move) and, with `synced`, new
    /// content and hash.
    Refresh {
        uid: Uid,
        icloud: Option<Resource>,
        fastmail: Option<Resource>,
        synced: Option<SyncedCard>,
    },
    /// State only: gone from both sides; drop the row.
    Forget { uid: Uid },
}

impl Op {
    pub fn uid(&self) -> &Uid {
        match self {
            Self::Create { uid, .. }
            | Self::Update { uid, .. }
            | Self::Delete { uid, .. }
            | Self::Conflict { uid, .. }
            | Self::Resurrect { uid, .. }
            | Self::CopyGroup { uid, .. }
            | Self::Adopt { uid, .. }
            | Self::Recreate { uid, .. }
            | Self::Refresh { uid, .. }
            | Self::Forget { uid } => uid,
        }
    }

    /// The spec's per-record log op. `None` for state-only ops, which only
    /// count in the cycle summary.
    pub fn log_op(&self) -> Option<&'static str> {
        match self {
            Self::Create { .. } | Self::Resurrect { .. } | Self::CopyGroup { .. } => Some("add"),
            Self::Update { .. } | Self::Conflict { .. } | Self::Recreate { .. } => Some("update"),
            Self::Delete { .. } => Some("remove"),
            Self::Adopt { .. } | Self::Refresh { .. } | Self::Forget { .. } => None,
        }
    }
}

/// The op line's photo note: ` photo-kept`, ` photo=set|removed|stripped`,
/// then ` +photo→<side>=<change>` for a counter write.
fn kept(synced: &SyncedCard) -> String {
    let mut note = match synced.photo {
        PhotoChange::Kept if synced.put_with_photo.is_some() => " photo-kept".to_owned(),
        PhotoChange::Set => " photo=set".to_owned(),
        PhotoChange::Removed => " photo=removed".to_owned(),
        PhotoChange::Stripped => " photo=stripped".to_owned(),
        _ => String::new(),
    };
    if let Some(counter) = &synced.counter {
        let change = match counter.change {
            PhotoChange::Set => "set",
            PhotoChange::Removed => "removed",
            PhotoChange::Stripped => "stripped",
            PhotoChange::None | PhotoChange::Kept => "kept",
        };
        let _ = write!(note, " +photo→{}={change}", counter.side);
    }
    note
}

/// One line, PII-free: sides, UID, hrefs and ETags only.
impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Create { uid, to, source, synced } => write!(f, "create {to} uid={uid} from={}{}", source.href, kept(synced)),
            Self::Update { uid, to, target, synced, .. } => write!(f, "update {to} uid={uid} {target}{}", kept(synced)),
            Self::Delete { uid, on, target } => write!(f, "delete {on} uid={uid} {target}"),
            Self::Conflict {
                uid,
                origin,
                winner,
                target,
                synced,
                ..
            } => write!(
                f,
                "conflict({}) {winner} wins uid={uid} → {} {target}{}",
                origin.as_str(),
                winner.other(),
                kept(synced)
            ),
            Self::Resurrect { uid, to, source, synced } => write!(f, "resurrect {to} uid={uid} from={}{}", source.href, kept(synced)),
            Self::CopyGroup { uid, source, relinked, .. } => write!(f, "copy-group icloud uid={uid} from={source} relinked={relinked}"),
            Self::Adopt { uid, icloud, fastmail, synced } => write!(f, "adopt uid={uid} icloud={icloud} fastmail={fastmail}{}", kept(synced)),
            Self::Refresh { uid, icloud, fastmail, synced } => {
                write!(f, "refresh uid={uid}")?;
                if let Some(resource) = icloud {
                    write!(f, " icloud={resource}")?;
                }
                if let Some(resource) = fastmail {
                    write!(f, " fastmail={resource}")?;
                }
                if synced.is_some() {
                    f.write_str(" content")?;
                }
                Ok(())
            }
            Self::Recreate {
                uid,
                pass,
                icloud,
                old_fastmail,
                fastmail_uid,
                put_icloud,
                conflict,
                ..
            } => {
                write!(f, "recreate({}) uid={uid} fastmail {old_fastmail} was {fastmail_uid}", pass.as_str())?;
                if put_icloud.is_some() {
                    write!(f, " put icloud {icloud}")?;
                }
                if let Some(conflict) = conflict {
                    write!(f, " {} wins", conflict.winner)?;
                }
                Ok(())
            }
            Self::Forget { uid } => write!(f, "forget uid={uid}"),
        }
    }
}

/// A card the planner will not act on this cycle. CG-8 records each as a
/// `card_failures` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diagnostic {
    /// The card did not parse (no UID, vCard 4.0, malformed). Its UID is held
    /// when the href is in state.
    Unreadable { side: Side, href: Href, etag: ETag, error: VCardError },
    /// One UID at several hrefs on one side: ambiguous, so none is synced.
    DuplicateUid { side: Side, uid: Uid, hrefs: Vec<Href> },
    /// The card at a synced href now carries another UID: both UIDs are held,
    /// never deleted or duplicated.
    UidChanged {
        side: Side,
        href: Href,
        etag: ETag,
        stored: Uid,
        found: Uid,
    },
    /// An update would replace a card that was not fetched this cycle, so its
    /// photo could not be kept. No op; `fetch_lists` prevents this, so CG-8
    /// only counts and warns.
    UnreadTarget { side: Side, uid: Uid, target: Resource },
    /// A synced row was skipped because `side`, the uncertain side, has
    /// nothing at this row's tracked resource there: that might be a real
    /// deletion, or it might be hiding at an href the planner could not
    /// attribute (unreadable, held, or an unattributed `Unchanged`). No op;
    /// CG-8 only counts and warns rather than recording a card failure.
    DeletionDeferred { side: Side, uid: Uid },
    /// A synced row's `Delete` on `on` was held: another row is being deleted
    /// on the other side (row `with`), and the two look like the same contact
    /// (`MatchKeys::may_be_same_contact`). A client that shows both accounts
    /// merges cards by name, so deleting one merged entry can remove a
    /// different pair's card on each side (CG-17). No op; CG-8 counts and
    /// warns. `identity` is for the baseline report only: `Display` prints
    /// UIDs and sides.
    DeleteHeld { on: Side, uid: Uid, with: Uid, identity: DisplayIdentity },
    /// CG-15: the photo of the iCloud card at `href` could not be downloaded.
    /// The contact is held this cycle on both sides, so a failed download is
    /// never read as a removed photo. CG-8 records a read failure.
    PhotoUnavailable { href: Href, etag: ETag, uid: Uid, reason: FailureReason },
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable { side, href, etag, error } => write!(f, "unreadable {side} {href}@{etag}: {error}"),
            Self::DuplicateUid { side, uid, hrefs } => {
                let hrefs: Vec<&str> = hrefs.iter().map(Href::as_str).collect();
                write!(f, "duplicate uid={uid} on {side}: {}", hrefs.join(", "))
            }
            Self::UidChanged {
                side,
                href,
                etag,
                stored,
                found,
            } => write!(f, "uid changed on {side} {href}@{etag}: {stored} → {found}"),
            Self::UnreadTarget { side, uid, target } => write!(f, "unread target {side} uid={uid} {target}"),
            Self::DeletionDeferred { side, uid } => write!(f, "deletion deferred on {side} uid={uid}: unreadable card on that side"),
            Self::DeleteHeld { on, uid, with, .. } => write!(f, "delete held on {on} uid={uid}: may be the same contact as uid={with}"),
            Self::PhotoUnavailable { href, etag, uid, reason } => write!(f, "photo unavailable icloud uid={uid} {href}@{etag}: {}", reason.as_str()),
        }
    }
}

/// The operations for one cycle plus the cards held back and why.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub ops: Vec<Op>,
    pub diagnostics: Vec<Diagnostic>,
}

impl fmt::Display for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for op in &self.ops {
            writeln!(f, "{op}")?;
        }
        for diagnostic in &self.diagnostics {
            writeln!(f, "! {diagnostic}")?;
        }
        Ok(())
    }
}

/// A parsed card whose UID has no state row: input to pairing (CG-7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsyncedCard {
    pub resource: Resource,
    pub card: VCard,
}

/// Both sides' unsynced cards, each side in UID order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unsynced {
    pub icloud: Vec<UnsyncedCard>,
    pub fastmail: Vec<UnsyncedCard>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::fixtures::{EMBEDDED_PHOTO, URI_PHOTO, card, card_with, res};

    fn plain(uid: &str) -> SyncedCard {
        SyncedCard::recorded(&card_with(uid, "Jane Doe", "EMAIL:jane@example.com\r\n"))
    }

    #[test]
    fn plan_renders_one_pii_free_line_per_item() {
        let u1 = Uid::from("u1");
        let kept = SyncedCard::for_push(&card("u1", "Jane Doe"), Some(&card_with("u1", "Jane Doe", EMBEDDED_PHOTO)));
        let plan = Plan {
            ops: vec![
                Op::Create {
                    uid: u1.clone(),
                    to: Side::Fastmail,
                    source: res("/i/u1.vcf", "i1"),
                    synced: plain("u1"),
                },
                Op::Update {
                    uid: u1.clone(),
                    to: Side::Fastmail,
                    target: res("/f/u1.vcf", "f1"),
                    source: res("/i/u1.vcf", "i2"),
                    synced: kept,
                },
                Op::Delete {
                    uid: u1.clone(),
                    on: Side::ICloud,
                    target: res("/i/u1.vcf", "i1"),
                },
                Op::Conflict {
                    uid: u1.clone(),
                    origin: ConflictOrigin::Sync,
                    winner: Side::ICloud,
                    target: res("/f/u1.vcf", "f2"),
                    source: res("/i/u1.vcf", "i2"),
                    synced: plain("u1"),
                    icloud_card: plain("u1").card,
                    fastmail_card: plain("u1").card,
                },
                Op::Resurrect {
                    uid: u1.clone(),
                    to: Side::ICloud,
                    source: res("/f/u1.vcf", "f2"),
                    synced: plain("u1"),
                },
                Op::CopyGroup {
                    uid: u1.clone(),
                    source: res("/f/u1.vcf", "f2"),
                    rewritten: plain("u1").card,
                    synced: plain("u1"),
                    relinked: 2,
                },
                Op::Adopt {
                    uid: u1.clone(),
                    icloud: res("/i/u1.vcf", "i1"),
                    fastmail: res("/f/u1.vcf", "f1"),
                    synced: plain("u1"),
                },
                Op::Refresh {
                    uid: u1.clone(),
                    icloud: Some(res("/i/u1.vcf", "i2")),
                    fastmail: None,
                    synced: Some(plain("u1")),
                },
                Op::Forget { uid: u1 },
            ],
            diagnostics: vec![
                Diagnostic::Unreadable {
                    side: Side::Fastmail,
                    href: Href::from("/f/bad.vcf"),
                    etag: ETag::from("b1"),
                    error: VCardError::MissingUid,
                },
                Diagnostic::DuplicateUid {
                    side: Side::ICloud,
                    uid: Uid::from("u2"),
                    hrefs: vec![Href::from("/i/a.vcf"), Href::from("/i/b.vcf")],
                },
                Diagnostic::UidChanged {
                    side: Side::Fastmail,
                    href: Href::from("/f/u3.vcf"),
                    etag: ETag::from("f9"),
                    stored: Uid::from("u3"),
                    found: Uid::from("u4"),
                },
                Diagnostic::UnreadTarget {
                    side: Side::ICloud,
                    uid: Uid::from("u5"),
                    target: res("/i/u5.vcf", "i5"),
                },
                Diagnostic::DeletionDeferred {
                    side: Side::Fastmail,
                    uid: Uid::from("u6"),
                },
                Diagnostic::DeleteHeld {
                    on: Side::Fastmail,
                    uid: Uid::from("u7"),
                    with: Uid::from("u8"),
                    identity: card_with("u7", "Harbor Grill", "EMAIL:jane@example.com\r\n").display_identity(),
                },
            ],
        };

        let rendered = plan.to_string();

        assert!(!rendered.contains("jane@example.com"), "card content leaked: {rendered}");
        assert!(!rendered.contains("Harbor Grill"), "identity leaked into the plan: {rendered}");
        insta::assert_snapshot!(rendered, @r"
        create fastmail uid=u1 from=/i/u1.vcf
        update fastmail uid=u1 /f/u1.vcf@f1 photo-kept
        delete icloud uid=u1 /i/u1.vcf@i1
        conflict(sync) icloud wins uid=u1 → fastmail /f/u1.vcf@f2
        resurrect icloud uid=u1 from=/f/u1.vcf
        copy-group icloud uid=u1 from=/f/u1.vcf@f2 relinked=2
        adopt uid=u1 icloud=/i/u1.vcf@i1 fastmail=/f/u1.vcf@f1
        refresh uid=u1 icloud=/i/u1.vcf@i2 content
        forget uid=u1
        ! unreadable fastmail /f/bad.vcf@b1: vCard has no UID
        ! duplicate uid=u2 on icloud: /i/a.vcf, /i/b.vcf
        ! uid changed on fastmail /f/u3.vcf@f9: u3 → u4
        ! unread target icloud uid=u5 /i/u5.vcf@i5
        ! deletion deferred on fastmail uid=u6: unreadable card on that side
        ! delete held on fastmail uid=u7: may be the same contact as uid=u8
        ");
    }

    #[test]
    fn log_op_names_only_server_writes() {
        let u1 = Uid::from("u1");
        assert_eq!(
            Op::Delete {
                uid: u1.clone(),
                on: Side::ICloud,
                target: res("/i/u1.vcf", "i1")
            }
            .log_op(),
            Some("remove")
        );
        assert_eq!(Op::Forget { uid: u1.clone() }.log_op(), None);
        assert_eq!(
            Op::Refresh {
                uid: u1.clone(),
                icloud: None,
                fastmail: None,
                synced: None
            }
            .log_op(),
            None
        );
        assert_eq!(
            Op::Resurrect {
                uid: u1.clone(),
                to: Side::ICloud,
                source: res("/f/u1.vcf", "f1"),
                synced: plain("u1")
            }
            .log_op(),
            Some("add")
        );
        let copy = Op::CopyGroup {
            uid: u1.clone(),
            source: res("/f/u1.vcf", "f1"),
            rewritten: plain("u1").card,
            synced: plain("u1"),
            relinked: 1,
        };
        assert_eq!((copy.log_op(), copy.uid()), (Some("add"), &u1));
        assert_eq!(Op::Forget { uid: u1.clone() }.uid(), &u1);
    }

    #[test]
    fn recreate_renders_its_steps() {
        let icloud = card("ic-1", "Jane Doe");
        let fastmail = card("fm-1", "Jane Doe");
        let op = Op::Recreate {
            uid: Uid::from("ic-1"),
            pass: PairPass::Identity,
            icloud: res("/i/ic-1.vcf", "i1"),
            old_fastmail: res("/f/fm-1.vcf", "f1"),
            fastmail_uid: Uid::from("fm-1"),
            put_icloud: Some(fastmail.with_uid(&Uid::from("ic-1"))),
            create_fastmail: fastmail.with_uid(&Uid::from("ic-1")),
            synced: SyncedCard::recorded(&fastmail.with_uid(&Uid::from("ic-1"))),
            conflict: Some(RecreateConflict {
                winner: Side::Fastmail,
                icloud_card: icloud,
                fastmail_card: fastmail,
            }),
        };
        assert_eq!(
            op.to_string(),
            "recreate(identity) uid=ic-1 fastmail /f/fm-1.vcf@f1 was fm-1 put icloud /i/ic-1.vcf@i1 fastmail wins"
        );
        assert_eq!(op.log_op(), Some("update"));
    }

    #[test]
    fn for_push_drops_the_source_photo_and_keeps_the_targets() {
        let source = card_with("u1", "Jane Doe", &format!("NOTE:new\r\n{URI_PHOTO}"));
        let target = card_with("u1", "Jane Doe", EMBEDDED_PHOTO);

        let pushed = SyncedCard::for_push(&source, Some(&target));

        let recorded = card_with("u1", "Jane Doe", "NOTE:new\r\n");
        assert_eq!(pushed.card, recorded);
        assert_eq!(pushed.content_hash, source.canonical_hash(SYNC_HASH));
        assert_eq!(pushed.put_with_photo, Some(recorded.with_photos_of(&target)));
        assert_eq!(pushed.body(), &card_with("u1", "Jane Doe", &format!("NOTE:new\r\n{EMBEDDED_PHOTO}")));

        // No target photo, or no target at all: the plain card is PUT.
        assert_eq!(SyncedCard::for_push(&source, Some(&card("u1", "Jane Doe"))).put_with_photo, None);
        assert_eq!(SyncedCard::for_push(&source, None).body(), &recorded);
    }
}
