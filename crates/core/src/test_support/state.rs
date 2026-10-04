//! An in-memory state store implementing every sync-state repository, for
//! driving `SyncService` without a database.

use std::{
    any::Any,
    sync::{Arc, Mutex, MutexGuard},
};

use chrono::{DateTime, Utc};

use crate::{
    Error, RepositoryError,
    contact::{Href, Side, Uid},
    repository::{Repository, RepositoryService, RepositoryServiceBuilder, Transaction},
    state::{
        BackoffPolicy, BaselineSkip, BaselineSkipRepository, CardFailure, CardFailureRepository, Conflict, ConflictRepository, ContactState,
        ContactStateRepository, Endpoint, EndpointRepository, FailedCard, NewBaselineSkip, NewConflict, NewContactState, NewPendingRecreate, PendingRecreate,
        PendingRecreateRepository,
    },
};

/// Timestamp the fake stamps on the row bookkeeping it owns (`created_at`,
/// `updated_at`).
const STAMP: DateTime<Utc> = DateTime::<Utc>::UNIX_EPOCH;
const POISONED: &str = "InMemoryState lock poisoned";

#[derive(Debug, Clone, Default)]
struct Tables {
    contacts: Vec<ContactState>,
    endpoints: Vec<Endpoint>,
    conflicts: Vec<Conflict>,
    failures: Vec<CardFailure>,
    skips: Vec<BaselineSkip>,
    pending: Vec<PendingRecreate>,
    next_id: u64,
}

impl Tables {
    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
}

/// Every sync-state repository over shared in-memory tables.
///
/// A read-write transaction snapshots the tables and restores them on
/// rollback. A read-only one rejects writes with `RepositoryError::ReadOnly`,
/// like the SQLite adapter. `fail_next_write` fails the next write with
/// `RepositoryError::Database`, to simulate a state write failing after a
/// server write. Row timestamps the store owns are `UNIX_EPOCH`.
#[derive(Default)]
pub struct InMemoryState {
    tables: Arc<Mutex<Tables>>,
    fail_next_write: Mutex<bool>,
}

struct MemoryTransaction {
    tables: Arc<Mutex<Tables>>,
    /// The tables as they were at `begin`; `None` for a read-only
    /// transaction.
    undo: Option<Tables>,
}

#[async_trait::async_trait]
impl Transaction for MemoryTransaction {
    fn as_any(&self) -> &dyn Any {
        self
    }

    async fn commit(self: Box<Self>) -> Result<(), Error> {
        Ok(())
    }

    async fn rollback(self: Box<Self>) -> Result<(), Error> {
        let this = *self;
        if let Some(undo) = this.undo {
            *this.tables.lock().expect(POISONED) = undo;
        }
        Ok(())
    }
}

impl InMemoryState {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// A `RepositoryService` whose every repository is this store.
    #[must_use]
    pub fn repository_service(self: &Arc<Self>) -> Arc<RepositoryService> {
        Arc::new(
            RepositoryServiceBuilder::default()
                .repository(self.clone())
                .contact_state_repository(self.clone())
                .endpoint_repository(self.clone())
                .conflict_repository(self.clone())
                .card_failure_repository(self.clone())
                .baseline_skip_repository(self.clone())
                .pending_recreate_repository(self.clone())
                .build()
                .expect("every repository is set"),
        )
    }

    /// Fails the next write, in any repository, with
    /// `RepositoryError::Database`.
    pub fn fail_next_write(&self) {
        *self.fail_next_write.lock().expect(POISONED) = true;
    }

    /// Resets every row's photo state to untracked, as migration 000007 does
    /// on upgrade.
    pub fn untrack_photos(&self) {
        for row in &mut self.tables().contacts {
            row.photo = crate::state::PhotoState::default();
        }
    }

    #[must_use]
    pub fn contacts(&self) -> Vec<ContactState> {
        self.tables().contacts.clone()
    }

    #[must_use]
    pub fn endpoints(&self) -> Vec<Endpoint> {
        self.tables().endpoints.clone()
    }

    /// Every conflict, oldest first.
    #[must_use]
    pub fn conflicts(&self) -> Vec<Conflict> {
        self.tables().conflicts.clone()
    }

