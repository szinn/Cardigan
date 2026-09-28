use sea_orm_migration::{
    prelude::*,
    schema::{blob, pk_auto, text, timestamp_with_time_zone},
};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(PendingRecreates::Table)
                    .if_not_exists()
                    .col(pk_auto(PendingRecreates::Id))
                    // One pending Recreate per contact: upsert replaces it.
                    .col(text(PendingRecreates::Uid).unique_key())
                    .col(text(PendingRecreates::IcloudHref))
                    .col(text(PendingRecreates::OldFastmailHref))
                    .col(text(PendingRecreates::OldFastmailUid))
                    .col(text(PendingRecreates::NewFastmailHref))
                    .col(blob(PendingRecreates::Card))
                    .col(timestamp_with_time_zone(PendingRecreates::CreatedAt))
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[derive(DeriveIden)]
pub(crate) enum PendingRecreates {
    Table,
    Id,
    Uid,
    IcloudHref,
    OldFastmailHref,
    OldFastmailUid,
    NewFastmailHref,
    Card,
    CreatedAt,
}
