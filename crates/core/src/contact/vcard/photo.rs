use super::{Property, VCard};

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