    #[must_use]
    pub fn failures(&self) -> Vec<CardFailure> {
        self.tables().failures.clone()
    }

    #[must_use]
    pub fn skips(&self) -> Vec<BaselineSkip> {
        self.tables().skips.clone()
    }

    #[must_use]
    pub fn pending_recreates(&self) -> Vec<PendingRecreate> {
        self.tables().pending.clone()
    }

    fn tables(&self) -> MutexGuard<'_, Tables> {
        self.tables.lock().expect(POISONED)
    }

    fn transaction(tx: &dyn Transaction) -> Result<&MemoryTransaction, Error> {
        tx.as_any().downcast_ref::<MemoryTransaction>().ok_or(Error::InvalidTransactionType)
    }

    fn read(&self, tx: &dyn Transaction) -> Result<MutexGuard<'_, Tables>, Error> {
        Self::transaction(tx)?;
        Ok(self.tables())
    }

    fn write(&self, tx: &dyn Transaction) -> Result<MutexGuard<'_, Tables>, Error> {
        if Self::transaction(tx)?.undo.is_none() {
            return Err(RepositoryError::ReadOnly.into());
        }
        if std::mem::take(&mut *self.fail_next_write.lock().expect(POISONED)) {
            return Err(RepositoryError::Database("injected write failure".to_owned()).into());
        }
        Ok(self.tables())
    }
}

#[async_trait::async_trait]
impl Repository for InMemoryState {
    async fn begin(&self) -> Result<Box<dyn Transaction>, Error> {
        let undo = self.tables().clone();
        Ok(Box::new(MemoryTransaction {
            tables: self.tables.clone(),
            undo: Some(undo),
        }))
    }

    async fn begin_read_only(&self) -> Result<Box<dyn Transaction>, Error> {
        Ok(Box::new(MemoryTransaction {
            tables: self.tables.clone(),
            undo: None,
        }))
    }

    async fn close(&self) -> Result<(), Error> {
        Ok(())
    }

    async fn ping(&self) -> Result<(), Error> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl ContactStateRepository for InMemoryState {
    async fn list_all(&self, transaction: &dyn Transaction) -> Result<Vec<ContactState>, Error> {
        Ok(self.read(transaction)?.contacts.clone())
    }

    async fn find_by_uid(&self, transaction: &dyn Transaction, uid: &Uid) -> Result<Option<ContactState>, Error> {
        Ok(self.read(transaction)?.contacts.iter().find(|row| &row.uid == uid).cloned())
    }

    async fn add(&self, transaction: &dyn Transaction, new: NewContactState) -> Result<ContactState, Error> {
        let mut tables = self.write(transaction)?;
        if tables
            .contacts
            .iter()
            .any(|row| row.uid == new.uid || row.icloud.href == new.icloud.href || row.fastmail.href == new.fastmail.href)
        {
            return Err(RepositoryError::Constraint(format!("duplicate contact uid={}", new.uid)).into());
        }
        let row = ContactState {
            id: tables.next_id(),
            version: 1,
            uid: new.uid,
            icloud: new.icloud,
            fastmail: new.fastmail,
            content_hash: new.content_hash,
            hash_version: new.hash_version,
            photo: new.photo,
            last_synced_vcard: new.last_synced_vcard,
            last_synced_at: new.last_synced_at,
            created_at: STAMP,
            updated_at: STAMP,
        };
        tables.contacts.push(row.clone());
        Ok(row)
    }

    async fn update(&self, transaction: &dyn Transaction, mut state: ContactState) -> Result<ContactState, Error> {
        let mut tables = self.write(transaction)?;
        let row = tables.contacts.iter_mut().find(|row| row.id == state.id).ok_or(RepositoryError::NotFound)?;
        if row.version != state.version {
            return Err(RepositoryError::Conflict.into());
        }
        state.icloud.last_seen_at = state.icloud.last_seen_at.max(row.icloud.last_seen_at);
        state.fastmail.last_seen_at = state.fastmail.last_seen_at.max(row.fastmail.last_seen_at);
        state.version += 1;
        *row = state.clone();
        Ok(state)
    }

    async fn mark_seen(&self, transaction: &dyn Transaction, side: Side, uids: &[Uid], seen_at: DateTime<Utc>) -> Result<u64, Error> {
        let mut tables = self.write(transaction)?;
        let mut changed = 0;
        for row in tables.contacts.iter_mut().filter(|row| uids.contains(&row.uid)) {
            match side {
                Side::ICloud => row.icloud.last_seen_at = seen_at,
                Side::Fastmail => row.fastmail.last_seen_at = seen_at,
            }
            changed += 1;
        }
        Ok(changed)
    }

    async fn delete_by_uid(&self, transaction: &dyn Transaction, uid: &Uid) -> Result<(), Error> {
        let mut tables = self.write(transaction)?;
        let before = tables.contacts.len();
        tables.contacts.retain(|row| &row.uid != uid);
        if tables.contacts.len() == before {
            return Err(RepositoryError::NotFound.into());
        }
        Ok(())
    }

    async fn delete_all(&self, transaction: &dyn Transaction) -> Result<u64, Error> {
        let mut tables = self.write(transaction)?;
        Ok(tables.contacts.drain(..).count() as u64)
    }
}

#[async_trait::async_trait]
impl EndpointRepository for InMemoryState {
    async fn find(&self, transaction: &dyn Transaction, side: Side) -> Result<Option<Endpoint>, Error> {
        Ok(self.read(transaction)?.endpoints.iter().find(|endpoint| endpoint.side == side).cloned())
    }

