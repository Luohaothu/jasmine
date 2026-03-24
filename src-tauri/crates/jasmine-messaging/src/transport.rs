use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::stream::SplitSink;
use futures_util::{Sink, SinkExt, StreamExt};
use jasmine_core::{ProtocolMessage, StorageEngine, CURRENT_PROTOCOL_VERSION};
use jasmine_crypto::{
    fingerprint, public_key_from_base64, public_key_to_base64, EncryptedFrame, PublicKey, Session,
    SessionKeyManager, StaticSecret,
};
use jasmine_storage::SqliteStorage;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, watch};
use tokio::time::{self, Instant};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::{Error as TungsteniteError, Message};
use tokio_tungstenite::WebSocketStream;

use crate::{MessagingError, Result, WsDisconnectReason, WsPeerConnection, WsPeerIdentity};

const ENCRYPTED_PROTOCOL_FRAME_TYPE: u32 = 0x01;
const TRANSPORT_FRAME_TYPE_LEN: usize = 4;
const MIN_SUPPORTED_PROTOCOL_VERSION: u32 = 2;
const PROTOCOL_VERSION_INCOMPATIBLE_CLOSE_REASON: &str = "protocol version incompatible";

pub(crate) enum ConnectionCommand {
    Send(ProtocolMessage),
    Close,
}

pub(crate) struct HandshakeOutcome {
    pub peer: WsPeerConnection,
    pub session: Session,
}

pub(crate) async fn perform_client_handshake<S>(
    socket: &mut WebSocketStream<S>,
    local_peer: &WsPeerIdentity,
    local_private_key: Option<&[u8]>,
    peer_key_store: Option<&SqliteStorage>,
    remote_address: String,
    handshake_timeout: Duration,
) -> Result<HandshakeOutcome>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let local_secret = validate_local_transport_identity(local_peer, local_private_key)?;

    send_protocol_message(socket, &local_peer.to_protocol_message()).await?;
    let message = read_handshake_message(socket, handshake_timeout, "PeerInfo").await?;
    let (identity, peer_public_key) = match parse_handshake_peer_info(message) {
        Ok(value) => value,
        Err(error) => return fail_handshake(socket, &error.to_string(), error).await,
    };

    let negotiated_protocol_version = ensure_remote_protocol_version_compatible(
        socket,
        local_peer.protocol_version,
        identity.protocol_version,
    )
    .await?;

    if let Err(error) = persist_peer_key(peer_key_store, &identity, &peer_public_key).await {
        return fail_handshake(socket, &error.to_string(), error).await;
    }

    let (our_ephemeral_public, pending) =
        SessionKeyManager::initiate_key_exchange(&local_secret, &peer_public_key);
    send_protocol_message(
        socket,
        &ProtocolMessage::KeyExchangeInit {
            ephemeral_public_key: public_key_to_base64(&our_ephemeral_public),
            protocol_version: negotiated_protocol_version,
        },
    )
    .await?;

    let message = read_handshake_message(socket, handshake_timeout, "KeyExchangeResponse").await?;
    let peer_ephemeral_key = match parse_key_exchange_response(message) {
        Ok(value) => value,
        Err(error) => return fail_handshake(socket, &error.to_string(), error).await,
    };

    let session =
        match SessionKeyManager::complete_key_exchange_initiator(pending, &peer_ephemeral_key) {
            Ok(value) => value,
            Err(error) => {
                let error = MessagingError::Validation(error.to_string());
                return fail_handshake(socket, &error.to_string(), error).await;
            }
        };

    Ok(HandshakeOutcome {
        peer: WsPeerConnection {
            identity,
            address: remote_address,
        },
        session,
    })
}

