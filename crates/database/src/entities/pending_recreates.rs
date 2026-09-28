// `sea_orm::model` expands to generated impls whose async trait methods have
// no internal `await`; the allow must be module-level because the lint
// attaches to macro-generated sibling items, not the annotated struct.
#![allow(clippy::unused_async_trait_impl, reason = "sea_orm::model-generated code, not user code")]

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// A journaled Recreate. `card` is full contact data: never log a `Model`.
/// Rows are replaced or deleted, never updated in place, so there is no
/// `version` or `updated_at`.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "pending_recreates")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    #[sea_orm(unique)]
    pub uid: String,
    pub icloud_href: String,
    pub old_fastmail_href: String,
    pub old_fastmail_uid: String,
    pub new_fastmail_href: String,
    #[sea_orm(column_type = "Blob")]
    pub card: Vec<u8>,
    pub created_at: DateTimeWithTimeZone,
}

impl ActiveModelBehavior for ActiveModel {}
