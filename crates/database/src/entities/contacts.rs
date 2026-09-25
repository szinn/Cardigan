// `sea_orm::model` expands to generated impls whose async trait methods have
// no internal `await`; the allow must be module-level because the lint
// attaches to macro-generated sibling items, not the annotated struct.
#![allow(clippy::unused_async_trait_impl, reason = "sea_orm::model-generated code, not user code")]

use chrono::Utc;
use sea_orm::{ActiveValue::Set, entity::prelude::*};
use serde::{Deserialize, Serialize};

/// A contact's sync state. `last_synced_vcard` holds full contact data:
/// never log a `Model`.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "contacts")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub version: i64,
    #[sea_orm(unique)]
    pub uid: String,
    #[sea_orm(unique)]
    pub icloud_href: String,
    pub icloud_etag: String,
    pub icloud_last_seen_at: DateTimeWithTimeZone,
    #[sea_orm(unique)]
    pub fastmail_href: String,
    pub fastmail_etag: String,
    pub fastmail_last_seen_at: DateTimeWithTimeZone,
    pub content_hash: String,
    pub hash_version: i64,
    pub photo_stripped: bool,
    #[sea_orm(column_type = "Blob")]
    pub last_synced_vcard: Vec<u8>,
    pub last_synced_at: DateTimeWithTimeZone,
    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
}

#[async_trait::async_trait]
impl ActiveModelBehavior for ActiveModel {
    fn new() -> Self {
        Self {
            created_at: Set(Utc::now().into()),
            updated_at: Set(Utc::now().into()),
            ..ActiveModelTrait::default()
        }
    }

    async fn before_save<C>(mut self, _db: &C, _insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        if self.is_changed() {
            self.version = Set(self.version.unwrap() + 1);
            self.updated_at = Set(Utc::now().into());
        }
        Ok(self)
    }
}
