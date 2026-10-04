use sea_orm_migration::prelude::*;

use super::m20260925_000001_create_contacts_table::Contacts;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[derive(DeriveIden)]
enum PhotoColumns {
    IcloudPhotoUri,
    IcloudPhotoHash,
    FastmailPhotoHash,
    PhotoTracked,
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // SQLite adds one column per ALTER TABLE.
        for column in [
            ColumnDef::new(PhotoColumns::IcloudPhotoUri).text().null().to_owned(),
            ColumnDef::new(PhotoColumns::IcloudPhotoHash).text().null().to_owned(),
            ColumnDef::new(PhotoColumns::FastmailPhotoHash).text().null().to_owned(),
            ColumnDef::new(PhotoColumns::PhotoTracked).boolean().not_null().default(false).to_owned(),
        ] {
            manager.alter_table(Table::alter().table(Contacts::Table).add_column(column).to_owned()).await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
