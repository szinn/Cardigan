use cg_core::{AddressBookError, addressbook::Collection, contact::Href};
use url::Url;

use crate::{
    client::{DavRequest, HttpClient, propfind},
    config::ProviderQuirks,
    href::normalize_path,
    xml::{
        multistatus::{self, Multistatus},
        request,
    },
};

/// Redirect hops followed per discovery request.
const MAX_REDIRECTS: usize = 5;

/// The collection `discover` bound the adapter to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Bound {
    /// Absolute collection URL, as the server reported it.
    pub collection_url: Url,
    /// Canonical path of the collection, without a trailing slash.
    pub collection_path: String,
    pub supports_sync_collection: bool,
}

impl Bound {
    fn new(collection_url: Url, supports_sync_collection: bool) -> Self {
        let collection_path = normalize_path(collection_url.path()).trim_end_matches('/').to_owned();
        Self {
            collection_url,
            collection_path,
            supports_sync_collection,
        }
    }

    /// True when `canonical` (a canonical path) is the collection itself.
    pub(crate) fn is_collection(&self, canonical: &str) -> bool {
        canonical.trim_end_matches('/') == self.collection_path
    }

    /// Absolute URL of a card resource.
    pub(crate) fn resource_url(&self, href: &Href) -> Result<Url, AddressBookError> {
        self.collection_url
            .join(href.as_str())
            .map_err(|_| AddressBookError::Permanent(format!("invalid href {href}")))
    }
}

/// Full discovery: principal, address book home set, collection selection.
pub(crate) async fn discover(http: &HttpClient, entry_url: &Url, quirks: &ProviderQuirks) -> Result<(Collection, Bound), AddressBookError> {
    let principal = find_principal(http, entry_url).await?;
    let home = find_home_set(http, &principal).await?;
    let (collection_url, supports_sync_collection) = select_collection(http, &home, quirks).await?;
    refuse_if_insecure(http, &collection_url)?;
    let discovered_host = collection_url
        .host_str()
        .ok_or_else(|| AddressBookError::Permanent("address book URL has no host".into()))?
        .to_owned();
    tracing::info!(
        host = %discovered_host,
        collection = last_segment(&collection_url),
        sync_collection = supports_sync_collection,
        "CardDAV discovery complete"
    );
    let collection = Collection {
        addressbook_url: collection_url.to_string(),
        discovered_host,
        supports_sync_collection,
    };
    Ok((collection, Bound::new(collection_url, supports_sync_collection)))
}

/// PROPFIND that follows 301/302/307/308 itself, re-sending method and body,
/// so credentials survive a hop to another host. Returns the URL that finally
/// answered (hrefs resolve against it) and the parsed multistatus.
async fn propfind_following(http: &HttpClient, url: &Url, depth: &'static str, body: &str) -> Result<(Url, Multistatus), AddressBookError> {
    let mut url = url.clone();
    refuse_if_insecure(http, &url)?;
    for _ in 0..=MAX_REDIRECTS {
        let context = format!("PROPFIND {}", url.path());
        let response = http.send(DavRequest::new(propfind(), url.clone()).depth(depth).xml(body.to_owned())).await?;
        match response.status.as_u16() {
            207 => return Ok((url, multistatus::parse(&response.body)?)),
            301 | 302 | 307 | 308 => {
                let location = response
                    .header("location")
                    .ok_or_else(|| AddressBookError::Permanent(format!("{context}: redirect without Location")))?;
                let next = url
                    .join(location)
                    .map_err(|_| AddressBookError::Permanent(format!("{context}: invalid redirect Location")))?;
                refuse_if_insecure(http, &next).map_err(|_| {
                    AddressBookError::Permanent(format!(
                        "{context}: refusing insecure redirect to {}",
                        next.host_str().unwrap_or("an unknown host")
                    ))
                })?;
                url = next;
            }
            _ => return Err(response.error(&context, None)),
        }
    }
    Err(AddressBookError::Permanent(format!("PROPFIND: more than {MAX_REDIRECTS} redirects")))
}

