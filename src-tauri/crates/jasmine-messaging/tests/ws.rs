use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use jasmine_core::{AckStatus, ProtocolMessage, StorageEngine, CURRENT_PROTOCOL_VERSION};
use jasmine_crypto::{
    fingerprint, public_key_from_base64, public_key_to_base64, EncryptedFrame, PublicKey, Session,
    SessionKeyManager, StaticSecret,
};
use jasmine_messaging::{
    WsClient, WsClientConfig, WsClientEvent, WsDisconnectReason, WsPeerConnection, WsPeerIdentity,
    WsServer, WsServerConfig, WsServerEvent,
};
use jasmine_storage::SqliteStorage;
use tempfile::TempDir;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::time;
use tokio_tungstenite::{
    accept_async, connect_async,
    tungstenite::{protocol::frame::coding::CloseCode, Message},
    WebSocketStream,
};

const ENCRYPTED_PROTOCOL_FRAME_TYPE: u32 = 0x01;
const MESSAGE_NONCE_PREFIX_LEN: usize = 4;
const TRANSPORT_FRAME_TYPE_LEN: usize = 4;

fn peer(device_id: &str, display_name: &str) -> WsPeerIdentity {
    let secret = StaticSecret::from(deterministic_private_key_bytes(device_id));
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

fn peer_info_message(identity: &WsPeerIdentity) -> ProtocolMessage {
    ProtocolMessage::PeerInfo {
        device_id: identity.device_id.clone(),
        display_name: identity.display_name.clone(),
        avatar_hash: identity.avatar_hash.clone(),
        public_key: Some(identity.public_key.clone()),
        protocol_version: Some(identity.protocol_version),
    }
}

fn parse_peer_info_message(message: ProtocolMessage) -> WsPeerIdentity {
    match message {
        ProtocolMessage::PeerInfo {
            device_id,
            display_name,
            avatar_hash,
            public_key: Some(public_key),
            protocol_version: Some(protocol_version),
        } => WsPeerIdentity {
            device_id,
            display_name,
            avatar_hash,
            public_key,
            protocol_version,
        },
        other => panic!("expected peer info, got {other:?}"),
    }
}

fn server_config(
    identity: WsPeerIdentity,
    peer_key_store: Option<Arc<SqliteStorage>>,
) -> WsServerConfig {
    let mut config = WsServerConfig::new(identity.clone());
    config.bind_addr = "127.0.0.1:0".to_string();
    config.local_private_key = Some(local_private_key(&identity.device_id));
    config.peer_key_store = peer_key_store;
    config.heartbeat_interval = Duration::from_millis(100);
    config.max_missed_heartbeats = 3;
    config.handshake_timeout = Duration::from_secs(1);
    config
}

fn client_config(
    identity: WsPeerIdentity,
    peer_key_store: Option<Arc<SqliteStorage>>,
) -> WsClientConfig {
    let mut config = WsClientConfig::new(identity.clone());
    config.local_private_key = Some(local_private_key(&identity.device_id));
    config.peer_key_store = peer_key_store;
    config.heartbeat_interval = Duration::from_millis(100);
    config.max_missed_heartbeats = 3;
    config.handshake_timeout = Duration::from_secs(1);
    config
}

fn temp_storage() -> (TempDir, Arc<SqliteStorage>) {
    let temp_dir = TempDir::new().expect("create temp dir");
    let storage = Arc::new(
        SqliteStorage::open(temp_dir.path().join("messages.sqlite3")).expect("open sqlite storage"),
    );
    (temp_dir, storage)
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

async fn expect_protocol_message<S>(socket: &mut WebSocketStream<S>, label: &str) -> ProtocolMessage
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let frame = match time::timeout(Duration::from_secs(2), socket.next()).await {
        Ok(Some(frame)) => frame,
        Ok(None) => panic!("{label} missing frame"),
        Err(_) => panic!("{label} timeout"),
    };

    match frame {
        Ok(Message::Text(payload)) => {
            ProtocolMessage::from_json(payload.as_str()).expect("parse protocol message")
        }
        Ok(Message::Binary(payload)) => {
            let text = std::str::from_utf8(payload.as_ref()).expect("binary frame is utf8");
            ProtocolMessage::from_json(text).expect("parse binary protocol message")
        }
        other => panic!("unexpected websocket frame for {label}: {other:?}"),
    }
}

async fn expect_text_protocol_message<S>(
    socket: &mut WebSocketStream<S>,
    label: &str,
) -> ProtocolMessage
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let frame = match time::timeout(Duration::from_secs(2), socket.next()).await {
        Ok(Some(frame)) => frame,
        Ok(None) => panic!("{label} missing frame"),
        Err(_) => panic!("{label} timeout"),
    };

    match frame {
        Ok(Message::Text(payload)) => {
            ProtocolMessage::from_json(payload.as_str()).expect("parse text protocol message")
        }
        other => panic!("expected text frame for {label}, got {other:?}"),
    }
}

