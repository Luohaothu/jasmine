use std::sync::Arc;
use std::time::Duration;

use jasmine_core::{ChatId, GroupInfo, Message, ProtocolMessage, UserId};
use jasmine_messaging::{
    ChatService, ChatServiceConfig, ChatServiceEvent, WsClient, WsClientConfig, WsPeerIdentity,
    WsServer, WsServerConfig, WsServerEvent,
};
use jasmine_storage::{ChatType, SqliteStorage};
use tempfile::TempDir;
use tokio::time;
use uuid::Uuid;

const TEST_ACK_TIMEOUT: Duration = Duration::from_millis(250);

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
async fn group_fanout_delivers_to_all_online_members_with_group_chat_id() {
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