pub(crate) async fn perform_server_handshake<S>(
    socket: &mut WebSocketStream<S>,
    local_peer: &WsPeerIdentity,
    local_private_key: Option<&[u8]>,
    peer_key_store: Option<&SqliteStorage>,
    remote_address: String,
    handshake_timeout: Duration,
) -> Result<HandshakeOutcome>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let local_secret = validate_local_transport_identity(local_peer, local_private_key)?;

    let message = read_handshake_message(socket, handshake_timeout, "PeerInfo").await?;
    let (identity, peer_public_key) = match parse_handshake_peer_info(message) {
        Ok(value) => value,
        Err(error) => return fail_handshake(socket, &error.to_string(), error).await,
    };

    let negotiated_protocol_version = ensure_remote_protocol_version_compatible(
        socket,
        local_peer.protocol_version,
        identity.protocol_version,
    )
    .await?;

    if let Err(error) = persist_peer_key(peer_key_store, &identity, &peer_public_key).await {
        return fail_handshake(socket, &error.to_string(), error).await;
    }

    send_protocol_message(
        socket,
        &peer_info_with_protocol_version(local_peer, negotiated_protocol_version),
    )
    .await?;

    let message = read_handshake_message(socket, handshake_timeout, "KeyExchangeInit").await?;
    let peer_ephemeral_key = match parse_key_exchange_init(message, negotiated_protocol_version) {
        Ok(value) => value,
        Err(error) => return fail_handshake(socket, &error.to_string(), error).await,
    };

    let (our_ephemeral_public, session) = match SessionKeyManager::accept_key_exchange(
        &local_secret,
        &peer_public_key,
        &peer_ephemeral_key,
    ) {
        Ok(value) => value,
        Err(error) => {
            let error = MessagingError::Validation(error.to_string());
            return fail_handshake(socket, &error.to_string(), error).await;
        }
    };

    send_protocol_message(
        socket,
        &ProtocolMessage::KeyExchangeResponse {
            ephemeral_public_key: public_key_to_base64(&our_ephemeral_public),
        },
    )
    .await?;

    Ok(HandshakeOutcome {
        peer: WsPeerConnection {
            identity,
            address: remote_address,
        },
        session,
    })
}

pub(crate) async fn run_connection_loop<S, F>(
    socket: WebSocketStream<S>,
    session: Arc<Mutex<Session>>,
    heartbeat_interval: Duration,
    max_missed_heartbeats: u32,
    mut command_rx: mpsc::Receiver<ConnectionCommand>,
    mut shutdown_rx: watch::Receiver<bool>,
    mut on_message: F,
) -> WsDisconnectReason
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnMut(ProtocolMessage) + Send,
{
    let (mut writer, mut reader) = socket.split();
    let mut heartbeat = time::interval(heartbeat_interval);
    let timeout = heartbeat_timeout(heartbeat_interval, max_missed_heartbeats);
    let mut last_seen = Instant::now();

    loop {
        tokio::select! {
            changed = shutdown_rx.changed() => {
                if changed.is_ok() && *shutdown_rx.borrow() {
                    let _ = close_socket(&mut writer, CloseCode::Normal, "shutdown").await;
                    return WsDisconnectReason::LocalClosed;
                }
            }
            command = command_rx.recv() => {
                match command {
                    Some(ConnectionCommand::Send(message)) => {
                        if let Err(error) = send_encrypted_protocol_message_sink(&mut writer, &session, &message).await {
                            return error_to_disconnect_reason(error);
                        }
                    }
                    Some(ConnectionCommand::Close) | None => {
                        let _ = close_socket(&mut writer, CloseCode::Normal, "close").await;
                        return WsDisconnectReason::LocalClosed;
                    }
                }
            }
            _ = heartbeat.tick() => {
                if Instant::now().duration_since(last_seen) >= timeout {
                    let _ = close_socket(&mut writer, CloseCode::Policy, "heartbeat timeout").await;
                    return WsDisconnectReason::HeartbeatTimedOut;
                }

                if let Err(error) = send_encrypted_protocol_message_sink(&mut writer, &session, &ProtocolMessage::Ping).await {
                    return error_to_disconnect_reason(error);
                }
            }
            incoming = reader.next() => {
                match handle_incoming_message(&mut writer, &session, &mut last_seen, incoming, &mut on_message).await {
                    Some(reason) => return reason,
                    None => continue,
                }
            }
        }
    }
}

