//! vCard 3.0 cards kept verbatim, with a lossless parse for inspection.
//!
//! PII: nothing in this module may put property values into `Debug` output
//! or error messages — only the UID, property names, counts and line numbers.

mod canonical;
mod parser;

use std::{fmt, ops::Range};

pub use canonical::{CANONICAL_VERSION, CardHash, HashOptions};

use super::Uid;

/// One vCard 3.0 card: the bytes exactly as received plus a parsed view.
/// Equality is byte equality; use `canonical_hash` to ask "meaningfully
/// different?".
#[derive(Clone, PartialEq, Eq)]
pub struct VCard {
    raw: Vec<u8>,
    properties: Vec<Property>,
    uid: Uid,
}

impl VCard {
    /// Parses one vCard 3.0 card, keeping `raw` byte-for-byte.
    pub fn parse(raw: impl Into<Vec<u8>>) -> Result<Self, VCardError> {
        let raw = raw.into();
        let properties = parser::parse(&raw)?;
        let uid = properties
            .iter()
            .find(|p| p.is("UID"))
            .map(|p| p.value().trim())
            .filter(|v| !v.is_empty())
            .map(Uid::from)
            .ok_or(VCardError::MissingUid)?;

        Ok(Self { raw, properties, uid })
    }

    pub fn uid(&self) -> &Uid {
        &self.uid
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.raw
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.raw
    }

    pub fn properties(&self) -> &[Property] {
        &self.properties
    }

    /// Properties whose name matches `name`, ignoring case and group.
    pub fn properties_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Property> {
        self.properties.iter().filter(move |p| p.is(name))
    }
}

impl fmt::Debug for VCard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VCard")
            .field("uid", &self.uid)
            .field("properties", &self.properties.len())
            .field("bytes", &self.raw.len())
            .finish()
    }
}

/// One unfolded content line: `[group.]name *(;param):value`.
#[derive(Clone, PartialEq, Eq)]
pub struct Property {
    /// Byte range in the card's raw bytes, covering every physical line
    /// (folds included) and the trailing line break.
    span: Range<usize>,
    /// 1-based physical line number where the property starts.
    line: usize,
    group: Option<String>,
    name: String,
    params: Vec<Param>,
    value: String,
}

impl Property {
    pub fn group(&self) -> Option<&str> {
        self.group.as_deref()
    }

    /// The property name as written.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Case-insensitive name comparison.
    pub fn is(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }

    pub fn params(&self) -> &[Param] {
        &self.params
    }

    /// The value after unfolding, still in escaped form.
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl fmt::Debug for Property {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Property")
            .field("group", &self.group)
            .field("name", &self.name)
            .field("params", &self.params.len())
            .field("value_len", &self.value.len())
            .finish_non_exhaustive()
    }
}

/// One property parameter. Values have surrounding quotes removed and comma
/// lists split; a bare parameter (`TEL;HOME:`) is recorded as `TYPE=HOME`.
#[derive(Clone, PartialEq, Eq)]
pub struct Param {
    name: String,
    values: Vec<String>,
}

impl Param {
    /// The parameter name as written.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Case-insensitive name comparison.
    pub fn is(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }

    pub fn values(&self) -> &[String] {
        &self.values
    }
}

impl fmt::Debug for Param {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Param").field("name", &self.name).field("values", &self.values.len()).finish()
    }
}

