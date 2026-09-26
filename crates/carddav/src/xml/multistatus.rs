use std::fmt;

use cg_core::AddressBookError;
use roxmltree::{Document, Node};

pub(crate) const DAV: &str = "DAV:";
pub(crate) const CARDDAV: &str = "urn:ietf:params:xml:ns:carddav";

/// A parsed 207 body.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Multistatus {
    pub responses: Vec<Entry>,
    /// Top-level `DAV:sync-token` (sync-collection responses only).
    pub sync_token: Option<String>,
}

/// One `DAV:response`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Entry {
    /// `DAV:href` text as the server sent it (not canonical yet).
    pub href: String,
    /// Response-level status: 404 for a removed member, 507 for a truncated
    /// sync-collection.
    pub status: Option<u16>,
    /// The first non-2xx propstat status, if any.
    pub propstat_error: Option<u16>,
    /// Properties from 2xx propstats.
    pub props: Props,
}

/// The properties this crate reads. `address_data` is card content (PII):
/// `Debug` prints its length only.
#[derive(Default, PartialEq, Eq)]
pub(crate) struct Props {
    pub etag: Option<String>,
    /// `CARDDAV:address-data`, with the parser's LF line ends (see
    /// `restore_crlf`).
    pub address_data: Option<String>,
    /// `DAV:current-user-principal`'s href.
    pub current_user_principal: Option<String>,
    /// `CARDDAV:addressbook-home-set` hrefs.
    pub addressbook_home_set: Vec<String>,
    /// `DAV:resourcetype` contains `CARDDAV:addressbook`.
    pub is_addressbook: bool,
    pub displayname: Option<String>,
    /// `DAV:supported-report-set` lists `DAV:sync-collection`.
    pub supports_sync_collection: bool,
}

impl fmt::Debug for Props {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Props")
            .field("etag", &self.etag)
            .field("address_data_bytes", &self.address_data.as_ref().map(String::len))
            .field("current_user_principal", &self.current_user_principal)
            .field("addressbook_home_set", &self.addressbook_home_set)
            .field("is_addressbook", &self.is_addressbook)
            .field("displayname", &self.displayname)
            .field("supports_sync_collection", &self.supports_sync_collection)
            .finish()
    }
}

/// Parses a 207 multistatus body. Elements are matched by namespace URI and
/// local name, so any prefix works. DTDs are rejected (roxmltree default).
pub(crate) fn parse(body: &[u8]) -> Result<Multistatus, AddressBookError> {
    let text = std::str::from_utf8(body).map_err(|_| malformed("body is not UTF-8"))?;
    let doc = Document::parse(text).map_err(|_| malformed("not well-formed XML"))?;
    let root = doc.root_element();
    if !is(root, DAV, "multistatus") {
        return Err(malformed("root element is not DAV:multistatus"));
    }
    let mut multistatus = Multistatus::default();
    for child in root.children().filter(Node::is_element) {
        if is(child, DAV, "response") {
            multistatus.responses.push(parse_response(child)?);
        } else if is(child, DAV, "sync-token") {
            multistatus.sync_token = non_empty(&text_of(child));
        }
    }
    Ok(multistatus)
}

/// True when `body` is a `DAV:error` naming the `DAV:valid-sync-token`
/// precondition (RFC 6578 §3.2).
pub(crate) fn is_valid_sync_token_error(body: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(body) else { return false };
    let Ok(doc) = Document::parse(text) else { return false };
    let root = doc.root_element();
    is(root, DAV, "error") && root.descendants().any(|node| is(node, DAV, "valid-sync-token"))
}

/// The parser turns a raw CR LF into LF (XML end-of-line handling) but keeps
/// `&#13;` as CR. vCard needs CRLF, so every LF not already preceded by CR
/// gets one.
pub(crate) fn restore_crlf(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + text.len() / 32);
    let mut previous = 0u8;
    for &byte in text.as_bytes() {
        if byte == b'\n' && previous != b'\r' {
            out.push(b'\r');
        }
        out.push(byte);
        previous = byte;
    }
    out
}

