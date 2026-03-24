use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use jasmine_core::{
    protocol::CallType, AppSettings, DeviceIdentity, Message, OgMetadata, PeerInfo,
};
use jasmine_storage::CachedOgMetadata;
use serde::Serialize;
use serde_json::Value;
use tauri::Emitter as _;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PeerPayload {
    pub id: String,
    pub name: String,
    pub status: String,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GroupMemberInfo {
    #[serde(rename = "id")]
    pub device_id: String,
    pub name: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupInfoResponse {
    pub id: String,
    pub name: String,
    pub members: Vec<GroupMemberInfo>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredGroupInfo {
    pub id: String,
    pub name: String,
    pub member_ids: Vec<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebRTCPlatformInfo {
    pub platform: String,
    pub webview: String,
    pub webview_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallSupportInfo {
    pub supported: bool,
    pub platform: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnFingerprintInfo {
    pub device_id: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerFingerprintInfo {
    pub peer_id: String,
    pub fingerprint: String,
    pub verified: bool,
    pub first_seen: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessagePayload {
    pub id: String,
    pub sender_id: String,
    pub receiver_id: String,
    pub content: String,
    pub timestamp: i64,
    pub encrypted: Option<bool>,
    pub status: String,
    pub edit_version: u32,
    pub edited_at: Option<u64>,
    pub is_deleted: bool,
    pub deleted_at: Option<u64>,
    pub reply_to_id: Option<String>,
    pub reply_to_preview: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferPayload {
    pub id: String,
    pub filename: String,
    pub size: u64,
    pub progress: f64,
    pub speed: u64,
    pub state: String,
    pub resumable: bool,
    pub sender_id: Option<String>,
    pub local_path: Option<String>,
    pub thumbnail_path: Option<String>,
    pub direction: Option<String>,
    pub folder_id: Option<String>,
    pub folder_relative_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileOfferPayload {
    pub(crate) id: String,
    pub(crate) filename: String,
    pub(crate) size: u64,
    pub(crate) sender_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct TransferProgressPayload {
    pub(crate) id: String,
    pub(crate) progress: f64,
    pub(crate) speed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TransferStatePayload {
    pub(crate) id: String,
    pub(crate) state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MessageEditedPayload {
    pub(crate) message_id: String,
    pub(crate) new_content: String,
    pub(crate) edit_version: u32,
    pub(crate) edited_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MessageDeletedPayload {
    pub(crate) message_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MentionReceivedPayload {
    pub(crate) message_id: String,
    pub(crate) mentioned_user_id: String,
    pub(crate) sender_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FolderOfferPayload {
    pub(crate) folder_transfer_id: String,
    pub(crate) folder_name: String,
    pub(crate) file_count: usize,
    pub(crate) total_size: u64,
    pub(crate) sender_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FolderProgressPayload {
    pub(crate) folder_transfer_id: String,
    pub(crate) sent_bytes: u64,
    pub(crate) total_bytes: u64,
    pub(crate) completed_files: usize,
    pub(crate) total_files: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FolderCompletedPayload {
    pub(crate) folder_transfer_id: String,
    pub(crate) status: String,
    pub(crate) failed_files: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ThumbnailReadyPayload {
    pub(crate) transfer_id: String,
    pub(crate) thumbnail_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ThumbnailFailedPayload {
    pub(crate) transfer_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PeerLostPayload {
    pub(crate) id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PeerIncompatiblePayload {
    pub(crate) id: String,
    pub(crate) reason: String,
}

#[derive(Debug, Clone)]
pub struct SetupContext {
    pub app_data_dir: PathBuf,
    pub database_path: PathBuf,
    pub runtime_config: AppRuntimeConfig,
}

#[derive(Debug, Clone)]
pub struct AppRuntimeConfig {
    pub ws_bind_addr: String,
}

impl Default for AppRuntimeConfig {
    fn default() -> Self {
        Self {
            ws_bind_addr: jasmine_messaging::DEFAULT_WS_BIND_ADDR.to_string(),
        }
    }
}

pub struct AppState {
    pub(crate) local_device_id: String,
    pub(crate) discovery: Arc<dyn DiscoveryServiceHandle>,
    pub(crate) messaging: Arc<dyn MessagingServiceHandle>,
    pub(crate) og_metadata: Arc<dyn OgMetadataServiceHandle>,
    pub(crate) transfers: Arc<dyn TransferServiceHandle>,
    pub(crate) identity_service: Arc<dyn IdentityServiceHandle>,
    pub(crate) settings_service: Arc<dyn SettingsServiceHandle>,
}

impl AppState {
    pub fn new(
        local_device_id: String,
        discovery: Arc<dyn DiscoveryServiceHandle>,
        messaging: Arc<dyn MessagingServiceHandle>,
        og_metadata: Arc<dyn OgMetadataServiceHandle>,
        transfers: Arc<dyn TransferServiceHandle>,
        identity_service: Arc<dyn IdentityServiceHandle>,
        settings_service: Arc<dyn SettingsServiceHandle>,
    ) -> Self {
        Self {
            local_device_id,
            discovery,
            messaging,
            og_metadata,
            transfers,
            identity_service,
            settings_service,
        }
    }

    pub fn local_device_id(&self) -> String {
        self.local_device_id.clone()
    }

    pub async fn start_discovery(&self) -> Result<(), String> {
        super::discovery::start_discovery_impl(self).await
    }

    pub async fn stop_discovery(&self) -> Result<(), String> {
        super::discovery::stop_discovery_impl(self).await
    }

    pub async fn get_peers(&self) -> Result<Vec<PeerPayload>, String> {
        super::discovery::get_peers_impl(self).await
    }

    pub async fn send_message(
        &self,
        peer_id: String,
        content: String,
        reply_to_id: Option<String>,
    ) -> Result<String, String> {
        super::messaging::send_message_impl(self, peer_id, content, reply_to_id).await
    }

    pub async fn send_call_offer(
        &self,
        peer_id: String,
        call_id: String,
        sdp: String,
        call_type: CallType,
    ) -> Result<(), String> {
        super::calls::send_call_offer_impl(self, peer_id, call_id, sdp, call_type).await
    }

    pub async fn send_call_join(&self, group_id: String, call_id: String) -> Result<(), String> {
        super::calls::send_call_join_impl(self, group_id, call_id).await
    }

    pub async fn send_call_leave(&self, call_id: String) -> Result<(), String> {
        super::calls::send_call_leave_impl(self, call_id).await
    }

    pub async fn send_call_answer(
        &self,
        peer_id: String,
        call_id: String,
        sdp: String,
    ) -> Result<(), String> {
        super::calls::send_call_answer_impl(self, peer_id, call_id, sdp).await
    }

    pub async fn send_ice_candidate(
        &self,
        peer_id: String,
        call_id: String,
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
    ) -> Result<(), String> {
        super::calls::send_ice_candidate_impl(
            self,
            peer_id,
            call_id,
            candidate,
            sdp_mid,
            sdp_mline_index,
        )
        .await
    }

    pub async fn send_call_hangup(&self, peer_id: String, call_id: String) -> Result<(), String> {
        super::calls::send_call_hangup_impl(self, peer_id, call_id).await
    }

    pub async fn send_call_reject(
        &self,
        peer_id: String,
        call_id: String,
        reason: Option<String>,
    ) -> Result<(), String> {
        super::calls::send_call_reject_impl(self, peer_id, call_id, reason).await
    }

    pub async fn get_messages(
        &self,
        chat_id: String,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<ChatMessagePayload>, String> {
        super::messaging::get_messages_impl(self, chat_id, limit, offset).await
    }

    pub async fn get_reply_count(&self, message_id: String) -> Result<i64, String> {
        super::messaging::get_reply_count_impl(self, message_id).await
    }

    pub async fn get_reply_counts(
        &self,
        message_ids: Vec<String>,
    ) -> Result<HashMap<String, i64>, String> {
        super::messaging::get_reply_counts_impl(self, message_ids).await
    }

    pub async fn create_group(
        &self,
        name: String,
        members: Vec<String>,
    ) -> Result<GroupInfoResponse, String> {
        super::messaging::create_group_impl(self, name, members).await
    }

    pub async fn add_group_members(
        &self,
        group_id: String,
        member_ids: Vec<String>,
    ) -> Result<(), String> {
        super::messaging::add_group_members_impl(self, group_id, member_ids).await
    }

    pub async fn remove_group_members(
        &self,
        group_id: String,
        member_ids: Vec<String>,
    ) -> Result<(), String> {
        super::messaging::remove_group_members_impl(self, group_id, member_ids).await
    }

    pub async fn get_group_info(&self, group_id: String) -> Result<GroupInfoResponse, String> {
        super::messaging::get_group_info_impl(self, group_id).await
    }

    pub async fn list_groups(&self) -> Result<Vec<GroupInfoResponse>, String> {
        super::messaging::list_groups_impl(self).await
    }

    pub async fn leave_group(&self, group_id: String) -> Result<(), String> {
        super::messaging::leave_group_impl(self, group_id).await
    }

    pub async fn send_group_message(
        &self,
        group_id: String,
        content: String,
        reply_to_id: Option<String>,
    ) -> Result<String, String> {
        super::messaging::send_group_message_impl(self, group_id, content, reply_to_id).await
    }

    pub async fn edit_message(
        &self,
        message_id: String,
        new_content: String,
    ) -> Result<(), String> {
        super::messaging::edit_message_impl(self, message_id, new_content).await
    }

    pub async fn delete_message(&self, message_id: String) -> Result<(), String> {
        super::messaging::delete_message_impl(self, message_id).await
    }

    pub async fn send_file(&self, peer_id: String, file_path: String) -> Result<String, String> {
        super::transfers::send_file_impl(self, peer_id, file_path).await
    }

    pub async fn send_folder(
        &self,
        peer_id: String,
        folder_path: String,
    ) -> Result<String, String> {
        super::transfers::send_folder_impl(self, peer_id, folder_path).await
    }

    pub async fn accept_file(&self, offer_id: String) -> Result<(), String> {
        super::transfers::accept_file_impl(self, offer_id).await
    }

    pub async fn reject_file(
        &self,
        offer_id: String,
        reason: Option<String>,
    ) -> Result<(), String> {
        super::transfers::reject_file_impl(self, offer_id, reason).await
    }

    pub async fn cancel_transfer(&self, transfer_id: String) -> Result<(), String> {
        super::transfers::cancel_transfer_impl(self, transfer_id).await
    }

    pub async fn resume_transfer(&self, transfer_id: String) -> Result<String, String> {
        super::transfers::resume_transfer_impl(self, transfer_id).await
    }

    pub async fn retry_transfer(&self, transfer_id: String) -> Result<String, String> {
        super::transfers::retry_transfer_impl(self, transfer_id).await
    }

    pub async fn accept_folder_transfer(
        &self,
        folder_id: String,
        target_dir: String,
    ) -> Result<(), String> {
        super::transfers::accept_folder_transfer_impl(self, folder_id, target_dir).await
    }

    pub async fn reject_folder_transfer(&self, folder_id: String) -> Result<(), String> {
        super::transfers::reject_folder_transfer_impl(self, folder_id).await
    }

    pub async fn cancel_folder_transfer(&self, folder_id: String) -> Result<(), String> {
        super::transfers::cancel_folder_transfer_impl(self, folder_id).await
    }

    pub async fn get_transfers(&self) -> Result<Vec<TransferPayload>, String> {
        super::transfers::get_transfers_impl(self).await
    }

    pub fn get_identity(&self) -> Result<DeviceIdentity, String> {
        super::identity::get_identity_impl(self)
    }

    pub fn update_display_name(&self, name: String) -> Result<(), String> {
        super::identity::update_display_name_impl(self, name)
    }

    pub fn update_avatar(&self, path: String) -> Result<(), String> {
        super::identity::update_avatar_impl(self, path)
    }

    pub fn get_settings(&self) -> Result<AppSettings, String> {
        super::identity::get_settings_impl(self)
    }

    pub fn get_own_fingerprint(&self) -> Result<OwnFingerprintInfo, String> {
        super::identity::get_own_fingerprint_impl(self)
    }

    pub fn get_peer_fingerprint(&self, peer_id: String) -> Result<PeerFingerprintInfo, String> {
        super::identity::get_peer_fingerprint_impl(self, peer_id)
    }

    pub fn toggle_peer_verified(&self, peer_id: String, verified: bool) -> Result<(), String> {
        super::identity::toggle_peer_verified_impl(self, peer_id, verified)
    }

    pub async fn update_settings(&self, settings: AppSettings) -> Result<AppSettings, String> {
        super::identity::update_settings_impl(self, settings).await
    }

    pub async fn shutdown(&self) -> Result<(), String> {
        let messaging_result = self.messaging.shutdown().await;
        let discovery_result = self.discovery.stop().await;

        match (messaging_result, discovery_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(messaging), Err(discovery)) => Err(format!(
                "app shutdown failed: messaging: {messaging}; discovery: {discovery}"
            )),
        }
    }
}

pub trait FrontendEmitter: Send + Sync {
    fn emit_json(&self, event: &str, payload: Value) -> Result<(), String>;
}

pub struct TauriEmitter {
    app_handle: tauri::AppHandle,
}

impl TauriEmitter {
    pub fn new(app_handle: tauri::AppHandle) -> Self {
        Self { app_handle }
    }
}

impl FrontendEmitter for TauriEmitter {
    fn emit_json(&self, event: &str, payload: Value) -> Result<(), String> {
        self.app_handle
            .emit(event, payload)
            .map_err(|error| error.to_string())
    }
}

#[async_trait]
pub trait AppSetupFactory: Send + Sync {
    async fn build(
        &self,
        context: SetupContext,
        emitter: Arc<dyn FrontendEmitter>,
    ) -> Result<AppState, String>;
}

#[async_trait]
pub trait DiscoveryServiceHandle: Send + Sync {
    async fn start(&self) -> Result<(), String>;
    async fn stop(&self) -> Result<(), String>;
    fn peers(&self) -> Vec<PeerInfo>;
}

#[async_trait]
pub trait MessagingServiceHandle: Send + Sync {
    async fn send_message(
        &self,
        peer_id: &str,
        content: &str,
        reply_to_id: Option<&str>,
    ) -> Result<Message, String>;
    async fn send_call_offer(
        &self,
        peer_id: &str,
        call_id: &str,
        sdp: &str,
        call_type: CallType,
    ) -> Result<(), String>;
    async fn send_call_join(&self, group_id: &str, call_id: &str) -> Result<(), String>;
    async fn send_call_leave(&self, call_id: &str) -> Result<(), String>;
    async fn send_call_answer(&self, peer_id: &str, call_id: &str, sdp: &str)
        -> Result<(), String>;
    async fn send_ice_candidate(
        &self,
        peer_id: &str,
        call_id: &str,
        candidate: &str,
        sdp_mid: Option<&str>,
        sdp_mline_index: Option<u16>,
    ) -> Result<(), String>;
    async fn send_call_hangup(&self, peer_id: &str, call_id: &str) -> Result<(), String>;
    async fn send_call_reject(
        &self,
        peer_id: &str,
        call_id: &str,
        reason: Option<&str>,
    ) -> Result<(), String>;
    async fn get_messages(
        &self,
        chat_id: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<Message>, String>;
    async fn get_reply_count(&self, message_id: &str) -> Result<i64, String>;
    async fn get_reply_counts(
        &self,
        message_ids: &[String],
    ) -> Result<HashMap<String, i64>, String>;
    async fn create_group(&self, name: &str, members: &[String])
        -> Result<StoredGroupInfo, String>;
    async fn add_group_members(
        &self,
        group_id: &str,
        member_ids: &[String],
    ) -> Result<StoredGroupInfo, String>;
    async fn remove_group_members(
        &self,
        group_id: &str,
        member_ids: &[String],
    ) -> Result<StoredGroupInfo, String>;
    async fn get_group_info(&self, group_id: &str) -> Result<StoredGroupInfo, String>;
    async fn list_groups(&self) -> Result<Vec<StoredGroupInfo>, String>;
    async fn leave_group(&self, group_id: &str) -> Result<(), String>;
    async fn send_group_message(
        &self,
        group_id: &str,
        content: &str,
        reply_to_id: Option<&str>,
    ) -> Result<Message, String>;
    async fn edit_message(
        &self,
        message_id: &str,
        new_content: &str,
        sender_id: &str,
    ) -> Result<(), String>;
    async fn delete_message(&self, message_id: &str, sender_id: &str) -> Result<(), String>;
    async fn shutdown(&self) -> Result<(), String>;
}

#[async_trait]
pub trait OgMetadataServiceHandle: Send + Sync {
    async fn get_cached_og_metadata(&self, url: &str) -> Result<Option<CachedOgMetadata>, String>;
    async fn save_og_metadata(&self, metadata: &OgMetadata, ttl_seconds: u64)
        -> Result<(), String>;
    async fn fetch_remote_og_metadata(&self, url: &str) -> Result<OgMetadata, String>;
}

#[async_trait]
pub trait TransferServiceHandle: Send + Sync {
    async fn send_file(&self, peer_id: &str, file_path: &Path) -> Result<String, String>;
    async fn send_folder(&self, peer_id: &str, folder_path: &Path) -> Result<String, String>;
    async fn accept_file(&self, offer_id: &str) -> Result<String, String>;
    async fn resume_transfer(&self, transfer_id: &str) -> Result<String, String>;
    async fn retry_transfer(&self, transfer_id: &str) -> Result<String, String>;
    async fn reject_file(&self, offer_id: &str, reason: Option<String>) -> Result<(), String>;
    async fn cancel_transfer(&self, transfer_id: &str) -> Result<(), String>;
    async fn accept_folder_transfer(
        &self,
        folder_id: &str,
        target_dir: &Path,
    ) -> Result<(), String>;
    async fn reject_folder_transfer(&self, folder_id: &str) -> Result<(), String>;
    async fn cancel_folder_transfer(&self, folder_id: &str) -> Result<(), String>;
    async fn get_transfers(&self) -> Result<Vec<TransferPayload>, String>;
}

pub trait IdentityServiceHandle: Send + Sync {
    fn get_identity(&self) -> Result<DeviceIdentity, String>;
    fn update_display_name(&self, name: &str) -> Result<DeviceIdentity, String>;
    fn update_avatar(&self, path: &str) -> Result<DeviceIdentity, String>;
    fn get_own_fingerprint(&self) -> Result<OwnFingerprintInfo, String>;
    fn get_peer_fingerprint(&self, peer_id: &str) -> Result<PeerFingerprintInfo, String>;
    fn toggle_peer_verified(&self, peer_id: &str, verified: bool) -> Result<(), String>;
}

#[async_trait]
pub trait SettingsServiceHandle: Send + Sync {
    fn get_settings(&self) -> Result<AppSettings, String>;
    async fn update_settings(&self, settings: AppSettings) -> Result<AppSettings, String>;
}
