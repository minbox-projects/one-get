//! Shared runtime state managed by Tauri.
//!
//! The state owns the resolved paths, the settings store, the logger, the
//! service implementations, and the most recently handled deep-link event. The
//! settings store and the last event are behind mutexes so command functions
//! can take `&AppState` and remain directly testable.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::api::clock::{Clock, SystemClock};
use crate::api::desktop::PendingPrefill;
use crate::api::error::ApiError;
use crate::api::pairing_store::{PairedClient, PairingCode, PairingStore};
use crate::api::server::ApiServerHandle;
use crate::commands::app::{AutostartService, NotificationService, QuitService, SecretService};
use crate::commands::kernel::KernelRuntime;
use crate::config::{ProxyMode, Settings, SettingsStore};
use crate::engine::http::ProxyConfig;
use crate::engine::scheduler::NetworkOptions;
use crate::engine::service::EngineService;
use crate::error::AppError;
use crate::logging::Logger;
use crate::platform::deeplink::DeepLinkEvent;
use crate::platform::paths::AppPaths;
use crate::platform::proxy::{
    resolve_proxy, PlatformSystemProxySource, ProxyResolution, SystemProxySource,
};
use crate::video::service::VideoService;

/// Browser-integration availability, pairing state, and one pending prefill.
pub struct PairingRuntime {
    clock: Arc<dyn Clock>,
    store: Option<Arc<Mutex<PairingStore>>>,
    unavailable_reason: Option<String>,
    pending_code: Option<PairingCode>,
    pending_prefill: Option<PendingPrefill>,
}

impl PairingRuntime {
    /// Create an available runtime over a shared pairing store.
    pub fn available(store: Arc<Mutex<PairingStore>>, clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            store: Some(store),
            unavailable_reason: None,
            pending_code: None,
            pending_prefill: None,
        }
    }

    /// Create an unavailable runtime with a stable user-safe reason.
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            clock: Arc::new(SystemClock::default()),
            store: None,
            unavailable_reason: Some(reason.into()),
            pending_code: None,
            pending_prefill: None,
        }
    }

    /// Current time from the injected clock.
    pub fn now_ms(&self) -> i64 {
        self.clock.now_ms()
    }

    pub(crate) fn is_available(&self) -> bool {
        self.store.is_some()
    }

    pub(crate) fn reason(&self) -> Option<&str> {
        self.unavailable_reason.as_deref()
    }

    pub(crate) fn generate_code(&mut self) -> Result<PairingCode, ApiError> {
        let store = self.store.as_ref().ok_or_else(pairing_unavailable)?;
        let now_ms = self.now_ms();
        let code = lock_store(store).generate_code(now_ms)?;
        self.pending_code = Some(code.clone());
        Ok(code)
    }

    pub(crate) fn current_code(&mut self) -> Option<PairingCode> {
        let now_ms = self.now_ms();
        if self
            .pending_code
            .as_ref()
            .is_some_and(|code| now_ms >= code.expires_at_ms)
        {
            self.pending_code = None;
        }
        self.pending_code.clone()
    }

    pub(crate) fn clear_pending_code(&mut self) {
        self.pending_code = None;
    }

    pub(crate) fn list_clients(&mut self) -> Result<Vec<PairedClient>, ApiError> {
        let store = self.store.as_ref().ok_or_else(pairing_unavailable)?;
        lock_store(store).list_clients()
    }

    pub(crate) fn revoke(&mut self, id: &str) -> Result<bool, ApiError> {
        let store = self.store.as_ref().ok_or_else(pairing_unavailable)?;
        lock_store(store).revoke(id)
    }

    pub(crate) fn set_unavailable(&mut self, reason: impl Into<String>) {
        self.store = None;
        self.unavailable_reason = Some(reason.into());
        self.pending_code = None;
    }

    pub(crate) fn queue_prefill(&mut self, prefill: PendingPrefill) {
        self.pending_prefill = Some(prefill);
    }

    pub(crate) fn take_pending_prefill(&mut self) -> Option<PendingPrefill> {
        self.pending_prefill.take()
    }
}

/// Runtime state shared by Tauri commands, the tray, and deep-link handlers.
pub struct AppState {
    paths: AppPaths,
    settings: Mutex<SettingsStore>,
    logger: Logger,
    secrets: Arc<dyn SecretService>,
    notifications: Arc<dyn NotificationService>,
    autostart: Arc<dyn AutostartService>,
    quit: Arc<dyn QuitService>,
    engine: Option<Arc<EngineService>>,
    kernel: Option<Arc<KernelRuntime>>,
    video: Option<Arc<VideoService>>,
    system_proxy: Arc<dyn SystemProxySource>,
    proxy_unavailable_reason: Mutex<Option<String>>,
    pairing: Mutex<PairingRuntime>,
    api_server: Mutex<Option<ApiServerHandle>>,
    last_event: Mutex<DeepLinkEvent>,
}

