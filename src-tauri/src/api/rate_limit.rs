//! Bounded per-key sliding-window request limiting.

use std::collections::{HashMap, VecDeque};

/// Capacity and window of a sliding-window budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitPolicy {
    /// Requests allowed inside one window.
    pub capacity: usize,
    /// Window length in milliseconds.
    pub window_ms: i64,
}

impl RateLimitPolicy {
    /// Create a policy with at least a one-millisecond window.
    pub fn new(capacity: usize, window_ms: i64) -> Self {
        Self {
            capacity,
            window_ms: window_ms.max(1),
        }
    }
}

/// The decision for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitDecision {
    /// Whether the request may proceed.
    pub allowed: bool,
    /// Whole seconds until the caller should retry when rejected.
    pub retry_after_secs: u64,
}

/// A bounded least-recently-used set of sliding-window buckets.
pub struct RateLimiter {
    policy: RateLimitPolicy,
    max_entries: usize,
    buckets: HashMap<String, VecDeque<i64>>,
    recency: VecDeque<String>,
}

impl RateLimiter {
    /// Create a limiter that tracks at most `max_entries` keys.
    pub fn new(policy: RateLimitPolicy, max_entries: usize) -> Self {
        Self {
            policy,
            max_entries,
            buckets: HashMap::new(),
            recency: VecDeque::new(),
        }
    }

    /// Check and, when allowed, consume one unit for `key`.
    pub fn check(&mut self, key: &str, now_ms: i64) -> RateLimitDecision {
        if self.max_entries == 0 {
            return RateLimitDecision {
                allowed: false,
                retry_after_secs: 1,
            };
        }

        if self.buckets.contains_key(key) {
            self.touch(key);
            let bucket = match self.buckets.get_mut(key) {
                Some(bucket) => bucket,
                None => {
                    return RateLimitDecision {
                        allowed: false,
                        retry_after_secs: 1,
                    }
                }
            };
            expire(bucket, now_ms, self.policy.window_ms);

            if bucket.len() < self.policy.capacity {
                bucket.push_back(now_ms);
                return RateLimitDecision {
                    allowed: true,
                    retry_after_secs: 0,
                };
            }

            let oldest = bucket.front().copied().unwrap_or(now_ms);
            return RateLimitDecision {
                allowed: false,
                retry_after_secs: remaining_secs(oldest, now_ms, self.policy.window_ms),
            };
        }

        self.evict();
        let mut bucket = VecDeque::new();
        if self.policy.capacity > 0 {
            bucket.push_back(now_ms);
        }
        self.buckets.insert(key.to_string(), bucket);
        self.recency.push_back(key.to_string());

        RateLimitDecision {
            allowed: self.policy.capacity > 0,
            retry_after_secs: if self.policy.capacity > 0 { 0 } else { 1 },
        }
    }

    /// Number of currently tracked keys.
    pub fn tracked_keys(&self) -> usize {
        self.buckets.len()
    }

    fn touch(&mut self, key: &str) {
        if let Some(position) = self.recency.iter().position(|existing| existing == key) {
            self.recency.remove(position);
        }
        self.recency.push_back(key.to_string());
    }

    fn evict(&mut self) {
        while self.buckets.len() >= self.max_entries {
            match self.recency.pop_front() {
                Some(oldest) => {
                    self.buckets.remove(&oldest);
                }
                None => break,
            }
        }
    }
}

fn expire(bucket: &mut VecDeque<i64>, now_ms: i64, window_ms: i64) {
    while let Some(&oldest) = bucket.front() {
        if oldest.saturating_add(window_ms) <= now_ms {
            bucket.pop_front();
        } else {
            break;
        }
    }
}

fn remaining_secs(oldest_ms: i64, now_ms: i64, window_ms: i64) -> u64 {
    let remaining = oldest_ms
        .saturating_add(window_ms)
        .saturating_sub(now_ms)
        .max(0) as u64;
    remaining.div_ceil(1_000).max(1)
}