    async fn upsert_discovery(&self, transaction: &dyn Transaction, side: Side, addressbook_url: &str, discovered_host: &str) -> Result<Endpoint, Error> {
        let mut tables = self.write(transaction)?;
        if let Some(endpoint) = tables.endpoints.iter_mut().find(|endpoint| endpoint.side == side) {
            if endpoint.addressbook_url != addressbook_url {
                endpoint.sync_token = None;
            }
            addressbook_url.clone_into(&mut endpoint.addressbook_url);
            discovered_host.clone_into(&mut endpoint.discovered_host);
            endpoint.updated_at = STAMP;
            return Ok(endpoint.clone());
        }
        let endpoint = Endpoint {
            side,
            addressbook_url: addressbook_url.to_owned(),
            discovered_host: discovered_host.to_owned(),
            sync_token: None,
            updated_at: STAMP,
        };
        tables.endpoints.push(endpoint.clone());
        Ok(endpoint)
    }

    async fn set_sync_token(&self, transaction: &dyn Transaction, side: Side, sync_token: Option<String>) -> Result<(), Error> {
        let mut tables = self.write(transaction)?;
        let endpoint = tables
            .endpoints
            .iter_mut()
            .find(|endpoint| endpoint.side == side)
            .ok_or(RepositoryError::NotFound)?;
        endpoint.sync_token = sync_token;
        Ok(())
    }

    async fn delete_all(&self, transaction: &dyn Transaction) -> Result<u64, Error> {
        let mut tables = self.write(transaction)?;
        Ok(tables.endpoints.drain(..).count() as u64)
    }
}

#[async_trait::async_trait]
impl ConflictRepository for InMemoryState {
    async fn add(&self, transaction: &dyn Transaction, new: NewConflict) -> Result<Conflict, Error> {
        let mut tables = self.write(transaction)?;
        let row = Conflict {
            id: tables.next_id(),
            uid: new.uid,
            origin: new.origin,
            winner: new.winner,
            icloud_vcard: new.icloud_vcard,
            fastmail_vcard: new.fastmail_vcard,
            detected_at: new.detected_at,
        };
        tables.conflicts.push(row.clone());
        Ok(row)
    }

    async fn list_all(&self, transaction: &dyn Transaction) -> Result<Vec<Conflict>, Error> {
        Ok(self.read(transaction)?.conflicts.iter().rev().cloned().collect())
    }

