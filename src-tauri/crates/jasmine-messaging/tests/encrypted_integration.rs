use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use jasmine_core::{
    ChatId, Message, MessageStatus, ProtocolMessage, StorageEngine, UserId,
    CURRENT_PROTOCOL_VERSION,
};
use jasmine_crypto::{chunk_nonce, encrypt, public_key_to_base64, PublicKey, StaticSecret};
use jasmine_messaging::{
    ChatService, ChatServiceConfig, ChatServiceEvent, WsClient, WsClientConfig, WsClientEvent,
    WsPeerIdentity, WsServer, WsServerConfig,
};
use jasmine_storage::{ChatType, SqliteStorage};
use serde::Serialize;
use tempfile::TempDir;
use tokio::time;
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};
use uuid::Uuid;

const TEST_ACK_TIMEOUT: Duration = Duration::from_millis(250);
const GROUP_MESSAGE_AAD_PREFIX: &[u8] = b"jasmine/group-message/v1";

#[derive(Serialize)]
struct GroupMessageCiphertextPayload {
    id: String,
    content: String,
    timestamp: i64,
    reply_to_id: Option<String>,
    reply_to_preview: Option<String>,
}

struct Node {
    identity: WsPeerIdentity,
    _temp_dir: TempDir,
    storage: Arc<SqliteStorage>,
    server: Arc<WsServer>,
    chat: ChatService<SqliteStorage>,
}

impl Node {
    async fn new(seed: u128, display_name: &str) -> Self {
        let identity = peer(seed, display_name);
        let temp_dir = TempDir::new().expect("create temp dir");
        let storage = Arc::new(
            SqliteStorage::open(temp_dir.path().join("messages.sqlite3"))
                .expect("open sqlite storage"),
        );
        let server = Arc::new(
            WsServer::bind(server_config(identity.clone()))
                .await
                .expect("bind ws server"),
        );
        let chat = ChatService::with_config(
            identity.clone(),
            Arc::clone(&server),
            Arc::clone(&storage),
            ChatServiceConfig {
                ack_timeout: TEST_ACK_TIMEOUT,
            },
        );

        Self {
            identity,
            _temp_dir: temp_dir,
            storage,
            server,
            chat,
        }
    }

    async fn connect_to(&self, peer: &Self) -> Arc<WsClient> {
        let client = Arc::new(
            WsClient::connect(ws_url(&peer.server), client_config(self.identity.clone()))
                .await
                .expect("connect peer websocket client"),
        );
        self.chat
            .register_client(Arc::clone(&client))
            .await
            .expect("register chat client");
        client
    }

    async fn shutdown(self) {
        self.chat.shutdown().await.expect("shutdown chat service");
        self.server.shutdown().await.expect("shutdown ws server");
    }
}

fn peer(seed: u128, display_name: &str) -> WsPeerIdentity {
    let device_id = Uuid::from_u128(seed).to_string();
    let secret = StaticSecret::from(deterministic_private_key_bytes(&device_id));
    let public_key = PublicKey::from(&secret);
    WsPeerIdentity::new(device_id, display_name)
        .with_transport_identity(public_key_to_base64(&public_key), CURRENT_PROTOCOL_VERSION)
}

fn deterministic_private_key_bytes(device_id: &str) -> [u8; 32] {
    let mut bytes = [0_u8; 32];
    for (index, byte) in device_id.as_bytes().iter().enumerate() {
        let slot = index % 32;
        bytes[slot] = bytes[slot]
            .wrapping_add(*byte)
            .wrapping_add((index as u8).wrapping_mul(17));
    }
    if bytes.iter().all(|value| *value == 0) {
        bytes[0] = 1;
    }
    bytes
}

fn local_private_key(device_id: &str) -> Vec<u8> {
    StaticSecret::from(deterministic_private_key_bytes(device_id))
        .to_bytes()
        .to_vec()
}

