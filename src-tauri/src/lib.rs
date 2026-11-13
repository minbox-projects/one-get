//! OneGet desktop shell: plugins, runtime initialization, tray, and deep links.

#![deny(dead_code, unused_imports)]

pub mod api;
pub mod app_state;
pub mod commands;
pub mod config;
pub mod engine;
pub mod error;
pub mod kernel;
pub mod logging;
pub mod platform;
pub mod store;

use std::sync::{Arc, Mutex};

use tauri::{Emitter, Manager, WindowEvent};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_deep_link::DeepLinkExt;

use crate::api::clock::{Clock, SystemClock};
use crate::api::desktop::TauriDesktopBridge;
use crate::api::pairing_store::PairingStore;
use crate::api::protocol::API_PORT;
use crate::api::rate_limit::RateLimitPolicy;
use crate::api::server::{ApiServerConfig, ApiServerHandle};
use crate::app_state::{AppState, PairingRuntime};
use crate::commands::app::{
    AutostartService, KeyringSecretService, NotificationService, PlatformQuitService, QuitService,
    SecretService,
};
use crate::config::{CloseBehavior, SettingsStore};
use crate::engine::scheduler::EngineOptions;
use crate::engine::service::EngineService;
use crate::logging::{LogConfig, Logger};
use crate::platform::autostart::PluginAutostartService;
use crate::platform::deeplink::register_schemes;
use crate::platform::notifications::PluginNotificationService;
use crate::platform::paths::AppPaths;
use crate::store::Database;

const PAIR_ORIGIN_REQUESTS: usize = 10;
const PAIR_ORIGIN_WINDOW_MS: i64 = 60_000;
const CAPTURE_TOKEN_REQUESTS: usize = 120;
const CAPTURE_TOKEN_WINDOW_MS: i64 = 60_000;

/// Build and run the desktop application.
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            focus_main_window(app);
            forward_deep_links(app, &argv);
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            commands::ipc::app_info,
            commands::ipc::settings_get,
            commands::ipc::settings_status,
            commands::ipc::settings_save,
            commands::ipc::deep_link_last,
            commands::ipc::diagnostics_secret_roundtrip,
            commands::ipc::diagnostics_notification_status,
            commands::ipc::diagnostics_send_notification,
            commands::ipc::diagnostics_autostart_get,
            commands::ipc::diagnostics_autostart_set,
            commands::ipc::app_graceful_quit,
            commands::ipc::downloads_list,
            commands::ipc::downloads_segments,
            commands::ipc::downloads_probe,
            commands::ipc::downloads_add,
            commands::ipc::downloads_pause,
            commands::ipc::downloads_resume,
            commands::ipc::downloads_cancel,
            commands::ipc::downloads_retry,
            commands::ipc::downloads_delete_record,
            commands::ipc::downloads_delete_file,
            commands::ipc::downloads_open_file,
            commands::ipc::downloads_open_folder,
            commands::ipc::pairing_status,
            commands::ipc::pairing_generate_code,
            commands::ipc::pairing_list_clients,
            commands::ipc::pairing_revoke,
            commands::ipc::pairing_take_pending_prefill,
        ])
        .setup(|app| {
            initialize(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let state = window.state::<Arc<AppState>>();
                    match state.settings().close_behavior {
                        CloseBehavior::Tray => {
                            let _ = window.hide();
                        }
                        CloseBehavior::Quit => {
                            state.shutdown_engine();
                            window.app_handle().exit(0);
                        }
                    }
                }
            }
        });

    if let Err(error) = builder.run(tauri::generate_context!()) {
        eprintln!("fatal startup error: {error}");
        std::process::exit(1);
    }
}

fn initialize(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let paths = AppPaths::for_user()?;
    paths.ensure()?;

    let settings = SettingsStore::load(paths.data.join("settings.json"))?;
    let snapshot = settings.settings();
    let level = snapshot.log_level;
    let logger = Logger::new(LogConfig::with_defaults(paths.logs.clone()).with_level(level))?;

    let destination = snapshot
        .default_directory
        .clone()
        .unwrap_or_else(|| paths.downloads.clone());
    let max_concurrent = usize::try_from(snapshot.max_concurrent_tasks)
        .unwrap_or(1)
        .max(1);
    let store = Database::open(paths.data.join("tasks.db"))?;
    let engine = Arc::new(EngineService::start(
        store,
        destination,
        EngineOptions { max_concurrent },
    )?);
    engine.apply_settings(max_concurrent, snapshot.global_rate_limit)?;

    let secrets: Arc<dyn SecretService> = Arc::new(KeyringSecretService::new());
    let notifications: Arc<dyn NotificationService> =
        Arc::new(PluginNotificationService::new(app.handle().clone()));
    let autostart: Arc<dyn AutostartService> =
        Arc::new(PluginAutostartService::new(app.handle().clone()));
    let quit: Arc<dyn QuitService> = Arc::new(PlatformQuitService);

    let (pairing_runtime, pairing_store) = match PairingStore::open(paths.data.join("pairing.db")) {
        Ok(store) => {
            let store = Arc::new(Mutex::new(store));
            let clock: Arc<dyn Clock> = Arc::new(SystemClock::default());
            let runtime = PairingRuntime::available(Arc::clone(&store), clock);
            (runtime, Some(store))
        }
        Err(error) => (PairingRuntime::unavailable(error.code), None),
    };

    let state = Arc::new(
        AppState::new(
            paths,
            settings,
            logger,
            secrets,
            notifications,
            autostart,
            quit,
        )
        .with_engine(Arc::clone(&engine))
        .with_pairing(pairing_runtime),
    );

    if let Some(store) = pairing_store {
        let bridge = Arc::new(TauriDesktopBridge::new(
            app.handle().clone(),
            Arc::downgrade(&state),
        ));
        let config = ApiServerConfig {
            port: API_PORT,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            pairing_store: store,
            bridge,
            clock: Arc::new(SystemClock::default()),
            pair_origin_policy: RateLimitPolicy::new(PAIR_ORIGIN_REQUESTS, PAIR_ORIGIN_WINDOW_MS),
            token_policy: RateLimitPolicy::new(CAPTURE_TOKEN_REQUESTS, CAPTURE_TOKEN_WINDOW_MS),
        };
        match ApiServerHandle::start(config) {
            Ok(handle) => state.attach_api_server(handle),
            Err(error) => state.set_pairing_unavailable(&error.code),
        }
    }

    app.manage(Arc::clone(&state));

    platform::tray::build_tray(app.handle())?;
    register_schemes(app.handle())?;

    let handle = app.handle().clone();
    app.deep_link().on_open_url(move |event| {
        for url in event.urls() {
            let state = handle.state::<Arc<AppState>>();
            let result = commands::app::handle_deep_link(state.inner(), url.as_str());
            if let Err(error) = handle.emit("app://deep-link", &result) {
                eprintln!("failed to emit deep-link event: {error}");
            }
            focus_main_window(&handle);
        }
    });

    Ok(())
}

fn focus_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn forward_deep_links(app: &tauri::AppHandle, argv: &[String]) {
    for argument in argv {
        let is_deep_link = argument
            .get(..9)
            .map(|prefix| prefix.eq_ignore_ascii_case("oneget://"))
            .unwrap_or(false);
        if !is_deep_link {
            continue;
        }
        let state = app.state::<Arc<AppState>>();
        let result = commands::app::handle_deep_link(state.inner(), argument);
        if let Err(error) = app.emit("app://deep-link", &result) {
            eprintln!("failed to emit deep-link event: {error}");
        }
    }
}
