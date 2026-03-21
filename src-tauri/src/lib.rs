use std::sync::Arc;

use tauri::Manager;

pub mod commands;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let app_data_dir: std::path::PathBuf = app
                .path()
                .app_data_dir()
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            let state = tauri::async_runtime::block_on(commands::setup_default_app_state(
                &app_data_dir,
                Arc::new(commands::TauriEmitter::new(app.handle().clone())),
            ))
            .map_err(std::io::Error::other)?;

            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::start_discovery,
            commands::stop_discovery,
            commands::get_peers,
            commands::send_message,
            commands::get_messages,
            commands::create_group,
            commands::send_group_message,
            commands::edit_message,
            commands::delete_message,
            commands::send_file,
            commands::send_folder,
            commands::accept_file,
            commands::reject_file,
            commands::cancel_transfer,
            commands::accept_folder_transfer,
            commands::reject_folder_transfer,
            commands::cancel_folder_transfer,
            commands::get_transfers,
            commands::get_identity,
            commands::update_display_name,
            commands::update_avatar,
            commands::get_settings,
            commands::update_settings
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