/// Refuses a discovered URL (principal, home set, or collection href) that
/// `http` may not send credentials to, so discovery never silently drops
/// `Authorization` (a misleading `Unauthorized`) or binds a collection over
/// plain http.
fn refuse_if_insecure(http: &HttpClient, url: &Url) -> Result<(), AddressBookError> {
    if http.may_send_credentials(url) {
        Ok(())
    } else {
        Err(AddressBookError::Permanent(format!(
            "refusing insecure discovery URL at {}",
            url.host_str().unwrap_or("an unknown host")
        )))
    }
}

/// Principal at the entry URL, falling back to `/.well-known/carddav` (RFC
/// 6764) when the entry URL is the bare host and does not answer.
async fn find_principal(http: &HttpClient, entry_url: &Url) -> Result<Url, AddressBookError> {
    match principal_at(http, entry_url).await {
        Ok(Some(principal)) => return Ok(principal),
        Ok(None) | Err(AddressBookError::Permanent(_)) if entry_url.path() == "/" => {}
        Ok(None) => return Err(AddressBookError::Permanent("entry URL reports no current-user-principal".into())),
        Err(error) => return Err(error),
    }
    let well_known = entry_url
        .join("/.well-known/carddav")
        .map_err(|_| AddressBookError::Permanent("invalid entry URL".into()))?;
    principal_at(http, &well_known)
        .await?
        .ok_or_else(|| AddressBookError::Permanent("server reports no current-user-principal".into()))
}

async fn principal_at(http: &HttpClient, url: &Url) -> Result<Option<Url>, AddressBookError> {
    let (answered, multistatus) = propfind_following(http, url, "0", &request::propfind_current_user_principal()).await?;
    multistatus
        .responses
        .iter()
        .find_map(|entry| entry.props.current_user_principal.as_deref())
        .map(|href| {
            answered
                .join(href)
                .map_err(|_| AddressBookError::Permanent("invalid current-user-principal href".into()))
        })
        .transpose()
}

async fn find_home_set(http: &HttpClient, principal: &Url) -> Result<Url, AddressBookError> {
    let (answered, multistatus) = propfind_following(http, principal, "0", &request::propfind_addressbook_home_set()).await?;
    let href = multistatus
        .responses
        .iter()
        .find_map(|entry| entry.props.addressbook_home_set.first())
        .ok_or_else(|| AddressBookError::Permanent("principal has no addressbook-home-set".into()))?;
    answered
        .join(href)
        .map_err(|_| AddressBookError::Permanent("invalid addressbook-home-set href".into()))
}