    async fn list_for_uid(&self, transaction: &dyn Transaction, uid: &Uid) -> Result<Vec<Conflict>, Error> {
        Ok(self.read(transaction)?.conflicts.iter().rev().filter(|row| &row.uid == uid).cloned().collect())
    }
}

#[async_trait::async_trait]
impl PendingRecreateRepository for InMemoryState {
    async fn upsert(&self, transaction: &dyn Transaction, new: NewPendingRecreate) -> Result<PendingRecreate, Error> {
        let mut tables = self.write(transaction)?;
        let row = PendingRecreate {
            id: tables.next_id(),
            uid: new.uid,
            icloud_href: new.icloud_href,
            old_fastmail_href: new.old_fastmail_href,
            old_fastmail_uid: new.old_fastmail_uid,
            new_fastmail_href: new.new_fastmail_href,
            card: new.card,
            created_at: new.created_at,
        };
        tables.pending.retain(|pending| pending.uid != row.uid);
        tables.pending.push(row.clone());
        Ok(row)
    }

    async fn list_all(&self, transaction: &dyn Transaction) -> Result<Vec<PendingRecreate>, Error> {
        Ok(self.read(transaction)?.pending.clone())
    }

    async fn delete(&self, transaction: &dyn Transaction, uid: &Uid) -> Result<bool, Error> {
        let mut tables = self.write(transaction)?;
        let before = tables.pending.len();
        tables.pending.retain(|pending| &pending.uid != uid);
        Ok(tables.pending.len() != before)
    }
}

#[async_trait::async_trait]
impl CardFailureRepository for InMemoryState {
    async fn record_failure(
        &self,
        transaction: &dyn Transaction,
        failed: FailedCard,
        now: DateTime<Utc>,
        policy: &BackoffPolicy,
    ) -> Result<CardFailure, Error> {
        let mut tables = self.write(transaction)?;
        if let Some(row) = tables.failures.iter_mut().find(|row| row.side == failed.side && row.href == failed.href) {
            if row.etag == failed.etag {
                row.attempts = row.attempts.saturating_add(1);
            } else {
                row.attempts = 1;
                row.first_failed_at = now;
            }
            row.version += 1;
            row.uid = failed.uid;
            row.op = failed.op;
            row.etag = failed.etag;
            row.reason = failed.reason;
            row.last_failed_at = now;
            row.next_retry_at = now + policy.delay(row.attempts);
            return Ok(row.clone());
        }
        let row = CardFailure {
            id: tables.next_id(),
            version: 1,
            side: failed.side,
            href: failed.href,
            uid: failed.uid,
            op: failed.op,
            etag: failed.etag,
            reason: failed.reason,
            attempts: 1,
            first_failed_at: now,
            last_failed_at: now,
            next_retry_at: now + policy.delay(1),
        };
        tables.failures.push(row.clone());
        Ok(row)
    }

    async fn find(&self, transaction: &dyn Transaction, side: Side, href: &Href) -> Result<Option<CardFailure>, Error> {
        Ok(self
            .read(transaction)?
            .failures
            .iter()
            .find(|row| row.side == side && &row.href == href)
            .cloned())
    }

    async fn list_all(&self, transaction: &dyn Transaction) -> Result<Vec<CardFailure>, Error> {
        Ok(self.read(transaction)?.failures.clone())
    }

    async fn clear(&self, transaction: &dyn Transaction, side: Side, href: &Href) -> Result<bool, Error> {
        let mut tables = self.write(transaction)?;
        let before = tables.failures.len();
        tables.failures.retain(|row| !(row.side == side && &row.href == href));
        Ok(tables.failures.len() != before)
    }

    async fn delete_all(&self, transaction: &dyn Transaction) -> Result<u64, Error> {
        let mut tables = self.write(transaction)?;
        Ok(tables.failures.drain(..).count() as u64)
    }
}

fn skip_row(id: u64, version: u64, skip: NewBaselineSkip) -> BaselineSkip {
    BaselineSkip {
        id,
        version,
        side: skip.side,
        href: skip.href,
        uid: skip.uid,
        content_hash: skip.content_hash,
        hash_version: skip.hash_version,
        candidate_count: skip.candidate_count,
        skipped_at: skip.skipped_at,
    }
}

#[async_trait::async_trait]
impl BaselineSkipRepository for InMemoryState {
    async fn replace_all(&self, transaction: &dyn Transaction, skips: Vec<NewBaselineSkip>) -> Result<u64, Error> {
        let mut tables = self.write(transaction)?;
        tables.skips.clear();
        for skip in skips {
            let id = tables.next_id();
            tables.skips.push(skip_row(id, 1, skip));
        }
        Ok(tables.skips.len() as u64)
    }

