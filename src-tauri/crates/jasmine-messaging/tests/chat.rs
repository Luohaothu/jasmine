use std::sync::Arc;
use std::time::Duration;

use jasmine_core::{AckStatus, Message, MessageStatus, ProtocolMessage, StorageEngine};
use jasmine_messaging::{
    ChatService, ChatServiceConfig, ChatServiceEvent, WsClient, WsClientConfig, WsClientEvent,
    WsPeerIdentity, WsServer, WsServerConfig, WsServerEvent,
};
use jasmine_storage::SqliteStorage;
use tempfile::TempDir;
use tokio::time;
use uuid::Uuid;

const TEST_ACK_TIMEOUT: Duration = Duration::from_millis(250);

struct Node {
    identity: WsPeerIdentity,
    _temp_dir: TempDir,
    _storage: Arc<SqliteStorage>,
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
            _storage: storage,
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
    WsPeerIdentity::new(Uuid::from_u128(seed).to_string(), display_name)
}

fn server_config(identity: WsPeerIdentity) -> WsServerConfig {
    let mut config = WsServerConfig::new(identity);
    config.bind_addr = "127.0.0.1:0".to_string();
    config.heartbeat_interval = Duration::from_millis(100);
    config.max_missed_heartbeats = 3;
    config.handshake_timeout = Duration::from_secs(1);
    config
}

fn client_config(identity: WsPeerIdentity) -> WsClientConfig {
    let mut config = WsClientConfig::new(identity);
    config.heartbeat_interval = Duration::from_millis(100);
    config.max_missed_heartbeats = 3;
    config.handshake_timeout = Duration::from_secs(1);
    config
}

fn ws_url(server: &WsServer) -> String {
    format!("ws://{}", server.local_addr())
}

async fn expect_server_event(
    receiver: &mut tokio::sync::broadcast::Receiver<WsServerEvent>,
) -> WsServerEvent {
    time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .expect("server event timeout")
        .expect("server event channel closed")
}

async fn expect_client_event(
    receiver: &mut tokio::sync::broadcast::Receiver<WsClientEvent>,
) -> WsClientEvent {
    time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .expect("client event timeout")
        .expect("client event channel closed")
}

async fn expect_chat_event(
    receiver: &mut tokio::sync::broadcast::Receiver<ChatServiceEvent>,
) -> ChatServiceEvent {
    time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .expect("chat event timeout")
        .expect("chat event channel closed")
}

async fn expect_no_chat_event(receiver: &mut tokio::sync::broadcast::Receiver<ChatServiceEvent>) {
    assert!(time::timeout(Duration::from_millis(200), receiver.recv())
        .await
        .is_err());
}

