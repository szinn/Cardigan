use std::collections::HashSet;

use cg_core::{
    AddressBookError,
    addressbook::{FetchedCard, MultigetResult},
    contact::{ETag, Href},
};

use crate::{
    client::{DavRequest, HttpClient, report},
    discovery::Bound,
    href::{canonical_path, normalize_path},
    xml::{
        multistatus::{self, Entry},
        request,
    },
};

/// `addressbook-multiget` in batches of `batch` hrefs (no `Depth` header,
/// RFC 6352 §8.7). 404/410 hrefs, and requested hrefs absent from the
/// response, go in `missing`; any other per-href error fails the whole call.
pub(crate) async fn multiget(http: &HttpClient, bound: &Bound, batch: usize, hrefs: &[Href]) -> Result<MultigetResult, AddressBookError> {
    let context = format!("REPORT {}", bound.collection_url.path());
    let mut result = MultigetResult::default();
    for chunk in hrefs.chunks(batch.max(1)) {
        let requested: Vec<String> = chunk.iter().map(|href| normalize_path(href.as_str())).collect();
        let requested_set: HashSet<&str> = requested.iter().map(String::as_str).collect();
        let request = DavRequest::new(report(), bound.collection_url.clone()).xml(request::addressbook_multiget(&requested));
        let response = http.send(request).await?;
        if !response.is(207) {
            return Err(response.error(&context, None));
        }
        let mut answered = HashSet::new();
        for entry in multistatus::parse(&response.body)?.responses {
            let path = canonical_path(&entry.href, &bound.collection_url)?;
            // A stray response (e.g. the collection itself, or a card not in
            // this batch) is not classified: it is neither found nor missing.
            if !requested_set.contains(path.as_str()) {
                continue;
            }
            answered.insert(path.clone());
            match classify(&context, entry)? {
                Some((etag, body)) => result.found.push(FetchedCard {
                    href: Href::from(path),
                    etag,
                    body,
                }),
                None => result.missing.push(Href::from(path)),
            }
        }
        result
            .missing
            .extend(requested.into_iter().filter(|path| !answered.contains(path)).map(Href::from));
    }
    Ok(result)
}

/// `Some` for a fetched card, `None` for a gone one, `Err` for any other
/// per-href failure.
fn classify(context: &str, entry: Entry) -> Result<Option<(ETag, Vec<u8>)>, AddressBookError> {
    match entry.status {
        Some(404 | 410) => return Ok(None),
        Some(code) if !(200..300).contains(&code) => return Err(per_href_error(context, code)),
        _ => {}
    }
    match (entry.props.etag, entry.props.address_data) {
        (Some(etag), Some(data)) => Ok(Some((ETag::from(etag), multistatus::restore_crlf(&data)))),
        _ => match entry.propstat_error {
            Some(404 | 410) => Ok(None),
            Some(code) => Err(per_href_error(context, code)),
            None => Err(AddressBookError::Permanent(format!("{context}: card without getetag or address-data"))),
        },
    }
}

/// Decision 9: 5xx is transient, anything else permanent.
fn per_href_error(context: &str, code: u16) -> AddressBookError {
    if (500..600).contains(&code) {
        AddressBookError::Transient(format!("{context}: HTTP {code} for one card"))
    } else {
        AddressBookError::Permanent(format!("{context}: HTTP {code} for one card"))
    }
}

#[cfg(test)]
mod tests {
    use cg_core::{Error, addressbook::AddressBook};
    use wiremock::{
        Mock, MockServer, Request, Respond, ResponseTemplate,
        matchers::{method, path},
    };

    use super::*;
    use crate::{
        config::ProviderQuirks,
        test_util::{discovered, discovered_with, multistatus, plain_collection_entry},
        xml::request::escape,
    };

    const CARD: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:zoe\r\nFN:Zoë Ñandú\r\nitem1.X-ABLABEL:_$!<Home>!$_\r\nEND:VCARD\r\n";

