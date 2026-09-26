//! SeaORM entities. Must not import `cg_core`: domain conversions live in
//! `crate::adapters`.

pub(crate) mod prelude;

pub(crate) mod card_failures;
pub(crate) mod conflicts;
pub(crate) mod contacts;
pub(crate) mod endpoints;
