pub use sea_orm_migration::prelude::*;

mod m20260925_000001_create_contacts_table;
mod m20260925_000002_create_endpoints_table;
mod m20260925_000003_create_conflicts_table;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260925_000001_create_contacts_table::Migration),
            Box::new(m20260925_000002_create_endpoints_table::Migration),
            Box::new(m20260925_000003_create_conflicts_table::Migration),
        ]
    }
}
