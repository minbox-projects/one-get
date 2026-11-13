//! Desktop pairing service functions behind the Tauri IPC wrappers.
//!
//! These synchronous functions take [`AppState`] so the pairing surface can be
//! exercised without a Tauri runtime. They never return a bearer token: status
//! and client views expose only identifiers, origins, labels, and timestamps.

use serde::Serialize;

use crate::api::desktop::PendingPrefill;
use crate::api::error::ApiError;
use crate::api::pairing_store::{PairedClient, PairingCode};
use crate::api::protocol::API_PROTOCOL_VERSION;
use crate::app_state::AppState;
use crate::error::AppError;

/// A generated one-time pairing code and its expiry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingCodeView {
    /// Eight-digit code shown to the user.
    pub code: String,
    /// Epoch-millisecond expiry.
    pub expires_at_ms: i64,
}

/// A paired browser-extension client without its token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedClientView {
    /// Stable client identifier.
    pub id: String,
    /// User-visible label.
    pub label: String,
    /// Exact bound extension origin.
    pub origin: String,
    /// Epoch-millisecond creation time.
    pub created_at_ms: i64,
    /// Epoch-millisecond last authorization time.
    pub last_used_at_ms: i64,
}

/// Browser-integration availability, pending code, and paired clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingStatus {
    /// Whether the loopback service and pairing store are usable.
    pub available: bool,
    /// Capture protocol version supported by this build.
    pub protocol_version: u32,
    /// Stable user-safe reason the service is unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Pending pairing code, when one is active.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<PairingCodeView>,
    /// Paired clients.
    pub clients: Vec<PairedClientView>,
}

impl From<PairingCode> for PairingCodeView {
    fn from(code: PairingCode) -> Self {
        Self {
            code: code.code,
            expires_at_ms: code.expires_at_ms,
        }
    }
}

impl From<PairedClient> for PairedClientView {
    fn from(client: PairedClient) -> Self {
        Self {
            id: client.id,
            label: client.label,
            origin: client.origin,
            created_at_ms: client.created_at_ms,
            last_used_at_ms: client.last_used_at_ms,
        }
    }
}

/// Report availability, the pending code, and paired clients.
pub fn pairing_status(state: &AppState) -> Result<PairingStatus, AppError> {
    let mut runtime = state.pairing_runtime();
    if !runtime.is_available() {
        return Ok(PairingStatus {
            available: false,
            protocol_version: API_PROTOCOL_VERSION,
            reason: runtime.reason().map(str::to_string),
            code: None,
            clients: Vec::new(),
        });
    }

    let code = runtime.current_code().map(PairingCodeView::from);
    let clients = runtime
        .list_clients()
        .map_err(map_api)?
        .into_iter()
        .map(PairedClientView::from)
        .collect();

    Ok(PairingStatus {
        available: true,
        protocol_version: API_PROTOCOL_VERSION,
        reason: None,
        code,
        clients,
    })
}

/// Generate a new one-time pairing code, replacing any earlier code.
pub fn pairing_generate_code(state: &AppState) -> Result<PairingCodeView, AppError> {
    let mut runtime = state.pairing_runtime();
    runtime
        .generate_code()
        .map(PairingCodeView::from)
        .map_err(map_api)
}

/// List paired clients.
pub fn pairing_list_clients(state: &AppState) -> Result<Vec<PairedClientView>, AppError> {
    let mut runtime = state.pairing_runtime();
    let clients = runtime.list_clients().map_err(map_api)?;
    Ok(clients.into_iter().map(PairedClientView::from).collect())
}

/// Revoke a paired client by identifier.
pub fn pairing_revoke(state: &AppState, client_id: &str) -> Result<bool, AppError> {
    let mut runtime = state.pairing_runtime();
    runtime.revoke(client_id).map_err(map_api)
}

/// Take the single pending prefill exactly once.
pub fn pairing_take_pending_prefill(state: &AppState) -> Result<Option<PendingPrefill>, AppError> {
    Ok(state.take_pending_prefill())
}

fn map_api(error: ApiError) -> AppError {
    AppError::new(error.code, error.message)
}
