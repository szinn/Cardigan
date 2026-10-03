use std::collections::HashMap;

use super::{Property, VCard};
use crate::contact::Uid;

impl VCard {
    /// This card with its `UID` property replaced by `[group.]UID:<uid>`,
    /// keeping the original line ending. Folds are replaced with it;
    /// parameters on the old line are dropped. Every other byte is
    /// unchanged. Used when pairing gives a Fastmail card the iCloud UID.
    #[must_use]
    pub fn with_uid(&self, uid: &Uid) -> Self {
        let property = self.properties.iter().find(|p| p.is("UID")).expect("a parsed card always has a UID property");
        let ending = line_ending(&self.raw[property.span.clone()]);
        let mut line = String::new();
        if let Some(group) = property.group() {
            line.push_str(group);
            line.push('.');
        }
        line.push_str(property.name());
        line.push(':');
        line.push_str(uid.as_str());

        let mut raw = Vec::with_capacity(self.raw.len() + uid.as_str().len());
        raw.extend_from_slice(&self.raw[..property.span.start]);
        raw.extend_from_slice(line.as_bytes());
        raw.extend_from_slice(ending);
        raw.extend_from_slice(&self.raw[property.span.end..]);
        Self::parse(raw).expect("replacing the UID line keeps the card valid")
    }

    /// This card with every `X-ADDRESSBOOKSERVER-MEMBER:urn:uuid:<old>`
    /// whose `<old>` is a key of `aliases` naming its new UID instead (CG-14).
    /// Only the UID changes: the group, name, parameters, the `urn:uuid:`
    /// prefix as written and the line ending stay; a folded member line
    /// comes back unfolded, and any whitespace after the UID is not kept.
    /// Members are matched exactly as `member_uids` matches them. Every
    /// other byte is unchanged.
    #[must_use]
    pub fn with_member_uids(&self, aliases: &HashMap<Uid, Uid>) -> Self {
        let mut raw = Vec::with_capacity(self.raw.len());
        let mut copied = 0;
        let mut changed = false;
        for property in self.properties_named("X-ADDRESSBOOKSERVER-MEMBER") {
            let Some((keep, old)) = member_uid(property) else { continue };
            let Some(new) = aliases.get(&old) else { continue };
            let physical = &self.raw[property.span.clone()];
            let ending = line_ending(physical);
            let unfolded = unfold(&physical[..physical.len() - ending.len()]);
            // `value()` is the unfolded line's tail (parser::parse_line), so
            // everything before it is the head as written.
            let head = unfolded.len() - property.value().len();
            raw.extend_from_slice(&self.raw[copied..property.span.start]);
            raw.extend_from_slice(&unfolded[..head + keep]);
            raw.extend_from_slice(new.as_str().as_bytes());
            raw.extend_from_slice(ending);
            copied = property.span.end;
            changed = true;
        }
        if !changed {
            return self.clone();
        }
        raw.extend_from_slice(&self.raw[copied..]);
        Self::parse(raw).expect("rewriting member UIDs keeps the card valid")
    }

    /// An Apple group card (`X-ADDRESSBOOKSERVER-KIND:group`).
    pub fn is_group(&self) -> bool {
        self.properties_named("X-ADDRESSBOOKSERVER-KIND")
            .any(|p| p.value().trim().eq_ignore_ascii_case("group"))
    }

    /// The UIDs a group card lists
    /// (`X-ADDRESSBOOKSERVER-MEMBER:urn:uuid:<uid>`).
    pub fn member_uids(&self) -> Vec<Uid> {
        self.properties_named("X-ADDRESSBOOKSERVER-MEMBER")
            .filter_map(|p| member_uid(p).map(|(_, uid)| uid))
            .collect()
    }
}

/// A member line's UID, and how many bytes of its value to keep before it
/// (leading whitespace plus the `urn:uuid:` prefix as written). `None` for
/// any other member form.
fn member_uid(property: &Property) -> Option<(usize, Uid)> {
    const PREFIX: &str = "urn:uuid:";
    let value = property.value();
    let leading = value.len() - value.trim_start().len();
    let trimmed = value.trim();
    let rest = trimmed.get(PREFIX.len()..)?;
    (trimmed[..PREFIX.len()].eq_ignore_ascii_case(PREFIX) && !rest.is_empty()).then(|| (leading + PREFIX.len(), Uid::from(rest)))
}

