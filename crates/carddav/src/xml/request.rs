//! Request bodies. Small templates: every href and token is escaped.

use std::fmt::Write;

const PROLOG: &str = r#"<?xml version="1.0" encoding="utf-8"?>"#;

/// Escapes text for an XML element body.
pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

pub(crate) fn propfind_current_user_principal() -> String {
    format!(r#"{PROLOG}<d:propfind xmlns:d="DAV:"><d:prop><d:current-user-principal/></d:prop></d:propfind>"#)
}

pub(crate) fn propfind_addressbook_home_set() -> String {
    format!(r#"{PROLOG}<d:propfind xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav"><d:prop><card:addressbook-home-set/></d:prop></d:propfind>"#)
}

pub(crate) fn propfind_collections() -> String {
    format!(r#"{PROLOG}<d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/><d:displayname/><d:supported-report-set/></d:prop></d:propfind>"#)
}

pub(crate) fn propfind_getetag() -> String {
    format!(r#"{PROLOG}<d:propfind xmlns:d="DAV:"><d:prop><d:getetag/></d:prop></d:propfind>"#)
}

/// RFC 6578 `sync-collection`. `None` sends an empty token: the full
/// membership.
pub(crate) fn sync_collection(token: Option<&str>) -> String {
    let token = token.map_or_else(String::new, escape);
    format!(
        r#"{PROLOG}<d:sync-collection xmlns:d="DAV:"><d:sync-token>{token}</d:sync-token><d:sync-level>1</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>"#
    )
}

/// RFC 6352 `addressbook-multiget` for `hrefs` (canonical server paths).
pub(crate) fn addressbook_multiget(hrefs: &[String]) -> String {
    let hrefs = hrefs.iter().fold(String::new(), |mut out, href| {
        let _ = write!(out, "<d:href>{}</d:href>", escape(href));
        out
    });
    format!(
        r#"{PROLOG}<card:addressbook-multiget xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav"><d:prop><d:getetag/><card:address-data/></d:prop>{hrefs}</card:addressbook-multiget>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_covers_markup_characters() {
        assert_eq!(escape(r#"a&b<c>d"e'f"#), "a&amp;b&lt;c&gt;d&quot;e&apos;f");
        assert_eq!(escape("/card/Zoë.vcf"), "/card/Zoë.vcf");
    }

    #[test]
    fn sync_collection_bodies() {
        assert_eq!(
            sync_collection(None),
            r#"<?xml version="1.0" encoding="utf-8"?><d:sync-collection xmlns:d="DAV:"><d:sync-token></d:sync-token><d:sync-level>1</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>"#
        );
        assert!(sync_collection(Some("https://x.test/sync?a=1&b=2")).contains("<d:sync-token>https://x.test/sync?a=1&amp;b=2</d:sync-token>"));
    }

    #[test]
    fn multiget_lists_escaped_hrefs() {
        let body = addressbook_multiget(&["/card/a.vcf".to_owned(), "/card/b&c.vcf".to_owned()]);
        assert!(body.contains("<d:href>/card/a.vcf</d:href><d:href>/card/b&amp;c.vcf</d:href>"), "{body}");
        assert!(body.contains("<card:address-data/>"), "{body}");
    }

    #[test]
    fn every_body_is_well_formed_xml() {
        for body in [
            propfind_current_user_principal(),
            propfind_addressbook_home_set(),
            propfind_collections(),
            propfind_getetag(),
            sync_collection(Some("t&1")),
            addressbook_multiget(&["/a&b.vcf".to_owned()]),
        ] {
            roxmltree::Document::parse(&body).unwrap_or_else(|e| panic!("{e}: {body}"));
        }
    }
}
