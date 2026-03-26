#![allow(dead_code)]

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use jasmine_app::commands::{
    setup_default_app_state_with_config, AppRuntimeConfig, AppState, ChatMessagePayload,
    FrontendEmitter, PeerPayload,
};
use jasmine_core::{AppSettings, PeerInfo, ProtocolMessage, CURRENT_PROTOCOL_VERSION};
use jasmine_crypto::{generate_identity_keypair, public_key_to_base64};
use jasmine_messaging::{WsClient, WsClientConfig, WsPeerIdentity};
use jasmine_storage::SqliteStorage;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio::time;
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};
use uuid::Uuid;

pub const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);
pub const ACTION_TIMEOUT: Duration = Duration::from_secs(15);
pub const TEST_TIMEOUT: Duration = Duration::from_secs(45);
pub const POLL_INTERVAL: Duration = Duration::from_millis(50);
pub const NO_EVENT_WAIT: Duration = Duration::from_millis(750);

pub const EVENT_MESSAGE_RECEIVED: &str = "message-received";
pub const EVENT_GROUP_MESSAGE_RECEIVED: &str = "group-message-received";
pub const EVENT_MESSAGE_EDITED: &str = "message-edited";
pub const EVENT_MESSAGE_DELETED: &str = "message-deleted";
pub const EVENT_MENTION_RECEIVED: &str = "mention-received";
pub const EVENT_FILE_OFFER_RECEIVED: &str = "file-offer-received";
pub const EVENT_TRANSFER_STATE_CHANGED: &str = "transfer-state-changed";
pub const EVENT_TRANSFER_PROGRESS: &str = "transfer-progress";
pub const EVENT_FOLDER_OFFER_RECEIVED: &str = "folder-offer-received";
pub const EVENT_FOLDER_PROGRESS: &str = "folder-progress";
pub const EVENT_FOLDER_COMPLETED: &str = "folder-completed";
pub const EVENT_THUMBNAIL_READY: &str = "thumbnail-ready";
pub const EVENT_THUMBNAIL_FAILED: &str = "thumbnail-failed";
pub const EVENT_CALL_OFFER: &str = "call-offer";
pub const EVENT_CALL_ANSWER: &str = "call-answer";
pub const EVENT_ICE_CANDIDATE: &str = "ice-candidate";
pub const EVENT_CALL_HANGUP: &str = "call-hangup";
pub const EVENT_CALL_REJECT: &str = "call-reject";
pub const EVENT_CALL_JOIN: &str = "call-join";
pub const EVENT_CALL_LEAVE: &str = "call-leave";

#[derive(Clone, Debug)]
pub struct EmittedEvent {
    pub name: String,
    pub payload: Value,
}

#[derive(Default)]
pub struct EventCollector {
    pub events: Mutex<Vec<EmittedEvent>>,
    pub notify: Notify,
}

impl EventCollector {
    pub fn mark(&self) -> usize {
        self.events.lock().expect("lock emitted events").len()
    }

    pub fn events_since(&self, cursor: usize) -> Vec<EmittedEvent> {
        self.events
            .lock()
            .expect("lock emitted events")
            .iter()
            .skip(cursor)
            .cloned()
            .collect()
    }

    pub fn has_event_since<F>(&self, cursor: usize, name: &str, predicate: F) -> bool
    where
        F: Fn(&Value) -> bool,
    {
        self.events
            .lock()
            .expect("lock emitted events")
            .iter()
            .skip(cursor)
            .any(|event| event.name == name && predicate(&event.payload))
    }