    fn card_entry(href: &str, etag: &str, vcard: &str) -> String {
        format!(
            "<d:response><d:href>{href}</d:href><d:propstat><d:prop><d:getetag>{etag}</d:getetag><card:address-data>{}</card:address-data></d:prop><d:\
             status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>",
            escape(vcard)
        )
    }

    fn status_entry(href: &str, status: &str) -> String {
        format!("<d:response><d:href>{href}</d:href><d:status>HTTP/1.1 {status}</d:status></d:response>")
    }

    fn multiget_report() -> wiremock::MockBuilder {
        Mock::given(method("REPORT")).and(path("/home/card/"))
    }

    fn hrefs(names: &[&str]) -> Vec<Href> {
        names.iter().map(|n| Href::from(format!("/home/card/{n}.vcf"))).collect()
    }

    /// Answers every multiget with one card per requested href.
    struct EchoCards;

    impl Respond for EchoCards {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let body = String::from_utf8_lossy(&request.body);
            let entries: String = body
                .split("<d:href>")
                .skip(1)
                .filter_map(|rest| rest.split("</d:href>").next())
                .map(|href| card_entry(href, "\"e\"", "BEGIN:VCARD\r\nEND:VCARD\r\n"))
                .collect();
            ResponseTemplate::new(207).set_body_string(multistatus(&entries))
        }
    }

    #[tokio::test]
    async fn fetches_card_verbatim_with_crlf_and_non_ascii() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        // `escape` leaves CR alone, so the XML carries raw CR LF: the parser
        // turns it into LF and the adapter must restore it.
        multiget_report()
            .respond_with(ResponseTemplate::new(207).set_body_string(multistatus(&card_entry("/home/card/zoe.vcf", "\"e1\"", CARD))))
            .mount(&server)
            .await;

        let result = adapter.multiget(&hrefs(&["zoe"])).await.unwrap();

        assert_eq!(
            result.found,
            [FetchedCard {
                href: Href::from("/home/card/zoe.vcf"),
                etag: ETag::from("\"e1\""),
                body: CARD.as_bytes().to_vec(),
            }]
        );
        assert_eq!(result.missing, Vec::<Href>::new());
    }

    #[tokio::test]
    async fn batches_requests() {
        let server = MockServer::start().await;
        let quirks = ProviderQuirks {
            multiget_batch: 2,
            ..ProviderQuirks::default()
        };
        let adapter = discovered_with(&server, true, quirks).await;
        multiget_report().respond_with(EchoCards).expect(3).mount(&server).await;

        let result = adapter.multiget(&hrefs(&["a", "b", "c", "d", "e"])).await.unwrap();

        assert_eq!(result.found.len(), 5);
        assert_eq!(result.missing, Vec::<Href>::new());
        let reports: Vec<_> = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.method.as_str() == "REPORT")
            .collect();
        assert_eq!(reports.len(), 3);
        assert!(reports.iter().all(|r| !r.headers.contains_key("depth")));
    }

    #[tokio::test]
    async fn empty_input_sends_nothing() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        multiget_report().respond_with(EchoCards).expect(0).mount(&server).await;

        let result = adapter.multiget(&[]).await.unwrap();

        assert_eq!(result, MultigetResult::default());
    }

    #[tokio::test]
    async fn gone_and_unreported_hrefs_are_missing() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        multiget_report()
            .respond_with(ResponseTemplate::new(207).set_body_string(multistatus(&format!(
                "{}{}{}",
                card_entry("/home/card/a.vcf", "\"ea\"", CARD),
                status_entry("/home/card/gone.vcf", "404 Not Found"),
                "<d:response><d:href>/home/card/half.vcf</d:href><d:propstat><d:prop><card:address-data/></d:prop><d:status>HTTP/1.1 404 Not \
                 Found</d:status></d:propstat></d:response>"
            ))))
            .mount(&server)
            .await;

        let result = adapter.multiget(&hrefs(&["a", "gone", "half", "unreported"])).await.unwrap();

        assert_eq!(result.found.len(), 1);
        assert_eq!(result.missing, hrefs(&["gone", "half", "unreported"]));
    }

    #[tokio::test]
    async fn absolute_and_differently_encoded_hrefs_match_request() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        multiget_report()
            .respond_with(ResponseTemplate::new(207).set_body_string(multistatus(&card_entry(
                &format!("{}/home/card/a%7eb%c3%a9.vcf", server.uri()),
                "\"e\"",
                CARD,
            ))))
            .mount(&server)
            .await;

        let result = adapter.multiget(&[Href::from("/home/card/a~b%C3%A9.vcf")]).await.unwrap();

        assert!(result.missing.is_empty(), "matched href reported missing: {:?}", result.missing);
        assert_eq!(result.found.len(), 1);
        assert_eq!(result.found[0].href, Href::from("/home/card/a~b%C3%A9.vcf"));
    }

    #[tokio::test]
    async fn stray_responses_outside_the_requested_batch_are_ignored() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        multiget_report()
            .respond_with(ResponseTemplate::new(207).set_body_string(multistatus(&format!(
                "{}{}{}",
                plain_collection_entry("/home/card/"),
                card_entry("/home/card/a.vcf", "\"ea\"", CARD),
                card_entry("/home/card/unrequested.vcf", "\"eu\"", CARD)
            ))))
            .mount(&server)
            .await;

        let result = adapter.multiget(&hrefs(&["a"])).await.unwrap();

        assert_eq!(
            result.found,
            [FetchedCard {
                href: Href::from("/home/card/a.vcf"),
                etag: ETag::from("\"ea\""),
                body: CARD.as_bytes().to_vec(),
            }]
        );
        assert_eq!(result.missing, Vec::<Href>::new());
    }

    #[tokio::test]
    async fn per_href_server_error_fails_whole_call_as_transient() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        multiget_report()
            .respond_with(ResponseTemplate::new(207).set_body_string(multistatus(&format!(
                "{}{}",
                card_entry("/home/card/a.vcf", "\"ea\"", CARD),
                status_entry("/home/card/b.vcf", "500 Internal Server Error")
            ))))
            .mount(&server)
            .await;

        let error = adapter.multiget(&hrefs(&["a", "b"])).await.unwrap_err();

        assert!(matches!(error, Error::AddressBook(AddressBookError::Transient(_))), "{error:?}");
    }

    #[tokio::test]
    async fn per_href_forbidden_fails_whole_call_as_permanent() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        multiget_report()
            .respond_with(ResponseTemplate::new(207).set_body_string(multistatus(&status_entry("/home/card/a.vcf", "403 Forbidden"))))
            .mount(&server)
            .await;

        let error = adapter.multiget(&hrefs(&["a"])).await.unwrap_err();

        assert!(matches!(error, Error::AddressBook(AddressBookError::Permanent(_))), "{error:?}");
    }

    #[tokio::test]
    async fn hrefs_are_xml_escaped_in_the_request() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        multiget_report().respond_with(EchoCards).mount(&server).await;

        adapter.multiget(&[Href::from("/home/card/a&b.vcf")]).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        let report = requests.iter().find(|r| r.method.as_str() == "REPORT").unwrap();
        let body = String::from_utf8_lossy(&report.body);
        assert!(body.contains("<d:href>/home/card/a&amp;b.vcf</d:href>"), "{body}");
    }

    #[tokio::test]
    async fn rate_limited_multiget() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        multiget_report()
            .respond_with(ResponseTemplate::new(503).insert_header("Retry-After", "5"))
            .mount(&server)
            .await;

        let error = adapter.multiget(&hrefs(&["a"])).await.unwrap_err();

        match error {
            Error::AddressBook(inner) => assert_eq!(
                inner,
                AddressBookError::RateLimited {
                    retry_after: Some(std::time::Duration::from_secs(5))
                }
            ),
            other => panic!("expected AddressBook error, got {other:?}"),
        }
    }
}
