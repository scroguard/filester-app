//! Rate-limit tracking and retry/backoff policy.
//!
//! Filester allows 1,000 requests per hour per key. Responses may carry
//! `X-RateLimit-Limit`, `X-RateLimit-Remaining` and `X-RateLimit-Reset`.
//! Exceeding the limit yields HTTP 429 with `Retry-After`.
//!
//! The client uses this module in two ways:
//!
//! 1. **Proactive:** [`RateLimiter`] remembers the last observed budget. When
//!    `remaining == 0` and the reset is in the future, the next request waits
//!    for the reset instead of spending a request on a guaranteed 429. Every
//!    task sharing the client observes the same state.
//! 2. **Reactive:** on 429 (and, for idempotent requests, 502/503/504 and
//!    transient transport errors) the request is retried after
//!    [`RetryPolicy::delay_for`]. That is exponential backoff with full
//!    jitter, never shorter than the server's `Retry-After`.
//!
//! Waits longer than [`RetryPolicy::max_wait`] are never taken silently.
//! The caller gets [`ApiError::RateLimited`](super::ApiError::RateLimited)
//! with the required wait, so the UI can tell the user.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{HeaderMap, HeaderValue};

/// Header names, lowercase as `http` normalises them.
const H_LIMIT: &str = "x-ratelimit-limit";
const H_REMAINING: &str = "x-ratelimit-remaining";
const H_RESET: &str = "x-ratelimit-reset";

/// `X-RateLimit-Reset` values at or above this are read as Unix timestamps
/// (seconds). Smaller values are read as seconds-from-now. The API docs do
/// not specify the format, and 10^9 seconds (2001-09-09) cleanly separates
/// the two: no plausible delta is that large.
const EPOCH_THRESHOLD: u64 = 1_000_000_000;

/// The most recently observed rate-limit budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitInfo {
    /// Requests allowed per window.
    pub limit: Option<u32>,
    /// Requests left in the current window.
    pub remaining: Option<u32>,
    /// When the window resets.
    pub reset_at: Option<SystemTime>,
}

impl RateLimitInfo {
    /// Parse the `X-RateLimit-*` headers.
    ///
    /// Returns `None` when none of the headers is present. Individual
    /// malformed headers are ignored rather than failing the response.
    #[must_use]
    pub fn from_headers(headers: &HeaderMap, now: SystemTime) -> Option<Self> {
        let limit = header_u64(headers, H_LIMIT).and_then(|v| u32::try_from(v).ok());
        let remaining = header_u64(headers, H_REMAINING).and_then(|v| u32::try_from(v).ok());
        let reset_at = header_u64(headers, H_RESET).and_then(|v| parse_reset(v, now));
        if limit.is_none() && remaining.is_none() && reset_at.is_none() {
            None
        } else {
            Some(Self {
                limit,
                remaining,
                reset_at,
            })
        }
    }
}

fn header_u64(headers: &HeaderMap, name: &str) -> Option<u64> {
    headers.get(name)?.to_str().ok()?.trim().parse().ok()
}

/// Interpret an `X-RateLimit-Reset` value as an absolute instant.
fn parse_reset(value: u64, now: SystemTime) -> Option<SystemTime> {
    if value >= EPOCH_THRESHOLD {
        UNIX_EPOCH.checked_add(Duration::from_secs(value))
    } else {
        now.checked_add(Duration::from_secs(value))
    }
}

/// Parse `Retry-After` as delta-seconds or an HTTP-date (RFC 9110 §10.2.3).
///
/// A date in the past yields `Duration::ZERO`. Garbage yields `None`.
#[must_use]
pub(crate) fn parse_retry_after(value: &HeaderValue, now: SystemTime) -> Option<Duration> {
    let text = value.to_str().ok()?.trim();
    if let Ok(secs) = text.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let at = httpdate::parse_http_date(text).ok()?;
    Some(at.duration_since(now).unwrap_or(Duration::ZERO))
}

