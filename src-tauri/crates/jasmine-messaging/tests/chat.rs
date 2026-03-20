use std::sync::Arc;
use std::time::Duration;

use jasmine_core::{AckStatus, Message, MessageStatus, ProtocolMessage};
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
