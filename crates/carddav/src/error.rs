use std::time::{Duration, SystemTime};

use cg_core::{AddressBookError, contact::Href};
use reqwest::{StatusCode, header::HeaderValue};

/// Longest `Retry-After` passed on; larger server values are clamped.
pub(crate) const MAX_RETRY_AFTER: Duration = Duration::from_secs(3600);

/// Maps a non-success HTTP status onto the port's error taxonomy. `context`
/// is `"METHOD /path"`: never a body (PII).
pub(crate) fn status_error(context: &str, status: StatusCode, retry_after: Option<&HeaderValue>, href: Option<&Href>) -> AddressBookError {
    let code = status.as_u16();
    match code {
        401 => AddressBookError::Unauthorized,
        412 => match href {
            Some(href) => AddressBookError::PreconditionFailed { href: href.clone() },
            None => AddressBookError::Permanent(format!("{context}: HTTP {code}")),
        },
        429 | 503 => AddressBookError::RateLimited {
            retry_after: retry_after
                .and_then(|value| value.to_str().ok())
                .and_then(|value| parse_retry_after(value, SystemTime::now())),
        },
        500..=599 => AddressBookError::Transient(format!("{context}: HTTP {code}")),
        _ => AddressBookError::Permanent(format!("{context}: HTTP {code}")),
    }
}

/// Maps a transport failure (no HTTP status) onto the port's taxonomy.
pub(crate) fn transport_error(context: &str, error: &reqwest::Error) -> AddressBookError {
    if error.is_builder() {
        return AddressBookError::Permanent(format!("{context}: invalid request"));
    }
    let what = if error.is_timeout() {
        "timed out"
    } else if error.is_connect() {
        "connection failed"
    } else if error.is_body() || error.is_decode() {
        "reading the response failed"
    } else {
        "request failed"
    };
    AddressBookError::Transient(format!("{context}: {what}"))
}

/// Parses `Retry-After` as delta-seconds or an HTTP-date. A date in the past
/// is zero; anything longer than `MAX_RETRY_AFTER` is clamped; garbage is
/// `None`.
pub(crate) fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    let delay = if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
        value.parse::<u64>().map_or(MAX_RETRY_AFTER, Duration::from_secs)
    } else {
        let at = httpdate::parse_http_date(value).ok()?;
        at.duration_since(now).unwrap_or(Duration::ZERO)
    };
    Some(delay.min(MAX_RETRY_AFTER))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> SystemTime {
        httpdate::parse_http_date("Wed, 21 Oct 2015 07:28:00 GMT").unwrap()
    }

    #[test]
    fn retry_after_delta_seconds() {
        assert_eq!(parse_retry_after("30", now()), Some(Duration::from_secs(30)));
        assert_eq!(parse_retry_after(" 0 ", now()), Some(Duration::ZERO));
    }

    #[test]
    fn retry_after_http_date() {
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:30:00 GMT", now()), Some(Duration::from_secs(120)));
    }

    #[test]
    fn retry_after_in_the_past_is_zero() {
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:00:00 GMT", now()), Some(Duration::ZERO));
    }

    #[test]
    fn retry_after_is_clamped() {
        assert_eq!(parse_retry_after("86400", now()), Some(MAX_RETRY_AFTER));
        assert_eq!(parse_retry_after("99999999999999999999999", now()), Some(MAX_RETRY_AFTER));
        assert_eq!(parse_retry_after("Thu, 22 Oct 2015 07:28:00 GMT", now()), Some(MAX_RETRY_AFTER));
    }

    #[test]
    fn retry_after_garbage_is_none() {
        assert_eq!(parse_retry_after("soon", now()), None);
        assert_eq!(parse_retry_after("", now()), None);
        assert_eq!(parse_retry_after("-5", now()), None);
    }

    #[test]
    fn status_mapping() {
        let href = Href::from("/card/a.vcf");
        let ctx = "PUT /card/a.vcf";
        assert_eq!(status_error(ctx, StatusCode::UNAUTHORIZED, None, None), AddressBookError::Unauthorized);
        assert_eq!(
            status_error(ctx, StatusCode::PRECONDITION_FAILED, None, Some(&href)),
            AddressBookError::PreconditionFailed { href: href.clone() }
        );
        assert_eq!(
            status_error(ctx, StatusCode::PRECONDITION_FAILED, None, None),
            AddressBookError::Permanent("PUT /card/a.vcf: HTTP 412".into())
        );
        let thirty = HeaderValue::from_static("30");
        assert_eq!(
            status_error(ctx, StatusCode::TOO_MANY_REQUESTS, Some(&thirty), None),
            AddressBookError::RateLimited {
                retry_after: Some(Duration::from_secs(30))
            }
        );
        assert_eq!(
            status_error(ctx, StatusCode::SERVICE_UNAVAILABLE, None, None),
            AddressBookError::RateLimited { retry_after: None }
        );
        assert_eq!(
            status_error(ctx, StatusCode::BAD_GATEWAY, None, None),
            AddressBookError::Transient("PUT /card/a.vcf: HTTP 502".into())
        );
        assert_eq!(
            status_error(ctx, StatusCode::INTERNAL_SERVER_ERROR, None, None),
            AddressBookError::Transient("PUT /card/a.vcf: HTTP 500".into())
        );
        assert_eq!(
            status_error(ctx, StatusCode::NOT_FOUND, None, None),
            AddressBookError::Permanent("PUT /card/a.vcf: HTTP 404".into())
        );
        assert_eq!(
            status_error(ctx, StatusCode::FORBIDDEN, None, None),
            AddressBookError::Permanent("PUT /card/a.vcf: HTTP 403".into())
        );
        assert_eq!(
            status_error(ctx, StatusCode::MOVED_PERMANENTLY, None, None),
            AddressBookError::Permanent("PUT /card/a.vcf: HTTP 301".into())
        );
    }
}
