use std::path::Path;
use std::sync::Arc;

use jasmine_core::{TransferRecord, TransferStatus};
use jasmine_transfer::{ManagedTransfer, TransferDirection};
use tauri::State;

use super::types::{AppState, TransferPayload};

#[tauri::command]
pub async fn send_file(
    state: State<'_, Arc<AppState>>,
    peer_id: String,
    file_path: String,
) -> Result<String, String> {
    send_file_impl(state.inner().as_ref(), peer_id, file_path).await
}

#[allow(non_snake_case)]
#[tauri::command]
pub async fn send_folder(
    state: State<'_, Arc<AppState>>,
    peerId: String,
    folderPath: String,
) -> Result<String, String> {
    send_folder_impl(state.inner().as_ref(), peerId, folderPath).await
}

#[tauri::command]
pub async fn accept_file(state: State<'_, Arc<AppState>>, offer_id: String) -> Result<(), String> {
    accept_file_impl(state.inner().as_ref(), offer_id).await
}

#[tauri::command]
pub async fn reject_file(
    state: State<'_, Arc<AppState>>,
    offer_id: String,
    reason: Option<String>,
) -> Result<(), String> {
    reject_file_impl(state.inner().as_ref(), offer_id, reason).await
}

#[tauri::command]
pub async fn cancel_transfer(
    state: State<'_, Arc<AppState>>,
    transfer_id: String,
) -> Result<(), String> {
    cancel_transfer_impl(state.inner().as_ref(), transfer_id).await
}

#[tauri::command]
pub async fn resume_transfer(
    state: State<'_, Arc<AppState>>,
    transfer_id: String,
) -> Result<String, String> {
    resume_transfer_impl(state.inner().as_ref(), transfer_id).await
}

#[tauri::command]
pub async fn retry_transfer(
    state: State<'_, Arc<AppState>>,
    transfer_id: String,
) -> Result<String, String> {
    retry_transfer_impl(state.inner().as_ref(), transfer_id).await
}

#[allow(non_snake_case)]
#[tauri::command]
pub async fn accept_folder_transfer(
    state: State<'_, Arc<AppState>>,
    folderId: String,
    targetDir: String,
) -> Result<(), String> {
    accept_folder_transfer_impl(state.inner().as_ref(), folderId, targetDir).await
}

#[allow(non_snake_case)]
#[tauri::command]
pub async fn reject_folder_transfer(
    state: State<'_, Arc<AppState>>,
    folderId: String,
) -> Result<(), String> {
    reject_folder_transfer_impl(state.inner().as_ref(), folderId).await
}

#[allow(non_snake_case)]
#[tauri::command]
pub async fn cancel_folder_transfer(
    state: State<'_, Arc<AppState>>,
    folderId: String,
) -> Result<(), String> {
    cancel_folder_transfer_impl(state.inner().as_ref(), folderId).await
}