/// Decision 5: the only address book; else the provider's default name;
/// else the first in the home set.
async fn select_collection(http: &HttpClient, home: &Url, quirks: &ProviderQuirks) -> Result<(Url, bool), AddressBookError> {
    let (answered, multistatus) = propfind_following(http, home, "1", &request::propfind_collections()).await?;
    let books = multistatus
        .responses
        .iter()
        .filter(|entry| entry.props.is_addressbook)
        .map(|entry| {
            answered
                .join(&entry.href)
                .map(|url| (url, entry.props.supports_sync_collection))
                .map_err(|_| AddressBookError::Permanent("invalid address book href".into()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let chosen = match books.as_slice() {
        [] => return Err(AddressBookError::Permanent("addressbook-home-set holds no address book".into())),
        [only] => only,
        several => quirks
            .default_collection
            .as_deref()
            .and_then(|name| several.iter().find(|(url, _)| last_segment(url) == name))
            .unwrap_or(&several[0]),
    };
    Ok(chosen.clone())
}

/// Last non-empty path segment (`/h/Default/` → `Default`).
fn last_segment(url: &Url) -> &str {
    url.path_segments()
        .and_then(|mut segments| segments.rfind(|s| !s.is_empty()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use cg_core::{AddressBookError, Error, addressbook::AddressBook};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_string_contains, header, method, path},
    };

    use crate::{
        adapter::CardDavAddressBook,
        config::ProviderQuirks,
        test_util::{BASIC, addressbook_entry, config, home_set_body, mount_discovery, multistatus, plain_collection_entry, principal_body},
    };

    fn adapter(server: &MockServer, quirks: ProviderQuirks) -> CardDavAddressBook {
        CardDavAddressBook::new(config(&server.uri(), quirks)).unwrap()
    }

    async fn mount_home(server: &MockServer, home: &str, entries: String) {
        Mock::given(method("PROPFIND"))
            .and(path(home))
            .and(header("depth", "1"))
            .and(body_string_contains("supported-report-set"))
            .respond_with(ResponseTemplate::new(207).set_body_string(multistatus(&entries)))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn discovers_principal_home_set_and_single_collection() {
        let server = MockServer::start().await;
        mount_discovery(&server, true).await;

        let collection = adapter(&server, ProviderQuirks::default()).discover().await.unwrap();

        assert_eq!(collection.addressbook_url, format!("{}/home/card/", server.uri()));
        assert_eq!(collection.discovered_host, "127.0.0.1");
        assert!(collection.supports_sync_collection);
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 3);
        assert!(requests.iter().all(|r| r.headers.get("authorization").is_some_and(|v| v == BASIC)));
    }

    #[tokio::test]
    async fn supports_sync_collection_false_when_not_reported() {
        let server = MockServer::start().await;
        mount_discovery(&server, false).await;

        let collection = adapter(&server, ProviderQuirks::default()).discover().await.unwrap();

        assert!(!collection.supports_sync_collection);
    }

    #[tokio::test]
    async fn follows_redirect_to_other_server_with_credentials() {
        let entry = MockServer::start().await;
        let numbered = MockServer::start().await;
        Mock::given(method("PROPFIND"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(301).insert_header("Location", format!("{}/", numbered.uri())))
            .mount(&entry)
            .await;
        Mock::given(method("PROPFIND"))
            .and(header("authorization", BASIC))
            .and(path("/"))
            .and(body_string_contains("current-user-principal"))
            .respond_with(ResponseTemplate::new(207).set_body_string(principal_body("/p/")))
            .expect(1)
            .mount(&numbered)
            .await;
        Mock::given(method("PROPFIND"))
            .and(header("authorization", BASIC))
            .and(path("/p/"))
            .respond_with(ResponseTemplate::new(207).set_body_string(home_set_body("/p/", "/home/")))
            .mount(&numbered)
            .await;
        mount_home(&numbered, "/home/", addressbook_entry("/home/card/", true)).await;

        let collection = adapter(&entry, ProviderQuirks::default()).discover().await.unwrap();

        assert_eq!(collection.addressbook_url, format!("{}/home/card/", numbered.uri()));
    }

    #[tokio::test]
    async fn home_set_on_another_host_binds_there() {
        let entry = MockServer::start().await;
        let numbered = MockServer::start().await;
        Mock::given(method("PROPFIND"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(207).set_body_string(principal_body("/123/principal/")))
            .mount(&entry)
            .await;
        Mock::given(method("PROPFIND"))
            .and(path("/123/principal/"))
            .respond_with(ResponseTemplate::new(207).set_body_string(home_set_body("/123/principal/", &format!("{}/123/carddavhome/", numbered.uri()))))
            .mount(&entry)
            .await;
        mount_home(
            &numbered,
            "/123/carddavhome/",
            format!(
                "{}{}",
                plain_collection_entry("/123/carddavhome/"),
                addressbook_entry("/123/carddavhome/card/", true)
            ),
        )
        .await;

        let adapter = adapter(&entry, ProviderQuirks::icloud());
        let collection = adapter.discover().await.unwrap();

        assert_eq!(collection.addressbook_url, format!("{}/123/carddavhome/card/", numbered.uri()));
        let home_requests = numbered.received_requests().await.unwrap();
        assert!(home_requests.iter().all(|r| r.headers.get("authorization").is_some_and(|v| v == BASIC)));
        assert_eq!(adapter.bound().unwrap().collection_path, "/123/carddavhome/card");
    }

    #[tokio::test]
    async fn falls_back_to_well_known_when_root_has_no_principal() {
        let server = MockServer::start().await;
        Mock::given(method("PROPFIND"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("PROPFIND"))
            .and(path("/.well-known/carddav"))
            .respond_with(ResponseTemplate::new(301).insert_header("Location", "/dav/"))
            .mount(&server)
            .await;
        Mock::given(method("PROPFIND"))
            .and(path("/dav/"))
            .respond_with(ResponseTemplate::new(207).set_body_string(principal_body("/p/")))
            .mount(&server)
            .await;
        Mock::given(method("PROPFIND"))
            .and(path("/p/"))
            .respond_with(ResponseTemplate::new(207).set_body_string(home_set_body("/p/", "/home/")))
            .mount(&server)
            .await;
        mount_home(&server, "/home/", addressbook_entry("/home/card/", true)).await;

        let collection = adapter(&server, ProviderQuirks::default()).discover().await.unwrap();

        assert_eq!(collection.addressbook_url, format!("{}/home/card/", server.uri()));
    }

    async fn mount_principal_and_home(server: &MockServer) {
        Mock::given(method("PROPFIND"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(207).set_body_string(principal_body("/p/")))
            .mount(server)
            .await;
        Mock::given(method("PROPFIND"))
            .and(path("/p/"))
            .respond_with(ResponseTemplate::new(207).set_body_string(home_set_body("/p/", "/h/")))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn selects_the_provider_default_among_several() {
        let server = MockServer::start().await;
        mount_principal_and_home(&server).await;
        mount_home(
            &server,
            "/h/",
            format!("{}{}", addressbook_entry("/h/other/", true), addressbook_entry("/h/Default/", false)),
        )
        .await;

        let collection = adapter(&server, ProviderQuirks::fastmail()).discover().await.unwrap();

        assert_eq!(collection.addressbook_url, format!("{}/h/Default/", server.uri()));
        assert!(!collection.supports_sync_collection);
    }

    #[tokio::test]
    async fn selects_the_first_when_no_name_matches() {
        let server = MockServer::start().await;
        mount_principal_and_home(&server).await;
        mount_home(
            &server,
            "/h/",
            format!("{}{}", addressbook_entry("/h/first/", true), addressbook_entry("/h/second/", true)),
        )
        .await;

        let collection = adapter(&server, ProviderQuirks::icloud()).discover().await.unwrap();

        assert_eq!(collection.addressbook_url, format!("{}/h/first/", server.uri()));
    }

    #[tokio::test]
    async fn home_set_without_addressbook_is_permanent() {
        let server = MockServer::start().await;
        mount_principal_and_home(&server).await;
        mount_home(&server, "/h/", plain_collection_entry("/h/")).await;

        let error = adapter(&server, ProviderQuirks::default()).discover().await.unwrap_err();

        assert!(matches!(error, Error::AddressBook(AddressBookError::Permanent(_))), "{error:?}");
    }

    #[tokio::test]
    async fn unauthorized_is_reported() {
        let server = MockServer::start().await;
        Mock::given(method("PROPFIND")).respond_with(ResponseTemplate::new(401)).mount(&server).await;

        let error = adapter(&server, ProviderQuirks::default()).discover().await.unwrap_err();

        match error {
            Error::AddressBook(inner) => assert_eq!(inner, AddressBookError::Unauthorized),
            other => panic!("expected AddressBook error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn insecure_redirect_to_another_host_is_refused() {
        let server = MockServer::start().await;
        let port = server.address().port();
        Mock::given(method("PROPFIND"))
            .respond_with(ResponseTemplate::new(302).insert_header("Location", format!("http://localhost:{port}/")))
            .mount(&server)
            .await;

        let error = adapter(&server, ProviderQuirks::default()).discover().await.unwrap_err();

        match error {
            Error::AddressBook(AddressBookError::Permanent(message)) => assert!(message.contains("insecure"), "{message}"),
            other => panic!("expected Permanent, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn insecure_absolute_home_set_href_is_refused() {
        // wiremock only serves plain http, so an https-policy account cannot
        // be exercised directly. Instead this uses an absolute href to a
        // different plain-http host ("localhost") than the entry host
        // (127.0.0.1, from `server.uri()`), which `may_send_credentials`
        // also refuses -- the same mechanism
        // `insecure_redirect_to_another_host_is_refused`
        // below exercises for the redirect path.
        let server = MockServer::start().await;
        let port = server.address().port();
        Mock::given(method("PROPFIND"))
            .and(path("/"))
            .and(body_string_contains("current-user-principal"))
            .respond_with(ResponseTemplate::new(207).set_body_string(principal_body("/p/")))
            .mount(&server)
            .await;
        Mock::given(method("PROPFIND"))
            .and(path("/p/"))
            .and(body_string_contains("addressbook-home-set"))
            .respond_with(ResponseTemplate::new(207).set_body_string(home_set_body("/p/", &format!("http://localhost:{port}/home/"))))
            .mount(&server)
            .await;

        let error = adapter(&server, ProviderQuirks::default()).discover().await.unwrap_err();

        match error {
            Error::AddressBook(AddressBookError::Permanent(message)) => assert!(message.contains("insecure"), "{message}"),
            other => panic!("expected Permanent, got {other:?}"),
        }
        let requests = server.received_requests().await.unwrap();
        assert!(requests.iter().all(|r| r.url.path() != "/home/"), "must not query the insecure home set");
    }

    #[tokio::test]
    async fn insecure_collection_href_is_refused() {
        // Same technique as above: the home set resolves (relatively, so it
        // stays on the entry host), but the address book collection itself
        // is reported at an absolute href on a different plain-http host.
        let server = MockServer::start().await;
        let port = server.address().port();
        mount_principal_and_home(&server).await;
        mount_home(&server, "/h/", addressbook_entry(&format!("http://localhost:{port}/h/card/"), true)).await;

        let error = adapter(&server, ProviderQuirks::default()).discover().await.unwrap_err();

        match error {
            Error::AddressBook(AddressBookError::Permanent(message)) => assert!(message.contains("insecure"), "{message}"),
            other => panic!("expected Permanent, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn redirect_loop_is_permanent() {
        let server = MockServer::start().await;
        Mock::given(method("PROPFIND"))
            .respond_with(ResponseTemplate::new(307).insert_header("Location", "/"))
            .mount(&server)
            .await;

        let error = adapter(&server, ProviderQuirks::default()).discover().await.unwrap_err();

        match error {
            Error::AddressBook(AddressBookError::Permanent(message)) => assert!(message.contains("redirects"), "{message}"),
            other => panic!("expected Permanent, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn operations_before_discover_are_not_discovered() {
        let server = MockServer::start().await;
        let adapter = adapter(&server, ProviderQuirks::default());

        let error = adapter.list_etags().await.unwrap_err();

        match error {
            Error::AddressBook(inner) => assert_eq!(inner, AddressBookError::Permanent("not discovered".into())),
            other => panic!("expected AddressBook error, got {other:?}"),
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn rediscovery_replaces_the_binding() {
        let server = MockServer::start().await;
        mount_discovery(&server, true).await;
        let adapter = adapter(&server, ProviderQuirks::default());

        let first = adapter.discover().await.unwrap();
        let second = adapter.discover().await.unwrap();

        assert_eq!(first, second);
        assert_eq!(adapter.bound().unwrap().collection_path, "/home/card");
    }
}
