//! Test doubles for driving the sync engine without servers. Compiled for
//! cg-core's own tests and, with the `test-support` feature, for other
//! crates' tests.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Mutex, MutexGuard},
};

use crate::{
    AddressBookError, Error,
    addressbook::{AddressBook, ChangeSet, Changes, Collection, FetchedCard, MultigetResult, Precondition, SyncToken},
    contact::{ETag, Href},
};

/// Prefix of the sync tokens the fake issues; the rest is a version number.
const TOKEN_PREFIX: &str = "mem-sync-";

/// An `AddressBook` operation, for targeting injected failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Discover,
    ChangesSince,
    ListEtags,
    Multiget,
    Put,
    Delete,
}

/// A write the code under test attempted through the port, whether or not it
/// succeeded. Out-of-band edits (`external_put`, `external_delete`) are not
/// recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Write {
    Put { href: Href, precondition: Precondition, body: Vec<u8> },
    Delete { href: Href, if_match: Option<ETag> },
}

/// A stateful in-memory `AddressBook`. It enforces `If-Match` and
/// `If-None-Match`, serves real sync-token deltas from a change log, records
/// every write attempt, and can inject faults. Safe to share across tasks;
/// the lock is never held across an `.await`.
///
/// Diverges from a real adapter in a few ways: it needs no `discover()`
/// before use; `changed`, `removed` and `list_etags` come back in href order
/// (a real server's order is arbitrary — do not depend on it); it accepts any
/// href, including ones outside its collection; and it stores bodies verbatim
/// and never rewrites them (to simulate a server rewrite, follow a `put` with
/// `external_put`). Tests should use synthetic cards, since `Write` and
/// `card()` expose full bodies.
pub struct InMemoryAddressBook {
    state: Mutex<State>,
}

struct State {
    collection: Collection,
    cards: BTreeMap<Href, (ETag, Vec<u8>)>,
    /// Bumped on every mutation. ETags and sync tokens are derived from it.
    version: u64,
    /// `(version, href)` for every create, update and delete, in order.
    changes: Vec<(u64, Href)>,
    /// Tokens issued before this version are rejected.
    valid_from: u64,
    writes: Vec<Write>,
    omit_etag_on_put: bool,
    failures: VecDeque<(Op, AddressBookError)>,
}

