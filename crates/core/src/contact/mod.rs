//! Contacts: identifiers shared by both sides and vCard handling. Pure — no
//! I/O, no logging.

mod model;

pub use model::{ConflictWinner, ETag, Href, Side, Uid};
