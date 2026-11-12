//! Login autostart service backed by the Tauri autostart plugin.

use tauri::AppHandle;
use tauri_plugin_autostart::ManagerExt;

use crate::commands::app::AutostartService;
use crate::error::AppError;

/// Autostart registration through the platform plugin.
pub struct PluginAutostartService {
    app: AppHandle,
}

impl PluginAutostartService {
    /// Create a service bound to an application handle.
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl AutostartService for PluginAutostartService {
    fn is_enabled(&self) -> Result<bool, AppError> {
        self.app.autolaunch().is_enabled().map_err(map_error)
    }

    fn set_enabled(&self, enabled: bool) -> Result<bool, AppError> {
        let manager = self.app.autolaunch();
        if enabled {
            manager.enable().map_err(map_error)?;
        } else {
            manager.disable().map_err(map_error)?;
        }
        manager.is_enabled().map_err(map_error)
    }
}

fn map_error(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        "autostart_failed",
        format!("update login autostart: {error}"),
    )
}
