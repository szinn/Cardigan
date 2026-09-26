use std::sync::{PoisonError, RwLock};

use cg_core::{
    AddressBookError, Error,
    addressbook::{AddressBook, Changes, Collection, MultigetResult, Precondition, SyncToken},
    contact::{ETag, Href},
};
use url::Url;

use crate::{
    client::HttpClient,
    config::{CardDavConfig, ProviderQuirks},
    discovery::{self, Bound},
    multiget, sync, write,
};

/// `AddressBook` over CardDAV for one side. Call `discover` first: every
/// other method fails with `Permanent("not discovered")` until it succeeds.
pub struct CardDavAddressBook {
    http: HttpClient,
    entry_url: Url,
    quirks: ProviderQuirks,
    bound: RwLock<Option<Bound>>,
}

impl CardDavAddressBook {
    pub fn new(config: CardDavConfig) -> Result<Self, Error> {
        let CardDavConfig {
            entry_url,
            username,
            password,
            quirks,
            connect_timeout,
            request_timeout,
        } = config;
        let http = HttpClient::new(&entry_url, username, password, connect_timeout, request_timeout)?;
        Ok(Self {
            http,
            entry_url,
            quirks,
            bound: RwLock::new(None),
        })
    }

    /// The current binding, cloned so no lock is held across an `.await`.
    pub(crate) fn bound(&self) -> Result<Bound, AddressBookError> {
        self.bound
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| AddressBookError::Permanent("not discovered".into()))
    }
}

#[async_trait::async_trait]
impl AddressBook for CardDavAddressBook {
    async fn discover(&self) -> Result<Collection, Error> {
        let (collection, bound) = discovery::discover(&self.http, &self.entry_url, &self.quirks).await?;
        *self.bound.write().unwrap_or_else(PoisonError::into_inner) = Some(bound);
        Ok(collection)
    }

    async fn changes_since(&self, token: Option<&SyncToken>) -> Result<Changes, Error> {
        let bound = self.bound()?;
        Ok(sync::changes_since(&self.http, &bound, token).await?)
    }

    async fn list_etags(&self) -> Result<Vec<(Href, ETag)>, Error> {
        let bound = self.bound()?;
        Ok(sync::list_etags(&self.http, &bound).await?)
    }

    async fn multiget(&self, hrefs: &[Href]) -> Result<MultigetResult, Error> {
        let bound = self.bound()?;
        Ok(multiget::multiget(&self.http, &bound, self.quirks.multiget_batch, hrefs).await?)
    }

    async fn put(&self, href: &Href, body: &[u8], precondition: Precondition) -> Result<Option<ETag>, Error> {
        let bound = self.bound()?;
        Ok(write::put(&self.http, &bound, href, body, &precondition).await?)
    }

    async fn delete(&self, href: &Href, if_match: Option<&ETag>) -> Result<(), Error> {
        let bound = self.bound()?;
        Ok(write::delete(&self.http, &bound, href, if_match).await?)
    }
}