    async fn upsert(&self, transaction: &dyn Transaction, skip: NewBaselineSkip) -> Result<BaselineSkip, Error> {
        let mut tables = self.write(transaction)?;
        if let Some(row) = tables.skips.iter_mut().find(|row| row.side == skip.side && row.href == skip.href) {
            *row = skip_row(row.id, row.version + 1, skip);
            return Ok(row.clone());
        }
        let id = tables.next_id();
        let row = skip_row(id, 1, skip);
        tables.skips.push(row.clone());
        Ok(row)
    }

    async fn list_all(&self, transaction: &dyn Transaction) -> Result<Vec<BaselineSkip>, Error> {
        Ok(self.read(transaction)?.skips.clone())
    }

    async fn delete(&self, transaction: &dyn Transaction, side: Side, href: &Href) -> Result<bool, Error> {
        let mut tables = self.write(transaction)?;
        let before = tables.skips.len();
        tables.skips.retain(|row| !(row.side == side && &row.href == href));
        Ok(tables.skips.len() != before)
    }

    async fn delete_all(&self, transaction: &dyn Transaction) -> Result<u64, Error> {
        let mut tables = self.write(transaction)?;
        Ok(tables.skips.drain(..).count() as u64)
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;

    use super::*;
    use crate::{
        contact::{CANONICAL_VERSION, ETag, VCard},
        repository::{read_only_transaction, transaction},
        state::{ConflictOrigin, FailureOp, FailureReason, PhotoState, SideState},
        sync::SYNC_HASH,
    };

    fn new_contact(uid: &str) -> NewContactState {
        let card = VCard::parse(format!("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:Jane Doe\r\nEND:VCARD\r\n")).expect("card parses");
        let side = |href: String| SideState {
            href: Href::from(href),
            etag: ETag::from("e1"),
            last_seen_at: STAMP,
        };
        NewContactState {
            uid: Uid::from(uid),
            icloud: side(format!("/i/{uid}.vcf")),
            fastmail: side(format!("/f/{uid}.vcf")),
            content_hash: card.canonical_hash(SYNC_HASH),
            hash_version: CANONICAL_VERSION,
            photo: PhotoState::default(),
            last_synced_vcard: card,
            last_synced_at: STAMP,
        }
    }

    #[tokio::test]
    async fn rollback_restores_the_tables() {
        let state = InMemoryState::new();
        let store = state.clone();

        let result: Result<(), Error> = transaction(&*state, |tx| {
            Box::pin(async move {
                ContactStateRepository::add(&*store, tx, new_contact("u1")).await?;
                Err(Error::Infrastructure("boom".to_owned()))
            })
        })
        .await;

        result.unwrap_err();
        assert!(state.contacts().is_empty(), "the add was rolled back");
    }

    #[tokio::test]
    async fn read_only_transactions_reject_writes() {
        let state = InMemoryState::new();
        let store = state.clone();

        let error = read_only_transaction(&*state, |tx| {
            Box::pin(async move { ContactStateRepository::add(&*store, tx, new_contact("u1")).await })
        })
        .await
        .unwrap_err();

        assert!(matches!(error, Error::RepositoryError(RepositoryError::ReadOnly)), "{error:?}");
    }

    #[tokio::test]
    async fn update_rejects_a_stale_version_and_fail_next_write_fails_once() {
        let state = InMemoryState::new();
        let store = state.clone();
        let row = transaction(&*state, |tx| {
            Box::pin(async move { ContactStateRepository::add(&*store, tx, new_contact("u1")).await })
        })
        .await
        .unwrap();

        let store = state.clone();
        let stale_row = row.clone();
        let error = transaction(&*state, |tx| {
            Box::pin(async move {
                ContactStateRepository::update(&*store, tx, row).await?;
                ContactStateRepository::update(&*store, tx, stale_row).await
            })
        })
        .await
        .unwrap_err();
        assert!(matches!(error, Error::RepositoryError(RepositoryError::Conflict)), "{error:?}");

        state.fail_next_write();
        let store = state.clone();
        let first = transaction(&*state, |tx| {
            Box::pin(async move { ContactStateRepository::add(&*store, tx, new_contact("u2")).await })
        })
        .await;
        assert!(matches!(first, Err(Error::RepositoryError(RepositoryError::Database(_)))), "{first:?}");
        let store = state.clone();
        transaction(&*state, |tx| {
            Box::pin(async move { ContactStateRepository::add(&*store, tx, new_contact("u2")).await })
        })
        .await
        .expect("only the next write fails");
    }

    fn pending(uid: &str, card: &str) -> NewPendingRecreate {
        NewPendingRecreate {
            uid: Uid::from(uid),
            icloud_href: Href::from(format!("/i/{uid}.vcf")),
            old_fastmail_href: Href::from(format!("/dav/old-{uid}.vcf")),
            old_fastmail_uid: Uid::from(format!("fm-{uid}")),
            new_fastmail_href: Href::from(format!("/dav/{uid}.vcf")),
            card: card.as_bytes().to_vec(),
            created_at: DateTime::<Utc>::UNIX_EPOCH,
        }
    }

    #[tokio::test]
    async fn upsert_replaces_the_row_for_the_same_uid() {
        let state = InMemoryState::new();
        let service = state.repository_service();
        let repo = service.pending_recreate_repository().clone();
        crate::repository::transaction(&**service.repository(), |tx| {
            Box::pin(async move {
                repo.upsert(tx, pending("a", "first")).await?;
                repo.upsert(tx, pending("b", "other")).await?;
                repo.upsert(tx, pending("a", "second")).await?;
                Ok(())
            })
        })
        .await
        .unwrap();

        let rows = state.pending_recreates();
        let summary: Vec<(&str, &[u8])> = rows.iter().map(|row| (row.uid.as_str(), row.card.as_slice())).collect();
        assert_eq!(summary, [("b", b"other".as_slice()), ("a", b"second".as_slice())]);
    }

    #[tokio::test]
    async fn delete_reports_whether_a_row_was_removed() {
        let state = InMemoryState::new();
        let service = state.repository_service();
        let repo = service.pending_recreate_repository().clone();
        let removed = crate::repository::transaction(&**service.repository(), |tx| {
            Box::pin(async move {
                repo.upsert(tx, pending("a", "card")).await?;
                Ok((repo.delete(tx, &Uid::from("a")).await?, repo.delete(tx, &Uid::from("a")).await?))
            })
        })
        .await
        .unwrap();

        assert_eq!(removed, (true, false));
        assert_eq!(state.pending_recreates(), []);
    }

    #[tokio::test]
    async fn record_failure_counts_attempts_and_restarts_on_a_new_etag() {
        let state = InMemoryState::new();
        let policy = BackoffPolicy {
            base: TimeDelta::seconds(60),
            cap: TimeDelta::hours(24),
        };
        let failed = |etag: &str| FailedCard {
            side: Side::ICloud,
            href: Href::from("/i/a.vcf"),
            uid: None,
            op: FailureOp::Read,
            etag: Some(ETag::from(etag)),
            reason: FailureReason::InvalidCard,
        };
        let record = |card: FailedCard| {
            let store = state.clone();
            async move {
                let repository = store.clone();
                transaction(&*repository, |tx| {
                    Box::pin(async move { CardFailureRepository::record_failure(&*store, tx, card, STAMP, &policy).await })
                })
                .await
                .unwrap()
            }
        };

        assert_eq!(record(failed("e1")).await.attempts, 1);
        let second = record(failed("e1")).await;
        assert_eq!((second.attempts, second.next_retry_at), (2, STAMP + TimeDelta::seconds(120)));
        assert_eq!(record(failed("e2")).await.attempts, 1, "an edited card starts over");
        assert_eq!(state.failures().len(), 1);

        let conflict = NewConflict {
            uid: Uid::from("u1"),
            origin: ConflictOrigin::Sync,
            winner: Side::ICloud,
            icloud_vcard: Vec::new(),
            fastmail_vcard: Vec::new(),
            detected_at: STAMP,
        };
        let store = state.clone();
        transaction(&*state, |tx| Box::pin(async move { ConflictRepository::add(&*store, tx, conflict).await }))
            .await
            .unwrap();
        assert_eq!(state.conflicts().len(), 1);
    }
}
