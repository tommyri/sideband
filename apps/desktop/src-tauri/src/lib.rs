mod audio;
mod build_identity;
mod commands;
mod input;
mod onboarding;
mod persistence;
mod state;
mod types;
mod voice;
mod voice_view;

use state::AppState;
use std::sync::atomic::Ordering;
use tauri::{
    Manager, WindowEvent,
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::TrayIconBuilder,
};

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_store::Builder::default().build())
        .invoke_handler(tauri::generate_handler![
            commands::connect,
            commands::approve_fingerprint,
            commands::disconnect,
            commands::get_snapshot,
            commands::send_message,
            commands::create_channel,
            commands::load_history,
            onboarding::probe_server,
            onboarding::remove_server,
            onboarding::reorder_servers,
            voice::get_server_view,
            voice::join_voice,
            voice::leave_voice,
            voice::voice_control,
            voice::member_action,
            audio::audio_environment,
            audio::save_audio,
            audio::audio_level,
            audio::test_audio,
            audio::stop_audio_test,
            input::input_snapshot,
            input::save_shortcuts,
            input::suspend_shortcuts,
            input::capture_shortcut,
            input::cancel_shortcut_capture,
            input::configure_system_shortcuts,
        ])
        .setup(|app| {
            app.manage(AppState::open(&app.path().app_data_dir()?)?);
            input::init(app.handle());
            tauri::async_runtime::spawn(voice::watch(app.handle().clone()));
            let about = PredefinedMenuItem::about(
                app,
                Some("About this test build"),
                Some(build_identity::about()),
            )?;
            let app_menu = Menu::default(app.handle())?;
            app_menu.append(&Submenu::with_items(app, "Build", true, &[&about])?)?;
            app.set_menu(app_menu)?;
            let show = MenuItem::with_id(app, "show", "Show SocialSpeak", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let mute =
                CheckMenuItem::with_id(app, "mute", "Mute microphone", false, false, None::<&str>)?;
            let deafen =
                CheckMenuItem::with_id(app, "deafen", "Deafen", false, false, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &mute, &deafen, &about, &quit])?;
            app.manage(input::TrayControls { mute, deafen });
            let mut tray =
                TrayIconBuilder::new()
                    .menu(&menu)
                    .on_menu_event(|app, event| match event.id.as_ref() {
                        "show" => {
                            if let Some(window) = app.get_webview_window("main") {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                        "mute" => input::toggle(app, false),
                        "deafen" => input::toggle(app, true),
                        "quit" => app.exit(0),
                        _ => {}
                    });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<AppState>();
                if state.close_to_tray.load(Ordering::Relaxed) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("failed to run SocialSpeak desktop");
}
