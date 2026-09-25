use std::time::Duration;

use crate::contact::{Href, VCardError};

/// Categorizes errors for response mapping in adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Resource not found.
    NotFound,
    /// Resource conflict, e.g., optimistic locking failure.
    Conflict,
    /// Invalid input or constraint violation.
    InvalidInput,
    /// Malformed request data.
    BadRequest,
    /// Internal or infrastructure error.
    Internal,
    /// Service temporarily unavailable (transient — DB or storage unreachable).
    ServiceUnavailable,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    #[error("Invalid ID: {0}")]
    InvalidId(u64),

    #[error("Invalid page size: {0}")]
    InvalidPageSize(u64),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Invalid transaction type")]
    InvalidTransactionType,

    #[error("Invalid token: {0}")]
    InvalidToken(String),

    #[error("Infrastructure error: {0}")]
    Infrastructure(String),

    /// A port method that is defined but not yet implemented in this milestone.
    /// Returned by IMAP lifecycle stubs
    /// (`start_account`/`stop_account`/`status`) until MK-7 fills them in.
    /// Never panics the runtime.
    #[error("Not implemented: {0}")]
    Unimplemented(&'static str),

    #[error(transparent)]
    RepositoryError(#[from] RepositoryError),

    #[error(transparent)]
    VCard(#[from] VCardError),

    #[error(transparent)]
    AddressBook(#[from] AddressBookError),

    #[cfg(any(test, feature = "test-support"))]
    #[error("Mock not configured: {0}")]
    MockNotConfigured(&'static str),
}

impl Error {
    /// Returns the error kind for response mapping in adapters.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::InvalidId(_) | Self::InvalidPageSize(_) | Self::InvalidToken(_) => ErrorKind::BadRequest,
            Self::Validation(_) | Self::VCard(_) => ErrorKind::InvalidInput,
            Self::InvalidTransactionType | Self::Infrastructure(_) | Self::Unimplemented(_) => ErrorKind::Internal,
            Self::RepositoryError(e) => e.kind(),
            Self::AddressBook(e) => e.kind(),
            #[cfg(any(test, feature = "test-support"))]
            Self::MockNotConfigured(_) => ErrorKind::Internal,
        }
    }

    /// Returns `true` for errors caused by transient infrastructure failures
    /// (DB connectivity loss, NFS unavailable). The server can recover without
    /// a restart; subsystems should retry rather than propagating these.
    /// Also CardDAV rate limiting and transient server or network failures.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::RepositoryError(RepositoryError::Connection(_) | RepositoryError::Busy(_))
                | Self::AddressBook(AddressBookError::RateLimited { .. } | AddressBookError::Transient(_))
        )
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum RepositoryError {
    #[error("Constraint Error - {0}")]
    Constraint(String),

    #[error("Conflict Error")]
    Conflict,

    #[error("Not found")]
    NotFound,

    #[error("Read-only Transaction")]
    ReadOnly,

    #[error("Database error: {0}")]
    Database(String),

    #[error("Query canceled")]
    QueryCanceled,

    /// Database connection lost (DNS failure, network partition, pool
    /// exhausted). Transient — callers should retry.
    #[error("Connection Error: {0}")]
    Connection(String),

    /// Database temporarily locked/busy (SQLite `SQLITE_BUSY`/`SQLITE_LOCKED`,
    /// or an equivalent serialization conflict). Transient — the lock clears
    /// once the holding transaction commits, so callers should retry.
    #[error("Database busy: {0}")]
    Busy(String),
}

impl RepositoryError {
    /// Returns the error kind for response mapping in adapters.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::NotFound => ErrorKind::NotFound,
            Self::Conflict => ErrorKind::Conflict,
            Self::Constraint(_) => ErrorKind::InvalidInput,
            Self::ReadOnly | Self::Database(_) | Self::QueryCanceled => ErrorKind::Internal,
            Self::Connection(_) | Self::Busy(_) => ErrorKind::ServiceUnavailable,
        }
    }
}

