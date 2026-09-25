use cg_core::{
    Error, RepositoryError,
    contact::{CardHash, ETag, Href, Side, Uid, VCard},
    repository::Transaction,
    state::{ContactState, ContactStateRepository, NewContactState, SideState},
};
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait,
    ActiveValue::{NotSet, Set},
    ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
    prelude::DateTimeWithTimeZone,
    sea_query::Expr,
};

use crate::{
    entities::{contacts, prelude},
    error::handle_dberr,
    transaction::TransactionImpl,
};

/// Keeps `IN (...)` lists well under SQLite's bound-parameter limit.
const MARK_SEEN_CHUNK: usize = 500;

impl TryFrom<contacts::Model> for ContactState {
    type Error = Error;

    fn try_from(model: contacts::Model) -> Result<Self, Error> {
        let id = model.id;
        let corrupt = |column: &str| Error::RepositoryError(RepositoryError::Database(format!("contacts row {id}: invalid {column}")));
        Ok(Self {
            id: model.id as u64,
            version: model.version as u64,
            uid: Uid::from(model.uid),
            icloud: SideState {
                href: Href::from(model.icloud_href),
                etag: ETag::from(model.icloud_etag),
                last_seen_at: model.icloud_last_seen_at.with_timezone(&Utc),
            },
            fastmail: SideState {
                href: Href::from(model.fastmail_href),
                etag: ETag::from(model.fastmail_etag),
                last_seen_at: model.fastmail_last_seen_at.with_timezone(&Utc),
            },
            content_hash: CardHash::from_hex(&model.content_hash).map_err(|_| corrupt("content_hash"))?,
            hash_version: u8::try_from(model.hash_version).map_err(|_| corrupt("hash_version"))?,
            photo_stripped: model.photo_stripped,
            last_synced_vcard: VCard::parse(model.last_synced_vcard).map_err(|_| corrupt("last_synced_vcard"))?,
            last_synced_at: model.last_synced_at.with_timezone(&Utc),
            created_at: model.created_at.with_timezone(&Utc),
            updated_at: model.updated_at.with_timezone(&Utc),
        })
    }
}

pub(crate) struct ContactStateRepositoryAdapter;

impl ContactStateRepositoryAdapter {
    pub(crate) fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl ContactStateRepository for ContactStateRepositoryAdapter {
    async fn list_all(&self, transaction: &dyn Transaction) -> Result<Vec<ContactState>, Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        prelude::Contacts::find()
            .order_by_asc(contacts::Column::Id)
            .all(transaction)
            .await
            .map_err(handle_dberr)?
            .into_iter()
            .map(ContactState::try_from)
            .collect()
    }