/// Why a card could not be parsed. Messages carry line numbers only, never
/// card content.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VCardError {
    #[error("vCard line {line} is not valid UTF-8")]
    InvalidUtf8 { line: usize },

    #[error("vCard line {line} is malformed")]
    MalformedLine { line: usize },

    #[error("vCard does not start with BEGIN:VCARD")]
    MissingBegin,

    #[error("vCard has no END:VCARD")]
    MissingEnd,

    #[error("input contains more than one vCard")]
    MultipleCards,

    #[error("vCard has content after END:VCARD at line {line}")]
    TrailingContent { line: usize },

    #[error("vCard has no VERSION")]
    MissingVersion,

    #[error("unsupported vCard version {version}")]
    UnsupportedVersion { version: String },

    #[error("vCard has no UID")]
    MissingUid,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;

    const SIMPLE: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Doe;Jane;;;\r\nFN:Jane \
                          Doe\r\nitem1.EMAIL;type=INTERNET;type=pref:jane@example.com\r\nitem1.X-ABLabel:_$!<Other>!$_\r\nUID:ABC-123\r\nEND:VCARD\r\n";

    fn card(text: &str) -> VCard {
        VCard::parse(text).expect("card parses")
    }

    fn error(text: &[u8]) -> VCardError {
        VCard::parse(text).expect_err("card is rejected")
    }

    #[test]
    fn keeps_raw_bytes_verbatim() {
        assert_eq!(card(SIMPLE).as_bytes(), SIMPLE.as_bytes());
        assert_eq!(card(SIMPLE).into_bytes(), SIMPLE.as_bytes());
    }

    #[test]
    fn parses_group_name_params_and_value() {
        let card = card(SIMPLE);
        let email = &card.properties()[4];
        assert_eq!(email.group(), Some("item1"));
        assert_eq!(email.name(), "EMAIL");
        assert_eq!(email.params().len(), 2);
        assert!(email.params()[1].is("TYPE"));
        assert_eq!(email.params()[1].name(), "type");
        assert_eq!(email.params()[1].values(), ["pref"]);
        assert_eq!(email.value(), "jane@example.com");
        assert_eq!(card.properties_named("x-ablabel").count(), 1);
    }

    #[test]
    fn extracts_uid() {
        assert_eq!(card(SIMPLE).uid().as_str(), "ABC-123");
    }

    #[test]
    fn unfolds_continuation_lines_and_tracks_span() {
        let text = "BEGIN:VCARD\nVERSION:3.0\nUID:u1\nNOTE:hello\n  world\nEND:VCARD\n";
        let card = card(text);
        let note = card.properties_named("NOTE").next().expect("NOTE present");
        assert_eq!(note.value(), "hello world");
        assert_eq!(&card.as_bytes()[note.span.clone()], b"NOTE:hello\n  world\n");
        assert_eq!(note.line, 4);
    }

    #[test]
    fn fold_may_split_a_multibyte_character() {
        let raw: &[u8] = b"BEGIN:VCARD\r\nVERSION:3.0\r\nUID:u1\r\nFN:Ren\xC3\r\n \xA9e\r\nEND:VCARD\r\n";
        let card = VCard::parse(raw).expect("card parses");
        assert_eq!(card.properties_named("FN").next().map(Property::value), Some("Renée"));
    }

    #[test]
    fn accepts_lf_blank_lines_and_missing_final_newline() {
        let card = card("BEGIN:VCARD\nVERSION:3.0\n\nUID:u1\nEND:VCARD");
        assert_eq!(card.properties().len(), 4);
    }

    #[test]
    fn accepts_lowercase_structure() {
        assert_eq!(card("begin:vcard\nversion:3.0\nuid:u1\nend:vcard\n").uid().as_str(), "u1");
    }

    #[test]
    fn quoted_param_value_may_contain_colon_and_semicolon() {
        let card = card("BEGIN:VCARD\nVERSION:3.0\nUID:u1\nX-TEST;X-PARAM=\"a:b;c\":value:with:colons\nEND:VCARD\n");
        let prop = card.properties_named("X-TEST").next().expect("X-TEST present");
        assert_eq!(prop.params()[0].values(), ["a:b;c"]);
        assert_eq!(prop.value(), "value:with:colons");
    }

    #[test]
    fn splits_comma_lists_and_treats_bare_params_as_type() {
        let card = card("BEGIN:VCARD\nVERSION:3.0\nUID:u1\nTEL;HOME;TYPE=VOICE,CELL:+1 555\nEND:VCARD\n");
        let tel = card.properties_named("TEL").next().expect("TEL present");
        assert_eq!(tel.params()[0].name(), "TYPE");
        assert_eq!(tel.params()[0].values(), ["HOME"]);
        assert_eq!(tel.params()[1].values(), ["VOICE", "CELL"]);
    }

    #[test]
    fn rejects_bad_structure() {
        assert_eq!(error(b""), VCardError::MissingBegin);
        assert_eq!(error(b"VERSION:3.0\nBEGIN:VCARD\nUID:u1\nEND:VCARD\n"), VCardError::MissingBegin);
        assert_eq!(error(b"BEGIN:VCARD\nVERSION:3.0\nUID:u1\n"), VCardError::MissingEnd);
        assert_eq!(
            error(b"BEGIN:VCARD\nVERSION:3.0\nUID:u1\nEND:VCARD\nBEGIN:VCARD\nEND:VCARD\n"),
            VCardError::MultipleCards
        );
        assert_eq!(
            error(b"BEGIN:VCARD\nVERSION:3.0\nUID:u1\nEND:VCARD\nNOTE:after\n"),
            VCardError::TrailingContent { line: 5 }
        );
        assert_eq!(error(b"BEGIN:VCARD\nUID:u1\nEND:VCARD\n"), VCardError::MissingVersion);
        assert_eq!(error(b"BEGIN:VCARD\nVERSION:3.0\nUID: \nEND:VCARD\n"), VCardError::MissingUid);
        assert_eq!(error(b"BEGIN:VCARD\nVERSION:3.0\nEND:VCARD\n"), VCardError::MissingUid);
    }

    #[test]
    fn rejects_vcard_4() {
        assert_eq!(
            error(b"BEGIN:VCARD\nVERSION:4.0\nUID:u1\nEND:VCARD\n"),
            VCardError::UnsupportedVersion { version: "4.0".to_owned() }
        );
    }

    #[test]
    fn reports_malformed_and_non_utf8_lines_by_number() {
        assert_eq!(
            error(b"BEGIN:VCARD\nVERSION:3.0\nNOTE no colon\nEND:VCARD\n"),
            VCardError::MalformedLine { line: 3 }
        );
        assert_eq!(error(b"BEGIN:VCARD\nVERSION:3.0\n:no name\nEND:VCARD\n"), VCardError::MalformedLine { line: 3 });
        assert_eq!(error(b"BEGIN:VCARD\nVERSION:3.0\nFN:\xFF\nEND:VCARD\n"), VCardError::InvalidUtf8 { line: 3 });
    }

    #[test]
    fn errors_never_contain_card_content() {
        let malformed = error(b"BEGIN:VCARD\nVERSION:3.0\nNOTE secret-value\nEND:VCARD\n");
        assert!(!malformed.to_string().contains("secret"));
        let version = error(b"BEGIN:VCARD\nVERSION:4.0 secret\nUID:u1\nEND:VCARD\n");
        assert_eq!(version, VCardError::UnsupportedVersion { version: "4.0".to_owned() });
    }

    #[test]
    fn debug_output_is_redacted() {
        let card = card(SIMPLE);
        let debug = format!("{card:?} {:?}", card.properties());
        assert!(debug.contains("ABC-123"), "UID is loggable: {debug}");
        assert!(!debug.contains("jane@example.com"), "{debug}");
        assert!(!debug.contains("Jane"), "{debug}");
    }

    #[test]
    fn param_debug_output_is_redacted() {
        let card = card("BEGIN:VCARD\nVERSION:3.0\nUID:u1\nX-TEST;X-SECRET=hunter2:v\nEND:VCARD\n");
        let prop = card.properties_named("X-TEST").next().expect("X-TEST present");
        let debug = format!("{:?}", prop.params());
        assert!(!debug.contains("hunter2"), "{debug}");
    }

    #[test]
    fn converts_into_core_error_as_invalid_input() {
        let err: crate::Error = VCardError::MissingUid.into();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }
}
