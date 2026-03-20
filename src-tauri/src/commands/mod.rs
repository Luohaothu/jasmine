use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use jasmine_core::{
    AppSettings, ChatId, DeviceId, DeviceIdentity, DiscoveryService, IdentityStore, Message,
    MessageStatus, PeerInfo, ProtocolMessage, SettingsService, StorageEngine, TransferRecord,
    TransferStatus,
};
use jasmine_discovery::{DiscoveryManager, MdnsDiscoveryConfig, UdpBroadcastConfig};
use jasmine_messaging::{
    ChatService, ChatServiceEvent, WsClient, WsClientConfig, WsClientEvent, WsPeerIdentity,
    WsServer, WsServerConfig, WsServerEvent,
};
use jasmine_storage::SqliteStorage;
use jasmine_transfer::{
    FileReceiver, FileReceiverError, FileReceiverSignal, FileSender, FileSenderError,
    FileSenderSignal, TransferManager,
};
use serde::Serialize;
use serde_json::Value;
use tauri::{Emitter as _, State};
use tokio::sync::oneshot;
use tokio::time::{self, Duration};
use uuid::Uuid;

const EVENT_PEER_DISCOVERED: &str = "peer-discovered";
const EVENT_PEER_LOST: &str = "peer-lost";
const EVENT_MESSAGE_RECEIVED: &str = "message-received";
const EVENT_FILE_OFFER_RECEIVED: &str = "file-offer-received";
const EVENT_TRANSFER_PROGRESS: &str = "transfer-progress";
const EVENT_TRANSFER_STATE_CHANGED: &str = "transfer-state-changed";
const TRANSFER_POLL_INTERVAL: Duration = Duration::from_millis(250);
const TRANSFER_HISTORY_LIMIT: usize = 256;

type DefaultTransferManager = TransferManager<
    FileSender<TransferSignalBridge>,
    FileReceiver<TransferSignalBridge>,
    SqliteStorage,
>;

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

    pub async fn send_message(&self, peer_id: String, content: String) -> Result<String, String> {
        send_message_impl(self, peer_id, content).await
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
    ) -> Result<String, String> {
        send_group_message_impl(self, group_id, content).await
    }

    pub async fn send_file(&self, peer_id: String, file_path: String) -> Result<String, String> {
        send_file_impl(self, peer_id, file_path).await
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
    async fn send_message(&self, peer_id: &str, content: &str) -> Result<Message, String>;
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
    async fn send_group_message(&self, group_id: &str, content: &str) -> Result<Message, String>;
    async fn shutdown(&self) -> Result<(), String>;
}

#[async_trait]
pub trait TransferServiceHandle: Send + Sync {
    async fn send_file(&self, peer_id: &str, file_path: &Path) -> Result<String, String>;
    async fn accept_file(&self, offer_id: &str) -> Result<String, String>;
    async fn reject_file(&self, offer_id: &str, reason: Option<String>) -> Result<(), String>;
    async fn cancel_transfer(&self, transfer_id: &str) -> Result<(), String>;
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
) -> Result<String, String> {
    send_message_impl(state.inner().as_ref(), peerId, content).await
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
) -> Result<String, String> {
    send_group_message_impl(state.inner().as_ref(), groupId, content).await
}