/// Failures from an `AddressBook` adapter. Payloads never carry card content
/// (PII): an href, a duration, or a short server/transport description.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AddressBookError {
    /// The `If-Match` / `If-None-Match` precondition failed (HTTP 412): the
    /// resource changed, or appeared, since the caller last saw it.
    #[error("Precondition failed for {href}")]
    PreconditionFailed { href: Href },

    /// HTTP 429 or 503. `retry_after` is the server's `Retry-After`, when it
    /// sent one. Transient.
    #[error("Rate limited (retry after {retry_after:?})")]
    RateLimited { retry_after: Option<Duration> },

    /// 5xx, timeout or connection failure. Transient — retry with backoff.
    #[error("Transient CardDAV error: {0}")]
    Transient(String),

    /// The server rejected the credentials (bad app-specific password).
    /// Permanent until the configuration changes.
    #[error("CardDAV authentication failed")]
    Unauthorized,

    /// Any other failure the next attempt will not fix: another 4xx, or a
    /// malformed response.
    #[error("CardDAV error: {0}")]
    Permanent(String),
}

impl AddressBookError {
    /// Returns the error kind for response mapping in adapters.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::PreconditionFailed { .. } => ErrorKind::Conflict,
            Self::RateLimited { .. } | Self::Transient(_) => ErrorKind::ServiceUnavailable,
            Self::Unauthorized | Self::Permanent(_) => ErrorKind::Internal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_book_error_kinds() {
        assert_eq!(AddressBookError::PreconditionFailed { href: Href::from("/a.vcf") }.kind(), ErrorKind::Conflict);
        assert_eq!(AddressBookError::RateLimited { retry_after: None }.kind(), ErrorKind::ServiceUnavailable);
        assert_eq!(AddressBookError::Transient("timeout".into()).kind(), ErrorKind::ServiceUnavailable);
        assert_eq!(AddressBookError::Unauthorized.kind(), ErrorKind::Internal);
        assert_eq!(AddressBookError::Permanent("malformed multistatus".into()).kind(), ErrorKind::Internal);
        assert_eq!(Error::from(AddressBookError::Unauthorized).kind(), ErrorKind::Internal);
    }

    #[test]
    fn rate_limited_and_transient_address_book_errors_are_transient() {
        assert!(
            Error::from(AddressBookError::RateLimited {
                retry_after: Some(Duration::from_secs(30))
            })
            .is_transient()
        );
        assert!(Error::from(AddressBookError::Transient("503".into())).is_transient());
        assert!(!Error::from(AddressBookError::PreconditionFailed { href: Href::from("/a.vcf") }).is_transient());
        assert!(!Error::from(AddressBookError::Unauthorized).is_transient());
        assert!(!Error::from(AddressBookError::Permanent("400".into())).is_transient());
    }

    #[test]
    fn is_transient_connection_error() {
        let e = Error::RepositoryError(RepositoryError::Connection("timeout".into()));
        assert!(e.is_transient());
    }

    #[test]
    fn is_transient_database_busy() {
        let e = Error::RepositoryError(RepositoryError::Busy("database is locked".into()));
        assert!(e.is_transient(), "SQLite busy/locked must be transient so the worker retries");
    }

    #[test]
    fn database_busy_kind_is_service_unavailable() {
        assert_eq!(RepositoryError::Busy("x".into()).kind(), ErrorKind::ServiceUnavailable);
    }

    #[test]
    fn is_transient_returns_false_for_other_errors() {
        assert!(!Error::Infrastructure("bad".into()).is_transient());
        assert!(!Error::RepositoryError(RepositoryError::NotFound).is_transient());
        assert!(!Error::RepositoryError(RepositoryError::Constraint("dup".into())).is_transient());
        assert!(!Error::RepositoryError(RepositoryError::Database("boom".into())).is_transient());
    }

    #[test]
    fn connection_error_kind_is_service_unavailable() {
        let e = RepositoryError::Connection("x".into());
        assert_eq!(e.kind(), ErrorKind::ServiceUnavailable);
    }
}