fn server_config(identity: WsPeerIdentity) -> WsServerConfig {
    let mut config = WsServerConfig::new(identity);
    config.bind_addr = "127.0.0.1:0".to_string();
    config.local_private_key = Some(local_private_key(&config.local_peer.device_id));
    config.heartbeat_interval = Duration::from_millis(100);
    config.max_missed_heartbeats = 3;
    config.handshake_timeout = Duration::from_secs(1);
    config
}

fn client_config(identity: WsPeerIdentity) -> WsClientConfig {
    let mut config = WsClientConfig::new(identity);
    config.local_private_key = Some(local_private_key(&config.local_peer.device_id));
    config.heartbeat_interval = Duration::from_millis(100);
    config.max_missed_heartbeats = 3;
    config.handshake_timeout = Duration::from_secs(1);
    config
}

fn ws_url(server: &WsServer) -> String {
    format!("ws://{}", server.local_addr())
}

fn user_id_from_peer(peer_id: &str) -> UserId {
    UserId(Uuid::parse_str(peer_id).expect("peer id is uuid"))
}

async fn expect_chat_event(
    receiver: &mut tokio::sync::broadcast::Receiver<ChatServiceEvent>,
) -> ChatServiceEvent {
    time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .expect("chat event timeout")
        .expect("chat event channel closed")
}

async fn expect_message_event(
    receiver: &mut tokio::sync::broadcast::Receiver<ChatServiceEvent>,
) -> (String, Message) {
    time::timeout(Duration::from_secs(2), async {
        loop {
            match expect_chat_event(receiver).await {
                ChatServiceEvent::MessageReceived { peer_id, message } => {
                    return (peer_id, message)
                }
                _ => continue,
            }
        }
    })
    .await
    .expect("message event timeout")
}

async fn expect_client_event(
    receiver: &mut tokio::sync::broadcast::Receiver<WsClientEvent>,
) -> WsClientEvent {
    time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .expect("client event timeout")
        .expect("client event channel closed")
}

async fn expect_client_message<F>(
    receiver: &mut tokio::sync::broadcast::Receiver<WsClientEvent>,
    mut matcher: F,
) -> ProtocolMessage
where
    F: FnMut(&ProtocolMessage) -> bool,
{
    time::timeout(Duration::from_secs(2), async {
        loop {
            match expect_client_event(receiver).await {
                WsClientEvent::MessageReceived { message } if matcher(&message) => return message,
                WsClientEvent::MessageReceived { .. } => continue,
                WsClientEvent::Disconnected { reason } => {
                    panic!("client disconnected while waiting for matching message: {reason:?}")
                }
            }
        }
    })
    .await
    .expect("matching client event timeout")
}

async fn expect_no_chat_event(receiver: &mut tokio::sync::broadcast::Receiver<ChatServiceEvent>) {
    assert!(time::timeout(Duration::from_millis(200), receiver.recv())
        .await
        .is_err());
}

