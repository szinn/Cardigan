use cg_core::{
    AddressBookError,
    addressbook::Precondition,
    contact::{ETag, Href},
};
use reqwest::Method;
use url::Url;

use crate::{
    client::{DavRequest, HttpClient, propfind},
    discovery::Bound,
    xml::request,
};

/// PUT `body` verbatim under `precondition`. Returns the `ETag` header
/// exactly as sent (weak ones included), or `None` when there is none: no
/// follow-up request (the caller multigets).
pub(crate) async fn put(http: &HttpClient, bound: &Bound, href: &Href, body: &[u8], precondition: &Precondition) -> Result<Option<ETag>, AddressBookError> {
    let url = bound.resource_url(href)?;
    let context = format!("PUT {}", url.path());
    let request = DavRequest::new(Method::PUT, url).vcard(body);
    let request = match precondition {
        Precondition::IfMatch(etag) => request.header("If-Match", etag.as_str()),
        Precondition::IfNoneMatch => request.header("If-None-Match", "*"),
    };
    let response = http.send(request).await?;
    if !response.status.is_success() {
        return Err(response.error(&context, Some(href)));
    }
    Ok(response.header("etag").filter(|etag| !etag.is_empty()).map(ETag::from))
}

/// DELETE, guarded by `if_match`. Already gone (404/410) is success. On 412
/// the resource's existence is checked: gone is success (a strict server may
/// answer `If-Match` on a missing resource with 412), present is
/// `PreconditionFailed`.
pub(crate) async fn delete(http: &HttpClient, bound: &Bound, href: &Href, if_match: Option<&ETag>) -> Result<(), AddressBookError> {
    let url = bound.resource_url(href)?;
    let context = format!("DELETE {}", url.path());
    let mut request = DavRequest::new(Method::DELETE, url.clone());
    if let Some(etag) = if_match {
        request = request.header("If-Match", etag.as_str());
    }
    let response = http.send(request).await?;
    let status = response.status.as_u16();
    if response.status.is_success() || status == 404 || status == 410 {
        return Ok(());
    }
    if status == 412 && !exists(http, &url).await? {
        return Ok(());
    }
    Err(response.error(&context, Some(href)))
}

