use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use async_trait::async_trait;
use jasmine_core::{
    AppSettings, ChatId, DeviceId, DeviceIdentity, DiscoveryService, IdentityStore, Message,
    PeerInfo, ProtocolMessage, SettingsService, StorageEngine, TransferStatus,
};
use jasmine_crypto::{fingerprint, public_key_from_base64};
use jasmine_crypto::{load_private_key, store_private_key, StaticSecret};
use jasmine_discovery::{DiscoveryManager, MdnsDiscoveryConfig, UdpBroadcastConfig};
use jasmine_messaging::{
    ChatService, WsClient, WsClientConfig, WsClientEvent, WsPeerIdentity, WsServer, WsServerConfig,
    WsServerEvent,
};
use jasmine_storage::{ChatType, SqliteStorage};
use jasmine_transfer::{
    FileCryptoMaterial, FileReceiver, FileReceiverError, FileReceiverSignal, FileSender,
    FileSenderError, FileSenderSignal, FolderProgress, FolderProgressReporter, FolderReceiver,
    FolderTransferCoordinator, FolderTransferOutcome, FolderTransferStatus, TransferManager,
};
use tokio::sync::oneshot;
use tokio::time::{self, Duration};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::bridge::{emit_chat_service_event, emit_payload, FolderBridgeState};
use super::discovery::peer_payload;
use super::transfers::{transfer_payload_from_managed, transfer_payload_from_record};
use super::types::{
    AppRuntimeConfig, AppSetupFactory, AppState, DiscoveryServiceHandle, FileOfferPayload,
    FolderCompletedPayload, FolderOfferPayload, FolderProgressPayload, FrontendEmitter,
    IdentityServiceHandle, MessagingServiceHandle, OwnFingerprintInfo, PeerFingerprintInfo,
    PeerIncompatiblePayload, PeerLostPayload, SettingsServiceHandle, SetupContext, StoredGroupInfo,
    ThumbnailFailedPayload, ThumbnailReadyPayload, TransferPayload, TransferProgressPayload,
    TransferServiceHandle, TransferStatePayload,
};

