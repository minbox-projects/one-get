//! HTTP route handlers and the fixed admission order.
//!
//! Every route checks the header budget and exact loopback Host first. Write
//! routes then require an exact browser-extension Origin, JSON content type where a
//! body is expected, and a bearer token from the Authorization header only.

use std::sync::MutexGuard;

use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::response::Response;
use serde::Deserialize;
use serde_json::{json, Value};

use super::auth;
use super::capture;
use super::error::{self, ApiError};
use super::pairing::hash_secret;
use super::pairing_store::{PairResult, PairedClient, PairingStore};
use super::protocol::{CaptureRequest, API_PROTOCOL_VERSION, MAX_BODY_BYTES};
use super::rate_limit::RateLimiter;
use super::server::ApiState;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairRequestBody {
    protocol_version: u32,
    code: String,
    #[serde(default)]
    label: String,
}

/// Unauthenticated liveness and capability probe.
pub(crate) async fn health(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    if let Err(rejection) = common_guard(&state, &headers) {
        return auth::error_response(rejection, None);
    }
    if let Err(rejection) = auth::check_health_origin(&headers) {
        return auth::error_response(rejection, None);
    }
    auth::json_ok(json!({
        "ok": true,
        "appVersion": state.app_version,
        "protocolVersion": API_PROTOCOL_VERSION,
        "capabilities": ["capture", "prefill", "takeover"],
    }))
}

/// Exchange a pairing code for a one-time bearer token.
pub(crate) async fn pair(State(state): State<ApiState>, request: Request) -> Response {
    let headers = request.headers().clone();
    if let Err(rejection) = common_guard(&state, &headers) {
        return auth::error_response(rejection, None);
    }
    let origin = match auth::check_write_origin(&headers) {
        Ok(origin) => origin,
        Err(rejection) => return auth::error_response(rejection, None),
    };
    if let Err(rejection) = auth::check_json_content_type(&headers) {
        return auth::error_response(rejection, None);
    }

    let now_ms = state.clock.now_ms();
    if let Some(response) = check_pair_limit(&state, &origin, now_ms) {
        return response;
    }

    let body = match read_body(request).await {
        Ok(body) => body,
        Err(rejection) => return auth::error_response(rejection, None),
    };
    let parsed: PairRequestBody = match serde_json::from_slice(&body) {
        Ok(parsed) => parsed,
        Err(_) => {
            return auth::error_response(
                error::bad_request("INVALID_BODY", "pairing request is not valid JSON"),
                None,
            )
        }
    };
    if parsed.protocol_version != API_PROTOCOL_VERSION {
        return auth::error_response(error::conflict("PROTOCOL_MISMATCH"), None);
    }

    let outcome = {
        let mut store = lock_store(&state);
        store.pair(&parsed.code, &origin, &parsed.label, now_ms)
    };

    match outcome {
        Ok(PairResult::Paired { token, client }) => {
            state.bridge.pairing_succeeded(&client);
            state
                .bridge
                .log_event("pairing_succeeded", json!({ "clients": 1 }));
            auth::json_ok(json!({
                "token": token,
                "client": client_json(&client),
            }))
        }
        Ok(PairResult::InvalidCode | PairResult::Expired) => {
            auth::error_response(error::unauthorized("INVALID_CODE"), None)
        }
        Ok(PairResult::Locked { retry_after_secs }) => auth::error_response(
            error::too_many_requests("PAIRING_LOCKED"),
            Some(retry_after_secs),
        ),
        Err(rejection) => auth::error_response(rejection, None),
    }
}

