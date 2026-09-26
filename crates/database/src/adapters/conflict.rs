use cg_core::{
    Error, RepositoryError,
    contact::{Side, Uid},
    repository::Transaction,
    state::{Conflict, ConflictOrigin, ConflictRepository, NewConflict},
};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait,
    ActiveValue::{NotSet, Set},
    ColumnTrait, EntityTrait, QueryFilter, QueryOrder,
};

use crate::{
    entities::{conflicts, prelude},
    error::handle_dberr,
    transaction::TransactionImpl,
};

impl TryFrom<conflicts::Model> for Conflict {
    type Error = Error;

    fn try_from(model: conflicts::Model) -> Result<Self, Error> {
        let id = model.id;
        let corrupt = |column: &str| Error::RepositoryError(RepositoryError::Database(format!("conflicts row {id}: invalid {column}")));
        Ok(Self {
            id: model.id as u64,
            uid: Uid::from(model.uid),
            origin: model.origin.parse::<ConflictOrigin>().map_err(|_| corrupt("origin"))?,
            winner: model.winner.parse::<Side>().map_err(|_| corrupt("winner"))?,
            icloud_vcard: model.icloud_vcard,
            fastmail_vcard: model.fastmail_vcard,
            detected_at: model.detected_at.with_timezone(&Utc),
        })
    }
}

pub(crate) struct ConflictRepositoryAdapter;

impl ConflictRepositoryAdapter {
    pub(crate) fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl ConflictRepository for ConflictRepositoryAdapter {
    async fn add(&self, transaction: &dyn Transaction, new: NewConflict) -> Result<Conflict, Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        conflicts::ActiveModel {
            id: NotSet,
            uid: Set(new.uid.into_string()),
            origin: Set(new.origin.as_str().to_owned()),
            winner: Set(new.winner.as_str().to_owned()),
            icloud_vcard: Set(new.icloud_vcard),
            fastmail_vcard: Set(new.fastmail_vcard),
            detected_at: Set(new.detected_at.into()),
        }
        .insert(transaction)
        .await
        .map_err(handle_dberr)?
        .try_into()
    }

    async fn list_all(&self, transaction: &dyn Transaction) -> Result<Vec<Conflict>, Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        prelude::Conflicts::find()
            .order_by_desc(conflicts::Column::Id)
            .all(transaction)
            .await
            .map_err(handle_dberr)?
            .into_iter()
            .map(Conflict::try_from)
            .collect()
    }

    async fn list_for_uid(&self, transaction: &dyn Transaction, uid: &Uid) -> Result<Vec<Conflict>, Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        prelude::Conflicts::find()
            .filter(conflicts::Column::Uid.eq(uid.as_str()))
            .order_by_desc(conflicts::Column::Id)
            .all(transaction)
            .await
            .map_err(handle_dberr)?
            .into_iter()
            .map(Conflict::try_from)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cg_core::{
        Error, RepositoryError,
        contact::{Side, Uid},
        repository::RepositoryService,
        state::{ConflictOrigin, NewConflict},
    };
    use chrono::{DateTime, TimeZone, Utc};
    use sea_orm::{ActiveModelTrait, ActiveValue::Set, Database};

    use crate::{create_repository_service, entities::conflicts, transaction::TransactionImpl};

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

    fn new_conflict(uid: &str, secs: i64) -> NewConflict {
        NewConflict {
            uid: Uid::from(uid),
            origin: ConflictOrigin::Sync,
            winner: Side::ICloud,
            icloud_vcard: card_bytes("icloud"),
            fastmail_vcard: card_bytes("fastmail"),
            detected_at: at(secs),
        }
    }

    #[tokio::test]
    async fn add_and_list_round_trip_bytes() {
        let svc = setup().await;
        let repo = svc.conflict_repository();
        let tx = svc.repository().begin().await.unwrap();

        let mut new = new_conflict("u1", 0);
        new.origin = ConflictOrigin::Baseline;
        new.winner = Side::Fastmail;
        let added = repo.add(&*tx, new).await.unwrap();

        assert_eq!(repo.list_all(&*tx).await.unwrap().as_slice(), std::slice::from_ref(&added));
        assert_eq!(added.icloud_vcard, card_bytes("icloud"));
        assert_eq!(added.fastmail_vcard, card_bytes("fastmail"));
        assert_eq!(added.origin, ConflictOrigin::Baseline);
        assert_eq!(added.winner, Side::Fastmail);
        assert_eq!(added.detected_at, at(0));
    }

    #[tokio::test]
    async fn list_all_is_newest_first() {
        let svc = setup().await;
        let repo = svc.conflict_repository();
        let tx = svc.repository().begin().await.unwrap();
        let first = repo.add(&*tx, new_conflict("a", 0)).await.unwrap();
        let second = repo.add(&*tx, new_conflict("b", 1)).await.unwrap();

        assert_eq!(repo.list_all(&*tx).await.unwrap(), [second, first]);
    }

    #[tokio::test]
    async fn list_for_uid_returns_every_conflict_for_that_contact() {
        let svc = setup().await;
        let repo = svc.conflict_repository();
        let tx = svc.repository().begin().await.unwrap();
        let first = repo.add(&*tx, new_conflict("u1", 0)).await.unwrap();
        repo.add(&*tx, new_conflict("u2", 1)).await.unwrap();
        let again = repo.add(&*tx, new_conflict("u1", 2)).await.unwrap();

        assert_eq!(repo.list_for_uid(&*tx, &Uid::from("u1")).await.unwrap(), [again, first]);
        assert_eq!(repo.list_for_uid(&*tx, &Uid::from("none")).await.unwrap(), []);
    }

    #[tokio::test]
    async fn add_in_read_only_transaction_is_read_only() {
        let svc = setup().await;
        let tx = svc.repository().begin_read_only().await.unwrap();
        let err = svc.conflict_repository().add(&*tx, new_conflict("u1", 0)).await.unwrap_err();
        assert!(matches!(err, Error::RepositoryError(RepositoryError::ReadOnly)), "{err:?}");
    }

    fn raw_conflict_row() -> conflicts::ActiveModel {
        conflicts::ActiveModel {
            uid: Set("u1".to_owned()),
            origin: Set("sync".to_owned()),
            winner: Set("icloud".to_owned()),
            icloud_vcard: Set(card_bytes("icloud")),
            fastmail_vcard: Set(card_bytes("fastmail")),
            detected_at: Set(at(0).into()),
            ..Default::default()
        }
    }

    async fn assert_corrupt_row_is_database_error(bad: conflicts::ActiveModel, column: &str) {
        let svc = setup().await;
        let tx = svc.repository().begin().await.unwrap();
        bad.insert(TransactionImpl::get_db_transaction(&*tx).unwrap()).await.unwrap();

        let err = svc.conflict_repository().list_all(&*tx).await.unwrap_err();
        let Error::RepositoryError(RepositoryError::Database(message)) = err else {
            panic!("expected Database error, got {err:?}");
        };
        assert!(message.contains("conflicts") && message.contains(column), "{message}");
        assert!(!message.contains("Zoë"), "no card content in errors: {message}");
    }

    #[tokio::test]
    async fn corrupt_winner_is_database_error() {
        let mut bad = raw_conflict_row();
        bad.winner = Set("google".to_owned());
        assert_corrupt_row_is_database_error(bad, "winner").await;
    }

    #[tokio::test]
    async fn corrupt_origin_is_database_error() {
        let mut bad = raw_conflict_row();
        bad.origin = Set("merge".to_owned());
        assert_corrupt_row_is_database_error(bad, "origin").await;
    }
}