/// Retry and backoff limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total attempts including the first, so `1` disables retries.
    pub max_attempts: u32,
    /// Backoff ceiling for the first retry. Doubles per retry.
    pub base_delay: Duration,
    /// Upper bound on the exponential backoff ceiling.
    pub max_backoff: Duration,
    /// Longest single wait the client will take on its own, whether from
    /// `Retry-After` or a proactive rate-limit pause. Longer waits surface
    /// as [`ApiError::RateLimited`](super::ApiError::RateLimited).
    pub max_wait: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_delay: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
            max_wait: Duration::from_secs(120),
        }
    }
}

impl RetryPolicy {
    /// Exponential ceiling for retry number `retry` (0-based):
    /// `min(max_backoff, base_delay × 2^retry)`, saturating.
    #[must_use]
    pub fn backoff_ceiling(&self, retry: u32) -> Duration {
        let factor = 1u32.checked_shl(retry.min(31)).unwrap_or(u32::MAX);
        self.base_delay.saturating_mul(factor).min(self.max_backoff)
    }

    /// Delay before retry number `retry` (0-based).
    ///
    /// Uses "full jitter" (`ceiling × jitter`, `jitter ∈ [0, 1]`) to spread
    /// concurrent clients apart. If the server sent `Retry-After`, that value
    /// is a hard lower bound. `jitter` is a parameter so the function stays
    /// pure and testable. Out-of-range values are clamped.
    #[must_use]
    pub fn delay_for(&self, retry: u32, retry_after: Option<Duration>, jitter: f64) -> Duration {
        let jitter = if jitter.is_finite() {
            jitter.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let jittered = self.backoff_ceiling(retry).mul_f64(jitter);
        retry_after.map_or(jittered, |server| server.max(jittered))
    }
}

/// Shared, thread-safe view of the current rate-limit budget.
///
/// The lock is held only for field copies and never across an `.await`.
#[derive(Debug, Default)]
pub(crate) struct RateLimiter {
    state: Mutex<Option<RateLimitInfo>>,
}

impl RateLimiter {
    /// Record headers from a response.
    pub(crate) fn observe(&self, info: RateLimitInfo) {
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        *guard = Some(info);
    }

    /// Record a 429: the budget is exhausted until `until`.
    pub(crate) fn block_until(&self, until: SystemTime) {
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let limit = guard.and_then(|i| i.limit);
        *guard = Some(RateLimitInfo {
            limit,
            remaining: Some(0),
            reset_at: Some(until),
        });
    }