async fn expect_binary_frame_payload<S>(socket: &mut WebSocketStream<S>, label: &str) -> Vec<u8>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let frame = match time::timeout(Duration::from_secs(2), socket.next()).await {
        Ok(Some(frame)) => frame,
        Ok(None) => panic!("{label} missing frame"),
        Err(_) => panic!("{label} timeout"),
    };

    match frame {
        Ok(Message::Binary(payload)) => payload.to_vec(),
        other => panic!("expected binary frame for {label}, got {other:?}"),
    }
}

async fn expect_application_protocol_frame<S>(
    socket: &mut WebSocketStream<S>,
    session: &mut Session,
    label: &str,
) -> (Vec<u8>, ProtocolMessage)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        let payload = expect_binary_frame_payload(socket, label).await;
        let message = decrypt_transport_frame(session, &payload);
        if message == ProtocolMessage::Ping {
            socket
                .send(Message::Binary(
                    encrypt_transport_frame(session, &ProtocolMessage::Pong).into(),
                ))
                .await
                .expect("send encrypted pong from raw client");
            continue;
        }

        return (payload, message);
    }
}

struct RawClientHandshakeOutcome {
    remote_identity: WsPeerIdentity,
    session: Session,
}

fn encrypt_transport_frame(session: &mut Session, message: &ProtocolMessage) -> Vec<u8> {
    let payload = message
        .to_json()
        .expect("serialize encrypted transport protocol message");
    let frame = session
        .encrypt_message(payload.as_bytes())
        .expect("encrypt transport protocol message");
    let mut output = Vec::with_capacity(TRANSPORT_FRAME_TYPE_LEN + 12 + frame.ciphertext.len());
    output.extend_from_slice(&ENCRYPTED_PROTOCOL_FRAME_TYPE.to_be_bytes());
    output.extend_from_slice(&frame.to_bytes());
    output
}

fn decrypt_transport_frame(session: &mut Session, payload: &[u8]) -> ProtocolMessage {
    assert!(
        payload.len() > TRANSPORT_FRAME_TYPE_LEN,
        "encrypted transport payload must include type prefix and frame bytes"
    );
    assert_eq!(
        u32::from_be_bytes(
            payload[..TRANSPORT_FRAME_TYPE_LEN]
                .try_into()
                .expect("frame prefix")
        ),
        ENCRYPTED_PROTOCOL_FRAME_TYPE,
        "encrypted transport frame type mismatch"
    );
    let frame = EncryptedFrame::from_bytes(&payload[TRANSPORT_FRAME_TYPE_LEN..])
        .expect("decode encrypted transport frame");
    let plaintext = session
        .decrypt_message(&frame)
        .expect("decrypt transport frame");
    let text = std::str::from_utf8(&plaintext).expect("decrypted transport payload is utf8");
    ProtocolMessage::from_json(text).expect("parse decrypted transport message")
}

fn decode_transport_encrypted_frame(payload: &[u8]) -> EncryptedFrame {
    assert!(
        payload.len() > TRANSPORT_FRAME_TYPE_LEN,
        "encrypted transport payload must include type prefix and frame bytes"
    );
    assert_eq!(
        u32::from_be_bytes(
            payload[..TRANSPORT_FRAME_TYPE_LEN]
                .try_into()
                .expect("frame prefix")
        ),
        ENCRYPTED_PROTOCOL_FRAME_TYPE,
        "encrypted transport frame type mismatch"
    );

    EncryptedFrame::from_bytes(&payload[TRANSPORT_FRAME_TYPE_LEN..])
        .expect("decode encrypted transport frame")
}

fn assert_message_nonce_layout(frame: &EncryptedFrame, nonce_prefix: &[u8]) -> u64 {
    assert_eq!(
        &frame.nonce[..MESSAGE_NONCE_PREFIX_LEN],
        &nonce_prefix[..MESSAGE_NONCE_PREFIX_LEN],
        "message nonce must use the first 4 bytes of the session nonce prefix"
    );

    u64::from_be_bytes(
        frame.nonce[MESSAGE_NONCE_PREFIX_LEN..]
            .try_into()
            .expect("message nonce counter bytes"),
    )
}

