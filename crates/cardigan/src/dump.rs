//! `cardigan dump`: every card of one side's address book as one JSON
//! document. Card bodies are full contact data (PII): they go to the output
//! only, never to tracing or error messages.

use std::io::{self, Write};

use anyhow::{Context, anyhow, bail};
use cg_core::{
    addressbook::{AddressBook, Changes, FetchedCard},
    contact::{Href, Side},
};
use serde::Serialize;
use url::Url;

/// Everything `dump` prints.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Dump {
    pub target: &'static str,
    pub host: String,
    pub collection: String,
    pub listed_with: ListedWith,
    pub cards: Vec<DumpedCard>,
}

/// How the membership was listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ListedWith {
    SyncCollection,
    Propfind,
}

/// One card as the `AddressBook` port returned it.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct DumpedCard {
    pub href: String,
    pub etag: String,
    pub vcard: String,
}

/// Discovers `book`, lists its members (sync-collection when supported,
/// PROPFIND otherwise) and fetches them all. Cards deleted between listing
/// and fetching are left out.
pub async fn collect(side: Side, book: &dyn AddressBook) -> anyhow::Result<Dump> {
    let collection = book.discover().await.context("Discovery failed")?;

    let (listed_with, hrefs): (ListedWith, Vec<Href>) = if collection.supports_sync_collection {
        match book.changes_since(None).await.context("Listing cards with sync-collection failed")? {
            Changes::Delta(set) => (ListedWith::SyncCollection, set.changed.into_iter().map(|(href, _)| href).collect()),
            Changes::TokenInvalid => bail!("Listing cards with sync-collection failed: the server rejected a request without a token"),
        }
    } else {
        let listed = book.list_etags().await.context("Listing cards with PROPFIND failed")?;
        (ListedWith::Propfind, listed.into_iter().map(|(href, _)| href).collect())
    };

    let fetched = book.multiget(&hrefs).await.context("Fetching cards failed")?;
    let mut cards = fetched.found.into_iter().map(dumped_card).collect::<anyhow::Result<Vec<_>>>()?;
    cards.sort_by(|a, b| a.href.cmp(&b.href));

    Ok(Dump {
        target: side.as_str(),
        host: collection.discovered_host,
        collection: last_segment(&collection.addressbook_url),
        listed_with,
        cards,
    })
}

fn dumped_card(card: FetchedCard) -> anyhow::Result<DumpedCard> {
    let FetchedCard { href, etag, body } = card;
    let vcard = String::from_utf8(body).map_err(|_| anyhow!("Card {href} is not valid UTF-8"))?;
    Ok(DumpedCard {
        href: href.into_string(),
        etag: etag.into_string(),
        vcard,
    })
}

fn last_segment(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|url| url.path_segments()?.rfind(|segment| !segment.is_empty()).map(str::to_owned))
        .unwrap_or_default()
}

/// Writes `dump` as pretty-printed JSON followed by a newline.
pub fn write_json(dump: &Dump, mut out: impl Write) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut out, dump)?;
    out.write_all(b"\n")?;
    out.flush()
}

