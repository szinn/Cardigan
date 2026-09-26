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
                    .table(BaselineSkips::Table)
                    .if_not_exists()
                    .col(pk_auto(BaselineSkips::Id))
                    .col(big_integer(BaselineSkips::Version))
                    .col(text(BaselineSkips::Side))
                    .col(text(BaselineSkips::Href))
                    .col(text(BaselineSkips::Uid))
                    .col(text(BaselineSkips::ContentHash))
                    .col(big_integer(BaselineSkips::HashVersion))
                    .col(big_integer(BaselineSkips::CandidateCount))
                    .col(timestamp_with_time_zone(BaselineSkips::SkippedAt))
                    .col(timestamp_with_time_zone(BaselineSkips::CreatedAt))
                    .col(timestamp_with_time_zone(BaselineSkips::UpdatedAt))
                    .to_owned(),
            )
            .await?;
        // One row per skipped resource.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_baseline_skips_side_href")
                    .table(BaselineSkips::Table)
                    .col(BaselineSkips::Side)
                    .col(BaselineSkips::Href)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[derive(DeriveIden)]
pub(crate) enum BaselineSkips {
    Table,
    Id,
    Version,
    Side,
    Href,
    Uid,
    ContentHash,
    HashVersion,
    CandidateCount,
    SkippedAt,
    CreatedAt,
    UpdatedAt,
}
