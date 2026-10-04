pub use sea_orm_migration::prelude::*;

mod m20260925_000001_create_contacts_table;
mod m20260925_000002_create_endpoints_table;
mod m20260925_000003_create_conflicts_table;
mod m20260925_000004_create_card_failures_table;
mod m20260925_000005_create_baseline_skips_table;
mod m20260928_000006_create_pending_recreates_table;
mod m20261002_000007_add_photo_columns;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260925_000001_create_contacts_table::Migration),
            Box::new(m20260925_000002_create_endpoints_table::Migration),
            Box::new(m20260925_000003_create_conflicts_table::Migration),
            Box::new(m20260925_000004_create_card_failures_table::Migration),
            Box::new(m20260925_000005_create_baseline_skips_table::Migration),
            Box::new(m20260928_000006_create_pending_recreates_table::Migration),
            Box::new(m20261002_000007_add_photo_columns::Migration),
        ]
    }
}
