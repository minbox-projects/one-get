//! Tauri command surface and the synchronous service functions behind it.

pub mod app;

/// Tauri command wrappers registered with the runtime.
pub mod ipc {
    use std::sync::Arc;

    use tauri::State;

    use crate::app_state::AppState;
    use crate::config::Settings;
    use crate::error::AppError;
    use crate::platform::deeplink::DeepLinkEvent;

    use super::app;

    async fn run_blocking<T, F>(task: F) -> Result<T, AppError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, AppError> + Send + 'static,
    {
        tokio::task::spawn_blocking(task).await.map_err(|error| {
            AppError::new(
                "command_join_error",
                format!("background task failed: {error}"),
            )
        })?
    }

    #[tauri::command]
    pub fn app_info(state: State<'_, Arc<AppState>>) -> Result<app::AppInfo, AppError> {
        Ok(app::app_info(state.inner()))
    }

    #[tauri::command]
    pub fn settings_get(state: State<'_, Arc<AppState>>) -> Result<Settings, AppError> {
        Ok(app::settings_get(state.inner()))
    }

    #[tauri::command]
    pub async fn settings_save(
        state: State<'_, Arc<AppState>>,
        settings: Settings,
    ) -> Result<Settings, AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || app::settings_save(&state, settings)).await
    }

    #[tauri::command]
    pub fn deep_link_last(state: State<'_, Arc<AppState>>) -> Result<DeepLinkEvent, AppError> {
        Ok(app::deep_link_last(state.inner()))
    }

    #[tauri::command]
    pub async fn diagnostics_secret_roundtrip(
        state: State<'_, Arc<AppState>>,
        secret: String,
    ) -> Result<app::SecretRoundTripResult, AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || app::diagnostics_secret_roundtrip(&state, &secret)).await
    }

    #[tauri::command]
    pub fn diagnostics_notification_status(
        state: State<'_, Arc<AppState>>,
    ) -> Result<app::NotificationStatus, AppError> {
        app::diagnostics_notification_status(state.inner())
    }

    #[tauri::command]
    pub fn diagnostics_send_notification(
        state: State<'_, Arc<AppState>>,
    ) -> Result<app::NotificationSendResult, AppError> {
        app::diagnostics_send_notification(state.inner())
    }

    #[tauri::command]
    pub fn diagnostics_autostart_get(
        state: State<'_, Arc<AppState>>,
    ) -> Result<app::AutostartState, AppError> {
        app::diagnostics_autostart_get(state.inner())
    }

    #[tauri::command]
    pub fn diagnostics_autostart_set(
        state: State<'_, Arc<AppState>>,
        enabled: bool,
    ) -> Result<app::AutostartState, AppError> {
        app::diagnostics_autostart_set(state.inner(), enabled)
    }

    #[tauri::command]
    pub async fn app_graceful_quit(
        app: tauri::AppHandle,
        state: State<'_, Arc<AppState>>,
    ) -> Result<app::QuitResult, AppError> {
        let state = Arc::clone(state.inner());
        let result = run_blocking(move || app::app_graceful_quit(&state)).await?;
        app.exit(0);
        Ok(result)
    }
}
