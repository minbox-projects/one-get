//! Application, settings, diagnostics, and quit service functions.
//!
//! These synchronous functions receive [`AppState`] directly so they can be
//! exercised without a Tauri runtime. Thin Tauri command wrappers live in the
//! parent module and move blocking work off the UI thread.

use serde::Serialize;

use crate::app_state::AppState;
use crate::config::Settings;
use crate::error::AppError;
use crate::logging::LogLevel;
use crate::platform::deeplink::{parse_deep_link, DeepLinkEvent};
use crate::platform::secrets::{KeyringStore, SecretError, SecretStore, SecretValue};

/// A diagnostic secret round-trip result. Never carries the secret value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretRoundTripResult {
    /// Whether the value was stored and read back successfully.
    pub success: bool,
}

/// Application identity and log location.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    /// Product name.
    pub name: String,
    /// Build version.
    pub version: String,
    /// Absolute path of the current log file.
    pub log_path: String,
}

/// Current notification permission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationStatus {
    /// `granted`, `denied`, or `prompt`.
    pub permission: String,
}

/// Result of sending a diagnostic notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationSendResult {
    /// Whether the notification was handed to the platform.
    pub sent: bool,
}

/// Current autostart registration state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutostartState {
    /// Whether the app is registered to start at login.
    pub enabled: bool,
}

/// Result of a graceful quit request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuitResult {
    /// Whether shutdown was requested.
    pub quitting: bool,
}

/// Whether a write-only secret is configured. Never carries the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretStatus {
    /// Whether the secret currently exists in the credential store.
    pub configured: bool,
}

/// Round-trip a diagnostic secret without exposing its value.
pub trait SecretService: Send + Sync {
    /// Store, read back, and delete a diagnostic secret.
    fn roundtrip(&self, secret: &str) -> Result<SecretRoundTripResult, AppError>;

    /// Read a stored secret for an internal consumer.
    ///
    /// The default keeps minimal test doubles source-compatible; a real store
    /// overrides it.
    fn get_secret(&self, _key: &str) -> Result<Option<SecretValue>, AppError> {
        Err(secret_unavailable())
    }

    /// Store a secret under a stable key.
    fn set_secret(&self, _key: &str, _value: &SecretValue) -> Result<(), AppError> {
        Err(secret_unavailable())
    }

    /// Delete a secret. Deleting a missing key succeeds.
    fn delete_secret(&self, _key: &str) -> Result<(), AppError> {
        Err(secret_unavailable())
    }
}

/// Query and send diagnostic notifications.
pub trait NotificationService: Send + Sync {
    /// Current permission state.
    fn status(&self) -> Result<NotificationStatus, AppError>;
    /// Send a diagnostic notification.
    fn send_test(&self) -> Result<NotificationSendResult, AppError>;
}

/// Query and change the login autostart registration.
pub trait AutostartService: Send + Sync {
    /// Whether autostart is currently enabled.
    fn is_enabled(&self) -> Result<bool, AppError>;
    /// Enable or disable autostart, returning the resulting state.
    fn set_enabled(&self, enabled: bool) -> Result<bool, AppError>;
}

/// Request application shutdown.
pub trait QuitService: Send + Sync {
    /// Ask the application to quit.
    fn request_quit(&self) -> Result<(), AppError>;
}

/// Keyring-backed diagnostic secret service.
pub struct KeyringSecretService {
    store: KeyringStore,
}

impl KeyringSecretService {
    /// Create a service over the platform credential store.
    pub fn new() -> Self {
        Self {
            store: KeyringStore::new(),
        }
    }
}

impl Default for KeyringSecretService {
    fn default() -> Self {
        Self::new()
    }
}

const DIAGNOSTIC_SECRET_KEY: &str = "diagnostics-roundtrip";
const HF_TOKEN_KEY: &str = "huggingface-token";
pub(crate) const PROXY_USERNAME_KEY: &str = "proxy-username";
pub(crate) const PROXY_PASSWORD_KEY: &str = "proxy-password";

