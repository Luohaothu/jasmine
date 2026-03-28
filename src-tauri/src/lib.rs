use std::sync::Arc;

use tauri::Manager;

pub mod commands;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init());

    #[cfg(feature = "e2e-test")]
    {
        builder = builder.plugin(tauri_plugin_webdriver::init());
    }

    builder
        .setup(|app| {
            let mut app_data_dir: std::path::PathBuf = app
                .path()
                .app_data_dir()
                .map_err(|error| std::io::Error::other(error.to_string()))?;

            let emitter = Arc::new(commands::TauriEmitter::new(app.handle().clone()));
            let test_config = std::env::var("JASMINE_TEST_CONFIG")
                .ok()
                .map(|raw| {
                    serde_json::from_str::<commands::TestConfig>(&raw)
                        .map_err(|error| std::io::Error::other(error.to_string()))
                })
                .transpose()?;

            let state = if let Some(test_config) = test_config {
                app_data_dir = std::path::PathBuf::from(test_config.app_data_dir);
                let runtime_config = commands::AppRuntimeConfig {
                    ws_bind_addr: test_config.ws_bind_addr,
                    discovery_mode: Some(test_config.discovery.mode),
                    mdns_service_type: Some({
                        // mDNS service names are limited to 15 chars (RFC 6763).
                        // Hash the namespace to a short suffix.
                        let hash = test_config
                            .discovery
                            .namespace
                            .bytes()
                            .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
                        format!("_jt-{:08x}._tcp.local", hash)
                    }),
                    keystore_mode: Some(test_config.keystore.mode),
                    keystore_root: Some(std::path::PathBuf::from(test_config.keystore.root)),
                };

                tauri::async_runtime::block_on(commands::setup_default_app_state_with_config(
                    &app_data_dir,
                    emitter,
                    runtime_config,
                ))
                .map_err(std::io::Error::other)?
            } else {
                tauri::async_runtime::block_on(commands::setup_default_app_state(
                    &app_data_dir,
                    emitter,
                ))
                .map_err(std::io::Error::other)?
            };

            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_webrtc_platform_info,
            commands::check_call_support,
            commands::start_discovery,
            commands::stop_discovery,
            commands::get_peers,
            commands::send_message,
            commands::send_call_join,
            commands::send_call_leave,
            commands::send_call_offer,
            commands::send_call_answer,
            commands::send_ice_candidate,
            commands::send_call_hangup,
            commands::send_call_reject,
            commands::get_messages,
            commands::get_reply_count,
            commands::get_reply_counts,
            commands::create_group,
            commands::add_group_members,
            commands::remove_group_members,
            commands::get_group_info,
            commands::list_groups,
            commands::leave_group,
            commands::fetch_og_metadata,
            commands::send_group_message,
            commands::edit_message,
            commands::delete_message,
            commands::send_file,
            commands::send_folder,
            commands::accept_file,
            commands::reject_file,
            commands::cancel_transfer,
            commands::resume_transfer,
            commands::retry_transfer,
            commands::accept_folder_transfer,
            commands::reject_folder_transfer,
            commands::cancel_folder_transfer,
            commands::get_transfers,
            commands::get_identity,
            commands::get_own_fingerprint,
            commands::get_peer_fingerprint,
            commands::toggle_peer_verified,
            commands::update_display_name,
            commands::update_avatar,
            commands::get_settings,
            commands::update_settings
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