impl InMemoryAddressBook {
    #[must_use]
    pub fn new(collection: Collection) -> Self {
        Self {
            state: Mutex::new(State {
                collection,
                cards: BTreeMap::new(),
                version: 0,
                changes: Vec::new(),
                valid_from: 0,
                writes: Vec::new(),
                omit_etag_on_put: false,
                failures: VecDeque::new(),
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("InMemoryAddressBook state poisoned")
    }

    /// Creates or replaces a card as the user or the other client would.
    /// Returns its new ETag. Not recorded in `writes`.
    pub fn external_put(&self, href: impl Into<Href>, body: impl Into<Vec<u8>>) -> ETag {
        self.state().store(href.into(), body.into())
    }

    /// Deletes a card as the user or the other client would. Returns whether
    /// it existed. Not recorded in `writes`.
    pub fn external_delete(&self, href: &Href) -> bool {
        self.state().remove(href)
    }

    /// Every `put` and `delete` the code under test attempted, in order.
    #[must_use]
    pub fn writes(&self) -> Vec<Write> {
        self.state().writes.clone()
    }

    /// The stored ETag and body for `href`.
    #[must_use]
    pub fn card(&self, href: &Href) -> Option<(ETag, Vec<u8>)> {
        self.state().cards.get(href).cloned()
    }

    /// When on, successful `put`s store the card but return no ETag, like a
    /// server that omits the `ETag` header.
    pub fn omit_etag_on_put(&self, omit: bool) {
        self.state().omit_etag_on_put = omit;
    }

    /// Fails the next call of `op` with `error`. Queued failures are consumed
    /// one per matching call, in order.
    pub fn fail_next(&self, op: Op, error: AddressBookError) {
        self.state().failures.push_back((op, error));
    }

    /// Rejects every sync token issued so far. Tokens issued afterwards work.
    pub fn expire_tokens(&self) {
        let mut state = self.state();
        state.version += 1;
        state.valid_from = state.version;
    }

    /// When false, `discover` reports no `sync-collection` support and
    /// `changes_since` fails with `AddressBookError::Permanent`.
    pub fn set_supports_sync_collection(&self, supported: bool) {
        self.state().collection.supports_sync_collection = supported;
    }
}

impl Default for InMemoryAddressBook {
    /// A book for a sync-capable collection at a fixed test URL.
    fn default() -> Self {
        Self::new(Collection {
            addressbook_url: "https://carddav.example.test/addressbooks/user/default/".to_owned(),
            discovered_host: "carddav.example.test".to_owned(),
            supports_sync_collection: true,
        })
    }
}

impl State {
    fn take_failure(&mut self, op: Op) -> Result<(), Error> {
        match self.failures.iter().position(|(queued, _)| *queued == op) {
            Some(index) => {
                let (_, error) = self.failures.remove(index).expect("index comes from position");
                Err(error.into())
            }
            None => Ok(()),
        }
    }

    fn store(&mut self, href: Href, body: Vec<u8>) -> ETag {
        self.version += 1;
        let etag = ETag::new(format!("\"{}\"", self.version));
        self.cards.insert(href.clone(), (etag.clone(), body));
        self.changes.push((self.version, href));
        etag
    }

    fn remove(&mut self, href: &Href) -> bool {
        if self.cards.remove(href).is_none() {
            return false;
        }
        self.version += 1;
        self.changes.push((self.version, href.clone()));
        true
    }

    fn token(&self) -> SyncToken {
        SyncToken::new(format!("{TOKEN_PREFIX}{}", self.version))
    }

    /// The version a token was issued at, if the token is still valid.
    fn token_version(&self, token: &SyncToken) -> Option<u64> {
        let version = token.as_str().strip_prefix(TOKEN_PREFIX)?.parse::<u64>().ok()?;
        (self.valid_from..=self.version).contains(&version).then_some(version)
    }
}

#[async_trait::async_trait]
impl AddressBook for InMemoryAddressBook {
    async fn discover(&self) -> Result<Collection, Error> {
        let mut state = self.state();
        state.take_failure(Op::Discover)?;
        Ok(state.collection.clone())
    }

    async fn changes_since(&self, token: Option<&SyncToken>) -> Result<Changes, Error> {
        let mut state = self.state();
        state.take_failure(Op::ChangesSince)?;
        if !state.collection.supports_sync_collection {
            return Err(AddressBookError::Permanent("collection does not support sync-collection".to_owned()).into());
        }

        let Some(token) = token else {
            let changed = state.cards.iter().map(|(href, (etag, _))| (href.clone(), etag.clone())).collect();
            return Ok(Changes::Delta(ChangeSet {
                changed,
                removed: Vec::new(),
                token: state.token(),
            }));
        };
        let Some(since) = state.token_version(token) else {
            return Ok(Changes::TokenInvalid);
        };

        let touched: BTreeSet<&Href> = state.changes.iter().filter(|(version, _)| *version > since).map(|(_, href)| href).collect();
        let mut changed = Vec::new();
        let mut removed = Vec::new();
        for href in touched {
            match state.cards.get(href) {
                Some((etag, _)) => changed.push((href.clone(), etag.clone())),
                None => removed.push(href.clone()),
            }
        }
        Ok(Changes::Delta(ChangeSet {
            changed,
            removed,
            token: state.token(),
        }))
    }

    async fn list_etags(&self) -> Result<Vec<(Href, ETag)>, Error> {
        let mut state = self.state();
        state.take_failure(Op::ListEtags)?;
        Ok(state.cards.iter().map(|(href, (etag, _))| (href.clone(), etag.clone())).collect())
    }

    async fn multiget(&self, hrefs: &[Href]) -> Result<MultigetResult, Error> {
        let mut state = self.state();
        state.take_failure(Op::Multiget)?;
        let mut result = MultigetResult::default();
        for href in hrefs {
            match state.cards.get(href) {
                Some((etag, body)) => result.found.push(FetchedCard {
                    href: href.clone(),
                    etag: etag.clone(),
                    body: body.clone(),
                }),
                None => result.missing.push(href.clone()),
            }
        }
        Ok(result)
    }

    async fn put(&self, href: &Href, body: &[u8], precondition: Precondition) -> Result<Option<ETag>, Error> {
        let mut state = self.state();
        state.writes.push(Write::Put {
            href: href.clone(),
            precondition: precondition.clone(),
            body: body.to_vec(),
        });
        state.take_failure(Op::Put)?;

        let current = state.cards.get(href).map(|(etag, _)| etag);
        let satisfied = match &precondition {
            Precondition::IfMatch(expected) => current == Some(expected),
            Precondition::IfNoneMatch => current.is_none(),
        };
        if !satisfied {
            return Err(AddressBookError::PreconditionFailed { href: href.clone() }.into());
        }

        let etag = state.store(href.clone(), body.to_vec());
        Ok((!state.omit_etag_on_put).then_some(etag))
    }

    async fn delete(&self, href: &Href, if_match: Option<&ETag>) -> Result<(), Error> {
        let mut state = self.state();
        state.writes.push(Write::Delete {
            href: href.clone(),
            if_match: if_match.cloned(),
        });
        state.take_failure(Op::Delete)?;

        let precondition_failed = match (state.cards.get(href), if_match) {
            (None, _) => return Ok(()),
            (Some((current, _)), Some(expected)) => current != expected,
            (Some(_), None) => false,
        };
        if precondition_failed {
            return Err(AddressBookError::PreconditionFailed { href: href.clone() }.into());
        }
        state.remove(href);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn href(path: &str) -> Href {
        Href::from(path)
    }

    fn is_precondition_failed(err: &Error) -> bool {
        matches!(err, Error::AddressBook(AddressBookError::PreconditionFailed { .. }))
    }

    async fn full_sync(book: &InMemoryAddressBook) -> ChangeSet {
        match book.changes_since(None).await.unwrap() {
            Changes::Delta(delta) => delta,
            Changes::TokenInvalid => panic!("initial sync cannot be TokenInvalid"),
        }
    }

    async fn delta_since(book: &InMemoryAddressBook, token: &SyncToken) -> ChangeSet {
        match book.changes_since(Some(token)).await.unwrap() {
            Changes::Delta(delta) => delta,
            Changes::TokenInvalid => panic!("token {token} was rejected"),
        }
    }

    #[tokio::test]
    async fn discover_returns_the_collection() {
        let collection = Collection {
            addressbook_url: "https://p42-contacts.icloud.com/123/carddavhome/card/".to_owned(),
            discovered_host: "p42-contacts.icloud.com".to_owned(),
            supports_sync_collection: true,
        };
        let book = InMemoryAddressBook::new(collection.clone());
        assert_eq!(book.discover().await.unwrap(), collection);
    }

    #[tokio::test]
    async fn put_if_none_match_creates_and_records_the_write() {
        let book = InMemoryAddressBook::default();
        let etag = book
            .put(&href("/a.vcf"), b"card-a", Precondition::IfNoneMatch)
            .await
            .unwrap()
            .expect("etag returned");

        assert_eq!(book.card(&href("/a.vcf")), Some((etag, b"card-a".to_vec())));
        assert_eq!(
            book.writes(),
            [Write::Put {
                href: href("/a.vcf"),
                precondition: Precondition::IfNoneMatch,
                body: b"card-a".to_vec()
            }]
        );
    }

    #[tokio::test]
    async fn put_if_none_match_on_existing_is_precondition_failed() {
        let book = InMemoryAddressBook::default();
        book.external_put("/a.vcf", "original");

        let err = book.put(&href("/a.vcf"), b"clobber", Precondition::IfNoneMatch).await.unwrap_err();
        assert!(is_precondition_failed(&err), "{err:?}");
        assert_eq!(book.card(&href("/a.vcf")).unwrap().1, b"original");
    }

    #[tokio::test]
    async fn put_if_match_with_stale_etag_is_precondition_failed() {
        let book = InMemoryAddressBook::default();
        let stale = book.external_put("/a.vcf", "v1");
        let current = book.external_put("/a.vcf", "v2 edited elsewhere");

        let err = book.put(&href("/a.vcf"), b"v3", Precondition::IfMatch(stale)).await.unwrap_err();
        assert!(is_precondition_failed(&err), "{err:?}");
        assert_eq!(book.card(&href("/a.vcf")), Some((current, b"v2 edited elsewhere".to_vec())));
    }

    #[tokio::test]
    async fn put_if_match_with_current_etag_updates() {
        let book = InMemoryAddressBook::default();
        let current = book.external_put("/a.vcf", "v1");

        let new_etag = book.put(&href("/a.vcf"), b"v2", Precondition::IfMatch(current.clone())).await.unwrap().unwrap();
        assert_ne!(new_etag, current);
        assert_eq!(book.card(&href("/a.vcf")), Some((new_etag, b"v2".to_vec())));
    }

    #[tokio::test]
    async fn omit_etag_on_put_returns_none_but_multiget_has_etag() {
        let book = InMemoryAddressBook::default();
        book.omit_etag_on_put(true);

        assert_eq!(book.put(&href("/a.vcf"), b"card-a", Precondition::IfNoneMatch).await.unwrap(), None);

        let fetched = book.multiget(&[href("/a.vcf")]).await.unwrap();
        let (stored_etag, _) = book.card(&href("/a.vcf")).unwrap();
        assert_eq!(fetched.found.len(), 1);
        assert_eq!(fetched.found[0].etag, stored_etag);
        assert_eq!(fetched.found[0].body, b"card-a");
    }

    #[tokio::test]
    async fn multiget_reports_missing_hrefs() {
        let book = InMemoryAddressBook::default();
        let etag = book.external_put("/a.vcf", "card-a");

        let result = book.multiget(&[href("/a.vcf"), href("/gone.vcf")]).await.unwrap();
        assert_eq!(
            result.found,
            [FetchedCard {
                href: href("/a.vcf"),
                etag,
                body: b"card-a".to_vec()
            }]
        );
        assert_eq!(result.missing, [href("/gone.vcf")]);
    }

    #[tokio::test]
    async fn changes_since_none_lists_every_card() {
        let book = InMemoryAddressBook::default();
        let b = book.external_put("/b.vcf", "card-b");
        let a = book.external_put("/a.vcf", "card-a");

        let delta = full_sync(&book).await;
        assert_eq!(delta.changed, [(href("/a.vcf"), a), (href("/b.vcf"), b)]);
        assert_eq!(delta.removed, []);
    }

    #[tokio::test]
    async fn changes_since_token_returns_only_later_changes() {
        let book = InMemoryAddressBook::default();
        book.external_put("/a.vcf", "a1");
        book.external_put("/c.vcf", "c1");
        let t0 = full_sync(&book).await.token;

        let b = book.external_put("/b.vcf", "b1");
        let a2 = book.external_put("/a.vcf", "a2");
        assert!(book.external_delete(&href("/c.vcf")));

        let delta = delta_since(&book, &t0).await;
        assert_eq!(delta.changed, [(href("/a.vcf"), a2), (href("/b.vcf"), b)]);
        assert_eq!(delta.removed, [href("/c.vcf")]);

        let quiet = delta_since(&book, &delta.token).await;
        assert!(quiet.changed.is_empty() && quiet.removed.is_empty(), "{quiet:?}");
    }

    #[tokio::test]
    async fn card_created_and_deleted_since_token_is_only_removed() {
        let book = InMemoryAddressBook::default();
        let t0 = full_sync(&book).await.token;

        book.external_put("/brief.vcf", "here and gone");
        book.external_delete(&href("/brief.vcf"));

        let delta = delta_since(&book, &t0).await;
        assert!(delta.changed.is_empty(), "{delta:?}");
        assert_eq!(delta.removed, [href("/brief.vcf")]);
    }

    #[tokio::test]
    async fn unknown_token_is_token_invalid() {
        let book = InMemoryAddressBook::default();
        book.external_put("/a.vcf", "card-a");

        for token in ["garbage", "mem-sync-999", ""] {
            assert_eq!(
                book.changes_since(Some(&SyncToken::from(token))).await.unwrap(),
                Changes::TokenInvalid,
                "{token:?}"
            );
        }
    }

    #[tokio::test]
    async fn expire_tokens_invalidates_old_tokens_only() {
        let book = InMemoryAddressBook::default();
        book.external_put("/a.vcf", "card-a");
        let old = full_sync(&book).await.token;

        book.expire_tokens();
        assert_eq!(book.changes_since(Some(&old)).await.unwrap(), Changes::TokenInvalid);

        let fresh = full_sync(&book).await;
        assert_eq!(fresh.changed.len(), 1, "full resync after expiry lists every card");
        let b = book.external_put("/b.vcf", "card-b");
        assert_eq!(delta_since(&book, &fresh.token).await.changed, [(href("/b.vcf"), b)]);
    }

    #[tokio::test]
    async fn delete_missing_resource_is_ok_and_recorded() {
        let book = InMemoryAddressBook::default();
        let stale = ETag::from("\"stale\"");

        book.delete(&href("/gone.vcf"), Some(&stale)).await.unwrap();
        assert_eq!(
            book.writes(),
            [Write::Delete {
                href: href("/gone.vcf"),
                if_match: Some(stale)
            }]
        );
    }

    #[tokio::test]
    async fn delete_respects_if_match() {
        let book = InMemoryAddressBook::default();
        let stale = book.external_put("/a.vcf", "v1");
        let current = book.external_put("/a.vcf", "v2");

        let err = book.delete(&href("/a.vcf"), Some(&stale)).await.unwrap_err();
        assert!(is_precondition_failed(&err), "{err:?}");
        assert!(book.card(&href("/a.vcf")).is_some());

        book.delete(&href("/a.vcf"), Some(&current)).await.unwrap();
        assert_eq!(book.card(&href("/a.vcf")), None);
    }

    #[tokio::test]
    async fn fail_next_fails_only_the_next_matching_op() {
        let book = InMemoryAddressBook::default();
        book.fail_next(
            Op::Put,
            AddressBookError::RateLimited {
                retry_after: Some(Duration::from_secs(30)),
            },
        );

        assert!(book.list_etags().await.unwrap().is_empty(), "other operations are unaffected");
        let err = book.put(&href("/a.vcf"), b"card-a", Precondition::IfNoneMatch).await.unwrap_err();
        assert!(
            matches!(err, Error::AddressBook(AddressBookError::RateLimited { retry_after: Some(d) }) if d == Duration::from_secs(30)),
            "{err:?}"
        );
        assert!(err.is_transient());
        assert_eq!(book.card(&href("/a.vcf")), None, "a failed put changes nothing");

        book.put(&href("/a.vcf"), b"card-a", Precondition::IfNoneMatch).await.unwrap();
        assert_eq!(book.writes().len(), 2, "both attempts are recorded, including the failed one");
    }

    #[tokio::test]
    async fn out_of_band_edits_are_not_recorded_as_writes() {
        let book = InMemoryAddressBook::default();
        book.external_put("/a.vcf", "card-a");
        book.external_delete(&href("/a.vcf"));
        assert_eq!(book.writes(), []);
    }

    #[tokio::test]
    async fn without_sync_collection_changes_since_fails_and_list_etags_works() {
        let book = InMemoryAddressBook::default();
        book.set_supports_sync_collection(false);
        let a = book.external_put("/a.vcf", "card-a");

        assert!(!book.discover().await.unwrap().supports_sync_collection);
        let err = book.changes_since(None).await.unwrap_err();
        assert!(matches!(err, Error::AddressBook(AddressBookError::Permanent(_))), "{err:?}");
        assert_eq!(book.list_etags().await.unwrap(), [(href("/a.vcf"), a)]);
    }
}