    pub async fn wait_for_event<F>(
        &self,
        mut cursor: usize,
        name: &str,
        timeout: Duration,
        predicate: F,
    ) -> Value
    where
        F: Fn(&Value) -> bool,
    {
        let name = name.to_string();

        time::timeout(timeout, async {
            loop {
                let notified = self.notify.notified();
                if let Some(payload) = {
                    let events = self.events.lock().expect("lock emitted events");
                    let payload = events
                        .iter()
                        .skip(cursor)
                        .find(|event| event.name == name && predicate(&event.payload))
                        .map(|event| event.payload.clone());
                    cursor = events.len();
                    payload
                } {
                    return payload;
                }

                notified.await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for event {name}"))
    }
}

impl FrontendEmitter for EventCollector {
    fn emit_json(&self, event: &str, payload: Value) -> Result<(), String> {
        self.events
            .lock()
            .expect("lock emitted events")
            .push(EmittedEvent {
                name: event.to_string(),
                payload,
            });
        self.notify.notify_waiters();
        Ok(())
    }
}

pub struct TestNode {
    pub _temp_dir: TempDir,
    pub app_data_dir: PathBuf,
    pub download_dir: PathBuf,
    pub emitter: Arc<EventCollector>,
    pub state: Arc<AppState>,
}

impl TestNode {
    pub async fn new() -> Self {
        let temp_dir = TempDir::new().expect("create temp dir");
        let app_data_dir = temp_dir.path().join("app-data");
        let download_dir = temp_dir.path().join("downloads");
        std::fs::create_dir_all(&download_dir).expect("create downloads dir");

        let emitter = Arc::new(EventCollector::default());
        let state = setup_default_app_state_with_config(
            &app_data_dir,
            emitter.clone(),
            AppRuntimeConfig {
                ws_bind_addr: "0.0.0.0:0".to_string(),
            },
        )
        .await
        .expect("setup test app state");

        state
            .update_settings(AppSettings {
                download_dir: download_dir.to_string_lossy().into_owned(),
                max_concurrent_transfers: 3,
            })
            .await
            .expect("update download settings");

        Self {
            _temp_dir: temp_dir,
            app_data_dir,
            download_dir,
            emitter,
            state,
        }
    }

    pub fn device_id(&self) -> String {
        self.state.local_device_id()
    }

    pub fn database_path(&self) -> PathBuf {
        self.app_data_dir.join("jasmine.db")
    }

    pub async fn shutdown(self) {
        self.state.shutdown().await.expect("shutdown app state");
    }
}

pub fn suite_lock() -> &'static AsyncMutex<()> {
    static LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| AsyncMutex::new(()))
}

pub async fn within_test_timeout<F, T>(name: &str, future: F) -> T
where
    F: Future<Output = T>,
{
    time::timeout(TEST_TIMEOUT, future)
        .await
        .unwrap_or_else(|_| panic!("{name} exceeded {:?}", TEST_TIMEOUT))
}

pub async fn wait_for<T, F, Fut>(timeout: Duration, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    time::timeout(timeout, async {
        loop {
            if let Some(value) = check().await {
                return value;
            }

            time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect("timed out waiting for condition")
}

pub fn now_ms_i64() -> i64 {
    now_ms_u64() as i64
}

pub fn now_ms_u64() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after unix epoch")
        .as_millis() as u64
}

pub fn sha256_file(path: &Path) -> Vec<u8> {
    let bytes = std::fs::read(path).expect("read file for sha256");
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().to_vec()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn join_relative_path(root: &Path, relative_path: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for segment in relative_path.split('/') {
        path.push(segment);
    }
    path
}

pub fn icon_fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("icons/icon.png")
}

pub async fn wait_for_peer(node: &TestNode, peer_id: String) -> PeerPayload {
    wait_for(DISCOVERY_TIMEOUT, || {
        let state = Arc::clone(&node.state);
        let peer_id = peer_id.clone();
        async move {
            let peers = state.get_peers().await.ok()?;
            peers.into_iter().find(|peer| peer.id == peer_id)
        }
    })
    .await
}

pub async fn wait_for_peer_record(db_path: PathBuf, peer_id: String) -> PeerInfo {
    let storage = SqliteStorage::open(db_path).expect("open peer storage");

    wait_for(DISCOVERY_TIMEOUT, || {
        let storage = storage.clone();
        let peer_id = peer_id.clone();
        async move {
            let peers = storage.get_peers().await.ok()?;
            peers
                .into_iter()
                .find(|peer| peer.device_id.0.to_string() == peer_id)
        }
    })
    .await
}

pub async fn wait_for_message_payload(
    node: &TestNode,
    chat_id: String,
    message_id: String,
) -> ChatMessagePayload {
    wait_for(ACTION_TIMEOUT, || {
        let state = Arc::clone(&node.state);
        let chat_id = chat_id.clone();
        let message_id = message_id.clone();
        async move {
            let messages = state.get_messages(chat_id, 50, 0).await.ok()?;
            messages
                .into_iter()
                .find(|message| message.id == message_id)
        }
    })
    .await
}

pub async fn assert_no_event<F>(
    collector: &Arc<EventCollector>,
    cursor: usize,
    name: &str,
    timeout: Duration,
    predicate: F,
) where
    F: Fn(&Value) -> bool,
{
    time::sleep(timeout).await;
    assert!(
        !collector.has_event_since(cursor, name, predicate),
        "unexpected event {name} after cursor {cursor}"
    );
}

pub async fn connect_manual_client(
    target: &TestNode,
    probe: &TestNode,
    display_name: &str,
) -> (WsClient, String) {
    let target_id = target.device_id();
    let _ = wait_for_peer(probe, target_id.clone()).await;
    let peer = wait_for_peer_record(probe.database_path(), target_id).await;
    let ws_port = peer
        .ws_port
        .expect("target peer must advertise websocket port");
    let manual_id = Uuid::new_v4().to_string();
    let (private_key, public_key) = generate_identity_keypair();
    let local_peer = WsPeerIdentity::new(manual_id.clone(), display_name.to_string())
        .with_transport_identity(public_key_to_base64(&public_key), CURRENT_PROTOCOL_VERSION);
    let mut client_config = WsClientConfig::new(local_peer);
    client_config.local_private_key = Some(private_key.to_bytes().to_vec());
    let client = WsClient::connect(format!("ws://127.0.0.1:{ws_port}"), client_config)
        .await
        .expect("connect manual websocket client");
    (client, manual_id)
}

pub async fn legacy_protocol_rejection(
    target: &TestNode,
    probe: &TestNode,
    display_name: &str,
    protocol_version: u32,
) -> (ProtocolMessage, String) {
    let target_id = target.device_id();
    let _ = wait_for_peer(probe, target_id.clone()).await;
    let peer = wait_for_peer_record(probe.database_path(), target_id).await;
    let ws_port = peer
        .ws_port
        .expect("target peer must advertise websocket port");
    let manual_id = Uuid::new_v4().to_string();
    let (_, public_key) = generate_identity_keypair();
    let (mut socket, _) = connect_async(format!("ws://127.0.0.1:{ws_port}"))
        .await
        .expect("connect raw websocket legacy client");

    socket
        .send(WsMessage::Text(
            ProtocolMessage::PeerInfo {
                device_id: manual_id,
                display_name: display_name.to_string(),
                avatar_hash: None,
                public_key: Some(public_key_to_base64(&public_key)),
                protocol_version: Some(protocol_version),
            }
            .to_json()
            .expect("serialize legacy peer info")
            .into(),
        ))
        .await
        .expect("send legacy peer info");

    let incompatibility = time::timeout(ACTION_TIMEOUT, async {
        loop {
            match socket.next().await {
                Some(Ok(WsMessage::Text(payload))) => {
                    return ProtocolMessage::from_json(payload.as_str())
                        .expect("parse legacy incompatibility message");
                }
                Some(Ok(_)) => continue,
                Some(Err(error)) => {
                    panic!("legacy websocket transport error before close: {error}")
                }
                None => panic!("legacy websocket closed before incompatibility message"),
            }
        }
    })
    .await
    .expect("timed out waiting for legacy incompatibility message");

    time::timeout(ACTION_TIMEOUT, async {
        loop {
            match socket.next().await {
                Some(Ok(WsMessage::Close(Some(frame)))) => {
                    return (incompatibility, frame.reason.to_string());
                }
                Some(Ok(_)) => continue,
                Some(Err(error)) => panic!("legacy websocket closed with transport error: {error}"),
                None => panic!("legacy websocket closed without close reason"),
            }
        }
    })
    .await
    .expect("timed out waiting for legacy close reason")
}