fn malformed(what: &str) -> AddressBookError {
    AddressBookError::Permanent(format!("malformed multistatus: {what}"))
}

fn is(node: Node<'_, '_>, namespace: &str, name: &str) -> bool {
    node.is_element() && node.tag_name().namespace() == Some(namespace) && node.tag_name().name() == name
}

fn child<'a, 'input>(node: Node<'a, 'input>, namespace: &str, name: &str) -> Option<Node<'a, 'input>> {
    node.children().find(|c| is(*c, namespace, name))
}

/// Concatenated text (and CDATA) children.
fn text_of(node: Node<'_, '_>) -> String {
    node.children().filter(Node::is_text).filter_map(|c| c.text()).collect()
}

fn non_empty(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// `HTTP/1.1 404 Not Found` → 404.
fn parse_status(line: &str) -> Option<u16> {
    line.split_whitespace().nth(1)?.parse().ok()
}

fn parse_response(node: Node<'_, '_>) -> Result<Entry, AddressBookError> {
    let href = child(node, DAV, "href")
        .and_then(|h| non_empty(&text_of(h)))
        .ok_or_else(|| malformed("response without href"))?;
    let mut entry = Entry {
        href,
        status: child(node, DAV, "status").and_then(|s| parse_status(&text_of(s))),
        ..Entry::default()
    };
    for propstat in node.children().filter(|c| is(*c, DAV, "propstat")) {
        let code = child(propstat, DAV, "status")
            .and_then(|s| parse_status(&text_of(s)))
            .ok_or_else(|| malformed("propstat without status"))?;
        if !(200..300).contains(&code) {
            entry.propstat_error.get_or_insert(code);
            continue;
        }
        if let Some(prop) = child(propstat, DAV, "prop") {
            read_props(prop, &mut entry.props);
        }
    }
    Ok(entry)
}

fn read_props(prop: Node<'_, '_>, props: &mut Props) {
    for p in prop.children().filter(Node::is_element) {
        let name = p.tag_name();
        match (name.namespace(), name.name()) {
            (Some(DAV), "getetag") => props.etag = non_empty(&text_of(p)),
            (Some(CARDDAV), "address-data") => props.address_data = Some(text_of(p)),
            (Some(DAV), "current-user-principal") => {
                props.current_user_principal = child(p, DAV, "href").and_then(|h| non_empty(&text_of(h)));
            }
            (Some(CARDDAV), "addressbook-home-set") => {
                props.addressbook_home_set = p.children().filter(|c| is(*c, DAV, "href")).filter_map(|h| non_empty(&text_of(h))).collect();
            }
            (Some(DAV), "resourcetype") => props.is_addressbook = child(p, CARDDAV, "addressbook").is_some(),
            (Some(DAV), "displayname") => props.displayname = non_empty(&text_of(p)),
            (Some(DAV), "supported-report-set") => {
                props.supports_sync_collection = p.descendants().any(|d| is(d, DAV, "sync-collection"));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::multistatus;

    const DEFAULT_NS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<multistatus xmlns="DAV:">
  <response>
    <href>/123/carddavhome/card/a.vcf</href>
    <propstat>
      <prop><getetag>"e1"</getetag><address-data xmlns="urn:ietf:params:xml:ns:carddav">BEGIN:VCARD&#13;
VERSION:3.0&#13;
END:VCARD&#13;
</address-data></prop>
      <status>HTTP/1.1 200 OK</status>
    </propstat>
  </response>
  <sync-token>https://example.test/sync/1</sync-token>
</multistatus>"#;

    const PREFIXED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav">
  <D:response>
    <D:href>/123/carddavhome/card/a.vcf</D:href>
    <D:propstat>
      <D:prop><D:getetag>"e1"</D:getetag><C:address-data>BEGIN:VCARD&#13;
VERSION:3.0&#13;
END:VCARD&#13;
</C:address-data></D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
  <D:sync-token>https://example.test/sync/1</D:sync-token>
</D:multistatus>"#;

    #[test]
    fn prefixes_do_not_matter() {
        let a = parse(DEFAULT_NS.as_bytes()).unwrap();
        let b = parse(PREFIXED.as_bytes()).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.sync_token.as_deref(), Some("https://example.test/sync/1"));
        assert_eq!(a.responses.len(), 1);
        let entry = &a.responses[0];
        assert_eq!(entry.href, "/123/carddavhome/card/a.vcf");
        assert_eq!(entry.status, None);
        assert_eq!(entry.props.etag.as_deref(), Some("\"e1\""));
        assert_eq!(entry.props.address_data.as_deref(), Some("BEGIN:VCARD\r\nVERSION:3.0\r\nEND:VCARD\r\n"));
    }

    #[test]
    fn raw_crlf_is_restored_after_parsing() {
        let body = multistatus(
            "<d:response><d:href>/c/a.vcf</d:href><d:propstat><d:prop><d:getetag>\"e\"</d:getetag><card:address-data>BEGIN:VCARD\r\nFN:Zoë \
             Ñandú\r\nEND:VCARD\r\n</card:address-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>",
        );
        let parsed = parse(body.as_bytes()).unwrap();
        let data = parsed.responses[0].props.address_data.as_deref().unwrap();
        assert_eq!(data, "BEGIN:VCARD\nFN:Zoë Ñandú\nEND:VCARD\n");
        assert_eq!(restore_crlf(data), "BEGIN:VCARD\r\nFN:Zoë Ñandú\r\nEND:VCARD\r\n".as_bytes());
    }

    #[test]
    fn restore_crlf_leaves_existing_crlf_alone() {
        assert_eq!(restore_crlf("A\nB\r\nC\n"), b"A\r\nB\r\nC\r\n");
        assert_eq!(restore_crlf(""), b"");
    }

    #[test]
    #[rustfmt::skip] // A wrapped `\r\n` escape inside this literal would be mangled by rustfmt's line-continuation rewrap.
    fn cdata_address_data() {
        let body = multistatus(
            "<d:response><d:href>/c/a.vcf</d:href><d:propstat><d:prop><d:getetag>\"e\"</d:getetag><card:address-data><![CDATA[BEGIN:VCARD\r\nNOTE:a<b&c\r\nEND:VCARD\r\n]]></card:address-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>",
        );
        let parsed = parse(body.as_bytes()).unwrap();
        assert_eq!(parsed.responses[0].props.address_data.as_deref(), Some("BEGIN:VCARD\nNOTE:a<b&c\nEND:VCARD\n"));
    }

    #[test]
    fn response_and_propstat_statuses() {
        let body = multistatus(
            "<d:response><d:href>/c/gone.vcf</d:href><d:status>HTTP/1.1 404 Not \
             Found</d:status></d:response><d:response><d:href>/c/</d:href><d:status>HTTP/1.1 507 Insufficient \
             Storage</d:status></d:response><d:response><d:href>/c/half.vcf</d:href><d:propstat><d:prop><d:getetag>\"e\"</d:getetag></d:prop><d:status>HTTP/1.\
             1 200 OK</d:status></d:propstat><d:propstat><d:prop><card:address-data/></d:prop><d:status>HTTP/1.1 404 Not \
             Found</d:status></d:propstat></d:response><d:sync-token>t2</d:sync-token>",
        );
        let parsed = parse(body.as_bytes()).unwrap();
        assert_eq!(parsed.responses[0].status, Some(404));
        assert_eq!(parsed.responses[1].status, Some(507));
        let half = &parsed.responses[2];
        assert_eq!(half.status, None);
        assert_eq!(half.propstat_error, Some(404));
        assert_eq!(half.props.etag.as_deref(), Some("\"e\""));
        assert_eq!(half.props.address_data, None);
        assert_eq!(parsed.sync_token.as_deref(), Some("t2"));
    }

    #[test]
    fn discovery_properties() {
        let body = multistatus(
            "<d:response><d:href>/</d:href><d:propstat><d:prop>\
               <d:current-user-principal><d:href>/123/principal/</d:href></d:current-user-principal>\
               <card:addressbook-home-set><d:href>https://p42-contacts.icloud.com:443/123/carddavhome/</d:href><d:href>/second/</d:href></card:addressbook-home-set>\
             </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>\
             <d:response><d:href>/home/card/</d:href><d:propstat><d:prop>\
               <d:resourcetype><d:collection/><card:addressbook/></d:resourcetype>\
               <d:displayname>Contacts</d:displayname>\
               <d:supported-report-set><d:supported-report><d:report><d:sync-collection/></d:report></d:supported-report></d:supported-report-set>\
             </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>\
             <d:response><d:href>/home/</d:href><d:propstat><d:prop>\
               <d:resourcetype><d:collection/></d:resourcetype><d:getetag></d:getetag>\
             </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>",
        );
        let parsed = parse(body.as_bytes()).unwrap();
        let root = &parsed.responses[0].props;
        assert_eq!(root.current_user_principal.as_deref(), Some("/123/principal/"));
        assert_eq!(root.addressbook_home_set, ["https://p42-contacts.icloud.com:443/123/carddavhome/", "/second/"]);
        let book = &parsed.responses[1].props;
        assert!(book.is_addressbook);
        assert!(book.supports_sync_collection);
        assert_eq!(book.displayname.as_deref(), Some("Contacts"));
        let home = &parsed.responses[2].props;
        assert!(!home.is_addressbook);
        assert!(!home.supports_sync_collection);
        assert_eq!(home.etag, None, "an empty getetag is no etag");
    }

    #[test]
    fn malformed_bodies_are_permanent() {
        let bodies: [&[u8]; 5] = [
            b"not xml",
            b"\xff\xfe<d:multistatus xmlns:d=\"DAV:\"/>",
            br#"<d:error xmlns:d="DAV:"/>"#,
            br#"<?xml version="1.0"?><!DOCTYPE x [<!ENTITY e "boom">]><d:multistatus xmlns:d="DAV:"/>"#,
            br#"<d:multistatus xmlns:d="DAV:"><d:response><d:status>HTTP/1.1 200 OK</d:status></d:response></d:multistatus>"#,
        ];
        for body in bodies {
            match parse(body) {
                Err(AddressBookError::Permanent(message)) => assert!(message.starts_with("malformed multistatus"), "{message}"),
                other => panic!("expected Permanent, got {other:?}"),
            }
        }
    }

    #[test]
    fn valid_sync_token_error_detection() {
        assert!(is_valid_sync_token_error(
            br#"<?xml version="1.0"?><D:error xmlns:D="DAV:"><D:valid-sync-token/></D:error>"#
        ));
        assert!(is_valid_sync_token_error(br#"<error xmlns="DAV:"><valid-sync-token/></error>"#));
        assert!(!is_valid_sync_token_error(br#"<d:error xmlns:d="DAV:"><d:need-privileges/></d:error>"#));
        assert!(!is_valid_sync_token_error(b"Forbidden"));
        assert!(!is_valid_sync_token_error(b""));
    }

    #[test]
    fn props_debug_omits_card_content() {
        let props = Props {
            address_data: Some("BEGIN:VCARD\nEMAIL:jane@example.com\nEND:VCARD\n".into()),
            ..Props::default()
        };
        let debug = format!("{props:?}");
        assert!(!debug.contains("jane@example.com"), "card content leaked into Debug: {debug}");
    }
}