    async fn find_by_uid(&self, transaction: &dyn Transaction, uid: &Uid) -> Result<Option<ContactState>, Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        prelude::Contacts::find()
            .filter(contacts::Column::Uid.eq(uid.as_str()))
            .one(transaction)
            .await
            .map_err(handle_dberr)?
            .map(ContactState::try_from)
            .transpose()
    }

    async fn add(&self, transaction: &dyn Transaction, new: NewContactState) -> Result<ContactState, Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        let now: DateTimeWithTimeZone = Utc::now().into();
        let model = contacts::ActiveModel {
            id: NotSet,
            version: Set(0),
            uid: Set(new.uid.into_string()),
            icloud_href: Set(new.icloud.href.into_string()),
            icloud_etag: Set(new.icloud.etag.into_string()),
            icloud_last_seen_at: Set(new.icloud.last_seen_at.into()),
            fastmail_href: Set(new.fastmail.href.into_string()),
            fastmail_etag: Set(new.fastmail.etag.into_string()),
            fastmail_last_seen_at: Set(new.fastmail.last_seen_at.into()),
            content_hash: Set(new.content_hash.as_hex()),
            hash_version: Set(i64::from(new.hash_version)),
            photo_stripped: Set(new.photo_stripped),
            last_synced_vcard: Set(new.last_synced_vcard.into_bytes()),
            last_synced_at: Set(new.last_synced_at.into()),
            created_at: Set(now),
            updated_at: Set(now),
        };
        model.insert(transaction).await.map_err(handle_dberr)?.try_into()
    }

    async fn update(&self, transaction: &dyn Transaction, state: ContactState) -> Result<ContactState, Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        let existing = prelude::Contacts::find_by_id(state.id as i64)
            .one(transaction)
            .await
            .map_err(handle_dberr)?
            .ok_or(Error::RepositoryError(RepositoryError::NotFound))?;
        // Safe without a guarded `UPDATE … WHERE version = ?`: all access goes
        // through one connection, and this read and the write below share one
        // transaction.
        if existing.version as u64 != state.version {
            return Err(Error::RepositoryError(RepositoryError::Conflict));
        }

        let mut updater = existing.into_active_model();
        updater.uid = Set(state.uid.into_string());
        updater.icloud_href = Set(state.icloud.href.into_string());
        updater.icloud_etag = Set(state.icloud.etag.into_string());
        updater.icloud_last_seen_at = Set(state.icloud.last_seen_at.into());
        updater.fastmail_href = Set(state.fastmail.href.into_string());
        updater.fastmail_etag = Set(state.fastmail.etag.into_string());
        updater.fastmail_last_seen_at = Set(state.fastmail.last_seen_at.into());
        updater.content_hash = Set(state.content_hash.as_hex());
        updater.hash_version = Set(i64::from(state.hash_version));
        updater.photo_stripped = Set(state.photo_stripped);
        updater.last_synced_vcard = Set(state.last_synced_vcard.into_bytes());
        updater.last_synced_at = Set(state.last_synced_at.into());
        updater.update(transaction).await.map_err(handle_dberr)?.try_into()
    }

    async fn mark_seen(&self, transaction: &dyn Transaction, side: Side, uids: &[Uid], seen_at: DateTime<Utc>) -> Result<u64, Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        let column = match side {
            Side::ICloud => contacts::Column::IcloudLastSeenAt,
            Side::Fastmail => contacts::Column::FastmailLastSeenAt,
        };
        let seen_at: DateTimeWithTimeZone = seen_at.into();
        let mut changed = 0;
        for chunk in uids.chunks(MARK_SEEN_CHUNK) {
            let result = prelude::Contacts::update_many()
                .col_expr(column, Expr::value(seen_at))
                .filter(contacts::Column::Uid.is_in(chunk.iter().map(Uid::as_str)))
                .exec(transaction)
                .await
                .map_err(handle_dberr)?;
            changed += result.rows_affected;
        }
        Ok(changed)
    }

    async fn delete_by_uid(&self, transaction: &dyn Transaction, uid: &Uid) -> Result<(), Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        let result = prelude::Contacts::delete_many()
            .filter(contacts::Column::Uid.eq(uid.as_str()))
            .exec(transaction)
            .await
            .map_err(handle_dberr)?;
        if result.rows_affected == 0 {
            return Err(Error::RepositoryError(RepositoryError::NotFound));
        }
        Ok(())
    }

    async fn delete_all(&self, transaction: &dyn Transaction) -> Result<u64, Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        let result = prelude::Contacts::delete_many().exec(transaction).await.map_err(handle_dberr)?;
        Ok(result.rows_affected)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cg_core::{
        Error, RepositoryError,
        contact::{CANONICAL_VERSION, ETag, HashOptions, Href, Side, Uid, VCard},
        repository::RepositoryService,
        state::{NewContactState, SideState},
    };
    use chrono::{DateTime, TimeZone, Utc};
    use sea_orm::{ActiveModelTrait, ActiveValue::Set, Database};

    use crate::{create_repository_service, entities::contacts, transaction::TransactionImpl};

    async fn setup() -> Arc<RepositoryService> {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        create_repository_service(db).await.unwrap()
    }

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_790_000_000 + secs, 0).unwrap()
    }

    /// CRLF, a folded base64 photo and non-ASCII text: all must survive
    /// storage byte-for-byte.
    fn card_bytes(uid: &str) -> Vec<u8> {
        format!("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:Zoë Exämple\r\nPHOTO;ENCODING=b;TYPE=JPEG:QUJD\r\n QUJD\r\nEND:VCARD\r\n").into_bytes()
    }

    fn side(prefix: &str, uid: &str, secs: i64) -> SideState {
        SideState {
            href: Href::new(format!("/{prefix}/{uid}.vcf")),
            etag: ETag::new(format!("\"{prefix}-{uid}\"")),
            last_seen_at: at(secs),
        }
    }

    fn new_state(uid: &str) -> NewContactState {
        let vcard = VCard::parse(card_bytes(uid)).unwrap();
        NewContactState {
            uid: Uid::from(uid),
            icloud: side("icloud", uid, 0),
            fastmail: side("fastmail", uid, 0),
            content_hash: vcard.canonical_hash(HashOptions::default()),
            hash_version: CANONICAL_VERSION,
            photo_stripped: false,
            last_synced_vcard: vcard,
            last_synced_at: at(0),
        }
    }

    #[tokio::test]
    async fn add_and_find_round_trip() {
        let svc = setup().await;
        let repo = svc.contact_state_repository();
        let tx = svc.repository().begin().await.unwrap();

        let added = repo.add(&*tx, new_state("u1")).await.unwrap();
        assert_eq!(added.version, 1, "before_save bumps the written 0 to 1 on insert");

        let found = repo.find_by_uid(&*tx, &Uid::from("u1")).await.unwrap().expect("row exists");
        assert_eq!(found, added);
        assert_eq!(found.last_synced_vcard.as_bytes(), card_bytes("u1").as_slice());
        assert_eq!(found.icloud, side("icloud", "u1", 0));
        assert_eq!(found.fastmail, side("fastmail", "u1", 0));
        assert_eq!(found.hash_version, CANONICAL_VERSION);
        assert_eq!(found.last_synced_at, at(0));

        assert_eq!(repo.find_by_uid(&*tx, &Uid::from("missing")).await.unwrap(), None);
    }

    #[tokio::test]
    async fn add_duplicate_uid_is_constraint() {
        let svc = setup().await;
        let repo = svc.contact_state_repository();
        let tx = svc.repository().begin().await.unwrap();
        repo.add(&*tx, new_state("u1")).await.unwrap();

        let mut dup = new_state("u1");
        dup.icloud.href = Href::from("/icloud/other.vcf");
        dup.fastmail.href = Href::from("/fastmail/other.vcf");
        let err = repo.add(&*tx, dup).await.unwrap_err();
        assert!(matches!(err, Error::RepositoryError(RepositoryError::Constraint(_))), "{err:?}");
    }

    #[tokio::test]
    async fn add_duplicate_href_is_constraint() {
        let svc = setup().await;
        let repo = svc.contact_state_repository();
        let tx = svc.repository().begin().await.unwrap();
        repo.add(&*tx, new_state("u1")).await.unwrap();

        let mut other = new_state("u2");
        other.fastmail.href = Href::from("/fastmail/u1.vcf");
        let err = repo.add(&*tx, other).await.unwrap_err();
        assert!(matches!(err, Error::RepositoryError(RepositoryError::Constraint(_))), "{err:?}");
    }

    #[tokio::test]
    async fn add_in_read_only_transaction_is_read_only() {
        let svc = setup().await;
        let tx = svc.repository().begin_read_only().await.unwrap();
        let err = svc.contact_state_repository().add(&*tx, new_state("u1")).await.unwrap_err();
        assert!(matches!(err, Error::RepositoryError(RepositoryError::ReadOnly)), "{err:?}");
    }

    #[tokio::test]
    async fn list_all_returns_rows_in_id_order() {
        let svc = setup().await;
        let repo = svc.contact_state_repository();
        let tx = svc.repository().begin().await.unwrap();
        repo.add(&*tx, new_state("b")).await.unwrap();
        repo.add(&*tx, new_state("a")).await.unwrap();

        let uids: Vec<String> = repo.list_all(&*tx).await.unwrap().into_iter().map(|s| s.uid.into_string()).collect();
        assert_eq!(uids, ["b", "a"]);
    }

    #[tokio::test]
    async fn update_persists_fields_and_bumps_version() {
        let svc = setup().await;
        let repo = svc.contact_state_repository();
        let tx = svc.repository().begin().await.unwrap();
        let mut state = repo.add(&*tx, new_state("u1")).await.unwrap();

        state.fastmail.etag = ETag::from("\"fastmail-new\"");
        state.photo_stripped = true;
        state.last_synced_at = at(60);
        let updated = repo.update(&*tx, state.clone()).await.unwrap();

        assert_eq!(updated.version, state.version + 1);
        assert_eq!(updated.fastmail.etag.as_str(), "\"fastmail-new\"");
        assert!(updated.photo_stripped);
        assert_eq!(updated.last_synced_at, at(60));
        assert_eq!(repo.find_by_uid(&*tx, &Uid::from("u1")).await.unwrap(), Some(updated));
    }

    #[tokio::test]
    async fn update_with_stale_version_is_conflict() {
        let svc = setup().await;
        let repo = svc.contact_state_repository();
        let tx = svc.repository().begin().await.unwrap();
        let snapshot = repo.add(&*tx, new_state("u1")).await.unwrap();

        repo.update(&*tx, snapshot.clone()).await.unwrap();
        let err = repo.update(&*tx, snapshot).await.unwrap_err();
        assert!(matches!(err, Error::RepositoryError(RepositoryError::Conflict)), "{err:?}");
    }

    #[tokio::test]
    async fn update_missing_row_is_not_found() {
        let svc = setup().await;
        let repo = svc.contact_state_repository();
        let tx = svc.repository().begin().await.unwrap();
        let mut state = repo.add(&*tx, new_state("u1")).await.unwrap();
        repo.delete_by_uid(&*tx, &Uid::from("u1")).await.unwrap();

        state.photo_stripped = true;
        let err = repo.update(&*tx, state).await.unwrap_err();
        assert!(matches!(err, Error::RepositoryError(RepositoryError::NotFound)), "{err:?}");
    }

    #[tokio::test]
    async fn mark_seen_updates_only_that_side_without_bumping_version() {
        let svc = setup().await;
        let repo = svc.contact_state_repository();
        let tx = svc.repository().begin().await.unwrap();
        let u1 = repo.add(&*tx, new_state("u1")).await.unwrap();
        let u2 = repo.add(&*tx, new_state("u2")).await.unwrap();

        let changed = repo
            .mark_seen(&*tx, Side::Fastmail, &[Uid::from("u1"), Uid::from("missing")], at(100))
            .await
            .unwrap();
        assert_eq!(changed, 1);

        let seen = repo.find_by_uid(&*tx, &Uid::from("u1")).await.unwrap().unwrap();
        assert_eq!(seen.fastmail.last_seen_at, at(100));
        assert_eq!(seen.icloud.last_seen_at, at(0));
        assert_eq!(seen.version, u1.version);
        assert_eq!(repo.find_by_uid(&*tx, &Uid::from("u2")).await.unwrap(), Some(u2));
    }

    #[tokio::test]
    async fn mark_seen_with_no_uids_changes_nothing() {
        let svc = setup().await;
        let tx = svc.repository().begin().await.unwrap();
        assert_eq!(svc.contact_state_repository().mark_seen(&*tx, Side::ICloud, &[], at(1)).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn mark_seen_handles_more_uids_than_one_statement_binds() {
        let svc = setup().await;
        let repo = svc.contact_state_repository();
        let tx = svc.repository().begin().await.unwrap();
        repo.add(&*tx, new_state("u1")).await.unwrap();
        repo.add(&*tx, new_state("u1199")).await.unwrap();

        let uids: Vec<Uid> = (0..1200).map(|i| Uid::new(format!("u{i}"))).collect();
        assert_eq!(repo.mark_seen(&*tx, Side::ICloud, &uids, at(5)).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn delete_by_uid_removes_the_row() {
        let svc = setup().await;
        let repo = svc.contact_state_repository();
        let tx = svc.repository().begin().await.unwrap();
        repo.add(&*tx, new_state("u1")).await.unwrap();

        repo.delete_by_uid(&*tx, &Uid::from("u1")).await.unwrap();
        assert_eq!(repo.find_by_uid(&*tx, &Uid::from("u1")).await.unwrap(), None);

        let err = repo.delete_by_uid(&*tx, &Uid::from("u1")).await.unwrap_err();
        assert!(matches!(err, Error::RepositoryError(RepositoryError::NotFound)), "{err:?}");
    }

    #[tokio::test]
    async fn delete_all_removes_every_row() {
        let svc = setup().await;
        let repo = svc.contact_state_repository();
        let tx = svc.repository().begin().await.unwrap();
        repo.add(&*tx, new_state("u1")).await.unwrap();
        repo.add(&*tx, new_state("u2")).await.unwrap();

        assert_eq!(repo.delete_all(&*tx).await.unwrap(), 2);
        #[allow(clippy::assert_is_empty, reason = "assert_eq! against an empty Vec literal is less readable here")]
        {
            assert!(repo.list_all(&*tx).await.unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn corrupt_content_hash_is_database_error() {
        let svc = setup().await;
        let tx = svc.repository().begin().await.unwrap();
        let db_tx = TransactionImpl::get_db_transaction(&*tx).unwrap();
        let bad = contacts::ActiveModel {
            version: Set(0),
            uid: Set("u1".to_owned()),
            icloud_href: Set("/icloud/u1.vcf".to_owned()),
            icloud_etag: Set("\"e\"".to_owned()),
            icloud_last_seen_at: Set(at(0).into()),
            fastmail_href: Set("/fastmail/u1.vcf".to_owned()),
            fastmail_etag: Set("\"e\"".to_owned()),
            fastmail_last_seen_at: Set(at(0).into()),
            content_hash: Set("not-hex".to_owned()),
            hash_version: Set(1),
            photo_stripped: Set(false),
            last_synced_vcard: Set(card_bytes("u1")),
            last_synced_at: Set(at(0).into()),
            created_at: Set(at(0).into()),
            updated_at: Set(at(0).into()),
            ..Default::default()
        };
        bad.insert(db_tx).await.unwrap();

        let err = svc.contact_state_repository().find_by_uid(&*tx, &Uid::from("u1")).await.unwrap_err();
        let Error::RepositoryError(RepositoryError::Database(message)) = err else {
            panic!("expected Database error, got {err:?}");
        };
        assert!(message.contains("contacts") && message.contains("content_hash"), "{message}");
        assert!(!message.contains("Zoë"), "no card content in errors: {message}");
    }
}
