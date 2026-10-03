//! CG-14: after pairing gives Fastmail cards the iCloud UID, the
//! Fastmail-only groups copied this cycle are made to list the new UIDs.

use std::collections::HashMap;

use super::{Entry, Op, Snapshot, SyncedCard};
use crate::contact::{Side, Uid};

/// Old Fastmail UID → iCloud UID: every Recreate in `ops`, plus `replayed`
/// (Recreates CG-16's journal replay finished before this cycle planned).
pub(super) fn aliases(ops: &[Op], replayed: &[(Uid, Uid)]) -> HashMap<Uid, Uid> {
    let mut aliases: HashMap<Uid, Uid> = replayed.iter().cloned().collect();
    for op in ops {
        if let Op::Recreate { uid, fastmail_uid, .. } = op {
            aliases.insert(fastmail_uid.clone(), uid.clone());
        }
    }
    aliases
}

/// Replaces, in place, each `Create { to: ICloud }` of a group that lists an
/// alias with a `CopyGroup`. Ops keep their order, and pairing already puts
/// copies after every Recreate (triage F2).
pub(super) fn relink_groups(ops: &mut [Op], aliases: &HashMap<Uid, Uid>, fastmail: &Snapshot) {
    if aliases.is_empty() {
        return;
    }
    for op in ops.iter_mut() {
        if let Some(copy) = copy_group(op, aliases, fastmail) {
            *op = copy;
        }
    }
}

/// The `CopyGroup` for `op`, when it copies a group that lists an alias. The
/// Fastmail card comes from the snapshot, since the op holds only the
/// photo-free copy and the Fastmail PUT must keep the photo.
fn copy_group(op: &Op, aliases: &HashMap<Uid, Uid>, fastmail: &Snapshot) -> Option<Op> {
    let Op::Create {
        uid,
        to: Side::ICloud,
        source,
        synced,
    } = op
    else {
        return None;
    };
    if !synced.card.is_group() {
        return None;
    }
    let relinked = synced.card.member_uids().iter().filter(|member| aliases.contains_key(*member)).count();
    if relinked == 0 {
        return None;
    }
    let Some(Entry::Fetched { card: Ok(card), .. }) = fastmail.get(&source.href) else {
        return None;
    };
    let rewritten = card.with_member_uids(aliases);
    Some(Op::CopyGroup {
        uid: uid.clone(),
        source: source.clone(),
        synced: SyncedCard::recorded(&rewritten),
        rewritten,
        relinked,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        contact::VCard,
        sync::fixtures::{EMBEDDED_PHOTO, card, card_with, fetched, res, snapshot},
    };

    const MEMBER_FM1: &str = "X-ADDRESSBOOKSERVER-KIND:group\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:fm-1\r\n";

    fn create(to: Side, href: &str, card: &VCard) -> Op {
        Op::Create {
            uid: card.uid().clone(),
            to,
            source: res(href, "f-g"),
            synced: SyncedCard::recorded(card),
        }
    }

    fn map(pairs: &[(&str, &str)]) -> HashMap<Uid, Uid> {
        pairs.iter().map(|(old, new)| (Uid::from(*old), Uid::from(*new))).collect()
    }

    #[test]
    fn aliases_come_from_recreates_and_replay() {
        let recreate = Op::Recreate {
            uid: Uid::from("ic-3"),
            pass: crate::sync::PairPass::Content,
            icloud: res("/i/ic-3.vcf", "i"),
            old_fastmail: res("/f/fm-3.vcf", "f"),
            fastmail_uid: Uid::from("fm-3"),
            put_icloud: None,
            create_fastmail: card("ic-3", "Ann Lee"),
            synced: SyncedCard::recorded(&card("ic-3", "Ann Lee")),
            conflict: None,
        };

        let aliases = aliases(&[recreate], &[(Uid::from("fm-1"), Uid::from("ic-1"))]);

        assert_eq!(aliases, map(&[("fm-3", "ic-3"), ("fm-1", "ic-1")]));
    }

    #[test]
    fn a_group_copy_listing_an_alias_becomes_a_copy_group_in_place() {
        let group = card_with("fm-g", "Family", &format!("{MEMBER_FM1}{EMBEDDED_PHOTO}"));
        let person = card("fm-p", "Pat Kim");
        let fastmail = snapshot([("/f/g.vcf", fetched("f-g", group.clone())), ("/f/p.vcf", fetched("f-p", person.clone()))]);
        let mut ops = vec![create(Side::ICloud, "/f/p.vcf", &person), create(Side::ICloud, "/f/g.vcf", &group)];

        relink_groups(&mut ops, &map(&[("fm-1", "ic-1")]), &fastmail);

        assert!(matches!(ops[0], Op::Create { .. }), "a non-group copy is untouched");
        let Op::CopyGroup {
            uid,
            source,
            rewritten,
            synced,
            relinked,
        } = &ops[1]
        else {
            panic!("expected a CopyGroup, got {}", ops[1]);
        };
        assert_eq!((uid.as_str(), source, *relinked), ("fm-g", &res("/f/g.vcf", "f-g"), 1));
        assert_eq!(rewritten.member_uids(), [Uid::from("ic-1")]);
        assert!(
            rewritten.as_bytes().ends_with(format!("{EMBEDDED_PHOTO}END:VCARD\r\n").as_bytes()),
            "Fastmail keeps its photo"
        );
        assert_eq!(synced, &SyncedCard::recorded(rewritten), "iCloud and state get the photo-free card");
    }

    #[test]
    fn other_group_ops_are_left_alone() {
        let group = card_with("fm-g", "Family", MEMBER_FM1);
        let unrelated = card_with(
            "fm-h",
            "Club",
            "X-ADDRESSBOOKSERVER-KIND:group\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:other\r\n",
        );
        let fastmail = snapshot([("/f/g.vcf", fetched("f-g", group.clone())), ("/f/h.vcf", fetched("f-h", unrelated.clone()))]);
        let mut ops = vec![create(Side::Fastmail, "/f/g.vcf", &group), create(Side::ICloud, "/f/h.vcf", &unrelated)];
        let before = ops.clone();

        relink_groups(&mut ops, &map(&[("fm-1", "ic-1")]), &fastmail);

        assert_eq!(ops, before, "a copy to Fastmail and a group with no alias stay plain creates");
    }
}
