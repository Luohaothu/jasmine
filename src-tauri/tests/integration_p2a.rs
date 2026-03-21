use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jasmine_app::commands::{
    setup_default_app_state_with_config, AppRuntimeConfig, AppState, ChatMessagePayload,
    FrontendEmitter, PeerPayload, TransferPayload,
};
use jasmine_core::{
    parse_mentions, Message, PeerInfo, ProtocolMessage, StorageEngine, TransferStatus,
};
use jasmine_messaging::{WsClient, WsClientConfig, WsClientEvent, WsPeerIdentity};
use jasmine_storage::SqliteStorage;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::sync::{broadcast, Mutex as AsyncMutex, Notify};
use tokio::time;
use uuid::Uuid;

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);
const ACTION_TIMEOUT: Duration = Duration::from_secs(15);
const TEST_TIMEOUT: Duration = Duration::from_secs(45);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const NO_EVENT_WAIT: Duration = Duration::from_millis(750);

const EVENT_MESSAGE_RECEIVED: &str = "message-received";
const EVENT_MESSAGE_EDITED: &str = "message-edited";
const EVENT_MESSAGE_DELETED: &str = "message-deleted";
const EVENT_MENTION_RECEIVED: &str = "mention-received";
const EVENT_FILE_OFFER_RECEIVED: &str = "file-offer-received";
const EVENT_FOLDER_OFFER_RECEIVED: &str = "folder-offer-received";
const EVENT_FOLDER_PROGRESS: &str = "folder-progress";
const EVENT_FOLDER_COMPLETED: &str = "folder-completed";
const EVENT_THUMBNAIL_READY: &str = "thumbnail-ready";
const EVENT_THUMBNAIL_FAILED: &str = "thumbnail-failed";

#[derive(Clone, Debug)]
struct EmittedEvent {
    name: String,
    payload: Value,
}

#[derive(Default)]
struct EventCollector {
    events: Mutex<Vec<EmittedEvent>>,
    notify: Notify,
}

impl EventCollector {
    fn mark(&self) -> usize {
        self.events.lock().expect("lock emitted events").len()
    }

    fn events_since(&self, cursor: usize) -> Vec<EmittedEvent> {
        self.events
            .lock()
            .expect("lock emitted events")
            .iter()
            .skip(cursor)
            .cloned()
            .collect()
    }

    fn has_event_since<F>(&self, cursor: usize, name: &str, predicate: F) -> bool
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

