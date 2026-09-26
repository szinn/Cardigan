use sea_orm_migration::{
    prelude::*,
    schema::{big_integer, pk_auto, text, text_null, timestamp_with_time_zone},
};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(CardFailures::Table)
                    .if_not_exists()
                    .col(pk_auto(CardFailures::Id))
                    .col(big_integer(CardFailures::Version))
                    .col(text(CardFailures::Side))
                    .col(text(CardFailures::Href))
                    .col(text_null(CardFailures::Uid))
                    .col(text(CardFailures::Op))
                    .col(text_null(CardFailures::Etag))
                    .col(text(CardFailures::Reason))
                    .col(big_integer(CardFailures::Attempts))
                    .col(timestamp_with_time_zone(CardFailures::FirstFailedAt))
                    .col(timestamp_with_time_zone(CardFailures::LastFailedAt))
                    .col(timestamp_with_time_zone(CardFailures::NextRetryAt))
                    .col(timestamp_with_time_zone(CardFailures::CreatedAt))
                    .col(timestamp_with_time_zone(CardFailures::UpdatedAt))
                    .to_owned(),
            )
            .await?;
        // One row per failing resource.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_card_failures_side_href")
                    .table(CardFailures::Table)
                    .col(CardFailures::Side)
                    .col(CardFailures::Href)
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
pub(crate) enum CardFailures {
    Table,
    Id,
    Version,
    Side,
    Href,
    Uid,
    Op,
    Etag,
    Reason,
    Attempts,
    FirstFailedAt,
    LastFailedAt,
    NextRetryAt,
    CreatedAt,
    UpdatedAt,
}
