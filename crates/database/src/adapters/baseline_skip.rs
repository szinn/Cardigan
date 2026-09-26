use cg_core::{
    Error, RepositoryError,
    contact::{CardHash, Href, Side, Uid},
    repository::Transaction,
    state::{BaselineSkip, BaselineSkipRepository, NewBaselineSkip},
};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait,
    ActiveValue::{NotSet, Set},
    ColumnTrait, DatabaseTransaction, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
    prelude::DateTimeWithTimeZone,
};

use crate::{
    entities::{baseline_skips, prelude},
    error::handle_dberr,
    transaction::TransactionImpl,
};

fn corrupt(id: i64, column: &str) -> Error {
    Error::RepositoryError(RepositoryError::Database(format!("baseline_skips row {id}: invalid {column}")))
}

impl TryFrom<baseline_skips::Model> for BaselineSkip {
    type Error = Error;

    fn try_from(model: baseline_skips::Model) -> Result<Self, Error> {
        let id = model.id;
        Ok(Self {
            id: model.id as u64,
            version: model.version as u64,
            side: model.side.parse::<Side>().map_err(|_| corrupt(id, "side"))?,
            href: Href::from(model.href),
            uid: Uid::from(model.uid),
            content_hash: CardHash::from_hex(&model.content_hash).map_err(|_| corrupt(id, "content_hash"))?,
            hash_version: u8::try_from(model.hash_version).map_err(|_| corrupt(id, "hash_version"))?,
            candidate_count: u32::try_from(model.candidate_count).map_err(|_| corrupt(id, "candidate_count"))?,
            skipped_at: model.skipped_at.with_timezone(&Utc),
        })
    }
}

pub(crate) struct BaselineSkipRepositoryAdapter;

impl BaselineSkipRepositoryAdapter {
    pub(crate) fn new() -> Self {
        Self
    }
}

async fn find_row(transaction: &DatabaseTransaction, side: Side, href: &Href) -> Result<Option<baseline_skips::Model>, Error> {
    Ok(prelude::BaselineSkips::find()
        .filter(baseline_skips::Column::Side.eq(side.as_str()))
        .filter(baseline_skips::Column::Href.eq(href.as_str()))
        .one(transaction)
        .await
        .map_err(handle_dberr)?)
}

async fn insert(transaction: &DatabaseTransaction, skip: NewBaselineSkip) -> Result<baseline_skips::Model, Error> {
    let stamp: DateTimeWithTimeZone = Utc::now().into();
    Ok(baseline_skips::ActiveModel {
        id: NotSet,
        version: Set(0),
        side: Set(skip.side.as_str().to_owned()),
        href: Set(skip.href.into_string()),
        uid: Set(skip.uid.into_string()),
        content_hash: Set(skip.content_hash.as_hex()),
        hash_version: Set(i64::from(skip.hash_version)),
        candidate_count: Set(i64::from(skip.candidate_count)),
        skipped_at: Set(skip.skipped_at.into()),
        created_at: Set(stamp),
        updated_at: Set(stamp),
    }
    .insert(transaction)
    .await
    .map_err(handle_dberr)?)
}

