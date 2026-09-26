use cg_core::AddressBookError;
use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};
use url::Url;

/// Bytes escaped in a canonical path segment: controls plus everything RFC
/// 3986 does not allow raw in a segment. `/` is included because a `%2F`
/// inside a segment must stay a literal slash, not become a separator.
const SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'/')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

/// The canonical server-path form of an href as a multistatus reports it:
/// resolved against `base` (so relative and absolute hrefs both work), scheme
/// and host dropped, percent-encoding normalized by `normalize_path`.
pub(crate) fn canonical_path(raw: &str, base: &Url) -> Result<String, AddressBookError> {
    let url = base
        .join(raw.trim())
        .map_err(|_| AddressBookError::Permanent("malformed multistatus: unparseable href".into()))?;
    Ok(normalize_path(url.path()))
}

/// Decodes then re-encodes each segment with one fixed set (upper-case hex),
/// so `%7e`, `~` and `%7E` compare equal. A segment that does not decode to
/// UTF-8 is kept as it is.
pub(crate) fn normalize_path(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            let bytes: Vec<u8> = percent_decode_str(segment).collect();
            match std::str::from_utf8(&bytes) {
                Ok(decoded) => utf8_percent_encode(decoded, SEGMENT).to_string(),
                Err(_) => segment.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://p42-contacts.icloud.com/123/carddavhome/card/").unwrap()
    }

    #[test]
    fn absolute_href_loses_scheme_and_host() {
        assert_eq!(
            canonical_path("https://p42-contacts.icloud.com:443/123/carddavhome/card/a.vcf", &base()).unwrap(),
            "/123/carddavhome/card/a.vcf"
        );
    }

    #[test]
    fn relative_href_resolves_against_base() {
        assert_eq!(canonical_path("a.vcf", &base()).unwrap(), "/123/carddavhome/card/a.vcf");
        assert_eq!(canonical_path(" /x/b.vcf ", &base()).unwrap(), "/x/b.vcf");
    }

    #[test]
    fn percent_encoding_is_normalized() {
        assert_eq!(normalize_path("/card/a%7eb.vcf"), "/card/a~b.vcf");
        assert_eq!(normalize_path("/card/a%2fb.vcf"), "/card/a%2Fb.vcf");
        assert_eq!(normalize_path("/card/a%20b.vcf"), "/card/a%20b.vcf");
        assert_eq!(normalize_path("/card/a b.vcf"), "/card/a%20b.vcf");
        assert_eq!(normalize_path("/card/%C3%A9.vcf"), "/card/%C3%A9.vcf");
        assert_eq!(normalize_path("/card/%c3%a9.vcf"), "/card/%C3%A9.vcf");
    }

    #[test]
    fn plain_paths_are_unchanged() {
        let fastmail = "/dav/addressbooks/user/jane@fastmail.com/Default/";
        assert_eq!(normalize_path(fastmail), fastmail);
        assert_eq!(normalize_path("/123/carddavhome/card/0A1B-2C3D.vcf"), "/123/carddavhome/card/0A1B-2C3D.vcf");
    }

    #[test]
    fn invalid_utf8_segment_is_kept() {
        assert_eq!(normalize_path("/card/%FF.vcf"), "/card/%FF.vcf");
    }
}