fn tamper_transport_frame(payload: &mut [u8]) {
    assert!(
        payload.len() > TRANSPORT_FRAME_TYPE_LEN + 4,
        "tamper helper requires ciphertext bytes after frame prefix"
    );
    payload[TRANSPORT_FRAME_TYPE_LEN + 4] ^= 0x01;
}

async fn complete_raw_client_key_exchange_with_session<S>(
    socket: &mut WebSocketStream<S>,
    local_identity: &WsPeerIdentity,
) -> RawClientHandshakeOutcome
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    socket
        .send(Message::Text(
            peer_info_message(local_identity)
                .to_json()
                .expect("serialize peer info")
                .into(),
        ))
        .await
        .expect("send raw peer info");

    let remote_identity =
        parse_peer_info_message(expect_text_protocol_message(socket, "server peer info").await);
    let remote_public_key =
        public_key_from_base64(&remote_identity.public_key).expect("decode remote public key");
    let local_secret =
        StaticSecret::from(deterministic_private_key_bytes(&local_identity.device_id));
    let (ephemeral_public_key, pending) =
        SessionKeyManager::initiate_key_exchange(&local_secret, &remote_public_key);

    socket
        .send(Message::Text(
            ProtocolMessage::KeyExchangeInit {
                ephemeral_public_key: public_key_to_base64(&ephemeral_public_key),
                protocol_version: CURRENT_PROTOCOL_VERSION,
            }
            .to_json()
            .expect("serialize key exchange init")
            .into(),
        ))
        .await
        .expect("send key exchange init");

    let response = expect_text_protocol_message(socket, "server key exchange response").await;
    let server_ephemeral_public_key = match response {
        ProtocolMessage::KeyExchangeResponse {
            ephemeral_public_key,
        } => public_key_from_base64(&ephemeral_public_key)
            .expect("decode server ephemeral public key"),
        other => panic!("expected key exchange response, got {other:?}"),
    };

    let session =
        SessionKeyManager::complete_key_exchange_initiator(pending, &server_ephemeral_public_key)
            .expect("complete initiator session");

    RawClientHandshakeOutcome {
        remote_identity,
        session,
    }
}

async fn complete_raw_client_key_exchange<S>(
    socket: &mut WebSocketStream<S>,
    local_identity: &WsPeerIdentity,
) -> WsPeerIdentity
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    complete_raw_client_key_exchange_with_session(socket, local_identity)
        .await
        .remote_identity
}

async fn expect_handshake_rejection<S>(socket: &mut WebSocketStream<S>, label: &str)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    for _ in 0..8 {
        let frame = time::timeout(Duration::from_secs(2), socket.next())
            .await
            .unwrap_or_else(|_| panic!("{label} timeout waiting for rejection"));

        match frame {
            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
            Some(Ok(_)) => continue,
        }
    }

    panic!("expected rejection frame for {label}, but connection stayed open");
}

