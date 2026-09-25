use std::{
    collections::BTreeMap,
    fmt::{self, Write as _},
};

use sha2::{Digest, Sha256};

use super::{Property, VCard};

/// Version of the canonical form. Bump it whenever `canonical_form` changes
/// so stored hashes can be recognised as stale rather than as edits.
pub const CANONICAL_VERSION: u8 = 1;

/// Properties that change on every server write without a meaningful edit.
const VOLATILE: [&str; 2] = ["REV", "PRODID"];

/// Parameters whose values are case-insensitive, compared as sorted sets.
const CASE_INSENSITIVE_PARAMS: [&str; 4] = ["TYPE", "ENCODING", "VALUE", "CHARSET"];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HashOptions {
    /// Leave `PHOTO` out, for cards whose photo was stripped as oversize.
    pub exclude_photo: bool,
}

/// SHA-256 of a card's canonical form. Comparison only — never sent anywhere.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct CardHash([u8; 32]);

impl CardHash {
    fn of(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        Self(out)
    }

    /// Lowercase hex representation (64 chars).
    pub fn as_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in &self.0 {
            write!(s, "{b:02x}").expect("write to String is infallible");
        }
        s
    }

    /// Parses the 64-character hex form produced by `as_hex`.
    pub fn from_hex(s: impl AsRef<str>) -> Result<Self, crate::Error> {
        let s = s.as_ref();
        if s.len() != 64 {
            return Err(crate::Error::Validation(format!("CardHash hex must be 64 chars, got {}", s.len())));
        }
        let mut out = [0u8; 32];
        for (i, pair) in s.as_bytes().as_chunks::<2>().0.iter().enumerate() {
            let hex = std::str::from_utf8(pair).map_err(|e| crate::Error::Validation(e.to_string()))?;
            out[i] = u8::from_str_radix(hex, 16).map_err(|e| crate::Error::Validation(e.to_string()))?;
        }
        Ok(Self(out))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for CardHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_hex())
    }
}

impl fmt::Debug for CardHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CardHash({})", self.as_hex())
    }
}

impl VCard {
    /// Hash of the canonical form: equal hashes mean "no meaningful change".
    pub fn canonical_hash(&self, options: HashOptions) -> CardHash {
        CardHash::of(self.canonical_form(options).as_bytes())
    }

    /// The comparison-only text form. Rules (version `CANONICAL_VERSION`):
    /// - drop `REV`, `PRODID` (and `PHOTO` when `exclude_photo`);
    /// - upper-case property and parameter names, merge repeated parameters;
    /// - `TYPE`/`ENCODING`/`VALUE`/`CHARSET` values upper-cased, sorted and
    ///   de-duplicated; `ENCODING=BASE64` is `ENCODING=B`;
    /// - in values, `\N` becomes `\n`; all other bytes are kept;
    /// - Apple `itemN.` groups are relabelled `G1..` by their sorted content;
    /// - lines sorted, `\n`-terminated, after a `CANONICAL:<version>` line.
    pub(crate) fn canonical_form(&self, options: HashOptions) -> String {
        let mut lines = Vec::with_capacity(self.properties.len());
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();

        for property in self.properties.iter().filter(|p| !is_excluded(p, options)) {
            let line = canonical_line(property);
            match property.group() {
                Some(group) => groups.entry(group.to_ascii_lowercase()).or_default().push(line),
                None => lines.push(line),
            }
        }

        let mut grouped: Vec<Vec<String>> = groups
            .into_values()
            .map(|mut members| {
                members.sort();
                members
            })
            .collect();
        grouped.sort();
        for (index, members) in grouped.into_iter().enumerate() {
            let label = index + 1;
            lines.extend(members.into_iter().map(|line| format!("G{label}.{line}")));
        }
        lines.sort();

        let mut out = format!("CANONICAL:{CANONICAL_VERSION}\n");
        for line in lines {
            out.push_str(&line);
            out.push('\n');
        }
        out
    }
}

fn is_excluded(property: &Property, options: HashOptions) -> bool {
    VOLATILE.iter().any(|name| property.is(name)) || (options.exclude_photo && property.is("PHOTO"))
}

