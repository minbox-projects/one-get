//! Capture-intent processing after admission and protocol validation.
//!
//! A takeover creates exactly one desktop task and suppresses repeats for the
//! same token and browser source identifier. A prefill stores one bounded
//! pending slot and focuses the main window without creating a task.

use std::collections::{HashMap, VecDeque};
use std::sync::MutexGuard;

use serde_json::{json, Value};

use super::desktop::PendingPrefill;
use super::error::{self, ApiError};
use super::pairing::hash_secret;
use super::protocol::{self, CaptureIntent, CaptureRequest};
use super::server::ApiState;

/// Maximum retained token-and-source duplicate records.
pub const DEDUPE_CAPACITY: usize = 256;

/// Bounded first-in first-out duplicate suppression records.
pub struct CaptureDedupe {
    capacity: usize,
    entries: HashMap<String, String>,
    order: VecDeque<String>,
}

impl CaptureDedupe {
    /// Create a dedupe set that retains at most `capacity` records.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// Return the task already created for a token-and-source key.
    pub fn existing(&self, key: &str) -> Option<String> {
        self.entries.get(key).cloned()
    }

    /// Record the task created for a token-and-source key, evicting the oldest.
    pub fn remember(&mut self, key: String, task_id: String) {
        if let Some(existing) = self.entries.get_mut(&key) {
            *existing = task_id;
            return;
        }
        while self.entries.len() >= self.capacity {
            match self.order.pop_front() {
                Some(oldest) => {
                    self.entries.remove(&oldest);
                }
                None => break,
            }
        }
        self.entries.insert(key.clone(), task_id);
        self.order.push_back(key);
    }
}

/// Validate and execute one capture request.
pub(crate) fn process(
    state: &ApiState,
    request: CaptureRequest,
    token: &str,
    now_ms: i64,
) -> Result<Value, ApiError> {
    let validation = protocol::validate_capture_request(&request)?;
    if validation.accepted.is_empty() {
        return Err(error::bad_request(
            "INVALID_CAPTURE_ITEM",
            "capture contains no usable items",
        ));
    }

    match request.intent {
        CaptureIntent::Takeover => {
            let item = validation
                .accepted
                .into_iter()
                .next()
                .ok_or_else(invalid_capture_item)?;
            let key = request
                .source_id
                .filter(|source_id| !source_id.is_empty())
                .map(|source_id| dedupe_key(token, &source_id));

            let mut dedupe = lock_dedupe(state);
            if let Some(key) = key.as_deref() {
                if let Some(task_id) = dedupe.existing(key) {
                    return Ok(json!({
                        "intent": "takeover",
                        "accepted": 1,
                        "rejected": [],
                        "taskId": task_id,
                        "duplicate": true,
                    }));
                }
            }

            let task_id = state.bridge.create_takeover_task(&item, now_ms)?;
            if let Some(key) = key {
                dedupe.remember(key, task_id.clone());
            }
            drop(dedupe);

            state.bridge.focus_main_window();
            state.bridge.log_event(
                "capture_accepted",
                json!({ "intent": "takeover", "accepted": 1, "rejected": 0 }),
            );

            Ok(json!({
                "intent": "takeover",
                "accepted": 1,
                "rejected": [],
                "taskId": task_id,
                "duplicate": false,
            }))
        }
        CaptureIntent::Prefill => {
            let rejected: Vec<Value> = validation
                .rejected
                .iter()
                .map(|rejection| json!({ "index": rejection.index, "code": rejection.code }))
                .collect();
            let accepted = validation.accepted.len();
            let prefill = PendingPrefill {
                items: validation.accepted,
                received_at_ms: now_ms,
            };

            state.bridge.queue_prefill(prefill)?;
            state.bridge.focus_main_window();
            state.bridge.log_event(
                "capture_accepted",
                json!({
                    "intent": "prefill",
                    "accepted": accepted,
                    "rejected": validation.rejected.len(),
                }),
            );

            Ok(json!({
                "intent": "prefill",
                "accepted": accepted,
                "rejected": rejected,
                "duplicate": false,
            }))
        }
    }
}

fn dedupe_key(token: &str, source_id: &str) -> String {
    format!("{}:{source_id}", hash_secret(token))
}

fn lock_dedupe(state: &ApiState) -> MutexGuard<'_, CaptureDedupe> {
    state
        .dedupe
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn invalid_capture_item() -> ApiError {
    error::bad_request("INVALID_CAPTURE_ITEM", "capture contains no usable items")
}
