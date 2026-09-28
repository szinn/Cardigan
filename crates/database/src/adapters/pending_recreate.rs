use cg_core::{
    Error,
    contact::{Href, Uid},
    repository::Transaction,
    state::{NewPendingRecreate, PendingRecreate, PendingRecreateRepository},
};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait,
    ActiveValue::{NotSet, Set},
    ColumnTrait, EntityTrait, QueryFilter, QueryOrder,
};

use crate::{
    entities::{pending_recreates, prelude},
    error::handle_dberr,
    transaction::TransactionImpl,
};

impl From<pending_recreates::Model> for PendingRecreate {
    fn from(model: pending_recreates::Model) -> Self {
        Self {
            id: model.id as u64,
            uid: Uid::from(model.uid),
            icloud_href: Href::from(model.icloud_href),
            old_fastmail_href: Href::from(model.old_fastmail_href),
            old_fastmail_uid: Uid::from(model.old_fastmail_uid),
            new_fastmail_href: Href::from(model.new_fastmail_href),
            card: model.card,
            created_at: model.created_at.with_timezone(&Utc),
        }
    }
}

pub(crate) struct PendingRecreateRepositoryAdapter;

impl PendingRecreateRepositoryAdapter {
    pub(crate) fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl PendingRecreateRepository for PendingRecreateRepositoryAdapter {
    async fn upsert(&self, transaction: &dyn Transaction, new: NewPendingRecreate) -> Result<PendingRecreate, Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        prelude::PendingRecreates::delete_many()
            .filter(pending_recreates::Column::Uid.eq(new.uid.as_str()))
            .exec(transaction)
            .await
            .map_err(handle_dberr)?;
        let model = pending_recreates::ActiveModel {
            id: NotSet,
            uid: Set(new.uid.into_string()),
            icloud_href: Set(new.icloud_href.into_string()),
            old_fastmail_href: Set(new.old_fastmail_href.into_string()),
            old_fastmail_uid: Set(new.old_fastmail_uid.into_string()),
            new_fastmail_href: Set(new.new_fastmail_href.into_string()),
            card: Set(new.card),
            created_at: Set(new.created_at.into()),
        }
        .insert(transaction)
        .await
        .map_err(handle_dberr)?;
        Ok(model.into())
    }

