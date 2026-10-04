use std::{
    fmt,
    time::{Duration, Instant},
};

use cg_core::{AddressBookError, contact::Href};
use reqwest::{
    Client, Method, StatusCode,
    header::{CONTENT_TYPE, HeaderMap, RETRY_AFTER},
    redirect,
};
use secrecy::{ExposeSecret, SecretString};
use url::Url;

use crate::error::{status_error, transport_error};

const USER_AGENT: &str = concat!("cardigan/", env!("CARGO_PKG_VERSION"));
const XML: &str = "application/xml; charset=utf-8";
const VCARD: &str = "text/vcard; charset=utf-8";

pub(crate) fn propfind() -> Method {
    Method::from_bytes(b"PROPFIND").expect("PROPFIND is a valid method token")
}

pub(crate) fn report() -> Method {
    Method::from_bytes(b"REPORT").expect("REPORT is a valid method token")
}

/// One WebDAV request. The body is XML this crate built, or a vCard (PII:
/// never logged).
pub(crate) struct DavRequest {
    method: Method,
    url: Url,
    depth: Option<&'static str>,
    headers: Vec<(&'static str, String)>,
    body: Option<(Vec<u8>, &'static str)>,
    /// Replaces `"METHOD /path"` in logs and errors when the path itself is
    /// PII (a photo URI).
    redacted: Option<&'static str>,
}

impl DavRequest {
    pub(crate) fn new(method: Method, url: Url) -> Self {
        Self {
            method,
            url,
            depth: None,
            headers: Vec::new(),
            body: None,
            redacted: None,
        }
    }

    #[must_use]
    pub(crate) fn depth(mut self, depth: &'static str) -> Self {
        self.depth = Some(depth);
        self
    }

    #[must_use]
    pub(crate) fn header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }

    #[must_use]
    pub(crate) fn xml(mut self, body: String) -> Self {
        self.body = Some((body.into_bytes(), XML));
        self
    }

    #[must_use]
    pub(crate) fn vcard(mut self, body: &[u8]) -> Self {
        self.body = Some((body.to_vec(), VCARD));
        self
    }

    /// Logs and errors name this request as `context` instead of by its
    /// path.
    #[must_use]
    pub(crate) fn redacted_context(mut self, context: &'static str) -> Self {
        self.redacted = Some(context);
        self
    }

    /// `"METHOD /path"`, or the redacted context when set: the one name
    /// `send` uses in every log line and error message.
    pub(crate) fn context(&self) -> String {
        match self.redacted {
            Some(context) => context.to_owned(),
            None => format!("{} {}", self.method, self.url.path()),
        }
    }
}

/// A response of any status. Callers decide which statuses are success.
pub(crate) struct DavResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    /// Raw body: may hold card data (PII). Never log it.
    pub body: Vec<u8>,
}

impl fmt::Debug for DavResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DavResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

impl DavResponse {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }

    pub(crate) fn is(&self, code: u16) -> bool {
        self.status.as_u16() == code
    }

    /// This response's status as a port error.
    pub(crate) fn error(&self, context: &str, href: Option<&Href>) -> AddressBookError {
        status_error(context, self.status, self.headers.get(RETRY_AFTER), href)
    }
}

/// reqwest client plus credentials. Automatic redirects are off: reqwest
/// drops `Authorization` on a cross-host hop, and discovery follows
/// redirects itself.
pub(crate) struct HttpClient {
    client: Client,
    username: String,
    password: SecretString,
    /// Host of a plain-http entry URL; `None` when the entry URL is https.
    plain_http_host: Option<String>,
}

