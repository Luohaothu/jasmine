#![allow(async_fn_in_trait)]

pub mod identity;
pub mod protocol;
pub mod settings;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;
use uuid::Uuid;

pub use identity::{DeviceIdentity, IdentityStore};
pub use protocol::{AckStatus, ProtocolMessage};
pub use settings::{AppSettings, SettingsService};

pub type Result<T> = std::result::Result<T, CoreError>;
pub type PeerDiscoveredCallback = Arc<dyn Fn(PeerInfo) + Send + Sync + 'static>;
pub type PeerLostCallback = Arc<dyn Fn(DeviceId) + Send + Sync + 'static>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CoreError {
    #[error("operation not implemented: {0}")]
    NotImplemented(&'static str),
    #[error("persistence error: {0}")]
    Persistence(String),
    #[error("validation error: {0}")]
    Validation(String),
}

impl From<std::io::Error> for CoreError {
    fn from(value: std::io::Error) -> Self {
        Self::Persistence(value.to_string())
    }
}

impl From<serde_json::Error> for CoreError {
    fn from(value: serde_json::Error) -> Self {
        Self::Persistence(value.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UserId(pub Uuid);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeviceId(pub Uuid);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChatId(pub Uuid);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageStatus {
    Sent,
    Delivered,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferStatus {
    Pending,
    Active,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub id: Uuid,
    pub chat_id: ChatId,
    pub sender_id: UserId,
    pub content: String,
    pub timestamp_ms: i64,
    pub status: MessageStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    pub device_id: DeviceId,
    pub user_id: Option<UserId>,
    pub display_name: String,
    pub ws_port: Option<u16>,
    pub addresses: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupInfo {
    pub id: ChatId,
    pub name: String,
    pub member_ids: Vec<UserId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferOffer {
    pub id: Uuid,
    pub sender_device_id: DeviceId,
    pub file_name: String,
    pub file_size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferRecord {
    pub id: Uuid,
    pub peer_id: DeviceId,
    pub chat_id: Option<ChatId>,
    pub file_name: String,
    pub local_path: PathBuf,
    pub status: TransferStatus,
}

pub trait DiscoveryService: Send + Sync {
    async fn start(&self) -> Result<()>;
    async fn stop(&self) -> Result<()>;
    fn on_peer_discovered(&mut self, callback: PeerDiscoveredCallback);
    fn on_peer_lost(&mut self, callback: PeerLostCallback);
}

pub trait MessageTransport: Send + Sync {
    async fn send(&self, peer_id: &DeviceId, message: &Message) -> Result<()>;
    async fn broadcast(&self, group_id: &ChatId, message: &Message) -> Result<()>;
}

pub trait FileTransfer: Send + Sync {
    async fn send_file(&self, peer_id: &DeviceId, file_path: &Path) -> Result<TransferRecord>;
    async fn receive_file(&self, offer: &TransferOffer) -> Result<TransferRecord>;
}

pub trait StorageEngine: Send + Sync {
    async fn save_message(&self, message: &Message) -> Result<()>;
    async fn get_messages(
        &self,
        chat_id: &ChatId,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Message>>;
    async fn save_peer(&self, peer: &PeerInfo) -> Result<()>;
    async fn update_message_status(&self, msg_id: &Uuid, status: MessageStatus) -> Result<()>;
    async fn save_transfer(&self, transfer: &TransferRecord) -> Result<()>;
    async fn get_transfers(&self, limit: usize, offset: usize) -> Result<Vec<TransferRecord>>;
    async fn update_transfer_status(
        &self,
        transfer_id: &Uuid,
        status: TransferStatus,
    ) -> Result<()>;
}
