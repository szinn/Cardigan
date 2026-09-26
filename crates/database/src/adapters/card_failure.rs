use cg_core::{
    Error, RepositoryError,
    contact::{ETag, Href, Side, Uid},
    repository::Transaction,
    state::{BackoffPolicy, CardFailure, CardFailureRepository, FailedCard, FailureOp, FailureReason},
};
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait,
    ActiveValue::{NotSet, Set},
    ColumnTrait, DatabaseTransaction, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
    prelude::DateTimeWithTimeZone,
};

use crate::{
    entities::{card_failures, prelude},
    error::handle_dberr,
    transaction::TransactionImpl,
};

fn corrupt(id: i64, column: &str) -> Error {
    Error::RepositoryError(RepositoryError::Database(format!("card_failures row {id}: invalid {column}")))
}

impl TryFrom<card_failures::Model> for CardFailure {
    type Error = Error;

    fn try_from(model: card_failures::Model) -> Result<Self, Error> {
        let id = model.id;
        Ok(Self {
            id: model.id as u64,
            version: model.version as u64,
            side: model.side.parse::<Side>().map_err(|_| corrupt(id, "side"))?,
            href: Href::from(model.href),
            uid: model.uid.map(Uid::from),
            op: model.op.parse::<FailureOp>().map_err(|_| corrupt(id, "op"))?,
            etag: model.etag.map(ETag::from),
            reason: model.reason.parse::<FailureReason>().map_err(|_| corrupt(id, "reason"))?,
            attempts: u32::try_from(model.attempts).map_err(|_| corrupt(id, "attempts"))?,
            first_failed_at: model.first_failed_at.with_timezone(&Utc),
            last_failed_at: model.last_failed_at.with_timezone(&Utc),
            next_retry_at: model.next_retry_at.with_timezone(&Utc),
        })
    }
}

pub(crate) struct CardFailureRepositoryAdapter;

impl CardFailureRepositoryAdapter {
    pub(crate) fn new() -> Self {
        Self
    }
}

async fn find_row(transaction: &DatabaseTransaction, side: Side, href: &Href) -> Result<Option<card_failures::Model>, Error> {
    Ok(prelude::CardFailures::find()
        .filter(card_failures::Column::Side.eq(side.as_str()))
        .filter(card_failures::Column::Href.eq(href.as_str()))
        .one(transaction)
        .await
        .map_err(handle_dberr)?)
}

#[async_trait::async_trait]
impl CardFailureRepository for CardFailureRepositoryAdapter {
    async fn record_failure(
        &self,
        transaction: &dyn Transaction,
        failed: FailedCard,
        now: DateTime<Utc>,
        policy: &BackoffPolicy,
    ) -> Result<CardFailure, Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        let failed_at: DateTimeWithTimeZone = now.into();
        let etag = failed.etag.map(ETag::into_string);