impl AppState {
    /// Create the runtime state from its resolved dependencies.
    pub fn new(
        paths: AppPaths,
        settings: SettingsStore,
        logger: Logger,
        secrets: Arc<dyn SecretService>,
        notifications: Arc<dyn NotificationService>,
        autostart: Arc<dyn AutostartService>,
        quit: Arc<dyn QuitService>,
    ) -> Self {
        Self {
            paths,
            settings: Mutex::new(settings),
            logger,
            secrets,
            notifications,
            autostart,
            quit,
            engine: None,
            kernel: None,
            video: None,
            system_proxy: Arc::new(PlatformSystemProxySource),
            proxy_unavailable_reason: Mutex::new(None),
            pairing: Mutex::new(PairingRuntime::unavailable("PAIRING_UNAVAILABLE")),
            api_server: Mutex::new(None),
            last_event: Mutex::new(DeepLinkEvent::rejected(
                "no_deep_link".to_string(),
                "no deep link has been received".to_string(),
            )),
        }
    }

    /// Attach the long-lived download engine service.
    pub fn with_engine(mut self, engine: Arc<EngineService>) -> Self {
        self.engine = Some(engine);
        self
    }

    /// Attach the runtime component kernel.
    pub fn with_kernel(mut self, kernel: Arc<KernelRuntime>) -> Self {
        self.kernel = Some(kernel);
        self
    }

    /// Attach the long-lived video download service.
    pub fn with_video(mut self, video: Arc<VideoService>) -> Self {
        self.video = Some(video);
        self
    }

    /// Replace the system-proxy source; tests use this to inject a fixture.
    pub fn with_system_proxy_source(mut self, source: Arc<dyn SystemProxySource>) -> Self {
        self.system_proxy = source;
        self
    }

    /// Attach the browser-integration pairing runtime.
    pub fn with_pairing(self, runtime: PairingRuntime) -> Self {
        *self.lock_pairing() = runtime;
        self
    }

    /// Replace the pairing runtime with an unavailable reason.
    pub fn set_pairing_unavailable(&self, reason: &str) {
        self.lock_pairing().set_unavailable(reason);
    }

    /// Current time from the pairing clock.
    pub fn now_ms(&self) -> i64 {
        self.lock_pairing().now_ms()
    }

    /// Store the single pending prefill, replacing any earlier one.
    pub fn queue_prefill(&self, prefill: PendingPrefill) {
        self.lock_pairing().queue_prefill(prefill);
    }

    /// Take the pending prefill exactly once.
    pub fn take_pending_prefill(&self) -> Option<PendingPrefill> {
        self.lock_pairing().take_pending_prefill()
    }

    pub(crate) fn clear_pending_pairing_code(&self) {
        self.lock_pairing().clear_pending_code();
    }

