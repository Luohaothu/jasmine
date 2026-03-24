use std::sync::Arc;
use std::time::Duration;

use jasmine_core::{
    ChatId, GroupInfo, Message, ProtocolMessage, StorageEngine, UserId, CURRENT_PROTOCOL_VERSION,
};
use jasmine_crypto::{chunk_nonce, encrypt, public_key_to_base64, PublicKey, StaticSecret};
use jasmine_messaging::{
    ChatService, ChatServiceConfig, ChatServiceEvent, WsClient, WsClientConfig, WsClientEvent,
    WsPeerIdentity, WsServer, WsServerConfig, WsServerEvent,
};
use jasmine_storage::{ChatType, SqliteStorage};
use serde::Serialize;
use tempfile::TempDir;
use tokio::time;
use uuid::Uuid;

const TEST_ACK_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Serialize)]
struct GroupMessageCiphertextPayload {
    id: String,
    content: String,
    timestamp: i64,
    reply_to_id: Option<String>,
    reply_to_preview: Option<String>,
}

const GROUP_MESSAGE_AAD_PREFIX: &[u8] = b"jasmine/group-message/v1";

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

async fn expect_server_event(
    receiver: &mut tokio::sync::broadcast::Receiver<WsServerEvent>,
) -> WsServerEvent {
    time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .expect("server event timeout")
        .expect("server event channel closed")
}

async fn expect_chat_event(
    receiver: &mut tokio::sync::broadcast::Receiver<ChatServiceEvent>,
) -> ChatServiceEvent {
    time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .expect("chat event timeout")
        .expect("chat event channel closed")
}

async fn expect_group_message_event(
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
    .expect("group message event timeout")
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
                WsClientEvent::CallSignalingReceived { .. } => continue,
                WsClientEvent::Disconnected { reason } => {
                    panic!("client disconnected while waiting for matching message: {reason:?}")
                }
            }
        }
    })
    .await
    .expect("matching client message timeout")
}

