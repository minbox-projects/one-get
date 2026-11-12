//! OneGet desktop shell: plugins, runtime initialization, tray, and deep links.

pub mod app_state;
pub mod commands;
pub mod config;
pub mod error;
pub mod logging;
pub mod platform;

use std::sync::Arc;

use tauri::{Emitter, Manager, WindowEvent};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_deep_link::DeepLinkExt;

use crate::app_state::AppState;
use crate::commands::app::{
    AutostartService, KeyringSecretService, NotificationService, PlatformQuitService, QuitService,
    SecretService,
};
use crate::config::SettingsStore;
use crate::logging::{LogConfig, Logger};
use crate::platform::autostart::PluginAutostartService;
use crate::platform::deeplink::register_schemes;
use crate::platform::notifications::PluginNotificationService;
use crate::platform::paths::AppPaths;

/// Build and run the desktop application.
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            focus_main_window(app);
            forward_deep_links(app, &argv);
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            commands::ipc::app_info,
            commands::ipc::settings_get,
            commands::ipc::settings_save,
            commands::ipc::deep_link_last,
            commands::ipc::diagnostics_secret_roundtrip,
            commands::ipc::diagnostics_notification_status,
            commands::ipc::diagnostics_send_notification,
            commands::ipc::diagnostics_autostart_get,
            commands::ipc::diagnostics_autostart_set,
            commands::ipc::app_graceful_quit,
        ])
        .setup(|app| {
            initialize(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
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
    let level = settings.settings().log_level;
    let logger = Logger::new(LogConfig::with_defaults(paths.logs.clone()).with_level(level))?;

    let secrets: Arc<dyn SecretService> = Arc::new(KeyringSecretService::new());
    let notifications: Arc<dyn NotificationService> =
        Arc::new(PluginNotificationService::new(app.handle().clone()));
    let autostart: Arc<dyn AutostartService> =
        Arc::new(PluginAutostartService::new(app.handle().clone()));
    let quit: Arc<dyn QuitService> = Arc::new(PlatformQuitService);

    let state = Arc::new(AppState::new(
        paths,
        settings,
        logger,
        secrets,
        notifications,
        autostart,
        quit,
    ));
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
