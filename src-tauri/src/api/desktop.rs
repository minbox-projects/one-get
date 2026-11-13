//! Bridge between the loopback API and the desktop application.
//!
//! The API never touches engine or Tauri types directly; it calls this trait so
//! the transport, capture protocol, and admission rules can be exercised without
//! a running application. The production implementation maps a validated
//! takeover item to a download task, stores one pending prefill, focuses the
//! main window, and writes only non-secret log fields.

use std::sync::{Arc, Weak};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};
use url::Url;

use crate::app_state::AppState;
use crate::engine::service::AddDownload;
use crate::logging::LogLevel;

use super::error::{self, ApiError};
use super::pairing_store::PairedClient;
use super::protocol::CaptureItem;

/// Event emitted when a prefill capture is ready for the desktop UI.
pub const CAPTURE_PREFILL_EVENT: &str = "app://capture-prefill";

/// The single pending prefill waiting to be delivered to a download dialog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingPrefill {
    /// Accepted captured items, in request order.
    pub items: Vec<CaptureItem>,
    /// Epoch-millisecond time the capture arrived.
    pub received_at_ms: i64,
}

/// Desktop operations the loopback API may request.
pub trait DesktopBridge: Send + Sync {
    /// Create exactly one download task for a validated takeover item.
    fn create_takeover_task(&self, item: &CaptureItem, now_ms: i64) -> Result<String, ApiError>;

    /// Store a pending prefill for the desktop UI.
    fn queue_prefill(&self, prefill: PendingPrefill) -> Result<(), ApiError>;

    /// Bring the main window to the front.
    fn focus_main_window(&self);

    /// Write a non-secret structured log record.
    fn log_event(&self, event: &str, fields: Value);

    /// Notify the desktop that a browser extension paired successfully.
    fn pairing_succeeded(&self, _client: &PairedClient) {}
}

/// Production bridge backed by the running Tauri application and engine.
pub struct TauriDesktopBridge {
    app: AppHandle,
    state: Weak<AppState>,
}

impl TauriDesktopBridge {
    /// Create a bridge for the current application and shared runtime state.
    pub fn new(app: AppHandle, state: Weak<AppState>) -> Self {
        Self { app, state }
    }

    fn state(&self) -> Result<Arc<AppState>, ApiError> {
        self.state.upgrade().ok_or_else(unavailable)
    }
}

impl DesktopBridge for TauriDesktopBridge {
    fn create_takeover_task(&self, item: &CaptureItem, _now_ms: i64) -> Result<String, ApiError> {
        let state = self.state()?;
        let engine = state.engine().ok_or_else(unavailable)?;
        let connections = state.settings().default_connections.clamp(1, 64);
        let extra_headers = item
            .headers
            .iter()
            .map(|header| (header.name.clone(), header.value.clone()))
            .collect();

        engine
            .add(AddDownload {
                url: item.url.clone(),
                filename: capture_filename(item),
                destination: None,
                connections,
                rate_limit: None,
                expected_sha256: None,
                allow_html: false,
                extra_headers,
            })
            .map_err(|_| task_create_failed())
    }

    fn queue_prefill(&self, prefill: PendingPrefill) -> Result<(), ApiError> {
        let state = self.state()?;
        state.queue_prefill(prefill.clone());
        if self.app.emit(CAPTURE_PREFILL_EVENT, &prefill).is_err() {
            if let Some(state) = self.state.upgrade() {
                let _ = state.logger().log(
                    LogLevel::Warn,
                    "capture_prefill_event_failed",
                    serde_json::json!({ "items": prefill.items.len() }),
                );
            }
        }
        Ok(())
    }

    fn focus_main_window(&self) {
        if let Some(window) = self.app.get_webview_window("main") {
            let _ = window.show();
            let _ = window.unminimize();
            let _ = window.set_focus();
        }
    }

    fn log_event(&self, event: &str, fields: Value) {
        if let Some(state) = self.state.upgrade() {
            let _ = state.logger().log(LogLevel::Info, event, fields);
        }
    }

    fn pairing_succeeded(&self, _client: &PairedClient) {
        if let Some(state) = self.state.upgrade() {
            state.clear_pending_pairing_code();
        }
    }
}

fn capture_filename(item: &CaptureItem) -> String {
    if let Some(filename) = item.filename.as_ref() {
        if !filename.is_empty() {
            return filename.clone();
        }
    }
    Url::parse(&item.url)
        .ok()
        .and_then(|url| {
            url.path_segments()
                .and_then(|mut segments| segments.next_back().map(str::to_string))
        })
        .filter(|segment| !segment.is_empty())
        .unwrap_or_else(|| "download".to_string())
}

fn unavailable() -> ApiError {
    error::service_unavailable("ENGINE_UNAVAILABLE")
}

fn task_create_failed() -> ApiError {
    ApiError::new(
        422,
        "TASK_CREATE_FAILED",
        "the download task could not be created",
    )
}