const EVENT_PEER_DISCOVERED: &str = "peer-discovered";
const EVENT_PEER_LOST: &str = "peer-lost";
const EVENT_PEER_INCOMPATIBLE: &str = "peer-incompatible";
const EVENT_FILE_OFFER_RECEIVED: &str = "file-offer-received";
const EVENT_TRANSFER_PROGRESS: &str = "transfer-progress";
const EVENT_TRANSFER_STATE_CHANGED: &str = "transfer-state-changed";
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
        let (identity, generated_private_key) = identity_store
            .load_with_private_key()
            .map_err(|error| error.to_string())?;
        let local_private_key = resolve_local_private_key(&identity, generated_private_key)
            .map_err(|error| error.to_string())?;
        let settings_service = SettingsService::new(&context.app_data_dir);
        let storage = Arc::new(
            SqliteStorage::open(&context.database_path).map_err(|error| error.to_string())?,
        );

        let local_device_id = parse_uuid(&identity.device_id, "device_id")?;
        let local_peer = local_peer_identity(&identity);
        let mut server_config = WsServerConfig::new(local_peer.clone());
        server_config.local_private_key = Some(local_private_key.clone());
        server_config.peer_key_store = Some(Arc::clone(&storage));
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
            local_private_key,
            Arc::clone(&storage),
            Arc::clone(&discovery),
            Arc::clone(&server),
            Arc::clone(&emitter),
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
            }) as Arc<dyn DiscoveryServiceHandle>,
            Arc::new(RealMessagingService {
                chat,
                connector,
                server,
                storage: Arc::clone(&storage),
                local_device_id: identity.device_id.clone(),
            }) as Arc<dyn MessagingServiceHandle>,
            transfer_service,
            Arc::new(RealIdentityService {
                store: identity_store,
                storage: Arc::clone(&storage),
            }) as Arc<dyn IdentityServiceHandle>,
            Arc::new(RealSettingsService {
                settings: settings_service,
                transfer_manager,
            }) as Arc<dyn SettingsServiceHandle>,
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
        | ProtocolMessage::FileResumeRequest { .. }
        | ProtocolMessage::FileResumeAccept { .. }
        | ProtocolMessage::FolderResumeRequest { .. }
        | ProtocolMessage::FolderResumeAccept { .. }
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

    async fn file_crypto_material_for(
        &self,
        peer_id: &DeviceId,
        file_id: &str,
    ) -> Result<FileCryptoMaterial, String> {
        self.connector
            .file_crypto_material(&peer_id.0.to_string(), file_id)
            .await
    }

    fn route_response(&self, message: ProtocolMessage) -> bool {
        let offer_id = match &message {
            ProtocolMessage::FileAccept { offer_id } => offer_id.clone(),
            ProtocolMessage::FileReject { offer_id, .. } => offer_id.clone(),
            ProtocolMessage::FileResumeRequest { offer_id, .. } => offer_id.clone(),
            ProtocolMessage::FileResumeAccept { offer_id, .. } => offer_id.clone(),
            ProtocolMessage::FolderResumeRequest { folder_id, .. } => folder_id.clone(),
            ProtocolMessage::FolderResumeAccept { folder_id, .. } => folder_id.clone(),
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

    async fn file_crypto_material(
        &self,
        peer_id: &DeviceId,
        file_id: &str,
    ) -> Result<FileCryptoMaterial, FileSenderError> {
        self.file_crypto_material_for(peer_id, file_id)
            .await
            .map_err(FileSenderError::Signal)
    }
}

impl FileReceiverSignal for TransferSignalBridge {
    async fn send_message(
        &self,
        peer_id: &DeviceId,
        message: ProtocolMessage,
    ) -> Result<(), FileReceiverError> {
        let resume_offer_id = match &message {
            ProtocolMessage::FileResumeRequest { offer_id, .. } => Some(offer_id.clone()),
            ProtocolMessage::FolderResumeRequest { folder_id, .. } => Some(folder_id.clone()),
            _ => None,
        };
        if let Some(offer_id) = &resume_offer_id {
            let (tx, rx) = oneshot::channel();
            self.response_senders
                .lock()
                .expect("lock response senders")
                .insert(offer_id.clone(), tx);
            self.response_receivers
                .lock()
                .expect("lock response receivers")
                .insert(offer_id.clone(), rx);
        }

        let result = self
            .connector
            .send_protocol(&peer_id.0.to_string(), message)
            .await
            .map_err(FileReceiverError::Signal);

        if result.is_err() {
            if let Some(offer_id) = resume_offer_id {
                self.response_senders
                    .lock()
                    .expect("lock response senders")
                    .remove(&offer_id);
                self.response_receivers
                    .lock()
                    .expect("lock response receivers")
                    .remove(&offer_id);
            }
        }

        result
    }

    async fn wait_for_response(
        &self,
        offer_id: &str,
    ) -> Result<ProtocolMessage, FileReceiverError> {
        let receiver = self
            .response_receivers
            .lock()
            .expect("lock response receivers")
            .remove(offer_id)
            .ok_or_else(|| FileReceiverError::Signal("response receiver missing".to_string()))?;
        receiver
            .await
            .map_err(|_| FileReceiverError::Signal("response channel closed".to_string()))
    }

    async fn file_crypto_material(
        &self,
        peer_id: &DeviceId,
        file_id: &str,
    ) -> Result<FileCryptoMaterial, FileReceiverError> {
        self.file_crypto_material_for(peer_id, file_id)
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
    local_private_key: Vec<u8>,
    storage: Arc<SqliteStorage>,
    discovery: Arc<DiscoveryManager>,
    server: Arc<WsServer>,
    emitter: Arc<dyn FrontendEmitter>,
    clients: Arc<Mutex<HashMap<String, Arc<WsClient>>>>,
    observers: Mutex<Vec<Arc<dyn ClientObserver>>>,
}

impl PeerConnector {
    fn new(
        local_peer: WsPeerIdentity,
        local_private_key: Vec<u8>,
        storage: Arc<SqliteStorage>,
        discovery: Arc<DiscoveryManager>,
        server: Arc<WsServer>,
        emitter: Arc<dyn FrontendEmitter>,
    ) -> Self {
        Self {
            local_peer,
            local_private_key,
            storage,
            discovery,
            server,
            emitter,
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

    async fn file_crypto_material(
        &self,
        peer_id: &str,
        file_id: &str,
    ) -> Result<FileCryptoMaterial, String> {
        if let Ok((file_key, nonce_prefix)) = self.server.file_crypto_material(peer_id, file_id) {
            return Ok((file_key, nonce_prefix));
        }

        self.ensure_connected(peer_id).await?;
        let client = self
            .clients
            .lock()
            .expect("lock peer clients")
            .get(peer_id)
            .cloned()
            .ok_or_else(|| format!("peer {peer_id} is not connected"))?;
        let (file_key, nonce_prefix) = client.file_crypto_material(file_id);
        Ok((file_key, nonce_prefix))
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
        let mut client_config = WsClientConfig::new(self.local_peer.clone());
        client_config.local_private_key = Some(self.local_private_key.clone());
        client_config.peer_key_store = Some(Arc::clone(&self.storage));
        let client = match WsClient::connect(url, client_config).await {
            Ok(client) => Arc::new(client),
            Err(error) => {
                let error_message = error.to_string();
                self.emit_peer_incompatible_if_needed(peer_id, &error_message);
                return Err(error_message);
            }
        };

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

    fn emit_peer_incompatible_if_needed(&self, peer_id: &str, error_message: &str) {
        if !error_message.contains("minimum supported version is 2") {
            return;
        }

        let payload = PeerIncompatiblePayload {
            id: peer_id.to_string(),
            reason: "Update required".to_string(),
        };
        let _ = emit_payload(self.emitter.as_ref(), EVENT_PEER_INCOMPATIBLE, &payload);
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
    storage: Arc<SqliteStorage>,
    local_device_id: String,
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
    ) -> Result<StoredGroupInfo, String> {
        for member in members {
            let _ = self.connector.ensure_connected(member).await;
        }

        let group = self
            .chat
            .create_group(name.to_string(), members.to_vec())
            .await
            .map_err(|error| error.to_string())?;
        self.load_group_info(&group.id).await
    }

    async fn add_group_members(
        &self,
        group_id: &str,
        member_ids: &[String],
    ) -> Result<StoredGroupInfo, String> {
        for member in member_ids {
            let _ = self.connector.ensure_connected(member).await;
        }

        let group_id = ChatId(parse_uuid(group_id, "group_id")?);
        let group = self
            .chat
            .add_group_members(&group_id, member_ids.to_vec())
            .await
            .map_err(|error| error.to_string())?;
        self.load_group_info(&group.id).await
    }

    async fn remove_group_members(
        &self,
        group_id: &str,
        member_ids: &[String],
    ) -> Result<StoredGroupInfo, String> {
        let group_id = ChatId(parse_uuid(group_id, "group_id")?);
        let group = self
            .chat
            .remove_group_members(&group_id, member_ids.to_vec())
            .await
            .map_err(|error| error.to_string())?;
        self.load_group_info(&group.id).await
    }

    async fn get_group_info(&self, group_id: &str) -> Result<StoredGroupInfo, String> {
        let group_id = ChatId(parse_uuid(group_id, "group_id")?);
        self.load_group_info(&group_id).await
    }

    async fn list_groups(&self) -> Result<Vec<StoredGroupInfo>, String> {
        let chats = self
            .storage
            .get_chats()
            .await
            .map_err(|error| error.to_string())?;
        let mut groups = Vec::new();

        for chat in chats {
            if chat.chat_type != ChatType::Group {
                continue;
            }

            groups.push(self.load_group_info(&chat.id).await?);
        }

        Ok(groups)
    }

    async fn leave_group(&self, group_id: &str) -> Result<(), String> {
        let group_id = ChatId(parse_uuid(group_id, "group_id")?);
        self.chat
            .remove_group_members(&group_id, vec![self.local_device_id.clone()])
            .await
            .map(|_| ())
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

impl RealMessagingService {
    async fn load_group_info(&self, group_id: &ChatId) -> Result<StoredGroupInfo, String> {
        let chat = self
            .storage
            .get_chat(group_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("unknown group {}", group_id.0))?;
        if chat.chat_type != ChatType::Group {
            return Err(format!("unknown group {}", group_id.0));
        }

        let member_ids = self
            .storage
            .get_chat_members(group_id)
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|member_id| member_id.0.to_string())
            .collect();

        Ok(StoredGroupInfo {
            id: chat.id.0.to_string(),
            name: chat
                .name
                .ok_or_else(|| format!("group {} missing name", group_id.0))?,
            member_ids,
            created_at: chat.created_at_ms,
        })
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

    async fn resume_transfer(&self, transfer_id: &str) -> Result<String, String> {
        self.manager
            .accept_offer(transfer_id, None)
            .await
            .map_err(|error| error.to_string())
    }

    async fn retry_transfer(&self, transfer_id: &str) -> Result<String, String> {
        let record = self
            .storage
            .get_transfers(TRANSFER_HISTORY_LIMIT, 0)
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|record| record.id.to_string() == transfer_id)
            .ok_or_else(|| format!("unknown transfer {transfer_id}"))?;
        self.manager
            .send_file(record.peer_id, record.local_path, None)
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
    storage: Arc<SqliteStorage>,
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

    fn get_own_fingerprint(&self) -> Result<OwnFingerprintInfo, String> {
        let identity = self.store.load().map_err(|error| error.to_string())?;
        let public_key =
            public_key_from_base64(&identity.public_key).map_err(|error| error.to_string())?;
        Ok(OwnFingerprintInfo {
            device_id: identity.device_id,
            fingerprint: fingerprint(&public_key),
        })
    }

    fn get_peer_fingerprint(&self, peer_id: &str) -> Result<PeerFingerprintInfo, String> {
        let storage = Arc::clone(&self.storage);
        let peer_id = peer_id.to_string();
        let lookup_peer_id = peer_id.clone();
        let peer_key = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?
            .block_on(async move { storage.get_peer_key(&lookup_peer_id).await })
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("unknown peer fingerprint for {peer_id}"))?;

        Ok(PeerFingerprintInfo {
            peer_id: peer_key.device_id,
            fingerprint: peer_key.fingerprint,
            verified: peer_key.verified,
            first_seen: peer_key.first_seen,
        })
    }

    fn toggle_peer_verified(&self, peer_id: &str, verified: bool) -> Result<(), String> {
        let storage = Arc::clone(&self.storage);
        let peer_id = peer_id.to_string();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?
            .block_on(async move { storage.set_peer_verified(&peer_id, verified).await })
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

struct FolderEventReporter {
    emitter: Arc<dyn FrontendEmitter>,
}

impl FolderProgressReporter for FolderEventReporter {
    fn report(&self, progress: FolderProgress) {
        let payload = folder_progress_payload(&progress);
        let _ = emit_payload(self.emitter.as_ref(), EVENT_FOLDER_PROGRESS, &payload);
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

fn local_peer_identity(identity: &DeviceIdentity) -> WsPeerIdentity {
    WsPeerIdentity::new(identity.device_id.clone(), identity.display_name.clone())
        .with_transport_identity(identity.public_key.clone(), identity.protocol_version)
}

fn resolve_local_private_key(
    identity: &DeviceIdentity,
    generated_private_key: Option<Vec<u8>>,
) -> Result<Vec<u8>, jasmine_core::CoreError> {
    if let Some(private_key) = generated_private_key {
        let secret = static_secret_from_bytes(&private_key)?;
        store_private_key(&identity.device_id, &secret)
            .map_err(|error| jasmine_core::CoreError::Persistence(error.to_string()))?;
        return Ok(private_key);
    }

    load_private_key(&identity.device_id)
        .map_err(|error| jasmine_core::CoreError::Persistence(error.to_string()))?
        .map(|secret| secret.to_bytes().to_vec())
        .ok_or_else(|| {
            jasmine_core::CoreError::Persistence(format!(
                "missing private key for device {}",
                identity.device_id
            ))
        })
}

fn static_secret_from_bytes(bytes: &[u8]) -> Result<StaticSecret, jasmine_core::CoreError> {
    let actual = bytes.len();
    let bytes: [u8; 32] = bytes.try_into().map_err(|_| {
        jasmine_core::CoreError::Persistence(format!("private key must be 32 bytes, got {actual}"))
    })?;
    Ok(StaticSecret::from(bytes))
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
