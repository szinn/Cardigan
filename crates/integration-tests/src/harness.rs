//! Builds the system under test against one Radicale: two real CardDAV
//! adapters (wrapped in `FaultyBook`), a real SQLite state store in a temp
//! dir and a real `SyncService`. Control clients play the user.

use std::sync::Arc;

use cg_carddav::{CardDavAddressBook, CardDavConfig, ProviderQuirks};
use cg_core::{
    Error,
    addressbook::{AddressBook, Changes, Precondition},
    contact::{Href, Side},
    repository::{RepositoryService, read_only_transaction, transaction},
    service::{CycleMode, CycleOutcome, CycleRequest, SyncConfig, SyncService},
    state::{Conflict, ContactState, PendingRecreate},
};
use cg_database::{create_repository_service, open_database};
use chrono::TimeDelta;
use secrecy::SecretString;
use tempfile::TempDir;

use crate::{
    clock::SettableClock,
    faulty::{FaultyBook, Hook},
    radicale::{FASTMAIL, ICLOUD, PASSWORD, Radicale, USER},
};

const POLL: TimeDelta = TimeDelta::seconds(60);
const DEFAULT_BATCH: usize = 50;
const SYNC: CycleRequest = CycleRequest {
    mode: CycleMode::Sync,
    reset: false,
};

/// One card as the server holds it.
#[derive(Debug, Clone)]
pub(crate) struct Card {
    pub(crate) href: Href,
    pub(crate) body: String,
}

/// A synthetic, PII-free vCard 3.0; `extra` lines each end in `\r\n`.
pub(crate) fn vcard(uid: &str, name: &str, extra: &str) -> String {
    format!("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:{name}\r\nN:{name};;;;\r\n{extra}END:VCARD\r\n")
}

fn uid_of(body: &str) -> String {
    body.lines()
        .find_map(|line| line.strip_prefix("UID:"))
        .expect("every test card has a UID")
        .trim()
        .to_owned()
}

fn collection(side: Side) -> &'static str {
    match side {
        Side::ICloud => ICLOUD,
        Side::Fastmail => FASTMAIL,
    }
}

fn book(radicale: &Radicale, side: Side, batch: usize) -> CardDavAddressBook {
    let quirks = ProviderQuirks {
        default_collection: Some(collection(side).to_owned()),
        multiget_batch: batch,
    };
    CardDavAddressBook::new(CardDavConfig::new(
        radicale.base_url.clone(),
        USER,
        SecretString::from(PASSWORD.to_owned()),
        quirks,
    ))
    .expect("build CardDAV client")
}

/// Fetches `href`'s current ETag, then replaces it (`If-Match`).
async fn edit_card(book: Arc<CardDavAddressBook>, href: Href, body: String) {
    let current = book.multiget(std::slice::from_ref(&href)).await.expect("control multiget");
    let etag = current.found.into_iter().next().expect("card to edit exists").etag;
    book.put(&href, body.as_bytes(), Precondition::IfMatch(etag)).await.expect("control edit");
}

struct Engine {
    icloud: Arc<FaultyBook>,
    fastmail: Arc<FaultyBook>,
    repository_service: Arc<RepositoryService>,
    service: Arc<SyncService>,
}

async fn engine(radicale: &Radicale, db_url: &str, clock: &Arc<SettableClock>, batch: usize) -> Engine {
    let icloud = Arc::new(FaultyBook::new(Arc::new(book(radicale, Side::ICloud, batch))));
    let fastmail = Arc::new(FaultyBook::new(Arc::new(book(radicale, Side::Fastmail, batch))));
    let database = open_database(db_url).await.expect("open state database");
    let repository_service = create_repository_service(database).await.expect("migrate state database");
    let config = SyncConfig {
        winner: Side::ICloud,
        poll_interval: POLL,
    };
    let service = Arc::new(SyncService::new(
        icloud.clone(),
        fastmail.clone(),
        repository_service.clone(),
        config,
        clock.clone(),
    ));
    Engine {
        icloud,
        fastmail,
        repository_service,
        service,
    }
}

