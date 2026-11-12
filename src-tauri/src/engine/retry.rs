//! Bounded retry policy and stable transport error classification.
//!
//! The engine retries transient transport, timeout, and server failures a
//! bounded number of times with exponential backoff and full jitter. An
//! integer-seconds `Retry-After` header is honored exactly; an HTTP-date or a
//! malformed value falls back to the short exponential delay so a far-future
//! date can never stall a task.

use std::time::Duration;

use reqwest::header::{HeaderMap, RETRY_AFTER};

use super::error::EngineError;

/// Maximum attempts one request or revalidation may make before giving up.
pub const MAX_ATTEMPTS: usize = 5;

/// Base exponential delay before a first retry.
const BASE_DELAY_MS: u64 = 50;
/// Upper bound for one exponential backoff delay.
const MAX_DELAY_MS: u64 = 2_000;

/// Classify a reqwest transport failure into a stable retryable engine code.
pub fn classify_reqwest_error(error: &reqwest::Error) -> EngineError {
    if error.is_timeout() {
        EngineError::retryable("TIMEOUT", "the HTTP request timed out")
    } else if error.is_connect() {
        EngineError::retryable(
            "CONNECT_FAILED",
            "the HTTP connection could not be established",
        )
    } else {
        EngineError::retryable("NETWORK_INTERRUPTED", "the HTTP transfer was interrupted")
    }
}

/// Parse an integer-seconds `Retry-After` header.
///
/// An HTTP-date or any other malformed value returns `None`, which makes the
/// caller use the exponential fallback instead of sleeping until a date.
pub fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse::<u64>().ok().map(Duration::from_secs)
}

/// Delay before the attempt after `attempt` failures.
///
/// `attempt` is the number of attempts already made, starting at one. An
/// advertised integer `Retry-After` wins exactly; otherwise the delay grows
/// exponentially with full jitter.
pub fn delay_for_attempt(attempt: usize, retry_after: Option<Duration>) -> Duration {
    if let Some(delay) = retry_after {
        return delay;
    }
    Duration::from_millis(exponential_jitter_ms(attempt))
}

fn exponential_jitter_ms(attempt: usize) -> u64 {
    let shift = attempt.saturating_sub(1).min(6) as u32;
    let cap = BASE_DELAY_MS
        .saturating_mul(1u64 << shift)
        .min(MAX_DELAY_MS);
    full_jitter_ms(cap)
}

/// Full jitter spread retries uniformly over the whole exponential window so
/// concurrent failures do not synchronize on the same retry instant.
fn full_jitter_ms(cap: u64) -> u64 {
    if cap == 0 {
        return 0;
    }
    rand::random_range(0..=cap)
}
