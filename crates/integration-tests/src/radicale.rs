//! A Radicale container with two address books under one user.

use std::time::Duration;

use reqwest::{Method, StatusCode};
use testcontainers::{ContainerAsync, GenericImage, core::IntoContainerPort, runners::AsyncRunner};
use url::Url;

/// Radicale 3.8.1. Its default config uses `auth.type = none`: any user name
/// and password log in, and each user owns `/<user>/`.
const IMAGE: &str = "tomsquest/docker-radicale";
const TAG: &str = "3.8.1.0";
const PORT: u16 = 5232;

pub(crate) const USER: &str = "cardigan";
pub(crate) const PASSWORD: &str = "test-password";
/// The collection that plays iCloud.
pub(crate) const ICLOUD: &str = "icloud";
/// The collection that plays Fastmail.
pub(crate) const FASTMAIL: &str = "fastmail";

const MKCOL_ADDRESSBOOK: &str = r#"<?xml version="1.0" encoding="utf-8"?><D:mkcol xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav"><D:set><D:prop><D:resourcetype><D:collection/><C:addressbook/></D:resourcetype></D:prop></D:set></D:mkcol>"#;

/// A running Radicale; dropping it removes the container.
pub(crate) struct Radicale {
    _container: ContainerAsync<GenericImage>,
    pub(crate) base_url: Url,
    http: reqwest::Client,
}

impl Radicale {
    /// Starts the container, waits until it answers, and creates the
    /// `icloud` and `fastmail` address books.
    pub(crate) async fn start() -> Self {
        let container = GenericImage::new(IMAGE, TAG)
            .with_exposed_port(PORT.tcp())
            .start()
            .await
            .expect("radicale container should start — is docker/colima running?");
        let host = container.get_host().await.expect("radicale host").to_string();
        let port = container.get_host_port_ipv4(PORT.tcp()).await.expect("mapped radicale port");
        let radicale = Self {
            _container: container,
            base_url: Url::parse(&format!("http://{host}:{port}/")).expect("radicale base url"),
            http: reqwest::Client::new(),
        };
        radicale.wait_ready().await;
        radicale.create_addressbook(ICLOUD).await;
        radicale.create_addressbook(FASTMAIL).await;
        radicale
    }

    /// Polls `PROPFIND /` for up to 30 s (a readiness probe, not a log line).
    async fn wait_ready(&self) {
        let mut last = "no attempt made".to_owned();
        for _ in 0..60 {
            match self
                .http
                .request(method(b"PROPFIND"), self.base_url.clone())
                .basic_auth(USER, Some(PASSWORD))
                .header("Depth", "0")
                .send()
                .await
            {
                Ok(response) if response.status() == StatusCode::MULTI_STATUS => return,
                Ok(response) => last = format!("status {}", response.status()),
                Err(error) => last = format!("error {error}"),
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        panic!("radicale did not answer PROPFIND / within 30 s; last attempt: {last}");
    }

    async fn create_addressbook(&self, name: &str) {
        let url = self.base_url.join(&format!("{USER}/{name}/")).expect("collection url");
        let response = self
            .http
            .request(method(b"MKCOL"), url)
            .basic_auth(USER, Some(PASSWORD))
            .header("Content-Type", "application/xml; charset=utf-8")
            .body(MKCOL_ADDRESSBOOK)
            .send()
            .await
            .expect("MKCOL request");
        let status = response.status();
        if status != StatusCode::CREATED {
            let body_len = response.text().await.map_or(0, |body| body.len());
            panic!("MKCOL {name}: status {status}, body length {body_len}");
        }
    }
}

fn method(name: &[u8]) -> Method {
    Method::from_bytes(name).expect("a valid HTTP method")
}
