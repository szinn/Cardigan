use std::collections::BTreeMap;

use cg_core::{
    AddressBookError,
    addressbook::{ChangeSet, Changes, SyncToken},
    contact::{ETag, Href},
};

use crate::{
    client::{DavRequest, DavResponse, HttpClient, propfind, report},
    discovery::Bound,
    href::canonical_path,
    xml::{multistatus, request},
};

/// Pages followed for one truncated `sync-collection` before giving up.
pub(crate) const MAX_SYNC_PAGES: usize = 100;

enum Change {
    Changed(ETag),
    Removed,
}

/// RFC 6578 `sync-collection` (Depth 0). Follows truncation (507 on the
/// collection plus an intermediate token) until the delta is complete; the
/// latest observation per href wins. A rejected token is
/// `Changes::TokenInvalid`.
pub(crate) async fn changes_since(http: &HttpClient, bound: &Bound, token: Option<&SyncToken>) -> Result<Changes, AddressBookError> {
    let context = format!("REPORT {}", bound.collection_url.path());
    let mut merged: BTreeMap<Href, Change> = BTreeMap::new();
    let mut sent = token.map(|t| t.as_str().to_owned());
    for _ in 0..MAX_SYNC_PAGES {
        let request = DavRequest::new(report(), bound.collection_url.clone())
            .depth("0")
            .xml(request::sync_collection(sent.as_deref()));
        let response = http.send(request).await?;
        if !response.is(207) {
            if sent.is_some() && is_token_rejection(&response) {
                return Ok(Changes::TokenInvalid);
            }
            return Err(response.error(&context, None));
        }
        let page = multistatus::parse(&response.body)?;
        let next = page
            .sync_token
            .ok_or_else(|| AddressBookError::Permanent(format!("{context}: response without sync-token")))?;
        let mut truncated = false;
        let mut skipped_without_etag = 0usize;
        for entry in page.responses {
            let path = canonical_path(&entry.href, &bound.collection_url)?;
            if bound.is_collection(&path) {
                truncated |= entry.status == Some(507);
                continue;
            }
            if path.ends_with('/') {
                continue;
            }
            if entry.status == Some(404) {
                merged.insert(Href::from(path), Change::Removed);
            } else if let Some(etag) = entry.props.etag {
                merged.insert(Href::from(path), Change::Changed(ETag::from(etag)));
            } else {
                skipped_without_etag += 1;
                tracing::debug!(path = %path, "sync-collection member without an ETag skipped");
            }
        }
        if skipped_without_etag > 0 {
            tracing::warn!(count = skipped_without_etag, "sync-collection page had members without an ETag; skipped");
        }
        if !truncated {
            return Ok(Changes::Delta(into_change_set(merged, SyncToken::from(next))));
        }
        sent = Some(next);
    }
    Err(AddressBookError::Permanent(format!(
        "{context}: sync-collection incomplete after {MAX_SYNC_PAGES} pages"
    )))
}

/// Every member resource with its ETag (PROPFIND Depth 1).
pub(crate) async fn list_etags(http: &HttpClient, bound: &Bound) -> Result<Vec<(Href, ETag)>, AddressBookError> {
    let context = format!("PROPFIND {}", bound.collection_url.path());
    let request = DavRequest::new(propfind(), bound.collection_url.clone())
        .depth("1")
        .xml(request::propfind_getetag());
    let response = http.send(request).await?;
    if !response.is(207) {
        return Err(response.error(&context, None));
    }
    let mut etags = Vec::new();
    for entry in multistatus::parse(&response.body)?.responses {
        let path = canonical_path(&entry.href, &bound.collection_url)?;
        if bound.is_collection(&path) || path.ends_with('/') {
            continue;
        }
        if let Some(etag) = entry.props.etag {
            etags.push((Href::from(path), ETag::from(etag)));
        }
    }
    Ok(etags)
}

/// RFC 6578 §3.2: 400/403/409 with the `DAV:valid-sync-token` precondition.
fn is_token_rejection(response: &DavResponse) -> bool {
    matches!(response.status.as_u16(), 400 | 403 | 409) && multistatus::is_valid_sync_token_error(&response.body)
}

