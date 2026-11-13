//! Injectable wall-clock readings for pairing and rate limiting.

use std::time::{SystemTime, UNIX_EPOCH};

/// A source of epoch-millisecond wall-clock time.
pub trait Clock: Send + Sync {
    /// Current time in milliseconds since the Unix epoch.
    fn now_ms(&self) -> i64;
}

/// Wall-clock time read from the operating system.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock(());

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
            .unwrap_or(0)
    }
}