impl SecretService for KeyringSecretService {
    fn roundtrip(&self, secret: &str) -> Result<SecretRoundTripResult, AppError> {
        let value = SecretValue::new(secret.to_string()).map_err(map_secret_error)?;
        self.store
            .set(DIAGNOSTIC_SECRET_KEY, &value)
            .map_err(map_secret_error)?;

        let outcome = match self.store.get(DIAGNOSTIC_SECRET_KEY) {
            Ok(Some(stored)) if stored.expose_secret() == value.expose_secret() => {
                Ok(SecretRoundTripResult { success: true })
            }
            Ok(_) => Err(AppError::new(
                "secret_roundtrip_mismatch",
                "diagnostic secret did not round-trip",
            )),
            Err(error) => Err(map_secret_error(error)),
        };

        let _ = self.store.delete(DIAGNOSTIC_SECRET_KEY);
        outcome
    }

    fn get_secret(&self, key: &str) -> Result<Option<SecretValue>, AppError> {
        self.store.get(key).map_err(map_secret_error)
    }

    fn set_secret(&self, key: &str, value: &SecretValue) -> Result<(), AppError> {
        self.store.set(key, value).map_err(map_secret_error)
    }

    fn delete_secret(&self, key: &str) -> Result<(), AppError> {
        self.store.delete(key).map_err(map_secret_error)
    }
}

/// A no-op quit service; the command wrapper or tray performs the real exit.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlatformQuitService;

impl QuitService for PlatformQuitService {
    fn request_quit(&self) -> Result<(), AppError> {
        Ok(())
    }
}

/// Application identity and log file location.
pub fn app_info(state: &AppState) -> AppInfo {
    AppInfo {
        name: "OneGet".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        log_path: state.logger().current_path().display().to_string(),
    }
}

/// Load-time status of the settings document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsStatus {
    /// Whether a future schema makes the settings read-only.
    pub read_only: bool,
    /// Whether corrupt input was backed up and defaults were loaded.
    pub recovered: bool,
    /// Stable reason the configured system proxy cannot be used, when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_unavailable_reason: Option<String>,
}

/// Current settings.
pub fn settings_get(state: &AppState) -> Settings {
    state.settings()
}

/// Load-time status of the settings document.
pub fn settings_status(state: &AppState) -> Result<SettingsStatus, AppError> {
    Ok(SettingsStatus {
        read_only: state.settings_read_only(),
        recovered: state.settings_recovered(),
        proxy_unavailable_reason: state.proxy_unavailable_reason(),
    })
}

/// Persist settings, apply live engine limits, and return the stored value.
pub fn settings_save(state: &AppState, settings: Settings) -> Result<Settings, AppError> {
    settings.validate()?;
    let saved = state.save_settings(settings)?;
    if let Some(engine) = state.engine() {
        let max_concurrent = usize::try_from(saved.max_concurrent_tasks)
            .unwrap_or(1)
            .max(1);
        engine
            .apply_settings(max_concurrent, saved.global_rate_limit)
            .map_err(AppError::from)?;
    }
    Ok(saved)
}

/// Whether a HuggingFace token is configured.
pub fn hf_token_status(state: &AppState) -> Result<SecretStatus, AppError> {
    Ok(SecretStatus {
        configured: state.secrets().get_secret(HF_TOKEN_KEY)?.is_some(),
    })
}

/// Store a HuggingFace token and report only its configured state.
pub fn hf_token_save(state: &AppState, token: String) -> Result<SecretStatus, AppError> {
    let value = SecretValue::new(token).map_err(map_secret_error)?;
    state.secrets().set_secret(HF_TOKEN_KEY, &value)?;
    state.apply_huggingface_settings(&state.settings())?;
    Ok(SecretStatus { configured: true })
}

/// Remove the HuggingFace token.
pub fn hf_token_clear(state: &AppState) -> Result<SecretStatus, AppError> {
    state.secrets().delete_secret(HF_TOKEN_KEY)?;
    state.apply_huggingface_settings(&state.settings())?;
    Ok(SecretStatus { configured: false })
}

/// Whether both proxy credential values are configured.
pub fn proxy_credentials_status(state: &AppState) -> Result<SecretStatus, AppError> {
    let secrets = state.secrets();
    let configured = secrets.get_secret(PROXY_USERNAME_KEY)?.is_some()
        && secrets.get_secret(PROXY_PASSWORD_KEY)?.is_some();
    Ok(SecretStatus { configured })
}