    /// Latest known budget.
    pub(crate) fn snapshot(&self) -> Option<RateLimitInfo> {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// How long to wait before sending, if the budget is known to be
    /// exhausted.
    pub(crate) fn wait_needed(&self, now: SystemTime) -> Option<Duration> {
        let info = self.snapshot()?;
        if info.remaining != Some(0) {
            return None;
        }
        info.reset_at?
            .duration_since(now)
            .ok()
            .filter(|d| !d.is_zero())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        map
    }

    fn t(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn parses_all_headers_with_epoch_reset() {
        let now = t(1_800_000_000);
        let info = RateLimitInfo::from_headers(
            &headers(&[
                ("X-RateLimit-Limit", "1000"),
                ("X-RateLimit-Remaining", "998"),
                ("X-RateLimit-Reset", "1800000600"),
            ]),
            now,
        )
        .unwrap();
        assert_eq!(info.limit, Some(1000));
        assert_eq!(info.remaining, Some(998));
        assert_eq!(info.reset_at, Some(t(1_800_000_600)));
    }

    #[test]
    fn small_reset_is_relative() {
        let now = t(1_800_000_000);
        let info =
            RateLimitInfo::from_headers(&headers(&[("x-ratelimit-reset", "90")]), now).unwrap();
        assert_eq!(info.reset_at, Some(t(1_800_000_090)));
    }

    #[test]
    fn absent_or_garbage_headers() {
        let now = t(0);
        assert!(RateLimitInfo::from_headers(&HeaderMap::new(), now).is_none());
        assert!(
            RateLimitInfo::from_headers(&headers(&[("x-ratelimit-remaining", "lots")]), now)
                .is_none()
        );
        let partial = RateLimitInfo::from_headers(
            &headers(&[("x-ratelimit-remaining", "5"), ("x-ratelimit-limit", "-1")]),
            now,
        )
        .unwrap();
        assert_eq!(partial.remaining, Some(5));
        assert_eq!(partial.limit, None);
    }

    #[test]
    fn retry_after_seconds_and_http_date() {
        let now = t(1_445_412_480); // Wed, 21 Oct 2015 07:28:00 GMT
        let secs = HeaderValue::from_static(" 120 ");
        assert_eq!(
            parse_retry_after(&secs, now),
            Some(Duration::from_secs(120))
        );
        let date = HeaderValue::from_static("Wed, 21 Oct 2015 07:28:30 GMT");
        assert_eq!(parse_retry_after(&date, now), Some(Duration::from_secs(30)));
        let past = HeaderValue::from_static("Wed, 21 Oct 2015 07:00:00 GMT");
        assert_eq!(parse_retry_after(&past, now), Some(Duration::ZERO));
        assert_eq!(
            parse_retry_after(&HeaderValue::from_static("soon"), now),
            None
        );
        assert_eq!(
            parse_retry_after(&HeaderValue::from_static("-5"), now),
            None
        );
    }

    #[test]
    fn backoff_doubles_and_saturates() {
        let p = RetryPolicy {
            base_delay: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
            ..RetryPolicy::default()
        };
        assert_eq!(p.backoff_ceiling(0), Duration::from_millis(500));
        assert_eq!(p.backoff_ceiling(1), Duration::from_secs(1));
        assert_eq!(p.backoff_ceiling(3), Duration::from_secs(4));
        assert_eq!(p.backoff_ceiling(10), Duration::from_secs(30));
        assert_eq!(p.backoff_ceiling(u32::MAX), Duration::from_secs(30));
    }

    #[test]
    fn delay_applies_full_jitter_and_retry_after_floor() {
        let p = RetryPolicy::default();
        assert_eq!(p.delay_for(2, None, 0.0), Duration::ZERO);
        assert_eq!(p.delay_for(2, None, 1.0), Duration::from_secs(2));
        assert_eq!(p.delay_for(2, None, 0.5), Duration::from_secs(1));
        // Retry-After wins when larger than the jittered backoff…
        assert_eq!(
            p.delay_for(0, Some(Duration::from_secs(7)), 1.0),
            Duration::from_secs(7)
        );
        // …and the jittered backoff wins when larger than Retry-After.
        assert_eq!(
            p.delay_for(3, Some(Duration::from_secs(1)), 1.0),
            Duration::from_secs(4)
        );
        // Bad jitter values are clamped.
        assert_eq!(p.delay_for(1, None, f64::NAN), Duration::ZERO);
        assert_eq!(p.delay_for(1, None, 9.0), Duration::from_secs(1));
    }

    #[test]
    fn limiter_waits_only_when_exhausted_and_reset_in_future() {
        let limiter = RateLimiter::default();
        let now = t(1_000);
        assert_eq!(limiter.wait_needed(now), None);

        limiter.observe(RateLimitInfo {
            limit: Some(1000),
            remaining: Some(3),
            reset_at: Some(t(2_000)),
        });
        assert_eq!(limiter.wait_needed(now), None);

        limiter.block_until(t(1_030));
        assert_eq!(limiter.wait_needed(now), Some(Duration::from_secs(30)));
        assert_eq!(limiter.snapshot().unwrap().limit, Some(1000));
        assert_eq!(limiter.wait_needed(t(1_030)), None);
        assert_eq!(limiter.wait_needed(t(5_000)), None);
    }
}
