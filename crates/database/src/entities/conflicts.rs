// `sea_orm::model` expands to generated impls whose async trait methods have
// no internal `await`; the allow must be module-level because the lint
// attaches to macro-generated sibling items, not the annotated struct.
#![allow(clippy::unused_async_trait_impl, reason = "sea_orm::model-generated code, not user code")]

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// A recorded conflict. Both vCards are full contact data: never log a
/// `Model`. Append-only, so there is no `version` or `updated_at`.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "conflicts")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub uid: String,
    pub origin: String,
    pub winner: String,
    #[sea_orm(column_type = "Blob")]
    pub icloud_vcard: Vec<u8>,
    #[sea_orm(column_type = "Blob")]
    pub fastmail_vcard: Vec<u8>,
    pub detected_at: DateTimeWithTimeZone,
}

impl ActiveModelBehavior for ActiveModel {}