impl HttpClient {
    pub(crate) fn new(
        entry_url: &Url,
        username: String,
        password: SecretString,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<Self, AddressBookError> {
        if !matches!(entry_url.scheme(), "http" | "https") {
            return Err(AddressBookError::Permanent(format!(
                "entry URL scheme {:?} is not http or https",
                entry_url.scheme()
            )));
        }
        let plain_http_host = if entry_url.scheme() == "https" {
            None
        } else {
            tracing::warn!(
                host = entry_url.host_str().unwrap_or_default(),
                "CardDAV entry URL is not https: credentials and contacts travel unencrypted"
            );
            entry_url.host_str().map(str::to_owned)
        };
        let client = Client::builder()
            .redirect(redirect::Policy::none())
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|_| AddressBookError::Permanent("building the HTTP client failed".into()))?;
        Ok(Self {
            client,
            username,
            password,
            plain_http_host,
        })
    }

    /// Credentials go over https, or over http only to the host of a
    /// plain-http entry URL (a local test server).
    pub(crate) fn may_send_credentials(&self, url: &Url) -> bool {
        url.scheme() == "https" || (url.scheme() == "http" && self.plain_http_host.is_some() && url.host_str() == self.plain_http_host.as_deref())
    }

    /// Sends the request and returns the response whatever its status.
    /// Transport failures (timeout, refused connection) are errors.
    pub(crate) async fn send(&self, request: DavRequest) -> Result<DavResponse, AddressBookError> {
        let context = request.context();
        let mut builder = self.client.request(request.method.clone(), request.url.clone());
        if self.may_send_credentials(&request.url) {
            builder = builder.basic_auth(&self.username, Some(self.password.expose_secret()));
        }
        if let Some(depth) = request.depth {
            builder = builder.header("Depth", depth);
        }
        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }
        if let Some((body, content_type)) = request.body {
            builder = builder.header(CONTENT_TYPE, content_type).body(body);
        }

        let started = Instant::now();
        let response = builder.send().await.map_err(|e| transport_error(&context, &e))?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED {
            tracing::warn!(
                host = request.url.host_str().unwrap_or_default(),
                request = %context,
                "CardDAV server rejected the credentials; check this account's app-specific password"
            );
        }
        let headers = response.headers().clone();
        let body = response.bytes().await.map_err(|e| transport_error(&context, &e))?.to_vec();
        tracing::debug!(
            request = %context,
            status = status.as_u16(),
            elapsed_ms = started.elapsed().as_millis(),
            "CardDAV request"
        );
        Ok(DavResponse { status, headers, body })
    }
}

