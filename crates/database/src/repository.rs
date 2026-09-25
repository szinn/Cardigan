use cg_core::{
    Error,
    repository::{Repository, Transaction},
};
use sea_orm::{AccessMode, ConnectionTrait, DatabaseBackend, DatabaseConnection, DatabaseTransaction, TransactionTrait};

use crate::{error::handle_dberr, transaction::TransactionImpl};

#[derive(Clone)]
pub(crate) struct RepositoryImpl {
    database: DatabaseConnection,
}

impl RepositoryImpl {
    pub(crate) fn new(database_connection: DatabaseConnection) -> Self {
        Self { database: database_connection }
    }
}

#[async_trait::async_trait]
impl Repository for RepositoryImpl {
    async fn begin(&self) -> Result<Box<dyn Transaction>, Error> {
        let transaction = self.database.begin().await.map_err(handle_dberr)?;
        set_query_only(&transaction, false).await?;
        Ok(Box::new(TransactionImpl::new(transaction)))
    }

    async fn begin_read_only(&self) -> Result<Box<dyn Transaction>, Error> {
        let transaction = match self.database.get_database_backend() {
            DatabaseBackend::Sqlite => {
                let transaction = self.database.begin().await.map_err(handle_dberr)?;
                set_query_only(&transaction, true).await?;
                transaction
            }
            _ => self.database.begin_with_config(None, Some(AccessMode::ReadOnly)).await.map_err(handle_dberr)?,
        };
        Ok(Box::new(TransactionImpl::new(transaction)))
    }

    async fn close(&self) -> Result<(), Error> {
        self.database.clone().close().await.map_err(handle_dberr)?;

        Ok(())
    }

    async fn ping(&self) -> Result<(), Error> {
        self.database.ping().await.map_err(|e| cg_core::RepositoryError::Connection(e.to_string()))?;
        Ok(())
    }
}

/// SQLite has no read-only transactions, so a read-only transaction turns on
/// `query_only`, which makes the connection reject every write with
/// `SQLITE_READONLY`. It is a connection setting, not a transaction one, and
/// survives commit, rollback and drop. So every `begin` sets it explicitly.
/// This is sound because `open_database` gives SQLite a single pooled
/// connection.
async fn set_query_only(transaction: &DatabaseTransaction, on: bool) -> Result<(), Error> {
    if transaction.get_database_backend() != DatabaseBackend::Sqlite {
        return Ok(());
    }
    let pragma = if on { "PRAGMA query_only = ON" } else { "PRAGMA query_only = OFF" };
    transaction.execute_unprepared(pragma).await.map_err(handle_dberr)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cg_core::{
        Error, RepositoryError,
        repository::{RepositoryService, Transaction},
    };
    use sea_orm::{ConnectionTrait, Database};

    use crate::{create_repository_service, error::handle_dberr, transaction::TransactionImpl};

    async fn setup() -> Arc<RepositoryService> {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        create_repository_service(db).await.unwrap()
    }

    async fn exec(tx: &dyn Transaction, sql: &str) -> Result<(), Error> {
        TransactionImpl::get_db_transaction(tx)?.execute_unprepared(sql).await.map_err(handle_dberr)?;
        Ok(())
    }

    #[tokio::test]
    async fn read_only_transaction_rejects_writes() {
        let svc = setup().await;
        let tx = svc.repository().begin_read_only().await.unwrap();
        let err = exec(&*tx, "CREATE TABLE probe (x INTEGER)").await.unwrap_err();
        assert!(matches!(err, Error::RepositoryError(RepositoryError::ReadOnly)), "{err:?}");
    }

    #[tokio::test]
    async fn read_only_transaction_allows_reads() {
        let svc = setup().await;
        let tx = svc.repository().begin_read_only().await.unwrap();
        exec(&*tx, "SELECT 1").await.unwrap();
    }

    #[tokio::test]
    async fn read_write_after_read_only_rollback_can_write() {
        let svc = setup().await;
        let ro = svc.repository().begin_read_only().await.unwrap();
        exec(&*ro, "SELECT 1").await.unwrap();
        ro.rollback().await.unwrap();

        let tx = svc.repository().begin().await.unwrap();
        exec(&*tx, "CREATE TABLE probe (x INTEGER)").await.unwrap();
        exec(&*tx, "INSERT INTO probe VALUES (1)").await.unwrap();
        tx.commit().await.unwrap();
    }

    #[tokio::test]
    async fn read_write_after_dropped_read_only_can_write() {
        // `cg_core::repository::read_only_transaction` drops its transaction
        // without commit or rollback; the next writer must not inherit
        // `query_only`.
        let svc = setup().await;
        let ro = svc.repository().begin_read_only().await.unwrap();
        exec(&*ro, "SELECT 1").await.unwrap();
        drop(ro);

        let tx = svc.repository().begin().await.unwrap();
        exec(&*tx, "CREATE TABLE probe (x INTEGER)").await.unwrap();
        tx.commit().await.unwrap();
    }
}
