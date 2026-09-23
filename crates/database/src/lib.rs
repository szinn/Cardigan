// SeaORM uses i64 for all primary keys; domain types use u64. Auto-increment
// IDs are always positive and will not exceed i64::MAX in practice, so these
// casts are safe at this boundary. Page sizes/counts are similarly bounded.
#![allow(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "SeaORM i64/u64 boundary — IDs and page values are always in range"
)]

use std::sync::Arc;

use cg_core::{
    Error,
    repository::{Repository, RepositoryService, RepositoryServiceBuilder},
};
use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;

pub mod error;

pub use error::*;

use crate::{migrations::Migrator, repository::RepositoryImpl};

mod migrations;
mod repository;
mod transaction;

pub async fn open_database(database_path: &str) -> Result<DatabaseConnection, Error> {
    tracing::debug!("Connecting to database...");
    let mut opt = ConnectOptions::new(database_path);
    opt.max_connections(9)
        .min_connections(5)
        .sqlx_logging(true)
        .sqlx_logging_level(tracing::log::LevelFilter::Info);

    // For SQLite, apply PRAGMAs that sqlx does not set by default.
    // We use map_sqlx_sqlite_opts rather than URL query parameters because
    // sqlx-sqlite's URL parser only recognises mode/cache/immutable/vfs —
    // pragma names are not valid query parameters and will cause a parse error.
    if database_path.starts_with("sqlite:") {
        // SQLite permits only one writer at a time. With a multi-connection
        // pool, the read→write transaction pattern (claim a job, then
        // write) hits SQLITE_BUSY/BUSY_SNAPSHOT deadlocks that
        // `busy_timeout` cannot resolve — a stale snapshot fails
        // immediately rather than waiting. Serialize all access through
        // a single connection: correct and ample for an embedded,
        // single-process archiver. (WAL + synchronous=NORMAL are still set
        // below for crash-safety and durability.)
        opt.max_connections(1).min_connections(1);

        opt.map_sqlx_sqlite_opts(|o| {
            use std::time::Duration;

            use sqlx::sqlite::{SqliteJournalMode, SqliteSynchronous};
            o.journal_mode(SqliteJournalMode::Wal)
                .busy_timeout(Duration::from_secs(5))
                .synchronous(SqliteSynchronous::Normal)
                .foreign_keys(true)
        });
    }

    Ok(Database::connect(opt).await.map_err(handle_dberr)?)
}

pub async fn create_repository_service(database: DatabaseConnection) -> Result<Arc<RepositoryService>, Error> {
    let span = tracing::span!(tracing::Level::TRACE, "Migrations").entered();
    Migrator::up(&database, None).await.map_err(handle_dberr)?;
    span.exit();

    let repository_service = RepositoryServiceBuilder::default()
        .repository(Arc::new(RepositoryImpl::new(database)) as Arc<dyn Repository>)
        .build()
        .map_err(|e| Error::Infrastructure(e.to_string()))?;

    Ok(Arc::new(repository_service))
}
