//! Byte-rate pacing shared by a task's workers and, optionally, by every task.
//!
//! The limiter is a leaky bucket over byte counts: the first chunk starts
//! immediately and every later chunk waits until the configured number of
//! bytes per second has elapsed. A rate of zero means unlimited, so the caller
//! can remove a limit without restarting the engine or the tasks using it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tokio::time::{sleep_until, Instant};

/// A shared bytes-per-second pacing gate.
#[derive(Debug)]
pub struct RateLimiter {
    bytes_per_second: AtomicU64,
    next_available: Mutex<Option<Instant>>,
}

impl RateLimiter {
    /// Create a limiter that admits `bytes_per_second` bytes each second.
    pub fn new(bytes_per_second: u64) -> Self {
        Self {
            bytes_per_second: AtomicU64::new(bytes_per_second),
            next_available: Mutex::new(None),
        }
    }

    /// Configured steady throughput in bytes per second.
    pub fn bytes_per_second(&self) -> u64 {
        self.bytes_per_second.load(Ordering::SeqCst)
    }

    /// Change the steady throughput without replacing the limiter.
    ///
    /// A zero rate removes the limit. Callers already waiting on the previous
    /// schedule observe the new rate on their next chunk.
    pub fn set_rate(&self, bytes_per_second: u64) {
        let previous = self
            .bytes_per_second
            .swap(bytes_per_second, Ordering::SeqCst);
        if previous != bytes_per_second {
            *self
                .next_available
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        }
    }

    /// Wait until `bytes` may be transferred at the configured rate.
    ///
    /// Concurrent callers share the same schedule, so the aggregate throughput
    /// of several workers stays at the configured limit instead of multiplying
    /// it by the connection count.
    pub async fn acquire(&self, bytes: u64) {
        let rate = self.bytes_per_second.load(Ordering::SeqCst);
        if rate == 0 || bytes == 0 {
            return;
        }

        let now = Instant::now();
        let transfer = Duration::from_secs_f64(bytes as f64 / rate as f64);
        let scheduled = {
            let mut next = self
                .next_available
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
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
