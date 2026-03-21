use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use jasmine_app::commands::{
    setup_default_app_state_with_config, AppRuntimeConfig, AppState, ChatMessagePayload,
    FrontendEmitter, PeerPayload, TransferPayload,
};
use jasmine_core::{AppSettings, ChatId, Message, MessageStatus, StorageEngine};
use jasmine_storage::{ChatType, SqliteStorage};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio::time;
use uuid::Uuid;

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);
const ACTION_TIMEOUT: Duration = Duration::from_secs(10);
const TEST_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

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

fn parse_chat_id(raw: &str) -> ChatId {
    ChatId(Uuid::parse_str(raw).expect("chat id must be uuid"))
}

fn sha256_file(path: &Path) -> Vec<u8> {
    let bytes = std::fs::read(path).expect("read file for sha256");
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().to_vec()
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

async fn wait_for_persisted_peer(db_path: PathBuf, peer_id: String) {
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
    .await;
}

async fn wait_for_message_status(
    node: &TestNode,
    chat_id: String,
    message_id: String,
    status: String,
) -> ChatMessagePayload {
    let state = Arc::clone(&node.state);
    let result = time::timeout(ACTION_TIMEOUT, async {
        loop {
            let messages = state
                .get_messages(chat_id.clone(), 50, 0)
                .await
                .expect("load messages from app state");
            if let Some(message) = messages
                .into_iter()
                .find(|message| message.id == message_id && message.status == status)
            {
                return message;
            }

            time::sleep(POLL_INTERVAL).await;
        }
    })
    .await;

    match result {
        Ok(message) => message,
        Err(_) => {
            let messages = node
                .state
                .get_messages(chat_id, 50, 0)
                .await
                .unwrap_or_default();
            panic!(
                "timed out waiting for message {message_id} to reach status {status}; current messages: {messages:?}"
            );
        }
    }
}

async fn wait_for_db_message(
    db_path: PathBuf,
    chat_id: String,
    message_id: String,
    status: MessageStatus,
) -> Message {
    let storage = SqliteStorage::open(db_path).expect("open message storage");
    let chat_id = parse_chat_id(&chat_id);
    let message_id = Uuid::parse_str(&message_id).expect("message id must be uuid");

    wait_for(ACTION_TIMEOUT, || {
        let storage = storage.clone();
        let chat_id = chat_id.clone();
        let status = status.clone();
        async move {
            let messages = storage.get_messages(&chat_id, 50, 0).await.ok()?;
            messages
                .into_iter()
                .find(|message| message.id == message_id && message.status == status)
        }
    })
    .await
}

async fn wait_for_group_snapshot(
    db_path: PathBuf,
    group_id: String,
    expected_name: String,
    expected_members: Vec<String>,
) {
    let storage = SqliteStorage::open(db_path).expect("open group storage");
    let group_id = parse_chat_id(&group_id);

    wait_for(ACTION_TIMEOUT, || {
        let storage = storage.clone();
        let group_id = group_id.clone();
        let expected_name = expected_name.clone();
        let expected_members = expected_members.clone();
        async move {
            let chat = storage.get_chat(&group_id).await.ok().flatten()?;
            if chat.chat_type != ChatType::Group
                || chat.name.as_deref() != Some(expected_name.as_str())
            {
                return None;
            }

            let members = storage.get_chat_members(&group_id).await.ok()?;
            let actual_members = members
                .into_iter()
                .map(|member| member.0.to_string())
                .collect::<Vec<_>>();
            (actual_members == expected_members).then_some(())
        }
    })
    .await;
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

async fn wait_for_transfer_mix(
    node: &TestNode,
    active_count: usize,
    queued_ids: Vec<String>,
) -> Vec<TransferPayload> {
    wait_for(ACTION_TIMEOUT, || {
        let state = Arc::clone(&node.state);
        let queued_ids = queued_ids.clone();
        async move {
            let transfers = state.get_transfers().await.ok()?;
            let active = transfers
                .iter()
                .filter(|transfer| transfer.state == "active")
                .count();
            let mut actual_queued_ids = transfers
                .iter()
                .filter(|transfer| transfer.state == "queued")
                .map(|transfer| transfer.id.clone())
                .collect::<Vec<_>>();
            let mut expected_queued_ids = queued_ids.clone();
            actual_queued_ids.sort();
            expected_queued_ids.sort();

            (active == active_count && actual_queued_ids == expected_queued_ids)
                .then_some(transfers)
        }
    })
    .await
}

async fn wait_for_file(path: PathBuf, expected_size: u64) {
    wait_for(ACTION_TIMEOUT, || {
        let path = path.clone();
        async move {
            let metadata = std::fs::metadata(&path).ok()?;
            (metadata.len() == expected_size).then_some(())
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn test_discovery() {
    let _guard = suite_lock().lock().await;
    within_test_timeout("test_discovery", async {
        let alpha = TestNode::new().await;
        let beta = TestNode::new().await;
        let alpha_id = alpha.device_id();
        let beta_id = beta.device_id();
        let alpha_db_path = alpha.database_path();
        let beta_db_path = beta.database_path();

        let started = Instant::now();
        let (alpha_peer, beta_peer) = tokio::join!(
            wait_for_peer(&alpha, beta_id.clone()),
            wait_for_peer(&beta, alpha_id.clone()),
        );

        assert!(started.elapsed() < DISCOVERY_TIMEOUT);
        assert_eq!(alpha_peer.id, beta_id.clone());
        assert_eq!(beta_peer.id, alpha_id.clone());

        let _ = tokio::join!(
            wait_for_persisted_peer(alpha_db_path, beta_id),
            wait_for_persisted_peer(beta_db_path, alpha_id),
        );

        beta.shutdown().await;
        alpha.shutdown().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn test_message_roundtrip() {
    let _guard = suite_lock().lock().await;
    within_test_timeout("test_message_roundtrip", async {
        let alpha = TestNode::new().await;
        let beta = TestNode::new().await;
        let alpha_id = alpha.device_id();
        let beta_id = beta.device_id();
        let alpha_db_path = alpha.database_path();
        let beta_db_path = beta.database_path();

        let _ = tokio::join!(
            wait_for_peer(&alpha, beta_id.clone()),
            wait_for_peer(&beta, alpha_id.clone()),
        );

        let beta_cursor = beta.emitter.mark();
        let message_id = alpha
            .state
            .send_message(beta_id.clone(), "Hello from A".to_string(), None)
            .await
            .expect("send direct message");

        let inbound = beta
            .emitter
            .wait_for_event(beta_cursor, "message-received", ACTION_TIMEOUT, |payload| {
                payload.get("id").and_then(Value::as_str) == Some(message_id.as_str())
            })
            .await;

        assert_eq!(
            inbound.get("content").and_then(Value::as_str),
            Some("Hello from A")
        );
        assert_eq!(
            inbound.get("senderId").and_then(Value::as_str),
            Some(alpha_id.as_str())
        );
        assert_eq!(
            inbound.get("receiverId").and_then(Value::as_str),
            Some("local")
        );

        let sender_message = wait_for_message_status(
            &alpha,
            beta_id.clone(),
            message_id.clone(),
            "delivered".to_string(),
        )
        .await;
        assert_eq!(sender_message.content, "Hello from A");

        let receiver_message = wait_for_message_status(
            &beta,
            alpha_id.clone(),
            message_id.clone(),
            "delivered".to_string(),
        )
        .await;
        assert_eq!(receiver_message.content, "Hello from A");

        let (alpha_db, beta_db) = tokio::join!(
            wait_for_db_message(
                alpha_db_path,
                beta_id,
                message_id.clone(),
                MessageStatus::Delivered,
            ),
            wait_for_db_message(beta_db_path, alpha_id, message_id, MessageStatus::Delivered,),
        );

        assert_eq!(alpha_db.content, "Hello from A");
        assert_eq!(beta_db.content, "Hello from A");

        beta.shutdown().await;
        alpha.shutdown().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_group_fanout() {
    let _guard = suite_lock().lock().await;
    within_test_timeout("test_group_fanout", async {
        let alpha = TestNode::new().await;
        let beta = TestNode::new().await;
        let gamma = TestNode::new().await;
        let alpha_id = alpha.device_id();
        let beta_id = beta.device_id();
        let gamma_id = gamma.device_id();

        let _ = tokio::join!(
            wait_for_peer(&alpha, beta_id.clone()),
            wait_for_peer(&alpha, gamma_id.clone()),
            wait_for_peer(&beta, alpha_id.clone()),
            wait_for_peer(&gamma, alpha_id.clone()),
        );

        let group_id = alpha
            .state
            .create_group(
                "Study Group".to_string(),
                vec![beta_id.clone(), gamma_id.clone()],
            )
            .await
            .expect("create group");
        let expected_members = vec![alpha_id.clone(), beta_id.clone(), gamma_id.clone()];

        let _ = tokio::join!(
            wait_for_group_snapshot(
                alpha.database_path(),
                group_id.clone(),
                "Study Group".to_string(),
                expected_members.clone(),
            ),
            wait_for_group_snapshot(
                beta.database_path(),
                group_id.clone(),
                "Study Group".to_string(),
                expected_members.clone(),
            ),
            wait_for_group_snapshot(
                gamma.database_path(),
                group_id.clone(),
                "Study Group".to_string(),
                expected_members,
            ),
        );

        let beta_cursor = beta.emitter.mark();
        let gamma_cursor = gamma.emitter.mark();
        let message_id = alpha
            .state
            .send_group_message(group_id.clone(), "Hello group".to_string(), None)
            .await
            .expect("send group message");

        let (beta_event, gamma_event) = tokio::join!(
            beta.emitter.wait_for_event(
                beta_cursor,
                "message-received",
                ACTION_TIMEOUT,
                |payload| {
                    payload.get("id").and_then(Value::as_str) == Some(message_id.as_str())
                }
            ),
            gamma.emitter.wait_for_event(
                gamma_cursor,
                "message-received",
                ACTION_TIMEOUT,
                |payload| {
                    payload.get("id").and_then(Value::as_str) == Some(message_id.as_str())
                }
            ),
        );

        for payload in [&beta_event, &gamma_event] {
            assert_eq!(
                payload.get("content").and_then(Value::as_str),
                Some("Hello group")
            );
            assert_eq!(
                payload.get("receiverId").and_then(Value::as_str),
                Some(group_id.as_str())
            );
            assert_eq!(
                payload.get("senderId").and_then(Value::as_str),
                Some(alpha_id.as_str())
            );
        }

        let _ = tokio::join!(
            wait_for_db_message(
                beta.database_path(),
                group_id.clone(),
                message_id.clone(),
                MessageStatus::Delivered,
            ),
            wait_for_db_message(
                gamma.database_path(),
                group_id,
                message_id,
                MessageStatus::Delivered,
            ),
        );

        gamma.shutdown().await;
        beta.shutdown().await;
        alpha.shutdown().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_file_transfer() {
    let _guard = suite_lock().lock().await;
    within_test_timeout("test_file_transfer", async {
        let alpha = TestNode::new().await;
        let beta = TestNode::new().await;
        let alpha_id = alpha.device_id();
        let beta_id = beta.device_id();

        let _ = tokio::join!(
            wait_for_peer(&alpha, beta_id.clone()),
            wait_for_peer(&beta, alpha_id.clone()),
        );

        let file_name = "payload.bin";
        let source_path = alpha.app_data_dir.join(file_name);
        let payload = (0..(1024 * 1024))
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        std::fs::write(&source_path, &payload).expect("write source payload");

        let alpha_progress_cursor = alpha.emitter.mark();
        let beta_offer_cursor = beta.emitter.mark();
        let transfer_id = alpha
            .state
            .send_file(beta_id.clone(), source_path.to_string_lossy().into_owned())
            .await
            .expect("start file transfer");

        let offer = beta
            .emitter
            .wait_for_event(
                beta_offer_cursor,
                "file-offer-received",
                ACTION_TIMEOUT,
                |payload| payload.get("id").and_then(Value::as_str) == Some(transfer_id.as_str()),
            )
            .await;

        assert_eq!(
            offer.get("filename").and_then(Value::as_str),
            Some(file_name)
        );
        assert_eq!(
            offer.get("size").and_then(Value::as_u64),
            Some(payload.len() as u64)
        );
        assert_eq!(
            offer.get("senderId").and_then(Value::as_str),
            Some(alpha_id.as_str())
        );

        beta.state
            .accept_file(transfer_id.clone())
            .await
            .expect("accept file transfer");

        let progress = alpha
            .emitter
            .wait_for_event(
                alpha_progress_cursor,
                "transfer-progress",
                ACTION_TIMEOUT,
                |payload| payload.get("id").and_then(Value::as_str) == Some(transfer_id.as_str()),
            )
            .await;
        assert_eq!(
            progress.get("id").and_then(Value::as_str),
            Some(transfer_id.as_str())
        );

        let received_path = beta.download_dir.join(file_name);
        wait_for_file(received_path.clone(), payload.len() as u64).await;

        let (alpha_transfer, beta_transfer) = tokio::join!(
            wait_for_transfer_state(&alpha, transfer_id.clone(), "completed".to_string()),
            wait_for_transfer_state(&beta, transfer_id, "completed".to_string()),
        );

        assert_eq!(alpha_transfer.progress, 1.0);
        assert_eq!(beta_transfer.progress, 1.0);
        assert_eq!(sha256_file(&source_path), sha256_file(&received_path));

        beta.shutdown().await;
        alpha.shutdown().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn test_offline_message() {
    let _guard = suite_lock().lock().await;
    within_test_timeout("test_offline_message", async {
        let alpha = TestNode::new().await;
        let beta = TestNode::new().await;
        let alpha_id = alpha.device_id();
        let beta_id = beta.device_id();

        let _ = tokio::join!(
            wait_for_peer(&alpha, beta_id.clone()),
            wait_for_peer(&beta, alpha_id),
        );

        beta.shutdown().await;
        time::sleep(Duration::from_millis(250)).await;

        let message_id = alpha
            .state
            .send_message(beta_id.clone(), "Offline delivery".to_string(), None)
            .await
            .expect("queue offline message");

        let failed = wait_for_message_status(
            &alpha,
            beta_id.clone(),
            message_id.clone(),
            "failed".to_string(),
        )
        .await;
        assert_eq!(failed.content, "Offline delivery");

        let persisted = wait_for_db_message(
            alpha.database_path(),
            beta_id,
            message_id,
            MessageStatus::Failed,
        )
        .await;
        assert_eq!(persisted.content, "Offline delivery");

        alpha.shutdown().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn edge_cases() {
    let _guard = suite_lock().lock().await;
    within_test_timeout("edge_cases", async {
        let alpha = TestNode::new().await;
        assert!(alpha
            .state
            .get_peers()
            .await
            .expect("load empty peers")
            .is_empty());
        assert!(alpha
            .state
            .get_transfers()
            .await
            .expect("load empty transfers")
            .is_empty());
        assert!(alpha
            .state
            .get_messages(Uuid::new_v4().to_string(), 50, 0)
            .await
            .expect("load empty messages")
            .is_empty());

        let beta = TestNode::new().await;
        let alpha_id = alpha.device_id();
        let beta_id = beta.device_id();

        let _ = tokio::join!(
            wait_for_peer(&alpha, beta_id.clone()),
            wait_for_peer(&beta, alpha_id.clone()),
        );

        let large_message = "L".repeat(10_000);
        let beta_message_cursor = beta.emitter.mark();
        let large_message_id = alpha
            .state
            .send_message(beta_id.clone(), large_message.clone(), None)
            .await
            .expect("send 10k message");

        let inbound_large = beta
            .emitter
            .wait_for_event(
                beta_message_cursor,
                "message-received",
                ACTION_TIMEOUT,
                |payload| {
                    payload.get("id").and_then(Value::as_str) == Some(large_message_id.as_str())
                },
            )
            .await;
        assert_eq!(
            inbound_large.get("content").and_then(Value::as_str),
            Some(large_message.as_str())
        );

        let sender_large = wait_for_message_status(
            &alpha,
            beta_id.clone(),
            large_message_id.clone(),
            "delivered".to_string(),
        )
        .await;
        let receiver_large = wait_for_message_status(
            &beta,
            alpha_id.clone(),
            large_message_id.clone(),
            "delivered".to_string(),
        )
        .await;
        assert_eq!(sender_large.content.len(), 10_000);
        assert_eq!(receiver_large.content, large_message);

        let persisted_large = wait_for_db_message(
            beta.database_path(),
            alpha_id.clone(),
            large_message_id,
            MessageStatus::Delivered,
        )
        .await;
        assert_eq!(persisted_large.content.len(), 10_000);

        time::sleep(Duration::from_millis(2)).await;
        let second_message_id = alpha
            .state
            .send_message(beta_id.clone(), "Edge case follow-up 1".to_string(), None)
            .await
            .expect("send second edge message");
        let sender_second = wait_for_message_status(
            &alpha,
            beta_id.clone(),
            second_message_id,
            "delivered".to_string(),
        )
        .await;

        time::sleep(Duration::from_millis(2)).await;
        let third_message_id = alpha
            .state
            .send_message(beta_id.clone(), "Edge case follow-up 2".to_string(), None)
            .await
            .expect("send third edge message");
        let sender_third = wait_for_message_status(
            &alpha,
            beta_id.clone(),
            third_message_id,
            "delivered".to_string(),
        )
        .await;

        assert!(sender_large.timestamp < sender_second.timestamp);
        assert!(sender_second.timestamp < sender_third.timestamp);

        let beta_offer_cursor = beta.emitter.mark();
        let mut transfer_ids = Vec::new();
        for index in 0..4 {
            let file_path = alpha
                .app_data_dir
                .join(format!("edge-case-{}.bin", index + 1));
            let file_size = 1024 * 1024 + (index * 1024);
            std::fs::write(&file_path, vec![index as u8; file_size])
                .expect("write edge case transfer payload");

            let transfer_id = alpha
                .state
                .send_file(beta_id.clone(), file_path.to_string_lossy().into_owned())
                .await
                .expect("queue edge case transfer");
            transfer_ids.push(transfer_id);
        }

        for transfer_id in transfer_ids.iter().take(3) {
            let offer = beta
                .emitter
                .wait_for_event(
                    beta_offer_cursor,
                    "file-offer-received",
                    ACTION_TIMEOUT,
                    |payload| {
                        payload.get("id").and_then(Value::as_str) == Some(transfer_id.as_str())
                    },
                )
                .await;
            assert_eq!(
                offer.get("senderId").and_then(Value::as_str),
                Some(alpha_id.as_str())
            );
        }

        let queued_transfer_id = transfer_ids[3].clone();
        let queued_snapshot =
            wait_for_transfer_mix(&alpha, 3, vec![queued_transfer_id.clone()]).await;
        let active_transfer_ids = queued_snapshot
            .iter()
            .filter(|transfer| transfer.state == "active")
            .map(|transfer| transfer.id.clone())
            .collect::<Vec<_>>();
        assert!(transfer_ids[..3]
            .iter()
            .all(|transfer_id| active_transfer_ids.contains(transfer_id)));

        for transfer_id in transfer_ids.iter().take(3) {
            beta.state
                .accept_file(transfer_id.clone())
                .await
                .expect("accept concurrent edge transfer");
        }

        let fourth_offer = beta
            .emitter
            .wait_for_event(
                beta_offer_cursor,
                "file-offer-received",
                ACTION_TIMEOUT,
                |payload| {
                    payload.get("id").and_then(Value::as_str) == Some(queued_transfer_id.as_str())
                },
            )
            .await;
        assert_eq!(
            fourth_offer.get("senderId").and_then(Value::as_str),
            Some(alpha_id.as_str())
        );

        let promoted_transfer =
            wait_for_transfer_state(&alpha, queued_transfer_id.clone(), "active".to_string()).await;
        assert_eq!(promoted_transfer.id, queued_transfer_id);

        beta.state
            .accept_file(queued_transfer_id.clone())
            .await
            .expect("accept promoted edge transfer");

        for transfer_id in transfer_ids {
            let (sender_transfer, receiver_transfer) = tokio::join!(
                wait_for_transfer_state(&alpha, transfer_id.clone(), "completed".to_string()),
                wait_for_transfer_state(&beta, transfer_id.clone(), "completed".to_string()),
            );
            assert_eq!(sender_transfer.progress, 1.0);
            assert_eq!(receiver_transfer.progress, 1.0);
        }

        beta.shutdown().await;
        time::sleep(Duration::from_millis(250)).await;

        let offline_message_id = alpha
            .state
            .send_message(beta_id.clone(), "Offline edge case".to_string(), None)
            .await
            .expect("send offline edge message");
        let failed =
            wait_for_message_status(&alpha, beta_id, offline_message_id, "failed".to_string())
                .await;
        assert_eq!(failed.status, "failed");

        alpha.shutdown().await;
    })
    .await;
}