async fn handle_incoming_message<W, F>(
    writer: &mut SplitSink<WebSocketStream<W>, Message>,
    session: &Arc<Mutex<Session>>,
    last_seen: &mut Instant,
    incoming: Option<std::result::Result<Message, TungsteniteError>>,
    on_message: &mut F,
) -> Option<WsDisconnectReason>
where
    W: AsyncRead + AsyncWrite + Unpin,
    F: FnMut(ProtocolMessage) + Send,
{
    match incoming {
        Some(Ok(Message::Text(_payload))) => {
            *last_seen = Instant::now();
            let _ = close_socket(
                writer,
                CloseCode::Policy,
                "unexpected plaintext message after handshake",
            )
            .await;
            return Some(WsDisconnectReason::ProtocolViolation);
        }
        Some(Ok(Message::Binary(payload))) => {
            *last_seen = Instant::now();
            match decrypt_protocol_message(session, payload.as_ref()) {
                Ok(ProtocolMessage::Ping) => {
                    if let Err(error) = send_encrypted_protocol_message_sink(
                        writer,
                        session,
                        &ProtocolMessage::Pong,
                    )
                    .await
                    {
                        return Some(error_to_disconnect_reason(error));
                    }
                }
                Ok(ProtocolMessage::Pong) => {}
                Ok(ProtocolMessage::VersionIncompatible { .. }) => {
                    let _ = close_socket(
                        writer,
                        CloseCode::Policy,
                        PROTOCOL_VERSION_INCOMPATIBLE_CLOSE_REASON,
                    )
                    .await;
                    return Some(WsDisconnectReason::ProtocolVersionIncompatible);
                }
                Ok(message) if is_handshake_message(&message) => {
                    let _ = close_socket(writer, CloseCode::Policy, "unexpected handshake message")
                        .await;
                    return Some(WsDisconnectReason::ProtocolViolation);
                }
                Ok(message) => on_message(message),
                Err(error) => {
                    let _ = close_socket(writer, CloseCode::Policy, &error.to_string()).await;
                    return Some(WsDisconnectReason::ProtocolViolation);
                }
            }
        }
        Some(Ok(Message::Ping(payload))) => {
            *last_seen = Instant::now();
            if let Err(error) = writer.send(Message::Pong(payload)).await {
                return Some(transport_error_to_disconnect_reason(error));
            }
        }
        Some(Ok(Message::Pong(_))) => {
            *last_seen = Instant::now();
        }
        Some(Ok(Message::Close(_))) => {
            return Some(WsDisconnectReason::LocalClosed);
        }
        std::option::Option::None => {
            return Some(WsDisconnectReason::RemoteClosed);
        }
        Some(Ok(Message::Frame(_))) => {}
        Some(Err(error)) => {
            return Some(transport_error_to_disconnect_reason(error));
        }
    }

    None
}

fn is_handshake_message(message: &ProtocolMessage) -> bool {
    matches!(
        message,
        ProtocolMessage::PeerInfo { .. }
            | ProtocolMessage::KeyExchangeInit { .. }
            | ProtocolMessage::KeyExchangeResponse { .. }
            | ProtocolMessage::VersionIncompatible { .. }
    )
}

fn validate_local_transport_identity(
    local_peer: &WsPeerIdentity,
    local_private_key: Option<&[u8]>,
) -> Result<StaticSecret> {
    if local_peer.public_key.trim().is_empty() {
        return Err(MessagingError::Validation(
            "local websocket identity must advertise a public key".to_string(),
        ));
    }

    if local_peer.protocol_version != CURRENT_PROTOCOL_VERSION {
        return Err(MessagingError::Validation(format!(
            "local websocket protocol version must be {}, got {}",
            CURRENT_PROTOCOL_VERSION, local_peer.protocol_version
        )));
    }

    let Some(local_private_key) = local_private_key else {
        return Err(MessagingError::Validation(
            "local websocket private key is required for key exchange".to_string(),
        ));
    };

    let actual = local_private_key.len();
    let local_private_key: [u8; 32] = local_private_key.try_into().map_err(|_| {
        MessagingError::Validation(format!(
            "local websocket private key must be 32 bytes, got {actual}"
        ))
    })?;

    let local_secret = StaticSecret::from(local_private_key);
    let expected_public_key = public_key_to_base64(&PublicKey::from(&local_secret));
    if expected_public_key != local_peer.public_key {
        return Err(MessagingError::Validation(
            "local websocket private key does not match advertised public key".to_string(),
        ));
    }

    Ok(local_secret)
}

