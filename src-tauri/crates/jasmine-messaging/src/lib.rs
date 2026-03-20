mod chat;
mod transport;
mod ws_client;
mod ws_server;

use std::time::Duration;

use jasmine_core::{ChatId, Message, MessageTransport, ProtocolMessage};
use thiserror::Error;

pub use chat::{
    ChatService, ChatServiceConfig, ChatServiceEvent, ChatStorage, DEFAULT_CHAT_ACK_TIMEOUT,
};
pub use ws_client::{WsClient, WsClientEvent};
pub use ws_server::{WsServer, WsServerEvent};

pub const DEFAULT_WS_PORT: u16 = 9735;
pub const DEFAULT_WS_BIND_ADDR: &str = "0.0.0.0:9735";
pub const DEFAULT_WS_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
pub const DEFAULT_WS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
pub const DEFAULT_MAX_MISSED_HEARTBEATS: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MessagingError {
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("transport error: {0}")]
    Transport(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("validation error: {0}")]
    Validation(String),
    #[error("peer not connected: {0}")]
    PeerNotConnected(String),
}

pub type Result<T> = std::result::Result<T, MessagingError>;

impl From<jasmine_core::CoreError> for MessagingError {
    fn from(value: jasmine_core::CoreError) -> Self {
        Self::Storage(value.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsDisconnectReason {
    LocalClosed,
    RemoteClosed,
    HeartbeatTimedOut,
    ProtocolViolation,
    TransportError(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsPeerIdentity {
    pub device_id: String,
    pub display_name: String,
    pub avatar_hash: Option<String>,
}

impl WsPeerIdentity {
    pub fn new(device_id: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            device_id: device_id.into(),
            display_name: display_name.into(),
            avatar_hash: None,
        }
    }

    pub fn with_avatar_hash(mut self, avatar_hash: impl Into<String>) -> Self {
        self.avatar_hash = Some(avatar_hash.into());
        self
    }

    pub(crate) fn to_protocol_message(&self) -> ProtocolMessage {
        ProtocolMessage::PeerInfo {
            device_id: self.device_id.clone(),
            display_name: self.display_name.clone(),
            avatar_hash: self.avatar_hash.clone(),
        }
    }

    pub(crate) fn from_protocol_message(message: ProtocolMessage) -> Result<Self> {
        match message {
            ProtocolMessage::PeerInfo {
                device_id,
                display_name,
                avatar_hash,
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

                Ok(Self {
                    device_id,
                    display_name,
                    avatar_hash,
                })
            }
            other => Err(MessagingError::Protocol(format!(
                "first protocol message must be PeerInfo, got {other:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsPeerConnection {
    pub identity: WsPeerIdentity,
    pub address: String,
}

#[derive(Debug, Clone)]
pub struct WsServerConfig {
    pub bind_addr: String,
    pub local_peer: WsPeerIdentity,
    pub heartbeat_interval: Duration,
    pub max_missed_heartbeats: u32,
    pub handshake_timeout: Duration,
}

impl WsServerConfig {
    pub fn new(local_peer: WsPeerIdentity) -> Self {
        Self {
            bind_addr: DEFAULT_WS_BIND_ADDR.to_string(),
            local_peer,
            heartbeat_interval: DEFAULT_WS_HEARTBEAT_INTERVAL,
            max_missed_heartbeats: DEFAULT_MAX_MISSED_HEARTBEATS,
            handshake_timeout: DEFAULT_WS_HANDSHAKE_TIMEOUT,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WsClientConfig {
    pub local_peer: WsPeerIdentity,
    pub heartbeat_interval: Duration,
    pub max_missed_heartbeats: u32,
    pub handshake_timeout: Duration,
}

impl WsClientConfig {
    pub fn new(local_peer: WsPeerIdentity) -> Self {
        Self {
            local_peer,
            heartbeat_interval: DEFAULT_WS_HEARTBEAT_INTERVAL,
            max_missed_heartbeats: DEFAULT_MAX_MISSED_HEARTBEATS,
            handshake_timeout: DEFAULT_WS_HANDSHAKE_TIMEOUT,
        }
    }
}

pub struct MessagingModule<T: MessageTransport> {
    transport: T,
    active_chat: Option<ChatId>,
}

impl<T: MessageTransport> MessagingModule<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            active_chat: None,
        }
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    pub fn active_chat(&self) -> Option<&ChatId> {
        self.active_chat.as_ref()
    }

    pub fn into_message(message: Message) -> Message {
        message
    }
}
