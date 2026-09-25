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
}