    async fn wait_for_event<F>(
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

struct TestNode {
    _temp_dir: TempDir,
    app_data_dir: PathBuf,
    download_dir: PathBuf,
    emitter: Arc<EventCollector>,
    state: Arc<AppState>,
}

impl TestNode {
    async fn new() -> Self {
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
            .update_settings(jasmine_core::AppSettings {
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

    fn device_id(&self) -> String {
        self.state.local_device_id()
    }

    fn database_path(&self) -> PathBuf {
        self.app_data_dir.join("jasmine.db")
    }

    async fn shutdown(self) {
        self.state.shutdown().await.expect("shutdown app state");
    }
}

fn suite_lock() -> &'static AsyncMutex<()> {
    static LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| AsyncMutex::new(()))
}

async fn within_test_timeout<F, T>(name: &str, future: F) -> T
where
    F: Future<Output = T>,
{
    time::timeout(TEST_TIMEOUT, future)
        .await
        .unwrap_or_else(|_| panic!("{name} exceeded {:?}", TEST_TIMEOUT))
}

async fn wait_for<T, F, Fut>(timeout: Duration, mut check: F) -> T
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

fn now_ms_i64() -> i64 {
    now_ms_u64() as i64
}

fn now_ms_u64() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after unix epoch")
        .as_millis() as u64
}

fn sha256_file(path: &Path) -> Vec<u8> {
    let bytes = std::fs::read(path).expect("read file for sha256");
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().to_vec()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn join_relative_path(root: &Path, relative_path: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for segment in relative_path.split('/') {
        path.push(segment);
    }
    path
}

fn icon_fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("icons/icon.png")
}

async fn wait_for_peer(node: &TestNode, peer_id: String) -> PeerPayload {
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

async fn wait_for_peer_record(db_path: PathBuf, peer_id: String) -> PeerInfo {
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

async fn wait_for_message_payload(
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

async fn wait_for_stored_message(db_path: PathBuf, message_id: String) -> Message {
    let storage = SqliteStorage::open(db_path).expect("open message storage");

    wait_for(ACTION_TIMEOUT, || {
        let storage = storage.clone();
        let message_id = message_id.clone();
        async move { storage.get_message(&message_id).await.ok().flatten() }
    })
    .await
}

async fn wait_for_thumbnail_path(db_path: PathBuf, transfer_id: String) -> String {
    let storage = SqliteStorage::open(db_path).expect("open transfer storage");

    wait_for(ACTION_TIMEOUT, || {
        let storage = storage.clone();
        let transfer_id = transfer_id.clone();
        async move {
            storage
                .get_thumbnail_path(&transfer_id)
                .await
                .ok()
                .flatten()
        }
    })
    .await
}

async fn wait_for_transfer_state(
    node: &TestNode,
    transfer_id: String,
    state_label: String,
) -> TransferPayload {
    wait_for(ACTION_TIMEOUT, || {
        let state = Arc::clone(&node.state);
        let transfer_id = transfer_id.clone();
        let state_label = state_label.clone();
        async move {
            let transfers = state.get_transfers().await.ok()?;
            transfers
                .into_iter()
                .find(|transfer| transfer.id == transfer_id && transfer.state == state_label)
        }
    })
    .await
}

async fn wait_for_transfer_thumbnail_payload(
    node: &TestNode,
    transfer_id: String,
    expected_thumbnail_path: String,
) -> TransferPayload {
    wait_for(ACTION_TIMEOUT, || {
        let state = Arc::clone(&node.state);
        let transfer_id = transfer_id.clone();
        let expected_thumbnail_path = expected_thumbnail_path.clone();
        async move {
            let transfers = state.get_transfers().await.ok()?;
            transfers.into_iter().find(|transfer| {
                transfer.id == transfer_id
                    && transfer.thumbnail_path.as_deref() == Some(expected_thumbnail_path.as_str())
            })
        }
    })
    .await
}

async fn assert_no_event<F>(
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

async fn wait_for_ws_client_message<F>(
    receiver: &mut broadcast::Receiver<WsClientEvent>,
    predicate: F,
) -> ProtocolMessage
where
    F: Fn(&ProtocolMessage) -> bool,
{
    time::timeout(ACTION_TIMEOUT, async {
        loop {
            match receiver.recv().await {
                Ok(WsClientEvent::MessageReceived { message }) if predicate(&message) => {
                    return message;
                }
                Ok(WsClientEvent::MessageReceived { .. }) => continue,
                Ok(WsClientEvent::Disconnected { reason }) => {
                    panic!("manual client disconnected unexpectedly: {reason:?}");
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => {
                    panic!("manual client event channel closed")
                }
            }
        }
    })
    .await
    .expect("timed out waiting for websocket client message")
}

async fn connect_manual_client(
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
    let client = WsClient::connect(
        format!("ws://127.0.0.1:{ws_port}"),
        WsClientConfig::new(WsPeerIdentity::new(
            manual_id.clone(),
            display_name.to_string(),
        )),
    )
    .await
    .expect("connect manual websocket client");
    (client, manual_id)
}

fn write_relative_file(root: &Path, relative_path: &str, bytes: &[u8]) {
    let path = join_relative_path(root, relative_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create file parent directories");
    }
    std::fs::write(path, bytes).expect("write fixture file");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn phase2a_message_features_roundtrip() {
    let _guard = suite_lock().lock().await;
    within_test_timeout("phase2a_message_features_roundtrip", async {
        let alpha = TestNode::new().await;
        let beta = TestNode::new().await;
        let alpha_id = alpha.device_id();
        let beta_id = beta.device_id();
        let alpha_name = alpha
            .state
            .get_identity()
            .expect("load alpha identity")
            .display_name;

        let _ = tokio::join!(
            wait_for_peer(&alpha, beta_id.clone()),
            wait_for_peer(&beta, alpha_id.clone()),
        );

        let mention_content =
            format!("Hello @[Beta](user:{beta_id}), this mention should stay parseable.");
        let mention_cursor = beta.emitter.mark();
        let mention_id = alpha
            .state
            .send_message(beta_id.clone(), mention_content.clone(), None)
            .await
            .expect("send mention message");

        let inbound = beta
            .emitter
            .wait_for_event(
                mention_cursor,
                EVENT_MESSAGE_RECEIVED,
                ACTION_TIMEOUT,
                |payload| payload.get("id").and_then(Value::as_str) == Some(mention_id.as_str()),
            )
            .await;
        assert_eq!(
            inbound.get("content").and_then(Value::as_str),
            Some(mention_content.as_str())
        );

        let mention_event = beta
            .emitter
            .wait_for_event(
                mention_cursor,
                EVENT_MENTION_RECEIVED,
                ACTION_TIMEOUT,
                |payload| {
                    payload.get("messageId").and_then(Value::as_str) == Some(mention_id.as_str())
                },
            )
            .await;
        assert_eq!(
            mention_event.get("mentionedUserId").and_then(Value::as_str),
            Some(beta_id.as_str())
        );
        assert_eq!(
            mention_event.get("senderName").and_then(Value::as_str),
            Some(alpha_name.as_str())
        );

        let receiver_mention =
            wait_for_stored_message(beta.database_path(), mention_id.clone()).await;
        let parsed_mentions = parse_mentions(&receiver_mention.content);
        assert_eq!(parsed_mentions.len(), 1);
        assert_eq!(parsed_mentions[0].user_id, beta_id);

        let no_mention_cursor = beta.emitter.mark();
        let no_mention_id = alpha
            .state
            .send_message(
                beta_id.clone(),
                "Plain text without mentions".to_string(),
                None,
            )
            .await
            .expect("send plain message");
        let _ = beta
            .emitter
            .wait_for_event(
                no_mention_cursor,
                EVENT_MESSAGE_RECEIVED,
                ACTION_TIMEOUT,
                |payload| payload.get("id").and_then(Value::as_str) == Some(no_mention_id.as_str()),
            )
            .await;
        assert_no_event(
            &beta.emitter,
            no_mention_cursor,
            EVENT_MENTION_RECEIVED,
            NO_EVENT_WAIT,
            |payload| {
                payload.get("messageId").and_then(Value::as_str) == Some(no_mention_id.as_str())
            },
        )
        .await;

        let unauthorized = beta
            .state
            .edit_message(mention_id.clone(), "tampered".to_string())
            .await
            .expect_err("receiver should not edit sender message");
        assert!(
            unauthorized.contains("unauthorized"),
            "unexpected unauthorized edit error: {unauthorized}"
        );

        let edited_content = format!("Updated @[Beta](user:{beta_id}) mention content after edit.");
        let edit_cursor = beta.emitter.mark();
        alpha
            .state
            .edit_message(mention_id.clone(), edited_content.clone())
            .await
            .expect("edit mention message");

        let edit_event = beta
            .emitter
            .wait_for_event(
                edit_cursor,
                EVENT_MESSAGE_EDITED,
                ACTION_TIMEOUT,
                |payload| {
                    payload.get("messageId").and_then(Value::as_str) == Some(mention_id.as_str())
                },
            )
            .await;
        assert_eq!(
            edit_event.get("newContent").and_then(Value::as_str),
            Some(edited_content.as_str())
        );
        assert_eq!(
            edit_event.get("editVersion").and_then(Value::as_u64),
            Some(1)
        );
        assert!(edit_event.get("editedAt").and_then(Value::as_u64).is_some());

        let sender_edited =
            wait_for_stored_message(alpha.database_path(), mention_id.clone()).await;
        let receiver_edited =
            wait_for_stored_message(beta.database_path(), mention_id.clone()).await;
        for message in [&sender_edited, &receiver_edited] {
            assert_eq!(message.content, edited_content);
            assert_eq!(message.edit_version, 1);
            assert!(message.edited_at.is_some());
            assert_eq!(parse_mentions(&message.content).len(), 1);
            assert_eq!(parse_mentions(&message.content)[0].user_id, beta_id);
        }

        let reply_cursor = alpha.emitter.mark();
        let reply_content = "Snapshot reply should survive delete".to_string();
        let reply_id = beta
            .state
            .send_message(
                alpha_id.clone(),
                reply_content.clone(),
                Some(mention_id.clone()),
            )
            .await
            .expect("send quote reply");
        let _ = alpha
            .emitter
            .wait_for_event(
                reply_cursor,
                EVENT_MESSAGE_RECEIVED,
                ACTION_TIMEOUT,
                |payload| payload.get("id").and_then(Value::as_str) == Some(reply_id.as_str()),
            )
            .await;

        let sender_reply = wait_for_stored_message(beta.database_path(), reply_id.clone()).await;
        let receiver_reply = wait_for_stored_message(alpha.database_path(), reply_id.clone()).await;
        for reply in [&sender_reply, &receiver_reply] {
            assert_eq!(reply.reply_to_id.as_deref(), Some(mention_id.as_str()));
            assert_eq!(
                reply.reply_to_preview.as_deref(),
                Some(edited_content.as_str())
            );
        }

        let delete_cursor = beta.emitter.mark();
        alpha
            .state
            .delete_message(mention_id.clone())
            .await
            .expect("delete edited mention message");
        let _ = beta
            .emitter
            .wait_for_event(
                delete_cursor,
                EVENT_MESSAGE_DELETED,
                ACTION_TIMEOUT,
                |payload| {
                    payload.get("messageId").and_then(Value::as_str) == Some(mention_id.as_str())
                },
            )
            .await;

        let sender_deleted =
            wait_for_stored_message(alpha.database_path(), mention_id.clone()).await;
        let receiver_deleted =
            wait_for_stored_message(beta.database_path(), mention_id.clone()).await;
        for message in [&sender_deleted, &receiver_deleted] {
            assert!(message.is_deleted);
            assert!(message.deleted_at.is_some());
        }

        let deleted_payload =
            wait_for_message_payload(&beta, alpha_id.clone(), mention_id.clone()).await;
        assert!(deleted_payload.is_deleted);
        assert!(deleted_payload.deleted_at.is_some());

        let sender_reply_after_delete =
            wait_for_stored_message(beta.database_path(), reply_id.clone()).await;
        let receiver_reply_after_delete =
            wait_for_stored_message(alpha.database_path(), reply_id.clone()).await;
        for reply in [&sender_reply_after_delete, &receiver_reply_after_delete] {
            assert_eq!(reply.reply_to_id.as_deref(), Some(mention_id.as_str()));
            assert_eq!(
                reply.reply_to_preview.as_deref(),
                Some(edited_content.as_str())
            );
        }

        beta.shutdown().await;
        alpha.shutdown().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn phase2a_out_of_order_edit_and_delete_buffering() {
    let _guard = suite_lock().lock().await;
    within_test_timeout("phase2a_out_of_order_edit_and_delete_buffering", async {
        let beta = TestNode::new().await;
        let probe = TestNode::new().await;
        let (manual_client, manual_id) =
            connect_manual_client(&beta, &probe, "Manual Sender").await;
        let beta_id = beta.device_id();

        let edit_message_id = Uuid::new_v4().to_string();
        let edit_cursor = beta.emitter.mark();
        manual_client
            .send(ProtocolMessage::MessageEdit {
                message_id: edit_message_id.clone(),
                chat_id: beta_id.clone(),
                sender_id: manual_id.clone(),
                new_content: "Buffered edit wins".to_string(),
                edit_version: 1,
                timestamp_ms: now_ms_u64(),
            })
            .await
            .expect("send buffered edit");
        manual_client
            .send(ProtocolMessage::TextMessage {
                id: edit_message_id.clone(),
                chat_id: beta_id.clone(),
                sender_id: manual_id.clone(),
                content: "Original text arrives late".to_string(),
                timestamp: now_ms_i64(),
                reply_to_id: None,
                reply_to_preview: None,
            })
            .await
            .expect("send original message after edit");

        let _ = beta
            .emitter
            .wait_for_event(
                edit_cursor,
                EVENT_MESSAGE_RECEIVED,
                ACTION_TIMEOUT,
                |payload| {
                    payload.get("id").and_then(Value::as_str) == Some(edit_message_id.as_str())
                },
            )
            .await;
        let _ = beta
            .emitter
            .wait_for_event(
                edit_cursor,
                EVENT_MESSAGE_EDITED,
                ACTION_TIMEOUT,
                |payload| {
                    payload.get("messageId").and_then(Value::as_str)
                        == Some(edit_message_id.as_str())
                },
            )
            .await;

        let edited_message =
            wait_for_stored_message(beta.database_path(), edit_message_id.clone()).await;
        assert_eq!(edited_message.content, "Buffered edit wins");
        assert_eq!(edited_message.edit_version, 1);
        assert!(edited_message.edited_at.is_some());
        assert!(!edited_message.is_deleted);

        let delete_message_id = Uuid::new_v4().to_string();
        let delete_cursor = beta.emitter.mark();
        manual_client
            .send(ProtocolMessage::MessageDelete {
                message_id: delete_message_id.clone(),
                chat_id: beta_id.clone(),
                sender_id: manual_id.clone(),
                timestamp_ms: now_ms_u64(),
            })
            .await
            .expect("send buffered delete");
        manual_client
            .send(ProtocolMessage::TextMessage {
                id: delete_message_id.clone(),
                chat_id: beta_id,
                sender_id: manual_id,
                content: "This message is deleted on arrival".to_string(),
                timestamp: now_ms_i64(),
                reply_to_id: None,
                reply_to_preview: None,
            })
            .await
            .expect("send original message after delete");

        let _ = beta
            .emitter
            .wait_for_event(
                delete_cursor,
                EVENT_MESSAGE_RECEIVED,
                ACTION_TIMEOUT,
                |payload| {
                    payload.get("id").and_then(Value::as_str) == Some(delete_message_id.as_str())
                },
            )
            .await;
        let _ = beta
            .emitter
            .wait_for_event(
                delete_cursor,
                EVENT_MESSAGE_DELETED,
                ACTION_TIMEOUT,
                |payload| {
                    payload.get("messageId").and_then(Value::as_str)
                        == Some(delete_message_id.as_str())
                },
            )
            .await;

        let deleted_message =
            wait_for_stored_message(beta.database_path(), delete_message_id.clone()).await;
        assert!(deleted_message.is_deleted);
        assert!(deleted_message.deleted_at.is_some());
        assert_eq!(
            deleted_message.content,
            "This message is deleted on arrival"
        );

        manual_client
            .disconnect()
            .await
            .expect("disconnect manual client");
        probe.shutdown().await;
        beta.shutdown().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn phase2a_thumbnail_generation_and_non_image_skip() {
    let _guard = suite_lock().lock().await;
    within_test_timeout("phase2a_thumbnail_generation_and_non_image_skip", async {
        let alpha = TestNode::new().await;
        let beta = TestNode::new().await;
        let alpha_id = alpha.device_id();
        let beta_id = beta.device_id();

        let _ = tokio::join!(
            wait_for_peer(&alpha, beta_id.clone()),
            wait_for_peer(&beta, alpha_id.clone()),
        );

        let image_source = alpha.app_data_dir.join("phase2a-image.png");
        std::fs::copy(icon_fixture_path(), &image_source).expect("copy image fixture");
        let image_cursor = beta.emitter.mark();
        let image_transfer_id = alpha
            .state
            .send_file(beta_id.clone(), image_source.to_string_lossy().into_owned())
            .await
            .expect("send image file");

        let _ = beta
            .emitter
            .wait_for_event(
                image_cursor,
                EVENT_FILE_OFFER_RECEIVED,
                ACTION_TIMEOUT,
                |payload| {
                    payload.get("id").and_then(Value::as_str) == Some(image_transfer_id.as_str())
                },
            )
            .await;
        beta.state
            .accept_file(image_transfer_id.clone())
            .await
            .expect("accept image transfer");

        let _ = tokio::join!(
            wait_for_transfer_state(&alpha, image_transfer_id.clone(), "completed".to_string()),
            wait_for_transfer_state(&beta, image_transfer_id.clone(), "completed".to_string()),
        );
        let thumbnail_path =
            wait_for_thumbnail_path(beta.database_path(), image_transfer_id.clone()).await;
        let transfer_payload = wait_for_transfer_thumbnail_payload(
            &beta,
            image_transfer_id.clone(),
            thumbnail_path.clone(),
        )
        .await;
        assert_eq!(
            transfer_payload.thumbnail_path.as_deref(),
            Some(thumbnail_path.as_str())
        );
        assert!(thumbnail_path.ends_with(".webp"));
        assert!(thumbnail_path.contains("thumbnails"));
        assert!(Path::new(&thumbnail_path).exists());

        let ready_events = beta
            .emitter
            .events_since(image_cursor)
            .into_iter()
            .filter(|event| event.name == EVENT_THUMBNAIL_READY)
            .filter(|event| {
                event.payload.get("transferId").and_then(Value::as_str)
                    == Some(image_transfer_id.as_str())
            })
            .collect::<Vec<_>>();
        if let Some(ready_event) = ready_events.last() {
            assert_eq!(
                ready_event
                    .payload
                    .get("thumbnailPath")
                    .and_then(Value::as_str),
                Some(thumbnail_path.as_str())
            );
        }
        assert_no_event(
            &beta.emitter,
            image_cursor,
            EVENT_THUMBNAIL_FAILED,
            NO_EVENT_WAIT,
            |payload| {
                payload.get("transferId").and_then(Value::as_str)
                    == Some(image_transfer_id.as_str())
            },
        )
        .await;

        let text_source = alpha.app_data_dir.join("phase2a-notes.txt");
        std::fs::write(
            &text_source,
            b"plain text file should not generate thumbnails",
        )
        .expect("write text fixture");
        let text_cursor = beta.emitter.mark();
        let text_transfer_id = alpha
            .state
            .send_file(beta_id.clone(), text_source.to_string_lossy().into_owned())
            .await
            .expect("send text file");

        let _ = beta
            .emitter
            .wait_for_event(
                text_cursor,
                EVENT_FILE_OFFER_RECEIVED,
                ACTION_TIMEOUT,
                |payload| {
                    payload.get("id").and_then(Value::as_str) == Some(text_transfer_id.as_str())
                },
            )
            .await;
        beta.state
            .accept_file(text_transfer_id.clone())
            .await
            .expect("accept text transfer");
        let _ = tokio::join!(
            wait_for_transfer_state(&alpha, text_transfer_id.clone(), "completed".to_string()),
            wait_for_transfer_state(&beta, text_transfer_id.clone(), "completed".to_string()),
        );

        assert_no_event(
            &beta.emitter,
            text_cursor,
            EVENT_THUMBNAIL_READY,
            NO_EVENT_WAIT,
            |payload| {
                payload.get("transferId").and_then(Value::as_str) == Some(text_transfer_id.as_str())
            },
        )
        .await;
        assert_no_event(
            &beta.emitter,
            text_cursor,
            EVENT_THUMBNAIL_FAILED,
            NO_EVENT_WAIT,
            |payload| {
                payload.get("transferId").and_then(Value::as_str) == Some(text_transfer_id.as_str())
            },
        )
        .await;

        let storage = SqliteStorage::open(beta.database_path()).expect("open beta storage");
        assert_eq!(
            storage
                .get_thumbnail_path(&text_transfer_id)
                .await
                .expect("load text thumbnail path"),
            None
        );

        beta.shutdown().await;
        alpha.shutdown().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn phase2a_folder_transfer_reconstructs_and_rejects() {
    let _guard = suite_lock().lock().await;
    within_test_timeout("phase2a_folder_transfer_reconstructs_and_rejects", async {
        let alpha = TestNode::new().await;
        let beta = TestNode::new().await;
        let alpha_id = alpha.device_id();
        let beta_id = beta.device_id();

        let _ = tokio::join!(
            wait_for_peer(&alpha, beta_id.clone()),
            wait_for_peer(&beta, alpha_id.clone()),
        );

        let source_root = alpha.app_data_dir.join("phase2a-folder");
        let icon_bytes = std::fs::read(icon_fixture_path()).expect("read icon fixture");
        let files = vec![
            ("README.md", b"phase2a folder fixture".to_vec()),
            ("docs/guide.txt", b"guide".to_vec()),
            ("docs/nested/notes.md", b"notes".to_vec()),
            ("images/icon-copy.png", icon_bytes),
            ("data/one.bin", vec![1_u8; 64]),
            ("data/two.bin", vec![2_u8; 96]),
            ("data/three.bin", vec![3_u8; 128]),
            ("logs/app.log", b"alpha beta gamma".to_vec()),
            ("src/main.txt", b"main".to_vec()),
            ("src/lib.txt", b"lib".to_vec()),
        ];
        for (relative_path, bytes) in &files {
            write_relative_file(&source_root, relative_path, bytes);
        }

        let alpha_complete_cursor = alpha.emitter.mark();
        let beta_offer_cursor = beta.emitter.mark();
        let folder_transfer_id = alpha
            .state
            .send_folder(beta_id.clone(), source_root.to_string_lossy().into_owned())
            .await
            .expect("send folder");

        let offer = beta
            .emitter
            .wait_for_event(
                beta_offer_cursor,
                EVENT_FOLDER_OFFER_RECEIVED,
                ACTION_TIMEOUT,
                |payload| {
                    payload.get("folderTransferId").and_then(Value::as_str)
                        == Some(folder_transfer_id.as_str())
                },
            )
            .await;
        assert_eq!(
            offer.get("folderName").and_then(Value::as_str),
            Some("phase2a-folder")
        );
        assert_eq!(offer.get("fileCount").and_then(Value::as_u64), Some(10));

        let beta_complete_cursor = beta.emitter.mark();
        beta.state
            .accept_folder_transfer(
                folder_transfer_id.clone(),
                beta.download_dir.to_string_lossy().into_owned(),
            )
            .await
            .expect("accept folder transfer");

        let alpha_completed = alpha
            .emitter
            .wait_for_event(
                alpha_complete_cursor,
                EVENT_FOLDER_COMPLETED,
                ACTION_TIMEOUT,
                |payload| {
                    payload.get("folderTransferId").and_then(Value::as_str)
                        == Some(folder_transfer_id.as_str())
                },
            )
            .await;
        let beta_completed = beta
            .emitter
            .wait_for_event(
                beta_complete_cursor,
                EVENT_FOLDER_COMPLETED,
                ACTION_TIMEOUT,
                |payload| {
                    payload.get("folderTransferId").and_then(Value::as_str)
                        == Some(folder_transfer_id.as_str())
                },
            )
            .await;
        assert_eq!(
            alpha_completed.get("status").and_then(Value::as_str),
            Some("completed")
        );
        assert_eq!(
            beta_completed.get("status").and_then(Value::as_str),
            Some("completed")
        );

        let reconstructed_root = beta.download_dir.join("phase2a-folder");
        assert!(reconstructed_root.is_dir());
        for (relative_path, _) in &files {
            let source_path = join_relative_path(&source_root, relative_path);
            let target_path = join_relative_path(&reconstructed_root, relative_path);
            assert!(
                target_path.exists(),
                "missing reconstructed file {relative_path}"
            );
            assert_eq!(sha256_file(&source_path), sha256_file(&target_path));
        }

        let alpha_storage =
            SqliteStorage::open(alpha.database_path()).expect("open alpha transfer storage");
        let mut persisted_folder_files = alpha_storage
            .get_transfers(64, 0)
            .await
            .expect("load alpha transfers")
            .into_iter()
            .filter(|transfer| transfer.folder_id.as_deref() == Some(folder_transfer_id.as_str()))
            .collect::<Vec<_>>();
        persisted_folder_files
            .sort_by(|left, right| left.folder_relative_path.cmp(&right.folder_relative_path));

        let mut expected_relative_paths = files
            .iter()
            .map(|(relative_path, _)| relative_path.to_string())
            .collect::<Vec<_>>();
        expected_relative_paths.sort();
        assert_eq!(persisted_folder_files.len(), expected_relative_paths.len());
        assert!(persisted_folder_files.iter().all(|transfer| {
            transfer.status == TransferStatus::Completed
                && transfer.folder_id.as_deref() == Some(folder_transfer_id.as_str())
        }));
        assert_eq!(
            persisted_folder_files
                .iter()
                .map(|transfer| transfer.folder_relative_path.clone())
                .collect::<Vec<_>>(),
            expected_relative_paths
                .into_iter()
                .map(Some)
                .collect::<Vec<_>>()
        );

        let rejected_root = alpha.app_data_dir.join("phase2a-rejected-folder");
        write_relative_file(&rejected_root, "skip.txt", &b"should stay unsent"[..]);
        let reject_cursor = alpha.emitter.mark();
        let reject_offer_cursor = beta.emitter.mark();
        let rejected_folder_id = alpha
            .state
            .send_folder(beta_id, rejected_root.to_string_lossy().into_owned())
            .await
            .expect("send rejected folder");

        let _ = beta
            .emitter
            .wait_for_event(
                reject_offer_cursor,
                EVENT_FOLDER_OFFER_RECEIVED,
                ACTION_TIMEOUT,
                |payload| {
                    payload.get("folderTransferId").and_then(Value::as_str)
                        == Some(rejected_folder_id.as_str())
                },
            )
            .await;
        beta.state
            .reject_folder_transfer(rejected_folder_id.clone())
            .await
            .expect("reject folder transfer");

        let rejected = alpha
            .emitter
            .wait_for_event(
                reject_cursor,
                EVENT_FOLDER_COMPLETED,
                ACTION_TIMEOUT,
                |payload| {
                    payload.get("folderTransferId").and_then(Value::as_str)
                        == Some(rejected_folder_id.as_str())
                },
            )
            .await;
        assert_eq!(
            rejected.get("status").and_then(Value::as_str),
            Some("rejected")
        );

        let reject_progress_events = alpha
            .emitter
            .events_since(reject_cursor)
            .into_iter()
            .filter(|event| event.name == EVENT_FOLDER_PROGRESS)
            .filter(|event| {
                event
                    .payload
                    .get("folderTransferId")
                    .and_then(Value::as_str)
                    == Some(rejected_folder_id.as_str())
            })
            .collect::<Vec<_>>();
        assert!(reject_progress_events.iter().all(|event| {
            event
                .payload
                .get("sentBytes")
                .and_then(Value::as_u64)
                .unwrap_or_default()
                == 0
                && event
                    .payload
                    .get("completedFiles")
                    .and_then(Value::as_u64)
                    .unwrap_or_default()
                    == 0
        }));
        assert!(!beta.download_dir.join("phase2a-rejected-folder").exists());

        beta.shutdown().await;
        alpha.shutdown().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn phase2a_folder_manifest_sanitization_rejects_path_traversal() {
    let _guard = suite_lock().lock().await;
    within_test_timeout(
        "phase2a_folder_manifest_sanitization_rejects_path_traversal",
        async {
            let beta = TestNode::new().await;
            let probe = TestNode::new().await;
            let (manual_client, manual_id) =
                connect_manual_client(&beta, &probe, "Malicious Sender").await;
            let mut manual_events = manual_client.subscribe();

            let folder_transfer_id = Uuid::new_v4().to_string();
            let offer_id = Uuid::new_v4().to_string();
            let file_bytes = &b"owned"[..];
            let sha256 = sha256_hex(file_bytes);
            let offer_cursor = beta.emitter.mark();
            manual_client
                .send(ProtocolMessage::FolderManifest {
                    folder_transfer_id: folder_transfer_id.clone(),
                    manifest: jasmine_core::protocol::FolderManifestData {
                        folder_name: "malicious-folder".to_string(),
                        files: vec![jasmine_core::protocol::FolderFileEntry {
                            relative_path: "../escape.txt".to_string(),
                            size: file_bytes.len() as u64,
                            sha256: sha256.clone(),
                        }],
                        total_size: file_bytes.len() as u64,
                    },
                    sender_id: manual_id.clone(),
                })
                .await
                .expect("send malicious folder manifest");

            let _ = beta
                .emitter
                .wait_for_event(
                    offer_cursor,
                    EVENT_FOLDER_OFFER_RECEIVED,
                    ACTION_TIMEOUT,
                    |payload| {
                        payload.get("folderTransferId").and_then(Value::as_str)
                            == Some(folder_transfer_id.as_str())
                    },
                )
                .await;

            let complete_cursor = beta.emitter.mark();
            beta.state
                .accept_folder_transfer(
                    folder_transfer_id.clone(),
                    beta.download_dir.to_string_lossy().into_owned(),
                )
                .await
                .expect("accept malicious folder transfer");

            let accept_message = wait_for_ws_client_message(&mut manual_events, |message| {
                matches!(
                    message,
                    ProtocolMessage::FolderAccept { folder_transfer_id: accepted }
                        if accepted == &folder_transfer_id
                )
            })
            .await;
            assert!(matches!(
                accept_message,
                ProtocolMessage::FolderAccept { .. }
            ));

            manual_client
                .send(ProtocolMessage::FileOffer {
                    id: offer_id.clone(),
                    filename: "escape.txt".to_string(),
                    size: file_bytes.len() as u64,
                    sha256,
                    transfer_port: 1,
                })
                .await
                .expect("send malicious file offer");

            let reject_message = wait_for_ws_client_message(&mut manual_events, |message| {
                matches!(
                    message,
                    ProtocolMessage::FileReject { offer_id: rejected, .. }
                        if rejected == &offer_id
                )
            })
            .await;
            match reject_message {
                ProtocolMessage::FileReject { reason, .. } => {
                    assert!(
                        reason.unwrap_or_default().contains("path"),
                        "expected path-related file rejection"
                    );
                }
                other => panic!("expected file reject, got {other:?}"),
            }

            let completed = beta
                .emitter
                .wait_for_event(
                    complete_cursor,
                    EVENT_FOLDER_COMPLETED,
                    ACTION_TIMEOUT,
                    |payload| {
                        payload.get("folderTransferId").and_then(Value::as_str)
                            == Some(folder_transfer_id.as_str())
                    },
                )
                .await;
            assert_eq!(
                completed.get("status").and_then(Value::as_str),
                Some("partially-failed")
            );

            assert!(!beta.download_dir.join("escape.txt").exists());
            assert!(!beta.app_data_dir.join("escape.txt").exists());

            manual_client
                .disconnect()
                .await
                .expect("disconnect manual client");
            probe.shutdown().await;
            beta.shutdown().await;
        },
    )
    .await;
}