/// The line break `line` ends with: CRLF, LF, or none (the last line).
fn line_ending(line: &[u8]) -> &'static [u8] {
    if line.ends_with(b"\r\n") {
        b"\r\n"
    } else if line.ends_with(b"\n") {
        b"\n"
    } else {
        b""
    }
}

/// One property's physical lines (without the final line break) joined the
/// way `parser::unfold` joins them: each fold's line break and its leading
/// space or tab removed.
fn unfold(content: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(content.len());
    for (index, piece) in content.split(|&b| b == b'\n').enumerate() {
        let piece = piece.strip_suffix(b"\r").unwrap_or(piece);
        if index == 0 {
            out.extend_from_slice(piece);
        } else if !piece.is_empty() {
            // The parser skips blank lines inside a folded property.
            out.extend_from_slice(&piece[1..]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contact::HashOptions;

    const CARD: &str =
        "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Jane Doe\r\nUID:fm-1\r\nitem1.EMAIL;type=INTERNET:jane@example.com\r\nitem1.X-ABLabel:_$!<Other>!$_\r\nEND:VCARD\r\n";

    fn uid(value: &str) -> Uid {
        Uid::from(value)
    }

    #[test]
    fn with_uid_replaces_only_the_uid_line() {
        let card = VCard::parse(CARD).unwrap();

        let renamed = card.with_uid(&uid("ic-9"));

        assert_eq!(renamed.uid(), &uid("ic-9"));
        assert_eq!(renamed.as_bytes(), CARD.replace("UID:fm-1\r\n", "UID:ic-9\r\n").as_bytes());
        let without_uid = HashOptions {
            exclude_uid: true,
            ..HashOptions::default()
        };
        assert_eq!(renamed.canonical_hash(without_uid), card.canonical_hash(without_uid));
    }

    #[test]
    fn with_uid_replaces_a_folded_uid_line() {
        let card = VCard::parse("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:fm-\r\n 1\r\nFN:Jane\r\nEND:VCARD\r\n").unwrap();

        let renamed = card.with_uid(&uid("ic-9"));

        assert_eq!(renamed.as_bytes(), b"BEGIN:VCARD\r\nVERSION:3.0\r\nUID:ic-9\r\nFN:Jane\r\nEND:VCARD\r\n");
    }

    #[test]
    fn with_uid_keeps_lf_endings_and_the_group_and_drops_params() {
        let card = VCard::parse("BEGIN:VCARD\nVERSION:3.0\nFN:Jane\nx.UID;VALUE=text:fm-1\nEND:VCARD\n").unwrap();

        let renamed = card.with_uid(&uid("ic-9"));

        assert_eq!(renamed.as_bytes(), b"BEGIN:VCARD\nVERSION:3.0\nFN:Jane\nx.UID:ic-9\nEND:VCARD\n");
    }

    #[test]
    fn group_membership() {
        let group = VCard::parse(include_str!("fixtures/apple_group.vcf")).unwrap();
        assert!(group.is_group());
        assert_eq!(
            group.member_uids(),
            [uid("4F1B6B2A-8C1E-4D6F-9E5A-0B1C2D3E4F50"), uid("9A8B7C6D-5E4F-4A3B-8C2D-1E0F9A8B7C6D")]
        );

        let person = VCard::parse(CARD).unwrap();
        assert!(!person.is_group());
        assert_eq!(person.member_uids(), []);
    }

    #[test]
    fn member_uids_ignores_other_member_forms() {
        // rustfmt note: keep this literal on one line; a wrapped
        // `\`-continuation can land inside a `\r\n` escape and corrupt
        // it (see constraints.md).
        #[rustfmt::skip]
        let group = VCard::parse(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:g1\r\nFN:Team\r\nX-ADDRESSBOOKSERVER-KIND:Group\r\nX-ADDRESSBOOKSERVER-MEMBER:URN:UUID:m1\r\nX-ADDRESSBOOKSERVER-MEMBER:mailto:x\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:\r\nEND:VCARD\r\n",
        )
        .unwrap();
        assert!(group.is_group());
        assert_eq!(group.member_uids(), [uid("m1")]);
    }

    fn aliases(pairs: &[(&str, &str)]) -> std::collections::HashMap<Uid, Uid> {
        pairs.iter().map(|(old, new)| (uid(old), uid(new))).collect()
    }

    #[test]
    fn with_member_uids_rewrites_mapped_members_and_keeps_the_rest() {
        #[rustfmt::skip]
        let raw = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:g1\r\nFN:Team\r\nX-ADDRESSBOOKSERVER-KIND:group\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:fm-1\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:keep\r\nNOTE:urn:uuid:fm-1\r\nEND:VCARD\r\n";
        let group = VCard::parse(raw).unwrap();

        let rewritten = group.with_member_uids(&aliases(&[("fm-1", "ic-1")]));

        assert_eq!(
            rewritten.as_bytes(),
            raw.replacen("MEMBER:urn:uuid:fm-1", "MEMBER:urn:uuid:ic-1", 1).as_bytes(),
            "only the member line changes; NOTE is not a member"
        );
        assert_eq!(rewritten.member_uids(), [uid("ic-1"), uid("keep")]);
    }

    #[test]
    fn with_member_uids_unfolds_a_folded_member_line() {
        #[rustfmt::skip]
        let group = VCard::parse("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:g1\r\nX-ADDRESSBOOKSERVER-KIND:group\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:fm\r\n -1\r\nEND:VCARD\r\n").unwrap();

        let rewritten = group.with_member_uids(&aliases(&[("fm-1", "ic-1")]));

        #[rustfmt::skip]
        let expected: &[u8] = b"BEGIN:VCARD\r\nVERSION:3.0\r\nUID:g1\r\nX-ADDRESSBOOKSERVER-KIND:group\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:ic-1\r\nEND:VCARD\r\n";
        assert_eq!(rewritten.as_bytes(), expected);
    }

    #[test]
    fn with_member_uids_keeps_group_params_prefix_case_and_lf() {
        #[rustfmt::skip]
        let group = VCard::parse("BEGIN:VCARD\nVERSION:3.0\nUID:g1\nX-ADDRESSBOOKSERVER-KIND:group\nitem1.X-ADDRESSBOOKSERVER-MEMBER;X-NOTE=\"a:b\":URN:UUID:fm-1\nEND:VCARD\n").unwrap();

        let rewritten = group.with_member_uids(&aliases(&[("fm-1", "ic-1")]));

        #[rustfmt::skip]
        let expected: &[u8] = b"BEGIN:VCARD\nVERSION:3.0\nUID:g1\nX-ADDRESSBOOKSERVER-KIND:group\nitem1.X-ADDRESSBOOKSERVER-MEMBER;X-NOTE=\"a:b\":URN:UUID:ic-1\nEND:VCARD\n";
        assert_eq!(rewritten.as_bytes(), expected);
    }

    #[test]
    fn with_member_uids_without_a_match_is_byte_identical() {
        let group = VCard::parse(include_str!("fixtures/apple_group.vcf")).unwrap();

        let rewritten = group.with_member_uids(&aliases(&[("nobody", "ic-1")]));

        assert_eq!(rewritten, group);
    }

    #[test]
    fn with_member_uids_survives_a_blank_line_inside_a_fold() {
        #[rustfmt::skip]
        let group = VCard::parse("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:g1\r\nX-ADDRESSBOOKSERVER-KIND:group\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:fm\r\n\r\n -1\r\nEND:VCARD\r\n").unwrap();

        let rewritten = group.with_member_uids(&aliases(&[("fm-1", "ic-1")]));

        #[rustfmt::skip]
        let expected: &[u8] = b"BEGIN:VCARD\r\nVERSION:3.0\r\nUID:g1\r\nX-ADDRESSBOOKSERVER-KIND:group\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:ic-1\r\nEND:VCARD\r\n";
        assert_eq!(rewritten.as_bytes(), expected);
        assert_eq!(rewritten.member_uids(), [uid("ic-1")]);
    }

    #[test]
    fn with_member_uids_rewrites_two_mapped_members_around_an_unmapped_one() {
        #[rustfmt::skip]
        let raw = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:g1\r\nX-ADDRESSBOOKSERVER-KIND:group\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:fm-1\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:keep\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:fm-2\r\nEND:VCARD\r\n";
        #[rustfmt::skip]
        let expected = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:g1\r\nX-ADDRESSBOOKSERVER-KIND:group\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:ic-1\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:keep\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:ic-2\r\nEND:VCARD\r\n";
        let group = VCard::parse(raw).unwrap();

        let rewritten = group.with_member_uids(&aliases(&[("fm-1", "ic-1"), ("fm-2", "ic-2")]));

        assert_eq!(rewritten.as_bytes(), expected.as_bytes());
        assert_eq!(rewritten.member_uids(), [uid("ic-1"), uid("keep"), uid("ic-2")]);
    }
}
