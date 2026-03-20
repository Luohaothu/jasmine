use std::time::Duration;

use futures_util::stream::SplitSink;
use futures_util::{Sink, SinkExt, StreamExt};
use jasmine_core::ProtocolMessage;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, watch};
use tokio::time::{self, Instant};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::{Error as TungsteniteError, Message};
use tokio_tungstenite::WebSocketStream;

use crate::{MessagingError, Result, WsDisconnectReason, WsPeerConnection, WsPeerIdentity};

pub(crate) enum ConnectionCommand {
    Send(ProtocolMessage),
    Close,
}

pub(crate) async fn perform_client_handshake<S>(
    socket: &mut WebSocketStream<S>,
    local_peer: &WsPeerIdentity,
    remote_address: String,
    handshake_timeout: Duration,
) -> Result<WsPeerConnection>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    send_protocol_message(socket, &local_peer.to_protocol_message()).await?;
    let message = read_handshake_message(socket, handshake_timeout).await?;
    let identity = WsPeerIdentity::from_protocol_message(message)?;

    Ok(WsPeerConnection {
        identity,
        address: remote_address,
    })
}

pub(crate) async fn perform_server_handshake<S>(
    socket: &mut WebSocketStream<S>,
    local_peer: &WsPeerIdentity,
    remote_address: String,
    handshake_timeout: Duration,
) -> Result<WsPeerConnection>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let message = read_handshake_message(socket, handshake_timeout).await?;
    let identity = WsPeerIdentity::from_protocol_message(message)?;
    send_protocol_message(socket, &local_peer.to_protocol_message()).await?;

    Ok(WsPeerConnection {
        identity,
        address: remote_address,
    })
}

pub(crate) async fn run_connection_loop<S, F>(
    socket: WebSocketStream<S>,
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
                        if let Err(error) = send_protocol_message_sink(&mut writer, &message).await {
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

                if let Err(error) = send_protocol_message_sink(&mut writer, &ProtocolMessage::Ping).await {
                    return error_to_disconnect_reason(error);
                }
            }
            incoming = reader.next() => {
                match handle_incoming_message(&mut writer, &mut last_seen, incoming, &mut on_message).await {
                    Some(reason) => return reason,
                    None => continue,
                }
            }
        }
    }
}

async fn handle_incoming_message<W, F>(
    writer: &mut SplitSink<WebSocketStream<W>, Message>,
    last_seen: &mut Instant,
    incoming: Option<std::result::Result<Message, TungsteniteError>>,
    on_message: &mut F,
) -> Option<WsDisconnectReason>
where
    W: AsyncRead + AsyncWrite + Unpin,
    F: FnMut(ProtocolMessage) + Send,
{
    match incoming {
        Some(Ok(Message::Text(payload))) => {
            *last_seen = Instant::now();
            match ProtocolMessage::from_json(payload.as_str()) {
                Ok(ProtocolMessage::Ping) => {
                    if let Err(error) =
                        send_protocol_message_sink(writer, &ProtocolMessage::Pong).await
                    {
                        return Some(error_to_disconnect_reason(error));
                    }
                }
                Ok(ProtocolMessage::Pong) => {}
                Ok(message) => on_message(message),
                Err(_) => {
                    let _ = close_socket(writer, CloseCode::Policy, "invalid protocol json").await;
                    return Some(WsDisconnectReason::ProtocolViolation);
                }
            }
        }
        Some(Ok(Message::Binary(payload))) => {
            *last_seen = Instant::now();
            let text = match std::str::from_utf8(payload.as_ref()) {
                Ok(text) => text,
                Err(_) => {
                    let _ = close_socket(writer, CloseCode::Policy, "invalid utf8 payload").await;
                    return Some(WsDisconnectReason::ProtocolViolation);
                }
            };

            match ProtocolMessage::from_json(text) {
                Ok(ProtocolMessage::Ping) => {
                    if let Err(error) =
                        send_protocol_message_sink(writer, &ProtocolMessage::Pong).await
                    {
                        return Some(error_to_disconnect_reason(error));
                    }
                }
                Ok(ProtocolMessage::Pong) => {}
                Ok(message) => on_message(message),
                Err(_) => {
                    let _ = close_socket(writer, CloseCode::Policy, "invalid protocol json").await;
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
        None => {
            return Some(WsDisconnectReason::RemoteClosed);
        }
        Some(Ok(Message::Frame(_))) => {}
        Some(Err(error)) => {
            return Some(transport_error_to_disconnect_reason(error));
        }
    }

    None
}

async fn read_handshake_message<S>(
    socket: &mut WebSocketStream<S>,
    handshake_timeout: Duration,
) -> Result<ProtocolMessage>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let incoming = time::timeout(handshake_timeout, socket.next())
        .await
        .map_err(|_| MessagingError::Protocol("peer handshake timed out".to_string()))?;

    match incoming {
        Some(Ok(Message::Text(payload))) => ProtocolMessage::from_json(payload.as_str())
            .map_err(|error| MessagingError::Protocol(error.to_string())),
        Some(Ok(Message::Binary(payload))) => {
            let text = std::str::from_utf8(payload.as_ref()).map_err(|_| {
                MessagingError::Protocol("peer handshake payload must be utf-8 json".to_string())
            })?;
            ProtocolMessage::from_json(text)
                .map_err(|error| MessagingError::Protocol(error.to_string()))
        }
        Some(Ok(_)) => {
            let _ = close_socket(socket, CloseCode::Policy, "first message must be PeerInfo").await;
            Err(MessagingError::Protocol(
                "first message must be PeerInfo".to_string(),
            ))
        }
        Some(Err(error)) => Err(MessagingError::Transport(error.to_string())),
        None => Err(MessagingError::Transport(
            "connection closed before peer authentication".to_string(),
        )),
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

async fn send_protocol_message_sink<W>(
    writer: &mut SplitSink<WebSocketStream<W>, Message>,
    message: &ProtocolMessage,
) -> Result<()>
where
    W: AsyncRead + AsyncWrite + Unpin,
{
    let payload = message
        .to_json()
        .map_err(|error| MessagingError::Protocol(error.to_string()))?;
    writer
        .send(Message::Text(payload.into()))
        .await
        .map_err(|error| MessagingError::Transport(error.to_string()))
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