async fn expect_no_chat_event(receiver: &mut tokio::sync::broadcast::Receiver<ChatServiceEvent>) {
    assert!(time::timeout(Duration::from_millis(200), async {
        loop {
            match receiver.recv().await {
                Ok(ChatServiceEvent::MessageReceived { .. }) => {
                    panic!("unexpected group message event")
                }
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    })
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
                .load_messages(chat_id, 20, 0)
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

async fn wait_for_group(storage: &SqliteStorage, group_id: &ChatId) -> GroupInfo {
    time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(chat) = storage.get_chat(group_id).await.expect("load group chat") {
                if chat.chat_type == ChatType::Group {
                    let members = storage
                        .get_chat_members(group_id)
                        .await
                        .expect("load group members");
                    return GroupInfo {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn group_create_persists_members_and_fanouts_group_create() {
    let alpha = Node::new(100, "Alpha Node").await;
    let beta = Node::new(101, "Beta Node").await;
    let gamma = Node::new(102, "Gamma Node").await;

    let _alpha_to_beta = alpha.connect_to(&beta).await;
    let _alpha_to_gamma = alpha.connect_to(&gamma).await;

    let mut beta_server_events = beta.server.subscribe();
    let mut gamma_server_events = gamma.server.subscribe();

    let group = alpha
        .chat
        .create_group(
            "Study Group",
            vec![
                beta.identity.device_id.clone(),
                gamma.identity.device_id.clone(),
            ],
        )
        .await
        .expect("create group");

    let expected_members = vec![
        user_id_from_peer(&alpha.identity.device_id),
        user_id_from_peer(&beta.identity.device_id),
        user_id_from_peer(&gamma.identity.device_id),
    ];

    assert_eq!(group.name, "Study Group");
    assert_eq!(group.member_ids, expected_members);

    match expect_server_event(&mut beta_server_events).await {
        WsServerEvent::MessageReceived { peer, message } => {
            assert_eq!(peer.identity.device_id, alpha.identity.device_id);
            assert_eq!(
                message,
                ProtocolMessage::GroupCreate {
                    group_id: group.id.0.to_string(),
                    name: "Study Group".to_string(),
                    members: vec![
                        alpha.identity.device_id.clone(),
                        beta.identity.device_id.clone(),
                        gamma.identity.device_id.clone(),
                    ],
                }
            );
        }
        other => panic!("expected group create for beta, got {other:?}"),
    }

    match expect_server_event(&mut gamma_server_events).await {
        WsServerEvent::MessageReceived { peer, message } => {
            assert_eq!(peer.identity.device_id, alpha.identity.device_id);
            assert_eq!(
                message,
                ProtocolMessage::GroupCreate {
                    group_id: group.id.0.to_string(),
                    name: "Study Group".to_string(),
                    members: vec![
                        alpha.identity.device_id.clone(),
                        beta.identity.device_id.clone(),
                        gamma.identity.device_id.clone(),
                    ],
                }
            );
        }
        other => panic!("expected group create for gamma, got {other:?}"),
    }

    let alpha_group = wait_for_group(alpha.storage.as_ref(), &group.id).await;
    let beta_group = wait_for_group(beta.storage.as_ref(), &group.id).await;
    let gamma_group = wait_for_group(gamma.storage.as_ref(), &group.id).await;

    assert_eq!(alpha_group, group);
    assert_eq!(beta_group, group);
    assert_eq!(gamma_group, group);

    alpha.shutdown().await;
    beta.shutdown().await;
    gamma.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn group_encrypted_first_send_fans_out_group_message_protocol_variant() {
    let alpha = Node::new(109, "Alpha Node").await;
    let beta = Node::new(119, "Beta Node").await;

    let _alpha_to_beta = alpha.connect_to(&beta).await;

    let mut beta_server_events = beta.server.subscribe();
    let group = alpha
        .chat
        .create_group("Encrypted Room", vec![beta.identity.device_id.clone()])
        .await
        .expect("create group");

    match expect_server_event(&mut beta_server_events).await {
        WsServerEvent::MessageReceived { message, .. } => match message {
            ProtocolMessage::GroupCreate { group_id, .. } => {
                assert_eq!(group_id, group.id.0.to_string())
            }
            other => panic!("expected group create, got {other:?}"),
        },
        other => panic!("expected group create event, got {other:?}"),
    }

    alpha
        .chat
        .send_to_group(&group.id, "ciphertext fanout")
        .await
        .expect("send encrypted group message");

    match expect_server_event(&mut beta_server_events).await {
        WsServerEvent::MessageReceived { peer, message } => {
            assert_eq!(peer.identity.device_id, alpha.identity.device_id);
            match message {
                ProtocolMessage::SenderKeyDistribution {
                    group_id,
                    sender_key_data,
                    epoch,
                } => {
                    assert_eq!(group_id, group.id.0.to_string());
                    assert_eq!(epoch, 0);
                    assert!(!sender_key_data.is_empty());
                }
                other => panic!("expected sender key distribution, got {other:?}"),
            }
        }
        other => panic!("expected sender key distribution event, got {other:?}"),
    }

    match expect_server_event(&mut beta_server_events).await {
        WsServerEvent::MessageReceived { peer, message } => {
            assert_eq!(peer.identity.device_id, alpha.identity.device_id);
            match message {
                ProtocolMessage::GroupMessage {
                    group_id,
                    sender_key_id,
                    epoch,
                    nonce,
                    encrypted_content,
                } => {
                    assert_eq!(group_id, group.id.0.to_string());
                    assert_eq!(epoch, 0);
                    assert_ne!(sender_key_id, Uuid::nil());
                    assert_eq!(nonce.len(), 12);
                    assert!(!encrypted_content.is_empty());
                }
                other => panic!("expected encrypted group message, got {other:?}"),
            }
        }
        other => panic!("expected encrypted group message event, got {other:?}"),
    }

    let stored_sender_key = wait_for_sender_key(
        beta.storage.as_ref(),
        &group.id.0.to_string(),
        &alpha.identity.device_id,
    )
    .await;
    assert_eq!(stored_sender_key.group_id, group.id.0.to_string());
    assert_eq!(stored_sender_key.sender_device_id, alpha.identity.device_id);
    assert_eq!(stored_sender_key.epoch, 0);

    alpha.shutdown().await;
    beta.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn group_encrypted_nonce_progression_uses_distinct_monotonic_nonces() {
    let alpha = Node::new(108, "Alpha Node").await;
    let beta = Node::new(118, "Beta Node").await;

    let _alpha_to_beta = alpha.connect_to(&beta).await;
    let mut beta_server_events = beta.server.subscribe();
    let mut beta_chat_events = beta.chat.subscribe();
    let group = alpha
        .chat
        .create_group("Nonce Room", vec![beta.identity.device_id.clone()])
        .await
        .expect("create group");

    match expect_server_event(&mut beta_server_events).await {
        WsServerEvent::MessageReceived { message, .. } => match message {
            ProtocolMessage::GroupCreate { group_id, .. } => {
                assert_eq!(group_id, group.id.0.to_string())
            }
            other => panic!("expected group create, got {other:?}"),
        },
        other => panic!("expected group create event, got {other:?}"),
    }

    alpha
        .chat
        .send_to_group(&group.id, "nonce one")
        .await
        .expect("send first encrypted group message");

    match expect_server_event(&mut beta_server_events).await {
        WsServerEvent::MessageReceived { message, .. } => {
            assert!(matches!(
                message,
                ProtocolMessage::SenderKeyDistribution { .. }
            ))
        }
        other => panic!("expected sender key distribution event, got {other:?}"),
    }

    let (first_sender_key_id, first_epoch, first_nonce) =
        match expect_server_event(&mut beta_server_events).await {
            WsServerEvent::MessageReceived { message, .. } => match message {
                ProtocolMessage::GroupMessage {
                    sender_key_id,
                    epoch,
                    nonce,
                    ..
                } => (sender_key_id, epoch, nonce),
                other => panic!("expected first encrypted group message, got {other:?}"),
            },
            other => panic!("expected first encrypted group message event, got {other:?}"),
        };

    let (_, first_message) = expect_group_message_event(&mut beta_chat_events).await;
    assert_eq!(first_message.content, "nonce one");

    alpha
        .chat
        .send_to_group(&group.id, "nonce two")
        .await
        .expect("send second encrypted group message");

    let (second_sender_key_id, second_epoch, second_nonce) =
        match expect_server_event(&mut beta_server_events).await {
            WsServerEvent::MessageReceived { message, .. } => match message {
                ProtocolMessage::GroupMessage {
                    sender_key_id,
                    epoch,
                    nonce,
                    ..
                } => (sender_key_id, epoch, nonce),
                other => panic!("expected second encrypted group message, got {other:?}"),
            },
            other => panic!("expected second encrypted group message event, got {other:?}"),
        };

    let (_, second_message) = expect_group_message_event(&mut beta_chat_events).await;
    assert_eq!(second_message.content, "nonce two");

    assert_eq!(first_sender_key_id, second_sender_key_id);
    assert_eq!(first_epoch, 0);
    assert_eq!(second_epoch, 0);
    assert_ne!(first_nonce, second_nonce);
    assert_eq!(
        u32::from_be_bytes(first_nonce[8..12].try_into().expect("first nonce counter")),
        0
    );
    assert_eq!(
        u32::from_be_bytes(
            second_nonce[8..12]
                .try_into()
                .expect("second nonce counter")
        ),
        1
    );

    alpha.shutdown().await;
    beta.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn group_encrypted_epoch_lookup_uses_requested_epoch_not_latest() {
    let alpha = Node::new(107, "Alpha Node").await;
    let beta = Node::new(117, "Beta Node").await;

    let _alpha_to_beta = alpha.connect_to(&beta).await;
    let mut beta_chat_events = beta.chat.subscribe();
    let group = alpha
        .chat
        .create_group("Epoch Room", vec![beta.identity.device_id.clone()])
        .await
        .expect("create group");

    wait_for_group(beta.storage.as_ref(), &group.id).await;
    alpha
        .chat
        .send_to_group(&group.id, "prime sender key")
        .await
        .expect("send first encrypted group message");
    wait_for_sender_key(
        beta.storage.as_ref(),
        &group.id.0.to_string(),
        &alpha.identity.device_id,
    )
    .await;
    let (priming_peer_id, priming_message) =
        expect_group_message_event(&mut beta_chat_events).await;
    assert_eq!(priming_peer_id, alpha.identity.device_id);
    assert_eq!(priming_message.chat_id, group.id);
    assert_eq!(priming_message.content, "prime sender key");

    let bogus_epoch_one_key_id = Uuid::new_v4();
    beta.storage
        .save_sender_key(
            &group.id.0.to_string(),
            &alpha.identity.device_id,
            &bogus_epoch_one_key_id,
            &[0x99; 32],
            1,
            1_700_000_000_500,
        )
        .await
        .expect("save newer bogus sender key epoch");

    let latest_sender_key = beta
        .storage
        .get_sender_key(&group.id.0.to_string(), &alpha.identity.device_id)
        .await
        .expect("load latest sender key")
        .expect("latest sender key exists");
    assert_eq!(latest_sender_key.epoch, 1);
    assert_eq!(latest_sender_key.key_id, bogus_epoch_one_key_id);

    alpha
        .chat
        .send_to_group(&group.id, "decrypt with epoch zero")
        .await
        .expect("send encrypted group message with historical epoch");

    let (peer_id, message) = expect_group_message_event(&mut beta_chat_events).await;
    assert_eq!(peer_id, alpha.identity.device_id);
    assert_eq!(message.chat_id, group.id);
    assert_eq!(message.content, "decrypt with epoch zero");

    let latest_sender_key_after = beta
        .storage
        .get_sender_key(&group.id.0.to_string(), &alpha.identity.device_id)
        .await
        .expect("reload latest sender key")
        .expect("latest sender key still exists");
    assert_eq!(latest_sender_key_after.epoch, 1);
    assert_eq!(latest_sender_key_after.key_id, bogus_epoch_one_key_id);

    alpha.shutdown().await;
    beta.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn group_missing_sender_key_requests_distribution_and_recovers() {
    let alpha = Node::new(106, "Alpha Node").await;
    let beta = Node::new(116, "Beta Node").await;

    let alpha_to_beta = alpha.connect_to(&beta).await;
    let mut alpha_client_events = alpha_to_beta.subscribe();
    let mut beta_server_events = beta.server.subscribe();
    let mut beta_chat_events = beta.chat.subscribe();
    let group = alpha
        .chat
        .create_group("Recovery Room", vec![beta.identity.device_id.clone()])
        .await
        .expect("create group");

    match expect_server_event(&mut beta_server_events).await {
        WsServerEvent::MessageReceived { message, .. } => match message {
            ProtocolMessage::GroupCreate { group_id, .. } => {
                assert_eq!(group_id, group.id.0.to_string())
            }
            other => panic!("expected group create, got {other:?}"),
        },
        other => panic!("expected group create event, got {other:?}"),
    }

    alpha
        .chat
        .send_to_group(&group.id, "prime sender key")
        .await
        .expect("send first encrypted group message");

    match expect_server_event(&mut beta_server_events).await {
        WsServerEvent::MessageReceived { message, .. } => {
            assert!(matches!(
                message,
                ProtocolMessage::SenderKeyDistribution { .. }
            ))
        }
        other => panic!("expected sender key distribution event, got {other:?}"),
    }
    match expect_server_event(&mut beta_server_events).await {
        WsServerEvent::MessageReceived { message, .. } => {
            assert!(matches!(message, ProtocolMessage::GroupMessage { .. }))
        }
        other => panic!("expected initial encrypted group message event, got {other:?}"),
    }
    let (_, initial_message) = expect_group_message_event(&mut beta_chat_events).await;
    assert_eq!(initial_message.content, "prime sender key");

    beta.storage
        .delete_sender_keys_for_group(&group.id.0.to_string())
        .await
        .expect("delete receiver sender keys");
    assert!(beta
        .storage
        .get_sender_key(&group.id.0.to_string(), &alpha.identity.device_id)
        .await
        .expect("confirm sender key deletion")
        .is_none());

    alpha
        .chat
        .send_to_group(&group.id, "missing key should trigger recovery")
        .await
        .expect("send group message without receiver sender key");

    match expect_server_event(&mut beta_server_events).await {
        WsServerEvent::MessageReceived { message, .. } => {
            assert!(matches!(message, ProtocolMessage::GroupMessage { .. }))
        }
        other => panic!("expected encrypted group message needing recovery, got {other:?}"),
    }

    match expect_client_message(&mut alpha_client_events, |message| {
        matches!(message, ProtocolMessage::SenderKeyRequest { .. })
    })
    .await
    {
        ProtocolMessage::SenderKeyRequest {
            group_id,
            requesting_peer_id,
        } => {
            assert_eq!(group_id, group.id.0.to_string());
            assert_eq!(requesting_peer_id, beta.identity.device_id);
        }
        other => panic!("expected sender key request, got {other:?}"),
    }

    match expect_server_event(&mut beta_server_events).await {
        WsServerEvent::MessageReceived { message, .. } => match message {
            ProtocolMessage::SenderKeyDistribution { group_id, .. } => {
                assert_eq!(group_id, group.id.0.to_string())
            }
            other => panic!("expected sender key re-distribution, got {other:?}"),
        },
        other => panic!("expected sender key re-distribution event, got {other:?}"),
    }

    wait_for_sender_key(
        beta.storage.as_ref(),
        &group.id.0.to_string(),
        &alpha.identity.device_id,
    )
    .await;
    assert!(
        time::timeout(
            Duration::from_millis(200),
            expect_group_message_event(&mut beta_chat_events)
        )
        .await
        .is_err(),
        "beta should not receive post-removal decrypted group messages"
    );

    alpha
        .chat
        .send_to_group(&group.id, "after recovery")
        .await
        .expect("send group message after key recovery");

    match expect_server_event(&mut beta_server_events).await {
        WsServerEvent::MessageReceived { message, .. } => {
            assert!(matches!(message, ProtocolMessage::GroupMessage { .. }))
        }
        other => panic!("expected encrypted group message after recovery, got {other:?}"),
    }
    let (peer_id, recovered_message) = expect_group_message_event(&mut beta_chat_events).await;
    assert_eq!(peer_id, alpha.identity.device_id);
    assert_eq!(recovered_message.chat_id, group.id);
    assert_eq!(recovered_message.content, "after recovery");

    alpha.shutdown().await;
    beta.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn sender_key_rotation_on_member_removal_rotates_epoch_and_excludes_removed_member() {
    let alpha = Node::new(150, "Alpha Node").await;
    let beta = Node::new(151, "Beta Node").await;
    let gamma = Node::new(152, "Gamma Node").await;

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
        .expect("create rotation group");

    wait_for_group(beta.storage.as_ref(), &group.id).await;
    wait_for_group(gamma.storage.as_ref(), &group.id).await;

    alpha
        .chat
        .send_to_group(&group.id, "before rotation")
        .await
        .expect("send initial group message");
    wait_for_messages(&beta.chat, &group.id, 1).await;
    wait_for_messages(&gamma.chat, &group.id, 1).await;

    let alpha_epoch_zero = wait_for_sender_key(
        alpha.storage.as_ref(),
        &group.id.0.to_string(),
        &alpha.identity.device_id,
    )
    .await;
    let gamma_epoch_zero = wait_for_sender_key(
        gamma.storage.as_ref(),
        &group.id.0.to_string(),
        &alpha.identity.device_id,
    )
    .await;
    let beta_epoch_zero = wait_for_sender_key(
        beta.storage.as_ref(),
        &group.id.0.to_string(),
        &alpha.identity.device_id,
    )
    .await;
    assert_eq!(alpha_epoch_zero.epoch, 0);
    assert_eq!(gamma_epoch_zero.epoch, 0);
    assert_eq!(beta_epoch_zero.epoch, 0);

    let mut beta_server_events = beta.server.subscribe();
    let mut gamma_server_events = gamma.server.subscribe();
    let mut gamma_chat_events = gamma.chat.subscribe();
    let mut beta_chat_events = beta.chat.subscribe();

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

    wait_for_group_members(alpha.storage.as_ref(), &group.id, &expected_members).await;
    wait_for_group_members(beta.storage.as_ref(), &group.id, &expected_members).await;
    wait_for_group_members(gamma.storage.as_ref(), &group.id, &expected_members).await;

    match expect_server_event(&mut gamma_server_events).await {
        WsServerEvent::MessageReceived { message, .. } => {
            assert!(matches!(
                message,
                ProtocolMessage::SenderKeyDistribution { epoch: 1, .. }
            ))
        }
        other => {
            panic!("expected rotated sender key distribution for remaining member, got {other:?}")
        }
    }
    match expect_server_event(&mut gamma_server_events).await {
        WsServerEvent::MessageReceived { message, .. } => match message {
            ProtocolMessage::GroupCreate {
                group_id, members, ..
            } => {
                assert_eq!(group_id, group.id.0.to_string());
                assert_eq!(members.len(), 2);
                assert!(!members.contains(&beta.identity.device_id));
            }
            other => panic!("expected updated group snapshot for remaining member, got {other:?}"),
        },
        other => {
            panic!("expected updated group snapshot event for remaining member, got {other:?}")
        }
    }

    match expect_server_event(&mut beta_server_events).await {
        WsServerEvent::MessageReceived { message, .. } => match message {
            ProtocolMessage::GroupCreate {
                group_id, members, ..
            } => {
                assert_eq!(group_id, group.id.0.to_string());
                assert_eq!(members.len(), 2);
                assert!(!members.contains(&beta.identity.device_id));
            }
            other => panic!("expected updated group snapshot for removed member, got {other:?}"),
        },
        other => panic!("expected updated group snapshot event for removed member, got {other:?}"),
    }

    let alpha_epoch_one = wait_for_sender_key_epoch(
        alpha.storage.as_ref(),
        &group.id.0.to_string(),
        &alpha.identity.device_id,
        1,
    )
    .await;
    let gamma_epoch_one = wait_for_sender_key_epoch(
        gamma.storage.as_ref(),
        &group.id.0.to_string(),
        &alpha.identity.device_id,
        1,
    )
    .await;
    assert_eq!(alpha_epoch_one.epoch, 1);
    assert_eq!(gamma_epoch_one.epoch, 1);
    assert_ne!(alpha_epoch_one.key_id, alpha_epoch_zero.key_id);
    assert_ne!(alpha_epoch_one.key_data, alpha_epoch_zero.key_data);
    assert_eq!(gamma_epoch_one.key_id, alpha_epoch_one.key_id);
    assert!(beta
        .storage
        .get_sender_key_by_epoch(&group.id.0.to_string(), &alpha.identity.device_id, 1)
        .await
        .expect("lookup removed member latest sender key epoch")
        .is_none());
    assert!(gamma
        .storage
        .get_sender_key_by_epoch(&group.id.0.to_string(), &alpha.identity.device_id, 0)
        .await
        .expect("lookup old sender key for remaining member")
        .is_some());

    alpha
        .chat
        .send_to_group(&group.id, "after rotation")
        .await
        .expect("send rotated group message");

    let (peer_id, message) = expect_group_message_event(&mut gamma_chat_events).await;
    assert_eq!(peer_id, alpha.identity.device_id);
    assert_eq!(message.content, "after rotation");
    assert_eq!(message.chat_id, group.id);
    expect_no_chat_event(&mut beta_chat_events).await;
    let beta_messages = beta
        .chat
        .load_messages(&group.id, 20, 0)
        .await
        .expect("load removed member messages after rotation");
    assert_eq!(beta_messages.len(), 1);
    assert_eq!(beta_messages[0].content, "before rotation");

    alpha.shutdown().await;
    beta.shutdown().await;
    gamma.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn historical_old_epoch_group_message_remains_decryptable_after_rotation() {
    let alpha = Node::new(160, "Alpha Node").await;
    let beta = Node::new(161, "Beta Node").await;
    let gamma = Node::new(162, "Gamma Node").await;

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
        .expect("create historical group");

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
    assert_eq!(historical_sender_key.epoch, 0);

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
    assert!(gamma
        .storage
        .get_sender_key_by_epoch(&group.id.0.to_string(), &alpha.identity.device_id, 0)
        .await
        .expect("lookup historical sender key")
        .is_some());

    let mut gamma_chat_events = gamma.chat.subscribe();
    alpha_to_gamma
        .send(encrypt_group_message_for_test(
            &group.id,
            &alpha.identity.device_id,
            &historical_sender_key,
            42,
            "historical payload after rotation",
        ))
        .await
        .expect("send historical epoch encrypted group message directly");

    match expect_chat_event(&mut gamma_chat_events).await {
        ChatServiceEvent::MessageReceived { peer_id, message } => {
            assert_eq!(peer_id, alpha.identity.device_id);
            assert_eq!(message.chat_id, group.id);
            assert_eq!(message.content, "historical payload after rotation");
        }
        other => panic!("expected historical decrypted group message, got {other:?}"),
    }

    alpha.shutdown().await;
    beta.shutdown().await;
    gamma.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn group_member_left_voluntary_leave_rotates_remaining_member_sender_keys() {
    let alpha = Node::new(170, "Alpha Node").await;
    let beta = Node::new(171, "Beta Node").await;
    let gamma = Node::new(172, "Gamma Node").await;

    let _alpha_to_beta = alpha.connect_to(&beta).await;
    let _alpha_to_gamma = alpha.connect_to(&gamma).await;
    let _beta_to_gamma = beta.connect_to(&gamma).await;

    let group = alpha
        .chat
        .create_group(
            "Leave Rotation Room",
            vec![
                beta.identity.device_id.clone(),
                gamma.identity.device_id.clone(),
            ],
        )
        .await
        .expect("create leave-rotation group");

    wait_for_group(beta.storage.as_ref(), &group.id).await;
    wait_for_group(gamma.storage.as_ref(), &group.id).await;

    alpha
        .chat
        .send_to_group(&group.id, "alpha primes sender key")
        .await
        .expect("send alpha pre-leave group message");
    let alpha_epoch_zero = wait_for_sender_key(
        alpha.storage.as_ref(),
        &group.id.0.to_string(),
        &alpha.identity.device_id,
    )
    .await;
    wait_for_sender_key(
        beta.storage.as_ref(),
        &group.id.0.to_string(),
        &alpha.identity.device_id,
    )
    .await;
    wait_for_sender_key(
        gamma.storage.as_ref(),
        &group.id.0.to_string(),
        &alpha.identity.device_id,
    )
    .await;

    gamma
        .chat
        .send_to_group(&group.id, "gamma primes sender key")
        .await
        .expect("send gamma pre-leave group message");
    wait_for_sender_key(
        alpha.storage.as_ref(),
        &group.id.0.to_string(),
        &gamma.identity.device_id,
    )
    .await;
    wait_for_sender_key(
        beta.storage.as_ref(),
        &group.id.0.to_string(),
        &gamma.identity.device_id,
    )
    .await;
    let gamma_epoch_zero = wait_for_sender_key(
        gamma.storage.as_ref(),
        &group.id.0.to_string(),
        &gamma.identity.device_id,
    )
    .await;

    let updated = beta
        .chat
        .remove_group_members(&group.id, vec![beta.identity.device_id.clone()])
        .await
        .expect("beta leaves group");
    let expected_members = vec![
        user_id_from_peer(&alpha.identity.device_id),
        user_id_from_peer(&gamma.identity.device_id),
    ];
    assert_eq!(updated.member_ids, expected_members);

    wait_for_group_members(alpha.storage.as_ref(), &group.id, &expected_members).await;
    wait_for_group_members(beta.storage.as_ref(), &group.id, &expected_members).await;
    wait_for_group_members(gamma.storage.as_ref(), &group.id, &expected_members).await;

    let alpha_epoch_one = wait_for_sender_key_epoch(
        alpha.storage.as_ref(),
        &group.id.0.to_string(),
        &alpha.identity.device_id,
        1,
    )
    .await;
    let gamma_received_alpha_epoch_one = wait_for_sender_key_epoch(
        gamma.storage.as_ref(),
        &group.id.0.to_string(),
        &alpha.identity.device_id,
        1,
    )
    .await;
    let gamma_epoch_one = wait_for_sender_key_epoch(
        gamma.storage.as_ref(),
        &group.id.0.to_string(),
        &gamma.identity.device_id,
        1,
    )
    .await;
    let alpha_received_gamma_epoch_one = wait_for_sender_key_epoch(
        alpha.storage.as_ref(),
        &group.id.0.to_string(),
        &gamma.identity.device_id,
        1,
    )
    .await;

    assert_eq!(alpha_epoch_one.epoch, 1);
    assert_eq!(gamma_epoch_one.epoch, 1);
    assert_eq!(
        gamma_received_alpha_epoch_one.key_id,
        alpha_epoch_one.key_id
    );
    assert_eq!(
        alpha_received_gamma_epoch_one.key_id,
        gamma_epoch_one.key_id
    );
    assert_ne!(alpha_epoch_one.key_id, alpha_epoch_zero.key_id);
    assert_ne!(gamma_epoch_one.key_id, gamma_epoch_zero.key_id);
    assert!(beta
        .storage
        .get_sender_key_by_epoch(&group.id.0.to_string(), &alpha.identity.device_id, 1)
        .await
        .expect("lookup alpha rotated sender key for leaver")
        .is_none());
    assert!(beta
        .storage
        .get_sender_key_by_epoch(&group.id.0.to_string(), &gamma.identity.device_id, 1)
        .await
        .expect("lookup gamma rotated sender key for leaver")
        .is_none());

    let mut alpha_chat_events = alpha.chat.subscribe();
    let mut gamma_chat_events = gamma.chat.subscribe();
    let mut beta_chat_events = beta.chat.subscribe();

    alpha
        .chat
        .send_to_group(&group.id, "alpha after beta left")
        .await
        .expect("send post-leave alpha group message");
    let (alpha_peer_id, alpha_message) = expect_group_message_event(&mut gamma_chat_events).await;
    assert_eq!(alpha_peer_id, alpha.identity.device_id);
    assert_eq!(alpha_message.chat_id, group.id);
    assert_eq!(alpha_message.content, "alpha after beta left");

    gamma
        .chat
        .send_to_group(&group.id, "gamma after beta left")
        .await
        .expect("send post-leave gamma group message");
    let (gamma_peer_id, gamma_message) = expect_group_message_event(&mut alpha_chat_events).await;
    assert_eq!(gamma_peer_id, gamma.identity.device_id);
    assert_eq!(gamma_message.chat_id, group.id);
    assert_eq!(gamma_message.content, "gamma after beta left");

    expect_no_chat_event(&mut beta_chat_events).await;

    alpha.shutdown().await;
    beta.shutdown().await;
    gamma.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn group_message_roundtrip_delivers_to_all_online_members_with_group_chat_id() {
    let alpha = Node::new(110, "Alpha Node").await;
    let beta = Node::new(111, "Beta Node").await;
    let gamma = Node::new(112, "Gamma Node").await;

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

    let mut beta_chat_events = beta.chat.subscribe();
    let mut gamma_chat_events = gamma.chat.subscribe();

    let sent = alpha
        .chat
        .send_to_group(&group.id, "hello group")
        .await
        .expect("send group message");

    match expect_chat_event(&mut beta_chat_events).await {
        ChatServiceEvent::MessageReceived { peer_id, message } => {
            assert_eq!(peer_id, alpha.identity.device_id);
            assert_eq!(message.id, sent.id);
            assert_eq!(message.chat_id, group.id);
            assert_eq!(message.content, "hello group");
        }
        other => panic!("expected beta group message event, got {other:?}"),
    }

    match expect_chat_event(&mut gamma_chat_events).await {
        ChatServiceEvent::MessageReceived { peer_id, message } => {
            assert_eq!(peer_id, alpha.identity.device_id);
            assert_eq!(message.id, sent.id);
            assert_eq!(message.chat_id, group.id);
            assert_eq!(message.content, "hello group");
        }
        other => panic!("expected gamma group message event, got {other:?}"),
    }

    let alpha_messages = wait_for_messages(&alpha.chat, &group.id, 1).await;
    let beta_messages = wait_for_messages(&beta.chat, &group.id, 1).await;
    let gamma_messages = wait_for_messages(&gamma.chat, &group.id, 1).await;

    assert_eq!(alpha_messages[0].content, "hello group");
    assert_eq!(beta_messages[0].content, "hello group");
    assert_eq!(gamma_messages[0].content, "hello group");
    assert_eq!(beta_messages[0].chat_id, group.id);
    assert_eq!(gamma_messages[0].chat_id, group.id);

    alpha.shutdown().await;
    beta.shutdown().await;
    gamma.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn group_offline_member_is_skipped_without_blocking_online_delivery() {
    let alpha = Node::new(120, "Alpha Node").await;
    let gamma = Node::new(121, "Gamma Node").await;
    let offline_member_id = Uuid::from_u128(122).to_string();

    let _alpha_to_gamma = alpha.connect_to(&gamma).await;

    let group = alpha
        .chat
        .create_group(
            "Offline Room",
            vec![offline_member_id.clone(), gamma.identity.device_id.clone()],
        )
        .await
        .expect("create group with offline member");

    wait_for_group(gamma.storage.as_ref(), &group.id).await;

    let mut gamma_chat_events = gamma.chat.subscribe();
    let sent = time::timeout(
        Duration::from_millis(500),
        alpha
            .chat
            .send_to_group(&group.id, "still reaches online member"),
    )
    .await
    .expect("group send should not block on offline member")
    .expect("send group message");

    match expect_chat_event(&mut gamma_chat_events).await {
        ChatServiceEvent::MessageReceived { peer_id, message } => {
            assert_eq!(peer_id, alpha.identity.device_id);
            assert_eq!(message.id, sent.id);
            assert_eq!(message.content, "still reaches online member");
        }
        other => panic!("expected gamma group message event, got {other:?}"),
    }

    let gamma_messages = wait_for_messages(&gamma.chat, &group.id, 1).await;
    assert_eq!(gamma_messages[0].content, "still reaches online member");

    alpha.shutdown().await;
    gamma.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn group_reply_to_message_from_other_chat_does_not_leak_preview() {
    let alpha = Node::new(123, "Alpha Node").await;
    let beta = Node::new(124, "Beta Node").await;

    let _alpha_to_beta = alpha.connect_to(&beta).await;

    let group = alpha
        .chat
        .create_group("Review Room", vec![beta.identity.device_id.clone()])
        .await
        .expect("create group");

    wait_for_group(beta.storage.as_ref(), &group.id).await;

    let original = alpha
        .chat
        .send_message(&beta.identity.device_id, "Direct chat context")
        .await
        .expect("send direct message");

    let reply = alpha
        .chat
        .send_to_group_with_reply(
            &group.id,
            "Group reply should not include direct preview",
            Some(original.id.to_string()),
        )
        .await
        .expect("send group reply");

    assert_eq!(reply.reply_to_id, Some(original.id.to_string()));
    assert_eq!(reply.reply_to_preview, None);

    let alpha_group_messages = wait_for_messages(&alpha.chat, &group.id, 1).await;
    let beta_group_messages = wait_for_messages(&beta.chat, &group.id, 1).await;

    assert_eq!(
        alpha_group_messages[0].reply_to_id,
        Some(original.id.to_string())
    );
    assert_eq!(alpha_group_messages[0].reply_to_preview, None);
    assert_eq!(
        beta_group_messages[0].reply_to_id,
        Some(original.id.to_string())
    );
    assert_eq!(beta_group_messages[0].reply_to_preview, None);

    alpha.shutdown().await;
    beta.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn group_removed_member_stops_receiving_new_messages() {
    let alpha = Node::new(130, "Alpha Node").await;
    let beta = Node::new(131, "Beta Node").await;
    let gamma = Node::new(132, "Gamma Node").await;

    let _alpha_to_beta = alpha.connect_to(&beta).await;
    let _alpha_to_gamma = alpha.connect_to(&gamma).await;

    let group = alpha
        .chat
        .create_group(
            "Release Room",
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
        .send_to_group(&group.id, "before removal")
        .await
        .expect("send first group message");

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

    wait_for_group_members(alpha.storage.as_ref(), &group.id, &expected_members).await;
    wait_for_group_members(beta.storage.as_ref(), &group.id, &expected_members).await;
    wait_for_group_members(gamma.storage.as_ref(), &group.id, &expected_members).await;

    let mut beta_chat_events = beta.chat.subscribe();
    let mut gamma_chat_events = gamma.chat.subscribe();

    alpha
        .chat
        .send_to_group(&group.id, "after removal")
        .await
        .expect("send second group message");

    match expect_chat_event(&mut gamma_chat_events).await {
        ChatServiceEvent::MessageReceived { peer_id, message } => {
            assert_eq!(peer_id, alpha.identity.device_id);
            assert_eq!(message.content, "after removal");
        }
        other => panic!("expected gamma post-removal message, got {other:?}"),
    }

    time::sleep(Duration::from_millis(200)).await;
    let beta_messages = beta
        .chat
        .load_messages(&group.id, 20, 0)
        .await
        .expect("load beta group messages");
    assert_eq!(beta_messages.len(), 1);
    assert_eq!(beta_messages[0].content, "before removal");

    let beta_extra_event = time::timeout(Duration::from_millis(200), beta_chat_events.recv()).await;
    assert!(
        beta_extra_event.is_err(),
        "beta should not receive post-removal group messages"
    );

    alpha.shutdown().await;
    beta.shutdown().await;
    gamma.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn group_invited_member_only_receives_new_history_after_membership_update() {
    let alpha = Node::new(140, "Alpha Node").await;
    let beta = Node::new(141, "Beta Node").await;
    let gamma = Node::new(142, "Gamma Node").await;

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
    let mut gamma_server_events = gamma.server.subscribe();

    let updated = alpha
        .chat
        .add_group_members(&group.id, vec![gamma.identity.device_id.clone()])
        .await
        .expect("invite gamma into group");

    let expected_members = vec![
        user_id_from_peer(&alpha.identity.device_id),
        user_id_from_peer(&beta.identity.device_id),
        user_id_from_peer(&gamma.identity.device_id),
    ];
    assert_eq!(updated.member_ids, expected_members);

    match expect_server_event(&mut gamma_server_events).await {
        WsServerEvent::MessageReceived { peer, message } => {
            assert_eq!(peer.identity.device_id, alpha.identity.device_id);
            assert_eq!(
                message,
                ProtocolMessage::GroupInvite {
                    group_id: group.id.0.to_string(),
                    inviter_id: alpha.identity.device_id.clone(),
                }
            );
        }
        other => panic!("expected group invite for gamma, got {other:?}"),
    }

    wait_for_group(gamma.storage.as_ref(), &group.id).await;

    let mut gamma_chat_events = gamma.chat.subscribe();
    alpha
        .chat
        .send_to_group(&group.id, "after invite")
        .await
        .expect("send post-invite message");

    match expect_chat_event(&mut gamma_chat_events).await {
        ChatServiceEvent::MessageReceived { peer_id, message } => {
            assert_eq!(peer_id, alpha.identity.device_id);
            assert_eq!(message.content, "after invite");
            assert_eq!(message.chat_id, group.id);
        }
        other => panic!("expected gamma post-invite message, got {other:?}"),
    }

    let gamma_messages = wait_for_messages(&gamma.chat, &group.id, 1).await;
    assert_eq!(gamma_messages.len(), 1);
    assert_eq!(gamma_messages[0].content, "after invite");

    alpha.shutdown().await;
    beta.shutdown().await;
    gamma.shutdown().await;
}