/// `NAME;PARAM=v1,v2;...:value` without the group prefix.
fn canonical_line(property: &Property) -> String {
    let mut params: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for param in property.params() {
        let name = param.name().to_ascii_uppercase();
        let case_insensitive = CASE_INSENSITIVE_PARAMS.contains(&name.as_str());
        let values = param.values().iter().map(|value| {
            if !case_insensitive {
                return value.clone();
            }
            let value = value.to_ascii_uppercase();
            if name == "ENCODING" && value == "BASE64" { "B".to_owned() } else { value }
        });
        let merged: Vec<String> = values.collect();
        params.entry(name).or_default().extend(merged);
    }

    let mut line = property.name().to_ascii_uppercase();
    for (name, mut values) in params {
        if CASE_INSENSITIVE_PARAMS.contains(&name.as_str()) {
            values.sort();
            values.dedup();
        }
        write!(line, ";{name}={}", values.join(",")).expect("write to String is infallible");
    }
    line.push(':');
    line.push_str(&canonical_value(property.value()));
    line
}

/// Normalises the one equivalent escape spelling: `\N` → `\n`.
fn canonical_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('N') => out.push_str("\\n"),
            Some(next) => {
                out.push('\\');
                out.push(next);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wraps body lines in BEGIN/VERSION/UID/END, CRLF-terminated.
    fn vcf(body: &[&str]) -> String {
        let mut lines = vec!["BEGIN:VCARD", "VERSION:3.0", "UID:u1"];
        lines.extend_from_slice(body);
        lines.push("END:VCARD");
        lines.join("\r\n") + "\r\n"
    }

    fn hash(text: &str) -> CardHash {
        VCard::parse(text).expect("card parses").canonical_hash(HashOptions::default())
    }

    fn hash_without_photo(text: &str) -> CardHash {
        VCard::parse(text).expect("card parses").canonical_hash(HashOptions { exclude_photo: true })
    }

    #[test]
    fn canonical_form_is_exact() {
        let text = [
            "BEGIN:VCARD",
            "VERSION:3.0",
            "PRODID:-//Apple Inc.//iPhone OS 18.0//EN",
            "N:Doe;Jane;;;",
            "FN:Jane Doe",
            "item1.EMAIL;type=INTERNET;type=pref:jane@example.com",
            "item1.X-ABLabel:_$!<Other>!$_",
            "TEL;type=CELL;type=VOICE:+1 555 0100",
            "REV:2026-09-01T00:00:00Z",
            "UID:ABC-123",
            "END:VCARD",
            "",
        ]
        .join("\r\n");
        let card = VCard::parse(text).expect("card parses");
        assert_eq!(
            card.canonical_form(HashOptions::default()),
            "CANONICAL:1\nBEGIN:VCARD\nEND:VCARD\nFN:Jane \
             Doe\nG1.EMAIL;TYPE=INTERNET,PREF:jane@example.com\nG1.X-ABLABEL:_$!<Other>!$_\nN:Doe;Jane;;;\nTEL;TYPE=CELL,VOICE:+1 555 \
             0100\nUID:ABC-123\nVERSION:3.0\n"
        );
    }

    #[test]
    fn property_order_does_not_matter() {
        assert_eq!(hash(&vcf(&["FN:Jane", "NOTE:x"])), hash(&vcf(&["NOTE:x", "FN:Jane"])));
    }

    #[test]
    fn rev_and_prodid_churn_is_ignored() {
        assert_eq!(
            hash(&vcf(&["FN:Jane", "REV:2026-01-01T00:00:00Z", "PRODID:-//Apple//EN"])),
            hash(&vcf(&["FN:Jane", "REV:2026-09-24T10:00:00Z", "PRODID:-//Fastmail//EN"]))
        );
        assert_eq!(hash(&vcf(&["FN:Jane"])), hash(&vcf(&["FN:Jane", "REV:2026-01-01T00:00:00Z"])));
    }

    #[test]
    fn line_endings_and_folding_do_not_matter() {
        let crlf = vcf(&["NOTE:a fairly long note that one server folds and another does not"]);
        let lf_folded = "BEGIN:VCARD\nVERSION:3.0\nUID:u1\nNOTE:a fairly long note that one\n  server folds and another does not\nEND:VCARD\n";
        assert_eq!(hash(&crlf), hash(lf_folded));
    }

    #[test]
    fn parameter_spelling_does_not_matter() {
        let expected = hash(&vcf(&["EMAIL;TYPE=HOME,INTERNET:jane@example.com"]));
        assert_eq!(hash(&vcf(&["EMAIL;type=INTERNET;type=home:jane@example.com"])), expected);
        assert_eq!(hash(&vcf(&["EMAIL;TYPE=\"HOME\";TYPE=internet;TYPE=HOME:jane@example.com"])), expected);
        assert_eq!(hash(&vcf(&["EMAIL;INTERNET;HOME:jane@example.com"])), expected);
    }

    #[test]
    fn encoding_b_and_base64_are_equal() {
        assert_eq!(
            hash(&vcf(&["PHOTO;ENCODING=b;TYPE=JPEG:QUJD"])),
            hash(&vcf(&["PHOTO;TYPE=jpeg;ENCODING=BASE64:QUJD"]))
        );
    }

    #[test]
    fn escaped_newline_case_does_not_matter() {
        assert_eq!(hash(&vcf(&["NOTE:a\\Nb"])), hash(&vcf(&["NOTE:a\\nb"])));
        // `\\N` is an escaped backslash followed by a literal N, not a newline.
        assert_ne!(hash(&vcf(&["NOTE:a\\\\Nb"])), hash(&vcf(&["NOTE:a\\\\nb"])));
    }

    #[test]
    fn multi_valued_order_does_not_matter() {
        assert_eq!(
            hash(&vcf(&["EMAIL:a@example.com", "EMAIL:b@example.com"])),
            hash(&vcf(&["EMAIL:b@example.com", "EMAIL:a@example.com"]))
        );
    }

    #[test]
    fn apple_group_renumbering_does_not_matter() {
        let original = vcf(&[
            "item1.EMAIL:a@example.com",
            "item1.X-ABLabel:school",
            "item2.TEL:+1 555 0100",
            "item2.X-ABLabel:cabin",
        ]);
        let renumbered = vcf(&[
            "ITEM7.TEL:+1 555 0100",
            "item3.X-ABLabel:school",
            "ITEM7.X-ABLabel:cabin",
            "item3.EMAIL:a@example.com",
        ]);
        assert_eq!(hash(&original), hash(&renumbered));
    }

    #[test]
    fn moving_a_label_to_another_item_is_a_change() {
        assert_ne!(
            hash(&vcf(&[
                "item1.EMAIL:a@example.com",
                "item1.X-ABLabel:school",
                "item2.EMAIL:b@example.com",
                "item2.X-ABLabel:cabin"
            ])),
            hash(&vcf(&[
                "item1.EMAIL:a@example.com",
                "item1.X-ABLabel:cabin",
                "item2.EMAIL:b@example.com",
                "item2.X-ABLabel:school"
            ]))
        );
    }

    #[test]
    fn real_edits_are_changes() {
        let base = hash(&vcf(&["FN:Jane", "EMAIL;TYPE=HOME:jane@example.com"]));
        assert_ne!(base, hash(&vcf(&["FN:Jane", "EMAIL;TYPE=HOME:jane@example.org"])));
        assert_ne!(base, hash(&vcf(&["FN:Jane", "EMAIL;TYPE=WORK:jane@example.com"])));
        assert_ne!(base, hash(&vcf(&["FN:Jane", "EMAIL;TYPE=HOME:jane@example.com", "NOTE:new"])));
        assert_ne!(base, hash(&vcf(&["FN:Jane Doe", "EMAIL;TYPE=HOME:jane@example.com"])));
    }

    #[test]
    fn exclude_photo_ignores_photo() {
        let with_photo = vcf(&["FN:Jane", "PHOTO;ENCODING=b;TYPE=JPEG:QUJD"]);
        let without_photo = vcf(&["FN:Jane"]);
        assert_ne!(hash(&with_photo), hash(&without_photo));
        assert_eq!(hash_without_photo(&with_photo), hash_without_photo(&without_photo));
    }

    #[test]
    fn canonical_form_is_versioned() {
        let card = VCard::parse(vcf(&[])).expect("card parses");
        assert!(
            card.canonical_form(HashOptions::default())
                .starts_with(&format!("CANONICAL:{CANONICAL_VERSION}\n"))
        );
    }

    #[test]
    fn card_hash_hex_round_trips() {
        let hash = hash(&vcf(&["FN:Jane"]));
        let hex = hash.as_hex();
        assert_eq!(hex.len(), 64);
        assert_eq!(CardHash::from_hex(&hex).expect("valid hex"), hash);
        assert_eq!(hash.to_string(), hex);
        assert_eq!(format!("{hash:?}"), format!("CardHash({hex})"));
    }

    #[test]
    fn card_hash_from_hex_rejects_bad_input() {
        CardHash::from_hex("abc").unwrap_err();
        CardHash::from_hex("zz".repeat(32)).unwrap_err();
    }
}