fn parse_handshake_peer_info(message: ProtocolMessage) -> Result<(WsPeerIdentity, PublicKey)> {
    match message {
        ProtocolMessage::PeerInfo {
            device_id,
            display_name,
            avatar_hash,
            public_key,
            protocol_version,
        } => {
            if device_id.trim().is_empty() {
                return Err(MessagingError::Protocol(
                    "peer info device_id must not be empty".to_string(),
                ));
            }

            if display_name.trim().is_empty() {
                return Err(MessagingError::Protocol(
                    "peer info display_name must not be empty".to_string(),
                ));
            }

            let public_key = public_key.ok_or_else(|| {
                MessagingError::Protocol(
                    "peer info missing public_key; peer does not support key exchange".to_string(),
                )
            })?;
            if public_key.trim().is_empty() {
                return Err(MessagingError::Protocol(
                    "peer info public_key must not be empty".to_string(),
                ));
            }

            let protocol_version = protocol_version.ok_or_else(|| {
                MessagingError::Protocol(
                    "peer info missing protocol_version; peer does not support key exchange"
                        .to_string(),
                )
            })?;

            let public_key_value = decode_public_key(&public_key, "peer info public_key")?;
            let identity = WsPeerIdentity::from_protocol_message(ProtocolMessage::PeerInfo {
                device_id,
                display_name,
                avatar_hash,
                public_key: Some(public_key),
                protocol_version: Some(protocol_version),
            })?;

            Ok((identity, public_key_value))
        }
        other => Err(MessagingError::Protocol(format!(
            "expected PeerInfo, got {other:?}"
        ))),
    }
}

async fn ensure_remote_protocol_version_compatible<S>(
    socket: &mut WebSocketStream<S>,
    local_protocol_version: u32,
    remote_protocol_version: u32,
) -> Result<u32>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match negotiated_protocol_version(local_protocol_version, remote_protocol_version) {
        Ok(version) => Ok(version),
        Err(MessagingError::ProtocolVersionIncompatible {
            local_version,
            remote_version,
            message,
        }) => {
            reject_protocol_version_incompatible(socket, local_version, remote_version, message)
                .await
        }
        Err(error) => fail_handshake(socket, &error.to_string(), error).await,
    }
}

fn negotiated_protocol_version(
    local_protocol_version: u32,
    remote_protocol_version: u32,
) -> Result<u32> {
    if remote_protocol_version < MIN_SUPPORTED_PROTOCOL_VERSION {
        return Err(MessagingError::ProtocolVersionIncompatible {
            local_version: local_protocol_version,
            remote_version: remote_protocol_version,
            message: format!(
                "peer protocol version {remote_protocol_version} is incompatible; minimum supported version is {MIN_SUPPORTED_PROTOCOL_VERSION}"
            ),
        });
    }

    if remote_protocol_version > local_protocol_version {
        return Err(MessagingError::ProtocolVersionIncompatible {
            local_version: local_protocol_version,
            remote_version: remote_protocol_version,
            message: format!(
                "peer protocol version {remote_protocol_version} is newer than local supported version {local_protocol_version}"
            ),
        });
    }

    Ok(local_protocol_version.min(remote_protocol_version))
}