/// Store the proxy username and password and report only their configured state.
pub fn proxy_credentials_save(
    state: &AppState,
    username: String,
    password: String,
) -> Result<SecretStatus, AppError> {
    let username = SecretValue::new(username).map_err(map_secret_error)?;
    let password = SecretValue::new(password).map_err(map_secret_error)?;
    let secrets = state.secrets();
    secrets.set_secret(PROXY_USERNAME_KEY, &username)?;
    secrets.set_secret(PROXY_PASSWORD_KEY, &password)?;
    state.reapply_proxy_settings(&state.settings())?;
    Ok(SecretStatus { configured: true })
}

/// Remove the stored proxy credentials.
pub fn proxy_credentials_clear(state: &AppState) -> Result<SecretStatus, AppError> {
    let secrets = state.secrets();
    secrets.delete_secret(PROXY_USERNAME_KEY)?;
    secrets.delete_secret(PROXY_PASSWORD_KEY)?;
    state.reapply_proxy_settings(&state.settings())?;
    Ok(SecretStatus { configured: false })
}

/// The most recently handled deep-link event.
pub fn deep_link_last(state: &AppState) -> DeepLinkEvent {
    state.last_deep_link()
}

/// Parse, log, emit, and store the last deep-link event.
pub fn handle_deep_link(state: &AppState, raw: &str) -> DeepLinkEvent {
    let event = match parse_deep_link(raw) {
        Ok(event) => event,
        Err(error) => DeepLinkEvent::rejected(error.code, error.message),
    };

    if event.accepted {
        log_event(state, LogLevel::Info, "deep_link_accepted", true, None);
    } else {
        let code = event.error.as_ref().map(|error| error.code.clone());
        log_event(
            state,
            LogLevel::Warn,
            "deep_link_rejected",
            false,
            code.as_deref(),
        );
    }

    state.set_last_deep_link(event.clone());
    event
}

/// Run a diagnostic secret round-trip.
pub fn diagnostics_secret_roundtrip(
    state: &AppState,
    secret: &str,
) -> Result<SecretRoundTripResult, AppError> {
    state.secrets().roundtrip(secret)
}

/// Current notification permission.
pub fn diagnostics_notification_status(state: &AppState) -> Result<NotificationStatus, AppError> {
    state.notifications().status()
}

/// Send a diagnostic notification.
pub fn diagnostics_send_notification(state: &AppState) -> Result<NotificationSendResult, AppError> {
    state.notifications().send_test()
}

/// Current autostart state.
pub fn diagnostics_autostart_get(state: &AppState) -> Result<AutostartState, AppError> {
    Ok(AutostartState {
        enabled: state.autostart().is_enabled()?,
    })
}

/// Enable or disable autostart.
pub fn diagnostics_autostart_set(
    state: &AppState,
    enabled: bool,
) -> Result<AutostartState, AppError> {
    Ok(AutostartState {
        enabled: state.autostart().set_enabled(enabled)?,
    })
}

/// Log a shutdown record, stop the engine, and request a graceful quit.
pub fn app_graceful_quit(state: &AppState) -> Result<QuitResult, AppError> {
    state.shutdown_engine();
    if let Err(error) = state.logger().log(
        LogLevel::Info,
        "shutdown",
        serde_json::json!({ "graceful": true }),
    ) {
        eprintln!("failed to write shutdown log record: {error}");
    }
    state.quit().request_quit()?;
    Ok(QuitResult { quitting: true })
}

fn log_event(state: &AppState, level: LogLevel, event: &str, accepted: bool, code: Option<&str>) {
    let mut fields = serde_json::json!({ "accepted": accepted });
    if let Some(code) = code {
        fields["code"] = serde_json::Value::String(code.to_string());
    }
    if let Err(error) = state.logger().log(level, event, fields) {
        eprintln!("failed to write deep-link log record: {error}");
    }
}

fn map_secret_error(error: SecretError) -> AppError {
    AppError::new(error.code, error.message)
}

fn secret_unavailable() -> AppError {
    AppError::new("secret_unavailable", "platform secret store is unavailable")
}