async fn wait_for_messages(
    chat: &ChatService<SqliteStorage>,
    peer_id: &str,
    expected_len: usize,
) -> Vec<Message> {
    let chat_id = ChatService::<SqliteStorage>::direct_chat_id(peer_id).expect("direct chat id");

    time::timeout(Duration::from_secs(2), async {
        loop {
            let messages = chat
                .load_messages(&chat_id, 10, 0)
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

async fn wait_for_status(
    chat: &ChatService<SqliteStorage>,
    peer_id: &str,
    message_id: Uuid,
    expected_status: MessageStatus,
) -> Message {
    time::timeout(Duration::from_secs(2), async {
        loop {
            let messages = wait_for_messages(chat, peer_id, 1).await;
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

async fn wait_for_message_revision(
    storage: &Arc<SqliteStorage>,
    message_id: Uuid,
    expected_content: &str,
    expected_edit_version: u32,
) -> Message {
    let message_id_string = message_id.to_string();

    time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(message) = storage
                .get_message(&message_id_string)
                .await
                .expect("load message by id")
            {
                if message.content == expected_content
                    && message.edit_version == expected_edit_version
                {
                    return message;
                }
            }

            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("message should reach expected revision")
}

async fn wait_for_deleted_message(storage: &Arc<SqliteStorage>, message_id: Uuid) -> Message {
    let message_id_string = message_id.to_string();

    time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(message) = storage
                .get_message(&message_id_string)
                .await
                .expect("load deleted message by id")
            {
                if message.is_deleted {
                    return message;
                }
            }

            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("message should be marked deleted")
}

async fn wait_for_message(storage: &Arc<SqliteStorage>, message_id: Uuid) -> Message {
    let message_id_string = message_id.to_string();

    time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(message) = storage
                .get_message(&message_id_string)
                .await
                .expect("load message by id")
            {
                return message;
            }

            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("message should be persisted")
}

fn truncated_reply_preview(content: &str) -> String {
    let mut chars = content.chars();
    let preview: String = chars.by_ref().take(100).collect();

    if chars.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_send_receive_ack_flow_persists_sent_then_delivered() {
    let sender = Node::new(1, "Sender Node").await;
    let receiver_identity = peer(2, "Receiver Node");
    let receiver_server = Arc::new(
        WsServer::bind(server_config(receiver_identity.clone()))
            .await
            .expect("bind receiver server"),
    );
    let mut receiver_events = receiver_server.subscribe();

    let client = Arc::new(
        WsClient::connect(
            ws_url(&receiver_server),
            client_config(sender.identity.clone()),
        )
        .await
        .expect("connect sender client"),
    );
    sender
        .chat
        .register_client(Arc::clone(&client))
        .await
        .expect("register sender client");

    match expect_server_event(&mut receiver_events).await {
        WsServerEvent::PeerConnected { peer } => {
            assert_eq!(peer.identity.device_id, sender.identity.device_id)
        }
        other => panic!("expected receiver peer connection, got {other:?}"),
    }

    let queued = sender
        .chat
        .send_message(&receiver_identity.device_id, "hello receiver")
        .await
        .expect("queue outbound message");
    assert_eq!(queued.status, MessageStatus::Sent);

    let sent_messages = wait_for_messages(&sender.chat, &receiver_identity.device_id, 1).await;
    assert_eq!(sent_messages[0].status, MessageStatus::Sent);

    match expect_server_event(&mut receiver_events).await {
        WsServerEvent::MessageReceived { peer, message } => {
            assert_eq!(peer.identity.device_id, sender.identity.device_id);
            match message {
                ProtocolMessage::TextMessage {
                    id,
                    chat_id,
                    sender_id,
                    content,
                    ..
                } => {
                    assert_eq!(id, queued.id.to_string());
                    assert_eq!(chat_id, receiver_identity.device_id);
                    assert_eq!(sender_id, sender.identity.device_id);
                    assert_eq!(content, "hello receiver");
                }
                other => panic!("expected text message, got {other:?}"),
            }
        }
        other => panic!("expected receiver message event, got {other:?}"),
    }

    receiver_server
        .send_to(
            &sender.identity.device_id,
            ProtocolMessage::Ack {
                message_id: queued.id.to_string(),
                status: AckStatus::Received,
            },
        )
        .await
        .expect("send ack from receiver server");

    let delivered = wait_for_status(
        &sender.chat,
        &receiver_identity.device_id,
        queued.id,
        MessageStatus::Delivered,
    )
    .await;
    assert_eq!(delivered.content, "hello receiver");

    sender.shutdown().await;
    receiver_server
        .shutdown()
        .await
        .expect("shutdown receiver server");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn chat_receives_local_mention_emits_event_and_preserves_raw_content() {
    let sender = Node::new(20, "Sender Node").await;
    let receiver = Node::new(21, "Receiver Node").await;
    let mut receiver_server_events = receiver.server.subscribe();
    let mut receiver_chat_events = receiver.chat.subscribe();
    let _client = sender.connect_to(&receiver).await;

    match expect_server_event(&mut receiver_server_events).await {
        WsServerEvent::PeerConnected { peer } => {
            assert_eq!(peer.identity.device_id, sender.identity.device_id)
        }
        other => panic!("expected receiver peer connection, got {other:?}"),
    }

    let content = format!(
        "Hey @[{}](user:{}) check this",
        receiver.identity.display_name, receiver.identity.device_id
    );
    let sent = sender
        .chat
        .send_message(&receiver.identity.device_id, content.clone())
        .await
        .expect("send mention message");

    match expect_server_event(&mut receiver_server_events).await {
        WsServerEvent::MessageReceived { peer, message } => {
            assert_eq!(peer.identity.device_id, sender.identity.device_id);
            match message {
                ProtocolMessage::TextMessage { id, content, .. } => {
                    assert_eq!(id, sent.id.to_string());
                    assert_eq!(content, sent.content);
                }
                other => panic!("expected text message, got {other:?}"),
            }
        }
        other => panic!("expected mention message event, got {other:?}"),
    }

    match expect_chat_event(&mut receiver_chat_events).await {
        ChatServiceEvent::MessageReceived { peer_id, message } => {
            assert_eq!(peer_id, sender.identity.device_id);
            assert_eq!(message.id, sent.id);
            assert_eq!(message.content, sent.content);
        }
        other => panic!("expected receiver message event, got {other:?}"),
    }

    match expect_chat_event(&mut receiver_chat_events).await {
        ChatServiceEvent::MentionReceived {
            message_id,
            mentioned_user_id,
            sender_name,
        } => {
            assert_eq!(message_id, sent.id);
            assert_eq!(mentioned_user_id, receiver.identity.device_id);
            assert_eq!(sender_name, sender.identity.display_name);
        }
        other => panic!("expected mention received event, got {other:?}"),
    }

    let sender_stored = wait_for_message_revision(&sender._storage, sent.id, &content, 0).await;
    assert_eq!(sender_stored.content, content);
    let receiver_stored = wait_for_message_revision(&receiver._storage, sent.id, &content, 0).await;
    assert_eq!(receiver_stored.content, content);

    let delivered = wait_for_status(
        &sender.chat,
        &receiver.identity.device_id,
        sent.id,
        MessageStatus::Delivered,
    )
    .await;
    assert_eq!(delivered.content, content);

    sender.shutdown().await;
    receiver.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn chat_non_local_mention_does_not_emit_mention_event() {
    let sender = Node::new(22, "Sender Node").await;
    let receiver = Node::new(23, "Receiver Node").await;
    let mut receiver_server_events = receiver.server.subscribe();
    let mut receiver_chat_events = receiver.chat.subscribe();
    let _client = sender.connect_to(&receiver).await;

    match expect_server_event(&mut receiver_server_events).await {
        WsServerEvent::PeerConnected { peer } => {
            assert_eq!(peer.identity.device_id, sender.identity.device_id)
        }
        other => panic!("expected receiver peer connection, got {other:?}"),
    }

    let other_user_id = Uuid::from_u128(24).to_string();
    let content = format!("Hey @[Someone Else](user:{other_user_id}) hi");
    let sent = sender
        .chat
        .send_message(&receiver.identity.device_id, content.clone())
        .await
        .expect("send non-local mention message");

    match expect_server_event(&mut receiver_server_events).await {
        WsServerEvent::MessageReceived { peer, message } => {
            assert_eq!(peer.identity.device_id, sender.identity.device_id);
            match message {
                ProtocolMessage::TextMessage { id, content, .. } => {
                    assert_eq!(id, sent.id.to_string());
                    assert_eq!(content, sent.content);
                }
                other => panic!("expected text message, got {other:?}"),
            }
        }
        other => panic!("expected non-local mention message event, got {other:?}"),
    }

    match expect_chat_event(&mut receiver_chat_events).await {
        ChatServiceEvent::MessageReceived { peer_id, message } => {
            assert_eq!(peer_id, sender.identity.device_id);
            assert_eq!(message.id, sent.id);
            assert_eq!(message.content, sent.content);
        }
        other => panic!("expected receiver message event, got {other:?}"),
    }

    expect_no_chat_event(&mut receiver_chat_events).await;

    let receiver_stored = wait_for_message_revision(&receiver._storage, sent.id, &content, 0).await;
    assert_eq!(receiver_stored.content, content);

    let _delivered = wait_for_status(
        &sender.chat,
        &receiver.identity.device_id,
        sent.id,
        MessageStatus::Delivered,
    )
    .await;

    sender.shutdown().await;
    receiver.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn chat_multiple_mentions_emit_single_local_mention_event() {
    let sender = Node::new(25, "Sender Node").await;
    let receiver = Node::new(26, "Receiver Node").await;
    let mut receiver_server_events = receiver.server.subscribe();
    let mut receiver_chat_events = receiver.chat.subscribe();
    let _client = sender.connect_to(&receiver).await;

    match expect_server_event(&mut receiver_server_events).await {
        WsServerEvent::PeerConnected { peer } => {
            assert_eq!(peer.identity.device_id, sender.identity.device_id)
        }
        other => panic!("expected receiver peer connection, got {other:?}"),
    }

    let other_user_id = Uuid::from_u128(27).to_string();
    let content = format!(
        "Hi @[Someone Else](user:{other_user_id}) and @[{}](user:{}) and @[Another](user:{})",
        receiver.identity.display_name, receiver.identity.device_id, other_user_id
    );
    let sent = sender
        .chat
        .send_message(&receiver.identity.device_id, content.clone())
        .await
        .expect("send multiple mention message");

    match expect_server_event(&mut receiver_server_events).await {
        WsServerEvent::MessageReceived { peer, message } => {
            assert_eq!(peer.identity.device_id, sender.identity.device_id);
            match message {
                ProtocolMessage::TextMessage { id, content, .. } => {
                    assert_eq!(id, sent.id.to_string());
                    assert_eq!(content, sent.content);
                }
                other => panic!("expected text message, got {other:?}"),
            }
        }
        other => panic!("expected multiple mention message event, got {other:?}"),
    }

    match expect_chat_event(&mut receiver_chat_events).await {
        ChatServiceEvent::MessageReceived { peer_id, message } => {
            assert_eq!(peer_id, sender.identity.device_id);
            assert_eq!(message.id, sent.id);
            assert_eq!(message.content, sent.content);
        }
        other => panic!("expected receiver message event, got {other:?}"),
    }

    match expect_chat_event(&mut receiver_chat_events).await {
        ChatServiceEvent::MentionReceived {
            message_id,
            mentioned_user_id,
            sender_name,
        } => {
            assert_eq!(message_id, sent.id);
            assert_eq!(mentioned_user_id, receiver.identity.device_id);
            assert_eq!(sender_name, sender.identity.display_name);
        }
        other => panic!("expected mention received event, got {other:?}"),
    }

    expect_no_chat_event(&mut receiver_chat_events).await;

    let receiver_stored = wait_for_message_revision(&receiver._storage, sent.id, &content, 0).await;
    assert_eq!(receiver_stored.content, content);

    let _delivered = wait_for_status(
        &sender.chat,
        &receiver.identity.device_id,
        sent.id,
        MessageStatus::Delivered,
    )
    .await;

    sender.shutdown().await;
    receiver.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_receives_incoming_message_persists_emits_event_and_acknowledges() {
    let receiver = Node::new(10, "Receiver Node").await;
    let sender_identity = peer(11, "Sender Node");
    let sender_client = Arc::new(
        WsClient::connect(
            ws_url(&receiver.server),
            client_config(sender_identity.clone()),
        )
        .await
        .expect("connect sender client"),
    );
    let mut sender_events = sender_client.subscribe();
    let mut chat_events = receiver.chat.subscribe();
    let message_id = Uuid::new_v4();

    sender_client
        .send(ProtocolMessage::TextMessage {
            id: message_id.to_string(),
            chat_id: receiver.identity.device_id.clone(),
            sender_id: sender_identity.device_id.clone(),
            content: "hello inbound".to_string(),
            timestamp: 1_700_000_000_123,
            reply_to_id: None,
            reply_to_preview: None,
        })
        .await
        .expect("send inbound text message");

    match expect_chat_event(&mut chat_events).await {
        ChatServiceEvent::MessageReceived { peer_id, message } => {
            assert_eq!(peer_id, sender_identity.device_id);
            assert_eq!(message.id, message_id);
            assert_eq!(message.content, "hello inbound");
            assert_eq!(message.status, MessageStatus::Delivered);
        }
        other => panic!("expected chat message event, got {other:?}"),
    }

    let stored_messages = wait_for_messages(&receiver.chat, &sender_identity.device_id, 1).await;
    assert_eq!(stored_messages[0].id, message_id);
    assert_eq!(stored_messages[0].status, MessageStatus::Delivered);

    match expect_client_event(&mut sender_events).await {
        WsClientEvent::MessageReceived { message } => {
            assert_eq!(
                message,
                ProtocolMessage::Ack {
                    message_id: message_id.to_string(),
                    status: AckStatus::Received,
                }
            );
        }
        other => panic!("expected ack event, got {other:?}"),
    }

    sender_client
        .disconnect()
        .await
        .expect("disconnect sender client");
    receiver.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn chat_send_uses_inbound_server_connection_for_bidirectional_messages() {
    let alpha = Node::new(20, "Alpha Node").await;
    let beta = Node::new(21, "Beta Node").await;
    let _alpha_client = alpha.connect_to(&beta).await;
    let mut alpha_events = alpha.chat.subscribe();

    let queued = beta
        .chat
        .send_message(&alpha.identity.device_id, "reply over inbound connection")
        .await
        .expect("send message via inbound server connection");
    assert_eq!(queued.status, MessageStatus::Sent);

    match expect_chat_event(&mut alpha_events).await {
        ChatServiceEvent::MessageReceived { peer_id, message } => {
            assert_eq!(peer_id, beta.identity.device_id);
            assert_eq!(message.content, "reply over inbound connection");
        }
        other => panic!("expected chat message event, got {other:?}"),
    }

    let alpha_messages = wait_for_messages(&alpha.chat, &beta.identity.device_id, 1).await;
    assert_eq!(alpha_messages[0].content, "reply over inbound connection");
    assert_eq!(alpha_messages[0].status, MessageStatus::Delivered);

    let beta_delivered = wait_for_status(
        &beta.chat,
        &alpha.identity.device_id,
        queued.id,
        MessageStatus::Delivered,
    )
    .await;
    assert_eq!(beta_delivered.content, "reply over inbound connection");

    let beta_chat_id = ChatService::<SqliteStorage>::direct_chat_id(&alpha.identity.device_id)
        .expect("beta direct chat id");
    let loaded = beta
        .chat
        .load_messages(&beta_chat_id, 10, 0)
        .await
        .expect("load beta messages");
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].id, queued.id);

    alpha.shutdown().await;
    beta.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_offline_peer_immediately_marks_message_failed() {
    let sender = Node::new(30, "Sender Node").await;
    let offline_peer_id = Uuid::from_u128(31).to_string();

    let failed = sender
        .chat
        .send_message(&offline_peer_id, "nobody is listening")
        .await
        .expect("persist failed offline message");
    assert_eq!(failed.status, MessageStatus::Failed);

    let stored = wait_for_status(
        &sender.chat,
        &offline_peer_id,
        failed.id,
        MessageStatus::Failed,
    )
    .await;
    assert_eq!(stored.content, "nobody is listening");

    let direct_chat_id =
        ChatService::<SqliteStorage>::direct_chat_id(&offline_peer_id).expect("offline chat id");
    let loaded = sender
        .chat
        .load_messages(&direct_chat_id, 10, 0)
        .await
        .expect("load offline chat");
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].status, MessageStatus::Failed);

    sender.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_ack_timeout_marks_message_failed() {
    let sender = Node::new(40, "Sender Node").await;
    let receiver_identity = peer(41, "Slow Receiver");
    let receiver_server = Arc::new(
        WsServer::bind(server_config(receiver_identity.clone()))
            .await
            .expect("bind slow receiver"),
    );
    let mut receiver_events = receiver_server.subscribe();

    let client = Arc::new(
        WsClient::connect(
            ws_url(&receiver_server),
            client_config(sender.identity.clone()),
        )
        .await
        .expect("connect sender client"),
    );
    sender
        .chat
        .register_client(Arc::clone(&client))
        .await
        .expect("register sender client");

    match expect_server_event(&mut receiver_events).await {
        WsServerEvent::PeerConnected { peer } => {
            assert_eq!(peer.identity.device_id, sender.identity.device_id)
        }
        other => panic!("expected receiver peer connection, got {other:?}"),
    }

    let queued = sender
        .chat
        .send_message(&receiver_identity.device_id, "ack timeout")
        .await
        .expect("queue outbound message");
    assert_eq!(queued.status, MessageStatus::Sent);

    match expect_server_event(&mut receiver_events).await {
        WsServerEvent::MessageReceived { .. } => {}
        other => panic!("expected receiver message event, got {other:?}"),
    }

    let failed = wait_for_status(
        &sender.chat,
        &receiver_identity.device_id,
        queued.id,
        MessageStatus::Failed,
    )
    .await;
    assert_eq!(failed.content, "ack timeout");

    sender.shutdown().await;
    receiver_server
        .shutdown()
        .await
        .expect("shutdown slow receiver");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn chat_send_reply_message_snapshots_preview_and_preserves_it_after_original_delete() {
    let sender = Node::new(42, "Reply Sender").await;
    let receiver = Node::new(43, "Reply Receiver").await;
    let mut receiver_server_events = receiver.server.subscribe();
    let mut receiver_chat_events = receiver.chat.subscribe();
    let _client = sender.connect_to(&receiver).await;

    match expect_server_event(&mut receiver_server_events).await {
        WsServerEvent::PeerConnected { peer } => {
            assert_eq!(peer.identity.device_id, sender.identity.device_id)
        }
        other => panic!("expected receiver peer connection, got {other:?}"),
    }

    let original = sender
        .chat
        .send_message(&receiver.identity.device_id, "Original quoted message")
        .await
        .expect("send original message");

    match expect_server_event(&mut receiver_server_events).await {
        WsServerEvent::MessageReceived { peer, message } => {
            assert_eq!(peer.identity.device_id, sender.identity.device_id);
            match message {
                ProtocolMessage::TextMessage {
                    id,
                    content,
                    reply_to_id,
                    reply_to_preview,
                    ..
                } => {
                    assert_eq!(id, original.id.to_string());
                    assert_eq!(content, "Original quoted message");
                    assert_eq!(reply_to_id, None);
                    assert_eq!(reply_to_preview, None);
                }
                other => panic!("expected text message, got {other:?}"),
            }
        }
        other => panic!("expected original message event, got {other:?}"),
    }

    match expect_chat_event(&mut receiver_chat_events).await {
        ChatServiceEvent::MessageReceived { peer_id, message } => {
            assert_eq!(peer_id, sender.identity.device_id);
            assert_eq!(message.id, original.id);
            assert_eq!(message.content, "Original quoted message");
            assert_eq!(message.reply_to_id, None);
            assert_eq!(message.reply_to_preview, None);
        }
        other => panic!("expected original chat event, got {other:?}"),
    }

    let _original_delivered = wait_for_status(
        &sender.chat,
        &receiver.identity.device_id,
        original.id,
        MessageStatus::Delivered,
    )
    .await;

    let reply = sender
        .chat
        .send_message_with_reply(
            &receiver.identity.device_id,
            "Reply with quote",
            Some(original.id.to_string()),
        )
        .await
        .expect("send quoted reply");

    assert_eq!(reply.reply_to_id, Some(original.id.to_string()));
    assert_eq!(
        reply.reply_to_preview,
        Some("Original quoted message".to_string())
    );

    match expect_server_event(&mut receiver_server_events).await {
        WsServerEvent::MessageReceived { peer, message } => {
            assert_eq!(peer.identity.device_id, sender.identity.device_id);
            match message {
                ProtocolMessage::TextMessage {
                    id,
                    content,
                    reply_to_id,
                    reply_to_preview,
                    ..
                } => {
                    assert_eq!(id, reply.id.to_string());
                    assert_eq!(content, "Reply with quote");
                    assert_eq!(reply_to_id, Some(original.id.to_string()));
                    assert_eq!(
                        reply_to_preview,
                        Some("Original quoted message".to_string())
                    );
                }
                other => panic!("expected text message, got {other:?}"),
            }
        }
        other => panic!("expected quoted reply event, got {other:?}"),
    }

    match expect_chat_event(&mut receiver_chat_events).await {
        ChatServiceEvent::MessageReceived { peer_id, message } => {
            assert_eq!(peer_id, sender.identity.device_id);
            assert_eq!(message.id, reply.id);
            assert_eq!(message.content, "Reply with quote");
            assert_eq!(message.reply_to_id, Some(original.id.to_string()));
            assert_eq!(
                message.reply_to_preview,
                Some("Original quoted message".to_string())
            );
        }
        other => panic!("expected quoted reply chat event, got {other:?}"),
    }

    let _reply_delivered = wait_for_status(
        &sender.chat,
        &receiver.identity.device_id,
        reply.id,
        MessageStatus::Delivered,
    )
    .await;

    let sender_reply_before_delete = wait_for_message(&sender._storage, reply.id).await;
    assert_eq!(
        sender_reply_before_delete.reply_to_preview,
        Some("Original quoted message".to_string())
    );
    let receiver_reply_before_delete = wait_for_message(&receiver._storage, reply.id).await;
    assert_eq!(
        receiver_reply_before_delete.reply_to_preview,
        Some("Original quoted message".to_string())
    );

    sender
        .chat
        .delete_message(&original.id.to_string(), &sender.identity.device_id)
        .await
        .expect("delete original message after reply snapshot");

    let _sender_deleted_original = wait_for_deleted_message(&sender._storage, original.id).await;
    let _receiver_deleted_original =
        wait_for_deleted_message(&receiver._storage, original.id).await;

    let sender_reply_after_delete = wait_for_message(&sender._storage, reply.id).await;
    assert_eq!(
        sender_reply_after_delete.reply_to_id,
        Some(original.id.to_string())
    );
    assert_eq!(
        sender_reply_after_delete.reply_to_preview,
        Some("Original quoted message".to_string())
    );

    let receiver_reply_after_delete = wait_for_message(&receiver._storage, reply.id).await;
    assert_eq!(
        receiver_reply_after_delete.reply_to_id,
        Some(original.id.to_string())
    );
    assert_eq!(
        receiver_reply_after_delete.reply_to_preview,
        Some("Original quoted message".to_string())
    );

    sender.shutdown().await;
    receiver.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_reply_to_missing_original_stores_none_preview() {
    let sender = Node::new(44, "Missing Reply Sender").await;
    let offline_peer_id = Uuid::from_u128(45).to_string();
    let missing_message_id = Uuid::from_u128(46).to_string();

    let reply = sender
        .chat
        .send_message_with_reply(
            &offline_peer_id,
            "Reply without local original",
            Some(missing_message_id.clone()),
        )
        .await
        .expect("send reply without local original");

    assert_eq!(reply.status, MessageStatus::Failed);
    assert_eq!(reply.reply_to_id, Some(missing_message_id.clone()));
    assert_eq!(reply.reply_to_preview, None);

    let stored_reply = wait_for_message(&sender._storage, reply.id).await;
    assert_eq!(stored_reply.reply_to_id, Some(missing_message_id));
    assert_eq!(stored_reply.reply_to_preview, None);
    assert_eq!(stored_reply.content, "Reply without local original");

    sender.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_reply_to_message_from_another_direct_chat_does_not_leak_preview() {
    let sender = Node::new(54, "Cross Chat Sender").await;
    let original_peer_id = Uuid::from_u128(55).to_string();
    let reply_peer_id = Uuid::from_u128(56).to_string();

    let original = sender
        .chat
        .send_message(&original_peer_id, "Private message from another chat")
        .await
        .expect("store original direct message locally");
    assert_eq!(original.status, MessageStatus::Failed);

    let reply = sender
        .chat
        .send_message_with_reply(
            &reply_peer_id,
            "Reply should not leak preview",
            Some(original.id.to_string()),
        )
        .await
        .expect("send cross-chat reply");

    assert_eq!(reply.status, MessageStatus::Failed);
    assert_eq!(reply.reply_to_id, Some(original.id.to_string()));
    assert_eq!(reply.reply_to_preview, None);

    let stored_reply = wait_for_message(&sender._storage, reply.id).await;
    assert_eq!(stored_reply.reply_to_id, Some(original.id.to_string()));
    assert_eq!(stored_reply.reply_to_preview, None);
    assert_eq!(stored_reply.content, "Reply should not leak preview");

    sender.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_reply_preview_truncates_to_one_hundred_characters() {
    let sender = Node::new(47, "Truncation Sender").await;
    let peer_id = Uuid::from_u128(48).to_string();
    let original_content = "0123456789".repeat(11);
    let expected_preview = truncated_reply_preview(&original_content);

    let original = sender
        .chat
        .send_message(&peer_id, original_content.clone())
        .await
        .expect("store original message locally");
    assert_eq!(original.status, MessageStatus::Failed);

    let reply = sender
        .chat
        .send_message_with_reply(
            &peer_id,
            "Reply with truncated preview",
            Some(original.id.to_string()),
        )
        .await
        .expect("send reply with truncated preview");

    assert_eq!(reply.status, MessageStatus::Failed);
    assert_eq!(reply.reply_to_id, Some(original.id.to_string()));
    assert_eq!(reply.reply_to_preview, Some(expected_preview.clone()));

    let stored_reply = wait_for_message(&sender._storage, reply.id).await;
    assert_eq!(stored_reply.reply_to_id, Some(original.id.to_string()));
    assert_eq!(stored_reply.reply_to_preview, Some(expected_preview));
    assert_eq!(stored_reply.content, "Reply with truncated preview");

    sender.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_receives_incoming_reply_metadata_and_persists_it() {
    let receiver = Node::new(52, "Reply Metadata Receiver").await;
    let sender_identity = peer(53, "Reply Metadata Sender");
    let sender_client = Arc::new(
        WsClient::connect(
            ws_url(&receiver.server),
            client_config(sender_identity.clone()),
        )
        .await
        .expect("connect sender client"),
    );
    let mut sender_events = sender_client.subscribe();
    let mut receiver_chat_events = receiver.chat.subscribe();
    let message_id = Uuid::new_v4();
    let reply_to_id = Uuid::new_v4().to_string();
    let reply_to_preview = "Incoming preview snapshot".to_string();

    sender_client
        .send(ProtocolMessage::TextMessage {
            id: message_id.to_string(),
            chat_id: receiver.identity.device_id.clone(),
            sender_id: sender_identity.device_id.clone(),
            content: "incoming reply payload".to_string(),
            timestamp: 1_700_000_000_222,
            reply_to_id: Some(reply_to_id.clone()),
            reply_to_preview: Some(reply_to_preview.clone()),
        })
        .await
        .expect("send inbound reply text message");

    match expect_chat_event(&mut receiver_chat_events).await {
        ChatServiceEvent::MessageReceived { peer_id, message } => {
            assert_eq!(peer_id, sender_identity.device_id);
            assert_eq!(message.id, message_id);
            assert_eq!(message.content, "incoming reply payload");
            assert_eq!(message.reply_to_id, Some(reply_to_id.clone()));
            assert_eq!(message.reply_to_preview, Some(reply_to_preview.clone()));
        }
        other => panic!("expected inbound reply chat event, got {other:?}"),
    }

    let stored_message = wait_for_message(&receiver._storage, message_id).await;
    assert_eq!(stored_message.reply_to_id, Some(reply_to_id));
    assert_eq!(stored_message.reply_to_preview, Some(reply_to_preview));
    assert_eq!(stored_message.content, "incoming reply payload");

    match expect_client_event(&mut sender_events).await {
        WsClientEvent::MessageReceived { message } => {
            assert_eq!(
                message,
                ProtocolMessage::Ack {
                    message_id: message_id.to_string(),
                    status: AckStatus::Received,
                }
            );
        }
        other => panic!("expected ack event, got {other:?}"),
    }

    sender_client
        .disconnect()
        .await
        .expect("disconnect sender client");
    receiver.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn chat_edit_message_updates_storage_emits_event_and_sends_protocol_edit() {
    let sender = Node::new(50, "Editor Node").await;
    let receiver = Node::new(51, "Receiver Node").await;
    let mut receiver_server_events = receiver.server.subscribe();
    let _client = sender.connect_to(&receiver).await;

    match expect_server_event(&mut receiver_server_events).await {
        WsServerEvent::PeerConnected { peer } => {
            assert_eq!(peer.identity.device_id, sender.identity.device_id)
        }
        other => panic!("expected receiver peer connection, got {other:?}"),
    }

    let sent = sender
        .chat
        .send_message(&receiver.identity.device_id, "before edit")
        .await
        .expect("send original message");

    match expect_server_event(&mut receiver_server_events).await {
        WsServerEvent::MessageReceived { peer, message } => {
            assert_eq!(peer.identity.device_id, sender.identity.device_id);
            match message {
                ProtocolMessage::TextMessage { id, content, .. } => {
                    assert_eq!(id, sent.id.to_string());
                    assert_eq!(content, "before edit");
                }
                other => panic!("expected text message, got {other:?}"),
            }
        }
        other => panic!("expected original message event, got {other:?}"),
    }

    let _delivered = wait_for_status(
        &sender.chat,
        &receiver.identity.device_id,
        sent.id,
        MessageStatus::Delivered,
    )
    .await;
    let receiver_original =
        wait_for_message_revision(&receiver._storage, sent.id, "before edit", 0).await;
    assert_eq!(receiver_original.edited_at, None);

    let mut sender_chat_events = sender.chat.subscribe();
    let mut receiver_chat_events = receiver.chat.subscribe();

    sender
        .chat
        .edit_message(
            &sent.id.to_string(),
            "after edit",
            &sender.identity.device_id,
        )
        .await
        .expect("edit sent message");

    match expect_chat_event(&mut sender_chat_events).await {
        ChatServiceEvent::MessageEdited {
            message_id,
            new_content,
            edit_version,
        } => {
            assert_eq!(message_id, sent.id);
            assert_eq!(new_content, "after edit");
            assert_eq!(edit_version, 1);
        }
        other => panic!("expected local message edit event, got {other:?}"),
    }

    match expect_server_event(&mut receiver_server_events).await {
        WsServerEvent::MessageReceived { peer, message } => {
            assert_eq!(peer.identity.device_id, sender.identity.device_id);
            match message {
                ProtocolMessage::MessageEdit {
                    message_id,
                    chat_id,
                    sender_id,
                    new_content,
                    edit_version,
                    timestamp_ms,
                } => {
                    assert_eq!(message_id, sent.id.to_string());
                    assert_eq!(chat_id, receiver.identity.device_id);
                    assert_eq!(sender_id, sender.identity.device_id);
                    assert_eq!(new_content, "after edit");
                    assert_eq!(edit_version, 1);
                    assert!(timestamp_ms > 0);
                }
                other => panic!("expected message edit, got {other:?}"),
            }
        }
        other => panic!("expected edit message event, got {other:?}"),
    }

    match expect_chat_event(&mut receiver_chat_events).await {
        ChatServiceEvent::MessageEdited {
            message_id,
            new_content,
            edit_version,
        } => {
            assert_eq!(message_id, sent.id);
            assert_eq!(new_content, "after edit");
            assert_eq!(edit_version, 1);
        }
        other => panic!("expected receiver message edit event, got {other:?}"),
    }

    let sender_edited = wait_for_message_revision(&sender._storage, sent.id, "after edit", 1).await;
    assert!(sender_edited.edited_at.is_some());
    let receiver_edited =
        wait_for_message_revision(&receiver._storage, sent.id, "after edit", 1).await;
    assert!(receiver_edited.edited_at.is_some());

    sender.shutdown().await;
    receiver.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_edit_message_rejects_unauthorized_sender() {
    let sender = Node::new(60, "Sender Node").await;
    let offline_peer_id = Uuid::from_u128(61).to_string();
    let unauthorized_sender_id = Uuid::from_u128(62).to_string();

    let sent = sender
        .chat
        .send_message(&offline_peer_id, "original content")
        .await
        .expect("persist original message");
    let stored_before =
        wait_for_message_revision(&sender._storage, sent.id, "original content", 0).await;
    assert_eq!(stored_before.edited_at, None);

    let error = sender
        .chat
        .edit_message(
            &sent.id.to_string(),
            "hacked content",
            &unauthorized_sender_id,
        )
        .await
        .expect_err("unauthorized sender should not edit message");
    assert!(format!("{error}").contains("unauthorized"));

    let stored_after =
        wait_for_message_revision(&sender._storage, sent.id, "original content", 0).await;
    assert_eq!(stored_after.edited_at, None);

    sender.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn chat_delete_message_marks_storage_emits_event_and_sends_protocol_delete() {
    let sender = Node::new(63, "Delete Sender Node").await;
    let receiver = Node::new(64, "Delete Receiver Node").await;
    let mut receiver_server_events = receiver.server.subscribe();
    let _client = sender.connect_to(&receiver).await;

    match expect_server_event(&mut receiver_server_events).await {
        WsServerEvent::PeerConnected { peer } => {
            assert_eq!(peer.identity.device_id, sender.identity.device_id)
        }
        other => panic!("expected receiver peer connection, got {other:?}"),
    }

    let sent = sender
        .chat
        .send_message(&receiver.identity.device_id, "delete me")
        .await
        .expect("send original message");

    match expect_server_event(&mut receiver_server_events).await {
        WsServerEvent::MessageReceived { peer, message } => {
            assert_eq!(peer.identity.device_id, sender.identity.device_id);
            match message {
                ProtocolMessage::TextMessage { id, content, .. } => {
                    assert_eq!(id, sent.id.to_string());
                    assert_eq!(content, "delete me");
                }
                other => panic!("expected text message, got {other:?}"),
            }
        }
        other => panic!("expected original message event, got {other:?}"),
    }

    let _delivered = wait_for_status(
        &sender.chat,
        &receiver.identity.device_id,
        sent.id,
        MessageStatus::Delivered,
    )
    .await;
    let receiver_original =
        wait_for_message_revision(&receiver._storage, sent.id, "delete me", 0).await;
    assert!(!receiver_original.is_deleted);

    let mut sender_chat_events = sender.chat.subscribe();
    let mut receiver_chat_events = receiver.chat.subscribe();

    sender
        .chat
        .delete_message(&sent.id.to_string(), &sender.identity.device_id)
        .await
        .expect("delete sent message");

    match expect_chat_event(&mut sender_chat_events).await {
        ChatServiceEvent::MessageDeleted { message_id } => {
            assert_eq!(message_id, sent.id);
        }
        other => panic!("expected local message delete event, got {other:?}"),
    }

    let delete_timestamp_ms = match expect_server_event(&mut receiver_server_events).await {
        WsServerEvent::MessageReceived { peer, message } => {
            assert_eq!(peer.identity.device_id, sender.identity.device_id);
            match message {
                ProtocolMessage::MessageDelete {
                    message_id,
                    chat_id,
                    sender_id,
                    timestamp_ms,
                } => {
                    assert_eq!(message_id, sent.id.to_string());
                    assert_eq!(chat_id, receiver.identity.device_id);
                    assert_eq!(sender_id, sender.identity.device_id);
                    assert!(timestamp_ms > 0);
                    timestamp_ms
                }
                other => panic!("expected message delete, got {other:?}"),
            }
        }
        other => panic!("expected delete message event, got {other:?}"),
    };

    match expect_chat_event(&mut receiver_chat_events).await {
        ChatServiceEvent::MessageDeleted { message_id } => {
            assert_eq!(message_id, sent.id);
        }
        other => panic!("expected receiver message delete event, got {other:?}"),
    }

    let sender_deleted = wait_for_deleted_message(&sender._storage, sent.id).await;
    assert_eq!(sender_deleted.deleted_at, Some(delete_timestamp_ms));
    assert_eq!(sender_deleted.content, "delete me");

    let receiver_deleted = wait_for_deleted_message(&receiver._storage, sent.id).await;
    assert_eq!(receiver_deleted.deleted_at, Some(delete_timestamp_ms));
    assert_eq!(receiver_deleted.content, "delete me");

    sender.shutdown().await;
    receiver.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_delete_message_rejects_unauthorized_sender() {
    let sender = Node::new(65, "Sender Node").await;
    let offline_peer_id = Uuid::from_u128(66).to_string();
    let unauthorized_sender_id = Uuid::from_u128(67).to_string();

    let sent = sender
        .chat
        .send_message(&offline_peer_id, "original content")
        .await
        .expect("persist original message");
    let stored_before =
        wait_for_message_revision(&sender._storage, sent.id, "original content", 0).await;
    assert!(!stored_before.is_deleted);

    let error = sender
        .chat
        .delete_message(&sent.id.to_string(), &unauthorized_sender_id)
        .await
        .expect_err("unauthorized sender should not delete message");
    assert!(format!("{error}").contains("unauthorized"));

    let stored_after = sender
        ._storage
        .get_message(&sent.id.to_string())
        .await
        .expect("load message after unauthorized delete")
        .expect("message should still exist");
    assert!(!stored_after.is_deleted);
    assert_eq!(stored_after.deleted_at, None);

    sender.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_buffers_out_of_order_message_edit_until_original_message_arrives() {
    let receiver = Node::new(70, "Receiver Node").await;
    let sender_identity = peer(71, "Sender Node");
    let sender_client = Arc::new(
        WsClient::connect(
            ws_url(&receiver.server),
            client_config(sender_identity.clone()),
        )
        .await
        .expect("connect sender client"),
    );
    let mut sender_events = sender_client.subscribe();
    let mut receiver_chat_events = receiver.chat.subscribe();
    let message_id = Uuid::new_v4();
    let edit_timestamp_ms = 1_700_000_010_000;

    sender_client
        .send(ProtocolMessage::MessageEdit {
            message_id: message_id.to_string(),
            chat_id: receiver.identity.device_id.clone(),
            sender_id: sender_identity.device_id.clone(),
            new_content: "edited before original".to_string(),
            edit_version: 1,
            timestamp_ms: edit_timestamp_ms,
        })
        .await
        .expect("send pending edit first");

    time::sleep(Duration::from_millis(50)).await;
    assert!(receiver
        ._storage
        .get_message(&message_id.to_string())
        .await
        .expect("load pending message")
        .is_none());

    sender_client
        .send(ProtocolMessage::TextMessage {
            id: message_id.to_string(),
            chat_id: receiver.identity.device_id.clone(),
            sender_id: sender_identity.device_id.clone(),
            content: "original content".to_string(),
            timestamp: 1_700_000_000_000,
            reply_to_id: None,
            reply_to_preview: None,
        })
        .await
        .expect("send original text message");

    match expect_chat_event(&mut receiver_chat_events).await {
        ChatServiceEvent::MessageReceived { peer_id, message } => {
            assert_eq!(peer_id, sender_identity.device_id);
            assert_eq!(message.id, message_id);
            assert_eq!(message.content, "original content");
        }
        other => panic!("expected buffered message receive event, got {other:?}"),
    }

    match expect_chat_event(&mut receiver_chat_events).await {
        ChatServiceEvent::MessageEdited {
            message_id: edited_id,
            new_content,
            edit_version,
        } => {
            assert_eq!(edited_id, message_id);
            assert_eq!(new_content, "edited before original");
            assert_eq!(edit_version, 1);
        }
        other => panic!("expected buffered edit event, got {other:?}"),
    }

    let stored =
        wait_for_message_revision(&receiver._storage, message_id, "edited before original", 1)
            .await;
    assert_eq!(stored.edited_at, Some(edit_timestamp_ms));

    match expect_client_event(&mut sender_events).await {
        WsClientEvent::MessageReceived { message } => {
            assert_eq!(
                message,
                ProtocolMessage::Ack {
                    message_id: message_id.to_string(),
                    status: AckStatus::Received,
                }
            );
        }
        other => panic!("expected ack for original message, got {other:?}"),
    }

    sender_client
        .disconnect()
        .await
        .expect("disconnect sender client");
    receiver.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_buffers_out_of_order_message_delete_until_original_message_arrives() {
    let receiver = Node::new(72, "Receiver Node").await;
    let sender_identity = peer(73, "Sender Node");
    let sender_client = Arc::new(
        WsClient::connect(
            ws_url(&receiver.server),
            client_config(sender_identity.clone()),
        )
        .await
        .expect("connect sender client"),
    );
    let mut sender_events = sender_client.subscribe();
    let mut receiver_chat_events = receiver.chat.subscribe();
    let message_id = Uuid::new_v4();
    let delete_timestamp_ms = 1_700_000_020_000;

    sender_client
        .send(ProtocolMessage::MessageDelete {
            message_id: message_id.to_string(),
            chat_id: receiver.identity.device_id.clone(),
            sender_id: sender_identity.device_id.clone(),
            timestamp_ms: delete_timestamp_ms,
        })
        .await
        .expect("send pending delete first");

    time::sleep(Duration::from_millis(50)).await;
    assert!(receiver
        ._storage
        .get_message(&message_id.to_string())
        .await
        .expect("load pending delete message")
        .is_none());

    sender_client
        .send(ProtocolMessage::TextMessage {
            id: message_id.to_string(),
            chat_id: receiver.identity.device_id.clone(),
            sender_id: sender_identity.device_id.clone(),
            content: "original content".to_string(),
            timestamp: 1_700_000_000_000,
            reply_to_id: None,
            reply_to_preview: None,
        })
        .await
        .expect("send original text message");

    match expect_chat_event(&mut receiver_chat_events).await {
        ChatServiceEvent::MessageReceived { peer_id, message } => {
            assert_eq!(peer_id, sender_identity.device_id);
            assert_eq!(message.id, message_id);
            assert_eq!(message.content, "original content");
        }
        other => panic!("expected buffered message receive event, got {other:?}"),
    }

    match expect_chat_event(&mut receiver_chat_events).await {
        ChatServiceEvent::MessageDeleted {
            message_id: deleted_id,
        } => {
            assert_eq!(deleted_id, message_id);
        }
        other => panic!("expected buffered delete event, got {other:?}"),
    }

    let deleted = wait_for_deleted_message(&receiver._storage, message_id).await;
    assert_eq!(deleted.deleted_at, Some(delete_timestamp_ms));
    assert_eq!(deleted.content, "original content");

    match expect_client_event(&mut sender_events).await {
        WsClientEvent::MessageReceived { message } => {
            assert_eq!(
                message,
                ProtocolMessage::Ack {
                    message_id: message_id.to_string(),
                    status: AckStatus::Received,
                }
            );
        }
        other => panic!("expected ack for original message, got {other:?}"),
    }

    sender_client
        .disconnect()
        .await
        .expect("disconnect sender client");
    receiver.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_incoming_message_edits_use_version_then_timestamp_conflict_resolution() {
    let receiver = Node::new(80, "Receiver Node").await;
    let sender_identity = peer(81, "Sender Node");
    let sender_client = Arc::new(
        WsClient::connect(
            ws_url(&receiver.server),
            client_config(sender_identity.clone()),
        )
        .await
        .expect("connect sender client"),
    );
    let mut receiver_chat_events = receiver.chat.subscribe();
    let message_id = Uuid::new_v4();

    sender_client
        .send(ProtocolMessage::TextMessage {
            id: message_id.to_string(),
            chat_id: receiver.identity.device_id.clone(),
            sender_id: sender_identity.device_id.clone(),
            content: "original content".to_string(),
            timestamp: 1_700_000_000_000,
            reply_to_id: None,
            reply_to_preview: None,
        })
        .await
        .expect("send original text message");

    match expect_chat_event(&mut receiver_chat_events).await {
        ChatServiceEvent::MessageReceived { peer_id, message } => {
            assert_eq!(peer_id, sender_identity.device_id);
            assert_eq!(message.id, message_id);
            assert_eq!(message.content, "original content");
        }
        other => panic!("expected original receive event, got {other:?}"),
    }

    sender_client
        .send(ProtocolMessage::MessageEdit {
            message_id: message_id.to_string(),
            chat_id: receiver.identity.device_id.clone(),
            sender_id: sender_identity.device_id.clone(),
            new_content: "version 2 wins".to_string(),
            edit_version: 2,
            timestamp_ms: 200,
        })
        .await
        .expect("send winning higher-version edit");

    match expect_chat_event(&mut receiver_chat_events).await {
        ChatServiceEvent::MessageEdited {
            message_id: edited_id,
            new_content,
            edit_version,
        } => {
            assert_eq!(edited_id, message_id);
            assert_eq!(new_content, "version 2 wins");
            assert_eq!(edit_version, 2);
        }
        other => panic!("expected higher-version edit event, got {other:?}"),
    }

    let first_winner =
        wait_for_message_revision(&receiver._storage, message_id, "version 2 wins", 2).await;
    assert_eq!(first_winner.edited_at, Some(200));

    sender_client
        .send(ProtocolMessage::MessageEdit {
            message_id: message_id.to_string(),
            chat_id: receiver.identity.device_id.clone(),
            sender_id: sender_identity.device_id.clone(),
            new_content: "lower version loses".to_string(),
            edit_version: 1,
            timestamp_ms: 300,
        })
        .await
        .expect("send stale lower-version edit");

    assert!(
        time::timeout(Duration::from_millis(150), receiver_chat_events.recv())
            .await
            .is_err()
    );
    let after_lower_version = receiver
        ._storage
        .get_message(&message_id.to_string())
        .await
        .expect("load message after lower version")
        .expect("message exists after lower version");
    assert_eq!(after_lower_version.content, "version 2 wins");
    assert_eq!(after_lower_version.edited_at, Some(200));

    sender_client
        .send(ProtocolMessage::MessageEdit {
            message_id: message_id.to_string(),
            chat_id: receiver.identity.device_id.clone(),
            sender_id: sender_identity.device_id.clone(),
            new_content: "older same version loses".to_string(),
            edit_version: 2,
            timestamp_ms: 199,
        })
        .await
        .expect("send older same-version edit");

    assert!(
        time::timeout(Duration::from_millis(150), receiver_chat_events.recv())
            .await
            .is_err()
    );
    let after_older_same_version = receiver
        ._storage
        .get_message(&message_id.to_string())
        .await
        .expect("load message after older same version")
        .expect("message exists after older same version");
    assert_eq!(after_older_same_version.content, "version 2 wins");
    assert_eq!(after_older_same_version.edited_at, Some(200));

    sender_client
        .send(ProtocolMessage::MessageEdit {
            message_id: message_id.to_string(),
            chat_id: receiver.identity.device_id.clone(),
            sender_id: sender_identity.device_id.clone(),
            new_content: "newer same version wins".to_string(),
            edit_version: 2,
            timestamp_ms: 400,
        })
        .await
        .expect("send newer same-version edit");

    match expect_chat_event(&mut receiver_chat_events).await {
        ChatServiceEvent::MessageEdited {
            message_id: edited_id,
            new_content,
            edit_version,
        } => {
            assert_eq!(edited_id, message_id);
            assert_eq!(new_content, "newer same version wins");
            assert_eq!(edit_version, 2);
        }
        other => panic!("expected newer same-version edit event, got {other:?}"),
    }

    let final_message =
        wait_for_message_revision(&receiver._storage, message_id, "newer same version wins", 2)
            .await;
    assert_eq!(final_message.edited_at, Some(400));

    sender_client
        .disconnect()
        .await
        .expect("disconnect sender client");
    receiver.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_ignores_message_edit_after_message_delete() {
    let receiver = Node::new(82, "Receiver Node").await;
    let sender_identity = peer(83, "Sender Node");
    let sender_client = Arc::new(
        WsClient::connect(
            ws_url(&receiver.server),
            client_config(sender_identity.clone()),
        )
        .await
        .expect("connect sender client"),
    );
    let mut receiver_chat_events = receiver.chat.subscribe();
    let message_id = Uuid::new_v4();
    let delete_timestamp_ms = 500;

    sender_client
        .send(ProtocolMessage::TextMessage {
            id: message_id.to_string(),
            chat_id: receiver.identity.device_id.clone(),
            sender_id: sender_identity.device_id.clone(),
            content: "original content".to_string(),
            timestamp: 1_700_000_000_000,
            reply_to_id: None,
            reply_to_preview: None,
        })
        .await
        .expect("send original text message");

    match expect_chat_event(&mut receiver_chat_events).await {
        ChatServiceEvent::MessageReceived { peer_id, message } => {
            assert_eq!(peer_id, sender_identity.device_id);
            assert_eq!(message.id, message_id);
            assert_eq!(message.content, "original content");
        }
        other => panic!("expected original receive event, got {other:?}"),
    }

    sender_client
        .send(ProtocolMessage::MessageDelete {
            message_id: message_id.to_string(),
            chat_id: receiver.identity.device_id.clone(),
            sender_id: sender_identity.device_id.clone(),
            timestamp_ms: delete_timestamp_ms,
        })
        .await
        .expect("send delete message");

    match expect_chat_event(&mut receiver_chat_events).await {
        ChatServiceEvent::MessageDeleted {
            message_id: deleted_id,
        } => {
            assert_eq!(deleted_id, message_id);
        }
        other => panic!("expected message deleted event, got {other:?}"),
    }

    let deleted = wait_for_deleted_message(&receiver._storage, message_id).await;
    assert_eq!(deleted.deleted_at, Some(delete_timestamp_ms));
    assert_eq!(deleted.content, "original content");
    assert_eq!(deleted.edit_version, 0);
    assert_eq!(deleted.edited_at, None);

    sender_client
        .send(ProtocolMessage::MessageEdit {
            message_id: message_id.to_string(),
            chat_id: receiver.identity.device_id.clone(),
            sender_id: sender_identity.device_id.clone(),
            new_content: "should be ignored".to_string(),
            edit_version: 1,
            timestamp_ms: delete_timestamp_ms + 1,
        })
        .await
        .expect("send edit after delete");

    assert!(
        time::timeout(Duration::from_millis(150), receiver_chat_events.recv())
            .await
            .is_err()
    );

    let stored_after_ignored_edit = receiver
        ._storage
        .get_message(&message_id.to_string())
        .await
        .expect("load message after ignored edit")
        .expect("message exists after ignored edit");
    assert!(stored_after_ignored_edit.is_deleted);
    assert_eq!(
        stored_after_ignored_edit.deleted_at,
        Some(delete_timestamp_ms)
    );
    assert_eq!(stored_after_ignored_edit.content, "original content");
    assert_eq!(stored_after_ignored_edit.edit_version, 0);
    assert_eq!(stored_after_ignored_edit.edited_at, None);

    sender_client
        .disconnect()
        .await
        .expect("disconnect sender client");
    receiver.shutdown().await;
}
