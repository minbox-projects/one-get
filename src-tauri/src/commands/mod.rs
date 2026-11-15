//! Tauri command surface and the synchronous service functions behind it.

pub mod app;
pub mod downloads;
pub mod kernel;
pub mod pairing;
pub mod video;

/// Tauri command wrappers registered with the runtime.
pub mod ipc {
    use std::sync::Arc;

    use tauri::State;

    use crate::api::desktop::PendingPrefill;
    use crate::app_state::AppState;
    use crate::config::Settings;
    use crate::error::AppError;
    use crate::platform::deeplink::DeepLinkEvent;

    use super::{app, downloads, kernel, pairing, video};

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
    pub fn settings_status(
        state: State<'_, Arc<AppState>>,
    ) -> Result<app::SettingsStatus, AppError> {
        app::settings_status(state.inner())
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
    pub async fn hf_token_status(
        state: State<'_, Arc<AppState>>,
    ) -> Result<app::SecretStatus, AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || app::hf_token_status(&state)).await
    }

    #[tauri::command]
    pub async fn hf_token_save(
        state: State<'_, Arc<AppState>>,
        token: String,
    ) -> Result<app::SecretStatus, AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || app::hf_token_save(&state, token)).await
    }

    #[tauri::command]
    pub async fn hf_token_clear(
        state: State<'_, Arc<AppState>>,
    ) -> Result<app::SecretStatus, AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || app::hf_token_clear(&state)).await
    }

    #[tauri::command]
    pub async fn proxy_credentials_status(
        state: State<'_, Arc<AppState>>,
    ) -> Result<app::SecretStatus, AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || app::proxy_credentials_status(&state)).await
    }

    #[tauri::command]
    pub async fn proxy_credentials_save(
        state: State<'_, Arc<AppState>>,
        username: String,
        password: String,
    ) -> Result<app::SecretStatus, AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || app::proxy_credentials_save(&state, username, password)).await
    }

    #[tauri::command]
    pub async fn proxy_credentials_clear(
        state: State<'_, Arc<AppState>>,
    ) -> Result<app::SecretStatus, AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || app::proxy_credentials_clear(&state)).await
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

    #[tauri::command]
    pub async fn downloads_list(
        state: State<'_, Arc<AppState>>,
    ) -> Result<Vec<downloads::DownloadSummary>, AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || downloads::downloads_list(&state)).await
    }

    #[tauri::command]
    pub async fn downloads_segments(
        state: State<'_, Arc<AppState>>,
        id: String,
    ) -> Result<Vec<downloads::SegmentView>, AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || downloads::downloads_segments(&state, &id)).await
    }

    #[tauri::command]
    pub async fn downloads_probe(
        state: State<'_, Arc<AppState>>,
        request: downloads::ProbeDownloadRequest,
    ) -> Result<downloads::ProbeResult, AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || downloads::downloads_probe(&state, request)).await
    }

    #[tauri::command]
    pub async fn downloads_add(
        state: State<'_, Arc<AppState>>,
        request: downloads::AddDownloadRequest,
    ) -> Result<String, AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || downloads::downloads_add(&state, request)).await
    }

    #[tauri::command]
    pub async fn downloads_pause(
        state: State<'_, Arc<AppState>>,
        id: String,
    ) -> Result<(), AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || downloads::downloads_pause(&state, &id)).await
    }

    #[tauri::command]
    pub async fn downloads_resume(
        state: State<'_, Arc<AppState>>,
        id: String,
    ) -> Result<(), AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || downloads::downloads_resume(&state, &id)).await
    }

    #[tauri::command]
    pub async fn downloads_cancel(
        state: State<'_, Arc<AppState>>,
        id: String,
    ) -> Result<(), AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || downloads::downloads_cancel(&state, &id)).await
    }

    #[tauri::command]
    pub async fn downloads_retry(
        state: State<'_, Arc<AppState>>,
        id: String,
    ) -> Result<(), AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || downloads::downloads_retry(&state, &id)).await
    }

    #[tauri::command]
    pub async fn downloads_delete_record(
        state: State<'_, Arc<AppState>>,
        id: String,
    ) -> Result<(), AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || downloads::downloads_delete_record(&state, &id)).await
    }

    #[tauri::command]
    pub async fn downloads_delete_file(
        state: State<'_, Arc<AppState>>,
        id: String,
    ) -> Result<(), AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || downloads::downloads_delete_file(&state, &id)).await
    }

    #[tauri::command]
    pub async fn downloads_open_file(
        state: State<'_, Arc<AppState>>,
        id: String,
    ) -> Result<(), AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || downloads::downloads_open_file(&state, &id)).await
    }

    #[tauri::command]
    pub async fn downloads_open_folder(
        state: State<'_, Arc<AppState>>,
        id: String,
    ) -> Result<(), AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || downloads::downloads_open_folder(&state, &id)).await
    }

    #[tauri::command]
    pub async fn kernel_status(
        state: State<'_, Arc<AppState>>,
    ) -> Result<Vec<kernel::ComponentStatusView>, AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || kernel::kernel_status(&state)).await
    }

    #[tauri::command]
    pub async fn kernel_manifest_refresh(state: State<'_, Arc<AppState>>) -> Result<(), AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || kernel::kernel_refresh(&state)).await
    }

    #[tauri::command]
    pub async fn kernel_install(
        state: State<'_, Arc<AppState>>,
        component: String,
    ) -> Result<(), AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || kernel::kernel_install(&state, &component)).await
    }

    #[tauri::command]
    pub async fn kernel_update(
        state: State<'_, Arc<AppState>>,
        component: String,
    ) -> Result<(), AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || kernel::kernel_update(&state, &component)).await
    }

    #[tauri::command]
    pub async fn kernel_rollback(
        state: State<'_, Arc<AppState>>,
        component: String,
    ) -> Result<(), AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || kernel::kernel_rollback(&state, &component)).await
    }

    #[tauri::command]
    pub async fn video_probe(
        state: State<'_, Arc<AppState>>,
        request: video::VideoProbeRequest,
    ) -> Result<video::VideoProbeResult, AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || video::video_probe(&state, request)).await
    }

    #[tauri::command]
    pub async fn video_start(
        state: State<'_, Arc<AppState>>,
        request: video::VideoStartRequest,
    ) -> Result<String, AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || video::video_start(&state, request)).await
    }

    #[tauri::command]
    pub async fn pairing_status(
        state: State<'_, Arc<AppState>>,
    ) -> Result<pairing::PairingStatus, AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || pairing::pairing_status(&state)).await
    }

    #[tauri::command]
    pub async fn pairing_generate_code(
        state: State<'_, Arc<AppState>>,
    ) -> Result<pairing::PairingCodeView, AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || pairing::pairing_generate_code(&state)).await
    }

    #[tauri::command]
    pub async fn pairing_list_clients(
        state: State<'_, Arc<AppState>>,
    ) -> Result<Vec<pairing::PairedClientView>, AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || pairing::pairing_list_clients(&state)).await
    }

    #[tauri::command]
    pub async fn pairing_revoke(
        state: State<'_, Arc<AppState>>,
        client_id: String,
    ) -> Result<bool, AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || pairing::pairing_revoke(&state, &client_id)).await
    }

    #[tauri::command]
    pub async fn pairing_take_pending_prefill(
        state: State<'_, Arc<AppState>>,
    ) -> Result<Option<PendingPrefill>, AppError> {
        let state = Arc::clone(state.inner());
        run_blocking(move || pairing::pairing_take_pending_prefill(&state)).await
    }
}
