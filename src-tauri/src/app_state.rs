//! Shared runtime state managed by Tauri.
//!
//! The state owns the resolved paths, the settings store, the logger, the
//! service implementations, and the most recently handled deep-link event. The
//! settings store and the last event are behind mutexes so command functions
//! can take `&AppState` and remain directly testable.

use std::sync::{Arc, Mutex, MutexGuard};

use crate::commands::app::{AutostartService, NotificationService, QuitService, SecretService};
use crate::config::{Settings, SettingsStore};
use crate::engine::service::EngineService;
use crate::error::AppError;
use crate::logging::Logger;
use crate::platform::deeplink::DeepLinkEvent;
use crate::platform::paths::AppPaths;

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

    /// The attached download engine service, when one has started.
    pub fn engine(&self) -> Option<&EngineService> {
        self.engine.as_deref()
    }

    /// Pause active downloads and stop the engine service.
    pub fn shutdown_engine(&self) {
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

    /// Persist settings and return the store's resulting snapshot.
    pub fn save_settings(&self, settings: Settings) -> Result<Settings, AppError> {
        let mut store = self.lock_settings();
        store.save(settings)?;
        Ok(store.settings())
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
}
