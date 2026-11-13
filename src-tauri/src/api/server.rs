//! Loopback HTTP server construction, shared state, and graceful shutdown.
//!
//! The listener is bound synchronously on `127.0.0.1` before the serving thread
//! starts, so a test or startup path learns a typed bind failure without a
//! panic. The serving thread owns a current-thread Tokio runtime and stops on a
//! oneshot signal, which is also sent by `Drop`.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use axum::routing::{get, post};
use axum::Router;
use tokio::sync::oneshot;

use super::capture::CaptureDedupe;
use super::clock::Clock;
use super::desktop::DesktopBridge;
use super::error::ApiError;
use super::pairing_store::PairingStore;
use super::protocol::{loopback_authority, API_HOST};
use super::rate_limit::{RateLimitPolicy, RateLimiter};
use super::routes;

/// Maximum number of limiter keys retained per limiter.
pub const RATE_LIMIT_ENTRIES: usize = 256;

/// Configuration for the loopback API.
pub struct ApiServerConfig {
    /// Loopback port; zero selects an ephemeral port.
    pub port: u16,
    /// Application build version reported by `/health`.
    pub app_version: String,
    /// Shared pairing database.
    pub pairing_store: Arc<Mutex<PairingStore>>,
    /// Desktop operations invoked by the capture routes.
    pub bridge: Arc<dyn DesktopBridge>,
    /// Wall-clock source for pairing and rate limits.
    pub clock: Arc<dyn Clock>,
    /// Per-origin pairing budget.
    pub pair_origin_policy: RateLimitPolicy,
    /// Per-token capture budget.
    pub token_policy: RateLimitPolicy,
}

/// Shared state for every API route.
#[derive(Clone)]
pub struct ApiState {
    pub(crate) app_version: String,
    pub(crate) authority: String,
    pub(crate) store: Arc<Mutex<PairingStore>>,
    pub(crate) bridge: Arc<dyn DesktopBridge>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) pair_limiter: Arc<Mutex<RateLimiter>>,
    pub(crate) token_limiter: Arc<Mutex<RateLimiter>>,
    pub(crate) dedupe: Arc<Mutex<CaptureDedupe>>,
}

impl ApiState {
    /// Build route state from its configuration.
    pub fn new(config: ApiServerConfig) -> Self {
        let port = config.port;
        Self::with_port(config, port)
    }

    fn with_port(config: ApiServerConfig, port: u16) -> Self {
        Self {
            app_version: config.app_version,
            authority: loopback_authority(port),
            store: config.pairing_store,
            bridge: config.bridge,
            clock: config.clock,
            pair_limiter: Arc::new(Mutex::new(RateLimiter::new(
                config.pair_origin_policy,
                RATE_LIMIT_ENTRIES,
            ))),
            token_limiter: Arc::new(Mutex::new(RateLimiter::new(
                config.token_policy,
                RATE_LIMIT_ENTRIES,
            ))),
            dedupe: Arc::new(Mutex::new(CaptureDedupe::new(
                super::capture::DEDUPE_CAPACITY,
            ))),
        }
    }
}

/// Build the route table over shared state.
pub fn build_router(state: ApiState) -> Router {
    Router::new()
        .route("/health", get(routes::health))
        .route("/pair", post(routes::pair))
        .route("/capture", post(routes::capture))
        .route("/pair/revoke", post(routes::revoke))
        .with_state(state)
}

/// A running loopback API server.
pub struct ApiServerHandle {
    port: u16,
    origin: String,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl ApiServerHandle {
    /// Bind synchronously and serve on a dedicated runtime thread.
    pub fn start(config: ApiServerConfig) -> Result<Self, ApiError> {
        let address = SocketAddr::from(([127, 0, 0, 1], config.port));
        let listener = std::net::TcpListener::bind(address).map_err(|_| bind_failed())?;
        listener.set_nonblocking(true).map_err(|_| bind_failed())?;
        let port = listener.local_addr().map_err(|_| bind_failed())?.port();
        let state = ApiState::with_port(config, port);
        let router = build_router(state);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let thread = thread::Builder::new()
            .name("oneget-api".to_string())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(_) => return,
                };
                runtime.block_on(async move {
                    let listener = match tokio::net::TcpListener::from_std(listener) {
                        Ok(listener) => listener,
                        Err(_) => return,
                    };
                    let _ = axum::serve(listener, router)
                        .with_graceful_shutdown(async move {
                            let _ = shutdown_rx.await;
                        })
                        .await;
                });
            })
            .map_err(|_| bind_failed())?;

        Ok(Self {
            port,
            origin: format!("http://{API_HOST}:{port}"),
            shutdown: Some(shutdown_tx),
            thread: Some(thread),
        })
    }

    /// Bound loopback port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Base origin of the bound service.
    pub fn origin(&self) -> String {
        self.origin.clone()
    }

    /// Stop accepting connections and join the serving thread.
    pub fn shutdown(&mut self) {
        if let Some(sender) = self.shutdown.take() {
            let _ = sender.send(());
        }
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for ApiServerHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn bind_failed() -> ApiError {
    ApiError::new(
        503,
        "API_BIND_FAILED",
        "the local API port could not be bound",
    )
}
