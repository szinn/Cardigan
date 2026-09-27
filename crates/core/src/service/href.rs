//! Resource names for cards the daemon creates.

use std::fmt::Write as _;

use sha2::{Digest, Sha256};

use crate::contact::{Href, Uid};

/// The path of an absolute collection URL: everything from the first `/`
/// after the host. A value that is already a path is returned as is.
pub(super) fn collection_path(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.find('/').map_or("/", |at| &rest[at..])
}

/// Where the daemon creates `uid`'s card in the collection at
/// `addressbook_url`: the collection path, 32 hex digits of the UID's
/// SHA-256, and `.vcf`. The same UID always gets the same href, so a create
/// repeated after a crash hits `If-None-Match` instead of making a second
/// card; and any UID, including one with characters unsafe in a URL, gives
/// a safe name.
pub(super) fn mint_href(addressbook_url: &str, uid: &Uid) -> Href {
    let path = collection_path(addressbook_url);
    let separator = if path.ends_with('/') { "" } else { "/" };
    let mut name = String::with_capacity(32);
    for byte in Sha256::digest(uid.as_str().as_bytes()).iter().take(16) {
        write!(name, "{byte:02x}").expect("write to String is infallible");
    }
    Href::from(format!("{path}{separator}{name}.vcf"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_path_drops_scheme_and_host() {
        assert_eq!(
            collection_path("https://p42-contacts.icloud.com/123/carddavhome/card/"),
            "/123/carddavhome/card/"
        );
        assert_eq!(collection_path("https://carddav.fastmail.com"), "/");
        assert_eq!(collection_path("/already/a/path/"), "/already/a/path/");
    }

    #[test]
    fn minted_hrefs_are_stable_safe_and_per_uid() {
        let odd = Uid::from("a b/c?d");
        let href = mint_href("https://h.test/dav/", &odd);

        assert_eq!(href, mint_href("https://h.test/dav/", &odd), "stable");
        assert_ne!(href, mint_href("https://h.test/dav/", &Uid::from("other")));
        let name = href
            .as_str()
            .strip_prefix("/dav/")
            .and_then(|n| n.strip_suffix(".vcf"))
            .expect("under the collection");
        assert_eq!(name.len(), 32);
        assert!(name.chars().all(|c| c.is_ascii_hexdigit()), "{name}");
        assert!(mint_href("https://h.test/dav", &odd).as_str().starts_with("/dav/"), "adds the missing slash");
    }
}
