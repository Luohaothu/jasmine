use std::sync::Arc;

use jasmine_core::protocol::CallType;
use tauri::State;

use super::types::AppState;

#[allow(non_snake_case)]
#[tauri::command]
pub async fn send_call_join(
    state: State<'_, Arc<AppState>>,
    groupId: String,
    callId: String,
) -> Result<(), String> {
    send_call_join_impl(state.inner().as_ref(), groupId, callId).await
}

#[allow(non_snake_case)]
#[tauri::command]
pub async fn send_call_leave(
    state: State<'_, Arc<AppState>>,
    callId: String,
) -> Result<(), String> {
    send_call_leave_impl(state.inner().as_ref(), callId).await
}

#[allow(non_snake_case)]
#[tauri::command]
pub async fn send_call_offer(
    state: State<'_, Arc<AppState>>,
    peerId: String,
    callId: String,
    sdp: String,
    callType: CallType,
) -> Result<(), String> {
    send_call_offer_impl(state.inner().as_ref(), peerId, callId, sdp, callType).await
}

#[allow(non_snake_case)]
#[tauri::command]
pub async fn send_call_answer(
    state: State<'_, Arc<AppState>>,
    peerId: String,
    callId: String,
    sdp: String,
) -> Result<(), String> {
    send_call_answer_impl(state.inner().as_ref(), peerId, callId, sdp).await
}

#[allow(non_snake_case)]
#[tauri::command]
pub async fn send_ice_candidate(
    state: State<'_, Arc<AppState>>,
    peerId: String,
    callId: String,
    candidate: String,
    sdpMid: Option<String>,
    sdpMlineIndex: Option<u16>,
) -> Result<(), String> {
    send_ice_candidate_impl(
        state.inner().as_ref(),
        peerId,
        callId,
        candidate,
        sdpMid,
        sdpMlineIndex,
    )
    .await
}

#[allow(non_snake_case)]
#[tauri::command]
pub async fn send_call_hangup(
    state: State<'_, Arc<AppState>>,
    peerId: String,
    callId: String,
) -> Result<(), String> {
    send_call_hangup_impl(state.inner().as_ref(), peerId, callId).await
}

#[allow(non_snake_case)]
#[tauri::command]
pub async fn send_call_reject(
    state: State<'_, Arc<AppState>>,
    peerId: String,
    callId: String,
    reason: Option<String>,
) -> Result<(), String> {
    send_call_reject_impl(state.inner().as_ref(), peerId, callId, reason).await
}

pub(crate) async fn send_call_offer_impl(
    state: &AppState,
    peer_id: String,
    call_id: String,
    sdp: String,
    call_type: CallType,
) -> Result<(), String> {
    state
        .messaging
        .send_call_offer(&peer_id, &call_id, &sdp, call_type)
        .await
}

pub(crate) async fn send_call_join_impl(
    state: &AppState,
    group_id: String,
    call_id: String,
) -> Result<(), String> {
    state.messaging.send_call_join(&group_id, &call_id).await
}

pub(crate) async fn send_call_leave_impl(state: &AppState, call_id: String) -> Result<(), String> {
    state.messaging.send_call_leave(&call_id).await
}

pub(crate) async fn send_call_answer_impl(
    state: &AppState,
    peer_id: String,
    call_id: String,
    sdp: String,
) -> Result<(), String> {
    state
        .messaging
        .send_call_answer(&peer_id, &call_id, &sdp)
        .await
}

pub(crate) async fn send_ice_candidate_impl(
    state: &AppState,
    peer_id: String,
    call_id: String,
    candidate: String,
    sdp_mid: Option<String>,
    sdp_mline_index: Option<u16>,
) -> Result<(), String> {
    state
        .messaging
        .send_ice_candidate(
            &peer_id,
            &call_id,
            &candidate,
            sdp_mid.as_deref(),
            sdp_mline_index,
        )
        .await
}

pub(crate) async fn send_call_hangup_impl(
    state: &AppState,
    peer_id: String,
    call_id: String,
) -> Result<(), String> {
    state.messaging.send_call_hangup(&peer_id, &call_id).await
}

pub(crate) async fn send_call_reject_impl(
    state: &AppState,
    peer_id: String,
    call_id: String,
    reason: Option<String>,
) -> Result<(), String> {
    state
        .messaging
        .send_call_reject(&peer_id, &call_id, reason.as_deref())
        .await
}
