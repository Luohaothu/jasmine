use std::sync::Arc;

use jasmine_core::{AppSettings, DeviceIdentity};
use tauri::State;

use super::types::{AppState, OwnFingerprintInfo, PeerFingerprintInfo};

#[tauri::command]
pub fn get_identity(state: State<'_, Arc<AppState>>) -> Result<DeviceIdentity, String> {
    get_identity_impl(state.inner().as_ref())
}

#[tauri::command]
pub fn update_display_name(state: State<'_, Arc<AppState>>, name: String) -> Result<(), String> {
    update_display_name_impl(state.inner().as_ref(), name)
}

#[tauri::command]
pub fn update_avatar(state: State<'_, Arc<AppState>>, path: String) -> Result<(), String> {
    update_avatar_impl(state.inner().as_ref(), path)
}

#[tauri::command]
pub fn get_settings(state: State<'_, Arc<AppState>>) -> Result<AppSettings, String> {
    get_settings_impl(state.inner().as_ref())
}

#[tauri::command]
pub fn get_own_fingerprint(state: State<'_, Arc<AppState>>) -> Result<OwnFingerprintInfo, String> {
    get_own_fingerprint_impl(state.inner().as_ref())
}

#[allow(non_snake_case)]
#[tauri::command]
pub fn get_peer_fingerprint(
    state: State<'_, Arc<AppState>>,
    peerId: String,
) -> Result<PeerFingerprintInfo, String> {
    get_peer_fingerprint_impl(state.inner().as_ref(), peerId)
}

#[allow(non_snake_case)]
#[tauri::command]
pub fn toggle_peer_verified(
    state: State<'_, Arc<AppState>>,
    peerId: String,
    verified: bool,
) -> Result<(), String> {
    toggle_peer_verified_impl(state.inner().as_ref(), peerId, verified)
}

#[tauri::command]
pub async fn update_settings(
    state: State<'_, Arc<AppState>>,
    settings: AppSettings,
) -> Result<AppSettings, String> {
    update_settings_impl(state.inner().as_ref(), settings).await
}

pub(crate) fn get_identity_impl(state: &AppState) -> Result<DeviceIdentity, String> {
    state.identity_service.get_identity()
}

pub(crate) fn update_display_name_impl(state: &AppState, name: String) -> Result<(), String> {
    let _ = state.identity_service.update_display_name(&name)?;
    Ok(())
}

pub(crate) fn update_avatar_impl(state: &AppState, path: String) -> Result<(), String> {
    let _ = state.identity_service.update_avatar(&path)?;
    Ok(())
}

pub(crate) fn get_settings_impl(state: &AppState) -> Result<AppSettings, String> {
    state.settings_service.get_settings()
}

pub(crate) fn get_own_fingerprint_impl(state: &AppState) -> Result<OwnFingerprintInfo, String> {
    state.identity_service.get_own_fingerprint()
}

pub(crate) fn get_peer_fingerprint_impl(
    state: &AppState,
    peer_id: String,
) -> Result<PeerFingerprintInfo, String> {
    state.identity_service.get_peer_fingerprint(&peer_id)
}

pub(crate) fn toggle_peer_verified_impl(
    state: &AppState,
    peer_id: String,
    verified: bool,
) -> Result<(), String> {
    state
        .identity_service
        .toggle_peer_verified(&peer_id, verified)
}

pub(crate) async fn update_settings_impl(
    state: &AppState,
    settings: AppSettings,
) -> Result<AppSettings, String> {
    state.settings_service.update_settings(settings).await
}