/// Treats a closed output pipe (`cardigan dump icloud | head`) as success.
pub fn ignore_broken_pipe(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use cg_core::{
        AddressBookError,
        addressbook::{ChangeSet, Collection, MockAddressBook, MultigetResult, SyncToken},
        contact::ETag,
        test_support::{InMemoryAddressBook, Op},
    };
    use serde_json::json;

    use super::*;

    const URL: &str = "https://p42-contacts.icloud.com/1234/carddavhome/card/";
    const HOST: &str = "p42-contacts.icloud.com";
    const JANE: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Jane Example\r\nEND:VCARD\r\n";
    const ZOE: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Zoë Ñandú\r\nEND:VCARD\r\n";

    fn collection(supports_sync_collection: bool) -> Collection {
        Collection {
            addressbook_url: URL.to_owned(),
            discovered_host: HOST.to_owned(),
            supports_sync_collection,
        }
    }

    fn card(href: &str, etag: &ETag, vcard: &str) -> DumpedCard {
        DumpedCard {
            href: href.to_owned(),
            etag: etag.as_str().to_owned(),
            vcard: vcard.to_owned(),
        }
    }

    #[tokio::test]
    async fn lists_with_sync_collection_and_sorts_by_href() {
        let book = InMemoryAddressBook::new(collection(true));
        let zoe = book.external_put("/1234/carddavhome/card/z.vcf", ZOE);
        let jane = book.external_put("/1234/carddavhome/card/a.vcf", JANE);

        let dump = collect(Side::ICloud, &book).await.unwrap();

        assert_eq!(
            dump,
            Dump {
                target: "icloud",
                host: HOST.to_owned(),
                collection: "card".to_owned(),
                listed_with: ListedWith::SyncCollection,
                cards: vec![
                    card("/1234/carddavhome/card/a.vcf", &jane, JANE),
                    card("/1234/carddavhome/card/z.vcf", &zoe, ZOE)
                ],
            }
        );
    }

    #[tokio::test]
    async fn lists_with_propfind_when_sync_collection_is_unsupported() {
        let book = InMemoryAddressBook::new(collection(false));
        let jane = book.external_put("/1234/carddavhome/card/a.vcf", JANE);

        let dump = collect(Side::Fastmail, &book).await.unwrap();

        assert_eq!(dump.target, "fastmail");
        assert_eq!(dump.listed_with, ListedWith::Propfind);
        assert_eq!(dump.cards, vec![card("/1234/carddavhome/card/a.vcf", &jane, JANE)]);
    }

    #[tokio::test]
    async fn empty_address_book_has_no_cards() {
        let book = InMemoryAddressBook::new(collection(true));

        let dump = collect(Side::ICloud, &book).await.unwrap();

        assert_eq!(dump.cards, Vec::<DumpedCard>::new());
    }

    #[tokio::test]
    async fn card_deleted_between_listing_and_fetching_is_left_out() {
        let mut book = MockAddressBook::new();
        book.expect_discover().returning(|| Box::pin(async { Ok(collection(true)) }));
        book.expect_changes_since().returning(|_| {
            Box::pin(async {
                Ok(Changes::Delta(ChangeSet {
                    changed: vec![(Href::from("/c/a.vcf"), ETag::from("\"1\"")), (Href::from("/c/gone.vcf"), ETag::from("\"2\""))],
                    removed: Vec::new(),
                    token: SyncToken::from("t1"),
                }))
            })
        });
        book.expect_multiget()
            .withf(|hrefs| hrefs == vec![Href::from("/c/a.vcf"), Href::from("/c/gone.vcf")])
            .times(1)
            .returning(|_| {
                Box::pin(async {
                    Ok(MultigetResult {
                        found: vec![FetchedCard {
                            href: Href::from("/c/a.vcf"),
                            etag: ETag::from("\"1\""),
                            body: JANE.as_bytes().to_vec(),
                        }],
                        missing: vec![Href::from("/c/gone.vcf")],
                    })
                })
            });

        let dump = collect(Side::ICloud, &book).await.unwrap();

        assert_eq!(dump.cards, vec![card("/c/a.vcf", &ETag::from("\"1\""), JANE)]);
    }

    #[tokio::test]
    async fn non_utf8_card_is_an_error_naming_its_href() {
        let book = InMemoryAddressBook::new(collection(true));
        book.external_put("/1234/carddavhome/card/bad.vcf", b"BEGIN:VCARD\r\nFN:\xff\r\nEND:VCARD\r\n".to_vec());

        let error = collect(Side::ICloud, &book).await.unwrap_err();

        let message = format!("{error:#}");
        assert!(message.contains("/1234/carddavhome/card/bad.vcf"), "{message}");
        assert!(!message.contains("BEGIN:VCARD"), "card content leaked: {message}");
    }

    #[tokio::test]
    async fn discovery_failure_is_reported() {
        let book = InMemoryAddressBook::new(collection(true));
        book.fail_next(Op::Discover, AddressBookError::Unauthorized);

        let error = collect(Side::ICloud, &book).await.unwrap_err();

        let message = format!("{error:#}");
        assert!(message.starts_with("Discovery failed"), "{message}");
    }

    #[test]
    fn json_has_the_agreed_shape_and_keeps_crlf() {
        let dump = Dump {
            target: "icloud",
            host: HOST.to_owned(),
            collection: "card".to_owned(),
            listed_with: ListedWith::SyncCollection,
            cards: vec![card("/c/a.vcf", &ETag::from("W/\"1\""), ZOE)],
        };
        let mut out = Vec::new();

        write_json(&dump, &mut out).unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(text.ends_with("}\n"), "{text}");
        assert!(text.contains(r"FN:Zoë Ñandú\r\nEND:VCARD\r\n"), "{text}");
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            value,
            json!({
                "target": "icloud",
                "host": HOST,
                "collection": "card",
                "listed_with": "sync-collection",
                "cards": [{ "href": "/c/a.vcf", "etag": "W/\"1\"", "vcard": ZOE }],
            })
        );
    }

    #[test]
    fn propfind_serializes_as_propfind() {
        assert_eq!(serde_json::to_value(ListedWith::Propfind).unwrap(), json!("propfind"));
    }

    #[test]
    fn collection_is_the_last_path_segment() {
        assert_eq!(last_segment("https://h/1234/carddavhome/card/"), "card");
        assert_eq!(last_segment("https://h/dav/addressbooks/user/jane%40fastmail.com/Default"), "Default");
        assert_eq!(last_segment("not a url"), "");
    }

    #[test]
    fn broken_pipe_is_not_an_error() {
        ignore_broken_pipe(Err(io::Error::from(io::ErrorKind::BrokenPipe))).unwrap();
        ignore_broken_pipe(Ok(())).unwrap();
        let error = ignore_broken_pipe(Err(io::Error::from(io::ErrorKind::PermissionDenied))).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);

        // Test with a Write impl that fails with BrokenPipe, proving write_json
        // preserves the kind
        struct BrokenPipeWriter;
        impl Write for BrokenPipeWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "pipe closed"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let dump = Dump {
            target: "icloud",
            host: HOST.to_owned(),
            collection: "card".to_owned(),
            listed_with: ListedWith::SyncCollection,
            cards: vec![],
        };
        let write_result = write_json(&dump, BrokenPipeWriter);
        assert_eq!(write_result.as_ref().unwrap_err().kind(), io::ErrorKind::BrokenPipe);
        ignore_broken_pipe(write_result).unwrap();
    }
}