pub(crate) struct Harness {
    radicale: Radicale,
    _db_dir: TempDir,
    db_url: String,
    batch: usize,
    clock: Arc<SettableClock>,
    control_icloud: Arc<CardDavAddressBook>,
    control_fastmail: Arc<CardDavAddressBook>,
    pub(crate) icloud: Arc<FaultyBook>,
    pub(crate) fastmail: Arc<FaultyBook>,
    repository_service: Arc<RepositoryService>,
    service: Arc<SyncService>,
}

impl Harness {
    pub(crate) async fn start() -> Self {
        Self::start_with_batch(DEFAULT_BATCH).await
    }

    /// Like `start`, with at most `batch` hrefs per multiget REPORT.
    pub(crate) async fn start_with_batch(batch: usize) -> Self {
        let radicale = Radicale::start().await;
        let db_dir = tempfile::tempdir().expect("temp dir");
        let db_url = format!("sqlite://{}?mode=rwc", db_dir.path().join("cardigan.db").display());
        let clock = SettableClock::new();
        let control_icloud = Arc::new(book(&radicale, Side::ICloud, DEFAULT_BATCH));
        let control_fastmail = Arc::new(book(&radicale, Side::Fastmail, DEFAULT_BATCH));
        control_icloud.discover().await.expect("control discovery (icloud)");
        control_fastmail.discover().await.expect("control discovery (fastmail)");
        let Engine {
            icloud,
            fastmail,
            repository_service,
            service,
        } = engine(&radicale, &db_url, &clock, batch).await;
        Self {
            radicale,
            _db_dir: db_dir,
            db_url,
            batch,
            clock,
            control_icloud,
            control_fastmail,
            icloud,
            fastmail,
            repository_service,
            service,
        }
    }

    /// A new process on the same state file: new adapters (with fresh
    /// counters and no armed faults), a new database connection and a new
    /// `SyncService`.
    pub(crate) async fn restart(&mut self) {
        // The old connection may already be unusable after a crash; closing
        // is best effort.
        let _ = self.repository_service.repository().close().await;
        let Engine {
            icloud,
            fastmail,
            repository_service,
            service,
        } = engine(&self.radicale, &self.db_url, &self.clock, self.batch).await;
        self.icloud = icloud;
        self.fastmail = fastmail;
        self.repository_service = repository_service;
        self.service = service;
    }

    /// One sync cycle, an hour after the previous one, so any failure backoff
    /// is due.
    pub(crate) async fn cycle(&self) -> Result<CycleOutcome, Error> {
        self.clock.advance(TimeDelta::hours(1));
        self.service.run_cycle(SYNC).await
    }

    /// Runs cycles until one is `Idle`. The cycle after the engine's own
    /// writes may still be `Applied` with no writes, because the server
    /// reports those writes as changes.
    pub(crate) async fn settle(&self) {
        for _ in 0..6 {
            if matches!(self.cycle().await.expect("cycle while settling"), CycleOutcome::Idle) {
                return;
            }
        }
        panic!("did not settle to Idle within 6 cycles");
    }

    /// Runs a cycle that must panic (an armed `Fault::Crash`).
    pub(crate) async fn crash_cycle(&self) {
        self.clock.advance(TimeDelta::hours(1));
        let service = self.service.clone();
        let result = tokio::spawn(async move { service.run_cycle(SYNC).await }).await;
        assert!(
            result.expect_err("the cycle should crash").is_panic(),
            "the cycle should end in the simulated panic"
        );
    }

    fn control(&self, side: Side) -> &Arc<CardDavAddressBook> {
        match side {
            Side::ICloud => &self.control_icloud,
            Side::Fastmail => &self.control_fastmail,
        }
    }