#[tauri::command]
pub async fn send_file(
    state: State<'_, Arc<AppState>>,
    peer_id: String,
    file_path: String,
) -> Result<String, String> {
    send_file_impl(state.inner().as_ref(), peer_id, file_path).await
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
) -> Result<String, String> {
    let message = state.messaging.send_message(&peer_id, &content).await?;
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
) -> Result<String, String> {
    let message = state
        .messaging
        .send_group_message(&group_id, &content)
        .await?;
    Ok(message.id.to_string())
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
            Arc::clone(&emitter),
            identity.device_id.clone(),
        );

        let transfer_signal = TransferSignalBridge::new(Arc::clone(&connector));
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

        connector.add_observer(Arc::new(ChatClientObserver { chat: chat.clone() }));
        connector.add_observer(Arc::new(TransferClientObserver {
            signal: transfer_signal.clone(),
            transfer_manager: Arc::downgrade(&transfer_manager),
            emitter: Arc::clone(&emitter),
        }));

        spawn_server_transfer_bridge(
            Arc::clone(&server),
            Arc::clone(&transfer_manager),
            transfer_signal.clone(),
            Arc::clone(&emitter),
        );

        let transfer_service: Arc<dyn TransferServiceHandle> = Arc::new(RealTransferService {
            manager: Arc::clone(&transfer_manager),
            storage: Arc::clone(&storage),
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
    emitter: Arc<dyn FrontendEmitter>,
    local_device_id: String,
) {
    tauri::async_runtime::spawn(async move {
        let mut events = chat.subscribe();
        loop {
            match events.recv().await {
                Ok(ChatServiceEvent::MessageReceived { peer_id, message }) => {
                    let payload = chat_message_payload(&local_device_id, &peer_id, message);
                    let _ = emit_payload(emitter.as_ref(), EVENT_MESSAGE_RECEIVED, &payload);
                }
                Ok(ChatServiceEvent::MessageStatusUpdated { .. }) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

fn spawn_server_transfer_bridge(
    server: Arc<WsServer>,
    transfer_manager: Arc<DefaultTransferManager>,
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
                        sender_id,
                        sender_address,
                        Arc::downgrade(&transfer_manager),
                        signal.clone(),
                        Arc::clone(&emitter),
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
        let mut interval = time::interval(TRANSFER_POLL_INTERVAL);

        loop {
            interval.tick().await;
            let Ok(current) = transfers.get_transfers().await else {
                continue;
            };

            let mut current_by_id = HashMap::new();
            for transfer in current {
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

                current_by_id.insert(transfer.id.clone(), transfer);
            }

            previous = current_by_id;
        }
    });
}

async fn handle_transfer_protocol_message(
    message: ProtocolMessage,
    sender_id: DeviceId,
    sender_address: SocketAddr,
    transfer_manager: Weak<DefaultTransferManager>,
    signal: TransferSignalBridge,
    emitter: Arc<dyn FrontendEmitter>,
) {
    match message {
        file_offer @ ProtocolMessage::FileOffer { .. } => {
            let Some(transfer_manager) = transfer_manager.upgrade() else {
                return;
            };

            if let Ok(Some(notification)) = transfer_manager
                .handle_signal_message(sender_id, sender_address, file_offer)
                .await
            {
                let payload = FileOfferPayload {
                    id: notification.offer_id,
                    filename: notification.filename,
                    size: notification.size,
                    sender_id: notification.sender_id.0.to_string(),
                };
                let _ = emit_payload(emitter.as_ref(), EVENT_FILE_OFFER_RECEIVED, &payload);
            }
        }
        ProtocolMessage::FileAccept { .. } | ProtocolMessage::FileReject { .. } => {
            let _ = signal.route_response(message);
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
    emitter: Arc<dyn FrontendEmitter>,
}

#[async_trait::async_trait]
impl ClientObserver for TransferClientObserver {
    async fn on_client(&self, client: Arc<WsClient>) -> Result<(), String> {
        let remote_peer = client
            .remote_peer()
            .ok_or_else(|| "client remote peer is unavailable".to_string())?;
        let sender_id = parse_device_id(&remote_peer.identity.device_id)?;
        let sender_address =
            SocketAddr::from_str(&remote_peer.address).map_err(|error| error.to_string())?;
        let signal = self.signal.clone();
        let transfer_manager = self.transfer_manager.clone();
        let emitter = Arc::clone(&self.emitter);

        tauri::async_runtime::spawn(async move {
            let mut events = client.subscribe();
            loop {
                match events.recv().await {
                    Ok(WsClientEvent::MessageReceived { message }) => {
                        handle_transfer_protocol_message(
                            message,
                            sender_id.clone(),
                            sender_address,
                            transfer_manager.clone(),
                            signal.clone(),
                            Arc::clone(&emitter),
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
    async fn send_message(&self, peer_id: &str, content: &str) -> Result<Message, String> {
        let _ = self.connector.ensure_connected(peer_id).await;
        self.chat
            .send_message(peer_id, content.to_string())
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

    async fn send_group_message(&self, group_id: &str, content: &str) -> Result<Message, String> {
        let group_id = ChatId(parse_uuid(group_id, "group_id")?);
        self.chat
            .send_to_group(&group_id, content.to_string())
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
    let sender_uuid = message.sender_id.0.to_string();
    let chat_id = message.chat_id.0.to_string();
    let sender_is_local = sender_uuid == local_device_id;

    ChatMessagePayload {
        id: message.id.to_string(),
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
        content: message.content,
        timestamp: message.timestamp_ms,
        status: message_status_label(&message.status).to_string(),
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
    }
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
