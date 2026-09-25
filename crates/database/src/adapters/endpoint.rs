use cg_core::{
    Error, RepositoryError,
    contact::Side,
    repository::Transaction,
    state::{Endpoint, EndpointRepository},
};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait,
    ActiveValue::{NotSet, Set},
    ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter,
    prelude::DateTimeWithTimeZone,
};

use crate::{
    entities::{endpoints, prelude},
    error::handle_dberr,
    transaction::TransactionImpl,
};

impl TryFrom<endpoints::Model> for Endpoint {
    type Error = Error;

    fn try_from(model: endpoints::Model) -> Result<Self, Error> {
        let side = model
            .side
            .parse::<Side>()
            .map_err(|_| Error::RepositoryError(RepositoryError::Database(format!("endpoints row {}: invalid side", model.id))))?;
        Ok(Self {
            side,
            addressbook_url: model.addressbook_url,
            discovered_host: model.discovered_host,
            sync_token: model.sync_token,
            updated_at: model.updated_at.with_timezone(&Utc),
        })
    }
}

pub(crate) struct EndpointRepositoryAdapter;

impl EndpointRepositoryAdapter {
    pub(crate) fn new() -> Self {
        Self
    }
}

async fn find_row(transaction: &sea_orm::DatabaseTransaction, side: Side) -> Result<Option<endpoints::Model>, Error> {
    Ok(prelude::Endpoints::find()
        .filter(endpoints::Column::Side.eq(side.as_str()))
        .one(transaction)
        .await
        .map_err(handle_dberr)?)
}