/// Validate and execute one takeover or prefill capture.
pub(crate) async fn capture(State(state): State<ApiState>, request: Request) -> Response {
    let headers = request.headers().clone();
    if let Err(rejection) = common_guard(&state, &headers) {
        return auth::error_response(rejection, None);
    }
    let origin = match auth::check_write_origin(&headers) {
        Ok(origin) => origin,
        Err(rejection) => return auth::error_response(rejection, None),
    };
    if let Err(rejection) = auth::check_json_content_type(&headers) {
        return auth::error_response(rejection, None);
    }
    let token = match auth::bearer_token(&headers) {
        Ok(token) => token,
        Err(rejection) => return auth::error_response(rejection, None),
    };

    let now_ms = state.clock.now_ms();
    match authorize(&state, &token, &origin, now_ms) {
        Ok(()) => {}
        Err(rejection) => return auth::error_response(rejection, None),
    }

    let key = hash_secret(&token);
    let decision = {
        let mut limiter = lock_token_limiter(&state);
        limiter.check(&key, now_ms)
    };
    if !decision.allowed {
        return auth::rate_limited(decision.retry_after_secs);
    }

    let body = match read_body(request).await {
        Ok(body) => body,
        Err(rejection) => return auth::error_response(rejection, None),
    };
    let parsed: CaptureRequest = match serde_json::from_slice(&body) {
        Ok(parsed) => parsed,
        Err(_) => {
            return auth::error_response(
                error::bad_request("INVALID_BODY", "capture request is not valid JSON"),
                None,
            )
        }
    };

    match capture::process(&state, parsed, &token, now_ms) {
        Ok(value) => auth::json_ok(value),
        Err(rejection) => auth::error_response(rejection, None),
    }
}

/// Revoke the bearer token used for this request.
pub(crate) async fn revoke(State(state): State<ApiState>, request: Request) -> Response {
    let headers = request.headers().clone();
    if let Err(rejection) = common_guard(&state, &headers) {
        return auth::error_response(rejection, None);
    }
    let origin = match auth::check_write_origin(&headers) {
        Ok(origin) => origin,
        Err(rejection) => return auth::error_response(rejection, None),
    };
    let token = match auth::bearer_token(&headers) {
        Ok(token) => token,
        Err(rejection) => return auth::error_response(rejection, None),
    };

    let now_ms = state.clock.now_ms();
    let outcome = {
        let mut store = lock_store(&state);
        match store.authorize(&token, &origin, now_ms) {
            Ok(Some(client)) => store.revoke(&client.id).map(Some),
            Ok(None) => Ok(None),
            Err(rejection) => Err(rejection),
        }
    };

    match outcome {
        Ok(Some(revoked)) => {
            state
                .bridge
                .log_event("pairing_revoked", json!({ "revoked": revoked }));
            auth::json_ok(json!({ "revoked": revoked }))
        }
        Ok(None) => auth::error_response(error::unauthorized("UNAUTHORIZED"), None),
        Err(rejection) => auth::error_response(rejection, None),
    }
}

fn common_guard(state: &ApiState, headers: &HeaderMap) -> Result<(), ApiError> {
    auth::check_header_limits(headers)?;
    auth::check_host(headers, &state.authority)?;
    Ok(())
}

fn check_pair_limit(state: &ApiState, origin: &str, now_ms: i64) -> Option<Response> {
    let mut limiter = lock_pair_limiter(state);
    let decision = limiter.check(origin, now_ms);
    if decision.allowed {
        None
    } else {
        Some(auth::rate_limited(decision.retry_after_secs))
    }
}

fn authorize(state: &ApiState, token: &str, origin: &str, now_ms: i64) -> Result<(), ApiError> {
    let mut store = lock_store(state);
    match store.authorize(token, origin, now_ms) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(error::unauthorized("UNAUTHORIZED")),
        Err(rejection) => Err(rejection),
    }
}

async fn read_body(request: Request) -> Result<Vec<u8>, ApiError> {
    match axum::body::to_bytes(request.into_body(), MAX_BODY_BYTES.saturating_add(1)).await {
        Ok(bytes) if bytes.len() <= MAX_BODY_BYTES => Ok(bytes.to_vec()),
        _ => Err(error::payload_too_large()),
    }
}

fn client_json(client: &PairedClient) -> Value {
    json!({
        "id": client.id,
        "origin": client.origin,
        "label": client.label,
        "createdAtMs": client.created_at_ms,
        "lastUsedAtMs": client.last_used_at_ms,
    })
}

fn lock_store(state: &ApiState) -> MutexGuard<'_, PairingStore> {
    state
        .store
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn lock_pair_limiter(state: &ApiState) -> MutexGuard<'_, RateLimiter> {
    state
        .pair_limiter
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn lock_token_limiter(state: &ApiState) -> MutexGuard<'_, RateLimiter> {
    state
        .token_limiter
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
