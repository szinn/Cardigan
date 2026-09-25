use sea_orm_migration::{
    prelude::*,
    schema::{big_integer, pk_auto, text, timestamp_with_time_zone},
};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Endpoints::Table)
                    .if_not_exists()
                    .col(pk_auto(Endpoints::Id))
                    .col(big_integer(Endpoints::Version))
                    .col(text(Endpoints::Side).unique_key())
                    .col(text(Endpoints::AddressbookUrl))
                    .col(text(Endpoints::DiscoveredHost))
                    .col(text(Endpoints::SyncToken).null())
                    .col(timestamp_with_time_zone(Endpoints::CreatedAt))
                    .col(timestamp_with_time_zone(Endpoints::UpdatedAt))
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[derive(DeriveIden)]
pub(crate) enum Endpoints {
    Table,
    Id,
    Version,
    Side,
    AddressbookUrl,
    DiscoveredHost,
    SyncToken,
    CreatedAt,
    UpdatedAt,
}