async fn expect_handshake_close<S>(
    socket: &mut WebSocketStream<S>,
    label: &str,
) -> (CloseCode, String)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    for _ in 0..8 {
        let frame = time::timeout(Duration::from_secs(2), socket.next())
            .await
            .unwrap_or_else(|_| panic!("{label} timeout waiting for close reason"));

        match frame {
            Some(Ok(Message::Close(Some(frame)))) => return (frame.code, frame.reason.to_string()),
            Some(Ok(Message::Close(None))) | Some(Err(_)) | None => {
                panic!("expected close reason frame for {label}")
            }
            Some(Ok(_)) => continue,
        }
    }

    panic!("expected close reason frame for {label}, but connection stayed open");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ws_rejects_non_peer_info_first_message() {
    let server = WsServer::bind(server_config(peer("server-001", "Server Node"), None))
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

    expect_handshake_rejection(&mut socket, "non-peer-info first message").await;

    assert!(server.connected_peers().is_empty());
    server.shutdown().await.expect("shutdown ws server");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ws_key_exchange_client_sends_init_after_peer_info() {
    let client_identity = peer("client-001", "Client One");
    let server_identity = peer("server-001", "Server Node");
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind raw test listener");
    let address = listener.local_addr().expect("listener address");
    let expected_client_identity = client_identity.clone();
    let expected_server_identity = server_identity.clone();

    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept ws client");
        let mut socket = accept_async(stream).await.expect("accept websocket");

        let first = expect_protocol_message(&mut socket, "client peer info").await;
        assert_eq!(
            parse_peer_info_message(first),
            expected_client_identity.clone()
        );

        socket
            .send(Message::Text(
                peer_info_message(&expected_server_identity)
                    .to_json()
                    .expect("serialize server peer info")
                    .into(),
            ))
            .await
            .expect("send server peer info");

        let second = expect_protocol_message(&mut socket, "client key exchange init").await;
        let client_ephemeral_public_key = match second {
            ProtocolMessage::KeyExchangeInit {
                ephemeral_public_key,
                protocol_version,
            } => {
                assert_eq!(protocol_version, CURRENT_PROTOCOL_VERSION);
                public_key_from_base64(&ephemeral_public_key)
                    .expect("decode initiator ephemeral public key")
            }
            other => panic!("expected key exchange init, got {other:?}"),
        };

        let server_secret = StaticSecret::from(deterministic_private_key_bytes(
            &expected_server_identity.device_id,
        ));
        let client_public_key = public_key_from_base64(&expected_client_identity.public_key)
            .expect("decode client static public key");
        let (server_ephemeral_public_key, _) = SessionKeyManager::accept_key_exchange(
            &server_secret,
            &client_public_key,
            &client_ephemeral_public_key,
        )
        .expect("accept key exchange");

        socket
            .send(Message::Text(
                ProtocolMessage::KeyExchangeResponse {
                    ephemeral_public_key: public_key_to_base64(&server_ephemeral_public_key),
                }
                .to_json()
                .expect("serialize key exchange response")
                .into(),
            ))
            .await
            .expect("send key exchange response");

        time::sleep(Duration::from_millis(100)).await;
    });

    let client = WsClient::connect(
        format!("ws://{address}"),
        client_config(client_identity.clone(), None),
    )
    .await
    .expect("connect ws client through raw server handshake");

    assert_eq!(
        client.remote_peer(),
        Some(WsPeerConnection {
            identity: server_identity,
            address: address.to_string(),
        })
    );

    client.disconnect().await.expect("disconnect ws client");
    server_task.await.expect("await raw server task");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ws_client_authenticates_with_server_peer_info() {
    let server_identity = peer("server-001", "Server Node");
    let client_identity = peer("client-001", "Client One");
    let server = WsServer::bind(server_config(server_identity.clone(), None))
        .await
        .expect("bind ws server");
    let mut server_events = server.subscribe();

    let client = WsClient::connect(
        ws_url(&server),
        client_config(client_identity.clone(), None),
    )
    .await
    .expect("connect authenticated client");

    assert_eq!(
        client.remote_peer(),
        Some(WsPeerConnection {
            identity: server_identity,
            address: server.local_addr().to_string(),
        })
    );

    match expect_server_event(&mut server_events).await {
        WsServerEvent::PeerConnected { peer: connection } => {
            assert_eq!(connection.identity, client_identity);
        }
        other => panic!("expected peer connection event, got {other:?}"),
    }

    client.disconnect().await.expect("disconnect client");
    server.shutdown().await.expect("shutdown ws server");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ws_key_exchange_establishes_session_and_persists_peer_public_key() {
    let server_identity = peer("server-010", "Server Key Exchange");
    let client_identity = peer("client-010", "Client Key Exchange");
    let (_server_dir, server_store) = temp_storage();
    let (_client_dir, client_store) = temp_storage();
    let server = WsServer::bind(server_config(
        server_identity.clone(),
        Some(Arc::clone(&server_store)),
    ))
    .await
    .expect("bind ws server");
    let mut server_events = server.subscribe();

    let client = WsClient::connect(
        ws_url(&server),
        client_config(client_identity.clone(), Some(Arc::clone(&client_store))),
    )
    .await
    .expect("connect key exchange client");

    match expect_server_event(&mut server_events).await {
        WsServerEvent::PeerConnected { peer: connection } => {
            assert_eq!(connection.identity, client_identity.clone());
        }
        other => panic!("expected peer connection event, got {other:?}"),
    }

    let server_public_key =
        public_key_from_base64(&server_identity.public_key).expect("decode server public key");
    let client_public_key =
        public_key_from_base64(&client_identity.public_key).expect("decode client public key");

    let stored_server_key = client_store
        .get_peer_key(&server_identity.device_id)
        .await
        .expect("load stored server key")
        .expect("server key persisted");
    assert_eq!(
        stored_server_key.public_key,
        server_public_key.to_bytes().to_vec()
    );
    assert_eq!(
        stored_server_key.fingerprint,
        fingerprint(&server_public_key)
    );

    let stored_client_key = server_store
        .get_peer_key(&client_identity.device_id)
        .await
        .expect("load stored client key")
        .expect("client key persisted");
    assert_eq!(
        stored_client_key.public_key,
        client_public_key.to_bytes().to_vec()
    );
    assert_eq!(
        stored_client_key.fingerprint,
        fingerprint(&client_public_key)
    );

    let outbound = ProtocolMessage::TextMessage {
        id: "msg-key-exchange".to_string(),
        chat_id: "chat-key-exchange".to_string(),
        sender_id: client_identity.device_id.clone(),
        content: "handshake complete".to_string(),
        timestamp: 1_700_000_000_000,
        reply_to_id: None,
        reply_to_preview: None,
    };
    client
        .send(outbound.clone())
        .await
        .expect("send post-handshake protocol message");

    match expect_server_event(&mut server_events).await {
        WsServerEvent::MessageReceived { peer, message } => {
            assert_eq!(peer.identity, client_identity);
            assert_eq!(message, outbound);
        }
        other => panic!("expected post-handshake message, got {other:?}"),
    }

    client.disconnect().await.expect("disconnect client");
    server.shutdown().await.expect("shutdown ws server");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ws_roundtrip_json_messages_between_server_and_client() {
    let server_identity = peer("server-101", "Server Roundtrip");
    let client_identity = peer("client-101", "Client Roundtrip");
    let server = WsServer::bind(server_config(server_identity, None))
        .await
        .expect("bind ws server");
    let mut server_events = server.subscribe();
    let client = WsClient::connect(
        ws_url(&server),
        client_config(client_identity.clone(), None),
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
        sender_id: client_identity.device_id.clone(),
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
            assert_eq!(connection.identity, client_identity);
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ws_encrypted_roundtrip_after_handshake_uses_binary_frames() {
    let server = WsServer::bind(server_config(peer("server-111", "Server Node"), None))
        .await
        .expect("bind ws server");
    let mut server_events = server.subscribe();
    let (mut socket, _) = connect_async(ws_url(&server))
        .await
        .expect("connect raw websocket client");
    let client_identity = peer("client-111", "Client Raw");
    let mut handshake =
        complete_raw_client_key_exchange_with_session(&mut socket, &client_identity).await;
    let (_, nonce_prefix) = handshake.session.file_crypto_material();

    assert_eq!(handshake.remote_identity, peer("server-111", "Server Node"));
    match expect_server_event(&mut server_events).await {
        WsServerEvent::PeerConnected { peer } => {
            assert_eq!(peer.identity, client_identity);
        }
        other => panic!("expected peer connected event, got {other:?}"),
    }

    let outbound = ProtocolMessage::TextMessage {
        id: "msg-encrypted-roundtrip".to_string(),
        chat_id: "chat-encrypted-roundtrip".to_string(),
        sender_id: client_identity.device_id.clone(),
        content: "encrypted hello".to_string(),
        timestamp: 1_700_000_000_000,
        reply_to_id: None,
        reply_to_preview: None,
    };
    let encrypted_outbound = encrypt_transport_frame(&mut handshake.session, &outbound);
    assert_eq!(
        u32::from_be_bytes(
            encrypted_outbound[..TRANSPORT_FRAME_TYPE_LEN]
                .try_into()
                .expect("frame prefix"),
        ),
        ENCRYPTED_PROTOCOL_FRAME_TYPE,
    );
    socket
        .send(Message::Binary(encrypted_outbound.into()))
        .await
        .expect("send encrypted outbound message");

    match expect_server_event(&mut server_events).await {
        WsServerEvent::MessageReceived { peer, message } => {
            assert_eq!(peer.identity, client_identity);
            assert_eq!(message, outbound);
        }
        other => panic!("expected decrypted message event, got {other:?}"),
    }

    let response = ProtocolMessage::Ack {
        message_id: "msg-encrypted-roundtrip".to_string(),
        status: AckStatus::Received,
    };
    server
        .send_to(&client_identity.device_id, response.clone())
        .await
        .expect("send encrypted server response");

    let (encrypted_response, decrypted_response) = expect_application_protocol_frame(
        &mut socket,
        &mut handshake.session,
        "encrypted response",
    )
    .await;
    let encrypted_response_frame = decode_transport_encrypted_frame(&encrypted_response);
    let response_counter = assert_message_nonce_layout(&encrypted_response_frame, &nonce_prefix);
    assert_eq!(
        u32::from_be_bytes(
            encrypted_response[..TRANSPORT_FRAME_TYPE_LEN]
                .try_into()
                .expect("frame prefix"),
        ),
        ENCRYPTED_PROTOCOL_FRAME_TYPE,
    );
    assert_eq!(
        encrypted_response_frame.nonce[MESSAGE_NONCE_PREFIX_LEN..],
        response_counter.to_be_bytes(),
    );
    assert_eq!(decrypted_response, response);

    server.shutdown().await.expect("shutdown ws server");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ws_encrypted_transport_multiple_messages_use_binary_frames_and_unique_nonces() {
    let server = WsServer::bind(server_config(peer("server-112", "Server Node"), None))
        .await
        .expect("bind ws server");
    let mut server_events = server.subscribe();
    let (mut socket, _) = connect_async(ws_url(&server))
        .await
        .expect("connect raw websocket client");
    let client_identity = peer("client-112", "Client Raw");
    let mut handshake =
        complete_raw_client_key_exchange_with_session(&mut socket, &client_identity).await;
    let (_, nonce_prefix) = handshake.session.file_crypto_material();

    match expect_server_event(&mut server_events).await {
        WsServerEvent::PeerConnected { peer } => assert_eq!(peer.identity, client_identity),
        other => panic!("expected peer connected event, got {other:?}"),
    }

    let first = ProtocolMessage::Ack {
        message_id: "msg-transport-1".to_string(),
        status: AckStatus::Received,
    };
    let second = ProtocolMessage::Ack {
        message_id: "msg-transport-2".to_string(),
        status: AckStatus::Read,
    };
    server
        .send_to(&client_identity.device_id, first.clone())
        .await
        .expect("send first encrypted frame");
    server
        .send_to(&client_identity.device_id, second.clone())
        .await
        .expect("send second encrypted frame");

    let (first_frame, first_message) = expect_application_protocol_frame(
        &mut socket,
        &mut handshake.session,
        "first encrypted frame",
    )
    .await;
    let (second_frame, second_message) = expect_application_protocol_frame(
        &mut socket,
        &mut handshake.session,
        "second encrypted frame",
    )
    .await;
    let first_encrypted_frame = decode_transport_encrypted_frame(&first_frame);
    let second_encrypted_frame = decode_transport_encrypted_frame(&second_frame);
    let first_counter = assert_message_nonce_layout(&first_encrypted_frame, &nonce_prefix);
    let second_counter = assert_message_nonce_layout(&second_encrypted_frame, &nonce_prefix);

    assert_eq!(
        u32::from_be_bytes(
            first_frame[..TRANSPORT_FRAME_TYPE_LEN]
                .try_into()
                .expect("first frame prefix"),
        ),
        ENCRYPTED_PROTOCOL_FRAME_TYPE,
    );
    assert_eq!(
        u32::from_be_bytes(
            second_frame[..TRANSPORT_FRAME_TYPE_LEN]
                .try_into()
                .expect("second frame prefix"),
        ),
        ENCRYPTED_PROTOCOL_FRAME_TYPE,
    );
    assert_ne!(first_frame, second_frame);
    assert_ne!(
        &first_frame[TRANSPORT_FRAME_TYPE_LEN..TRANSPORT_FRAME_TYPE_LEN + 12],
        &second_frame[TRANSPORT_FRAME_TYPE_LEN..TRANSPORT_FRAME_TYPE_LEN + 12],
    );
    assert!(
        first_counter < second_counter,
        "message nonce counters must increase monotonically on the wire"
    );
    assert_eq!(first_message, first);
    assert_eq!(second_message, second);

    server.shutdown().await.expect("shutdown ws server");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ws_encrypted_tamper_closes_connection_with_protocol_violation() {
    let server = WsServer::bind(server_config(peer("server-113", "Server Node"), None))
        .await
        .expect("bind ws server");
    let mut server_events = server.subscribe();
    let (mut socket, _) = connect_async(ws_url(&server))
        .await
        .expect("connect raw websocket client");
    let client_identity = peer("client-113", "Client Raw");
    let mut handshake =
        complete_raw_client_key_exchange_with_session(&mut socket, &client_identity).await;

    match expect_server_event(&mut server_events).await {
        WsServerEvent::PeerConnected { peer } => assert_eq!(peer.identity, client_identity),
        other => panic!("expected peer connected event, got {other:?}"),
    }

    let outbound = ProtocolMessage::TextMessage {
        id: "msg-encrypted-tamper".to_string(),
        chat_id: "chat-encrypted-tamper".to_string(),
        sender_id: client_identity.device_id.clone(),
        content: "tamper this payload".to_string(),
        timestamp: 1_700_000_000_100,
        reply_to_id: None,
        reply_to_preview: None,
    };
    let mut tampered_frame = encrypt_transport_frame(&mut handshake.session, &outbound);
    tamper_transport_frame(&mut tampered_frame);
    socket
        .send(Message::Binary(tampered_frame.into()))
        .await
        .expect("send tampered encrypted frame");

    match expect_server_event(&mut server_events).await {
        WsServerEvent::PeerDisconnected { peer, reason } => {
            assert_eq!(peer.identity, client_identity);
            assert_eq!(reason, WsDisconnectReason::ProtocolViolation);
        }
        other => panic!("expected protocol violation disconnect, got {other:?}"),
    }

    expect_handshake_rejection(&mut socket, "tampered encrypted rejection").await;
    server.shutdown().await.expect("shutdown ws server");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ws_plaintext_after_handshake_is_rejected() {
    let server = WsServer::bind(server_config(peer("server-114", "Server Node"), None))
        .await
        .expect("bind ws server");
    let mut server_events = server.subscribe();
    let (mut socket, _) = connect_async(ws_url(&server))
        .await
        .expect("connect raw websocket client");
    let client_identity = peer("client-114", "Client Raw");
    let handshake =
        complete_raw_client_key_exchange_with_session(&mut socket, &client_identity).await;

    assert_eq!(handshake.remote_identity, peer("server-114", "Server Node"));
    match expect_server_event(&mut server_events).await {
        WsServerEvent::PeerConnected { peer } => assert_eq!(peer.identity, client_identity),
        other => panic!("expected peer connected event, got {other:?}"),
    }

    socket
        .send(Message::Text(
            ProtocolMessage::TextMessage {
                id: "msg-plaintext-after-handshake".to_string(),
                chat_id: "chat-plaintext-after-handshake".to_string(),
                sender_id: client_identity.device_id.clone(),
                content: "plaintext should fail".to_string(),
                timestamp: 1_700_000_000_200,
                reply_to_id: None,
                reply_to_preview: None,
            }
            .to_json()
            .expect("serialize plaintext post-handshake message")
            .into(),
        ))
        .await
        .expect("send plaintext frame after handshake");

    match expect_server_event(&mut server_events).await {
        WsServerEvent::PeerDisconnected { peer, reason } => {
            assert_eq!(peer.identity, client_identity);
            assert_eq!(reason, WsDisconnectReason::ProtocolViolation);
        }
        other => panic!("expected protocol violation disconnect, got {other:?}"),
    }

    expect_handshake_rejection(&mut socket, "plaintext after handshake rejection").await;
    server.shutdown().await.expect("shutdown ws server");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ws_key_exchange_incompatible_peer_without_public_key_is_rejected() {
    let server = WsServer::bind(server_config(peer("server-020", "Server Node"), None))
        .await
        .expect("bind ws server");
    let (mut socket, _) = connect_async(ws_url(&server))
        .await
        .expect("connect raw websocket");

    socket
        .send(Message::Text(
            ProtocolMessage::PeerInfo {
                device_id: "legacy-peer".to_string(),
                display_name: "Legacy Peer".to_string(),
                avatar_hash: None,
                public_key: None,
                protocol_version: None,
            }
            .to_json()
            .expect("serialize legacy peer info")
            .into(),
        ))
        .await
        .expect("send legacy peer info");

    expect_handshake_rejection(&mut socket, "missing public key rejection").await;

    assert!(server.connected_peers().is_empty());
    server.shutdown().await.expect("shutdown ws server");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ws_protocol_version_incompatible_peer_below_v2_receives_version_message_before_policy_close(
) {
    let server = WsServer::bind(server_config(peer("server-020b", "Server Node"), None))
        .await
        .expect("bind ws server");
    let (mut socket, _) = connect_async(ws_url(&server))
        .await
        .expect("connect raw websocket");
    let legacy_identity = peer("legacy-020b", "Legacy Peer");

    socket
        .send(Message::Text(
            ProtocolMessage::PeerInfo {
                device_id: legacy_identity.device_id.clone(),
                display_name: legacy_identity.display_name.clone(),
                avatar_hash: legacy_identity.avatar_hash.clone(),
                public_key: Some(legacy_identity.public_key.clone()),
                protocol_version: Some(1),
            }
            .to_json()
            .expect("serialize legacy peer info")
            .into(),
        ))
        .await
        .expect("send below-v2 peer info");

    match expect_protocol_message(&mut socket, "version incompatible message").await {
        ProtocolMessage::VersionIncompatible {
            local_version,
            remote_version,
            message,
        } => {
            assert_eq!(local_version, CURRENT_PROTOCOL_VERSION);
            assert_eq!(remote_version, 1);
            assert_eq!(
                message,
                "peer protocol version 1 is incompatible; minimum supported version is 2"
            );
        }
        other => panic!("expected version incompatible protocol message, got {other:?}"),
    }

    let (code, reason) = expect_handshake_close(&mut socket, "below-v2 incompatibility").await;
    assert_eq!(code, CloseCode::Policy);
    assert_eq!(reason, "protocol version incompatible");
    assert!(server.connected_peers().is_empty());
    server.shutdown().await.expect("shutdown ws server");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ws_key_exchange_incompatible_peer_missing_key_exchange_init_is_rejected() {
    let server = WsServer::bind(server_config(peer("server-021", "Server Node"), None))
        .await
        .expect("bind ws server");
    let mut server_events = server.subscribe();
    let (mut socket, _) = connect_async(ws_url(&server))
        .await
        .expect("connect raw websocket");
    let silent_peer = peer("silent-021", "Silent Peer");

    socket
        .send(Message::Text(
            peer_info_message(&silent_peer)
                .to_json()
                .expect("serialize peer info")
                .into(),
        ))
        .await
        .expect("send handshake peer info");

    let server_peer_info = expect_protocol_message(&mut socket, "server peer info").await;
    assert_eq!(
        parse_peer_info_message(server_peer_info),
        peer("server-021", "Server Node")
    );

    expect_handshake_rejection(&mut socket, "missing key exchange init rejection").await;

    assert!(server.connected_peers().is_empty());
    assert!(
        time::timeout(Duration::from_millis(200), server_events.recv())
            .await
            .is_err()
    );
    server.shutdown().await.expect("shutdown ws server");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ws_key_exchange_incompatible_peer_with_invalid_ephemeral_key_is_rejected() {
    let server = WsServer::bind(server_config(peer("server-022", "Server Node"), None))
        .await
        .expect("bind ws server");
    let (mut socket, _) = connect_async(ws_url(&server))
        .await
        .expect("connect raw websocket");
    let invalid_peer = peer("invalid-022", "Invalid Peer");

    socket
        .send(Message::Text(
            peer_info_message(&invalid_peer)
                .to_json()
                .expect("serialize peer info")
                .into(),
        ))
        .await
        .expect("send handshake peer info");

    let server_peer_info = expect_protocol_message(&mut socket, "server peer info").await;
    assert_eq!(
        parse_peer_info_message(server_peer_info),
        peer("server-022", "Server Node")
    );

    socket
        .send(Message::Text(
            ProtocolMessage::KeyExchangeInit {
                ephemeral_public_key: "not-base64".to_string(),
                protocol_version: CURRENT_PROTOCOL_VERSION,
            }
            .to_json()
            .expect("serialize invalid key exchange init")
            .into(),
        ))
        .await
        .expect("send invalid key exchange init");

    expect_handshake_rejection(&mut socket, "invalid ephemeral rejection").await;

    assert!(server.connected_peers().is_empty());
    server.shutdown().await.expect("shutdown ws server");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ws_disconnect_detects_silent_peer_via_heartbeat_timeout() {
    let server = WsServer::bind(server_config(peer("server-030", "Server Node"), None))
        .await
        .expect("bind ws server");
    let mut server_events = server.subscribe();
    let (mut socket, _) = connect_async(ws_url(&server))
        .await
        .expect("connect raw websocket");
    let silent_peer = peer("silent-030", "Silent Peer");

    let remote_identity = complete_raw_client_key_exchange(&mut socket, &silent_peer).await;
    assert_eq!(remote_identity, peer("server-030", "Server Node"));

    match expect_server_event(&mut server_events).await {
        WsServerEvent::PeerConnected { peer: connection } => {
            assert_eq!(connection.identity, silent_peer);
        }
        other => panic!("expected peer connection event, got {other:?}"),
    }

    match expect_server_event(&mut server_events).await {
        WsServerEvent::PeerDisconnected {
            peer: connection,
            reason,
        } => {
            assert_eq!(connection.identity, peer("silent-030", "Silent Peer"));
            assert_eq!(reason, WsDisconnectReason::HeartbeatTimedOut);
        }
        other => panic!("expected peer disconnected event, got {other:?}"),
    }

    server.shutdown().await.expect("shutdown ws server");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn ws_multi_client_supports_message_isolation_and_reconnect() {
    let server = WsServer::bind(server_config(peer("server-040", "Server Node"), None))
        .await
        .expect("bind ws server");
    let mut server_events = server.subscribe();
    let mut clients = Vec::new();

    for index in 0..10 {
        let identity = peer(&format!("client-{index:03}"), &format!("Client {index}"));
        let client = WsClient::connect(ws_url(&server), client_config(identity, None))
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

    let reconnected_identity = peer("client-000", "Client 0");
    let reconnected = WsClient::connect(
        ws_url(&server),
        client_config(reconnected_identity.clone(), None),
    )
    .await
    .expect("reconnect first client");

    match expect_server_event(&mut server_events).await {
        WsServerEvent::PeerConnected { peer } => {
            assert_eq!(peer.identity, reconnected_identity);
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
