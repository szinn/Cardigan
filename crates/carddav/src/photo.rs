//! Downloads iCloud photo URIs (CG-15 R6). Credentials only to https
//! `*.icloud.com`; no redirects; bodies over `MAX_PHOTO_BYTES` refused.
//! PII: a URI never appears in logs or errors.

use cg_core::{AddressBookError, Error, addressbook::PhotoFetcher, contact::PhotoUri};
use reqwest::Method;
use secrecy::SecretString;
use url::Url;

use crate::{
    client::{DavRequest, HttpClient},
    config::{DEFAULT_CONNECT_TIMEOUT, DEFAULT_REQUEST_TIMEOUT},
};

const MAX_PHOTO_BYTES: usize = 10 * 1024 * 1024;

pub struct IcloudPhotoFetcher {
    http: HttpClient,
}

impl IcloudPhotoFetcher {
    pub fn new(username: String, password: SecretString) -> Result<Self, AddressBookError> {
        let entry = Url::parse("https://gateway.icloud.com/").expect("a valid URL");
        Ok(Self {
            http: HttpClient::new(&entry, username, password, DEFAULT_CONNECT_TIMEOUT, DEFAULT_REQUEST_TIMEOUT)?,
        })
    }

    #[cfg(test)]
    fn for_test(base: &str, username: &str, password: &str) -> Self {
        let entry = Url::parse(base).unwrap();
        Self {
            http: HttpClient::new(
                &entry,
                username.into(),
                SecretString::from(password),
                DEFAULT_CONNECT_TIMEOUT,
                DEFAULT_REQUEST_TIMEOUT,
            )
            .unwrap(),
        }
    }

    /// GETs `url` (already allowed), mapping statuses like CardDAV.
    async fn fetch_from(&self, url: &Url) -> Result<Vec<u8>, AddressBookError> {
        let request = DavRequest::new(Method::GET, url.clone()).redacted_context("GET photo");
        let response = self.http.send(request).await?;
        if !response.status.is_success() {
            return Err(response.error("GET photo", None));
        }
        if response.body.len() > MAX_PHOTO_BYTES {
            return Err(AddressBookError::Permanent("GET photo: body over the size cap".into()));
        }
        Ok(response.body)
    }
}

/// https, no userinfo, and the host is `icloud.com` or a subdomain of it.
fn allowed(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.host_str().is_some_and(|host| host == "icloud.com" || host.ends_with(".icloud.com"))
}

#[async_trait::async_trait]
impl PhotoFetcher for IcloudPhotoFetcher {
    async fn fetch(&self, uri: &PhotoUri) -> Result<Vec<u8>, Error> {
        let url = Url::parse(uri.as_str()).map_err(|_| AddressBookError::Permanent("photo URI does not parse".into()))?;
        if !allowed(&url) {
            return Err(AddressBookError::Permanent("photo URI is not on an allowed host".into()).into());
        }
        Ok(self.fetch_from(&url).await?)
    }
}

#[cfg(test)]
mod tests {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    use super::*;

    #[test]
    fn only_https_icloud_hosts_are_allowed() {
        for ok in [
            "https://gateway.icloud.com/a",
            "https://p144-contacts.icloud.com/x",
            "https://GATEWAY.iCloud.COM/a",
        ] {
            assert!(allowed(&Url::parse(ok).unwrap()), "{ok}");
        }
        for refused in [
            "http://gateway.icloud.com/a",
            "https://evil.example/a",
            "https://icloud.com.evil.example/a",
            "https://noticloud.com/a",
            "https://gateway.icloud.com@evil.example/a",
            "https://u:p@x.icloud.com/a",
            "https://u@x.icloud.com/a",
            "https://gateway.icloud.com./a",
            "https://17.0.0.1/a",
            "https://[::1]/a",
        ] {
            assert!(!allowed(&Url::parse(refused).unwrap()), "{refused}");
        }
    }

    #[tokio::test]
    async fn a_refused_uri_sends_nothing() {
        let fetcher = IcloudPhotoFetcher::new("u".into(), SecretString::from("p")).unwrap();
        let error = fetcher.fetch(&PhotoUri::from("https://evil.example/a".to_owned())).await.unwrap_err();
        assert!(matches!(error, Error::AddressBook(AddressBookError::Permanent(_))), "{error:?}");
        assert!(!format!("{error}").contains("evil.example"), "no URI in error text");
    }

    #[tokio::test]
    async fn fetch_sends_basic_auth_and_the_user_agent_and_maps_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ok"))
            .and(header("user-agent", concat!("cardigan/", env!("CARGO_PKG_VERSION"))))
            .and(header("authorization", "Basic dTpw"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"JPEG".to_vec()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/moved"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/ok"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/gone"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let fetcher = IcloudPhotoFetcher::for_test(&server.uri(), "u", "p");

        assert_eq!(
            fetcher.fetch_from(&Url::parse(&format!("{}/ok", server.uri())).unwrap()).await.unwrap(),
            b"JPEG"
        );
        assert!(
            matches!(
                fetcher.fetch_from(&Url::parse(&format!("{}/moved", server.uri())).unwrap()).await,
                Err(AddressBookError::Permanent(_))
            ),
            "redirects are not followed"
        );
        assert_eq!(
            fetcher.fetch_from(&Url::parse(&format!("{}/gone", server.uri())).unwrap()).await,
            Err(AddressBookError::Unauthorized)
        );
    }

    #[derive(Clone, Default)]
    struct Captured(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn no_photo_path_reaches_logs_or_errors() {
        let captured = Captured::default();
        let writer = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/secret-ok"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"JPEG".to_vec()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/secret-denied"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/secret-missing"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let fetcher = IcloudPhotoFetcher::for_test(&server.uri(), "u", "p");
        let at = |p: &str| Url::parse(&format!("{}{p}", server.uri())).unwrap();

        fetcher.fetch_from(&at("/secret-ok")).await.unwrap();
        fetcher.fetch_from(&at("/secret-denied")).await.unwrap_err();
        let missing = fetcher.fetch_from(&at("/secret-missing")).await.unwrap_err();
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let unreachable = fetcher
            .fetch_from(&Url::parse(&format!("http://{closed}/secret-unreachable")).unwrap())
            .await
            .unwrap_err();

        let logs = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("GET photo"), "requests are still logged:\n{logs}");
        assert!(!logs.contains("secret"), "a photo path reached the logs:\n{logs}");
        assert!(matches!(unreachable, AddressBookError::Transient(_)), "{unreachable:?}");
        for error in [missing, unreachable] {
            let text = format!("{error} {error:?}");
            assert!(!text.contains("secret"), "a photo path reached an error: {text}");
        }
    }
}
