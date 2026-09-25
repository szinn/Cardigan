use sea_orm_migration::{
    prelude::*,
    schema::{big_integer, blob, boolean, pk_auto, text, timestamp_with_time_zone},
};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Contacts::Table)
                    .if_not_exists()
                    .col(pk_auto(Contacts::Id))
                    .col(big_integer(Contacts::Version))
                    .col(text(Contacts::Uid).unique_key())
                    .col(text(Contacts::IcloudHref).unique_key())
                    .col(text(Contacts::IcloudEtag))
                    .col(timestamp_with_time_zone(Contacts::IcloudLastSeenAt))
                    .col(text(Contacts::FastmailHref).unique_key())
                    .col(text(Contacts::FastmailEtag))
                    .col(timestamp_with_time_zone(Contacts::FastmailLastSeenAt))
                    .col(text(Contacts::ContentHash))
                    .col(big_integer(Contacts::HashVersion))
                    .col(boolean(Contacts::PhotoStripped))
                    .col(blob(Contacts::LastSyncedVcard))
                    .col(timestamp_with_time_zone(Contacts::LastSyncedAt))
                    .col(timestamp_with_time_zone(Contacts::CreatedAt))
                    .col(timestamp_with_time_zone(Contacts::UpdatedAt))
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[derive(DeriveIden)]
pub(crate) enum Contacts {
    Table,
    Id,
    Version,
    Uid,
    IcloudHref,
    IcloudEtag,
    IcloudLastSeenAt,
    FastmailHref,
    FastmailEtag,
    FastmailLastSeenAt,
    ContentHash,
    HashVersion,
    PhotoStripped,
    LastSyncedVcard,
    LastSyncedAt,
    CreatedAt,
    UpdatedAt,
}
