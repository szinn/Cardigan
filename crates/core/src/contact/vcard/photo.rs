use base64::{
    Engine, alphabet,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};

use super::{Property, VCard};
use crate::contact::{CardPhoto, PhotoData, PhotoUri};

impl VCard {
    /// Estimated decoded size in bytes of the embedded (base64) photo data,
    /// summed over all embedded `PHOTO` properties. `None` when the card has
    /// no embedded photo; URI-valued photos have no size.
    pub fn photo_size(&self) -> Option<u64> {
        let mut sizes = self
            .properties
            .iter()
            .filter(|p| is_embedded_photo(p))
            .map(|p| decoded_len(p.value()))
            .peekable();
        sizes.peek()?;
        Some(sizes.sum())
    }

    /// This card with every embedded `PHOTO` property removed. All other
    /// bytes are copied unchanged — nothing is re-serialized.
    #[must_use]
    pub fn strip_photo(&self) -> Self {
        let mut raw = Vec::with_capacity(self.raw.len());
        let mut cursor = 0;
        for photo in self.properties.iter().filter(|p| is_embedded_photo(p)) {
            raw.extend_from_slice(&self.raw[cursor..photo.span.start]);
            cursor = photo.span.end;
        }
        raw.extend_from_slice(&self.raw[cursor..]);

        Self::parse(raw).expect("removing PHOTO properties leaves BEGIN/VERSION/UID/END intact")
    }

    /// This card with every `PHOTO` property removed, embedded or URI. All
    /// other bytes are copied unchanged. The sync engine records and pushes
    /// cards in this form: photos are not synced in v1.
    #[must_use]
    pub fn without_photos(&self) -> Self {
        let mut raw = Vec::with_capacity(self.raw.len());
        let mut cursor = 0;
        for photo in self.properties.iter().filter(|p| p.is("PHOTO")) {
            raw.extend_from_slice(&self.raw[cursor..photo.span.start]);
            cursor = photo.span.end;
        }
        raw.extend_from_slice(&self.raw[cursor..]);

        Self::parse(raw).expect("removing PHOTO properties leaves BEGIN/VERSION/UID/END intact")
    }

    /// This card with `other`'s `PHOTO` properties (embedded or URI) added,
    /// copied byte-for-byte before `END:VCARD`. Every byte of this card is
    /// kept. Returns an unchanged copy when this card has a `PHOTO` of its
    /// own, or when `other` has none. Used so a write keeps the target card's
    /// own photo.
    #[must_use]
    pub fn with_photos_of(&self, other: &Self) -> Self {
        let photos: Vec<&Property> = other.properties.iter().filter(|p| p.is("PHOTO")).collect();
        if photos.is_empty() || self.properties.iter().any(|p| p.is("PHOTO")) {
            return self.clone();
        }
        let end = self.properties.last().expect("a parsed card ends with END:VCARD");
        let mut raw = Vec::with_capacity(self.raw.len() + photos.iter().map(|p| p.span.len()).sum::<usize>());
        raw.extend_from_slice(&self.raw[..end.span.start]);
        for photo in photos {
            raw.extend_from_slice(&other.raw[photo.span.clone()]);
        }
        raw.extend_from_slice(&self.raw[end.span.start..]);
        Self::parse(raw).expect("adding PHOTO lines keeps the card valid")
    }

    /// The first `PHOTO` property's identity (CG-15 R1); `None` without one.
    pub fn photo(&self) -> Option<CardPhoto> {
        let property = self.properties.iter().find(|p| p.is("PHOTO"))?;
        if is_embedded_photo(property) {
            let text: String = property.value().chars().filter(|c| !c.is_ascii_whitespace()).collect();
            return Some(
                BASE64
                    .decode(text)
                    .map_or(CardPhoto::Unreadable, |bytes| CardPhoto::Inline(PhotoData::new(bytes))),
            );
        }
        let is_uri = property
            .params()
            .iter()
            .any(|param| param.is("VALUE") && param.values().iter().any(|v| v.eq_ignore_ascii_case("uri")));
        let value = property.value().trim();
        if is_uri || value.starts_with("https://") || value.starts_with("http://") {
            return Some(CardPhoto::Uri(PhotoUri::from(value.to_owned())));
        }
        Some(CardPhoto::Unreadable)
    }

