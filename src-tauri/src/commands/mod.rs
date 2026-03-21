use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use async_trait::async_trait;
use jasmine_core::{
    protocol::FolderManifestData, AppSettings, ChatId, DeviceId, DeviceIdentity, DiscoveryService,
    IdentityStore, Message, MessageStatus, PeerInfo, ProtocolMessage, SettingsService,
    StorageEngine, TransferRecord, TransferStatus,
};
use jasmine_discovery::{DiscoveryManager, MdnsDiscoveryConfig, UdpBroadcastConfig};
use jasmine_messaging::{
    ChatService, ChatServiceEvent, WsClient, WsClientConfig, WsClientEvent, WsPeerIdentity,
    WsServer, WsServerConfig, WsServerEvent,
};
use jasmine_storage::SqliteStorage;
use jasmine_transfer::{
    FileReceiver, FileReceiverError, FileReceiverSignal, FileSender, FileSenderError,
    FileSenderSignal, FolderOfferNotification, FolderProgress, FolderProgressReporter,
    FolderReceiver, FolderTransferCoordinator, FolderTransferOutcome, FolderTransferStatus,
    TransferDirection, TransferManager,
};
use serde::Serialize;
use serde_json::Value;
use tauri::{Emitter as _, State};
use tokio::sync::oneshot;
use tokio::time::{self, Duration};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const EVENT_PEER_DISCOVERED: &str = "peer-discovered";
const EVENT_PEER_LOST: &str = "peer-lost";
const EVENT_MESSAGE_RECEIVED: &str = "message-received";
const EVENT_FILE_OFFER_RECEIVED: &str = "file-offer-received";
const EVENT_TRANSFER_PROGRESS: &str = "transfer-progress";
const EVENT_TRANSFER_STATE_CHANGED: &str = "transfer-state-changed";
const EVENT_MESSAGE_EDITED: &str = "message-edited";
const EVENT_MESSAGE_DELETED: &str = "message-deleted";
const EVENT_MENTION_RECEIVED: &str = "mention-received";
const EVENT_FOLDER_OFFER_RECEIVED: &str = "folder-offer-received";
const EVENT_FOLDER_PROGRESS: &str = "folder-progress";
const EVENT_FOLDER_COMPLETED: &str = "folder-completed";
const EVENT_THUMBNAIL_READY: &str = "thumbnail-ready";
const EVENT_THUMBNAIL_FAILED: &str = "thumbnail-failed";
const TRANSFER_POLL_INTERVAL: Duration = Duration::from_millis(250);
const TRANSFER_HISTORY_LIMIT: usize = 256;
const THUMBNAIL_WATCH_TIMEOUT: Duration = Duration::from_secs(5);

type DefaultTransferManager = TransferManager<
    FileSender<TransferSignalBridge>,
    FileReceiver<TransferSignalBridge>,
    SqliteStorage,
>;
type DefaultFolderCoordinator =
    FolderTransferCoordinator<TransferSignalBridge, Arc<DefaultTransferManager>>;
