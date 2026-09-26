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
                    .table(Conflicts::Table)
                    .if_not_exists()
                    .col(pk_auto(Conflicts::Id))
                    .col(text(Conflicts::Uid))
                    .col(text(Conflicts::Origin))
                    .col(text(Conflicts::Winner))
                    .col(blob(Conflicts::IcloudVcard))
                    .col(blob(Conflicts::FastmailVcard))
                    .col(timestamp_with_time_zone(Conflicts::DetectedAt))
                    .to_owned(),
            )
            .await?;
        // Not unique: one contact can conflict many times.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_conflicts_uid")
                    .table(Conflicts::Table)
                    .col(Conflicts::Uid)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[derive(DeriveIden)]
pub(crate) enum Conflicts {
    Table,
    Id,
    Uid,
    Origin,
    Winner,
    IcloudVcard,
    FastmailVcard,
    DetectedAt,
}
