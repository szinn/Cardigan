use std::str::FromStr;

use chrono::{DateTime, TimeDelta, Utc};

use crate::{
    AddressBookError, Error,
    contact::{ETag, Href, Side, Uid, VCardError},
    repository::Transaction,
};

pub type CardFailureId = u64;

/// Capped exponential backoff for a failing card: `base` after the first
/// failure, doubling per further failure, never more than `cap`. There is no
/// give-up state: a failure is retried until it clears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackoffPolicy {
    pub base: TimeDelta,
    pub cap: TimeDelta,
}

impl BackoffPolicy {
    /// The wait before the next retry after `attempts` consecutive failures
    /// (1 = the first failure).
    #[must_use]
    pub fn delay(&self, attempts: u32) -> TimeDelta {
        let mut delay = self.base;
        for _ in 1..attempts {
            match delay.checked_mul(2) {
                Some(doubled) if doubled < self.cap => delay = doubled,
                _ => return self.cap,
            }
        }
        delay.min(self.cap)
    }
}

/// What was being done to the card when it failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureOp {
    /// Fetching or parsing the source card.
    Read,
    Create,
    Update,
    Delete,
}

impl FailureOp {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }
}

impl FromStr for FailureOp {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "read" => Ok(Self::Read),
            "create" => Ok(Self::Create),
            "update" => Ok(Self::Update),
            "delete" => Ok(Self::Delete),
            _ => Err(format!("unknown failure op `{s}`")),
        }
    }
}

/// Why a card failed, as a category. Never holds server text, which can echo
/// card content (PII).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureReason {
    /// The card is not a valid vCard.
    InvalidCard,
    /// The card has no `UID`.
    MissingUid,
    /// The card is not vCard 3.0.
    UnsupportedVersion,
    /// The server refused a conditional write (412).
    PreconditionFailed,
    /// The server rate-limited the request (429/503).
    RateLimited,
    /// A 5xx, timeout or connection failure.
    Transient,
    /// The server rejected the credentials.
    Unauthorized,
    /// The server rejected the card or request for another reason.
    Rejected,
    /// Any other error (for example a state-store failure).
    Internal,
}

impl FailureReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidCard => "invalid_card",
            Self::MissingUid => "missing_uid",
            Self::UnsupportedVersion => "unsupported_version",
            Self::PreconditionFailed => "precondition_failed",
            Self::RateLimited => "rate_limited",
            Self::Transient => "transient",
            Self::Unauthorized => "unauthorized",
            Self::Rejected => "rejected",
            Self::Internal => "internal",
        }
    }
}

impl FromStr for FailureReason {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "invalid_card" => Ok(Self::InvalidCard),
            "missing_uid" => Ok(Self::MissingUid),
            "unsupported_version" => Ok(Self::UnsupportedVersion),
            "precondition_failed" => Ok(Self::PreconditionFailed),
            "rate_limited" => Ok(Self::RateLimited),
            "transient" => Ok(Self::Transient),
            "unauthorized" => Ok(Self::Unauthorized),
            "rejected" => Ok(Self::Rejected),
            "internal" => Ok(Self::Internal),
            _ => Err(format!("unknown failure reason `{s}`")),
        }
    }
}

impl From<&Error> for FailureReason {
    fn from(error: &Error) -> Self {
        match error {
            Error::VCard(VCardError::MissingUid) => Self::MissingUid,
            Error::VCard(VCardError::UnsupportedVersion { .. }) => Self::UnsupportedVersion,
            Error::VCard(_) => Self::InvalidCard,
            Error::AddressBook(AddressBookError::PreconditionFailed { .. }) => Self::PreconditionFailed,
            Error::AddressBook(AddressBookError::RateLimited { .. }) => Self::RateLimited,
            Error::AddressBook(AddressBookError::Transient(_)) => Self::Transient,
            Error::AddressBook(AddressBookError::Unauthorized) => Self::Unauthorized,
            Error::AddressBook(AddressBookError::Permanent(_)) => Self::Rejected,
            _ => Self::Internal,
        }
    }
}

/// A failure to record. `side` and `href` name the resource that failed: the
/// source card for read, create and update, and the destination card for
/// delete. `uid` is `None` when the card could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedCard {
    pub side: Side,
    pub href: Href,
    pub uid: Option<Uid>,
    pub op: FailureOp,
    /// The resource's ETag when it failed, so an edit can be detected.
    pub etag: Option<ETag>,
    pub reason: FailureReason,
}

/// A card that keeps failing, with its backoff state. Keyed by `side` and
/// `href`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardFailure {
    pub id: CardFailureId,
    pub version: u64,
    pub side: Side,
    pub href: Href,
    pub uid: Option<Uid>,
    pub op: FailureOp,
    pub etag: Option<ETag>,
    pub reason: FailureReason,
    /// Consecutive failures of this version of the card.
    pub attempts: u32,
    pub first_failed_at: DateTime<Utc>,
    pub last_failed_at: DateTime<Utc>,
    pub next_retry_at: DateTime<Utc>,
}

impl CardFailure {
    /// Whether to retry the card at `now`: its backoff has elapsed, or its
    /// ETag is no longer the one that failed (the user edited it).
    #[must_use]
    pub fn is_due(&self, now: DateTime<Utc>, current_etag: Option<&ETag>) -> bool {
        now >= self.next_retry_at || current_etag != self.etag.as_ref()
    }
}