type DefaultFolderReceiver = FolderReceiver<TransferSignalBridge>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PeerPayload {
    pub id: String,
    pub name: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessagePayload {
    pub id: String,
    pub sender_id: String,
    pub receiver_id: String,
    pub content: String,
    pub timestamp: i64,
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
    pub sender_id: Option<String>,
    pub local_path: Option<String>,
    pub thumbnail_path: Option<String>,
    pub direction: Option<String>,
    pub folder_id: Option<String>,
    pub folder_relative_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileOfferPayload {
    id: String,
    filename: String,
    size: u64,
    sender_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct TransferProgressPayload {
    id: String,
    progress: f64,
    speed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TransferStatePayload {
    id: String,
    state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct MessageEditedPayload {
    message_id: String,
    new_content: String,
    edit_version: u32,
    edited_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct MessageDeletedPayload {
    message_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct MentionReceivedPayload {
    message_id: String,
    mentioned_user_id: String,
    sender_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct FolderOfferPayload {
    folder_transfer_id: String,
    folder_name: String,
    file_count: usize,
    total_size: u64,
    sender_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct FolderProgressPayload {
    folder_transfer_id: String,
    sent_bytes: u64,
    total_bytes: u64,
    completed_files: usize,
    total_files: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct FolderCompletedPayload {
    folder_transfer_id: String,
    status: String,
    failed_files: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ThumbnailReadyPayload {
    transfer_id: String,
    thumbnail_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ThumbnailFailedPayload {
    transfer_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PeerLostPayload {
    id: String,
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
    local_device_id: String,
    discovery: Arc<dyn DiscoveryServiceHandle>,
    messaging: Arc<dyn MessagingServiceHandle>,
    transfers: Arc<dyn TransferServiceHandle>,
    identity_service: Arc<dyn IdentityServiceHandle>,
    settings_service: Arc<dyn SettingsServiceHandle>,
}

impl AppState {
    pub fn new(
        local_device_id: String,
        discovery: Arc<dyn DiscoveryServiceHandle>,
        messaging: Arc<dyn MessagingServiceHandle>,
        transfers: Arc<dyn TransferServiceHandle>,
        identity_service: Arc<dyn IdentityServiceHandle>,
        settings_service: Arc<dyn SettingsServiceHandle>,
    ) -> Self {
        Self {
            local_device_id,
            discovery,
            messaging,
            transfers,
            identity_service,
            settings_service,
        }
    }

    pub fn local_device_id(&self) -> String {
        self.local_device_id.clone()
    }

    pub async fn start_discovery(&self) -> Result<(), String> {
        start_discovery_impl(self).await
    }

    pub async fn stop_discovery(&self) -> Result<(), String> {
        stop_discovery_impl(self).await
    }

    pub async fn get_peers(&self) -> Result<Vec<PeerPayload>, String> {
        get_peers_impl(self).await
    }

    pub async fn send_message(
        &self,
        peer_id: String,
        content: String,
        reply_to_id: Option<String>,
    ) -> Result<String, String> {
        send_message_impl(self, peer_id, content, reply_to_id).await
    }

    pub async fn get_messages(
        &self,
        chat_id: String,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<ChatMessagePayload>, String> {
        get_messages_impl(self, chat_id, limit, offset).await
    }

    pub async fn create_group(&self, name: String, members: Vec<String>) -> Result<String, String> {
        create_group_impl(self, name, members).await
    }

    pub async fn send_group_message(
        &self,
        group_id: String,
        content: String,
        reply_to_id: Option<String>,
    ) -> Result<String, String> {
        send_group_message_impl(self, group_id, content, reply_to_id).await
    }

    pub async fn edit_message(
        &self,
        message_id: String,
        new_content: String,
    ) -> Result<(), String> {
        edit_message_impl(self, message_id, new_content).await
    }

    pub async fn delete_message(&self, message_id: String) -> Result<(), String> {
        delete_message_impl(self, message_id).await
    }

    pub async fn send_file(&self, peer_id: String, file_path: String) -> Result<String, String> {
        send_file_impl(self, peer_id, file_path).await
    }

    pub async fn send_folder(
        &self,
        peer_id: String,
        folder_path: String,
    ) -> Result<String, String> {
        send_folder_impl(self, peer_id, folder_path).await
    }

    pub async fn accept_file(&self, offer_id: String) -> Result<(), String> {
        accept_file_impl(self, offer_id).await
    }

    pub async fn reject_file(
        &self,
        offer_id: String,
        reason: Option<String>,
    ) -> Result<(), String> {
        reject_file_impl(self, offer_id, reason).await
    }

    pub async fn cancel_transfer(&self, transfer_id: String) -> Result<(), String> {
        cancel_transfer_impl(self, transfer_id).await
    }

    pub async fn accept_folder_transfer(
        &self,
        folder_id: String,
        target_dir: String,
    ) -> Result<(), String> {
        accept_folder_transfer_impl(self, folder_id, target_dir).await
    }

    pub async fn reject_folder_transfer(&self, folder_id: String) -> Result<(), String> {
        reject_folder_transfer_impl(self, folder_id).await
    }

    pub async fn cancel_folder_transfer(&self, folder_id: String) -> Result<(), String> {
        cancel_folder_transfer_impl(self, folder_id).await
    }

    pub async fn get_transfers(&self) -> Result<Vec<TransferPayload>, String> {
        get_transfers_impl(self).await
    }

    pub fn get_identity(&self) -> Result<DeviceIdentity, String> {
        get_identity_impl(self)
    }

    pub fn update_display_name(&self, name: String) -> Result<(), String> {
        update_display_name_impl(self, name)
    }

    pub fn update_avatar(&self, path: String) -> Result<(), String> {
        update_avatar_impl(self, path)
    }

    pub fn get_settings(&self) -> Result<AppSettings, String> {
        get_settings_impl(self)
    }

    pub async fn update_settings(&self, settings: AppSettings) -> Result<AppSettings, String> {
        update_settings_impl(self, settings).await
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

#[derive(Default)]
struct FolderBridgeState {
    pending_offers: Mutex<HashMap<String, PendingFolderRoute>>,
    active_receives: Mutex<HashMap<String, ActiveFolderRoute>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingFolderRoute {
    sender_id: DeviceId,
    expected_files: VecDeque<FolderFileRoute>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActiveFolderRoute {
    sender_id: DeviceId,
    expected_files: VecDeque<FolderFileRoute>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FolderFileRoute {
    filename: String,
    size: u64,
    sha256: String,
}

impl FolderFileRoute {
    fn from_manifest_path(path: &str, size: u64, sha256: &str) -> Option<Self> {
        let filename = path
            .rsplit('/')
            .find(|segment| !segment.is_empty())?
            .to_string();
        Some(Self {
            filename,
            size,
            sha256: sha256.to_string(),
        })
    }

    fn from_message(message: &ProtocolMessage) -> Option<Self> {
        let ProtocolMessage::FileOffer {
            filename,
            size,
            sha256,
            ..
        } = message
        else {
            return None;
        };

        Some(Self {
            filename: filename.clone(),
            size: *size,
            sha256: sha256.clone(),
        })
    }

    fn matches_offer(&self, offer: &Self) -> bool {
        self.filename == offer.filename
            && self.size == offer.size
            && self.sha256.eq_ignore_ascii_case(&offer.sha256)
    }
}

fn folder_file_routes(manifest: &FolderManifestData) -> VecDeque<FolderFileRoute> {
    manifest
        .files
        .iter()
        .filter_map(|entry| {
            FolderFileRoute::from_manifest_path(&entry.relative_path, entry.size, &entry.sha256)
        })
        .collect()
}

impl FolderBridgeState {
    fn remember_pending_offer(
        &self,
        offer: &FolderOfferNotification,
        manifest: &FolderManifestData,
    ) {
        self.pending_offers
            .lock()
            .expect("lock pending folder offers")
            .insert(
                offer.folder_transfer_id.clone(),
                PendingFolderRoute {
                    sender_id: offer.sender_id.clone(),
                    expected_files: folder_file_routes(manifest),
                },
            );
    }

    fn take_pending_offer(&self, folder_id: &str) -> Option<PendingFolderRoute> {
        self.pending_offers
            .lock()
            .expect("lock pending folder offers")
            .remove(folder_id)
    }

    fn has_pending_offer(&self, folder_id: &str) -> bool {
        self.pending_offers
            .lock()
            .expect("lock pending folder offers")
            .contains_key(folder_id)
    }

    fn activate_receive(&self, folder_id: String, pending_offer: PendingFolderRoute) {
        self.active_receives
            .lock()
            .expect("lock active folder receives")
            .insert(
                folder_id,
                ActiveFolderRoute {
                    sender_id: pending_offer.sender_id,
                    expected_files: pending_offer.expected_files,
                },
            );
    }

    fn deactivate_receive(&self, folder_id: &str) {
        self.active_receives
            .lock()
            .expect("lock active folder receives")
            .remove(folder_id);
    }

    fn routes_file_offer(&self, sender_id: &DeviceId, message: &ProtocolMessage) -> bool {
        let Some(offer) = FolderFileRoute::from_message(message) else {
            return false;
        };

        let mut active_receives = self
            .active_receives
            .lock()
            .expect("lock active folder receives");
        let Some(active_receive) = active_receives
            .values_mut()
            .find(|active_receive| active_receive.sender_id == *sender_id)
        else {
            return false;
        };

        let Some(expected_offer) = active_receive.expected_files.front() else {
            return false;
        };
        if !expected_offer.matches_offer(&offer) {
            return false;
        }

        active_receive.expected_files.pop_front();
        true
    }
}

struct FolderEventReporter {
    emitter: Arc<dyn FrontendEmitter>,
}

impl FolderProgressReporter for FolderEventReporter {
    fn report(&self, progress: FolderProgress) {
        let payload = folder_progress_payload(&progress);
        let _ = emit_payload(self.emitter.as_ref(), EVENT_FOLDER_PROGRESS, &payload);
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
    async fn get_messages(
        &self,
        chat_id: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<Message>, String>;
    async fn create_group(
        &self,
        name: &str,
        members: &[String],
    ) -> Result<jasmine_core::GroupInfo, String>;
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
pub trait TransferServiceHandle: Send + Sync {
    async fn send_file(&self, peer_id: &str, file_path: &Path) -> Result<String, String>;
    async fn send_folder(&self, peer_id: &str, folder_path: &Path) -> Result<String, String>;
    async fn accept_file(&self, offer_id: &str) -> Result<String, String>;
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
}

#[async_trait]
pub trait SettingsServiceHandle: Send + Sync {
    fn get_settings(&self) -> Result<AppSettings, String>;
    async fn update_settings(&self, settings: AppSettings) -> Result<AppSettings, String>;
}

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

#[allow(non_snake_case)]
#[tauri::command]
pub async fn send_message(
    state: State<'_, Arc<AppState>>,
    peerId: String,
    content: String,
    replyToId: Option<String>,
) -> Result<String, String> {
    send_message_impl(state.inner().as_ref(), peerId, content, replyToId).await
}

#[tauri::command]
pub async fn get_messages(
    state: State<'_, Arc<AppState>>,
    chat_id: String,
    limit: u32,
    offset: u32,
) -> Result<Vec<ChatMessagePayload>, String> {
    get_messages_impl(state.inner().as_ref(), chat_id, limit, offset).await
}

#[tauri::command]
pub async fn create_group(
    state: State<'_, Arc<AppState>>,
    name: String,
    members: Vec<String>,
) -> Result<String, String> {
    create_group_impl(state.inner().as_ref(), name, members).await
}

#[allow(non_snake_case)]
#[tauri::command]
pub async fn send_group_message(
    state: State<'_, Arc<AppState>>,
    groupId: String,
    content: String,
    replyToId: Option<String>,
) -> Result<String, String> {
    send_group_message_impl(state.inner().as_ref(), groupId, content, replyToId).await
}

#[allow(non_snake_case)]
#[tauri::command]
pub async fn edit_message(
    state: State<'_, Arc<AppState>>,
    messageId: String,
    newContent: String,
) -> Result<(), String> {
    edit_message_impl(state.inner().as_ref(), messageId, newContent).await
}

#[allow(non_snake_case)]
#[tauri::command]
pub async fn delete_message(
    state: State<'_, Arc<AppState>>,
    messageId: String,
) -> Result<(), String> {
    delete_message_impl(state.inner().as_ref(), messageId).await
}

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

#[tauri::command]
pub fn get_identity(state: State<'_, Arc<AppState>>) -> Result<DeviceIdentity, String> {
    get_identity_impl(state.inner().as_ref())
}

#[tauri::command]
pub fn update_display_name(state: State<'_, Arc<AppState>>, name: String) -> Result<(), String> {
    update_display_name_impl(state.inner().as_ref(), name)
}

#[allow(non_snake_case)]
#[tauri::command]
pub fn update_avatar(state: State<'_, Arc<AppState>>, path: String) -> Result<(), String> {
    update_avatar_impl(state.inner().as_ref(), path)
}

#[tauri::command]
pub fn get_settings(state: State<'_, Arc<AppState>>) -> Result<AppSettings, String> {
    get_settings_impl(state.inner().as_ref())
}

#[tauri::command]
pub async fn update_settings(
    state: State<'_, Arc<AppState>>,
    settings: AppSettings,
) -> Result<AppSettings, String> {
    update_settings_impl(state.inner().as_ref(), settings).await
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

pub(crate) async fn send_message_impl(
    state: &AppState,
    peer_id: String,
    content: String,
    reply_to_id: Option<String>,
) -> Result<String, String> {
    let message = state
        .messaging
        .send_message(&peer_id, &content, reply_to_id.as_deref())
        .await?;
    Ok(message.id.to_string())
}

pub(crate) async fn get_messages_impl(
    state: &AppState,
    chat_id: String,
    limit: u32,
    offset: u32,
) -> Result<Vec<ChatMessagePayload>, String> {
    let local_device_id = state.local_device_id();
    let messages = state
        .messaging
        .get_messages(&chat_id, limit, offset)
        .await?;
    Ok(messages
        .into_iter()
        .map(|message| chat_message_payload(&local_device_id, &chat_id, message))
        .collect())
}

pub(crate) async fn create_group_impl(
    state: &AppState,
    name: String,
    members: Vec<String>,
) -> Result<String, String> {
    let group = state.messaging.create_group(&name, &members).await?;
    Ok(group.id.0.to_string())
}

pub(crate) async fn send_group_message_impl(
    state: &AppState,
    group_id: String,
    content: String,
    reply_to_id: Option<String>,
) -> Result<String, String> {
    let message = state
        .messaging
        .send_group_message(&group_id, &content, reply_to_id.as_deref())
        .await?;
    Ok(message.id.to_string())
}

pub(crate) async fn edit_message_impl(
    state: &AppState,
    message_id: String,
    new_content: String,
) -> Result<(), String> {
    state
        .messaging
        .edit_message(&message_id, &new_content, &state.local_device_id())
        .await
}

pub(crate) async fn delete_message_impl(
    state: &AppState,
    message_id: String,
) -> Result<(), String> {
    state
        .messaging
        .delete_message(&message_id, &state.local_device_id())
        .await
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

pub(crate) async fn update_settings_impl(
    state: &AppState,
    settings: AppSettings,
) -> Result<AppSettings, String> {
    state.settings_service.update_settings(settings).await
}

pub async fn setup_default_app_state(
    app_data_dir: impl AsRef<Path>,
    emitter: Arc<dyn FrontendEmitter>,
) -> Result<Arc<AppState>, String> {
    setup_default_app_state_with_config(app_data_dir, emitter, AppRuntimeConfig::default()).await
}

pub async fn setup_default_app_state_with_config(
    app_data_dir: impl AsRef<Path>,
    emitter: Arc<dyn FrontendEmitter>,
    runtime_config: AppRuntimeConfig,
) -> Result<Arc<AppState>, String> {
    let state = setup_app_state_with_factory_and_config(
        app_data_dir,
        emitter,
        Arc::new(DefaultAppSetupFactory),
        runtime_config,
    )
    .await?;
    Ok(Arc::new(state))
}

pub async fn setup_app_state_with_factory(
    app_data_dir: impl AsRef<Path>,
    emitter: Arc<dyn FrontendEmitter>,
    factory: Arc<dyn AppSetupFactory>,
) -> Result<AppState, String> {
    setup_app_state_with_factory_and_config(
        app_data_dir,
        emitter,
        factory,
        AppRuntimeConfig::default(),
    )
    .await
}

pub async fn setup_app_state_with_factory_and_config(
    app_data_dir: impl AsRef<Path>,
    emitter: Arc<dyn FrontendEmitter>,
    factory: Arc<dyn AppSetupFactory>,
    runtime_config: AppRuntimeConfig,
) -> Result<AppState, String> {
    let app_data_dir = app_data_dir.as_ref().to_path_buf();
    let context = SetupContext {
        database_path: app_data_dir.join("jasmine.db"),
        app_data_dir,
        runtime_config,
    };
    factory.build(context, emitter).await
}

struct DefaultAppSetupFactory;

#[async_trait]
impl AppSetupFactory for DefaultAppSetupFactory {
    async fn build(
        &self,
        context: SetupContext,
        emitter: Arc<dyn FrontendEmitter>,
    ) -> Result<AppState, String> {
        std::fs::create_dir_all(&context.app_data_dir).map_err(|error| error.to_string())?;

        let identity_store = IdentityStore::new(&context.app_data_dir);
        let identity = identity_store.load().map_err(|error| error.to_string())?;
        let settings_service = SettingsService::new(&context.app_data_dir);
        let storage = Arc::new(
            SqliteStorage::open(&context.database_path).map_err(|error| error.to_string())?,
        );

        let local_device_id = parse_uuid(&identity.device_id, "device_id")?;
        let local_peer = local_peer_identity(&identity);
        let mut server_config = WsServerConfig::new(local_peer.clone());
        server_config.bind_addr = context.runtime_config.ws_bind_addr.clone();
        let server = Arc::new(
            WsServer::bind(server_config)
                .await
                .map_err(|error| error.to_string())?,
        );
        let ws_port = server.local_addr().port();

        let mut discovery = DiscoveryManager::new(
            MdnsDiscoveryConfig {
                device_id: DeviceId(local_device_id),
                display_name: identity.display_name.clone(),
                ws_port,
            },
            UdpBroadcastConfig::new(
                DeviceId(local_device_id),
                identity.display_name.clone(),
                ws_port,
            ),
        )
        .map_err(|error| error.to_string())?;

        attach_discovery_callbacks(&mut discovery, Arc::clone(&storage), Arc::clone(&emitter));

        let discovery = Arc::new(discovery);
        let connector = Arc::new(PeerConnector::new(
            local_peer.clone(),
            Arc::clone(&discovery),
            Arc::clone(&server),
        ));
        let chat = ChatService::new(
            local_peer.clone(),
            Arc::clone(&server),
            Arc::clone(&storage),
        );
        spawn_chat_event_bridge(
            chat.clone(),
            Arc::clone(&storage),
            Arc::clone(&emitter),
            identity.device_id.clone(),
        );

        let transfer_signal = TransferSignalBridge::new(Arc::clone(&connector));
        let folder_bridge = Arc::new(FolderBridgeState::default());
        let file_sender = Arc::new(FileSender::new(transfer_signal.clone()));
        let file_receiver = Arc::new(FileReceiver::new(
            transfer_signal.clone(),
            settings_service.clone(),
        ));
        let transfer_manager = Arc::new(
            TransferManager::new(
                file_sender,
                file_receiver,
                settings_service.clone(),
                Arc::clone(&storage),
            )
            .map_err(|error| error.to_string())?,
        );
        let folder_coordinator = Arc::new(DefaultFolderCoordinator::new(
            transfer_signal.clone(),
            Arc::clone(&transfer_manager),
        ));
        let folder_receiver = Arc::new(DefaultFolderReceiver::new(
            transfer_signal.clone(),
            settings_service.clone(),
        ));

        connector.add_observer(Arc::new(ChatClientObserver { chat: chat.clone() }));
        connector.add_observer(Arc::new(TransferClientObserver {
            signal: transfer_signal.clone(),
            transfer_manager: Arc::downgrade(&transfer_manager),
            folder_receiver: Arc::clone(&folder_receiver),
            folder_bridge: Arc::clone(&folder_bridge),
            emitter: Arc::clone(&emitter),
        }));

        spawn_server_transfer_bridge(
            Arc::clone(&server),
            Arc::clone(&transfer_manager),
            Arc::clone(&folder_receiver),
            Arc::clone(&folder_bridge),
            transfer_signal.clone(),
            Arc::clone(&emitter),
        );

        let transfer_service: Arc<dyn TransferServiceHandle> = Arc::new(RealTransferService {
            manager: Arc::clone(&transfer_manager),
            storage: Arc::clone(&storage),
            folder_coordinator,
            folder_receiver,
            folder_bridge,
            folder_cancellations: Arc::new(Mutex::new(HashMap::new())),
            emitter: Arc::clone(&emitter),
            local_device_id: DeviceId(local_device_id),
        });
        spawn_transfer_progress_bridge(Arc::clone(&transfer_service), Arc::clone(&emitter));

        discovery.start().await.map_err(|error| error.to_string())?;

        Ok(AppState::new(
            identity.device_id.clone(),
            Arc::new(RealDiscoveryService {
                manager: Arc::clone(&discovery),
            }),
            Arc::new(RealMessagingService {
                chat,
                connector,
                server,
            }),
            transfer_service,
            Arc::new(RealIdentityService {
                store: identity_store,
            }),
            Arc::new(RealSettingsService {
                settings: settings_service,
                transfer_manager,
            }),
        ))
    }
}

fn attach_discovery_callbacks(
    discovery: &mut DiscoveryManager,
    storage: Arc<SqliteStorage>,
    emitter: Arc<dyn FrontendEmitter>,
) {
    let discovered_storage = Arc::clone(&storage);
    let discovered_emitter = Arc::clone(&emitter);
    discovery.on_peer_discovered(Arc::new(move |peer| {
        let storage = Arc::clone(&discovered_storage);
        let emitter = Arc::clone(&discovered_emitter);
        let payload = peer_payload(peer.clone());
        tauri::async_runtime::spawn(async move {
            let _ = storage.save_peer(&peer).await;
            let _ = emit_payload(emitter.as_ref(), EVENT_PEER_DISCOVERED, &payload);
        });
    }));

    discovery.on_peer_lost(Arc::new(move |device_id| {
        let payload = PeerLostPayload {
            id: device_id.0.to_string(),
        };
        let _ = emit_payload(emitter.as_ref(), EVENT_PEER_LOST, &payload);
    }));
}

fn spawn_chat_event_bridge(
    chat: ChatService<SqliteStorage>,
    storage: Arc<SqliteStorage>,
    emitter: Arc<dyn FrontendEmitter>,
    local_device_id: String,
) {
    tauri::async_runtime::spawn(async move {
        let mut events = chat.subscribe();
        loop {
            match events.recv().await {
                Ok(event) => {
                    emit_chat_service_event(
                        event,
                        storage.as_ref(),
                        emitter.as_ref(),
                        &local_device_id,
                    )
                    .await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

async fn emit_chat_service_event(
    event: ChatServiceEvent,
    storage: &SqliteStorage,
    emitter: &dyn FrontendEmitter,
    local_device_id: &str,
) {
    match event {
        ChatServiceEvent::MessageReceived { peer_id, message } => {
            let payload = chat_message_payload(local_device_id, &peer_id, message);
            let _ = emit_payload(emitter, EVENT_MESSAGE_RECEIVED, &payload);
        }
        ChatServiceEvent::MessageEdited {
            message_id,
            new_content,
            edit_version,
        } => {
            let edited_at = storage
                .get_message(&message_id.to_string())
                .await
                .ok()
                .flatten()
                .and_then(|message| message.edited_at)
                .unwrap_or_default();
            let payload = MessageEditedPayload {
                message_id: message_id.to_string(),
                new_content,
                edit_version,
                edited_at,
            };
            let _ = emit_payload(emitter, EVENT_MESSAGE_EDITED, &payload);
        }
        ChatServiceEvent::MessageDeleted { message_id } => {
            let payload = MessageDeletedPayload {
                message_id: message_id.to_string(),
            };
            let _ = emit_payload(emitter, EVENT_MESSAGE_DELETED, &payload);
        }
        ChatServiceEvent::MentionReceived {
            message_id,
            mentioned_user_id,
            sender_name,
        } => {
            let payload = MentionReceivedPayload {
                message_id: message_id.to_string(),
                mentioned_user_id,
                sender_name,
            };
            let _ = emit_payload(emitter, EVENT_MENTION_RECEIVED, &payload);
        }
        ChatServiceEvent::MessageStatusUpdated { .. } => {}
    }
}

fn spawn_server_transfer_bridge(
    server: Arc<WsServer>,
    transfer_manager: Arc<DefaultTransferManager>,
    folder_receiver: Arc<DefaultFolderReceiver>,
    folder_bridge: Arc<FolderBridgeState>,
    signal: TransferSignalBridge,
    emitter: Arc<dyn FrontendEmitter>,
) {
    tauri::async_runtime::spawn(async move {
        let mut events = server.subscribe();
        loop {
            match events.recv().await {
                Ok(WsServerEvent::MessageReceived { peer, message }) => {
                    let Ok(sender_id) = parse_device_id(&peer.identity.device_id) else {
                        continue;
                    };
                    let Ok(sender_address) = SocketAddr::from_str(&peer.address) else {
                        continue;
                    };
                    handle_transfer_protocol_message(
                        message,
                        TransferProtocolContext {
                            sender_id,
                            sender_name: peer.identity.display_name,
                            sender_address,
                            transfer_manager: Arc::downgrade(&transfer_manager),
                            folder_receiver: Arc::clone(&folder_receiver),
                            folder_bridge: Arc::clone(&folder_bridge),
                            signal: signal.clone(),
                            emitter: Arc::clone(&emitter),
                        },
                    )
                    .await;
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

fn spawn_transfer_progress_bridge(
    transfers: Arc<dyn TransferServiceHandle>,
    emitter: Arc<dyn FrontendEmitter>,
) {
    tauri::async_runtime::spawn(async move {
        let mut previous = HashMap::<String, TransferPayload>::new();
        let mut pending_thumbnail_failures = HashMap::<String, Instant>::new();
        let mut emitted_thumbnail_ready = HashSet::<String>::new();
        let mut emitted_thumbnail_failed = HashSet::<String>::new();
        let mut interval = time::interval(TRANSFER_POLL_INTERVAL);

        loop {
            interval.tick().await;
            let Ok(current) = transfers.get_transfers().await else {
                continue;
            };

            let mut current_by_id = HashMap::new();
            let mut current_ids = HashSet::new();
            for transfer in current {
                let previous_transfer = previous.get(&transfer.id);
                if let Some(last) = previous.get(&transfer.id) {
                    if last.state != transfer.state {
                        let _ = emit_payload(
                            emitter.as_ref(),
                            EVENT_TRANSFER_STATE_CHANGED,
                            &TransferStatePayload {
                                id: transfer.id.clone(),
                                state: transfer.state.clone(),
                            },
                        );
                    }

                    if (last.progress - transfer.progress).abs() > f64::EPSILON
                        || last.speed != transfer.speed
                    {
                        let _ = emit_payload(
                            emitter.as_ref(),
                            EVENT_TRANSFER_PROGRESS,
                            &TransferProgressPayload {
                                id: transfer.id.clone(),
                                progress: transfer.progress,
                                speed: transfer.speed,
                            },
                        );
                    }
                } else {
                    let _ = emit_payload(
                        emitter.as_ref(),
                        EVENT_TRANSFER_STATE_CHANGED,
                        &TransferStatePayload {
                            id: transfer.id.clone(),
                            state: transfer.state.clone(),
                        },
                    );
                    let _ = emit_payload(
                        emitter.as_ref(),
                        EVENT_TRANSFER_PROGRESS,
                        &TransferProgressPayload {
                            id: transfer.id.clone(),
                            progress: transfer.progress,
                            speed: transfer.speed,
                        },
                    );
                }

                bridge_thumbnail_event(
                    emitter.as_ref(),
                    previous_transfer,
                    &transfer,
                    &mut pending_thumbnail_failures,
                    &mut emitted_thumbnail_ready,
                    &mut emitted_thumbnail_failed,
                );

                current_ids.insert(transfer.id.clone());
                current_by_id.insert(transfer.id.clone(), transfer);
            }

            pending_thumbnail_failures.retain(|transfer_id, _| current_ids.contains(transfer_id));
            emitted_thumbnail_ready.retain(|transfer_id| current_ids.contains(transfer_id));
            emitted_thumbnail_failed.retain(|transfer_id| current_ids.contains(transfer_id));
            previous = current_by_id;
        }
    });
}

struct TransferProtocolContext {
    sender_id: DeviceId,
    sender_name: String,
    sender_address: SocketAddr,
    transfer_manager: Weak<DefaultTransferManager>,
    folder_receiver: Arc<DefaultFolderReceiver>,
    folder_bridge: Arc<FolderBridgeState>,
    signal: TransferSignalBridge,
    emitter: Arc<dyn FrontendEmitter>,
}

async fn handle_transfer_protocol_message(
    message: ProtocolMessage,
    context: TransferProtocolContext,
) {
    match message {
        folder_manifest @ ProtocolMessage::FolderManifest { .. } => {
            let ProtocolMessage::FolderManifest { ref manifest, .. } = folder_manifest else {
                unreachable!();
            };
            let manifest = manifest.clone();
            if let Ok(Some(notification)) = context
                .folder_receiver
                .handle_signal_message(
                    context.sender_id.clone(),
                    context.sender_address,
                    folder_manifest,
                )
                .await
            {
                context
                    .folder_bridge
                    .remember_pending_offer(&notification, &manifest);
                let payload = FolderOfferPayload {
                    folder_transfer_id: notification.folder_transfer_id,
                    folder_name: notification.folder_name,
                    file_count: notification.file_count,
                    total_size: notification.total_size,
                    sender_name: context.sender_name,
                };
                let _ = emit_payload(
                    context.emitter.as_ref(),
                    EVENT_FOLDER_OFFER_RECEIVED,
                    &payload,
                );
            }
        }
        file_offer @ ProtocolMessage::FileOffer { .. } => {
            if context
                .folder_bridge
                .routes_file_offer(&context.sender_id, &file_offer)
            {
                let _ = context
                    .folder_receiver
                    .handle_signal_message(context.sender_id, context.sender_address, file_offer)
                    .await;
            } else {
                let Some(transfer_manager) = context.transfer_manager.upgrade() else {
                    return;
                };

                if let Ok(Some(notification)) = transfer_manager
                    .handle_signal_message(context.sender_id, context.sender_address, file_offer)
                    .await
                {
                    let payload = FileOfferPayload {
                        id: notification.offer_id,
                        filename: notification.filename,
                        size: notification.size,
                        sender_id: notification.sender_id.0.to_string(),
                    };
                    let _ = emit_payload(
                        context.emitter.as_ref(),
                        EVENT_FILE_OFFER_RECEIVED,
                        &payload,
                    );
                }
            }
        }
        ProtocolMessage::FileAccept { .. }
        | ProtocolMessage::FileReject { .. }
        | ProtocolMessage::FolderAccept { .. }
        | ProtocolMessage::FolderReject { .. } => {
            let _ = context.signal.route_response(message);
        }
        _ => {}
    }
}

#[derive(Clone)]
struct TransferSignalBridge {
    connector: Arc<PeerConnector>,
    response_senders: Arc<Mutex<HashMap<String, oneshot::Sender<ProtocolMessage>>>>,
    response_receivers: Arc<Mutex<HashMap<String, oneshot::Receiver<ProtocolMessage>>>>,
}

impl TransferSignalBridge {
    fn new(connector: Arc<PeerConnector>) -> Self {
        Self {
            connector,
            response_senders: Arc::new(Mutex::new(HashMap::new())),
            response_receivers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn route_response(&self, message: ProtocolMessage) -> bool {
        let offer_id = match &message {
            ProtocolMessage::FileAccept { offer_id } => offer_id.clone(),
            ProtocolMessage::FileReject { offer_id, .. } => offer_id.clone(),
            ProtocolMessage::FolderAccept { folder_transfer_id } => folder_transfer_id.clone(),
            ProtocolMessage::FolderReject {
                folder_transfer_id, ..
            } => folder_transfer_id.clone(),
            _ => return false,
        };

        let Some(sender) = self
            .response_senders
            .lock()
            .expect("lock response senders")
            .remove(&offer_id)
        else {
            return false;
        };

        let _ = sender.send(message);
        true
    }
}

impl FileSenderSignal for TransferSignalBridge {
    async fn send_message(
        &self,
        peer_id: &DeviceId,
        message: ProtocolMessage,
    ) -> Result<(), FileSenderError> {
        let offer_id = match &message {
            ProtocolMessage::FileOffer { id, .. } => Some(id.clone()),
            ProtocolMessage::FolderManifest {
                folder_transfer_id, ..
            } => Some(folder_transfer_id.clone()),
            _ => None,
        };

        if let Some(offer_id) = offer_id {
            let (tx, rx) = oneshot::channel();
            self.response_senders
                .lock()
                .expect("lock response senders")
                .insert(offer_id.clone(), tx);
            self.response_receivers
                .lock()
                .expect("lock response receivers")
                .insert(offer_id.clone(), rx);

            if let Err(error) = self
                .connector
                .send_protocol(&peer_id.0.to_string(), message)
                .await
            {
                self.response_senders
                    .lock()
                    .expect("lock response senders")
                    .remove(&offer_id);
                self.response_receivers
                    .lock()
                    .expect("lock response receivers")
                    .remove(&offer_id);
                return Err(FileSenderError::Signal(error));
            }

            return Ok(());
        }

        self.connector
            .send_protocol(&peer_id.0.to_string(), message)
            .await
            .map_err(FileSenderError::Signal)
    }

    async fn wait_for_response(&self, offer_id: &str) -> Result<ProtocolMessage, FileSenderError> {
        let receiver = self
            .response_receivers
            .lock()
            .expect("lock response receivers")
            .remove(offer_id)
            .ok_or(FileSenderError::SignalClosed)?;
        receiver.await.map_err(|_| FileSenderError::SignalClosed)
    }
}

impl FileReceiverSignal for TransferSignalBridge {
    async fn send_message(
        &self,
        peer_id: &DeviceId,
        message: ProtocolMessage,
    ) -> Result<(), FileReceiverError> {
        self.connector
            .send_protocol(&peer_id.0.to_string(), message)
            .await
            .map_err(FileReceiverError::Signal)
    }
}

#[async_trait::async_trait]
trait ClientObserver: Send + Sync {
    async fn on_client(&self, client: Arc<WsClient>) -> Result<(), String>;
}

struct PeerConnector {
    local_peer: WsPeerIdentity,
    discovery: Arc<DiscoveryManager>,
    server: Arc<WsServer>,
    clients: Arc<Mutex<HashMap<String, Arc<WsClient>>>>,
    observers: Mutex<Vec<Arc<dyn ClientObserver>>>,
}

impl PeerConnector {
    fn new(
        local_peer: WsPeerIdentity,
        discovery: Arc<DiscoveryManager>,
        server: Arc<WsServer>,
    ) -> Self {
        Self {
            local_peer,
            discovery,
            server,
            clients: Arc::new(Mutex::new(HashMap::new())),
            observers: Mutex::new(Vec::new()),
        }
    }

    fn add_observer(&self, observer: Arc<dyn ClientObserver>) {
        self.observers
            .lock()
            .expect("lock client observers")
            .push(observer);
    }

    async fn send_protocol(&self, peer_id: &str, message: ProtocolMessage) -> Result<(), String> {
        if self
            .server
            .connected_peers()
            .into_iter()
            .any(|peer| peer.identity.device_id == peer_id)
        {
            return self
                .server
                .send_to(peer_id, message)
                .await
                .map_err(|error| error.to_string());
        }

        self.ensure_connected(peer_id).await?;
        let client = self
            .clients
            .lock()
            .expect("lock peer clients")
            .get(peer_id)
            .cloned()
            .ok_or_else(|| format!("peer {peer_id} is not connected"))?;

        if client.send(message.clone()).await.is_err() {
            self.clients
                .lock()
                .expect("lock peer clients")
                .remove(peer_id);
            self.ensure_connected(peer_id).await?;
            let client = self
                .clients
                .lock()
                .expect("lock peer clients")
                .get(peer_id)
                .cloned()
                .ok_or_else(|| format!("peer {peer_id} is not connected"))?;
            client
                .send(message)
                .await
                .map_err(|retry| retry.to_string())
        } else {
            Ok(())
        }
    }

    async fn ensure_connected(&self, peer_id: &str) -> Result<(), String> {
        if self
            .server
            .connected_peers()
            .into_iter()
            .any(|peer| peer.identity.device_id == peer_id)
        {
            return Ok(());
        }

        if self
            .clients
            .lock()
            .expect("lock peer clients")
            .contains_key(peer_id)
        {
            return Ok(());
        }

        let peer = self
            .discovery
            .peers()
            .into_iter()
            .find(|peer| peer.device_id.0.to_string() == peer_id)
            .ok_or_else(|| format!("peer {peer_id} is not available"))?;
        let url = peer_ws_url(&peer)?;
        let client = Arc::new(
            WsClient::connect(url, WsClientConfig::new(self.local_peer.clone()))
                .await
                .map_err(|error| error.to_string())?,
        );

        let observers = self
            .observers
            .lock()
            .expect("lock client observers")
            .clone();
        for observer in observers {
            observer.on_client(Arc::clone(&client)).await?;
        }

        self.register_disconnect_cleanup(peer_id.to_string(), Arc::clone(&client));
        self.clients
            .lock()
            .expect("lock peer clients")
            .insert(peer_id.to_string(), client);
        Ok(())
    }

    fn register_disconnect_cleanup(&self, peer_id: String, client: Arc<WsClient>) {
        let clients = Arc::clone(&self.clients);
        tauri::async_runtime::spawn(async move {
            let mut events = client.subscribe();
            loop {
                match events.recv().await {
                    Ok(WsClientEvent::Disconnected { .. }) => {
                        clients.lock().expect("lock peer clients").remove(&peer_id);
                        break;
                    }
                    Ok(WsClientEvent::MessageReceived { .. }) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
}

struct ChatClientObserver {
    chat: ChatService<SqliteStorage>,
}

#[async_trait::async_trait]
impl ClientObserver for ChatClientObserver {
    async fn on_client(&self, client: Arc<WsClient>) -> Result<(), String> {
        self.chat
            .register_client(client)
            .await
            .map_err(|error| error.to_string())
    }
}

struct TransferClientObserver {
    signal: TransferSignalBridge,
    transfer_manager: Weak<DefaultTransferManager>,
    folder_receiver: Arc<DefaultFolderReceiver>,
    folder_bridge: Arc<FolderBridgeState>,
    emitter: Arc<dyn FrontendEmitter>,
}

#[async_trait::async_trait]
impl ClientObserver for TransferClientObserver {
    async fn on_client(&self, client: Arc<WsClient>) -> Result<(), String> {
        let remote_peer = client
            .remote_peer()
            .ok_or_else(|| "client remote peer is unavailable".to_string())?;
        let sender_id = parse_device_id(&remote_peer.identity.device_id)?;
        let sender_name = remote_peer.identity.display_name.clone();
        let sender_address =
            SocketAddr::from_str(&remote_peer.address).map_err(|error| error.to_string())?;
        let signal = self.signal.clone();
        let transfer_manager = self.transfer_manager.clone();
        let folder_receiver = Arc::clone(&self.folder_receiver);
        let folder_bridge = Arc::clone(&self.folder_bridge);
        let emitter = Arc::clone(&self.emitter);

        tauri::async_runtime::spawn(async move {
            let mut events = client.subscribe();
            loop {
                match events.recv().await {
                    Ok(WsClientEvent::MessageReceived { message }) => {
                        handle_transfer_protocol_message(
                            message,
                            TransferProtocolContext {
                                sender_id: sender_id.clone(),
                                sender_name: sender_name.clone(),
                                sender_address,
                                transfer_manager: transfer_manager.clone(),
                                folder_receiver: Arc::clone(&folder_receiver),
                                folder_bridge: Arc::clone(&folder_bridge),
                                signal: signal.clone(),
                                emitter: Arc::clone(&emitter),
                            },
                        )
                        .await;
                    }
                    Ok(WsClientEvent::Disconnected { .. }) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        Ok(())
    }
}

struct RealDiscoveryService {
    manager: Arc<DiscoveryManager>,
}

#[async_trait]
impl DiscoveryServiceHandle for RealDiscoveryService {
    async fn start(&self) -> Result<(), String> {
        self.manager
            .start()
            .await
            .map_err(|error| error.to_string())
    }

    async fn stop(&self) -> Result<(), String> {
        self.manager.stop().await.map_err(|error| error.to_string())
    }

    fn peers(&self) -> Vec<PeerInfo> {
        self.manager.peers()
    }
}

struct RealMessagingService {
    chat: ChatService<SqliteStorage>,
    connector: Arc<PeerConnector>,
    server: Arc<WsServer>,
}

#[async_trait]
impl MessagingServiceHandle for RealMessagingService {
    async fn send_message(
        &self,
        peer_id: &str,
        content: &str,
        reply_to_id: Option<&str>,
    ) -> Result<Message, String> {
        let _ = self.connector.ensure_connected(peer_id).await;
        self.chat
            .send_message_with_reply(
                peer_id,
                content.to_string(),
                reply_to_id.map(str::to_string),
            )
            .await
            .map_err(|error| error.to_string())
    }

    async fn get_messages(
        &self,
        chat_id: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<Message>, String> {
        let chat_id = ChatId(parse_uuid(chat_id, "chat_id")?);
        self.chat
            .load_messages(&chat_id, limit as usize, offset as usize)
            .await
            .map_err(|error| error.to_string())
    }

    async fn create_group(
        &self,
        name: &str,
        members: &[String],
    ) -> Result<jasmine_core::GroupInfo, String> {
        for member in members {
            let _ = self.connector.ensure_connected(member).await;
        }

        self.chat
            .create_group(name.to_string(), members.to_vec())
            .await
            .map_err(|error| error.to_string())
    }

    async fn send_group_message(
        &self,
        group_id: &str,
        content: &str,
        reply_to_id: Option<&str>,
    ) -> Result<Message, String> {
        let group_id = ChatId(parse_uuid(group_id, "group_id")?);
        self.chat
            .send_to_group_with_reply(
                &group_id,
                content.to_string(),
                reply_to_id.map(str::to_string),
            )
            .await
            .map_err(|error| error.to_string())
    }

    async fn edit_message(
        &self,
        message_id: &str,
        new_content: &str,
        sender_id: &str,
    ) -> Result<(), String> {
        self.chat
            .edit_message(message_id, new_content, sender_id)
            .await
            .map_err(|error| error.to_string())
    }

    async fn delete_message(&self, message_id: &str, sender_id: &str) -> Result<(), String> {
        self.chat
            .delete_message(message_id, sender_id)
            .await
            .map_err(|error| error.to_string())
    }

    async fn shutdown(&self) -> Result<(), String> {
        shutdown_messaging_blocking(self.chat.clone(), Arc::clone(&self.server))
    }
}

struct RealTransferService {
    manager: Arc<DefaultTransferManager>,
    storage: Arc<SqliteStorage>,
    folder_coordinator: Arc<DefaultFolderCoordinator>,
    folder_receiver: Arc<DefaultFolderReceiver>,
    folder_bridge: Arc<FolderBridgeState>,
    folder_cancellations: Arc<Mutex<HashMap<String, CancellationToken>>>,
    emitter: Arc<dyn FrontendEmitter>,
    local_device_id: DeviceId,
}

#[async_trait]
impl TransferServiceHandle for RealTransferService {
    async fn send_file(&self, peer_id: &str, file_path: &Path) -> Result<String, String> {
        self.manager
            .send_file(
                DeviceId(parse_uuid(peer_id, "peer_id")?),
                file_path.to_path_buf(),
                None,
            )
            .await
            .map_err(|error| error.to_string())
    }

    async fn send_folder(&self, _peer_id: &str, _folder_path: &Path) -> Result<String, String> {
        let peer_id = DeviceId(parse_uuid(_peer_id, "peer_id")?);
        let folder_transfer_id = Uuid::new_v4().to_string();
        let cancellation = CancellationToken::new();
        self.folder_cancellations
            .lock()
            .expect("lock folder transfer cancellations")
            .insert(folder_transfer_id.clone(), cancellation.clone());

        let coordinator = Arc::clone(&self.folder_coordinator);
        let emitter = Arc::clone(&self.emitter);
        let cancellations = Arc::clone(&self.folder_cancellations);
        let sender_id = self.local_device_id.clone();
        let folder_path = _folder_path.to_path_buf();
        let task_folder_id = folder_transfer_id.clone();

        tauri::async_runtime::spawn(async move {
            let reporter: Arc<dyn FolderProgressReporter> = Arc::new(FolderEventReporter {
                emitter: Arc::clone(&emitter),
            });
            let result = coordinator
                .send_folder_with_id(
                    sender_id,
                    peer_id,
                    folder_path,
                    task_folder_id.clone(),
                    cancellation,
                    Some(reporter),
                )
                .await;
            cancellations
                .lock()
                .expect("lock folder transfer cancellations")
                .remove(&task_folder_id);
            emit_folder_completion_result(emitter.as_ref(), &task_folder_id, result);
        });

        Ok(folder_transfer_id)
    }

    async fn accept_file(&self, offer_id: &str) -> Result<String, String> {
        self.manager
            .accept_offer(offer_id, None)
            .await
            .map_err(|error| error.to_string())
    }

    async fn reject_file(&self, offer_id: &str, reason: Option<String>) -> Result<(), String> {
        self.manager
            .reject_offer(offer_id, reason)
            .await
            .map_err(|error| error.to_string())
    }

    async fn cancel_transfer(&self, transfer_id: &str) -> Result<(), String> {
        self.manager
            .cancel_transfer(transfer_id)
            .await
            .map_err(|error| error.to_string())
    }

    async fn accept_folder_transfer(
        &self,
        _folder_id: &str,
        _target_dir: &Path,
    ) -> Result<(), String> {
        let pending_offer = self
            .folder_bridge
            .take_pending_offer(_folder_id)
            .ok_or_else(|| format!("unknown folder transfer {}", _folder_id))?;
        self.folder_bridge
            .activate_receive(_folder_id.to_string(), pending_offer);

        let cancellation = CancellationToken::new();
        self.folder_cancellations
            .lock()
            .expect("lock folder transfer cancellations")
            .insert(_folder_id.to_string(), cancellation.clone());

        let receiver = Arc::clone(&self.folder_receiver);
        let folder_bridge = Arc::clone(&self.folder_bridge);
        let cancellations = Arc::clone(&self.folder_cancellations);
        let emitter = Arc::clone(&self.emitter);
        let folder_id = _folder_id.to_string();
        let target_dir = _target_dir.to_path_buf();

        tauri::async_runtime::spawn(async move {
            let reporter: Arc<dyn FolderProgressReporter> = Arc::new(FolderEventReporter {
                emitter: Arc::clone(&emitter),
            });
            let result = receiver
                .accept_offer_with_cancellation(
                    &folder_id,
                    target_dir,
                    cancellation,
                    Some(reporter),
                )
                .await;
            folder_bridge.deactivate_receive(&folder_id);
            cancellations
                .lock()
                .expect("lock folder transfer cancellations")
                .remove(&folder_id);
            emit_folder_completion_result(emitter.as_ref(), &folder_id, result);
        });

        Ok(())
    }

    async fn reject_folder_transfer(&self, _folder_id: &str) -> Result<(), String> {
        let _ = self.folder_bridge.take_pending_offer(_folder_id);
        self.folder_receiver
            .reject_offer(_folder_id, None)
            .await
            .map_err(|error| error.to_string())
    }

    async fn cancel_folder_transfer(&self, _folder_id: &str) -> Result<(), String> {
        if let Some(cancellation) = self
            .folder_cancellations
            .lock()
            .expect("lock folder transfer cancellations")
            .get(_folder_id)
            .cloned()
        {
            cancellation.cancel();
            return Ok(());
        }

        if self.folder_bridge.has_pending_offer(_folder_id) {
            let _ = self.folder_bridge.take_pending_offer(_folder_id);
        }
        self.folder_receiver
            .cancel_transfer(_folder_id)
            .await
            .map_err(|error| error.to_string())
    }

    async fn get_transfers(&self) -> Result<Vec<TransferPayload>, String> {
        let snapshot = self.manager.snapshot();
        let mut transfers = HashMap::<String, TransferPayload>::new();

        for transfer in snapshot
            .active_transfers
            .iter()
            .chain(snapshot.queued_transfers.iter())
        {
            transfers.insert(transfer.id.clone(), transfer_payload_from_managed(transfer));
        }

        let persisted = self
            .storage
            .get_transfers(TRANSFER_HISTORY_LIMIT, 0)
            .await
            .map_err(|error| error.to_string())?;

        for record in persisted {
            transfers
                .entry(record.id.to_string())
                .or_insert_with(|| transfer_payload_from_record(&record));
        }

        let mut values = transfers.into_values().collect::<Vec<_>>();
        values.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(values)
    }
}

struct RealIdentityService {
    store: IdentityStore,
}

impl IdentityServiceHandle for RealIdentityService {
    fn get_identity(&self) -> Result<DeviceIdentity, String> {
        self.store.load().map_err(|error| error.to_string())
    }

    fn update_display_name(&self, name: &str) -> Result<DeviceIdentity, String> {
        self.store
            .update_name(name)
            .map_err(|error| error.to_string())
    }

    fn update_avatar(&self, path: &str) -> Result<DeviceIdentity, String> {
        self.store
            .update_avatar(path)
            .map_err(|error| error.to_string())
    }
}

struct RealSettingsService {
    settings: SettingsService,
    transfer_manager: Arc<DefaultTransferManager>,
}

#[async_trait]
impl SettingsServiceHandle for RealSettingsService {
    fn get_settings(&self) -> Result<AppSettings, String> {
        self.settings.load().map_err(|error| error.to_string())
    }

    async fn update_settings(&self, settings: AppSettings) -> Result<AppSettings, String> {
        self.settings
            .save(&settings)
            .map_err(|error| error.to_string())?;
        self.transfer_manager
            .on_settings_changed()
            .await
            .map_err(|error| error.to_string())?;
        self.settings.load().map_err(|error| error.to_string())
    }
}

fn peer_payload(peer: PeerInfo) -> PeerPayload {
    PeerPayload {
        id: peer.device_id.0.to_string(),
        name: peer.display_name,
        status: "online".to_string(),
    }
}

fn chat_message_payload(
    local_device_id: &str,
    peer_id: &str,
    message: Message,
) -> ChatMessagePayload {
    let Message {
        id,
        chat_id,
        sender_id,
        content,
        timestamp_ms,
        status,
        edit_version,
        edited_at,
        is_deleted,
        deleted_at,
        reply_to_id,
        reply_to_preview,
    } = message;
    let sender_uuid = sender_id.0.to_string();
    let chat_id = chat_id.0.to_string();
    let sender_is_local = sender_uuid == local_device_id;

    ChatMessagePayload {
        id: id.to_string(),
        sender_id: if sender_is_local {
            "local".to_string()
        } else {
            sender_uuid.clone()
        },
        receiver_id: if sender_is_local {
            chat_id.clone()
        } else if chat_id == peer_id || chat_id == sender_uuid {
            "local".to_string()
        } else {
            chat_id.clone()
        },
        content,
        timestamp: timestamp_ms,
        status: message_status_label(&status).to_string(),
        edit_version,
        edited_at,
        is_deleted,
        deleted_at,
        reply_to_id,
        reply_to_preview,
    }
}

fn transfer_payload_from_managed(transfer: &jasmine_transfer::ManagedTransfer) -> TransferPayload {
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
        sender_id: Some(transfer.peer_id.0.to_string()),
        local_path: Some(transfer.local_path.to_string_lossy().into_owned()),
        thumbnail_path: None,
        direction: Some(transfer_direction_label(transfer.direction).to_string()),
        folder_id: transfer.folder_id.clone(),
        folder_relative_path: transfer.folder_relative_path.clone(),
    }
}

fn transfer_payload_from_record(record: &TransferRecord) -> TransferPayload {
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
        sender_id: Some(record.peer_id.0.to_string()),
        local_path: Some(record.local_path.to_string_lossy().into_owned()),
        thumbnail_path: record.thumbnail_path.clone(),
        direction: None,
        folder_id: record.folder_id.clone(),
        folder_relative_path: record.folder_relative_path.clone(),
    }
}

fn transfer_direction_label(direction: TransferDirection) -> &'static str {
    match direction {
        TransferDirection::Send => "send",
        TransferDirection::Receive => "receive",
    }
}

fn folder_progress_payload(progress: &FolderProgress) -> FolderProgressPayload {
    FolderProgressPayload {
        folder_transfer_id: progress.folder_transfer_id.clone(),
        sent_bytes: progress.sent_bytes,
        total_bytes: progress.total_bytes,
        completed_files: progress.completed_files,
        total_files: progress.total_files,
    }
}

fn emit_folder_completion_result<E>(
    emitter: &dyn FrontendEmitter,
    folder_transfer_id: &str,
    result: Result<FolderTransferOutcome, E>,
) where
    E: ToString,
{
    let payload = match result {
        Ok(outcome) => {
            let failed_files = outcome
                .file_results
                .iter()
                .filter(|file| file.status == TransferStatus::Failed)
                .count();
            FolderCompletedPayload {
                folder_transfer_id: outcome.folder_transfer_id,
                status: folder_transfer_status_label(outcome.progress.status).to_string(),
                failed_files: (failed_files > 0).then_some(failed_files),
            }
        }
        Err(_) => FolderCompletedPayload {
            folder_transfer_id: folder_transfer_id.to_string(),
            status: "failed".to_string(),
            failed_files: None,
        },
    };

    let _ = emit_payload(emitter, EVENT_FOLDER_COMPLETED, &payload);
}

fn folder_transfer_status_label(status: FolderTransferStatus) -> &'static str {
    match status {
        FolderTransferStatus::Pending => "pending",
        FolderTransferStatus::Sending => "sending",
        FolderTransferStatus::Completed => "completed",
        FolderTransferStatus::PartiallyFailed => "partially-failed",
        FolderTransferStatus::Cancelled => "cancelled",
        FolderTransferStatus::Rejected => "rejected",
    }
}

fn bridge_thumbnail_event(
    emitter: &dyn FrontendEmitter,
    previous: Option<&TransferPayload>,
    transfer: &TransferPayload,
    pending_thumbnail_failures: &mut HashMap<String, Instant>,
    emitted_thumbnail_ready: &mut HashSet<String>,
    emitted_thumbnail_failed: &mut HashSet<String>,
) {
    let is_candidate = pending_thumbnail_failures.contains_key(&transfer.id)
        || emitted_thumbnail_ready.contains(&transfer.id)
        || emitted_thumbnail_failed.contains(&transfer.id)
        || is_received_image_transfer(previous, transfer);

    if !is_candidate {
        return;
    }

    if let Some(thumbnail_path) = transfer.thumbnail_path.as_ref() {
        if !emitted_thumbnail_ready.contains(&transfer.id)
            || previous.and_then(|value| value.thumbnail_path.as_ref()) != Some(thumbnail_path)
        {
            let payload = ThumbnailReadyPayload {
                transfer_id: transfer.id.clone(),
                thumbnail_path: thumbnail_path.clone(),
            };
            let _ = emit_payload(emitter, EVENT_THUMBNAIL_READY, &payload);
        }

        emitted_thumbnail_ready.insert(transfer.id.clone());
        emitted_thumbnail_failed.remove(&transfer.id);
        pending_thumbnail_failures.remove(&transfer.id);
        return;
    }

    if transfer.state != "completed" {
        pending_thumbnail_failures.remove(&transfer.id);
        emitted_thumbnail_failed.remove(&transfer.id);
        return;
    }

    let started_at = pending_thumbnail_failures
        .entry(transfer.id.clone())
        .or_insert_with(Instant::now);
    if started_at.elapsed() >= THUMBNAIL_WATCH_TIMEOUT
        && !emitted_thumbnail_failed.contains(&transfer.id)
    {
        let payload = ThumbnailFailedPayload {
            transfer_id: transfer.id.clone(),
        };
        let _ = emit_payload(emitter, EVENT_THUMBNAIL_FAILED, &payload);
        emitted_thumbnail_failed.insert(transfer.id.clone());
    }
}

fn is_received_image_transfer(
    previous: Option<&TransferPayload>,
    transfer: &TransferPayload,
) -> bool {
    let direction = transfer
        .direction
        .as_deref()
        .or(previous.and_then(|value| value.direction.as_deref()));
    if direction != Some("receive") {
        return false;
    }

    transfer
        .local_path
        .as_deref()
        .or(previous.and_then(|value| value.local_path.as_deref()))
        .is_some_and(is_supported_image_path)
}

fn is_supported_image_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "jpg" | "jpeg" | "png" | "gif" | "webp"
            )
        })
}

fn progress_ratio(status: TransferStatus, bytes_sent: u64, total_bytes: u64) -> f64 {
    match status {
        TransferStatus::Completed => 1.0,
        _ if total_bytes == 0 => 0.0,
        _ => bytes_sent as f64 / total_bytes as f64,
    }
}

fn message_status_label(status: &MessageStatus) -> &'static str {
    match status {
        MessageStatus::Sent => "sent",
        MessageStatus::Delivered => "delivered",
        MessageStatus::Failed => "failed",
    }
}

fn transfer_status_label(status: &TransferStatus) -> &'static str {
    match status {
        TransferStatus::Pending => "queued",
        TransferStatus::Active => "active",
        TransferStatus::Completed => "completed",
        TransferStatus::Failed | TransferStatus::Cancelled => "failed",
    }
}

fn local_peer_identity(identity: &DeviceIdentity) -> WsPeerIdentity {
    WsPeerIdentity::new(identity.device_id.clone(), identity.display_name.clone())
}

fn peer_ws_url(peer: &PeerInfo) -> Result<String, String> {
    let address = peer
        .addresses
        .first()
        .cloned()
        .ok_or_else(|| format!("peer {} does not advertise an address", peer.device_id.0))?;
    let port = peer.ws_port.ok_or_else(|| {
        format!(
            "peer {} does not advertise a websocket port",
            peer.device_id.0
        )
    })?;
    Ok(format!("ws://{address}:{port}"))
}

fn parse_uuid(raw: &str, field: &str) -> Result<Uuid, String> {
    Uuid::parse_str(raw).map_err(|_| format!("{field} must be a valid UUID"))
}

fn parse_device_id(raw: &str) -> Result<DeviceId, String> {
    Ok(DeviceId(parse_uuid(raw, "device_id")?))
}

fn emit_payload<S: Serialize>(
    emitter: &dyn FrontendEmitter,
    event: &str,
    payload: &S,
) -> Result<(), String> {
    let value = serde_json::to_value(payload).map_err(|error| error.to_string())?;
    emitter.emit_json(event, value)
}

fn shutdown_messaging_blocking(
    chat: ChatService<SqliteStorage>,
    server: Arc<WsServer>,
) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build messaging shutdown runtime");
        let shutdown_outcome = runtime.block_on(async move {
            chat.shutdown().await.map_err(|error| error.to_string())?;
            server.shutdown().await.map_err(|error| error.to_string())
        });
        let _ = tx.send(shutdown_outcome);
    });

    rx.recv()
        .map_err(|_| "messaging shutdown thread terminated".to_string())?
}

#[cfg(test)]
mod tests;
