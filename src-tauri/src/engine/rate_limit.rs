//! Byte-rate pacing shared by a task's workers and, optionally, by every task.
//!
//! The limiter is a leaky bucket over byte counts: the first chunk starts
//! immediately and every later chunk waits until the configured number of
//! bytes per second has elapsed. A rate of zero means unlimited, so the caller
//! can remove a limit without restarting the engine.

use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::{sleep_until, Instant};

/// A shared bytes-per-second pacing gate.
#[derive(Debug)]
pub struct RateLimiter {
    bytes_per_second: u64,
    next_available: Mutex<Option<Instant>>,
}

impl RateLimiter {
    /// Create a limiter that admits `bytes_per_second` bytes each second.
    pub fn new(bytes_per_second: u64) -> Self {
        Self {
            bytes_per_second,
            next_available: Mutex::new(None),
        }
    }

    /// Configured steady throughput in bytes per second.
    pub fn bytes_per_second(&self) -> u64 {
        self.bytes_per_second
    }

    /// Wait until `bytes` may be transferred at the configured rate.
    ///
    /// Concurrent callers share the same schedule, so the aggregate throughput
    /// of several workers stays at the configured limit instead of multiplying
    /// it by the connection count.
    pub async fn acquire(&self, bytes: u64) {
        if self.bytes_per_second == 0 || bytes == 0 {
            return;
        }

        let now = Instant::now();
        let transfer = Duration::from_secs_f64(bytes as f64 / self.bytes_per_second as f64);
        let scheduled = {
            let mut next = self.next_available.lock().await;
            let start = match *next {
                Some(previous) if previous > now => previous,
                _ => now,
            };
            *next = Some(start + transfer);
            start
        };

        if scheduled > now {
            sleep_until(scheduled).await;
        }
    }
}