fn peer_info_with_protocol_version(
    local_peer: &WsPeerIdentity,
    protocol_version: u32,
) -> ProtocolMessage {
    ProtocolMessage::PeerInfo {
        device_id: local_peer.device_id.clone(),
        display_name: local_peer.display_name.clone(),
        avatar_hash: local_peer.avatar_hash.clone(),
        public_key: (!local_peer.public_key.trim().is_empty())
            .then_some(local_peer.public_key.clone()),
        protocol_version: Some(protocol_version),
    }
}

fn parse_key_exchange_init(
    message: ProtocolMessage,
    expected_protocol_version: u32,
) -> Result<PublicKey> {
    match message {
        ProtocolMessage::KeyExchangeInit {
            ephemeral_public_key,
            protocol_version,
        } => {
            if protocol_version != expected_protocol_version {
                return Err(MessagingError::Protocol(format!(
                    "unexpected negotiated protocol version, expected {expected_protocol_version}, got {protocol_version}"
                )));
            }

            decode_public_key(
                &ephemeral_public_key,
                "key exchange init ephemeral_public_key",
            )
        }
        other => Err(MessagingError::Protocol(format!(
            "expected KeyExchangeInit, got {other:?}"
        ))),
    }
}

async fn reject_protocol_version_incompatible<S, T>(
    socket: &mut WebSocketStream<S>,
    local_version: u32,
    remote_version: u32,
    message: String,
) -> Result<T>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let _ = send_protocol_message(
        socket,
        &ProtocolMessage::VersionIncompatible {
            local_version,
            remote_version,
            message: message.clone(),
        },
    )
    .await;
    let _ = close_socket(
        socket,
        CloseCode::Policy,
        PROTOCOL_VERSION_INCOMPATIBLE_CLOSE_REASON,
    )
    .await;
    Err(MessagingError::ProtocolVersionIncompatible {
        local_version,
        remote_version,
        message,
    })
}

fn parse_key_exchange_response(message: ProtocolMessage) -> Result<PublicKey> {
    match message {
        ProtocolMessage::KeyExchangeResponse {
            ephemeral_public_key,
        } => decode_public_key(
            &ephemeral_public_key,
            "key exchange response ephemeral_public_key",
        ),
        other => Err(MessagingError::Protocol(format!(
            "expected KeyExchangeResponse, got {other:?}"
        ))),
    }
}

fn decode_public_key(encoded: &str, label: &str) -> Result<PublicKey> {
    public_key_from_base64(encoded)
        .map_err(|error| MessagingError::Validation(format!("invalid {label}: {error}")))
}

async fn persist_peer_key(
    peer_key_store: Option<&SqliteStorage>,
    peer: &WsPeerIdentity,
    public_key: &PublicKey,
) -> Result<()> {
    let Some(peer_key_store) = peer_key_store else {
        return Ok(());
    };

    let public_key_bytes = public_key.to_bytes();
    let stored = peer_key_store
        .get_peer_key(&peer.device_id)
        .await
        .map_err(|error| MessagingError::Storage(error.to_string()))?;

    if let Some(stored) = stored {
        if stored.public_key.as_slice() != public_key_bytes.as_slice() {
            return Err(MessagingError::Validation(format!(
                "peer public key mismatch for {}",
                peer.device_id
            )));
        }

        peer_key_store
            .update_peer_key_last_seen(&peer.device_id)
            .await
            .map_err(|error| MessagingError::Storage(error.to_string()))?;
        return Ok(());
    }

    peer_key_store
        .save_peer_key(&peer.device_id, &public_key_bytes, &fingerprint(public_key))
        .await
        .map_err(|error| MessagingError::Storage(error.to_string()))
}

async fn fail_handshake<S, T>(
    socket: &mut WebSocketStream<S>,
    reason: &str,
    error: MessagingError,
) -> Result<T>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let _ = close_socket(socket, CloseCode::Policy, reason).await;
    Err(error)
}