#[async_trait::async_trait]
#[cfg_attr(test, mockall::automock)]
#[allow(unused_lifetimes, reason = "Generated by mockall")]
pub trait CardFailureRepository: Send + Sync {
    /// Records a failure of `failed`'s resource at `now`. The first failure
    /// inserts a row with `attempts = 1`; a repeat increments `attempts`,
    /// unless the card's ETag differs from the recorded one, which restarts
    /// the count at 1 (a new version of the card). `next_retry_at` is
    /// `now + policy.delay(attempts)`.
    async fn record_failure(&self, transaction: &dyn Transaction, failed: FailedCard, now: DateTime<Utc>, policy: &BackoffPolicy)
    -> Result<CardFailure, Error>;

    async fn find(&self, transaction: &dyn Transaction, side: Side, href: &Href) -> Result<Option<CardFailure>, Error>;

    /// Every failure, ordered by id (the cycle summary).
    async fn list_all(&self, transaction: &dyn Transaction) -> Result<Vec<CardFailure>, Error>;

    /// Removes the failure for a resource once its operation succeeds or the
    /// card is gone. Returns whether a row was removed.
    async fn clear(&self, transaction: &dyn Transaction, side: Side, href: &Href) -> Result<bool, Error>;

    /// Removes every row (`--reset`); returns how many were removed.
    async fn delete_all(&self, transaction: &dyn Transaction) -> Result<u64, Error>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> BackoffPolicy {
        BackoffPolicy {
            base: TimeDelta::seconds(60),
            cap: TimeDelta::seconds(600),
        }
    }

    #[test]
    fn backoff_doubles_from_base_and_caps() {
        let policy = policy();
        let delays: Vec<i64> = (1..=6).map(|n| policy.delay(n).num_seconds()).collect();
        assert_eq!(delays, [60, 120, 240, 480, 600, 600]);
        assert_eq!(policy.delay(u32::MAX), TimeDelta::seconds(600));
    }

    #[test]
    fn failure_op_and_reason_round_trip_through_str() {
        for op in [FailureOp::Read, FailureOp::Create, FailureOp::Update, FailureOp::Delete] {
            assert_eq!(op.as_str().parse::<FailureOp>(), Ok(op));
        }
        for reason in [
            FailureReason::InvalidCard,
            FailureReason::MissingUid,
            FailureReason::UnsupportedVersion,
            FailureReason::PreconditionFailed,
            FailureReason::RateLimited,
            FailureReason::Transient,
            FailureReason::Unauthorized,
            FailureReason::Rejected,
            FailureReason::Internal,
        ] {
            assert_eq!(reason.as_str().parse::<FailureReason>(), Ok(reason));
        }
        "oops".parse::<FailureReason>().unwrap_err();
        "merge".parse::<FailureOp>().unwrap_err();
    }

    #[test]
    fn failure_reason_categorizes_errors() {
        let cases = [
            (Error::VCard(VCardError::MissingUid), FailureReason::MissingUid),
            (
                Error::VCard(VCardError::UnsupportedVersion { version: "4.0".into() }),
                FailureReason::UnsupportedVersion,
            ),
            (Error::VCard(VCardError::MissingEnd), FailureReason::InvalidCard),
            (
                AddressBookError::PreconditionFailed { href: Href::from("/a.vcf") }.into(),
                FailureReason::PreconditionFailed,
            ),
            (AddressBookError::RateLimited { retry_after: None }.into(), FailureReason::RateLimited),
            (AddressBookError::Transient("503".into()).into(), FailureReason::Transient),
            (AddressBookError::Unauthorized.into(), FailureReason::Unauthorized),
            (
                AddressBookError::Permanent("400 Bad Request: EMAIL:jane@example.com".into()).into(),
                FailureReason::Rejected,
            ),
            (Error::Infrastructure("boom".into()), FailureReason::Internal),
        ];
        for (error, expected) in cases {
            assert_eq!(FailureReason::from(&error), expected, "{error:?}");
        }
    }

    #[test]
    fn is_due_when_backoff_elapsed_or_etag_changed() {
        let failed_at = DateTime::<Utc>::UNIX_EPOCH;
        let failure = CardFailure {
            id: 1,
            version: 1,
            side: Side::ICloud,
            href: Href::from("/a.vcf"),
            uid: None,
            op: FailureOp::Read,
            etag: Some(ETag::from("\"e1\"")),
            reason: FailureReason::MissingUid,
            attempts: 1,
            first_failed_at: failed_at,
            last_failed_at: failed_at,
            next_retry_at: failed_at + TimeDelta::seconds(60),
        };
        let same = ETag::from("\"e1\"");
        let edited = ETag::from("\"e2\"");

        assert!(!failure.is_due(failed_at + TimeDelta::seconds(59), Some(&same)));
        assert!(failure.is_due(failed_at + TimeDelta::seconds(60), Some(&same)));
        assert!(failure.is_due(failed_at + TimeDelta::seconds(1), Some(&edited)), "an edit retries at once");
        assert!(
            failure.is_due(failed_at + TimeDelta::seconds(1), None),
            "a card now without an ETag counts as changed"
        );
    }
}