async fn wait_for_messages(
    chat: &ChatService<SqliteStorage>,
    chat_id: &ChatId,
    expected_len: usize,
) -> Vec<Message> {
    time::timeout(Duration::from_secs(2), async {
        loop {
            let messages = chat
                .load_messages(chat_id, 50, 0)
                .await
                .expect("load chat messages");
            if messages.len() >= expected_len {
                return messages;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("messages should be persisted")
}

async fn wait_for_direct_messages(
    chat: &ChatService<SqliteStorage>,
    peer_id: &str,
    expected_len: usize,
) -> Vec<Message> {
    let chat_id = ChatService::<SqliteStorage>::direct_chat_id(peer_id).expect("direct chat id");
    wait_for_messages(chat, &chat_id, expected_len).await
}

async fn wait_for_status(
    chat: &ChatService<SqliteStorage>,
    peer_id: &str,
    message_id: Uuid,
    expected_status: MessageStatus,
) -> Message {
    time::timeout(Duration::from_secs(2), async {
        loop {
            let messages = wait_for_direct_messages(chat, peer_id, 1).await;
            if let Some(message) = messages
                .into_iter()
                .find(|message| message.id == message_id)
            {
                if message.status == expected_status {
                    return message;
                }
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("message should reach expected status")
}

async fn wait_for_group(storage: &SqliteStorage, group_id: &ChatId) -> jasmine_core::GroupInfo {
    time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(chat) = storage.get_chat(group_id).await.expect("load group chat") {
                if chat.chat_type == ChatType::Group {
                    let members = storage
                        .get_chat_members(group_id)
                        .await
                        .expect("load group members");
                    return jasmine_core::GroupInfo {
                        id: chat.id,
                        name: chat.name.expect("group chat name"),
                        member_ids: members,
                    };
                }
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("group should be persisted")
}

async fn wait_for_group_members(storage: &SqliteStorage, group_id: &ChatId, expected: &[UserId]) {
    time::timeout(Duration::from_secs(2), async {
        loop {
            let members = storage
                .get_chat_members(group_id)
                .await
                .expect("load group members");
            if members == expected {
                return;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("group members should match expected set");
}

async fn wait_for_sender_key(
    storage: &SqliteStorage,
    group_id: &str,
    sender_device_id: &str,
) -> jasmine_core::SenderKeyInfo {
    time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(sender_key) = storage
                .get_sender_key(group_id, sender_device_id)
                .await
                .expect("load sender key")
            {
                return sender_key;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("sender key should be persisted")
}

async fn wait_for_sender_key_epoch(
    storage: &SqliteStorage,
    group_id: &str,
    sender_device_id: &str,
    epoch: u32,
) -> jasmine_core::SenderKeyInfo {
    time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(sender_key) = storage
                .get_sender_key_by_epoch(group_id, sender_device_id, epoch)
                .await
                .expect("load sender key by epoch")
            {
                return sender_key;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("sender key epoch should be persisted")
}

fn sender_key_material(sender_key: &jasmine_core::SenderKeyInfo) -> [u8; 32] {
    sender_key
        .key_data
        .as_slice()
        .try_into()
        .expect("sender key material should be 32 bytes")
}

fn group_message_aad(group_id: &str, sender_id: &str, sender_key_id: &Uuid, epoch: u32) -> Vec<u8> {
    let mut aad = Vec::with_capacity(
        GROUP_MESSAGE_AAD_PREFIX.len() + group_id.len() + sender_id.len() + 16 + 7,
    );
    aad.extend_from_slice(GROUP_MESSAGE_AAD_PREFIX);
    aad.push(0);
    aad.extend_from_slice(group_id.as_bytes());
    aad.push(0);
    aad.extend_from_slice(sender_id.as_bytes());
    aad.push(0);
    aad.extend_from_slice(sender_key_id.as_bytes());
    aad.extend_from_slice(&epoch.to_be_bytes());
    aad
}

fn encrypt_group_message_for_test(
    group_id: &ChatId,
    sender_id: &str,
    sender_key: &jasmine_core::SenderKeyInfo,
    chunk_index: u32,
    content: &str,
) -> ProtocolMessage {
    let payload = serde_json::to_vec(&GroupMessageCiphertextPayload {
        id: Uuid::new_v4().to_string(),
        content: content.to_string(),
        timestamp: 1_700_000_123_456,
        reply_to_id: None,
        reply_to_preview: None,
    })
    .expect("serialize test group message payload");
    let nonce = chunk_nonce(&[0u8; 8], chunk_index);
    let encrypted_content = encrypt(
        &sender_key_material(sender_key),
        &nonce,
        &payload,
        &group_message_aad(
            &group_id.0.to_string(),
            sender_id,
            &sender_key.key_id,
            sender_key.epoch,
        ),
    )
    .expect("encrypt test group message payload");

    ProtocolMessage::GroupMessage {
        group_id: group_id.0.to_string(),
        sender_key_id: sender_key.key_id,
        epoch: sender_key.epoch,
        nonce,
        encrypted_content,
    }
}

async fn legacy_protocol_rejection(
    server: &Arc<WsServer>,
    display_name: &str,
    protocol_version: u32,
) -> (ProtocolMessage, String) {
    let manual_id = Uuid::new_v4().to_string();
    let (_, public_key) = jasmine_crypto::generate_identity_keypair();
    let (mut socket, _) = connect_async(ws_url(server))
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

    let incompatibility = time::timeout(Duration::from_secs(2), async {
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
    .expect("legacy peer incompatibility message timeout");

    time::timeout(Duration::from_secs(2), async {
        loop {
            match socket.next().await {
                Some(Ok(WsMessage::Close(Some(frame)))) => {
                    return (incompatibility, frame.reason.to_string())
                }
                Some(Ok(_)) => continue,
                Some(Err(error)) => panic!("legacy websocket transport error: {error}"),
                None => panic!("legacy websocket closed without reason"),
            }
        }
    })
    .await
    .expect("legacy peer close reason timeout")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn encrypted_integration_direct_connect_send_receive_ack_roundtrip() {
    let alpha = Node::new(201, "Alpha Node").await;
    let beta = Node::new(202, "Beta Node").await;

    let _client = alpha.connect_to(&beta).await;
    let mut beta_events = beta.chat.subscribe();

    let sent = alpha
        .chat
        .send_message(&beta.identity.device_id, "hello encrypted direct")
        .await
        .expect("send direct message");

    let (peer_id, received) = expect_message_event(&mut beta_events).await;
    assert_eq!(peer_id, alpha.identity.device_id);
    assert_eq!(received.id, sent.id);
    assert_eq!(received.content, "hello encrypted direct");
    assert_eq!(
        received.chat_id,
        ChatService::<SqliteStorage>::direct_chat_id(&alpha.identity.device_id).unwrap()
    );

    let delivered = wait_for_status(
        &alpha.chat,
        &beta.identity.device_id,
        sent.id,
        MessageStatus::Delivered,
    )
    .await;
    assert_eq!(delivered.status, MessageStatus::Delivered);

    alpha.shutdown().await;
    beta.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn encrypted_integration_direct_multiple_messages_roundtrip_without_regression() {
    let alpha = Node::new(203, "Alpha Node").await;
    let beta = Node::new(204, "Beta Node").await;

    let _client = alpha.connect_to(&beta).await;
    let mut beta_events = beta.chat.subscribe();
    let contents = ["one", "two", "three", "four", "five"];

    let mut message_ids = Vec::new();
    for content in contents {
        let sent = alpha
            .chat
            .send_message(&beta.identity.device_id, content)
            .await
            .expect("send direct message in burst");
        message_ids.push((sent.id, content.to_string()));
    }

    let mut received_contents = Vec::new();
    for _ in 0..message_ids.len() {
        let (_, message) = expect_message_event(&mut beta_events).await;
        received_contents.push(message.content);
    }
    assert_eq!(
        received_contents,
        vec!["one", "two", "three", "four", "five"]
    );

    let stored =
        wait_for_direct_messages(&beta.chat, &alpha.identity.device_id, message_ids.len()).await;
    assert_eq!(stored.len(), message_ids.len());

    alpha.shutdown().await;
    beta.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn encrypted_integration_direct_unicode_and_long_content_roundtrip() {
    let alpha = Node::new(205, "Alpha Node").await;
    let beta = Node::new(206, "Beta Node").await;

    let _client = alpha.connect_to(&beta).await;
    let mut beta_events = beta.chat.subscribe();
    let content = format!("秘密メッセージ 🚀 — {}", "长内容".repeat(80));

    alpha
        .chat
        .send_message(&beta.identity.device_id, &content)
        .await
        .expect("send unicode direct message");

    let (_, message) = expect_message_event(&mut beta_events).await;
    assert_eq!(message.content, content);

    alpha.shutdown().await;
    beta.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn encrypted_integration_reconnect_establishes_new_session_and_messaging_continues() {
    let alpha = Node::new(207, "Alpha Node").await;
    let beta = Node::new(208, "Beta Node").await;

    let client = alpha.connect_to(&beta).await;
    let first_material = client.sender_key_distribution_material();
    let mut client_events = client.subscribe();
    client.disconnect().await.expect("disconnect first client");
    match expect_client_event(&mut client_events).await {
        WsClientEvent::Disconnected { .. } => {}
        other => panic!("expected disconnect event, got {other:?}"),
    }

    let reconnected = alpha.connect_to(&beta).await;
    let second_material = reconnected.sender_key_distribution_material();
    assert_ne!(first_material, second_material);

    let mut beta_events = beta.chat.subscribe();
    let sent = alpha
        .chat
        .send_message(&beta.identity.device_id, "after reconnect")
        .await
        .expect("send after reconnect");
    let (_, message) = expect_message_event(&mut beta_events).await;
    assert_eq!(message.id, sent.id);
    assert_eq!(message.content, "after reconnect");

    alpha.shutdown().await;
    beta.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn encrypted_integration_group_three_member_roundtrip() {
    let alpha = Node::new(209, "Alpha Node").await;
    let beta = Node::new(210, "Beta Node").await;
    let gamma = Node::new(211, "Gamma Node").await;

    let _alpha_to_beta = alpha.connect_to(&beta).await;
    let _alpha_to_gamma = alpha.connect_to(&gamma).await;

    let group = alpha
        .chat
        .create_group(
            "Project Room",
            vec![
                beta.identity.device_id.clone(),
                gamma.identity.device_id.clone(),
            ],
        )
        .await
        .expect("create group");

    wait_for_group(beta.storage.as_ref(), &group.id).await;
    wait_for_group(gamma.storage.as_ref(), &group.id).await;

    let mut beta_events = beta.chat.subscribe();
    let mut gamma_events = gamma.chat.subscribe();
    let sent = alpha
        .chat
        .send_to_group(&group.id, "hello encrypted group")
        .await
        .expect("send group message");

    let (beta_peer, beta_message) = expect_message_event(&mut beta_events).await;
    let (gamma_peer, gamma_message) = expect_message_event(&mut gamma_events).await;
    assert_eq!(beta_peer, alpha.identity.device_id);
    assert_eq!(gamma_peer, alpha.identity.device_id);
    assert_eq!(beta_message.id, sent.id);
    assert_eq!(gamma_message.id, sent.id);
    assert_eq!(beta_message.content, "hello encrypted group");
    assert_eq!(gamma_message.content, "hello encrypted group");

    alpha.shutdown().await;
    beta.shutdown().await;
    gamma.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn encrypted_integration_group_missing_sender_key_recovery() {
    let alpha = Node::new(212, "Alpha Node").await;
    let beta = Node::new(213, "Beta Node").await;

    let alpha_to_beta = alpha.connect_to(&beta).await;
    let mut alpha_client_events = alpha_to_beta.subscribe();
    let group = alpha
        .chat
        .create_group("Recovery Room", vec![beta.identity.device_id.clone()])
        .await
        .expect("create group");

    wait_for_group(beta.storage.as_ref(), &group.id).await;
    alpha
        .chat
        .send_to_group(&group.id, "prime sender key")
        .await
        .expect("send first group message");
    wait_for_messages(&beta.chat, &group.id, 1).await;

    beta.storage
        .delete_sender_keys_for_group(&group.id.0.to_string())
        .await
        .expect("delete sender keys");
    assert!(beta
        .storage
        .get_sender_key(&group.id.0.to_string(), &alpha.identity.device_id)
        .await
        .expect("confirm sender key deletion")
        .is_none());

    let mut beta_events = beta.chat.subscribe();
    alpha
        .chat
        .send_to_group(&group.id, "needs recovery")
        .await
        .expect("send group message needing recovery");

    let message = expect_client_message(&mut alpha_client_events, |message| {
        matches!(message, ProtocolMessage::SenderKeyRequest { .. })
    })
    .await;
    match message {
        ProtocolMessage::SenderKeyRequest {
            group_id,
            requesting_peer_id,
        } => {
            assert_eq!(group_id, group.id.0.to_string());
            assert_eq!(requesting_peer_id, beta.identity.device_id);
        }
        other => panic!("expected sender key request, got {other:?}"),
    }

    wait_for_sender_key(
        beta.storage.as_ref(),
        &group.id.0.to_string(),
        &alpha.identity.device_id,
    )
    .await;
    expect_no_chat_event(&mut beta_events).await;

    alpha
        .chat
        .send_to_group(&group.id, "after recovery")
        .await
        .expect("send group message after recovery");

    let (_, recovered) = expect_message_event(&mut beta_events).await;
    assert_eq!(recovered.content, "after recovery");

    alpha.shutdown().await;
    beta.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn encrypted_integration_sender_key_rotation_removed_member_loses_new_access() {
    let alpha = Node::new(214, "Alpha Node").await;
    let beta = Node::new(215, "Beta Node").await;
    let gamma = Node::new(216, "Gamma Node").await;

    let _alpha_to_beta = alpha.connect_to(&beta).await;
    let _alpha_to_gamma = alpha.connect_to(&gamma).await;

    let group = alpha
        .chat
        .create_group(
            "Rotation Room",
            vec![
                beta.identity.device_id.clone(),
                gamma.identity.device_id.clone(),
            ],
        )
        .await
        .expect("create group");
    wait_for_group(beta.storage.as_ref(), &group.id).await;
    wait_for_group(gamma.storage.as_ref(), &group.id).await;

    alpha
        .chat
        .send_to_group(&group.id, "before rotation")
        .await
        .expect("send pre-rotation group message");
    wait_for_messages(&beta.chat, &group.id, 1).await;
    wait_for_messages(&gamma.chat, &group.id, 1).await;

    let updated = alpha
        .chat
        .remove_group_members(&group.id, vec![beta.identity.device_id.clone()])
        .await
        .expect("remove beta from group");
    let expected_members = vec![
        user_id_from_peer(&alpha.identity.device_id),
        user_id_from_peer(&gamma.identity.device_id),
    ];
    assert_eq!(updated.member_ids, expected_members);

    let gamma_epoch_one = wait_for_sender_key_epoch(
        gamma.storage.as_ref(),
        &group.id.0.to_string(),
        &alpha.identity.device_id,
        1,
    )
    .await;
    assert!(beta
        .storage
        .get_sender_key_by_epoch(&group.id.0.to_string(), &alpha.identity.device_id, 1)
        .await
        .expect("lookup removed member epoch")
        .is_none());
    assert_eq!(gamma_epoch_one.epoch, 1);

    let mut gamma_events = gamma.chat.subscribe();
    let mut beta_events = beta.chat.subscribe();
    alpha
        .chat
        .send_to_group(&group.id, "after rotation")
        .await
        .expect("send post-rotation group message");

    let (_, gamma_message) = expect_message_event(&mut gamma_events).await;
    assert_eq!(gamma_message.content, "after rotation");
    expect_no_chat_event(&mut beta_events).await;

    alpha.shutdown().await;
    beta.shutdown().await;
    gamma.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn encrypted_integration_historical_old_epoch_remains_decryptable() {
    let alpha = Node::new(217, "Alpha Node").await;
    let beta = Node::new(218, "Beta Node").await;
    let gamma = Node::new(219, "Gamma Node").await;

    let _alpha_to_beta = alpha.connect_to(&beta).await;
    let alpha_to_gamma = alpha.connect_to(&gamma).await;

    let group = alpha
        .chat
        .create_group(
            "History Room",
            vec![
                beta.identity.device_id.clone(),
                gamma.identity.device_id.clone(),
            ],
        )
        .await
        .expect("create group");
    wait_for_group(beta.storage.as_ref(), &group.id).await;
    wait_for_group(gamma.storage.as_ref(), &group.id).await;

    alpha
        .chat
        .send_to_group(&group.id, "before rotation")
        .await
        .expect("send initial group message");
    wait_for_messages(&gamma.chat, &group.id, 1).await;
    let historical_sender_key = wait_for_sender_key(
        alpha.storage.as_ref(),
        &group.id.0.to_string(),
        &alpha.identity.device_id,
    )
    .await;

    alpha
        .chat
        .remove_group_members(&group.id, vec![beta.identity.device_id.clone()])
        .await
        .expect("remove beta to trigger rotation");
    wait_for_group_members(
        gamma.storage.as_ref(),
        &group.id,
        &[
            user_id_from_peer(&alpha.identity.device_id),
            user_id_from_peer(&gamma.identity.device_id),
        ],
    )
    .await;
    let latest_sender_key = wait_for_sender_key_epoch(
        gamma.storage.as_ref(),
        &group.id.0.to_string(),
        &alpha.identity.device_id,
        1,
    )
    .await;
    assert_eq!(latest_sender_key.epoch, 1);

    let mut gamma_events = gamma.chat.subscribe();
    alpha_to_gamma
        .send(encrypt_group_message_for_test(
            &group.id,
            &alpha.identity.device_id,
            &historical_sender_key,
            42,
            "historical payload",
        ))
        .await
        .expect("send historical epoch message directly");

    let (_, historical) = expect_message_event(&mut gamma_events).await;
    assert_eq!(historical.content, "historical payload");

    alpha.shutdown().await;
    beta.shutdown().await;
    gamma.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn encrypted_integration_invited_member_receives_current_sender_key_and_new_message() {
    let alpha = Node::new(220, "Alpha Node").await;
    let beta = Node::new(221, "Beta Node").await;
    let gamma = Node::new(222, "Gamma Node").await;

    let _alpha_to_beta = alpha.connect_to(&beta).await;

    let group = alpha
        .chat
        .create_group("Writers", vec![beta.identity.device_id.clone()])
        .await
        .expect("create initial group");
    wait_for_group(beta.storage.as_ref(), &group.id).await;

    alpha
        .chat
        .send_to_group(&group.id, "before invite")
        .await
        .expect("send pre-invite message");
    wait_for_messages(&beta.chat, &group.id, 1).await;

    let _alpha_to_gamma = alpha.connect_to(&gamma).await;
    let mut gamma_events = gamma.chat.subscribe();
    alpha
        .chat
        .add_group_members(&group.id, vec![gamma.identity.device_id.clone()])
        .await
        .expect("invite gamma");

    wait_for_group(gamma.storage.as_ref(), &group.id).await;
    wait_for_sender_key(
        gamma.storage.as_ref(),
        &group.id.0.to_string(),
        &alpha.identity.device_id,
    )
    .await;

    alpha
        .chat
        .send_to_group(&group.id, "after invite")
        .await
        .expect("send post-invite message");

    let (_, message) = expect_message_event(&mut gamma_events).await;
    assert_eq!(message.content, "after invite");

    alpha.shutdown().await;
    beta.shutdown().await;
    gamma.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn encrypted_integration_legacy_protocol_peer_is_rejected() {
    let alpha = Node::new(223, "Alpha Node").await;
    let beta = Node::new(224, "Beta Node").await;

    let (message, reason) = legacy_protocol_rejection(&alpha.server, "Legacy Peer", 1).await;
    assert_eq!(
        message,
        ProtocolMessage::VersionIncompatible {
            local_version: CURRENT_PROTOCOL_VERSION,
            remote_version: 1,
            message: "peer protocol version 1 is incompatible; minimum supported version is 2"
                .to_string(),
        }
    );
    assert_eq!(reason, "protocol version incompatible");

    alpha.shutdown().await;
    beta.shutdown().await;
}
