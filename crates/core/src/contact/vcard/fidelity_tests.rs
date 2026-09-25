//! Fidelity and hash-contract tests over hand-authored fixture cards. The
//! fixtures are fictional; replace them with scrubbed captures once CG-12
//! (dump) exists.

use std::fmt::Write as _;

use super::{HashOptions, VCard};

const APPLE_FULL: &[u8] = include_bytes!("fixtures/apple_full.vcf");
const APPLE_FULL_REWRITTEN: &[u8] = include_bytes!("fixtures/apple_full_rewritten.vcf");
const APPLE_GROUP: &[u8] = include_bytes!("fixtures/apple_group.vcf");

const FIXTURES: [(&str, &[u8]); 3] = [
    ("apple_full", APPLE_FULL),
    ("apple_full_rewritten", APPLE_FULL_REWRITTEN),
    ("apple_group", APPLE_GROUP),
];

const EXCLUDE_PHOTO: HashOptions = HashOptions {
    exclude_photo: true,
    exclude_uid: false,
};

fn card(bytes: &[u8]) -> VCard {
    VCard::parse(bytes).expect("fixture parses")
}

fn to_crlf(bytes: &[u8]) -> Vec<u8> {
    std::str::from_utf8(bytes).expect("fixture is UTF-8").replace('\n', "\r\n").into_bytes()
}

/// The fixture with its `PHOTO` property (and continuation lines) removed.
fn without_photo_lines(bytes: &[u8]) -> Vec<u8> {
    let text = std::str::from_utf8(bytes).expect("fixture is UTF-8");
    let mut out = String::new();
    let mut in_photo = false;
    for line in text.split_inclusive('\n') {
        in_photo = line.starts_with("PHOTO") || (in_photo && line.starts_with(' '));
        if !in_photo {
            out.push_str(line);
        }
    }
    out.into_bytes()
}

#[test]
fn fixtures_round_trip_byte_for_byte() {
    for (name, bytes) in FIXTURES {
        assert_eq!(card(bytes).as_bytes(), bytes, "{name}");
        let crlf = to_crlf(bytes);
        assert_eq!(card(&crlf).as_bytes(), crlf.as_slice(), "{name} (CRLF)");
    }
}

#[test]
fn crlf_and_lf_hash_equal() {
    for (name, bytes) in FIXTURES {
        assert_eq!(
            card(bytes).canonical_hash(HashOptions::default()),
            card(&to_crlf(bytes)).canonical_hash(HashOptions::default()),
            "{name}"
        );
    }
}

#[test]
fn server_rewrite_hashes_equal_to_original() {
    let original = card(APPLE_FULL);
    let rewritten = card(APPLE_FULL_REWRITTEN);
    assert_eq!(
        original.canonical_form(HashOptions::default()),
        rewritten.canonical_form(HashOptions::default())
    );
    assert_eq!(original.canonical_hash(EXCLUDE_PHOTO), rewritten.canonical_hash(EXCLUDE_PHOTO));
}

#[test]
fn photo_size_of_fixture() {
    assert_eq!(card(APPLE_FULL).photo_size(), Some(120));
    assert_eq!(card(APPLE_FULL_REWRITTEN).photo_size(), Some(120));
    assert_eq!(card(APPLE_GROUP).photo_size(), None);
}

#[test]
fn strip_photo_keeps_every_other_fixture_byte() {
    let original = card(APPLE_FULL);
    let stripped = original.strip_photo();
    assert_eq!(stripped.as_bytes(), without_photo_lines(APPLE_FULL).as_slice());
    assert_eq!(stripped.canonical_hash(EXCLUDE_PHOTO), original.canonical_hash(EXCLUDE_PHOTO));
    for name in ["X-ABLabel", "X-ABADR", "X-SOCIALPROFILE", "X-ABRELATEDNAMES", "X-CARDIGAN-UNKNOWN"] {
        assert_eq!(stripped.properties_named(name).count(), original.properties_named(name).count(), "{name}");
    }
}

#[test]
fn group_card_keeps_members() {
    let group = card(APPLE_GROUP);
    assert_eq!(group.properties_named("X-ADDRESSBOOKSERVER-MEMBER").count(), 2);
    assert_eq!(group.display_identity().to_string(), "Book Club");
}

#[test]
fn identity_of_fixtures() {
    assert_eq!(card(APPLE_FULL).display_identity().to_string(), "Jane Appleseed (Example Corp)");
    assert!(card(APPLE_FULL).match_keys().is_match(&card(APPLE_FULL_REWRITTEN).match_keys()));
    assert!(!card(APPLE_FULL).match_keys().is_match(&card(APPLE_GROUP).match_keys()));
}

#[test]
fn canonical_form_snapshots() {
    insta::assert_snapshot!("apple_full_canonical", card(APPLE_FULL).canonical_form(HashOptions::default()));
    insta::assert_snapshot!("apple_group_canonical", card(APPLE_GROUP).canonical_form(HashOptions::default()));
}

/// Pins the persisted hash contract (CG-3 stores these hashes). If this
/// snapshot changes, the canonical form changed: bump `CANONICAL_VERSION`
/// and plan how stored hashes are migrated — do not just accept it.
#[test]
fn hash_contract() {
    let mut table = String::new();
    for (name, bytes) in FIXTURES {
        let card = card(bytes);
        writeln!(table, "{name}: {}", card.canonical_hash(HashOptions::default())).expect("write to String is infallible");
        writeln!(table, "{name} (exclude_photo): {}", card.canonical_hash(EXCLUDE_PHOTO)).expect("write to String is infallible");
    }
    insta::assert_snapshot!("hash_contract", table);
}