        let model = match find_row(transaction, failed.side, &failed.href).await? {
            None => {
                let stamp: DateTimeWithTimeZone = Utc::now().into();
                card_failures::ActiveModel {
                    id: NotSet,
                    version: Set(0),
                    side: Set(failed.side.as_str().to_owned()),
                    href: Set(failed.href.into_string()),
                    uid: Set(failed.uid.map(Uid::into_string)),
                    op: Set(failed.op.as_str().to_owned()),
                    etag: Set(etag),
                    reason: Set(failed.reason.as_str().to_owned()),
                    attempts: Set(1),
                    first_failed_at: Set(failed_at),
                    last_failed_at: Set(failed_at),
                    next_retry_at: Set((now + policy.delay(1)).into()),
                    created_at: Set(stamp),
                    updated_at: Set(stamp),
                }
                .insert(transaction)
                .await
                .map_err(handle_dberr)?
            }
            Some(row) => {
                // A different ETag is a new version of the card: restart its
                // count.
                let restarted = row.etag != etag;
                let attempts = if restarted {
                    1
                } else {
                    u32::try_from(row.attempts).map_err(|_| corrupt(row.id, "attempts"))?.saturating_add(1)
                };
                let mut updater = row.into_active_model();
                if restarted {
                    updater.first_failed_at = Set(failed_at);
                }
                updater.uid = Set(failed.uid.map(Uid::into_string));
                updater.op = Set(failed.op.as_str().to_owned());
                updater.etag = Set(etag);
                updater.reason = Set(failed.reason.as_str().to_owned());
                updater.attempts = Set(i64::from(attempts));
                updater.last_failed_at = Set(failed_at);
                updater.next_retry_at = Set((now + policy.delay(attempts)).into());
                updater.update(transaction).await.map_err(handle_dberr)?
            }
        };
        model.try_into()
    }

    async fn find(&self, transaction: &dyn Transaction, side: Side, href: &Href) -> Result<Option<CardFailure>, Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        find_row(transaction, side, href).await?.map(CardFailure::try_from).transpose()
    }

    async fn list_all(&self, transaction: &dyn Transaction) -> Result<Vec<CardFailure>, Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        prelude::CardFailures::find()
            .order_by_asc(card_failures::Column::Id)
            .all(transaction)
            .await
            .map_err(handle_dberr)?
            .into_iter()
            .map(CardFailure::try_from)
            .collect()
    }

    async fn clear(&self, transaction: &dyn Transaction, side: Side, href: &Href) -> Result<bool, Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        let result = prelude::CardFailures::delete_many()
            .filter(card_failures::Column::Side.eq(side.as_str()))
            .filter(card_failures::Column::Href.eq(href.as_str()))
            .exec(transaction)
            .await
            .map_err(handle_dberr)?;
        Ok(result.rows_affected > 0)
    }

    async fn delete_all(&self, transaction: &dyn Transaction) -> Result<u64, Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        let result = prelude::CardFailures::delete_many().exec(transaction).await.map_err(handle_dberr)?;
        Ok(result.rows_affected)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cg_core::{
        Error, RepositoryError,
        contact::{ETag, Href, Side, Uid},
        repository::RepositoryService,
        state::{BackoffPolicy, FailedCard, FailureOp, FailureReason},
    };
    use chrono::{DateTime, TimeDelta, TimeZone, Utc};
    use sea_orm::{ActiveModelTrait, ActiveValue::Set, Database};

    use crate::{create_repository_service, entities::card_failures, error::handle_dberr, transaction::TransactionImpl};

    async fn setup() -> Arc<RepositoryService> {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        create_repository_service(db).await.unwrap()
    }

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_790_000_000 + secs, 0).unwrap()
    }

    fn policy() -> BackoffPolicy {
        BackoffPolicy {
            base: TimeDelta::seconds(60),
            cap: TimeDelta::seconds(600),
        }
    }

    fn failed(side: Side, href: &str, etag: &str) -> FailedCard {
        FailedCard {
            side,
            href: Href::from(href),
            uid: Some(Uid::from("u1")),
            op: FailureOp::Update,
            etag: Some(ETag::from(etag)),
            reason: FailureReason::Rejected,
        }
    }

    #[tokio::test]
    async fn failure_without_uid_is_one_row_that_backs_off() {
        let svc = setup().await;
        let repo = svc.card_failure_repository();
        let tx = svc.repository().begin().await.unwrap();
        let unparsable = FailedCard {
            side: Side::Fastmail,
            href: Href::from("/fastmail/broken.vcf"),
            uid: None,
            op: FailureOp::Read,
            etag: Some(ETag::from("\"e1\"")),
            reason: FailureReason::MissingUid,
        };

        let first = repo.record_failure(&*tx, unparsable.clone(), at(0), &policy()).await.unwrap();
        assert_eq!(
            (first.attempts, first.uid.clone(), first.reason, first.op),
            (1, None, FailureReason::MissingUid, FailureOp::Read)
        );
        assert_eq!(first.next_retry_at, at(60));

        let second = repo.record_failure(&*tx, unparsable, at(60), &policy()).await.unwrap();
        assert_eq!(second.id, first.id);
        assert_eq!(second.attempts, 2);
        assert_eq!(repo.list_all(&*tx).await.unwrap(), [second]);
    }

    #[tokio::test]
    async fn repeat_failures_back_off_up_to_the_cap() {
        let svc = setup().await;
        let repo = svc.card_failure_repository();
        let tx = svc.repository().begin().await.unwrap();

        let mut last = None;
        for (n, secs) in [0, 60, 180, 420, 900, 1500].into_iter().enumerate() {
            let row = repo
                .record_failure(&*tx, failed(Side::ICloud, "/a.vcf", "\"e1\""), at(secs), &policy())
                .await
                .unwrap();
            assert_eq!(row.attempts, u32::try_from(n + 1).unwrap());
            last = Some(row);
        }
        let last = last.unwrap();
        assert_eq!(last.first_failed_at, at(0));
        assert_eq!(last.last_failed_at, at(1500));
        assert_eq!(last.next_retry_at, at(1500 + 600), "capped at 600 s");
        assert!(last.version > 1, "repeats bump the row version");
    }

    #[tokio::test]
    async fn changed_etag_restarts_attempts() {
        let svc = setup().await;
        let repo = svc.card_failure_repository();
        let tx = svc.repository().begin().await.unwrap();
        repo.record_failure(&*tx, failed(Side::ICloud, "/a.vcf", "\"e1\""), at(0), &policy())
            .await
            .unwrap();
        repo.record_failure(&*tx, failed(Side::ICloud, "/a.vcf", "\"e1\""), at(60), &policy())
            .await
            .unwrap();

        let edited = repo
            .record_failure(&*tx, failed(Side::ICloud, "/a.vcf", "\"e2\""), at(90), &policy())
            .await
            .unwrap();
        assert_eq!(edited.attempts, 1);
        assert_eq!(edited.first_failed_at, at(90));
        assert_eq!(edited.etag, Some(ETag::from("\"e2\"")));
        assert_eq!(edited.next_retry_at, at(150));
    }

    #[tokio::test]
    async fn same_href_on_each_side_is_a_separate_failure() {
        let svc = setup().await;
        let repo = svc.card_failure_repository();
        let tx = svc.repository().begin().await.unwrap();
        repo.record_failure(&*tx, failed(Side::ICloud, "/a.vcf", "\"e1\""), at(0), &policy())
            .await
            .unwrap();
        repo.record_failure(&*tx, failed(Side::Fastmail, "/a.vcf", "\"e1\""), at(0), &policy())
            .await
            .unwrap();

        assert_eq!(repo.list_all(&*tx).await.unwrap().len(), 2);
        assert_eq!(
            repo.find(&*tx, Side::Fastmail, &Href::from("/a.vcf")).await.unwrap().unwrap().side,
            Side::Fastmail
        );
        assert_eq!(repo.find(&*tx, Side::ICloud, &Href::from("/missing.vcf")).await.unwrap(), None);
    }

    #[tokio::test]
    async fn clear_removes_only_that_resource() {
        let svc = setup().await;
        let repo = svc.card_failure_repository();
        let tx = svc.repository().begin().await.unwrap();
        repo.record_failure(&*tx, failed(Side::ICloud, "/a.vcf", "\"e1\""), at(0), &policy())
            .await
            .unwrap();
        repo.record_failure(&*tx, failed(Side::ICloud, "/b.vcf", "\"e1\""), at(0), &policy())
            .await
            .unwrap();

        assert!(repo.clear(&*tx, Side::ICloud, &Href::from("/a.vcf")).await.unwrap());
        assert!(!repo.clear(&*tx, Side::ICloud, &Href::from("/a.vcf")).await.unwrap());
        let remaining: Vec<String> = repo.list_all(&*tx).await.unwrap().into_iter().map(|f| f.href.into_string()).collect();
        assert_eq!(remaining, ["/b.vcf"]);
    }

    #[tokio::test]
    async fn delete_all_removes_every_row() {
        let svc = setup().await;
        let repo = svc.card_failure_repository();
        let tx = svc.repository().begin().await.unwrap();
        repo.record_failure(&*tx, failed(Side::ICloud, "/a.vcf", "\"e1\""), at(0), &policy())
            .await
            .unwrap();
        repo.record_failure(&*tx, failed(Side::Fastmail, "/b.vcf", "\"e1\""), at(0), &policy())
            .await
            .unwrap();

        assert_eq!(repo.delete_all(&*tx).await.unwrap(), 2);
        assert_eq!(repo.list_all(&*tx).await.unwrap(), []);
    }

    #[tokio::test]
    async fn record_failure_in_read_only_transaction_is_read_only() {
        let svc = setup().await;
        let tx = svc.repository().begin_read_only().await.unwrap();
        let err = svc
            .card_failure_repository()
            .record_failure(&*tx, failed(Side::ICloud, "/a.vcf", "\"e1\""), at(0), &policy())
            .await
            .unwrap_err();
        assert!(matches!(err, Error::RepositoryError(RepositoryError::ReadOnly)), "{err:?}");
    }

    fn raw_failure_row(href: &str) -> card_failures::ActiveModel {
        card_failures::ActiveModel {
            version: Set(0),
            side: Set("icloud".to_owned()),
            href: Set(href.to_owned()),
            uid: Set(None),
            op: Set("read".to_owned()),
            etag: Set(None),
            reason: Set("missing_uid".to_owned()),
            attempts: Set(1),
            first_failed_at: Set(at(0).into()),
            last_failed_at: Set(at(0).into()),
            next_retry_at: Set(at(60).into()),
            created_at: Set(at(0).into()),
            updated_at: Set(at(0).into()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn duplicate_side_and_href_is_constraint() {
        let svc = setup().await;
        let tx = svc.repository().begin().await.unwrap();
        let db_tx = TransactionImpl::get_db_transaction(&*tx).unwrap();
        raw_failure_row("/a.vcf").insert(db_tx).await.unwrap();

        let err = raw_failure_row("/a.vcf").insert(db_tx).await.map_err(handle_dberr).unwrap_err();
        assert!(matches!(err, RepositoryError::Constraint(_)), "{err:?}");
    }

    #[tokio::test]
    async fn corrupt_reason_is_database_error() {
        let svc = setup().await;
        let tx = svc.repository().begin().await.unwrap();
        let mut bad = raw_failure_row("/a.vcf");
        bad.reason = Set("server said: EMAIL:jane@example.com".to_owned());
        bad.insert(TransactionImpl::get_db_transaction(&*tx).unwrap()).await.unwrap();

        let err = svc.card_failure_repository().list_all(&*tx).await.unwrap_err();
        let Error::RepositoryError(RepositoryError::Database(message)) = err else {
            panic!("expected Database error, got {err:?}");
        };
        assert!(message.contains("card_failures") && message.contains("reason"), "{message}");
        assert!(!message.contains("jane@example.com"), "no stored text in errors: {message}");
    }
}