    pub(crate) fn pairing_runtime(&self) -> MutexGuard<'_, PairingRuntime> {
        self.lock_pairing()
    }

    pub(crate) fn attach_api_server(&self, handle: ApiServerHandle) {
        *self
            .api_server
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(handle);
    }

    /// The attached download engine service, when one has started.
    pub fn engine(&self) -> Option<&EngineService> {
        self.engine.as_deref()
    }

    /// The runtime component kernel, when initialization succeeded.
    pub fn kernel(&self) -> Option<&KernelRuntime> {
        self.kernel.as_deref()
    }

    /// The video download service, when initialization succeeded.
    pub fn video(&self) -> Option<&VideoService> {
        self.video.as_deref()
    }

    /// Stop the loopback API, the video service, then the HTTP engine.
    pub fn shutdown_engine(&self) {
        let handle = self
            .api_server
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(mut handle) = handle {
            handle.shutdown();
        }
        if let Some(video) = &self.video {
            let _ = video.shutdown();
        }
        if let Some(engine) = &self.engine {
            engine.shutdown();
        }
    }

    /// Resolved application directories.
    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    /// Structured logger.
    pub fn logger(&self) -> &Logger {
        &self.logger
    }

    /// Diagnostic secret service.
    pub fn secrets(&self) -> &dyn SecretService {
        self.secrets.as_ref()
    }

    /// Notification service.
    pub fn notifications(&self) -> &dyn NotificationService {
        self.notifications.as_ref()
    }

    /// Autostart service.
    pub fn autostart(&self) -> &dyn AutostartService {
        self.autostart.as_ref()
    }

    /// Quit service.
    pub fn quit(&self) -> &dyn QuitService {
        self.quit.as_ref()
    }

    /// Current settings snapshot.
    pub fn settings(&self) -> Settings {
        self.lock_settings().settings()
    }

    /// Whether the loaded settings document is read-only.
    pub fn settings_read_only(&self) -> bool {
        self.lock_settings().is_read_only()
    }

    /// Whether corrupt settings input was recovered from a backup.
    pub fn settings_recovered(&self) -> bool {
        self.lock_settings().is_recovered()
    }

    /// Persist settings, then apply the effective proxy and credential state.
    pub fn save_settings(&self, settings: Settings) -> Result<Settings, AppError> {
        let saved = {
            let mut store = self.lock_settings();
            store.save(settings)?;
            store.settings()
        };
        self.apply_proxy_settings(&saved)?;
        self.apply_network_settings(&saved)?;
        Ok(saved)
    }

    /// Apply the saved transport settings to the running engine.
    pub(crate) fn apply_network_settings(&self, settings: &Settings) -> Result<(), AppError> {
        if let Some(engine) = &self.engine {
            engine
                .apply_network_settings(network_options(settings))
                .map_err(AppError::from)?;
        }
        Ok(())
    }

    /// The last stable reason a system proxy could not be used.
    pub fn proxy_unavailable_reason(&self) -> Option<String> {
        self.proxy_unavailable_reason
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Resolve the saved proxy configuration and apply it to the live engine.
    ///
    /// Direct and system modes delete stored proxy credentials because those
    /// modes never attach authentication. An unavailable system proxy connects
    /// directly and records only a stable reason code.
    fn apply_proxy_settings(&self, settings: &Settings) -> Result<(), AppError> {
        let credentials = self.proxy_credentials()?;
        let resolution = resolve_proxy(
            settings,
            credentials
                .as_ref()
                .map(|(username, password)| (username.as_str(), password.as_str())),
            self.system_proxy.as_ref(),
        );

        let effective = match &resolution {
            ProxyResolution::Proxy(resolved) => {
                self.set_proxy_unavailable_reason(None);
                Some(ProxyConfig {
                    url: resolved.url.clone(),
                    username: resolved.username.clone(),
                    password: resolved.password.clone(),
                })
            }
            ProxyResolution::Direct => {
                self.set_proxy_unavailable_reason(None);
                None
            }
            ProxyResolution::Unavailable { reason } => {
                self.set_proxy_unavailable_reason(Some(reason.clone()));
                None
            }
        };

        if let Some(engine) = &self.engine {
            engine.set_proxy(effective.clone())?;
        }
        if let Some(kernel) = &self.kernel {
            kernel.set_proxy(effective.clone());
        }
        if let Some(video) = &self.video {
            video
                .set_proxy(effective)
                .map_err(|error| AppError::new(error.code(), error.message()))?;
        }

        if matches!(settings.proxy_mode, ProxyMode::Direct | ProxyMode::System) {
            let secrets = self.secrets();
            // A credentials store that is unavailable must not make an
            // otherwise valid settings save fail; there is simply nothing to
            // attach in these modes.
            let _ = secrets.delete_secret(crate::commands::app::PROXY_USERNAME_KEY);
            let _ = secrets.delete_secret(crate::commands::app::PROXY_PASSWORD_KEY);
        }
        Ok(())
    }

    fn proxy_credentials(&self) -> Result<Option<(String, String)>, AppError> {
        let secrets = self.secrets();
        let Some(username) = secrets
            .get_secret(crate::commands::app::PROXY_USERNAME_KEY)
            .ok()
            .flatten()
        else {
            return Ok(None);
        };
        let Some(password) = secrets
            .get_secret(crate::commands::app::PROXY_PASSWORD_KEY)
            .ok()
            .flatten()
        else {
            return Ok(None);
        };
        Ok(Some((
            username.expose_secret().to_string(),
            password.expose_secret().to_string(),
        )))
    }

    fn set_proxy_unavailable_reason(&self, reason: Option<String>) {
        *self
            .proxy_unavailable_reason
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = reason;
    }

    /// The most recently handled deep-link event.
    pub fn last_deep_link(&self) -> DeepLinkEvent {
        self.lock_last_event().clone()
    }

    /// Replace the most recently handled deep-link event.
    pub fn set_last_deep_link(&self, event: DeepLinkEvent) {
        *self.lock_last_event() = event;
    }

    fn lock_settings(&self) -> MutexGuard<'_, SettingsStore> {
        self.settings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_last_event(&self) -> MutexGuard<'_, DeepLinkEvent> {
        self.last_event
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_pairing(&self) -> MutexGuard<'_, PairingRuntime> {
        self.pairing
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Translate saved settings into the transport options the engine applies.
pub(crate) fn network_options(settings: &Settings) -> NetworkOptions {
    NetworkOptions {
        user_agent: settings.user_agent.clone(),
        connect_timeout: Duration::from_secs(u64::from(settings.connect_timeout_secs)),
        read_timeout: Duration::from_secs(u64::from(settings.read_timeout_secs)),
        retry_attempts: settings.retry_attempts as usize,
    }
}

fn lock_store(store: &Arc<Mutex<PairingStore>>) -> MutexGuard<'_, PairingStore> {
    store
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn pairing_unavailable() -> ApiError {
    ApiError::new(
        503,
        "PAIRING_UNAVAILABLE",
        "browser integration is unavailable",
    )
}
