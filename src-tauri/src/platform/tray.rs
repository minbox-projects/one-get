//! System tray icon with Show and Quit actions.

use std::sync::Arc;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

use crate::app_state::AppState;
use crate::commands::app;
use crate::error::AppError;

/// Build the tray icon and its menu.
pub fn build_tray(app: &AppHandle) -> Result<(), AppError> {
    let show = MenuItem::with_id(app, "show", "Show", true, None::<&str>)
        .map_err(|error| tray_error("create show menu item", error))?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)
        .map_err(|error| tray_error("create quit menu item", error))?;
    let menu = Menu::with_items(app, &[&show, &quit])
        .map_err(|error| tray_error("create tray menu", error))?;

    let mut builder = TrayIconBuilder::with_id("oneget-tray")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => focus_main_window(app),
            "quit" => request_quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                focus_main_window(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    builder
        .build(app)
        .map_err(|error| tray_error("build tray icon", error))?;
    Ok(())
}

fn focus_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn request_quit(app: &AppHandle) {
    let state = app.state::<Arc<AppState>>();
    let _ = app::app_graceful_quit(state.inner());
    app.exit(0);
}

fn tray_error(context: &str, error: impl std::fmt::Display) -> AppError {
    AppError::new("tray_setup_failed", format!("{context}: {error}"))
}