    /// A user's new card at `/<user>/<collection>/<name>.vcf`.
    pub(crate) async fn put(&self, side: Side, name: &str, body: &str) -> Href {
        let href = Href::from(format!("/{USER}/{}/{name}.vcf", collection(side)));
        self.control(side)
            .put(&href, body.as_bytes(), Precondition::IfNoneMatch)
            .await
            .expect("control put");
        href
    }

    /// A user's edit of an existing card.
    pub(crate) async fn edit(&self, side: Side, href: &Href, body: &str) {
        edit_card(self.control(side).clone(), href.clone(), body.to_owned()).await;
    }

    /// The same edit, deferred: for `FaultyBook::before_next_put`.
    pub(crate) fn edit_hook(&self, side: Side, href: Href, body: String) -> Hook {
        let book = self.control(side).clone();
        Box::new(move || Box::pin(edit_card(book, href, body)))
    }

    /// A user's delete.
    pub(crate) async fn remove(&self, side: Side, href: &Href) {
        self.control(side).delete(href, None).await.expect("control delete");
    }

    /// Every card on `side`, sorted by href.
    pub(crate) async fn cards(&self, side: Side) -> Vec<Card> {
        let book = self.control(side);
        let Changes::Delta(listing) = book.changes_since(None).await.expect("control listing") else {
            panic!("a full listing never reports TokenInvalid");
        };
        let hrefs: Vec<Href> = listing.changed.into_iter().map(|(href, _)| href).collect();
        let mut found = book.multiget(&hrefs).await.expect("control multiget").found;
        found.sort_by(|a, b| a.href.as_str().cmp(b.href.as_str()));
        found
            .into_iter()
            .map(|card| Card {
                href: card.href,
                body: String::from_utf8(card.body).expect("test cards are UTF-8"),
            })
            .collect()
    }

    /// The UID of every card on `side`, sorted (a duplicate shows twice).
    pub(crate) async fn uids(&self, side: Side) -> Vec<String> {
        let mut uids: Vec<String> = self.cards(side).await.iter().map(|card| uid_of(&card.body)).collect();
        uids.sort();
        uids
    }

    pub(crate) async fn contacts(&self) -> Vec<ContactState> {
        let repository = self.repository_service.contact_state_repository().clone();
        read_only_transaction(&**self.repository_service.repository(), |tx| {
            Box::pin(async move { repository.list_all(tx).await })
        })
        .await
        .expect("list contact state")
    }

    pub(crate) async fn pending(&self) -> Vec<PendingRecreate> {
        let repository = self.repository_service.pending_recreate_repository().clone();
        read_only_transaction(&**self.repository_service.repository(), |tx| {
            Box::pin(async move { repository.list_all(tx).await })
        })
        .await
        .expect("list pending recreates")
    }

    pub(crate) async fn conflicts(&self) -> Vec<Conflict> {
        let repository = self.repository_service.conflict_repository().clone();
        read_only_transaction(&**self.repository_service.repository(), |tx| {
            Box::pin(async move { repository.list_all(tx).await })
        })
        .await
        .expect("list conflicts")
    }

    /// The stored sync token for `side`.
    pub(crate) async fn token(&self, side: Side) -> Option<String> {
        let repository = self.repository_service.endpoint_repository().clone();
        read_only_transaction(&**self.repository_service.repository(), |tx| {
            Box::pin(async move { repository.find(tx, side).await })
        })
        .await
        .expect("read endpoint")
        .and_then(|endpoint| endpoint.sync_token)
    }

    /// Overwrites the stored sync token for `side`.
    pub(crate) async fn set_token(&self, side: Side, token: &str) {
        let repository = self.repository_service.endpoint_repository().clone();
        let token = token.to_owned();
        transaction(&**self.repository_service.repository(), |tx| {
            Box::pin(async move { repository.set_sync_token(tx, side, Some(token)).await })
        })
        .await
        .expect("set sync token");
    }
}