#[async_trait::async_trait]
impl EndpointRepository for EndpointRepositoryAdapter {
    async fn find(&self, transaction: &dyn Transaction, side: Side) -> Result<Option<Endpoint>, Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        find_row(transaction, side).await?.map(Endpoint::try_from).transpose()
    }

    async fn upsert_discovery(&self, transaction: &dyn Transaction, side: Side, addressbook_url: &str, discovered_host: &str) -> Result<Endpoint, Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        let model = match find_row(transaction, side).await? {
            None => {
                let now: DateTimeWithTimeZone = Utc::now().into();
                endpoints::ActiveModel {
                    id: NotSet,
                    version: Set(0),
                    side: Set(side.as_str().to_owned()),
                    addressbook_url: Set(addressbook_url.to_owned()),
                    discovered_host: Set(discovered_host.to_owned()),
                    sync_token: Set(None),
                    created_at: Set(now),
                    updated_at: Set(now),
                }
                .insert(transaction)
                .await
                .map_err(handle_dberr)?
            }
            Some(row) if row.addressbook_url == addressbook_url && row.discovered_host == discovered_host => row,
            Some(row) => {
                let url_changed = row.addressbook_url != addressbook_url;
                let mut updater = row.into_active_model();
                updater.addressbook_url = Set(addressbook_url.to_owned());
                updater.discovered_host = Set(discovered_host.to_owned());
                if url_changed {
                    updater.sync_token = Set(None);
                }
                updater.update(transaction).await.map_err(handle_dberr)?
            }
        };
        model.try_into()
    }

    async fn set_sync_token(&self, transaction: &dyn Transaction, side: Side, sync_token: Option<String>) -> Result<(), Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        let row = find_row(transaction, side).await?.ok_or(Error::RepositoryError(RepositoryError::NotFound))?;
        if row.sync_token == sync_token {
            return Ok(());
        }
        let mut updater = row.into_active_model();
        updater.sync_token = Set(sync_token);
        updater.update(transaction).await.map_err(handle_dberr)?;
        Ok(())
    }

    async fn delete_all(&self, transaction: &dyn Transaction) -> Result<u64, Error> {
        let transaction = TransactionImpl::get_db_transaction(transaction)?;
        let result = prelude::Endpoints::delete_many().exec(transaction).await.map_err(handle_dberr)?;
        Ok(result.rows_affected)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cg_core::{Error, RepositoryError, contact::Side, repository::RepositoryService, state::Endpoint};
    use sea_orm::{ActiveModelTrait, ActiveValue::Set, Database, EntityTrait};

    use crate::{
        create_repository_service,
        entities::{endpoints, prelude},
        transaction::TransactionImpl,
    };

    const URL: &str = "https://p42-contacts.icloud.com/123/carddavhome/card/";
    const HOST: &str = "p42-contacts.icloud.com";

    async fn setup() -> Arc<RepositoryService> {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        create_repository_service(db).await.unwrap()
    }

    #[tokio::test]
    async fn find_is_none_before_discovery() {
        let svc = setup().await;
        let tx = svc.repository().begin().await.unwrap();
        assert_eq!(svc.endpoint_repository().find(&*tx, Side::ICloud).await.unwrap(), None);
    }

    #[tokio::test]
    async fn upsert_discovery_inserts_then_finds() {
        let svc = setup().await;
        let repo = svc.endpoint_repository();
        let tx = svc.repository().begin().await.unwrap();

        let endpoint = repo.upsert_discovery(&*tx, Side::ICloud, URL, HOST).await.unwrap();
        assert_eq!(endpoint.side, Side::ICloud);
        assert_eq!(endpoint.addressbook_url, URL);
        assert_eq!(endpoint.discovered_host, HOST);
        assert_eq!(endpoint.sync_token, None);
        assert_eq!(repo.find(&*tx, Side::ICloud).await.unwrap(), Some(endpoint));
        assert_eq!(repo.find(&*tx, Side::Fastmail).await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_sync_token_stores_and_clears() {
        let svc = setup().await;
        let repo = svc.endpoint_repository();
        let tx = svc.repository().begin().await.unwrap();
        repo.upsert_discovery(&*tx, Side::Fastmail, URL, HOST).await.unwrap();

        repo.set_sync_token(&*tx, Side::Fastmail, Some("token-1".to_owned())).await.unwrap();
        assert_eq!(repo.find(&*tx, Side::Fastmail).await.unwrap().unwrap().sync_token.as_deref(), Some("token-1"));

        repo.set_sync_token(&*tx, Side::Fastmail, None).await.unwrap();
        assert_eq!(repo.find(&*tx, Side::Fastmail).await.unwrap().unwrap().sync_token, None);
    }

    #[tokio::test]
    async fn set_sync_token_before_discovery_is_not_found() {
        let svc = setup().await;
        let tx = svc.repository().begin().await.unwrap();
        let err = svc
            .endpoint_repository()
            .set_sync_token(&*tx, Side::ICloud, Some("t".to_owned()))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::RepositoryError(RepositoryError::NotFound)), "{err:?}");
    }

    #[tokio::test]
    async fn rediscovery_with_same_url_keeps_sync_token() {
        let svc = setup().await;
        let repo = svc.endpoint_repository();
        let tx = svc.repository().begin().await.unwrap();
        repo.upsert_discovery(&*tx, Side::ICloud, URL, HOST).await.unwrap();
        repo.set_sync_token(&*tx, Side::ICloud, Some("token-1".to_owned())).await.unwrap();

        let endpoint = repo.upsert_discovery(&*tx, Side::ICloud, URL, "p43-contacts.icloud.com").await.unwrap();
        assert_eq!(endpoint.discovered_host, "p43-contacts.icloud.com");
        assert_eq!(endpoint.sync_token.as_deref(), Some("token-1"));
    }

    #[tokio::test]
    async fn rediscovery_with_new_url_clears_sync_token() {
        let svc = setup().await;
        let repo = svc.endpoint_repository();
        let tx = svc.repository().begin().await.unwrap();
        repo.upsert_discovery(&*tx, Side::ICloud, URL, HOST).await.unwrap();
        repo.set_sync_token(&*tx, Side::ICloud, Some("token-1".to_owned())).await.unwrap();

        let moved = "https://p43-contacts.icloud.com/123/carddavhome/card-2/";
        let endpoint = repo.upsert_discovery(&*tx, Side::ICloud, moved, "p43-contacts.icloud.com").await.unwrap();
        assert_eq!(endpoint.addressbook_url, moved);
        assert_eq!(endpoint.sync_token, None);
    }

    #[tokio::test]
    async fn delete_all_removes_every_row() {
        let svc = setup().await;
        let repo = svc.endpoint_repository();
        let tx = svc.repository().begin().await.unwrap();
        repo.upsert_discovery(&*tx, Side::ICloud, URL, HOST).await.unwrap();
        repo.upsert_discovery(&*tx, Side::Fastmail, "https://carddav.fastmail.com/dav/", "carddav.fastmail.com")
            .await
            .unwrap();

        assert_eq!(repo.delete_all(&*tx).await.unwrap(), 2);
        assert_eq!(repo.find(&*tx, Side::ICloud).await.unwrap(), None);
    }

    #[tokio::test]
    async fn upsert_in_read_only_transaction_is_read_only() {
        let svc = setup().await;
        let tx = svc.repository().begin_read_only().await.unwrap();
        let err = svc.endpoint_repository().upsert_discovery(&*tx, Side::ICloud, URL, HOST).await.unwrap_err();
        assert!(matches!(err, Error::RepositoryError(RepositoryError::ReadOnly)), "{err:?}");
    }

    #[tokio::test]
    async fn corrupt_side_is_database_error() {
        let svc = setup().await;
        let tx = svc.repository().begin().await.unwrap();
        let db_tx = TransactionImpl::get_db_transaction(&*tx).unwrap();
        let now = chrono::Utc::now();
        endpoints::ActiveModel {
            version: Set(0),
            side: Set("google".to_owned()),
            addressbook_url: Set(URL.to_owned()),
            discovered_host: Set(HOST.to_owned()),
            sync_token: Set(None),
            created_at: Set(now.into()),
            updated_at: Set(now.into()),
            ..Default::default()
        }
        .insert(db_tx)
        .await
        .unwrap();

        // `find` filters by a known side, so convert the stored row directly.
        let model = prelude::Endpoints::find().one(db_tx).await.unwrap().unwrap();
        let err = Endpoint::try_from(model).unwrap_err();
        let Error::RepositoryError(RepositoryError::Database(message)) = err else {
            panic!("expected Database error, got {err:?}");
        };
        assert!(message.contains("endpoints") && message.contains("side"), "{message}");
    }
}
