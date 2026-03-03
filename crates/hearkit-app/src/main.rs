#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod state;

use state::AppState;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconEvent;
use tauri::{Emitter, Manager, WindowEvent};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("hearkit=debug".parse().unwrap()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            let app_state = AppState::new()?;
            let hotkey = app_state
                .pipeline
                .lock()
                .map(|p| p.config().app.hotkey.clone())
                .unwrap_or_else(|_| "CmdOrCtrl+Shift+R".to_string());
            app.manage(app_state);

            // Build tray menu
            let show = MenuItem::new(app, "Show Hearkit", true, None::<&str>)?;
            let separator = PredefinedMenuItem::separator(app)?;
            let quit = MenuItem::new(app, "Quit", true, None::<&str>)?;

            let menu = Menu::with_items(app, &[&show, &separator, &quit])?;

            let tray = app.tray_by_id("main").expect("tray not found");
            tray.set_menu(Some(menu))?;

            // Handle menu clicks
            let show_id = show.id().clone();
            let quit_id = quit.id().clone();
            tray.on_menu_event(move |app, event| {
                if event.id() == &show_id {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                } else if event.id() == &quit_id {
                    app.exit(0);
                }
            });

            // Handle tray icon click — show window on left click
            tray.on_tray_icon_event(|tray, event| {
                if let TrayIconEvent::Click { button, .. } = event {
                    if button == tauri::tray::MouseButton::Left {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                }
            });

            // Register global hotkey to toggle recording
            let shortcut: Shortcut = hotkey.parse().map_err(|e| {
                anyhow::anyhow!("failed to parse hotkey '{hotkey}': {e}")
            })?;
            if let Err(e) = app.global_shortcut().on_shortcut(shortcut, |app, _shortcut, event| {
                if event.state != ShortcutState::Pressed {
                    return;
                }

                let state = app.state::<AppState>();
                let mut pipeline = match state.pipeline.lock() {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::error!("pipeline lock poisoned: {e}");
                        return;
                    }
                };
                let mut recording = match state.recording.lock() {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::error!("recording lock poisoned: {e}");
                        return;
                    }
                };

                if let Some(handle) = recording.take() {
                    // Stop recording
                    match pipeline.stop_recording(handle) {
                        Ok(meeting) => {
                            tracing::info!("recording stopped via hotkey: {}", meeting.id);
                            let _ = app.emit("recording-toggled", false);
                        }
                        Err(e) => tracing::error!("failed to stop recording via hotkey: {e}"),
                    }
                } else {
                    // Start recording
                    match pipeline.start_recording() {
                        Ok(handle) => {
                            let id = handle.id.clone();
                            *recording = Some(handle);
                            tracing::info!("recording started via hotkey: {id}");
                            let _ = app.emit("recording-toggled", true);
                        }
                        Err(e) => tracing::error!("failed to start recording via hotkey: {e}"),
                    }
                }
            }) {
                tracing::warn!("failed to register global hotkey '{hotkey}': {e}");
            } else {
                tracing::info!("global hotkey registered: {hotkey}");
            }

            // Show the window on startup
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            // Hide window instead of closing (menubar app behavior)
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::start_recording,
            commands::stop_recording,
            commands::list_meetings,
            commands::get_meeting,
            commands::transcribe_meeting,
            commands::analyze_meeting,
            commands::get_settings,
            commands::save_settings,
            commands::check_model_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running hearkit");
}
