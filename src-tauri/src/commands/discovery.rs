use std::sync::Arc;

use jasmine_core::PeerInfo;
use tauri::State;

use super::types::{AppState, PeerPayload};

#[tauri::command]
pub async fn start_discovery(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    start_discovery_impl(state.inner().as_ref()).await
}

#[tauri::command]
pub async fn stop_discovery(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    stop_discovery_impl(state.inner().as_ref()).await
}

#[tauri::command]
pub async fn get_peers(state: State<'_, Arc<AppState>>) -> Result<Vec<PeerPayload>, String> {
    get_peers_impl(state.inner().as_ref()).await
}

pub(crate) async fn start_discovery_impl(state: &AppState) -> Result<(), String> {
    state.discovery.start().await
}

pub(crate) async fn stop_discovery_impl(state: &AppState) -> Result<(), String> {
    state.discovery.stop().await
}

pub(crate) async fn get_peers_impl(state: &AppState) -> Result<Vec<PeerPayload>, String> {
    Ok(state
        .discovery
        .peers()
        .into_iter()
        .map(peer_payload)
        .collect())
}

pub(crate) fn peer_payload(peer: PeerInfo) -> PeerPayload {
    PeerPayload {
        id: peer.device_id.0.to_string(),
        name: peer.display_name,
        status: "online".to_string(),
        warning: None,
    }
}
