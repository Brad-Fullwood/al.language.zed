//! The retry policy both HTTP download paths follow.

use std::time::Duration;

/// Longest a server may push a retry out. A `Retry-After` beyond this, or a
/// backoff that grows past it, is clamped: a congestion signal must not stall
/// symbol download indefinitely.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

/// Statuses worth retrying: rate limiting and the transient gateway failures.
/// Anything else is the server's answer, not a hiccup.
pub(crate) fn is_retryable_status(status: u16) -> bool {
    matches!(status, 429 | 502 | 503 | 504)
}

/// How long to wait before `attempt`'s retry, `attempt` counting from zero.
///
/// The server's `Retry-After` wins when it sends a parseable one; otherwise
/// the wait doubles from 200ms per attempt. Either way it is capped at
/// [`MAX_RETRY_DELAY`].
pub(crate) fn retry_delay(headers: &reqwest::header::HeaderMap, attempt: u32) -> Duration {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_millis(200 * (1u64 << attempt.min(16))))
        .min(MAX_RETRY_DELAY)
}

#[cfg(test)]
mod tests {
    use super::{is_retryable_status, retry_delay, MAX_RETRY_DELAY};
    use std::time::Duration;

    fn headers_with(retry_after: Option<&str>) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(value) = retry_after {
            headers.insert(
                reqwest::header::RETRY_AFTER,
                value.parse().expect("header value"),
            );
        }
        headers
    }

    #[test]
    fn only_rate_limiting_and_gateway_failures_are_retried() {
        for status in [429, 502, 503, 504] {
            assert!(is_retryable_status(status), "{status}");
        }
        for status in [200, 301, 400, 401, 403, 404, 500, 501] {
            assert!(!is_retryable_status(status), "{status}");
        }
    }

    #[test]
    fn backoff_doubles_from_two_hundred_milliseconds() {
        let headers = headers_with(None);
        assert_eq!(retry_delay(&headers, 0), Duration::from_millis(200));
        assert_eq!(retry_delay(&headers, 1), Duration::from_millis(400));
        assert_eq!(retry_delay(&headers, 2), Duration::from_millis(800));
    }

    #[test]
    fn a_parseable_retry_after_header_wins() {
        assert_eq!(
            retry_delay(&headers_with(Some("7")), 0),
            Duration::from_secs(7)
        );
    }

    #[test]
    fn an_unparseable_retry_after_header_falls_back_to_the_backoff() {
        // HTTP also allows a date here, which this parser does not read.
        assert_eq!(
            retry_delay(&headers_with(Some("Wed, 21 Oct 2026 07:28:00 GMT")), 1),
            Duration::from_millis(400)
        );
    }

    #[test]
    fn neither_the_header_nor_the_backoff_can_exceed_the_cap() {
        assert_eq!(
            retry_delay(&headers_with(Some("86400")), 0),
            MAX_RETRY_DELAY
        );
        assert_eq!(retry_delay(&headers_with(None), 30), MAX_RETRY_DELAY);
    }
}