fn into_change_set(merged: BTreeMap<Href, Change>, token: SyncToken) -> ChangeSet {
    let mut changed = Vec::new();
    let mut removed = Vec::new();
    for (href, change) in merged {
        match change {
            Change::Changed(etag) => changed.push((href, etag)),
            Change::Removed => removed.push(href),
        }
    }
    ChangeSet { changed, removed, token }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use cg_core::{Error, addressbook::AddressBook};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_string_contains, header, method, path},
    };

    use super::*;
    use crate::{
        config::ProviderQuirks,
        test_util::{discovered, discovered_with, multistatus},
    };

    fn changed(href: &str, etag: &str) -> String {
        format!(
            "<d:response><d:href>{href}</d:href><d:propstat><d:prop><d:getetag>{etag}</d:getetag></d:prop><d:status>HTTP/1.1 200 \
             OK</d:status></d:propstat></d:response>"
        )
    }

    fn removed(href: &str) -> String {
        format!("<d:response><d:href>{href}</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>")
    }

    fn truncated() -> String {
        "<d:response><d:href>/home/card/</d:href><d:status>HTTP/1.1 507 Insufficient Storage</d:status></d:response>".to_owned()
    }

    fn token(value: &str) -> String {
        format!("<d:sync-token>{value}</d:sync-token>")
    }

    fn sync_report(sent_token: &str) -> wiremock::MockBuilder {
        Mock::given(method("REPORT"))
            .and(path("/home/card/"))
            .and(header("depth", "0"))
            .and(body_string_contains(format!("<d:sync-token>{sent_token}</d:sync-token>")))
    }

    fn delta(changes: Changes) -> ChangeSet {
        match changes {
            Changes::Delta(set) => set,
            Changes::TokenInvalid => panic!("expected a delta, got TokenInvalid"),
        }
    }

    #[tokio::test]
    async fn initial_sync_returns_full_membership_and_token() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        sync_report("")
            .respond_with(ResponseTemplate::new(207).set_body_string(multistatus(&format!(
                "{}{}{}",
                changed("/home/card/b.vcf", "\"eb\""),
                changed("/home/card/a.vcf", "\"ea\""),
                token("t1")
            ))))
            .expect(1)
            .mount(&server)
            .await;

        let set = delta(adapter.changes_since(None).await.unwrap());

        assert_eq!(
            set.changed,
            [
                (Href::from("/home/card/a.vcf"), ETag::from("\"ea\"")),
                (Href::from("/home/card/b.vcf"), ETag::from("\"eb\""))
            ]
        );
        assert_eq!(set.removed, Vec::<Href>::new());
        assert_eq!(set.token, SyncToken::from("t1"));
    }

    #[tokio::test]
    async fn delta_reports_changed_and_removed() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        sync_report("t1")
            .respond_with(ResponseTemplate::new(207).set_body_string(multistatus(&format!(
                "{}{}{}",
                changed("/home/card/a.vcf", "\"ea2\""),
                removed("/home/card/gone.vcf"),
                token("t2")
            ))))
            .mount(&server)
            .await;

        let set = delta(adapter.changes_since(Some(&SyncToken::from("t1"))).await.unwrap());

        assert_eq!(set.changed, [(Href::from("/home/card/a.vcf"), ETag::from("\"ea2\""))]);
        assert_eq!(set.removed, [Href::from("/home/card/gone.vcf")]);
        assert_eq!(set.token, SyncToken::from("t2"));
    }

    #[tokio::test]
    async fn truncated_response_is_continued_and_merged() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        sync_report("t0")
            .respond_with(ResponseTemplate::new(207).set_body_string(multistatus(&format!(
                "{}{}{}{}",
                changed("/home/card/a.vcf", "\"ea\""),
                changed("/home/card/b.vcf", "\"eb\""),
                truncated(),
                token("t1")
            ))))
            .expect(1)
            .mount(&server)
            .await;
        sync_report("t1")
            .respond_with(ResponseTemplate::new(207).set_body_string(multistatus(&format!(
                "{}{}{}",
                removed("/home/card/a.vcf"),
                changed("/home/card/c.vcf", "\"ec\""),
                token("t2")
            ))))
            .expect(1)
            .mount(&server)
            .await;

        let set = delta(adapter.changes_since(Some(&SyncToken::from("t0"))).await.unwrap());

        assert_eq!(
            set.changed,
            [
                (Href::from("/home/card/b.vcf"), ETag::from("\"eb\"")),
                (Href::from("/home/card/c.vcf"), ETag::from("\"ec\""))
            ]
        );
        assert_eq!(set.removed, [Href::from("/home/card/a.vcf")]);
        assert_eq!(set.token, SyncToken::from("t2"));
    }

    #[tokio::test]
    async fn endless_truncation_is_permanent() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        Mock::given(method("REPORT"))
            .respond_with(ResponseTemplate::new(207).set_body_string(multistatus(&format!("{}{}", truncated(), token("again")))))
            .expect(MAX_SYNC_PAGES as u64)
            .mount(&server)
            .await;

        let error = adapter.changes_since(None).await.unwrap_err();

        match error {
            Error::AddressBook(AddressBookError::Permanent(message)) => assert!(message.contains("incomplete"), "{message}"),
            other => panic!("expected Permanent, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejected_token_is_token_invalid() {
        for status in [403, 409, 400] {
            let server = MockServer::start().await;
            let adapter = discovered(&server).await;
            sync_report("old")
                .respond_with(ResponseTemplate::new(status).set_body_string(r#"<?xml version="1.0"?><D:error xmlns:D="DAV:"><D:valid-sync-token/></D:error>"#))
                .mount(&server)
                .await;

            let changes = adapter.changes_since(Some(&SyncToken::from("old"))).await.unwrap();

            assert_eq!(changes, Changes::TokenInvalid, "status {status}");
        }
    }

    #[tokio::test]
    async fn forbidden_without_valid_sync_token_is_permanent() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        sync_report("t1").respond_with(ResponseTemplate::new(403)).mount(&server).await;

        let error = adapter.changes_since(Some(&SyncToken::from("t1"))).await.unwrap_err();

        match error {
            Error::AddressBook(inner) => assert_eq!(inner, AddressBookError::Permanent("REPORT /home/card/: HTTP 403".into())),
            other => panic!("expected AddressBook error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rate_limited_report() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        sync_report("")
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "30"))
            .mount(&server)
            .await;

        let error = adapter.changes_since(None).await.unwrap_err();

        match error {
            Error::AddressBook(inner) => assert_eq!(
                inner,
                AddressBookError::RateLimited {
                    retry_after: Some(Duration::from_secs(30))
                }
            ),
            other => panic!("expected AddressBook error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn response_without_sync_token_is_permanent() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        sync_report("")
            .respond_with(ResponseTemplate::new(207).set_body_string(multistatus(&changed("/home/card/a.vcf", "\"e\""))))
            .mount(&server)
            .await;

        let error = adapter.changes_since(None).await.unwrap_err();

        assert!(matches!(error, Error::AddressBook(AddressBookError::Permanent(_))), "{error:?}");
    }

    #[tokio::test]
    async fn hrefs_are_canonical_and_subcollections_skipped() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        sync_report("")
            .respond_with(ResponseTemplate::new(207).set_body_string(multistatus(&format!(
                "{}{}{}",
                changed(&format!("{}/home/card/a%7eb.vcf", server.uri()), "\"e\""),
                changed("/home/card/sub/", "\"s\""),
                token("t1")
            ))))
            .mount(&server)
            .await;

        let set = delta(adapter.changes_since(None).await.unwrap());

        assert_eq!(set.changed, [(Href::from("/home/card/a~b.vcf"), ETag::from("\"e\""))]);
    }

    fn no_etag(href: &str) -> String {
        format!("<d:response><d:href>{href}</d:href><d:propstat><d:prop></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>")
    }

    #[tokio::test]
    async fn member_without_etag_is_skipped_from_changeset() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        sync_report("")
            .respond_with(ResponseTemplate::new(207).set_body_string(multistatus(&format!(
                "{}{}{}",
                no_etag("/home/card/noetag.vcf"),
                changed("/home/card/a.vcf", "\"ea\""),
                token("t1")
            ))))
            .mount(&server)
            .await;

        let set = delta(adapter.changes_since(None).await.unwrap());

        assert_eq!(set.changed, [(Href::from("/home/card/a.vcf"), ETag::from("\"ea\""))]);
        assert!(!set.changed.iter().any(|(href, _)| href.as_str() == "/home/card/noetag.vcf"));
        assert!(!set.removed.iter().any(|href| href.as_str() == "/home/card/noetag.vcf"));
    }

    #[tokio::test]
    async fn list_etags_skips_the_collection_itself() {
        let server = MockServer::start().await;
        let adapter = discovered_with(&server, false, ProviderQuirks::default()).await;
        Mock::given(method("PROPFIND"))
            .and(path("/home/card/"))
            .and(header("depth", "1"))
            .and(body_string_contains("getetag"))
            .respond_with(ResponseTemplate::new(207).set_body_string(multistatus(&format!(
                "{}{}{}",
                changed("/home/card/", "\"collection\""),
                changed("/home/card/a.vcf", "\"ea\""),
                changed("/home/card/b.vcf", "\"eb\"")
            ))))
            .expect(1)
            .mount(&server)
            .await;

        let etags = adapter.list_etags().await.unwrap();

        assert_eq!(
            etags,
            [
                (Href::from("/home/card/a.vcf"), ETag::from("\"ea\"")),
                (Href::from("/home/card/b.vcf"), ETag::from("\"eb\""))
            ]
        );
    }

    #[tokio::test]
    async fn list_etags_maps_errors() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        Mock::given(method("PROPFIND"))
            .and(path("/home/card/"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let error = adapter.list_etags().await.unwrap_err();

        match error {
            Error::AddressBook(inner) => assert_eq!(inner, AddressBookError::RateLimited { retry_after: None }),
            other => panic!("expected AddressBook error, got {other:?}"),
        }
    }
}
