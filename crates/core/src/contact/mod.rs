//! Contacts: identifiers shared by both sides and vCard handling. Pure — no
//! I/O, no async, no tracing.

mod model;
mod vcard;

pub use model::{ConflictWinner, ETag, Href, Side, Uid};
pub use vcard::{CANONICAL_VERSION, CardHash, DisplayIdentity, HashOptions, MatchKeys, Param, Property, VCard, VCardError};