#[async_trait::async_trait]
impl BaselineSkipRepository for BaselineSkipRepositoryAdapter {
    async fn replace_all(&self, transaction: &dyn Transaction, skips: Vec<NewBaselineSkip>) -> Result<u64, Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        prelude::BaselineSkips::delete_many().exec(transaction).await.map_err(handle_dberr)?;
        let mut stored = 0;
        for skip in skips {
            insert(transaction, skip).await?;
            stored += 1;
        }
        Ok(stored)
    }

    async fn upsert(&self, transaction: &dyn Transaction, skip: NewBaselineSkip) -> Result<BaselineSkip, Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        let model = match find_row(transaction, skip.side, &skip.href).await? {
            None => insert(transaction, skip).await?,
            Some(row) => {
                let mut updater = row.into_active_model();
                updater.uid = Set(skip.uid.into_string());
                updater.content_hash = Set(skip.content_hash.as_hex());
                updater.hash_version = Set(i64::from(skip.hash_version));
                updater.candidate_count = Set(i64::from(skip.candidate_count));
                updater.skipped_at = Set(skip.skipped_at.into());
                updater.update(transaction).await.map_err(handle_dberr)?
            }
        };
        model.try_into()
    }

    async fn list_all(&self, transaction: &dyn Transaction) -> Result<Vec<BaselineSkip>, Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        prelude::BaselineSkips::find()
            .order_by_asc(baseline_skips::Column::Id)
            .all(transaction)
            .await
            .map_err(handle_dberr)?
            .into_iter()
            .map(BaselineSkip::try_from)
            .collect()
    }

    async fn delete(&self, transaction: &dyn Transaction, side: Side, href: &Href) -> Result<bool, Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        let result = prelude::BaselineSkips::delete_many()
            .filter(baseline_skips::Column::Side.eq(side.as_str()))
            .filter(baseline_skips::Column::Href.eq(href.as_str()))
            .exec(transaction)
            .await
            .map_err(handle_dberr)?;
        Ok(result.rows_affected > 0)
    }

    async fn delete_all(&self, transaction: &dyn Transaction) -> Result<u64, Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        let result = prelude::BaselineSkips::delete_many().exec(transaction).await.map_err(handle_dberr)?;
        Ok(result.rows_affected)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cg_core::{
        Error, RepositoryError,
        contact::{CANONICAL_VERSION, CardHash, Href, Side, Uid},
        repository::RepositoryService,
        state::{BackoffPolicy, ConflictOrigin, FailedCard, FailureOp, FailureReason, NewBaselineSkip, NewConflict},
    };
    use chrono::{DateTime, TimeDelta, TimeZone, Utc};
    use sea_orm::{ActiveModelTrait, ActiveValue::Set, Database};

    use crate::{create_repository_service, entities::baseline_skips, transaction::TransactionImpl};

    async fn setup() -> Arc<RepositoryService> {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        create_repository_service(db).await.unwrap()
    }

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_790_000_000 + secs, 0).unwrap()
    }

    fn hash(byte: &str) -> CardHash {
        CardHash::from_hex(byte.repeat(32)).unwrap()
    }

    fn skip(side: Side, href: &str, uid: &str) -> NewBaselineSkip {
        NewBaselineSkip {
            side,
            href: Href::from(href),
            uid: Uid::from(uid),
            content_hash: hash("ab"),
            hash_version: CANONICAL_VERSION,
            candidate_count: 2,
            skipped_at: at(0),
        }
    }

    fn hrefs(skips: &[cg_core::state::BaselineSkip]) -> Vec<(Side, String)> {
        skips.iter().map(|s| (s.side, s.href.as_str().to_owned())).collect()
    }

    #[tokio::test]
    async fn replace_all_replaces_every_skip() {
        let svc = setup().await;
        let repo = svc.baseline_skip_repository();
        let tx = svc.repository().begin().await.unwrap();

        let stored = repo
            .replace_all(&*tx, vec![skip(Side::ICloud, "/a.vcf", "a"), skip(Side::Fastmail, "/b.vcf", "b")])
            .await
            .unwrap();
        assert_eq!(stored, 2);
        assert_eq!(repo.replace_all(&*tx, vec![skip(Side::ICloud, "/c.vcf", "c")]).await.unwrap(), 1);

        let all = repo.list_all(&*tx).await.unwrap();
        assert_eq!(hrefs(&all), [(Side::ICloud, "/c.vcf".to_owned())]);
        assert_eq!(all[0].content_hash, hash("ab"));
        assert_eq!(all[0].hash_version, CANONICAL_VERSION);
        assert_eq!(all[0].candidate_count, 2);
        assert_eq!(all[0].skipped_at, at(0));
    }

    #[tokio::test]
    async fn upsert_inserts_then_updates_in_place() {
        let svc = setup().await;
        let repo = svc.baseline_skip_repository();
        let tx = svc.repository().begin().await.unwrap();
        let first = repo.upsert(&*tx, skip(Side::ICloud, "/a.vcf", "a")).await.unwrap();

        let mut edited = skip(Side::ICloud, "/a.vcf", "a");
        edited.content_hash = hash("cd");
        edited.candidate_count = 0;
        edited.skipped_at = at(120);
        let second = repo.upsert(&*tx, edited).await.unwrap();

        assert_eq!(second.id, first.id);
        assert!(second.version > first.version);
        assert_eq!((second.content_hash, second.candidate_count, second.skipped_at), (hash("cd"), 0, at(120)));
        assert_eq!(repo.list_all(&*tx).await.unwrap(), [second]);
    }

    #[tokio::test]
    async fn same_href_on_each_side_is_a_separate_skip() {
        let svc = setup().await;
        let repo = svc.baseline_skip_repository();
        let tx = svc.repository().begin().await.unwrap();
        repo.upsert(&*tx, skip(Side::ICloud, "/a.vcf", "a")).await.unwrap();
        repo.upsert(&*tx, skip(Side::Fastmail, "/a.vcf", "a2")).await.unwrap();
        assert_eq!(repo.list_all(&*tx).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn delete_removes_only_that_resource() {
        let svc = setup().await;
        let repo = svc.baseline_skip_repository();
        let tx = svc.repository().begin().await.unwrap();
        repo.replace_all(&*tx, vec![skip(Side::ICloud, "/a.vcf", "a"), skip(Side::ICloud, "/b.vcf", "b")])
            .await
            .unwrap();

        assert!(repo.delete(&*tx, Side::ICloud, &Href::from("/a.vcf")).await.unwrap());
        assert!(!repo.delete(&*tx, Side::ICloud, &Href::from("/a.vcf")).await.unwrap());
        assert_eq!(hrefs(&repo.list_all(&*tx).await.unwrap()), [(Side::ICloud, "/b.vcf".to_owned())]);
    }

    #[tokio::test]
    async fn delete_all_removes_every_row() {
        let svc = setup().await;
        let repo = svc.baseline_skip_repository();
        let tx = svc.repository().begin().await.unwrap();
        repo.replace_all(&*tx, vec![skip(Side::ICloud, "/a.vcf", "a"), skip(Side::Fastmail, "/b.vcf", "b")])
            .await
            .unwrap();

        assert_eq!(repo.delete_all(&*tx).await.unwrap(), 2);
        assert_eq!(repo.list_all(&*tx).await.unwrap(), []);
    }

    #[tokio::test]
    async fn replace_all_in_read_only_transaction_is_read_only() {
        let svc = setup().await;
        let tx = svc.repository().begin_read_only().await.unwrap();
        let err = svc
            .baseline_skip_repository()
            .replace_all(&*tx, vec![skip(Side::ICloud, "/a.vcf", "a")])
            .await
            .unwrap_err();
        assert!(matches!(err, Error::RepositoryError(RepositoryError::ReadOnly)), "{err:?}");
    }

    /// `--reset` clears the other state tables; the conflict history must
    /// survive it.
    #[tokio::test]
    async fn reset_tables_leave_conflicts_intact() {
        let svc = setup().await;
        let tx = svc.repository().begin().await.unwrap();
        let conflict = svc
            .conflict_repository()
            .add(
                &*tx,
                NewConflict {
                    uid: Uid::from("u1"),
                    origin: ConflictOrigin::Sync,
                    winner: Side::ICloud,
                    icloud_vcard: b"icloud".to_vec(),
                    fastmail_vcard: b"fastmail".to_vec(),
                    detected_at: at(0),
                },
            )
            .await
            .unwrap();
        let failed = FailedCard {
            side: Side::ICloud,
            href: Href::from("/a.vcf"),
            uid: None,
            op: FailureOp::Read,
            etag: None,
            reason: FailureReason::MissingUid,
        };
        let policy = BackoffPolicy {
            base: TimeDelta::seconds(60),
            cap: TimeDelta::seconds(600),
        };
        svc.card_failure_repository().record_failure(&*tx, failed, at(0), &policy).await.unwrap();
        svc.baseline_skip_repository().upsert(&*tx, skip(Side::ICloud, "/b.vcf", "b")).await.unwrap();

        svc.contact_state_repository().delete_all(&*tx).await.unwrap();
        svc.endpoint_repository().delete_all(&*tx).await.unwrap();
        assert_eq!(svc.card_failure_repository().delete_all(&*tx).await.unwrap(), 1);
        assert_eq!(svc.baseline_skip_repository().delete_all(&*tx).await.unwrap(), 1);

        assert_eq!(svc.conflict_repository().list_all(&*tx).await.unwrap(), [conflict]);
    }

    fn raw_skip_row() -> baseline_skips::ActiveModel {
        baseline_skips::ActiveModel {
            version: Set(0),
            side: Set("icloud".to_owned()),
            href: Set("/a.vcf".to_owned()),
            uid: Set("a".to_owned()),
            content_hash: Set("ab".repeat(32)),
            hash_version: Set(1),
            candidate_count: Set(2),
            skipped_at: Set(at(0).into()),
            created_at: Set(at(0).into()),
            updated_at: Set(at(0).into()),
            ..Default::default()
        }
    }

    async fn assert_corrupt_row_is_database_error(bad: baseline_skips::ActiveModel, column: &str) {
        let svc = setup().await;
        let tx = svc.repository().begin().await.unwrap();
        bad.insert(TransactionImpl::get_db_transaction(&*tx).unwrap()).await.unwrap();

        let err = svc.baseline_skip_repository().list_all(&*tx).await.unwrap_err();
        let Error::RepositoryError(RepositoryError::Database(message)) = err else {
            panic!("expected Database error, got {err:?}");
        };
        assert!(message.contains("baseline_skips") && message.contains(column), "{message}");
    }

    #[tokio::test]
    async fn corrupt_content_hash_is_database_error() {
        let mut bad = raw_skip_row();
        bad.content_hash = Set("not-hex".to_owned());
        assert_corrupt_row_is_database_error(bad, "content_hash").await;
    }

    #[tokio::test]
    async fn corrupt_hash_version_is_database_error() {
        let mut bad = raw_skip_row();
        bad.hash_version = Set(300);
        assert_corrupt_row_is_database_error(bad, "hash_version").await;
    }
}