    async fn list_all(&self, transaction: &dyn Transaction) -> Result<Vec<PendingRecreate>, Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        Ok(prelude::PendingRecreates::find()
            .order_by_asc(pending_recreates::Column::Id)
            .all(transaction)
            .await
            .map_err(handle_dberr)?
            .into_iter()
            .map(PendingRecreate::from)
            .collect())
    }

    async fn delete(&self, transaction: &dyn Transaction, uid: &Uid) -> Result<bool, Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        let result = prelude::PendingRecreates::delete_many()
            .filter(pending_recreates::Column::Uid.eq(uid.as_str()))
            .exec(transaction)
            .await
            .map_err(handle_dberr)?;
        Ok(result.rows_affected > 0)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cg_core::{
        Error, RepositoryError,
        contact::{Href, Uid},
        repository::RepositoryService,
        state::NewPendingRecreate,
    };
    use chrono::{DateTime, TimeZone, Utc};
    use sea_orm::Database;

    use crate::create_repository_service;

    async fn setup() -> Arc<RepositoryService> {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        create_repository_service(db).await.unwrap()
    }

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_790_000_000 + secs, 0).unwrap()
    }

    /// CRLF, non-ASCII text and a byte that is not valid UTF-8: stored and
    /// returned verbatim, never parsed.
    fn card_bytes(label: &str) -> Vec<u8> {
        let mut bytes = format!("BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Zoë {label}\r\nNOTE:").into_bytes();
        bytes.push(0xFF);
        bytes.extend_from_slice(b"\r\nEND:VCARD\r\n");
        bytes
    }

    fn pending(uid: &str, label: &str, secs: i64) -> NewPendingRecreate {
        NewPendingRecreate {
            uid: Uid::from(uid),
            icloud_href: Href::from(format!("/i/{uid}.vcf")),
            old_fastmail_href: Href::from(format!("/dav/old-{uid}.vcf")),
            old_fastmail_uid: Uid::from(format!("fm-{uid}")),
            new_fastmail_href: Href::from(format!("/dav/{uid}.vcf")),
            card: card_bytes(label),
            created_at: at(secs),
        }
    }

    #[tokio::test]
    async fn upsert_and_list_round_trip_every_field() {
        let svc = setup().await;
        let repo = svc.pending_recreate_repository();
        let tx = svc.repository().begin().await.unwrap();

        let added = repo.upsert(&*tx, pending("u1", "one", 0)).await.unwrap();

        assert_eq!(repo.list_all(&*tx).await.unwrap().as_slice(), std::slice::from_ref(&added));
        assert_eq!(added.uid, Uid::from("u1"));
        assert_eq!(added.icloud_href, Href::from("/i/u1.vcf"));
        assert_eq!(added.old_fastmail_href, Href::from("/dav/old-u1.vcf"));
        assert_eq!(added.old_fastmail_uid, Uid::from("fm-u1"));
        assert_eq!(added.new_fastmail_href, Href::from("/dav/u1.vcf"));
        assert_eq!(added.card, card_bytes("one"));
        assert_eq!(added.created_at, at(0));
    }

    #[tokio::test]
    async fn upsert_replaces_the_row_for_the_same_uid() {
        let svc = setup().await;
        let repo = svc.pending_recreate_repository();
        let tx = svc.repository().begin().await.unwrap();
        repo.upsert(&*tx, pending("a", "first", 0)).await.unwrap();
        let other = repo.upsert(&*tx, pending("b", "other", 1)).await.unwrap();
        let again = repo.upsert(&*tx, pending("a", "second", 2)).await.unwrap();

        assert_eq!(repo.list_all(&*tx).await.unwrap(), [other, again.clone()]);
        assert_eq!(again.card, card_bytes("second"));
    }

    #[tokio::test]
    async fn list_all_is_oldest_first() {
        let svc = setup().await;
        let repo = svc.pending_recreate_repository();
        let tx = svc.repository().begin().await.unwrap();
        let first = repo.upsert(&*tx, pending("z", "first", 0)).await.unwrap();
        let second = repo.upsert(&*tx, pending("a", "second", 1)).await.unwrap();

        assert_eq!(repo.list_all(&*tx).await.unwrap(), [first, second]);
    }

    #[tokio::test]
    async fn delete_reports_whether_a_row_was_removed() {
        let svc = setup().await;
        let repo = svc.pending_recreate_repository();
        let tx = svc.repository().begin().await.unwrap();
        repo.upsert(&*tx, pending("u1", "one", 0)).await.unwrap();

        assert!(repo.delete(&*tx, &Uid::from("u1")).await.unwrap());
        assert!(!repo.delete(&*tx, &Uid::from("u1")).await.unwrap());
        assert_eq!(repo.list_all(&*tx).await.unwrap(), []);
    }

    #[tokio::test]
    async fn rolled_back_upsert_leaves_no_row() {
        let svc = setup().await;
        let repo = svc.pending_recreate_repository();
        let tx = svc.repository().begin().await.unwrap();
        repo.upsert(&*tx, pending("u1", "one", 0)).await.unwrap();
        tx.rollback().await.unwrap();

        let tx = svc.repository().begin().await.unwrap();
        assert_eq!(repo.list_all(&*tx).await.unwrap(), []);
    }

    #[tokio::test]
    async fn upsert_in_read_only_transaction_is_read_only() {
        let svc = setup().await;
        let tx = svc.repository().begin_read_only().await.unwrap();
        let err = svc.pending_recreate_repository().upsert(&*tx, pending("u1", "one", 0)).await.unwrap_err();
        assert!(matches!(err, Error::RepositoryError(RepositoryError::ReadOnly)), "{err:?}");
    }
}