async fn read_handshake_message<S>(
    socket: &mut WebSocketStream<S>,
    handshake_timeout: Duration,
    expected_message: &str,
) -> Result<ProtocolMessage>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let incoming = match time::timeout(handshake_timeout, socket.next()).await {
        Ok(value) => value,
        Err(_) => {
            let reason = format!("{expected_message} timed out");
            let _ = close_socket(socket, CloseCode::Policy, &reason).await;
            return Err(MessagingError::Protocol(reason));
        }
    };

    match incoming {
        Some(Ok(Message::Text(payload))) => {
            parse_handshake_protocol_message(payload.as_str(), expected_message)
        }
        Some(Ok(Message::Binary(payload))) => {
            let text = std::str::from_utf8(payload.as_ref()).map_err(|_| {
                MessagingError::Protocol(format!("{expected_message} payload must be utf-8 json"))
            })?;
            parse_handshake_protocol_message(text, expected_message)
        }
        Some(Ok(Message::Close(frame))) => Err(MessagingError::Protocol(
            frame
                .as_ref()
                .map(|frame| frame.reason.to_string())
                .filter(|reason| !reason.is_empty())
                .unwrap_or_else(|| format!("connection closed during {expected_message}")),
        )),
        Some(Ok(_)) => {
            let reason = format!("expected {expected_message} during handshake");
            let _ = close_socket(socket, CloseCode::Policy, &reason).await;
            Err(MessagingError::Protocol(reason))
        }
        Some(Err(error)) => Err(MessagingError::Transport(error.to_string())),
        std::option::Option::None => Err(MessagingError::Transport(format!(
            "connection closed before {expected_message}"
        ))),
    }
}

fn parse_handshake_protocol_message(
    payload: &str,
    expected_message: &str,
) -> Result<ProtocolMessage> {
    match ProtocolMessage::from_json(payload).map_err(|error| {
        MessagingError::Protocol(format!("invalid {expected_message} json: {error}"))
    })? {
        ProtocolMessage::VersionIncompatible {
            local_version,
            remote_version,
            message,
        } => Err(MessagingError::ProtocolVersionIncompatible {
            local_version,
            remote_version,
            message,
        }),
        message => Ok(message),
    }
}

fn heartbeat_timeout(interval: Duration, max_missed_heartbeats: u32) -> Duration {
    interval
        .checked_mul(max_missed_heartbeats.max(1))
        .unwrap_or(Duration::from_secs(u64::MAX))
}

pub(crate) async fn send_protocol_message<S>(
    socket: &mut WebSocketStream<S>,
    message: &ProtocolMessage,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let payload = message
        .to_json()
        .map_err(|error| MessagingError::Protocol(error.to_string()))?;
    socket
        .send(Message::Text(payload.into()))
        .await
        .map_err(|error| MessagingError::Transport(error.to_string()))
}

async fn send_encrypted_protocol_message_sink<W>(
    writer: &mut SplitSink<WebSocketStream<W>, Message>,
    session: &Arc<Mutex<Session>>,
    message: &ProtocolMessage,
) -> Result<()>
where
    W: AsyncRead + AsyncWrite + Unpin,
{
    let payload = message
        .to_json()
        .map_err(|error| MessagingError::Protocol(error.to_string()))?;
    let frame_bytes = encrypt_protocol_message(session, payload.as_bytes())?;
    writer
        .send(Message::Binary(frame_bytes.into()))
        .await
        .map_err(|error| MessagingError::Transport(error.to_string()))
}

fn encrypt_protocol_message(session: &Arc<Mutex<Session>>, plaintext: &[u8]) -> Result<Vec<u8>> {
    let frame = session
        .lock()
        .expect("lock websocket session for encrypt")
        .encrypt_message(plaintext)
        .map_err(|error| MessagingError::Protocol(error.to_string()))?;
    let frame_bytes = frame.to_bytes();
    let mut output = Vec::with_capacity(TRANSPORT_FRAME_TYPE_LEN + frame_bytes.len());
    output.extend_from_slice(&ENCRYPTED_PROTOCOL_FRAME_TYPE.to_be_bytes());
    output.extend_from_slice(&frame_bytes);
    Ok(output)
}