    /// This card with every `PHOTO` replaced by one inline
    /// `PHOTO;ENCODING=b[;TYPE=…]:` line, folded at 75 octets, before
    /// `END:VCARD`. Every other byte is unchanged. Line breaks follow the
    /// card's `END` line (CRLF or LF).
    #[must_use]
    pub fn with_inline_photo(&self, bytes: &[u8]) -> Self {
        let base = self.without_photos();
        let end = base.properties.last().expect("a parsed card ends with END:VCARD");
        let ending: &str = if base.raw[end.span.clone()].ends_with(b"\r\n") { "\r\n" } else { "\n" };
        let mut line = String::from("PHOTO;ENCODING=b");
        if let Some(kind) = image_type(bytes) {
            line.push_str(";TYPE=");
            line.push_str(kind);
        }
        line.push(':');
        line.push_str(&BASE64.encode(bytes));
        let mut raw = Vec::with_capacity(base.raw.len() + line.len() + line.len() / 74 * 3);
        raw.extend_from_slice(&base.raw[..end.span.start]);
        raw.extend_from_slice(fold(&line, ending).as_bytes());
        raw.extend_from_slice(&base.raw[end.span.start..]);
        Self::parse(raw).expect("adding a PHOTO line keeps the card valid")
    }
}

/// Standard alphabet, padding optional on decode (servers vary).
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// The vCard `TYPE` for a sniffed image format.
fn image_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("JPEG")
    } else if bytes.starts_with(b"\x89PNG") {
        Some("PNG")
    } else if bytes.starts_with(b"GIF8") {
        Some("GIF")
    } else {
        None
    }
}

/// `line` folded at 75 octets (RFC 6350 §3.2): continuation lines start
/// with one space. `line` is ASCII here, so octets are chars.
fn fold(line: &str, ending: &str) -> String {
    let mut out = String::with_capacity(line.len() + line.len() / 74 * (ending.len() + 1) + ending.len());
    let (first, mut rest) = line.split_at(line.len().min(75));
    out.push_str(first);
    out.push_str(ending);
    while !rest.is_empty() {
        let (chunk, tail) = rest.split_at(rest.len().min(74));
        out.push(' ');
        out.push_str(chunk);
        out.push_str(ending);
        rest = tail;
    }
    out
}

fn is_embedded_photo(property: &Property) -> bool {
    property.is("PHOTO")
        && property
            .params()
            .iter()
            .any(|param| param.is("ENCODING") && param.values().iter().any(|v| v.eq_ignore_ascii_case("b") || v.eq_ignore_ascii_case("BASE64")))
}