/// PROPFIND Depth 0: does the resource still exist?
async fn exists(http: &HttpClient, url: &Url) -> Result<bool, AddressBookError> {
    let context = format!("PROPFIND {}", url.path());
    let request = DavRequest::new(propfind(), url.clone()).depth("0").xml(request::propfind_getetag());
    let response = http.send(request).await?;
    match response.status.as_u16() {
        404 | 410 => Ok(false),
        200..=299 => Ok(true),
        _ => Err(response.error(&context, None)),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use cg_core::{Error, addressbook::AddressBook};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    use super::*;
    use crate::test_util::{discovered, multistatus};

    const CARD: &[u8] = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:zoe\r\nFN:Zoë\r\nEND:VCARD\r\n".as_bytes();

    fn href(name: &str) -> Href {
        Href::from(format!("/home/card/{name}.vcf"))
    }

    fn requests_to<'a>(requests: &'a [wiremock::Request], path: &str) -> Vec<&'a wiremock::Request> {
        requests.iter().filter(|r| r.url.path() == path).collect()
    }

    #[tokio::test]
    async fn create_sends_if_none_match_and_returns_etag() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        Mock::given(method("PUT"))
            .and(path("/home/card/new.vcf"))
            .and(header("if-none-match", "*"))
            .and(header("content-type", "text/vcard; charset=utf-8"))
            .respond_with(ResponseTemplate::new(201).insert_header("ETag", "\"e1\""))
            .expect(1)
            .mount(&server)
            .await;

        let etag = adapter.put(&href("new"), CARD, Precondition::IfNoneMatch).await.unwrap();

        assert_eq!(etag, Some(ETag::from("\"e1\"")));
    }

    #[tokio::test]
    async fn update_sends_if_match() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        Mock::given(method("PUT"))
            .and(path("/home/card/a.vcf"))
            .and(header("if-match", "\"e1\""))
            .respond_with(ResponseTemplate::new(204).insert_header("ETag", "\"e2\""))
            .expect(1)
            .mount(&server)
            .await;

        let etag = adapter.put(&href("a"), CARD, Precondition::IfMatch(ETag::from("\"e1\""))).await.unwrap();

        assert_eq!(etag, Some(ETag::from("\"e2\"")));
    }

    #[tokio::test]
    async fn put_sends_body_verbatim() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(201).insert_header("ETag", "\"e1\""))
            .mount(&server)
            .await;

        adapter.put(&href("zoe"), CARD, Precondition::IfNoneMatch).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        let puts = requests_to(&requests, "/home/card/zoe.vcf");
        assert_eq!(puts.len(), 1);
        assert_eq!(puts[0].body, CARD);
    }

    #[tokio::test]
    async fn put_without_etag_returns_none_and_makes_no_follow_up() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        Mock::given(method("PUT")).respond_with(ResponseTemplate::new(201)).mount(&server).await;

        let etag = adapter.put(&href("new"), CARD, Precondition::IfNoneMatch).await.unwrap();

        assert_eq!(etag, None);
        let requests = server.received_requests().await.unwrap();
        let to_card = requests_to(&requests, "/home/card/new.vcf");
        assert_eq!(to_card.len(), 1, "expected only the PUT, got {} requests", to_card.len());
        assert_eq!(to_card[0].method.as_str(), "PUT");
        assert!(!requests.iter().any(|r| r.method.as_str() == "REPORT"), "put must not fetch the ETag itself");
    }

    #[tokio::test]
    async fn weak_etag_is_returned_verbatim() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(204).insert_header("ETag", "W/\"weak-1\""))
            .mount(&server)
            .await;

        let etag = adapter.put(&href("a"), CARD, Precondition::IfMatch(ETag::from("\"e1\""))).await.unwrap();

        assert_eq!(etag, Some(ETag::from("W/\"weak-1\"")));
    }

    #[tokio::test]
    async fn stale_etag_is_precondition_failed() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        Mock::given(method("PUT")).respond_with(ResponseTemplate::new(412)).mount(&server).await;

        let error = adapter.put(&href("a"), CARD, Precondition::IfMatch(ETag::from("\"old\""))).await.unwrap_err();

        match error {
            Error::AddressBook(inner) => assert_eq!(inner, AddressBookError::PreconditionFailed { href: href("a") }),
            other => panic!("expected AddressBook error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn put_rate_limited() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "10"))
            .mount(&server)
            .await;

        let error = adapter.put(&href("a"), CARD, Precondition::IfNoneMatch).await.unwrap_err();

        match error {
            Error::AddressBook(inner) => assert_eq!(
                inner,
                AddressBookError::RateLimited {
                    retry_after: Some(Duration::from_secs(10))
                }
            ),
            other => panic!("expected AddressBook error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_with_if_match() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        Mock::given(method("DELETE"))
            .and(path("/home/card/a.vcf"))
            .and(header("if-match", "\"e1\""))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        adapter.delete(&href("a"), Some(&ETag::from("\"e1\""))).await.unwrap();
    }

    #[tokio::test]
    async fn delete_of_gone_resource_is_ok() {
        for status in [404, 410] {
            let server = MockServer::start().await;
            let adapter = discovered(&server).await;
            Mock::given(method("DELETE")).respond_with(ResponseTemplate::new(status)).mount(&server).await;

            adapter.delete(&href("a"), None).await.unwrap();
        }
    }

    #[tokio::test]
    async fn delete_412_on_gone_resource_is_ok() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        Mock::given(method("DELETE"))
            .and(path("/home/card/a.vcf"))
            .respond_with(ResponseTemplate::new(412))
            .mount(&server)
            .await;
        Mock::given(method("PROPFIND"))
            .and(path("/home/card/a.vcf"))
            .and(header("depth", "0"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;

        adapter.delete(&href("a"), Some(&ETag::from("\"e1\""))).await.unwrap();
    }

    #[tokio::test]
    async fn delete_412_on_live_resource_is_precondition_failed() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        Mock::given(method("DELETE")).respond_with(ResponseTemplate::new(412)).mount(&server).await;
        Mock::given(method("PROPFIND"))
            .and(path("/home/card/a.vcf"))
            .respond_with(ResponseTemplate::new(207).set_body_string(multistatus(
                "<d:response><d:href>/home/card/a.vcf</d:href><d:propstat><d:prop><d:getetag>\"e2\"</d:getetag></d:prop><d:status>HTTP/1.1 200 \
                 OK</d:status></d:propstat></d:response>",
            )))
            .mount(&server)
            .await;

        let error = adapter.delete(&href("a"), Some(&ETag::from("\"e1\""))).await.unwrap_err();

        match error {
            Error::AddressBook(inner) => assert_eq!(inner, AddressBookError::PreconditionFailed { href: href("a") }),
            other => panic!("expected AddressBook error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_unauthorized() {
        let server = MockServer::start().await;
        let adapter = discovered(&server).await;
        Mock::given(method("DELETE")).respond_with(ResponseTemplate::new(401)).mount(&server).await;

        let error = adapter.delete(&href("a"), None).await.unwrap_err();

        match error {
            Error::AddressBook(inner) => assert_eq!(inner, AddressBookError::Unauthorized),
            other => panic!("expected AddressBook error, got {other:?}"),
        }
    }
}