fn decrypt_protocol_message(
    session: &Arc<Mutex<Session>>,
    payload: &[u8],
) -> Result<ProtocolMessage> {
    let frame_payload = parse_encrypted_protocol_frame(payload)?;
    let frame = EncryptedFrame::from_bytes(frame_payload)
        .map_err(|error| MessagingError::Protocol(error.to_string()))?;
    let plaintext = session
        .lock()
        .expect("lock websocket session for decrypt")
        .decrypt_message(&frame)
        .map_err(|error| MessagingError::Protocol(error.to_string()))?;
    let text = std::str::from_utf8(&plaintext).map_err(|_| {
        MessagingError::Protocol("decrypted payload must be utf-8 json".to_string())
    })?;
    ProtocolMessage::from_json(text).map_err(|error| {
        MessagingError::Protocol(format!("invalid decrypted protocol json: {error}"))
    })
}

fn parse_encrypted_protocol_frame(payload: &[u8]) -> Result<&[u8]> {
    if payload.len() < TRANSPORT_FRAME_TYPE_LEN {
        return Err(MessagingError::Protocol(format!(
            "encrypted transport frame missing type prefix: {} bytes",
            payload.len()
        )));
    }

    let frame_type = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
    if frame_type != ENCRYPTED_PROTOCOL_FRAME_TYPE {
        return Err(MessagingError::Protocol(format!(
            "unexpected encrypted transport frame type: {frame_type:#010x}"
        )));
    }

    Ok(&payload[TRANSPORT_FRAME_TYPE_LEN..])
}

async fn close_socket<W>(
    writer: &mut W,
    code: CloseCode,
    reason: &str,
) -> std::result::Result<(), TungsteniteError>
where
    W: Sink<Message, Error = TungsteniteError> + Unpin,
{
    writer
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.to_string().into(),
        })))
        .await
}

fn error_to_disconnect_reason(error: MessagingError) -> WsDisconnectReason {
    match error {
        MessagingError::ProtocolVersionIncompatible { .. } => {
            WsDisconnectReason::ProtocolVersionIncompatible
        }
        MessagingError::Protocol(_) => WsDisconnectReason::ProtocolViolation,
        MessagingError::Validation(_) => WsDisconnectReason::ProtocolViolation,
        MessagingError::Transport(message) => {
            if message.contains("Connection closed") || message.contains("Already closed") {
                WsDisconnectReason::RemoteClosed
            } else {
                WsDisconnectReason::TransportError(message)
            }
        }
        MessagingError::Storage(message) => WsDisconnectReason::TransportError(message),
        MessagingError::PeerNotConnected(message) => WsDisconnectReason::TransportError(message),
    }
}

fn transport_error_to_disconnect_reason(error: TungsteniteError) -> WsDisconnectReason {
    error_to_disconnect_reason(MessagingError::Transport(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{error_to_disconnect_reason, negotiated_protocol_version};
    use crate::{MessagingError, WsDisconnectReason};

    #[test]
    fn protocol_version_incompatible_maps_to_dedicated_disconnect_reason() {
        let reason = error_to_disconnect_reason(MessagingError::ProtocolVersionIncompatible {
            local_version: 2,
            remote_version: 1,
            message: "peer protocol version 1 is incompatible; minimum supported version is 2"
                .to_string(),
        });

        assert_eq!(reason, WsDisconnectReason::ProtocolVersionIncompatible);
    }

    #[test]
    fn negotiated_protocol_version_accepts_v2_and_v3_peers() {
        assert_eq!(negotiated_protocol_version(3, 3).expect("negotiate v3"), 3);
        assert_eq!(negotiated_protocol_version(3, 2).expect("negotiate v2"), 2);
    }

    #[test]
    fn negotiated_protocol_version_rejects_newer_remote_peer() {
        let error = negotiated_protocol_version(3, 4).expect_err("reject newer remote version");

        assert!(matches!(
            error,
            MessagingError::ProtocolVersionIncompatible {
                local_version: 3,
                remote_version: 4,
                ..
            }
        ));
    }
}
