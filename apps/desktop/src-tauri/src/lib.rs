mod admin;
mod attachments;
mod audio;
mod audit;
mod build_identity;
mod channel_admin;
mod chat;
mod commands;
mod diagnostics;
mod dm;
mod input;
mod moderation_admin;
mod notifications;
mod onboarding;
mod persistence;
mod role_admin;
mod search;
mod server_settings;
mod state;
mod testing;
mod types;
mod updates;
mod viewer;
mod viewer_gpu;
mod voice;
mod voice_view;
mod webhooks;

use state::AppState;
use std::sync::atomic::Ordering;
use tauri::{
    Manager, WindowEvent,
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::TrayIconBuilder,
};

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(updates::Pending::default())
        .manage(viewer::Viewers::default())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_store::Builder::default().build())
        .invoke_handler(tauri::generate_handler![
            viewer::open_stream_viewer,
            viewer::stream_viewers,
            updates::check_update,
            updates::install_update,
            notifications::notification_preferences,
            notifications::save_notification_preferences,
            notifications::test_notification,
            diagnostics::diagnostics_snapshot,
            testing::test_build_identity,
            testing::export_test_report,
            commands::connect,
            commands::approve_fingerprint,
            commands::disconnect,
            commands::get_snapshot,
            commands::send_message,
            commands::create_channel,
            commands::load_history,
            chat::chat_snapshot,
            chat::chat_action,
            chat::message_context,
            chat::open_chat_link,
            admin::invite_page,
            moderation_admin::get_moderation_page,
            moderation_admin::moderate_member,
            server_settings::get_server_settings,
            server_settings::save_server_settings,
            server_settings::upload_server_icon,
            search::search_messages,
            role_admin::role_admin_snapshot,
            role_admin::role_admin_action,
            role_admin::role_overrides,
            channel_admin::channel_admin_snapshot,
            channel_admin::channel_admin_action,
            webhooks::webhook_page,
            webhooks::create_server_webhook,
            webhooks::revoke_server_webhook,
            audit::audit_page,
            admin::create_server_invite,
            admin::revoke_server_invite,
            attachments::attachment_transfer,
            attachments::cancel_attachment,
            dm::dm_action,
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
            notifications::init(app.handle());
            tauri::async_runtime::spawn(voice::watch(app.handle().clone()));
            let about = PredefinedMenuItem::about(
                app,
                Some("About this test build"),
                Some(build_identity::about()),
            )?;
            let app_menu = Menu::default(app.handle())?;
            app_menu.append(&Submenu::with_items(app, "Build", true, &[&about])?)?;
            app.set_menu(app_menu)?;
            let show = MenuItem::with_id(app, "show", "Show Sideband", true, None::<&str>)?;
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
                if window.label() == "main" && state.close_to_tray.load(Ordering::Relaxed) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("failed to run Sideband desktop");
}