#[tauri::command]
pub async fn get_transfers(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<TransferPayload>, String> {
    get_transfers_impl(state.inner().as_ref()).await
}

pub(crate) async fn send_file_impl(
    state: &AppState,
    peer_id: String,
    file_path: String,
) -> Result<String, String> {
    state
        .transfers
        .send_file(&peer_id, Path::new(&file_path))
        .await
}

pub(crate) async fn send_folder_impl(
    state: &AppState,
    peer_id: String,
    folder_path: String,
) -> Result<String, String> {
    state
        .transfers
        .send_folder(&peer_id, Path::new(&folder_path))
        .await
}

pub(crate) async fn accept_file_impl(state: &AppState, offer_id: String) -> Result<(), String> {
    let _ = state.transfers.accept_file(&offer_id).await?;
    Ok(())
}

pub(crate) async fn reject_file_impl(
    state: &AppState,
    offer_id: String,
    reason: Option<String>,
) -> Result<(), String> {
    state.transfers.reject_file(&offer_id, reason).await
}

pub(crate) async fn cancel_transfer_impl(
    state: &AppState,
    transfer_id: String,
) -> Result<(), String> {
    state.transfers.cancel_transfer(&transfer_id).await
}

pub(crate) async fn resume_transfer_impl(
    state: &AppState,
    transfer_id: String,
) -> Result<String, String> {
    state.transfers.resume_transfer(&transfer_id).await
}

pub(crate) async fn retry_transfer_impl(
    state: &AppState,
    transfer_id: String,
) -> Result<String, String> {
    state.transfers.retry_transfer(&transfer_id).await
}

pub(crate) async fn accept_folder_transfer_impl(
    state: &AppState,
    folder_id: String,
    target_dir: String,
) -> Result<(), String> {
    state
        .transfers
        .accept_folder_transfer(&folder_id, Path::new(&target_dir))
        .await
}

pub(crate) async fn reject_folder_transfer_impl(
    state: &AppState,
    folder_id: String,
) -> Result<(), String> {
    state.transfers.reject_folder_transfer(&folder_id).await
}

pub(crate) async fn cancel_folder_transfer_impl(
    state: &AppState,
    folder_id: String,
) -> Result<(), String> {
    state.transfers.cancel_folder_transfer(&folder_id).await
}

pub(crate) async fn get_transfers_impl(state: &AppState) -> Result<Vec<TransferPayload>, String> {
    state.transfers.get_transfers().await
}

pub(crate) fn transfer_payload_from_managed(transfer: &ManagedTransfer) -> TransferPayload {
    let progress = transfer.progress.as_ref().map_or_else(
        || progress_ratio(transfer.status.clone(), 0, transfer.total_bytes),
        |progress| {
            progress_ratio(
                transfer.status.clone(),
                progress.bytes_sent,
                progress.total_bytes,
            )
        },
    );
    let speed = transfer
        .progress
        .as_ref()
        .map_or(0, |progress| progress.speed_bps);

    TransferPayload {
        id: transfer.id.clone(),
        filename: transfer.file_name.clone(),
        size: transfer.total_bytes,
        progress,
        speed,
        state: transfer_status_label(&transfer.status).to_string(),
        resumable: transfer.direction == TransferDirection::Receive
            && transfer.status == TransferStatus::Failed
            && transfer.resume_token.is_some(),
        sender_id: Some(transfer.peer_id.0.to_string()),
        local_path: Some(transfer.local_path.to_string_lossy().into_owned()),
        thumbnail_path: None,
        direction: Some(transfer_direction_label(transfer.direction).to_string()),
        folder_id: transfer.folder_id.clone(),
        folder_relative_path: transfer.folder_relative_path.clone(),
    }
}

pub(crate) fn transfer_payload_from_record(record: &TransferRecord) -> TransferPayload {
    let size = std::fs::metadata(&record.local_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    TransferPayload {
        id: record.id.to_string(),
        filename: record.file_name.clone(),
        size,
        progress: progress_ratio(record.status.clone(), size, size),
        speed: 0,
        state: transfer_status_label(&record.status).to_string(),
        resumable: record.status == TransferStatus::Failed && record.resume_token.is_some(),
        sender_id: Some(record.peer_id.0.to_string()),
        local_path: Some(record.local_path.to_string_lossy().into_owned()),
        thumbnail_path: record.thumbnail_path.clone(),
        direction: None,
        folder_id: record.folder_id.clone(),
        folder_relative_path: record.folder_relative_path.clone(),
    }
}

pub(crate) fn transfer_direction_label(direction: TransferDirection) -> &'static str {
    match direction {
        TransferDirection::Send => "send",
        TransferDirection::Receive => "receive",
    }
}

pub(crate) fn progress_ratio(status: TransferStatus, bytes_sent: u64, total_bytes: u64) -> f64 {
    match status {
        TransferStatus::Completed => 1.0,
        _ if total_bytes == 0 => 0.0,
        _ => bytes_sent as f64 / total_bytes as f64,
    }
}

pub(crate) fn transfer_status_label(status: &TransferStatus) -> &'static str {
    match status {
        TransferStatus::Pending => "queued",
        TransferStatus::Active => "active",
        TransferStatus::Completed => "completed",
        TransferStatus::Failed | TransferStatus::Cancelled => "failed",
    }
}
