//! Notification service backed by the Tauri notification plugin.

use tauri::AppHandle;
use tauri_plugin_notification::{NotificationExt, PermissionState};

use crate::commands::app::{NotificationSendResult, NotificationService, NotificationStatus};
use crate::error::AppError;

/// Notification permission and delivery through the platform plugin.
pub struct PluginNotificationService {
    app: AppHandle,
}

impl PluginNotificationService {
    /// Create a service bound to an application handle.
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }

    fn permission(&self) -> Result<PermissionState, AppError> {
        self.app.notification().permission_state().map_err(|error| {
            AppError::new(
                "notification_status_failed",
                format!("query notification permission: {error}"),
            )
        })
    }
}

impl NotificationService for PluginNotificationService {
    fn status(&self) -> Result<NotificationStatus, AppError> {
        Ok(NotificationStatus {
            permission: permission_label(self.permission()?).to_string(),
        })
    }

    fn send_test(&self) -> Result<NotificationSendResult, AppError> {
        if matches!(self.permission()?, PermissionState::Denied) {
            return Err(AppError::new(
                "notification_denied",
                "notification permission denied",
            ));
        }

        self.app
            .notification()
            .builder()
            .title("OneGet")
            .body("Test notification from OneGet")
            .show()
            .map_err(|error| {
                AppError::new(
                    "notification_send_failed",
                    format!("send notification: {error}"),
                )
            })?;

        Ok(NotificationSendResult { sent: true })
    }
}

fn permission_label(state: PermissionState) -> &'static str {
    match state {
        PermissionState::Granted => "granted",
        PermissionState::Denied => "denied",
        _ => "prompt",
    }
}
