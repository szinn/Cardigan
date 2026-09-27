use super::VCard;
use crate::contact::Uid;

impl VCard {
    /// This card with its `UID` property replaced by `[group.]UID:<uid>`,
    /// keeping the original line ending. Folds are replaced with it;
    /// parameters on the old line are dropped. Every other byte is
    /// unchanged. Used when pairing gives a Fastmail card the iCloud UID.
    #[must_use]
    pub fn with_uid(&self, uid: &Uid) -> Self {
        let property = self.properties.iter().find(|p| p.is("UID")).expect("a parsed card always has a UID property");
        let old = &self.raw[property.span.clone()];
        let ending: &[u8] = if old.ends_with(b"\r\n") {
            b"\r\n"
        } else if old.ends_with(b"\n") {
            b"\n"
        } else {
            b""
        };
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

    /// An Apple group card (`X-ADDRESSBOOKSERVER-KIND:group`).
    pub fn is_group(&self) -> bool {
        self.properties_named("X-ADDRESSBOOKSERVER-KIND")
            .any(|p| p.value().trim().eq_ignore_ascii_case("group"))
    }

    /// The UIDs a group card lists
    /// (`X-ADDRESSBOOKSERVER-MEMBER:urn:uuid:<uid>`).
    pub fn member_uids(&self) -> Vec<Uid> {
        const PREFIX: &str = "urn:uuid:";
        self.properties_named("X-ADDRESSBOOKSERVER-MEMBER")
            .filter_map(|p| {
                let value = p.value().trim();
                let rest = value.get(PREFIX.len()..)?;
                let prefix = &value[..PREFIX.len()];
                (prefix.eq_ignore_ascii_case(PREFIX) && !rest.is_empty()).then(|| Uid::from(rest))
            })
            .collect()
    }
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
}
