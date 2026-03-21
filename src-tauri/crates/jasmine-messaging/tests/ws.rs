use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use jasmine_core::ProtocolMessage;
use jasmine_messaging::{
    WsClient, WsClientConfig, WsClientEvent, WsDisconnectReason, WsPeerConnection, WsPeerIdentity,
    WsServer, WsServerConfig, WsServerEvent,
};
use tokio::time;
use tokio_tungstenite::{connect_async, tungstenite::Message};

fn peer(device_id: &str, display_name: &str) -> WsPeerIdentity {
    WsPeerIdentity::new(device_id, display_name)
}

fn server_config() -> WsServerConfig {
    let mut config = WsServerConfig::new(peer("server-001", "Server Node"));
    config.bind_addr = "127.0.0.1:0".to_string();
    config.heartbeat_interval = Duration::from_millis(100);
    config.max_missed_heartbeats = 3;
    config.handshake_timeout = Duration::from_secs(1);
    config
}

fn client_config(device_id: &str, display_name: &str) -> WsClientConfig {
    let mut config = WsClientConfig::new(peer(device_id, display_name));
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ws_rejects_non_peer_info_first_message() {
    let server = WsServer::bind(server_config())
        .await
        .expect("bind ws server");
    let (mut socket, _) = connect_async(ws_url(&server))
        .await
        .expect("connect raw websocket");

    socket
        .send(Message::Text(
            ProtocolMessage::Ping
                .to_json()
                .expect("serialize ping")
                .into(),
        ))
        .await
        .expect("send invalid first message");

    let next = time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("server should close invalid client");

    match next {
        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {}
        other => panic!("unexpected websocket response: {other:?}"),
    }

    assert!(server.connected_peers().is_empty());
    server.shutdown().await.expect("shutdown ws server");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ws_client_authenticates_with_server_peer_info() {
    let server = WsServer::bind(server_config())
        .await
        .expect("bind ws server");
    let mut server_events = server.subscribe();

    let client = WsClient::connect(ws_url(&server), client_config("client-001", "Client One"))
        .await
        .expect("connect authenticated client");

    assert_eq!(
        client.remote_peer(),
        Some(WsPeerConnection {
            identity: peer("server-001", "Server Node"),
            address: server.local_addr().to_string(),
        })
    );

    match expect_server_event(&mut server_events).await {
        WsServerEvent::PeerConnected { peer: connection } => {
            assert_eq!(connection.identity, peer("client-001", "Client One"));
        }
        other => panic!("expected peer connection event, got {other:?}"),
    }

    client.disconnect().await.expect("disconnect client");
    server.shutdown().await.expect("shutdown ws server");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ws_roundtrip_json_messages_between_server_and_client() {
    let server = WsServer::bind(server_config())
        .await
        .expect("bind ws server");
    let mut server_events = server.subscribe();
    let client = WsClient::connect(
        ws_url(&server),
        client_config("client-101", "Client Roundtrip"),
    )
    .await
    .expect("connect authenticated client");
    let mut client_events = client.subscribe();

    match expect_server_event(&mut server_events).await {
        WsServerEvent::PeerConnected { .. } => {}
        other => panic!("expected peer connection event, got {other:?}"),
    }

    let outbound = ProtocolMessage::TextMessage {
        id: "msg-001".to_string(),
        chat_id: "chat-001".to_string(),
        sender_id: "client-101".to_string(),
        content: "hello over websocket".to_string(),
        timestamp: 1_700_000_000_000,
        reply_to_id: None,
        reply_to_preview: None,
    };

    client
        .send(outbound.clone())
        .await
        .expect("send client protocol message");

    match expect_server_event(&mut server_events).await {
        WsServerEvent::MessageReceived {
            peer: connection,
            message,
        } => {
            assert_eq!(connection.identity, peer("client-101", "Client Roundtrip"));
            assert_eq!(message, outbound);
        }
        other => panic!("expected message received event, got {other:?}"),
    }

    let response = ProtocolMessage::Ack {
        message_id: "msg-001".to_string(),
        status: jasmine_core::AckStatus::Received,
    };

    server
        .send_to("client-101", response.clone())
        .await
        .expect("send server response");

    match expect_client_event(&mut client_events).await {
        WsClientEvent::MessageReceived { message } => assert_eq!(message, response),
        other => panic!("expected client message event, got {other:?}"),
    }

    client.disconnect().await.expect("disconnect client");
    server.shutdown().await.expect("shutdown ws server");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ws_disconnect_detects_silent_peer_via_heartbeat_timeout() {
    let server = WsServer::bind(server_config())
        .await
        .expect("bind ws server");
    let mut server_events = server.subscribe();
    let (mut socket, _) = connect_async(ws_url(&server))
        .await
        .expect("connect raw websocket");

    socket
        .send(Message::Text(
            ProtocolMessage::PeerInfo {
                device_id: "silent-001".to_string(),
                display_name: "Silent Peer".to_string(),
                avatar_hash: None,
            }
            .to_json()
            .expect("serialize peer info")
            .into(),
        ))
        .await
        .expect("send handshake");

    let handshake = time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("server handshake timeout")
        .expect("server handshake response");

    match handshake {
        Ok(Message::Text(payload)) => {
            let message =
                ProtocolMessage::from_json(payload.as_str()).expect("parse server handshake");
            assert!(matches!(message, ProtocolMessage::PeerInfo { .. }));
        }
        other => panic!("unexpected server handshake frame: {other:?}"),
    }

    match expect_server_event(&mut server_events).await {
        WsServerEvent::PeerConnected { peer: connection } => {
            assert_eq!(connection.identity, peer("silent-001", "Silent Peer"));
        }
        other => panic!("expected peer connection event, got {other:?}"),
    }

    match expect_server_event(&mut server_events).await {
        WsServerEvent::PeerDisconnected {
            peer: connection,
            reason,
        } => {
            assert_eq!(connection.identity, peer("silent-001", "Silent Peer"));
            assert_eq!(reason, WsDisconnectReason::HeartbeatTimedOut);
        }
        other => panic!("expected peer disconnected event, got {other:?}"),
    }

    server.shutdown().await.expect("shutdown ws server");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn ws_multi_client_supports_message_isolation_and_reconnect() {
    let server = WsServer::bind(server_config())
        .await
        .expect("bind ws server");
    let mut server_events = server.subscribe();
    let mut clients = Vec::new();

    for index in 0..10 {
        let client = WsClient::connect(
            ws_url(&server),
            client_config(&format!("client-{index:03}"), &format!("Client {index}")),
        )
        .await
        .expect("connect concurrent client");
        clients.push(client);
    }

    let mut connected_ids = Vec::new();
    while connected_ids.len() < 10 {
        match expect_server_event(&mut server_events).await {
            WsServerEvent::PeerConnected { peer } => connected_ids.push(peer.identity.device_id),
            other => panic!("expected peer connected event, got {other:?}"),
        }
    }

    connected_ids.sort();
    assert_eq!(connected_ids.len(), 10);
    assert_eq!(server.connected_peers().len(), 10);

    for (index, client) in clients.iter().enumerate() {
        client
            .send(ProtocolMessage::TextMessage {
                id: format!("msg-{index:03}"),
                chat_id: "chat-multi".to_string(),
                sender_id: format!("client-{index:03}"),
                content: format!("payload-{index}"),
                timestamp: 1_700_000_000_000 + index as i64,
                reply_to_id: None,
                reply_to_preview: None,
            })
            .await
            .expect("send isolated client message");
    }

    let mut received_ids = Vec::new();
    while received_ids.len() < 10 {
        match expect_server_event(&mut server_events).await {
            WsServerEvent::MessageReceived { peer, message } => {
                received_ids.push(peer.identity.device_id.clone());
                assert!(matches!(message, ProtocolMessage::TextMessage { .. }));
            }
            other => panic!("expected message received event, got {other:?}"),
        }
    }

    received_ids.sort();
    assert_eq!(received_ids.len(), 10);

    clients
        .remove(0)
        .disconnect()
        .await
        .expect("disconnect first client");

    match expect_server_event(&mut server_events).await {
        WsServerEvent::PeerDisconnected { peer, reason } => {
            assert_eq!(peer.identity.device_id, "client-000");
            assert_eq!(reason, WsDisconnectReason::LocalClosed);
        }
        other => panic!("expected disconnect event, got {other:?}"),
    }

    let reconnected = WsClient::connect(ws_url(&server), client_config("client-000", "Client 0"))
        .await
        .expect("reconnect first client");

    match expect_server_event(&mut server_events).await {
        WsServerEvent::PeerConnected { peer } => {
            assert_eq!(peer.identity.device_id, "client-000");
        }
        other => panic!("expected reconnect event, got {other:?}"),
    }

    reconnected
        .send(ProtocolMessage::TextMessage {
            id: "msg-reconnect".to_string(),
            chat_id: "chat-multi".to_string(),
            sender_id: "client-000".to_string(),
            content: "reconnected payload".to_string(),
            timestamp: 1_700_000_000_100,
            reply_to_id: None,
            reply_to_preview: None,
        })
        .await
        .expect("send reconnect message");

    match expect_server_event(&mut server_events).await {
        WsServerEvent::MessageReceived { peer, .. } => {
            assert_eq!(peer.identity.device_id, "client-000");
        }
        other => panic!("expected reconnect message event, got {other:?}"),
    }

    reconnected
        .disconnect()
        .await
        .expect("disconnect reconnect client");
    for client in clients {
        client.disconnect().await.expect("disconnect client");
    }
    server.shutdown().await.expect("shutdown ws server");
}