#[cfg(test)]
mod tests {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_string, header, method, path},
    };

    use super::*;
    use crate::test_util::BASIC;

    fn client(entry: &str, request_timeout: Duration) -> HttpClient {
        HttpClient::new(
            &Url::parse(entry).unwrap(),
            "user".to_owned(),
            SecretString::from("pass"),
            Duration::from_secs(5),
            request_timeout,
        )
        .unwrap()
    }

    fn url(server: &MockServer, path: &str) -> Url {
        Url::parse(&format!("{}{path}", server.uri())).unwrap()
    }

    #[tokio::test]
    async fn sends_credentials_depth_and_xml_body() {
        let server = MockServer::start().await;
        Mock::given(method("PROPFIND"))
            .and(path("/dav/"))
            .and(header("authorization", BASIC))
            .and(header("depth", "0"))
            .and(header("content-type", "application/xml; charset=utf-8"))
            .and(body_string("<x/>"))
            .respond_with(ResponseTemplate::new(207).set_body_string("<ok/>"))
            .expect(1)
            .mount(&server)
            .await;

        let http = client(&server.uri(), Duration::from_secs(5));
        let response = http
            .send(DavRequest::new(propfind(), url(&server, "/dav/")).depth("0").xml("<x/>".to_owned()))
            .await
            .unwrap();

        assert_eq!(response.status, StatusCode::MULTI_STATUS);
        assert_eq!(response.body, b"<ok/>");
    }

    #[tokio::test]
    async fn non_success_status_is_returned_not_raised() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(404)).mount(&server).await;

        let http = client(&server.uri(), Duration::from_secs(5));
        let response = http.send(DavRequest::new(Method::GET, url(&server, "/gone.vcf"))).await.unwrap();

        assert!(response.is(404));
        assert_eq!(
            response.error("GET /gone.vcf", None),
            AddressBookError::Permanent("GET /gone.vcf: HTTP 404".into())
        );
    }

    #[tokio::test]
    async fn retry_after_header_reaches_the_error() {
        let server = MockServer::start().await;
        Mock::given(method("REPORT"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "30"))
            .mount(&server)
            .await;

        let http = client(&server.uri(), Duration::from_secs(5));
        let response = http.send(DavRequest::new(report(), url(&server, "/card/"))).await.unwrap();

        assert_eq!(
            response.error("REPORT /card/", None),
            AddressBookError::RateLimited {
                retry_after: Some(Duration::from_secs(30))
            }
        );
    }

    #[tokio::test]
    async fn timeout_is_transient() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(500)))
            .mount(&server)
            .await;

        let http = client(&server.uri(), Duration::from_millis(100));
        let error = http.send(DavRequest::new(Method::GET, url(&server, "/slow"))).await.unwrap_err();

        assert_eq!(error, AddressBookError::Transient("GET /slow: timed out".into()));
    }

    #[tokio::test]
    async fn refused_connection_is_transient() {
        let http = client("http://127.0.0.1:1/", Duration::from_secs(5));
        let error = http
            .send(DavRequest::new(Method::GET, Url::parse("http://127.0.0.1:1/x").unwrap()))
            .await
            .unwrap_err();

        assert_eq!(error, AddressBookError::Transient("GET /x: connection failed".into()));
    }

    #[test]
    fn credentials_only_over_https_or_to_the_plain_http_entry_host() {
        let local = client("http://127.0.0.1:5232/", Duration::from_secs(5));
        assert!(local.may_send_credentials(&Url::parse("http://127.0.0.1:9999/other-port").unwrap()));
        assert!(!local.may_send_credentials(&Url::parse("http://localhost:5232/").unwrap()));
        assert!(local.may_send_credentials(&Url::parse("https://p42-contacts.icloud.com/").unwrap()));

        let icloud = client("https://contacts.icloud.com/", Duration::from_secs(5));
        assert!(icloud.may_send_credentials(&Url::parse("https://p42-contacts.icloud.com/").unwrap()));
        assert!(!icloud.may_send_credentials(&Url::parse("http://contacts.icloud.com/").unwrap()));
    }

    #[test]
    fn non_http_scheme_entry_url_is_rejected() {
        let result = HttpClient::new(
            &Url::parse("ftp://example.com/").unwrap(),
            "user".to_owned(),
            SecretString::from("pass"),
            Duration::from_secs(5),
            Duration::from_secs(5),
        );

        match result {
            Err(error) => assert!(matches!(error, AddressBookError::Permanent(_)), "{error:?}"),
            Ok(_) => panic!("expected ftp:// entry URL to be rejected"),
        }
    }

    #[test]
    fn debug_omits_body_content() {
        let response = DavResponse {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: b"BEGIN:VCARD\r\nEMAIL:jane@example.com\r\nEND:VCARD\r\n".to_vec(),
        };

        let debug = format!("{response:?}");

        assert!(!debug.contains("jane@example.com"), "body leaked into Debug: {debug}");
        assert!(debug.contains("body_bytes"), "{debug}");
    }

    #[tokio::test]
    async fn no_credentials_to_plain_http_foreign_host() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(200)).mount(&server).await;

        // The entry host is "localhost", the server is reached as 127.0.0.1.
        let port = server.address().port();
        let http = client(&format!("http://localhost:{port}/"), Duration::from_secs(5));
        http.send(DavRequest::new(Method::GET, url(&server, "/x"))).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(!requests[0].headers.contains_key("authorization"));
    }
}