/// Decoded length of base64 text without decoding it.
fn decoded_len(value: &str) -> u64 {
    let symbols = value.bytes().filter(|b| !b.is_ascii_whitespace());
    let chars = symbols.clone().count() as u64;
    let padding = symbols.rev().take_while(|&b| b == b'=').count() as u64;
    (chars * 3 / 4).saturating_sub(padding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contact::HashOptions;

    fn card(text: &str) -> VCard {
        VCard::parse(text).expect("card parses")
    }

    fn with_body(body: &str) -> String {
        format!("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:u1\r\nFN:Jane\r\n{body}END:VCARD\r\n")
    }

    #[test]
    fn photo_size_estimates_decoded_bytes() {
        assert_eq!(card(&with_body("PHOTO;ENCODING=b;TYPE=JPEG:QUJDQUJD\r\n")).photo_size(), Some(6));
        assert_eq!(card(&with_body("PHOTO;ENCODING=b:QUI=\r\n")).photo_size(), Some(2));
        assert_eq!(card(&with_body("PHOTO;ENCODING=b:QQ==\r\n")).photo_size(), Some(1));
        assert_eq!(card(&with_body("PHOTO;ENCODING=b:QUJD\r\n QUJD\r\n")).photo_size(), Some(6));
        assert_eq!(card(&with_body("photo;encoding=BASE64:QUJD\r\n")).photo_size(), Some(3));
    }

    #[test]
    fn cards_without_embedded_photo_have_no_size() {
        assert_eq!(card(&with_body("")).photo_size(), None);
        assert_eq!(card(&with_body("PHOTO;VALUE=uri:https://example.com/p.jpg\r\n")).photo_size(), None);
    }

    #[test]
    fn strip_photo_removes_only_embedded_photo_lines() {
        let original = with_body("item1.X-ABLabel:keep\r\nPHOTO;ENCODING=b;TYPE=JPEG:QUJD\r\n QUJD\r\nX-UNKNOWN;X-P=1:keep too\r\n");
        let stripped = card(&original).strip_photo();
        assert_eq!(
            stripped.as_bytes(),
            with_body("item1.X-ABLabel:keep\r\nX-UNKNOWN;X-P=1:keep too\r\n").as_bytes()
        );
        assert_eq!(stripped.photo_size(), None);
        assert_eq!(stripped.uid().as_str(), "u1");
    }

    #[test]
    fn strip_photo_keeps_uri_photos() {
        let original = with_body("PHOTO;VALUE=uri:https://example.com/p.jpg\r\n");
        assert_eq!(card(&original).strip_photo().as_bytes(), original.as_bytes());
    }

    #[test]
    fn stripped_card_hashes_equal_when_photo_excluded() {
        let original = card(&with_body("PHOTO;ENCODING=b;TYPE=JPEG:QUJD\r\n"));
        let stripped = original.strip_photo();
        let exclude = HashOptions {
            exclude_photo: true,
            ..HashOptions::default()
        };
        assert_eq!(original.canonical_hash(exclude), stripped.canonical_hash(exclude));
        assert_ne!(original.canonical_hash(HashOptions::default()), stripped.canonical_hash(HashOptions::default()));
    }

    #[test]
    fn without_photos_removes_embedded_and_uri_photos() {
        let pictured = card(&with_body(
            "NOTE:x\r\nPHOTO;ENCODING=b;TYPE=JPEG:QUJD\r\nPHOTO;VALUE=uri:https://example.com/p.jpg\r\n",
        ));
        assert_eq!(pictured.without_photos().as_bytes(), with_body("NOTE:x\r\n").as_bytes());
    }

    #[test]
    fn with_photos_of_copies_the_other_cards_photos_before_end() {
        let plain = card(&with_body("NOTE:edited\r\n"));
        let pictured = card(&with_body("PHOTO;VALUE=uri:https://example.com/p.jpg\r\nNOTE:old\r\n"));

        let merged = plain.with_photos_of(&pictured);

        assert_eq!(
            merged.as_bytes(),
            with_body("NOTE:edited\r\nPHOTO;VALUE=uri:https://example.com/p.jpg\r\n").as_bytes()
        );
    }

    #[test]
    fn with_photos_of_leaves_cards_it_should_not_touch() {
        let pictured = card(&with_body("PHOTO;ENCODING=b;TYPE=JPEG:QUJD\r\n"));
        let other = card(&with_body("PHOTO;VALUE=uri:https://example.com/p.jpg\r\n"));
        assert_eq!(pictured.with_photos_of(&other), pictured);

        let plain = card(&with_body("NOTE:x\r\n"));
        assert_eq!(plain.with_photos_of(&card(&with_body(""))), plain);
    }
}

#[cfg(test)]
mod photo_identity_tests {
    use crate::contact::{CardPhoto, PhotoHash, PhotoUri, VCard};

    fn card(photo_line: &str) -> VCard {
        VCard::parse(format!("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:u1\r\nFN:Jane\r\n{photo_line}END:VCARD\r\n")).unwrap()
    }

    #[test]
    fn inline_photo_decodes_either_encoding_case() {
        for line in [
            "PHOTO;ENCODING=b;TYPE=JPEG:QUJD\r\n",
            "PHOTO;ENCODING=B:QUJD\r\n",
            "PHOTO;ENCODING=B:QU\r\n JD\r\n",
        ] {
            match card(line).photo() {
                Some(CardPhoto::Inline(data)) => assert_eq!(&*data.bytes, b"ABC", "{line:?}"),
                other => panic!("{line:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn uri_photo_and_first_photo_wins() {
        #[rustfmt::skip]
        let c = card("PHOTO;X-ABCROP-RECTANGLE=ABClipRect_1&0&0&1&1&x;VALUE=uri:https://gateway.icloud.com/a/b\r\nPHOTO;ENCODING=b:QUJD\r\n");
        assert_eq!(c.photo(), Some(CardPhoto::Uri(PhotoUri::from("https://gateway.icloud.com/a/b".to_owned()))));
        assert_eq!(card("").photo(), None);
        assert_eq!(card("PHOTO;ENCODING=b:!!!\r\n").photo(), Some(CardPhoto::Unreadable));
    }

    #[test]
    fn with_inline_photo_replaces_every_photo_and_folds() {
        let c = card("PHOTO;VALUE=uri:https://gateway.icloud.com/a/b\r\nPHOTO;ENCODING=b:QUJD\r\n");
        let jpeg = [&[0xFF, 0xD8, 0xFF][..], &[7u8; 100][..]].concat();

        let replaced = c.with_inline_photo(&jpeg);

        let text = String::from_utf8(replaced.as_bytes().to_vec()).unwrap();
        assert_eq!(text.matches("PHOTO").count(), 1, "{text}");
        assert!(text.contains("PHOTO;ENCODING=b;TYPE=JPEG:"), "{text}");
        assert!(text.lines().all(|line| line.len() <= 75), "folded at 75 octets: {text}");
        assert_eq!(replaced.photo(), Some(CardPhoto::Inline(crate::contact::PhotoData::new(jpeg))));
        assert!(text.ends_with("END:VCARD\r\n"));
        assert_eq!(PhotoHash::of(b"x"), PhotoHash::of(b"x"));
    }

    #[test]
    fn with_inline_photo_omits_type_for_unknown_bytes() {
        let text = String::from_utf8(card("").with_inline_photo(b"ABC").as_bytes().to_vec()).unwrap();
        assert!(text.contains("PHOTO;ENCODING=b:QUJD\r\n"), "{text}");
    }
}
