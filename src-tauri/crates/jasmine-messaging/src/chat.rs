use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jasmine_core::{
    parse_mentions, AckStatus, ChatId, CoreError, GroupInfo, Message, MessageStatus,
    ProtocolMessage, SenderKeyInfo, StorageEngine, UserId,
};
use jasmine_crypto::{
    chunk_nonce, create_distribution_message, decrypt, encrypt, generate_sender_key,
    process_distribution_message, SenderKey,
};
use jasmine_storage::{ChatRecord, ChatType, SqliteStorage};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;
use tokio::time;
use tracing::warn;
use uuid::Uuid;

use crate::{
    MessagingError, Result, WsClient, WsClientEvent, WsPeerIdentity, WsServer, WsServerEvent,
};

pub const DEFAULT_CHAT_ACK_TIMEOUT: Duration = Duration::from_secs(5);
const MESSAGE_EDIT_BUFFER_TTL: Duration = Duration::from_secs(30 * 60);
const MESSAGE_DELETE_BUFFER_TTL: Duration = Duration::from_secs(30 * 60);
const REPLY_TO_PREVIEW_CHAR_LIMIT: usize = 100;

#[derive(Debug, Clone)]
pub struct ChatServiceConfig {
    pub ack_timeout: Duration,
}

impl Default for ChatServiceConfig {
    fn default() -> Self {
        Self {
            ack_timeout: DEFAULT_CHAT_ACK_TIMEOUT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatServiceEvent {
    MessageReceived {
        peer_id: String,
        message: Message,
    },
    GroupCreated {
        group_id: ChatId,
    },
    GroupUpdated {
        group_id: ChatId,
    },
    GroupMembersAdded {
        group_id: ChatId,
        member_ids: Vec<String>,
    },
    GroupMembersRemoved {
        group_id: ChatId,
        member_ids: Vec<String>,
    },
    MessageEdited {
        message_id: Uuid,
        new_content: String,
        edit_version: u32,
    },
    MessageDeleted {
        message_id: Uuid,
    },
    MentionReceived {
        message_id: Uuid,
        mentioned_user_id: String,
        sender_name: String,
    },
    MessageStatusUpdated {
        peer_id: String,
        message_id: Uuid,
        status: MessageStatus,
    },
}

#[derive(Debug, Clone)]
struct GroupState {
    id: ChatId,
    name: String,
    member_ids: Vec<UserId>,
    created_at_ms: i64,
}

impl GroupState {
    fn info(&self) -> GroupInfo {
        GroupInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            member_ids: self.member_ids.clone(),
        }
    }

    fn from_info(info: GroupInfo, created_at_ms: i64) -> Self {
        Self {
            id: info.id,
            name: info.name,
            member_ids: info.member_ids,
            created_at_ms,
        }
    }
}

#[allow(async_fn_in_trait)]
pub trait ChatStorage: StorageEngine {
    async fn save_group(&self, group: &GroupInfo, created_at_ms: i64) -> jasmine_core::Result<()>;
    async fn get_group(&self, group_id: &ChatId) -> jasmine_core::Result<Option<(GroupInfo, i64)>>;
    async fn save_message_with_vector_clock(
        &self,
        message: &Message,
        vector_clock: Option<&HashMap<String, u64>>,
    ) -> jasmine_core::Result<()> {
        let _ = vector_clock;
        self.save_message(message).await
    }
}

impl ChatStorage for SqliteStorage {
    async fn save_group(&self, group: &GroupInfo, created_at_ms: i64) -> jasmine_core::Result<()> {
        self.replace_chat_members(&group.id, &group.member_ids, created_at_ms)
            .await?;

        self.save_chat(&ChatRecord {
            id: group.id.clone(),
            chat_type: ChatType::Group,
            name: Some(group.name.clone()),
            created_at_ms,
        })
        .await
    }

    async fn get_group(&self, group_id: &ChatId) -> jasmine_core::Result<Option<(GroupInfo, i64)>> {
        let Some(chat) = self.get_chat(group_id).await? else {
            return Ok(None);
        };
        if chat.chat_type != ChatType::Group {
            return Ok(None);
        }

        let Some(name) = chat.name else {
            return Err(CoreError::Persistence(
                "group chat record missing name".to_string(),
            ));
        };
        let member_ids = self.get_chat_members(group_id).await?;

        Ok(Some((
            GroupInfo {
                id: chat.id,
                name,
                member_ids,
            },
            chat.created_at_ms,
        )))
    }

    async fn save_message_with_vector_clock(
        &self,
        message: &Message,
        vector_clock: Option<&HashMap<String, u64>>,
    ) -> jasmine_core::Result<()> {
        SqliteStorage::save_message_with_vector_clock(self, message, vector_clock).await
    }
}

#[derive(Clone)]
enum ReplyRoute {
    Client(Arc<WsClient>),
    Server(String),
}

struct IncomingTextMessage {
    id: String,
    chat_id: String,
    sender_id: String,
    content: String,
    timestamp: i64,
    reply_to_id: Option<String>,
    reply_to_preview: Option<String>,
    vector_clock: Option<HashMap<String, u64>>,
}

struct IncomingGroupMessage {
    group_id: String,
    sender_key_id: Uuid,
    epoch: u32,
    nonce: [u8; 12],
    encrypted_content: Vec<u8>,
}

struct IncomingMessageEdit {
    message_id: String,
    chat_id: String,
    sender_id: String,
    new_content: String,
    edit_version: u32,
    timestamp_ms: u64,
}

struct IncomingMessageDelete {
    message_id: String,
    chat_id: String,
    sender_id: String,
    timestamp_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EncodedSenderKeyDistributionPayload {
    key_id: String,
    encrypted_key_material: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GroupMessageCiphertextPayload {
    id: String,
    content: String,
    timestamp: i64,
    #[serde(default)]
    reply_to_id: Option<String>,
    #[serde(default)]
    reply_to_preview: Option<String>,
}

const GROUP_MESSAGE_AAD_PREFIX: &[u8] = b"jasmine/group-message/v1";

#[derive(Debug, Clone)]
struct PendingEdit {
    chat_id: String,
    sender_id: String,
    new_content: String,
    edit_version: u32,
    timestamp_ms: u64,
    expires_at_ms: u64,
}

impl PendingEdit {
    fn into_incoming(self, message_id: String) -> IncomingMessageEdit {
        IncomingMessageEdit {
            message_id,
            chat_id: self.chat_id,
            sender_id: self.sender_id,
            new_content: self.new_content,
            edit_version: self.edit_version,
            timestamp_ms: self.timestamp_ms,
        }
    }
}

#[derive(Debug, Clone)]
struct PendingDelete {
    chat_id: String,
    sender_id: String,
    timestamp_ms: u64,
    expires_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct DirectMessageVectorClock {
    counters: HashMap<String, u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VectorClockRelation {
    Less,
    Equal,
    Greater,
    Concurrent,
}

impl DirectMessageVectorClock {
    fn increment(&mut self, actor_id: &str) -> HashMap<String, u64> {
        let counter = self.counters.entry(actor_id.to_string()).or_insert(0);
        *counter = counter.saturating_add(1);
        self.counters.clone()
    }

    fn merge(&mut self, other: &HashMap<String, u64>) {
        for (actor_id, counter) in other {
            let current = self.counters.entry(actor_id.clone()).or_insert(0);
            *current = (*current).max(*counter);
        }
    }

    fn compare(&self, other: &HashMap<String, u64>) -> VectorClockRelation {
        compare_vector_clocks(&self.counters, other)
    }
}

impl PendingDelete {
    fn into_incoming(self, message_id: String) -> IncomingMessageDelete {
        IncomingMessageDelete {
            message_id,
            chat_id: self.chat_id,
            sender_id: self.sender_id,
            timestamp_ms: self.timestamp_ms,
        }
    }
}

impl ReplyRoute {
    async fn send(&self, server: &WsServer, message: ProtocolMessage) -> Result<()> {
        match self {
            Self::Client(client) => client.send(message).await,
            Self::Server(peer_id) => server.send_to(peer_id, message).await,
        }
    }
}

struct ChatServiceInner<S: ChatStorage + 'static> {
    local_peer: WsPeerIdentity,
    server: Arc<WsServer>,
    storage: Arc<S>,
    config: ChatServiceConfig,
    events: broadcast::Sender<ChatServiceEvent>,
    clients: Mutex<HashMap<String, Arc<WsClient>>>,
    client_tasks: Mutex<HashMap<String, JoinHandle<()>>>,
    background_tasks: Mutex<Vec<JoinHandle<()>>>,
    pending_acks: Mutex<HashMap<String, oneshot::Sender<()>>>,
    pending_edits: Mutex<HashMap<String, Vec<PendingEdit>>>,
    pending_deletes: Mutex<HashMap<String, PendingDelete>>,
    direct_message_vector_clocks: Mutex<HashMap<String, DirectMessageVectorClock>>,
    group_sender_key_counters: Mutex<HashMap<String, u32>>,
}

pub struct ChatService<S: ChatStorage + 'static> {
    inner: Arc<ChatServiceInner<S>>,
}

impl<S: ChatStorage + 'static> Clone for ChatService<S> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<S: ChatStorage + 'static> ChatService<S> {
    pub fn new(local_peer: WsPeerIdentity, server: Arc<WsServer>, storage: Arc<S>) -> Self {
        Self::with_config(local_peer, server, storage, ChatServiceConfig::default())
    }

    pub fn with_config(
        local_peer: WsPeerIdentity,
        server: Arc<WsServer>,
        storage: Arc<S>,
        config: ChatServiceConfig,
    ) -> Self {
        let (events, _) = broadcast::channel(256);
        let inner = Arc::new(ChatServiceInner {
            local_peer,
            server,
            storage,
            config,
            events,
            clients: Mutex::new(HashMap::new()),
            client_tasks: Mutex::new(HashMap::new()),
            background_tasks: Mutex::new(Vec::new()),
            pending_acks: Mutex::new(HashMap::new()),
            pending_edits: Mutex::new(HashMap::new()),
            pending_deletes: Mutex::new(HashMap::new()),
            direct_message_vector_clocks: Mutex::new(HashMap::new()),
            group_sender_key_counters: Mutex::new(HashMap::new()),
        });

        let mut server_events = inner.server.subscribe();
        let inner_for_server = Arc::clone(&inner);
        let server_task = tokio::spawn(async move {
            loop {
                match server_events.recv().await {
                    Ok(event) => inner_for_server.handle_server_event(event).await,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        inner
            .background_tasks
            .lock()
            .expect("lock chat background tasks")
            .push(server_task);

        Self { inner }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ChatServiceEvent> {
        self.inner.events.subscribe()
    }

    pub fn direct_chat_id(peer_id: &str) -> Result<ChatId> {
        Ok(ChatId(parse_uuid(peer_id, "peer_id")?))
    }

    pub async fn register_client(&self, client: Arc<WsClient>) -> Result<()> {
        let remote_peer = client.remote_peer().ok_or_else(|| {
            MessagingError::Transport("websocket client remote peer is unavailable".to_string())
        })?;
        let peer_id = remote_peer.identity.device_id.clone();

        let previous_client = self
            .inner
            .clients
            .lock()
            .expect("lock chat clients")
            .insert(peer_id.clone(), Arc::clone(&client));
        if let Some(previous_task) = self
            .inner
            .client_tasks
            .lock()
            .expect("lock chat client tasks")
            .insert(
                peer_id.clone(),
                spawn_client_listener(
                    Arc::clone(&self.inner),
                    peer_id.clone(),
                    Arc::clone(&client),
                ),
            )
        {
            previous_task.abort();
        }
        if let Some(previous_client) = previous_client {
            disconnect_client_blocking(previous_client);
        }

        Ok(())
    }

    pub async fn send_message(&self, peer_id: &str, content: impl Into<String>) -> Result<Message> {
        self.send_message_with_reply(peer_id, content, None).await
    }

    pub async fn send_message_with_reply(
        &self,
        peer_id: &str,
        content: impl Into<String>,
        reply_to_id: Option<String>,
    ) -> Result<Message> {
        let chat_id = Self::direct_chat_id(peer_id)?;
        let sender_id = user_id_from_peer_id(&self.inner.local_peer.device_id)?;
        let content = content.into();
        let reply_to_preview = self
            .inner
            .load_reply_preview(&chat_id, reply_to_id.as_deref())
            .await?;
        let vector_clock = self.inner.next_outgoing_direct_vector_clock(peer_id);
        let mut message = Message {
            id: Uuid::new_v4(),
            chat_id,
            sender_id,
            content,
            timestamp_ms: now_ms(),
            status: MessageStatus::Sent,
            edit_version: 0,
            edited_at: None,
            is_deleted: false,
            deleted_at: None,
            reply_to_id,
            reply_to_preview,
        };

        self.inner
            .save_message_with_vector_clock(message.clone(), Some(vector_clock.clone()))
            .await?;
        parse_mentions(&message.content);
        self.inner
            .emit_status(peer_id.to_string(), message.id, MessageStatus::Sent);

        let protocol_message = ProtocolMessage::TextMessage {
            id: message.id.to_string(),
            chat_id: peer_id.to_string(),
            sender_id: self.inner.local_peer.device_id.clone(),
            content: message.content.clone(),
            timestamp: message.timestamp_ms,
            reply_to_id: message.reply_to_id.clone(),
            reply_to_preview: message.reply_to_preview.clone(),
            vector_clock: Some(vector_clock),
        };

        Arc::clone(&self.inner).arm_ack_timeout(peer_id.to_string(), message.id);

        if let Err(error) = self.inner.send_to_peer(peer_id, protocol_message).await {
            self.inner.cancel_ack_timeout(message.id);
            self.inner
                .mark_message_status(peer_id.to_string(), message.id, MessageStatus::Failed)
                .await?;
            message.status = MessageStatus::Failed;
            warn!(peer_id, message_id = %message.id, error = %error, "direct message send failed");
            return Ok(message);
        }
        Ok(message)
    }

    pub async fn edit_message(
        &self,
        message_id: &str,
        new_content: &str,
        sender_id: &str,
    ) -> Result<()> {
        if sender_id != self.inner.local_peer.device_id {
            return Err(MessagingError::Validation(format!(
                "unauthorized edit sender {sender_id} for local peer {}",
                self.inner.local_peer.device_id
            )));
        }

        let stored_message = self
            .inner
            .load_message(message_id.to_string())
            .await?
            .ok_or_else(|| MessagingError::Validation(format!("message {message_id} not found")))?;
        let editor_id = user_id_from_peer_id(sender_id)?;

        if stored_message.sender_id != editor_id {
            return Err(MessagingError::Validation(format!(
                "unauthorized edit for message {message_id} by sender {sender_id}"
            )));
        }

        let edit_version = stored_message.edit_version.saturating_add(1);
        let timestamp_ms = now_ms_u64();
        let message_uuid = stored_message.id;
        let protocol_message = ProtocolMessage::MessageEdit {
            message_id: message_id.to_string(),
            chat_id: stored_message.chat_id.0.to_string(),
            sender_id: sender_id.to_string(),
            new_content: new_content.to_string(),
            edit_version,
            timestamp_ms,
        };

        self.inner
            .update_message_content(
                message_id.to_string(),
                new_content.to_string(),
                edit_version,
                timestamp_ms,
            )
            .await?;
        self.inner
            .fanout_message_edit(&stored_message, protocol_message)
            .await?;
        self.inner
            .emit_message_edited(message_uuid, new_content.to_string(), edit_version);

        Ok(())
    }

    pub async fn delete_message(&self, message_id: &str, sender_id: &str) -> Result<()> {
        if sender_id != self.inner.local_peer.device_id {
            return Err(MessagingError::Validation(format!(
                "unauthorized delete sender {sender_id} for local peer {}",
                self.inner.local_peer.device_id
            )));
        }

        let stored_message = self
            .inner
            .load_message(message_id.to_string())
            .await?
            .ok_or_else(|| MessagingError::Validation(format!("message {message_id} not found")))?;
        let deleter_id = user_id_from_peer_id(sender_id)?;

        if stored_message.sender_id != deleter_id {
            return Err(MessagingError::Validation(format!(
                "unauthorized delete for message {message_id} by sender {sender_id}"
            )));
        }

        if stored_message.is_deleted {
            return Ok(());
        }

        let timestamp_ms = now_ms_u64();
        let message_uuid = stored_message.id;
        let protocol_message = ProtocolMessage::MessageDelete {
            message_id: message_id.to_string(),
            chat_id: stored_message.chat_id.0.to_string(),
            sender_id: sender_id.to_string(),
            timestamp_ms,
        };

        self.inner
            .mark_message_deleted(message_id.to_string(), timestamp_ms)
            .await?;
        self.inner
            .fanout_message_delete(&stored_message, protocol_message)
            .await?;
        self.inner.emit_message_deleted(message_uuid);

        Ok(())
    }

    pub async fn create_group(
        &self,
        name: impl Into<String>,
        member_ids: Vec<String>,
    ) -> Result<GroupInfo> {
        let group = GroupState {
            id: ChatId(Uuid::new_v4()),
            name: normalize_group_name(name.into())?,
            member_ids: build_group_members(&self.inner.local_peer.device_id, member_ids)?,
            created_at_ms: now_ms(),
        };

        self.inner.save_group(group.clone()).await?;
        self.inner.emit_group_created(group.id.clone());
        self.inner
            .fanout_group_create(
                &group,
                other_group_member_peer_ids(&group, &self.inner.local_peer.device_id),
            )
            .await;

        Ok(group.info())
    }

    pub async fn send_to_group(
        &self,
        group_id: &ChatId,
        content: impl Into<String>,
    ) -> Result<Message> {
        self.send_to_group_with_reply(group_id, content, None).await
    }

    pub async fn send_to_group_with_reply(
        &self,
        group_id: &ChatId,
        content: impl Into<String>,
        reply_to_id: Option<String>,
    ) -> Result<Message> {
        let group = self
            .inner
            .load_group(group_id.clone())
            .await?
            .ok_or_else(|| MessagingError::Validation(format!("unknown group {}", group_id.0)))?;
        ensure_group_contains_peer(&group, &self.inner.local_peer.device_id)?;

        let recipients = other_group_member_peer_ids(&group, &self.inner.local_peer.device_id);
        let sender_id = user_id_from_peer_id(&self.inner.local_peer.device_id)?;
        let content = content.into();
        let reply_to_preview = self
            .inner
            .load_reply_preview(&group.id, reply_to_id.as_deref())
            .await?;
        let mut message = Message {
            id: Uuid::new_v4(),
            chat_id: group.id.clone(),
            sender_id,
            content,
            timestamp_ms: now_ms(),
            status: MessageStatus::Sent,
            edit_version: 0,
            edited_at: None,
            is_deleted: false,
            deleted_at: None,
            reply_to_id,
            reply_to_preview,
        };

        self.inner.save_message(message.clone()).await?;
        parse_mentions(&message.content);

        let sender_key = self
            .inner
            .ensure_sender_key_distributed(&group, &recipients)
            .await?;

        let mut delivered_to_any = recipients.is_empty();
        for peer_id in recipients {
            let protocol_message = self.inner.encrypt_group_message(&message, &sender_key)?;

            match self.inner.send_to_peer(&peer_id, protocol_message).await {
                Ok(()) => delivered_to_any = true,
                Err(MessagingError::PeerNotConnected(_)) => continue,
                Err(error) => {
                    warn!(peer_id, group_id = %group.id.0, error = %error, "group message send failed");
                }
            }
        }

        message.status = if delivered_to_any {
            MessageStatus::Delivered
        } else {
            MessageStatus::Failed
        };
        self.inner
            .update_message_status(message.id, message.status.clone())
            .await?;

        Ok(message)
    }

    pub async fn add_group_members(
        &self,
        group_id: &ChatId,
        member_ids: Vec<String>,
    ) -> Result<GroupInfo> {
        let group = self
            .inner
            .load_group(group_id.clone())
            .await?
            .ok_or_else(|| MessagingError::Validation(format!("unknown group {}", group_id.0)))?;
        ensure_group_contains_peer(&group, &self.inner.local_peer.device_id)?;

        let added_member_ids = add_unique_members(&group.member_ids, member_ids)?;
        if added_member_ids.is_empty() {
            return Ok(group.info());
        }

        let mut updated = group.clone();
        updated.member_ids.extend(added_member_ids.iter().cloned());

        self.inner.save_group(updated.clone()).await?;
        self.inner
            .emit_group_members_added(&updated.id, user_ids_to_peer_ids(&added_member_ids));
        self.inner.emit_group_updated(updated.id.clone());
        self.inner
            .fanout_group_invite(
                &updated.id,
                &self.inner.local_peer.device_id,
                user_ids_to_peer_ids(&added_member_ids),
            )
            .await;
        self.inner
            .fanout_group_create(
                &updated,
                other_group_member_peer_ids(&updated, &self.inner.local_peer.device_id),
            )
            .await;
        if let Some(sender_key) = self
            .inner
            .load_sender_key(&updated.id.0.to_string(), &self.inner.local_peer.device_id)
            .await?
        {
            if let Ok(sender_key) = sender_key_from_info(&sender_key) {
                self.inner
                    .distribute_sender_key(
                        &updated.id.0.to_string(),
                        &self.inner.local_peer.device_id,
                        &sender_key,
                        &user_ids_to_peer_ids(&added_member_ids),
                    )
                    .await;
            }
        }

        Ok(updated.info())
    }

    pub async fn remove_group_members(
        &self,
        group_id: &ChatId,
        member_ids: Vec<String>,
    ) -> Result<GroupInfo> {
        let group = self
            .inner
            .load_group(group_id.clone())
            .await?
            .ok_or_else(|| MessagingError::Validation(format!("unknown group {}", group_id.0)))?;
        ensure_group_contains_peer(&group, &self.inner.local_peer.device_id)?;

        let updated = GroupState {
            member_ids: remove_members(&group.member_ids, member_ids)?,
            ..group.clone()
        };
        let removed_member_ids = removed_members(&group.member_ids, &updated.member_ids);
        let local_peer_left_group = removed_member_ids.len() == 1
            && removed_member_ids[0] == self.inner.local_peer.device_id;

        self.inner.save_group(updated.clone()).await?;
        self.inner
            .emit_group_members_removed(&updated.id, removed_member_ids.clone());
        self.inner.emit_group_updated(updated.id.clone());
        if local_peer_left_group {
            self.inner
                .fanout_group_member_left(
                    &updated.id,
                    &self.inner.local_peer.device_id,
                    user_ids_to_peer_ids(&updated.member_ids),
                )
                .await;
            return Ok(updated.info());
        }

        self.inner
            .rotate_sender_key_on_membership_reduction(&group, &updated)
            .await?;
        self.inner
            .fanout_group_create(
                &updated,
                other_group_member_peer_ids(&group, &self.inner.local_peer.device_id),
            )
            .await;

        Ok(updated.info())
    }

    pub async fn load_messages(
        &self,
        chat_id: &ChatId,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Message>> {
        self.inner
            .load_messages(chat_id.clone(), limit, offset)
            .await
    }

    pub async fn shutdown(&self) -> Result<()> {
        let pending_acks: Vec<oneshot::Sender<()>> = self
            .inner
            .pending_acks
            .lock()
            .expect("lock pending chat acks")
            .drain()
            .map(|(_, cancel)| cancel)
            .collect();
        for cancel in pending_acks {
            let _ = cancel.send(());
        }

        let background_tasks: Vec<JoinHandle<()>> = self
            .inner
            .background_tasks
            .lock()
            .expect("lock chat background tasks")
            .drain(..)
            .collect();
        for task in background_tasks {
            task.abort();
        }

        let client_tasks: Vec<JoinHandle<()>> = self
            .inner
            .client_tasks
            .lock()
            .expect("lock chat client tasks")
            .drain()
            .map(|(_, task)| task)
            .collect();
        for task in client_tasks {
            task.abort();
        }

        let clients: Vec<Arc<WsClient>> = self
            .inner
            .clients
            .lock()
            .expect("lock chat clients")
            .drain()
            .map(|(_, client)| client)
            .collect();
        for client in clients {
            let _ = client.disconnect().await;
        }

        Ok(())
    }
}

fn spawn_client_listener<S: ChatStorage + 'static>(
    inner: Arc<ChatServiceInner<S>>,
    peer_id: String,
    client: Arc<WsClient>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut events = client.subscribe();

        loop {
            match events.recv().await {
                Ok(WsClientEvent::MessageReceived { message }) => {
                    let route = ReplyRoute::Client(Arc::clone(&client));
                    if let Err(error) = inner
                        .process_protocol_message(peer_id.clone(), route, message)
                        .await
                    {
                        warn!(peer_id, error = %error, "chat client message handling failed");
                    }
                }
                Ok(WsClientEvent::Disconnected { reason }) => {
                    inner
                        .clients
                        .lock()
                        .expect("lock chat clients")
                        .remove(&peer_id);
                    warn!(peer_id, reason = ?reason, "chat client disconnected");
                    break;
                }
                Ok(WsClientEvent::CallSignalingReceived { .. }) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

fn disconnect_client_blocking(client: Arc<WsClient>) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build chat client disconnect runtime");
        let disconnect_outcome = runtime.block_on(async move { client.disconnect().await });
        let _ = tx.send(disconnect_outcome);
    });

    let _ = rx.recv();
}

impl<S: ChatStorage + 'static> ChatServiceInner<S> {
    async fn handle_server_event(&self, event: WsServerEvent) {
        if let WsServerEvent::MessageReceived { peer, message } = event {
            let peer_id = peer.identity.device_id;
            let route = ReplyRoute::Server(peer_id.clone());

            if let Err(error) = self
                .process_protocol_message(peer_id.clone(), route, message)
                .await
            {
                warn!(peer_id, error = %error, "chat server message handling failed");
            }
        }
    }

    async fn process_protocol_message(
        &self,
        peer_id: String,
        reply_route: ReplyRoute,
        message: ProtocolMessage,
    ) -> Result<()> {
        match message {
            ProtocolMessage::TextMessage {
                id,
                chat_id,
                sender_id,
                content,
                timestamp,
                reply_to_id,
                reply_to_preview,
                vector_clock,
            } => {
                self.handle_incoming_text_message(
                    peer_id,
                    reply_route,
                    IncomingTextMessage {
                        id,
                        chat_id,
                        sender_id,
                        content,
                        timestamp,
                        reply_to_id,
                        reply_to_preview,
                        vector_clock,
                    },
                )
                .await
            }
            ProtocolMessage::GroupMessage {
                group_id,
                sender_key_id,
                epoch,
                nonce,
                encrypted_content,
            } => {
                self.handle_incoming_group_message(
                    peer_id,
                    reply_route,
                    IncomingGroupMessage {
                        group_id,
                        sender_key_id,
                        epoch,
                        nonce,
                        encrypted_content,
                    },
                )
                .await
            }
            ProtocolMessage::GroupCreate {
                group_id,
                name,
                members,
            } => {
                self.handle_group_create(peer_id, group_id, name, members)
                    .await
            }
            ProtocolMessage::GroupInvite { .. } => Ok(()),
            ProtocolMessage::GroupMemberLeft {
                group_id,
                member_id,
            } => {
                self.handle_group_member_left(peer_id, group_id, member_id)
                    .await
            }
            ProtocolMessage::SenderKeyDistribution {
                group_id,
                sender_key_data,
                epoch,
            } => {
                self.handle_sender_key_distribution(peer_id, group_id, sender_key_data, epoch)
                    .await
            }
            ProtocolMessage::SenderKeyRequest {
                group_id,
                requesting_peer_id,
            } => {
                self.handle_sender_key_request(peer_id, group_id, requesting_peer_id)
                    .await
            }
            ProtocolMessage::Ack { message_id, .. } => {
                let message_uuid = parse_uuid(&message_id, "message_id")?;
                let cancel = self
                    .pending_acks
                    .lock()
                    .expect("lock pending chat acks")
                    .remove(&message_id);

                if let Some(cancel) = cancel {
                    let _ = cancel.send(());
                    self.mark_message_status(peer_id, message_uuid, MessageStatus::Delivered)
                        .await?;
                }

                Ok(())
            }
            ProtocolMessage::MessageEdit {
                message_id,
                chat_id,
                sender_id,
                new_content,
                edit_version,
                timestamp_ms,
            } => {
                self.handle_incoming_message_edit(
                    peer_id,
                    IncomingMessageEdit {
                        message_id,
                        chat_id,
                        sender_id,
                        new_content,
                        edit_version,
                        timestamp_ms,
                    },
                )
                .await
            }
            ProtocolMessage::MessageDelete {
                message_id,
                chat_id,
                sender_id,
                timestamp_ms,
            } => {
                self.handle_incoming_message_delete(
                    peer_id,
                    IncomingMessageDelete {
                        message_id,
                        chat_id,
                        sender_id,
                        timestamp_ms,
                    },
                )
                .await
            }
            _ => Ok(()),
        }
    }

    async fn handle_incoming_text_message(
        &self,
        peer_id: String,
        reply_route: ReplyRoute,
        text: IncomingTextMessage,
    ) -> Result<()> {
        if text.sender_id != peer_id {
            return Err(MessagingError::Validation(format!(
                "incoming sender_id {} does not match connected peer {peer_id}",
                text.sender_id
            )));
        }

        let sender_user_id = user_id_from_peer_id(&peer_id)?;
        let message_id = parse_uuid(&text.id, "message_id")?;
        let is_direct_message = text.chat_id == self.local_peer.device_id;
        let incoming_vector_clock = text.vector_clock.clone();
        let stored_chat_id = if is_direct_message {
            ChatId(parse_uuid(&peer_id, "peer_id")?)
        } else {
            let group_chat_id = ChatId(parse_uuid(&text.chat_id, "chat_id")?);
            let Some(group) = self.load_group(group_chat_id.clone()).await? else {
                return Ok(());
            };

            if !group.member_ids.contains(&sender_user_id)
                || !group
                    .member_ids
                    .contains(&user_id_from_peer_id(&self.local_peer.device_id)?)
            {
                return Ok(());
            }

            group_chat_id
        };
        let mentioned_user_id = local_mention_user_id(&text.content, &self.local_peer.device_id);

        let stored_message = Message {
            id: message_id,
            chat_id: stored_chat_id,
            sender_id: sender_user_id,
            content: text.content,
            timestamp_ms: text.timestamp,
            status: MessageStatus::Delivered,
            edit_version: 0,
            edited_at: None,
            is_deleted: false,
            deleted_at: None,
            reply_to_id: text.reply_to_id,
            reply_to_preview: text.reply_to_preview,
        };

        self.save_message_with_vector_clock(
            stored_message.clone(),
            if is_direct_message {
                incoming_vector_clock.clone()
            } else {
                None
            },
        )
        .await?;
        if is_direct_message {
            let _vector_clock_relation =
                self.merge_incoming_direct_vector_clock(&peer_id, incoming_vector_clock.as_ref());
        }
        let _ = self.events.send(ChatServiceEvent::MessageReceived {
            peer_id: peer_id.clone(),
            message: stored_message,
        });
        if let Some(mentioned_user_id) = mentioned_user_id {
            self.emit_mention_received(
                message_id,
                mentioned_user_id,
                self.peer_display_name(&peer_id),
            );
        }
        let pending_delete = self.take_pending_delete(&text.id);
        let delete_timestamp_ms = pending_delete.as_ref().map(|delete| delete.timestamp_ms);
        self.apply_buffered_message_edits(&text.id, delete_timestamp_ms)
            .await;
        if let Some(delete) = pending_delete {
            self.handle_incoming_message_delete(
                delete.sender_id.clone(),
                delete.into_incoming(text.id.clone()),
            )
            .await?;
        }

        reply_route
            .send(
                &self.server,
                ProtocolMessage::Ack {
                    message_id: text.id,
                    status: AckStatus::Received,
                },
            )
            .await
    }

    async fn handle_incoming_message_edit(
        &self,
        peer_id: String,
        edit: IncomingMessageEdit,
    ) -> Result<()> {
        if edit.sender_id != peer_id {
            return Err(MessagingError::Validation(format!(
                "incoming edit sender_id {} does not match connected peer {peer_id}",
                edit.sender_id
            )));
        }

        let Some(stored_message) = self.load_message(edit.message_id.clone()).await? else {
            self.buffer_pending_edit(
                edit.message_id,
                PendingEdit {
                    chat_id: edit.chat_id,
                    sender_id: edit.sender_id,
                    new_content: edit.new_content,
                    edit_version: edit.edit_version,
                    timestamp_ms: edit.timestamp_ms,
                    expires_at_ms: now_ms_u64()
                        .saturating_add(MESSAGE_EDIT_BUFFER_TTL.as_millis() as u64),
                },
            );
            return Ok(());
        };

        if stored_message.is_deleted {
            return Ok(());
        }

        let expected_sender_id = user_id_from_peer_id(&edit.sender_id)?;
        if stored_message.sender_id != expected_sender_id {
            return Err(MessagingError::Validation(format!(
                "unauthorized incoming edit for message {} by sender {}",
                edit.message_id, edit.sender_id
            )));
        }

        if !incoming_message_matches_message_chat(
            &stored_message,
            &peer_id,
            &self.local_peer.device_id,
            &edit.chat_id,
        ) {
            return Err(MessagingError::Validation(format!(
                "incoming edit chat_id {} does not match stored message {}",
                edit.chat_id, edit.message_id
            )));
        }

        if !should_apply_message_edit(&stored_message, edit.edit_version, edit.timestamp_ms) {
            return Ok(());
        }

        let message_id = edit.message_id.clone();
        let new_content = edit.new_content.clone();

        self.update_message_content(
            message_id,
            new_content.clone(),
            edit.edit_version,
            edit.timestamp_ms,
        )
        .await?;
        self.emit_message_edited(stored_message.id, new_content, edit.edit_version);

        Ok(())
    }

    async fn handle_incoming_message_delete(
        &self,
        peer_id: String,
        delete: IncomingMessageDelete,
    ) -> Result<()> {
        if delete.sender_id != peer_id {
            return Err(MessagingError::Validation(format!(
                "incoming delete sender_id {} does not match connected peer {peer_id}",
                delete.sender_id
            )));
        }

        let Some(stored_message) = self.load_message(delete.message_id.clone()).await? else {
            self.buffer_pending_delete(
                delete.message_id,
                PendingDelete {
                    chat_id: delete.chat_id,
                    sender_id: delete.sender_id,
                    timestamp_ms: delete.timestamp_ms,
                    expires_at_ms: now_ms_u64()
                        .saturating_add(MESSAGE_DELETE_BUFFER_TTL.as_millis() as u64),
                },
            );
            return Ok(());
        };

        if stored_message.is_deleted {
            return Ok(());
        }

        let expected_sender_id = user_id_from_peer_id(&delete.sender_id)?;
        if stored_message.sender_id != expected_sender_id {
            return Err(MessagingError::Validation(format!(
                "unauthorized incoming delete for message {} by sender {}",
                delete.message_id, delete.sender_id
            )));
        }

        if !incoming_message_matches_message_chat(
            &stored_message,
            &peer_id,
            &self.local_peer.device_id,
            &delete.chat_id,
        ) {
            return Err(MessagingError::Validation(format!(
                "incoming delete chat_id {} does not match stored message {}",
                delete.chat_id, delete.message_id
            )));
        }

        self.mark_message_deleted(delete.message_id, delete.timestamp_ms)
            .await?;
        self.emit_message_deleted(stored_message.id);

        Ok(())
    }

    async fn handle_group_create(
        &self,
        peer_id: String,
        group_id: String,
        name: String,
        members: Vec<String>,
    ) -> Result<()> {
        let member_ids = parse_member_ids(members)?;
        if !member_ids.contains(&user_id_from_peer_id(&peer_id)?) {
            return Err(MessagingError::Validation(format!(
                "group snapshot members must include sender {peer_id}"
            )));
        }

        let group_id = ChatId(parse_uuid(&group_id, "group_id")?);
        let previous_group = self.load_group(group_id.clone()).await?;
        let created_at_ms = previous_group
            .as_ref()
            .map(|group| group.created_at_ms)
            .unwrap_or_else(now_ms);
        let next_group = GroupState {
            id: group_id,
            name: normalize_group_name(name)?,
            member_ids,
            created_at_ms,
        };
        self.save_group(next_group.clone()).await?;
        self.emit_group_state_change(previous_group.as_ref(), &next_group);
        Ok(())
    }

    async fn handle_sender_key_distribution(
        &self,
        peer_id: String,
        group_id: String,
        sender_key_data: String,
        epoch: u32,
    ) -> Result<()> {
        let session_key = self.sender_key_distribution_material(&peer_id)?;
        let distribution =
            parse_protocol_sender_key_distribution(&peer_id, group_id, sender_key_data, epoch)?;
        let sender_key = process_distribution_message(&distribution, &session_key)
            .map_err(|error| MessagingError::Validation(error.to_string()))?;

        self.save_sender_key(&distribution.group_id, &peer_id, &sender_key)
            .await
    }

    async fn handle_sender_key_request(
        &self,
        peer_id: String,
        group_id: String,
        requesting_peer_id: String,
    ) -> Result<()> {
        if requesting_peer_id != peer_id {
            return Err(MessagingError::Validation(format!(
                "sender key request requesting_peer_id {} does not match connected peer {peer_id}",
                requesting_peer_id
            )));
        }

        let group_chat_id = ChatId(parse_uuid(&group_id, "group_id")?);
        let Some(group) = self.load_group(group_chat_id).await? else {
            return Ok(());
        };

        let requester_user_id = user_id_from_peer_id(&peer_id)?;
        let local_user_id = user_id_from_peer_id(&self.local_peer.device_id)?;
        if !group.member_ids.contains(&requester_user_id)
            || !group.member_ids.contains(&local_user_id)
        {
            return Ok(());
        }

        let Some(stored_sender_key) = self
            .load_sender_key(&group_id, &self.local_peer.device_id)
            .await?
        else {
            return Ok(());
        };

        let sender_key = sender_key_from_info(&stored_sender_key)?;
        let session_key = self.sender_key_distribution_material(&peer_id)?;
        let distribution = create_distribution_message(
            group_id,
            self.local_peer.device_id.clone(),
            &sender_key,
            &session_key,
        )
        .map_err(|error| MessagingError::Validation(error.to_string()))?;
        let message = protocol_sender_key_distribution_message(&distribution)?;
        self.send_to_peer(&peer_id, message).await
    }

    async fn handle_group_member_left(
        &self,
        peer_id: String,
        group_id: String,
        member_id: String,
    ) -> Result<()> {
        if member_id != peer_id {
            return Err(MessagingError::Validation(format!(
                "group member left member_id {} does not match connected peer {peer_id}",
                member_id
            )));
        }

        let group_chat_id = ChatId(parse_uuid(&group_id, "group_id")?);
        let Some(group) = self.load_group(group_chat_id).await? else {
            return Ok(());
        };

        let departed_user_id = user_id_from_peer_id(&member_id)?;
        let updated_member_ids = group
            .member_ids
            .iter()
            .filter(|member| **member != departed_user_id)
            .cloned()
            .collect::<Vec<_>>();
        let updated = GroupState {
            member_ids: updated_member_ids,
            ..group.clone()
        };

        if updated.member_ids != group.member_ids {
            self.save_group(updated.clone()).await?;
            self.emit_group_members_removed(&updated.id, vec![member_id]);
            self.emit_group_updated(updated.id.clone());
        }

        self.rotate_sender_key_after_group_member_left(&updated)
            .await?;
        self.fanout_group_create(
            &updated,
            other_group_member_peer_ids(&updated, &self.local_peer.device_id),
        )
        .await;
        Ok(())
    }

    async fn handle_incoming_group_message(
        &self,
        peer_id: String,
        reply_route: ReplyRoute,
        incoming: IncomingGroupMessage,
    ) -> Result<()> {
        let group_id = incoming.group_id;
        let sender_key_id = incoming.sender_key_id;
        let epoch = incoming.epoch;
        let nonce = incoming.nonce;
        let encrypted_content = incoming.encrypted_content;

        let Some(stored_sender_key) = self
            .load_sender_key_by_epoch(&group_id, &peer_id, epoch)
            .await?
        else {
            self.request_sender_key_recovery(reply_route, &peer_id, &group_id)
                .await;
            return Ok(());
        };

        if stored_sender_key.key_id != sender_key_id {
            return Err(MessagingError::Validation(format!(
                "sender key id mismatch for group {group_id}, sender {peer_id}, epoch {epoch}"
            )));
        }

        let key_material = sender_key_material_array(&stored_sender_key)?;
        let plaintext = decrypt(
            &key_material,
            &nonce,
            &encrypted_content,
            &group_message_aad(&group_id, &peer_id, &sender_key_id, epoch),
        )
        .map_err(|error| MessagingError::Validation(error.to_string()))?;
        let payload: GroupMessageCiphertextPayload =
            serde_json::from_slice(&plaintext).map_err(|error| {
                MessagingError::Validation(format!("invalid group message payload: {error}"))
            })?;

        self.handle_incoming_text_message(
            peer_id.clone(),
            reply_route,
            IncomingTextMessage {
                id: payload.id,
                chat_id: group_id,
                sender_id: peer_id,
                content: payload.content,
                timestamp: payload.timestamp,
                reply_to_id: payload.reply_to_id,
                reply_to_preview: payload.reply_to_preview,
                vector_clock: None,
            },
        )
        .await
    }

    async fn request_sender_key_recovery(
        &self,
        reply_route: ReplyRoute,
        peer_id: &str,
        group_id: &str,
    ) {
        let request = ProtocolMessage::SenderKeyRequest {
            group_id: group_id.to_string(),
            requesting_peer_id: self.local_peer.device_id.clone(),
        };

        if let Err(error) = reply_route.send(&self.server, request).await {
            warn!(peer_id, group_id, error = %error, "sender key recovery request failed");
        }
    }

    async fn send_to_peer(&self, peer_id: &str, message: ProtocolMessage) -> Result<()> {
        let client = self
            .clients
            .lock()
            .expect("lock chat clients")
            .get(peer_id)
            .cloned();
        if let Some(client) = client {
            return client.send(message).await;
        }

        if self
            .server
            .connected_peers()
            .into_iter()
            .any(|peer| peer.identity.device_id == peer_id)
        {
            return self.server.send_to(peer_id, message).await;
        }

        Err(MessagingError::PeerNotConnected(peer_id.to_string()))
    }

    async fn mark_message_status(
        &self,
        peer_id: String,
        message_id: Uuid,
        status: MessageStatus,
    ) -> Result<()> {
        self.update_message_status(message_id, status.clone())
            .await?;
        self.emit_status(peer_id, message_id, status);
        Ok(())
    }

    fn emit_status(&self, peer_id: String, message_id: Uuid, status: MessageStatus) {
        let _ = self.events.send(ChatServiceEvent::MessageStatusUpdated {
            peer_id,
            message_id,
            status,
        });
    }

    fn arm_ack_timeout(self: Arc<Self>, peer_id: String, message_id: Uuid) {
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let message_key = message_id.to_string();

        if let Some(previous_cancel) = self
            .pending_acks
            .lock()
            .expect("lock pending chat acks")
            .insert(message_key.clone(), cancel_tx)
        {
            let _ = previous_cancel.send(());
        }

        tokio::spawn(async move {
            tokio::select! {
                _ = time::sleep(self.config.ack_timeout) => {
                    let expired = self
                        .pending_acks
                        .lock()
                        .expect("lock pending chat acks")
                        .remove(&message_key)
                        .is_some();
                    if expired {
                        if let Err(error) = self.mark_message_status(peer_id.clone(), message_id, MessageStatus::Failed).await {
                            warn!(peer_id, message_id = %message_id, error = %error, "chat ack timeout status update failed");
                        }
                    }
                }
                _ = cancel_rx => {}
            }
        });
    }

    fn cancel_ack_timeout(&self, message_id: Uuid) {
        if let Some(cancel) = self
            .pending_acks
            .lock()
            .expect("lock pending chat acks")
            .remove(&message_id.to_string())
        {
            let _ = cancel.send(());
        }
    }

    async fn fanout_group_create(&self, group: &GroupState, peer_ids: Vec<String>) {
        let members = user_ids_to_peer_ids(&group.member_ids);
        for peer_id in peer_ids {
            let message = ProtocolMessage::GroupCreate {
                group_id: group.id.0.to_string(),
                name: group.name.clone(),
                members: members.clone(),
            };

            match self.send_to_peer(&peer_id, message).await {
                Ok(()) | Err(MessagingError::PeerNotConnected(_)) => {}
                Err(error) => {
                    warn!(peer_id, group_id = %group.id.0, error = %error, "group snapshot send failed");
                }
            }
        }
    }

    async fn fanout_group_invite(
        &self,
        group_id: &ChatId,
        inviter_id: &str,
        peer_ids: Vec<String>,
    ) {
        for peer_id in peer_ids {
            let message = ProtocolMessage::GroupInvite {
                group_id: group_id.0.to_string(),
                inviter_id: inviter_id.to_string(),
            };

            match self.send_to_peer(&peer_id, message).await {
                Ok(()) | Err(MessagingError::PeerNotConnected(_)) => {}
                Err(error) => {
                    warn!(peer_id, group_id = %group_id.0, error = %error, "group invite send failed");
                }
            }
        }
    }

    async fn fanout_message_edit(
        &self,
        message: &Message,
        protocol_message: ProtocolMessage,
    ) -> Result<()> {
        if let Some(group) = self.load_group(message.chat_id.clone()).await? {
            for peer_id in other_group_member_peer_ids(&group, &self.local_peer.device_id) {
                match self.send_to_peer(&peer_id, protocol_message.clone()).await {
                    Ok(()) | Err(MessagingError::PeerNotConnected(_)) => {}
                    Err(error) => {
                        warn!(peer_id, message_id = %message.id, error = %error, "message edit send failed");
                    }
                }
            }

            return Ok(());
        }

        let peer_id = message.chat_id.0.to_string();
        match self.send_to_peer(&peer_id, protocol_message).await {
            Ok(()) | Err(MessagingError::PeerNotConnected(_)) => Ok(()),
            Err(error) => {
                warn!(peer_id, message_id = %message.id, error = %error, "message edit send failed");
                Ok(())
            }
        }
    }

    async fn fanout_message_delete(
        &self,
        message: &Message,
        protocol_message: ProtocolMessage,
    ) -> Result<()> {
        if let Some(group) = self.load_group(message.chat_id.clone()).await? {
            for peer_id in other_group_member_peer_ids(&group, &self.local_peer.device_id) {
                match self.send_to_peer(&peer_id, protocol_message.clone()).await {
                    Ok(()) | Err(MessagingError::PeerNotConnected(_)) => {}
                    Err(error) => {
                        warn!(peer_id, message_id = %message.id, error = %error, "message delete send failed");
                    }
                }
            }

            return Ok(());
        }

        let peer_id = message.chat_id.0.to_string();
        match self.send_to_peer(&peer_id, protocol_message).await {
            Ok(()) | Err(MessagingError::PeerNotConnected(_)) => Ok(()),
            Err(error) => {
                warn!(peer_id, message_id = %message.id, error = %error, "message delete send failed");
                Ok(())
            }
        }
    }

    fn emit_message_edited(&self, message_id: Uuid, new_content: String, edit_version: u32) {
        let _ = self.events.send(ChatServiceEvent::MessageEdited {
            message_id,
            new_content,
            edit_version,
        });
    }

    fn emit_message_deleted(&self, message_id: Uuid) {
        let _ = self
            .events
            .send(ChatServiceEvent::MessageDeleted { message_id });
    }

    fn emit_mention_received(
        &self,
        message_id: Uuid,
        mentioned_user_id: String,
        sender_name: String,
    ) {
        let _ = self.events.send(ChatServiceEvent::MentionReceived {
            message_id,
            mentioned_user_id,
            sender_name,
        });
    }

    fn emit_group_created(&self, group_id: ChatId) {
        let _ = self
            .events
            .send(ChatServiceEvent::GroupCreated { group_id });
    }

    fn emit_group_updated(&self, group_id: ChatId) {
        let _ = self
            .events
            .send(ChatServiceEvent::GroupUpdated { group_id });
    }

    fn emit_group_members_added(&self, group_id: &ChatId, member_ids: Vec<String>) {
        if member_ids.is_empty() {
            return;
        }
        let _ = self.events.send(ChatServiceEvent::GroupMembersAdded {
            group_id: group_id.clone(),
            member_ids,
        });
    }

    fn emit_group_members_removed(&self, group_id: &ChatId, member_ids: Vec<String>) {
        if member_ids.is_empty() {
            return;
        }
        let _ = self.events.send(ChatServiceEvent::GroupMembersRemoved {
            group_id: group_id.clone(),
            member_ids,
        });
    }

    fn emit_group_state_change(&self, previous: Option<&GroupState>, current: &GroupState) {
        let Some(previous) = previous else {
            self.emit_group_created(current.id.clone());
            return;
        };

        let added = added_members(&previous.member_ids, &current.member_ids);
        let removed = removed_members(&previous.member_ids, &current.member_ids);
        self.emit_group_members_added(&current.id, added);
        self.emit_group_members_removed(&current.id, removed);
        if previous.name != current.name || previous.member_ids != current.member_ids {
            self.emit_group_updated(current.id.clone());
        }
    }

    fn peer_display_name(&self, peer_id: &str) -> String {
        if let Some(display_name) = self
            .clients
            .lock()
            .expect("lock chat clients")
            .get(peer_id)
            .and_then(|client| client.remote_peer())
            .map(|peer| peer.identity.display_name)
        {
            return display_name;
        }

        self.server
            .connected_peers()
            .into_iter()
            .find(|peer| peer.identity.device_id == peer_id)
            .map(|peer| peer.identity.display_name)
            .unwrap_or_else(|| peer_id.to_string())
    }

    fn sender_key_distribution_material(&self, peer_id: &str) -> Result<[u8; 32]> {
        if let Some(material) = self
            .clients
            .lock()
            .expect("lock chat clients")
            .get(peer_id)
            .map(|client| client.sender_key_distribution_material())
        {
            return Ok(material);
        }

        self.server.sender_key_distribution_material(peer_id)
    }

    fn next_outgoing_direct_vector_clock(&self, peer_id: &str) -> HashMap<String, u64> {
        self.direct_message_vector_clocks
            .lock()
            .expect("lock direct message vector clocks")
            .entry(peer_id.to_string())
            .or_default()
            .increment(&self.local_peer.device_id)
    }

    fn merge_incoming_direct_vector_clock(
        &self,
        peer_id: &str,
        incoming_vector_clock: Option<&HashMap<String, u64>>,
    ) -> Option<VectorClockRelation> {
        let incoming_vector_clock = incoming_vector_clock?;
        let mut direct_message_vector_clocks = self
            .direct_message_vector_clocks
            .lock()
            .expect("lock direct message vector clocks");
        let local_vector_clock = direct_message_vector_clocks
            .entry(peer_id.to_string())
            .or_default();
        let relation = local_vector_clock.compare(incoming_vector_clock);
        local_vector_clock.merge(incoming_vector_clock);
        Some(relation)
    }

    async fn ensure_sender_key_distributed(
        &self,
        group: &GroupState,
        recipients: &[String],
    ) -> Result<SenderKeyInfo> {
        let group_id = group.id.0.to_string();
        let local_sender_id = self.local_peer.device_id.clone();
        if let Some(existing) = self.load_sender_key(&group_id, &local_sender_id).await? {
            return Ok(existing);
        }

        let sender_key = generate_sender_key();
        self.save_sender_key(&group_id, &local_sender_id, &sender_key)
            .await?;
        let stored_sender_key = self
            .load_sender_key(&group_id, &local_sender_id)
            .await?
            .ok_or_else(|| {
                MessagingError::Storage(format!(
                    "sender key for group {group_id} and sender {local_sender_id} was not persisted"
                ))
            })?;

        self.distribute_sender_key(
            &group.id.0.to_string(),
            &local_sender_id,
            &sender_key,
            recipients,
        )
        .await;

        Ok(stored_sender_key)
    }

    async fn rotate_sender_key_on_membership_reduction(
        &self,
        previous: &GroupState,
        updated: &GroupState,
    ) -> Result<()> {
        if updated.member_ids.len() >= previous.member_ids.len() {
            return Ok(());
        }

        let local_user_id = user_id_from_peer_id(&self.local_peer.device_id)?;
        if !updated.member_ids.contains(&local_user_id) {
            return Ok(());
        }

        self.rotate_local_sender_key(updated, false).await
    }

    async fn rotate_sender_key_after_group_member_left(&self, group: &GroupState) -> Result<()> {
        let local_user_id = user_id_from_peer_id(&self.local_peer.device_id)?;
        if !group.member_ids.contains(&local_user_id) {
            return Ok(());
        }

        self.rotate_local_sender_key(group, true).await
    }

    async fn rotate_local_sender_key(
        &self,
        group: &GroupState,
        create_if_missing: bool,
    ) -> Result<()> {
        let local_sender_id = self.local_peer.device_id.clone();
        let existing = self
            .load_sender_key(&group.id.0.to_string(), &local_sender_id)
            .await?;
        if existing.is_none() && !create_if_missing {
            return Ok(());
        }

        let next_epoch = existing
            .as_ref()
            .map(|sender_key| sender_key.epoch.saturating_add(1))
            .unwrap_or(0);

        let mut rotated = generate_sender_key();
        rotated.epoch = next_epoch;
        self.save_sender_key(&group.id.0.to_string(), &local_sender_id, &rotated)
            .await?;
        self.distribute_sender_key(
            &group.id.0.to_string(),
            &local_sender_id,
            &rotated,
            &other_group_member_peer_ids(group, &self.local_peer.device_id),
        )
        .await;

        Ok(())
    }

    async fn fanout_group_member_left(
        &self,
        group_id: &ChatId,
        member_id: &str,
        peer_ids: Vec<String>,
    ) {
        for peer_id in peer_ids {
            let message = ProtocolMessage::GroupMemberLeft {
                group_id: group_id.0.to_string(),
                member_id: member_id.to_string(),
            };

            match self.send_to_peer(&peer_id, message).await {
                Ok(()) | Err(MessagingError::PeerNotConnected(_)) => {}
                Err(error) => {
                    warn!(peer_id, group_id = %group_id.0, error = %error, "group member left send failed");
                }
            }
        }
    }

    async fn distribute_sender_key(
        &self,
        group_id: &str,
        sender_id: &str,
        sender_key: &SenderKey,
        recipients: &[String],
    ) {
        for peer_id in recipients {
            let session_key = match self.sender_key_distribution_material(peer_id) {
                Ok(material) => material,
                Err(MessagingError::PeerNotConnected(_)) => continue,
                Err(error) => {
                    warn!(peer_id, group_id, error = %error, "sender key distribution session unavailable");
                    continue;
                }
            };

            let distribution = create_distribution_message(
                group_id.to_string(),
                sender_id.to_string(),
                sender_key,
                &session_key,
            )
            .map_err(|error| MessagingError::Validation(error.to_string()));
            let Ok(distribution) = distribution else {
                warn!(peer_id, group_id, "sender key distribution creation failed");
                continue;
            };
            let message = match protocol_sender_key_distribution_message(&distribution) {
                Ok(message) => message,
                Err(error) => {
                    warn!(peer_id, group_id, error = %error, "sender key distribution encoding failed");
                    continue;
                }
            };

            match self.send_to_peer(peer_id, message).await {
                Ok(()) | Err(MessagingError::PeerNotConnected(_)) => {}
                Err(error) => {
                    warn!(peer_id, group_id, error = %error, "sender key distribution send failed");
                }
            }
        }
    }

    fn encrypt_group_message(
        &self,
        message: &Message,
        sender_key: &SenderKeyInfo,
    ) -> Result<ProtocolMessage> {
        let sender_key_id = sender_key.key_id;
        let counter_key = sender_key_counter_key(&message.chat_id.0.to_string(), sender_key);
        let counter = {
            let mut counters = self
                .group_sender_key_counters
                .lock()
                .expect("lock group sender key counters");
            let counter = counters.entry(counter_key).or_insert(0);
            let value = *counter;
            *counter = counter.saturating_add(1);
            value
        };
        let nonce = chunk_nonce(&[0u8; 8], counter);
        let key_material = sender_key_material_array(sender_key)?;
        let payload = serde_json::to_vec(&GroupMessageCiphertextPayload {
            id: message.id.to_string(),
            content: message.content.clone(),
            timestamp: message.timestamp_ms,
            reply_to_id: message.reply_to_id.clone(),
            reply_to_preview: message.reply_to_preview.clone(),
        })
        .map_err(|error| MessagingError::Protocol(error.to_string()))?;
        let encrypted_content = encrypt(
            &key_material,
            &nonce,
            &payload,
            &group_message_aad(
                &message.chat_id.0.to_string(),
                &self.local_peer.device_id,
                &sender_key_id,
                sender_key.epoch,
            ),
        )
        .map_err(|error| MessagingError::Validation(error.to_string()))?;

        Ok(ProtocolMessage::GroupMessage {
            group_id: message.chat_id.0.to_string(),
            sender_key_id,
            epoch: sender_key.epoch,
            nonce,
            encrypted_content,
        })
    }

    fn buffer_pending_edit(&self, message_id: String, edit: PendingEdit) {
        let mut pending_edits = self
            .pending_edits
            .lock()
            .expect("lock pending message edits");
        prune_expired_pending_edits(&mut pending_edits, now_ms_u64());
        pending_edits.entry(message_id).or_default().push(edit);
    }

    fn take_pending_edits(&self, message_id: &str) -> Vec<PendingEdit> {
        let mut pending_edits = self
            .pending_edits
            .lock()
            .expect("lock pending message edits");
        prune_expired_pending_edits(&mut pending_edits, now_ms_u64());
        pending_edits.remove(message_id).unwrap_or_default()
    }

    fn buffer_pending_delete(&self, message_id: String, delete: PendingDelete) {
        let mut pending_deletes = self
            .pending_deletes
            .lock()
            .expect("lock pending message deletes");
        prune_expired_pending_deletes(&mut pending_deletes, now_ms_u64());

        match pending_deletes.get_mut(&message_id) {
            Some(existing) if existing.timestamp_ms <= delete.timestamp_ms => {}
            Some(existing) => *existing = delete,
            None => {
                pending_deletes.insert(message_id, delete);
            }
        }
    }

    fn take_pending_delete(&self, message_id: &str) -> Option<PendingDelete> {
        let mut pending_deletes = self
            .pending_deletes
            .lock()
            .expect("lock pending message deletes");
        prune_expired_pending_deletes(&mut pending_deletes, now_ms_u64());
        pending_deletes.remove(message_id)
    }

    async fn apply_buffered_message_edits(
        &self,
        message_id: &str,
        delete_timestamp_ms: Option<u64>,
    ) {
        for edit in self
            .take_pending_edits(message_id)
            .into_iter()
            .filter(|edit| {
                delete_timestamp_ms
                    .map(|delete_timestamp_ms| edit.timestamp_ms < delete_timestamp_ms)
                    .unwrap_or(true)
            })
        {
            if let Err(error) = self
                .handle_incoming_message_edit(
                    edit.sender_id.clone(),
                    edit.into_incoming(message_id.to_string()),
                )
                .await
            {
                warn!(message_id, error = %error, "applying buffered message edit failed");
            }
        }
    }

    async fn save_message(&self, message: Message) -> Result<()> {
        self.save_message_with_vector_clock(message, None).await
    }

    async fn save_message_with_vector_clock(
        &self,
        message: Message,
        vector_clock: Option<HashMap<String, u64>>,
    ) -> Result<()> {
        let storage = Arc::clone(&self.storage);

        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| MessagingError::Storage(error.to_string()))?;

            runtime
                .block_on(async move {
                    storage
                        .save_message_with_vector_clock(&message, vector_clock.as_ref())
                        .await
                })
                .map_err(MessagingError::from)
        })
        .await
        .map_err(|error| MessagingError::Storage(error.to_string()))?
    }

    async fn save_group(&self, group: GroupState) -> Result<()> {
        let storage = Arc::clone(&self.storage);
        let group_info = group.info();
        let created_at_ms = group.created_at_ms;

        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| MessagingError::Storage(error.to_string()))?;

            runtime
                .block_on(async move { storage.save_group(&group_info, created_at_ms).await })
                .map_err(MessagingError::from)
        })
        .await
        .map_err(|error| MessagingError::Storage(error.to_string()))?
    }

    async fn save_sender_key(
        &self,
        group_id: &str,
        sender_device_id: &str,
        sender_key: &SenderKey,
    ) -> Result<()> {
        let storage = Arc::clone(&self.storage);
        let group_id = group_id.to_string();
        let sender_device_id = sender_device_id.to_string();
        let key_id = sender_key.key_id;
        let key_material = sender_key.key_material.to_vec();
        let epoch = sender_key.epoch;
        let created_at_ms = u64::try_from(sender_key.created_at).map_err(|_| {
            MessagingError::Validation(format!(
                "sender key created_at must be non-negative, got {}",
                sender_key.created_at
            ))
        })?;

        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| MessagingError::Storage(error.to_string()))?;

            runtime
                .block_on(async move {
                    storage
                        .save_sender_key(
                            &group_id,
                            &sender_device_id,
                            &key_id,
                            &key_material,
                            epoch,
                            created_at_ms,
                        )
                        .await
                })
                .map_err(MessagingError::from)
        })
        .await
        .map_err(|error| MessagingError::Storage(error.to_string()))?
    }

    async fn load_sender_key(
        &self,
        group_id: &str,
        sender_device_id: &str,
    ) -> Result<Option<SenderKeyInfo>> {
        let storage = Arc::clone(&self.storage);
        let group_id = group_id.to_string();
        let sender_device_id = sender_device_id.to_string();

        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| MessagingError::Storage(error.to_string()))?;

            runtime
                .block_on(async move { storage.get_sender_key(&group_id, &sender_device_id).await })
                .map_err(MessagingError::from)
        })
        .await
        .map_err(|error| MessagingError::Storage(error.to_string()))?
    }

    async fn load_sender_key_by_epoch(
        &self,
        group_id: &str,
        sender_device_id: &str,
        epoch: u32,
    ) -> Result<Option<SenderKeyInfo>> {
        let storage = Arc::clone(&self.storage);
        let group_id = group_id.to_string();
        let sender_device_id = sender_device_id.to_string();

        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| MessagingError::Storage(error.to_string()))?;

            runtime
                .block_on(async move {
                    storage
                        .get_sender_key_by_epoch(&group_id, &sender_device_id, epoch)
                        .await
                })
                .map_err(MessagingError::from)
        })
        .await
        .map_err(|error| MessagingError::Storage(error.to_string()))?
    }

    async fn load_group(&self, group_id: ChatId) -> Result<Option<GroupState>> {
        let storage = Arc::clone(&self.storage);

        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| MessagingError::Storage(error.to_string()))?;

            runtime
                .block_on(async move { storage.get_group(&group_id).await })
                .map(|maybe_group| {
                    maybe_group.map(|(group_info, created_at_ms)| {
                        GroupState::from_info(group_info, created_at_ms)
                    })
                })
                .map_err(MessagingError::from)
        })
        .await
        .map_err(|error| MessagingError::Storage(error.to_string()))?
    }

    async fn load_messages(
        &self,
        chat_id: ChatId,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Message>> {
        let storage = Arc::clone(&self.storage);

        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| MessagingError::Storage(error.to_string()))?;

            runtime
                .block_on(async move { storage.get_messages(&chat_id, limit, offset).await })
                .map_err(MessagingError::from)
        })
        .await
        .map_err(|error| MessagingError::Storage(error.to_string()))?
    }

    async fn load_message(&self, message_id: String) -> Result<Option<Message>> {
        let storage = Arc::clone(&self.storage);

        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| MessagingError::Storage(error.to_string()))?;

            runtime
                .block_on(async move { storage.get_message(&message_id).await })
                .map_err(MessagingError::from)
        })
        .await
        .map_err(|error| MessagingError::Storage(error.to_string()))?
    }

    async fn load_reply_preview(
        &self,
        chat_id: &ChatId,
        reply_to_id: Option<&str>,
    ) -> Result<Option<String>> {
        let Some(reply_to_id) = reply_to_id else {
            return Ok(None);
        };

        Ok(self
            .load_message(reply_to_id.to_string())
            .await?
            .filter(|message| message.chat_id == *chat_id)
            .map(|message| snapshot_reply_preview(&message.content)))
    }

    async fn update_message_status(&self, message_id: Uuid, status: MessageStatus) -> Result<()> {
        let storage = Arc::clone(&self.storage);

        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| MessagingError::Storage(error.to_string()))?;

            runtime
                .block_on(async move { storage.update_message_status(&message_id, status).await })
                .map_err(MessagingError::from)
        })
        .await
        .map_err(|error| MessagingError::Storage(error.to_string()))?
    }

    async fn update_message_content(
        &self,
        message_id: String,
        new_content: String,
        edit_version: u32,
        edited_at_ms: u64,
    ) -> Result<()> {
        let storage = Arc::clone(&self.storage);

        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| MessagingError::Storage(error.to_string()))?;

            runtime
                .block_on(async move {
                    storage
                        .update_message_content(
                            &message_id,
                            &new_content,
                            edit_version,
                            edited_at_ms,
                        )
                        .await
                })
                .map_err(MessagingError::from)
        })
        .await
        .map_err(|error| MessagingError::Storage(error.to_string()))?
    }

    async fn mark_message_deleted(&self, message_id: String, deleted_at_ms: u64) -> Result<()> {
        let storage = Arc::clone(&self.storage);

        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| MessagingError::Storage(error.to_string()))?;

            runtime
                .block_on(async move {
                    storage
                        .mark_message_deleted(&message_id, deleted_at_ms)
                        .await
                })
                .map_err(MessagingError::from)
        })
        .await
        .map_err(|error| MessagingError::Storage(error.to_string()))?
    }
}

fn protocol_sender_key_distribution_message(
    distribution: &jasmine_crypto::SenderKeyDistribution,
) -> Result<ProtocolMessage> {
    let payload = EncodedSenderKeyDistributionPayload {
        key_id: distribution.key_id.to_string(),
        encrypted_key_material: distribution.encrypted_key_material.clone(),
    };
    let sender_key_data = serde_json::to_string(&payload)
        .map_err(|error| MessagingError::Protocol(error.to_string()))?;

    Ok(ProtocolMessage::SenderKeyDistribution {
        group_id: distribution.group_id.clone(),
        sender_key_data,
        epoch: distribution.epoch,
    })
}

fn parse_protocol_sender_key_distribution(
    sender_id: &str,
    group_id: String,
    sender_key_data: String,
    epoch: u32,
) -> Result<jasmine_crypto::SenderKeyDistribution> {
    let payload: EncodedSenderKeyDistributionPayload = serde_json::from_str(&sender_key_data)
        .map_err(|error| {
            MessagingError::Validation(format!("invalid sender key distribution payload: {error}"))
        })?;

    Ok(jasmine_crypto::SenderKeyDistribution {
        group_id,
        sender_id: sender_id.to_string(),
        key_id: parse_uuid(&payload.key_id, "sender_key_distribution.key_id")?,
        epoch,
        encrypted_key_material: payload.encrypted_key_material,
    })
}

fn parse_uuid(raw: &str, field: &str) -> Result<Uuid> {
    Uuid::parse_str(raw).map_err(|error| {
        MessagingError::Validation(format!("{field} must be a valid UUID: {error}"))
    })
}

fn sender_key_counter_key(group_id: &str, sender_key: &SenderKeyInfo) -> String {
    format!("{group_id}:{}:{}", sender_key.key_id, sender_key.epoch,)
}

fn sender_key_material_array(sender_key: &SenderKeyInfo) -> Result<[u8; 32]> {
    let actual = sender_key.key_data.len();
    sender_key.key_data.as_slice().try_into().map_err(|_| {
        MessagingError::Validation(format!(
            "sender key {} material must be 32 bytes, got {actual}",
            sender_key.key_id
        ))
    })
}

fn sender_key_from_info(sender_key: &SenderKeyInfo) -> Result<SenderKey> {
    Ok(SenderKey {
        key_id: sender_key.key_id,
        epoch: sender_key.epoch,
        key_material: sender_key_material_array(sender_key)?,
        created_at: i64::try_from(sender_key.created_at).map_err(|_| {
            MessagingError::Validation(format!(
                "sender key {} created_at does not fit i64: {}",
                sender_key.key_id, sender_key.created_at
            ))
        })?,
    })
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

fn user_id_from_peer_id(peer_id: &str) -> Result<UserId> {
    Ok(UserId(parse_uuid(peer_id, "peer_id")?))
}

fn normalize_group_name(name: String) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(MessagingError::Validation(
            "group name must not be empty".to_string(),
        ));
    }

    Ok(trimmed.to_string())
}

fn build_group_members(local_peer_id: &str, member_ids: Vec<String>) -> Result<Vec<UserId>> {
    let mut all_member_ids = Vec::with_capacity(member_ids.len() + 1);
    all_member_ids.push(local_peer_id.to_string());
    all_member_ids.extend(member_ids);
    parse_member_ids(all_member_ids)
}

fn parse_member_ids(member_ids: Vec<String>) -> Result<Vec<UserId>> {
    let mut seen = HashSet::new();
    let mut parsed = Vec::new();

    for member_id in member_ids {
        let user_id = user_id_from_peer_id(&member_id)?;
        let dedupe_key = user_id.0.to_string();
        if seen.insert(dedupe_key) {
            parsed.push(user_id);
        }
    }

    Ok(parsed)
}

fn add_unique_members(existing: &[UserId], member_ids: Vec<String>) -> Result<Vec<UserId>> {
    let mut seen: HashSet<String> = existing
        .iter()
        .map(|member_id| member_id.0.to_string())
        .collect();
    let mut added = Vec::new();

    for member_id in member_ids {
        let user_id = user_id_from_peer_id(&member_id)?;
        let dedupe_key = user_id.0.to_string();
        if seen.insert(dedupe_key) {
            added.push(user_id);
        }
    }

    Ok(added)
}

fn remove_members(existing: &[UserId], member_ids: Vec<String>) -> Result<Vec<UserId>> {
    let removed: HashSet<String> = parse_member_ids(member_ids)?
        .into_iter()
        .map(|member_id| member_id.0.to_string())
        .collect();

    Ok(existing
        .iter()
        .filter(|member_id| !removed.contains(&member_id.0.to_string()))
        .cloned()
        .collect())
}

fn added_members(previous: &[UserId], current: &[UserId]) -> Vec<String> {
    let previous: HashSet<Uuid> = previous.iter().map(|member_id| member_id.0).collect();
    current
        .iter()
        .filter(|member_id| !previous.contains(&member_id.0))
        .map(|member_id| member_id.0.to_string())
        .collect()
}

fn removed_members(previous: &[UserId], current: &[UserId]) -> Vec<String> {
    let current: HashSet<Uuid> = current.iter().map(|member_id| member_id.0).collect();
    previous
        .iter()
        .filter(|member_id| !current.contains(&member_id.0))
        .map(|member_id| member_id.0.to_string())
        .collect()
}

fn ensure_group_contains_peer(group: &GroupState, peer_id: &str) -> Result<()> {
    let peer_user_id = user_id_from_peer_id(peer_id)?;
    if group.member_ids.contains(&peer_user_id) {
        Ok(())
    } else {
        Err(MessagingError::Validation(format!(
            "peer {peer_id} is not a member of group {}",
            group.id.0
        )))
    }
}

fn user_ids_to_peer_ids(member_ids: &[UserId]) -> Vec<String> {
    member_ids
        .iter()
        .map(|member_id| member_id.0.to_string())
        .collect()
}

fn other_group_member_peer_ids(group: &GroupState, local_peer_id: &str) -> Vec<String> {
    user_ids_to_peer_ids(&group.member_ids)
        .into_iter()
        .filter(|peer_id| peer_id != local_peer_id)
        .collect()
}

fn incoming_message_matches_message_chat(
    message: &Message,
    peer_id: &str,
    local_peer_id: &str,
    incoming_chat_id: &str,
) -> bool {
    let stored_chat_id = message.chat_id.0.to_string();

    stored_chat_id == incoming_chat_id
        || (stored_chat_id == peer_id && incoming_chat_id == local_peer_id)
}

fn should_apply_message_edit(message: &Message, edit_version: u32, timestamp_ms: u64) -> bool {
    if edit_version > message.edit_version {
        return true;
    }

    if edit_version < message.edit_version {
        return false;
    }

    matches!(message.edited_at, Some(current_timestamp_ms) if timestamp_ms > current_timestamp_ms)
}

fn compare_vector_clocks(
    left: &HashMap<String, u64>,
    right: &HashMap<String, u64>,
) -> VectorClockRelation {
    let mut left_is_less = false;
    let mut left_is_greater = false;

    for actor_id in left.keys().chain(right.keys()) {
        let left_counter = left.get(actor_id).copied().unwrap_or(0);
        let right_counter = right.get(actor_id).copied().unwrap_or(0);

        if left_counter < right_counter {
            left_is_less = true;
        } else if left_counter > right_counter {
            left_is_greater = true;
        }

        if left_is_less && left_is_greater {
            return VectorClockRelation::Concurrent;
        }
    }

    match (left_is_less, left_is_greater) {
        (true, false) => VectorClockRelation::Less,
        (false, true) => VectorClockRelation::Greater,
        (false, false) => VectorClockRelation::Equal,
        (true, true) => VectorClockRelation::Concurrent,
    }
}

fn local_mention_user_id(content: &str, local_peer_id: &str) -> Option<String> {
    parse_mentions(content)
        .into_iter()
        .find(|mention| mention.user_id == local_peer_id)
        .map(|mention| mention.user_id)
}

fn snapshot_reply_preview(content: &str) -> String {
    let mut chars = content.chars();
    let preview: String = chars.by_ref().take(REPLY_TO_PREVIEW_CHAR_LIMIT).collect();

    if chars.next().is_some() {
        let mut truncated = preview;
        truncated.push('…');
        truncated
    } else {
        preview
    }
}

fn prune_expired_pending_edits(pending_edits: &mut HashMap<String, Vec<PendingEdit>>, now_ms: u64) {
    pending_edits.retain(|_, edits| {
        edits.retain(|edit| edit.expires_at_ms > now_ms);
        !edits.is_empty()
    });
}

fn prune_expired_pending_deletes(
    pending_deletes: &mut HashMap<String, PendingDelete>,
    now_ms: u64,
) {
    pending_deletes.retain(|_, delete| delete.expires_at_ms > now_ms);
}

fn now_ms() -> i64 {
    now_ms_u64() as i64
}

fn now_ms_u64() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::{
        compare_vector_clocks, prune_expired_pending_deletes, prune_expired_pending_edits,
        should_apply_message_edit, snapshot_reply_preview, PendingDelete, PendingEdit,
        VectorClockRelation,
    };
    use jasmine_core::{ChatId, Message, MessageStatus, UserId};
    use std::collections::HashMap;
    use uuid::Uuid;

    #[test]
    fn pending_edit_buffer_discards_expired_entries() {
        let message_id = Uuid::new_v4().to_string();
        let mut pending_edits = HashMap::from([(
            message_id,
            vec![
                PendingEdit {
                    chat_id: "chat-1".to_string(),
                    sender_id: "sender-1".to_string(),
                    new_content: "expired".to_string(),
                    edit_version: 1,
                    timestamp_ms: 10,
                    expires_at_ms: 99,
                },
                PendingEdit {
                    chat_id: "chat-1".to_string(),
                    sender_id: "sender-1".to_string(),
                    new_content: "fresh".to_string(),
                    edit_version: 2,
                    timestamp_ms: 20,
                    expires_at_ms: 101,
                },
            ],
        )]);

        prune_expired_pending_edits(&mut pending_edits, 100);

        let retained = pending_edits.values().next().expect("pending edit remains");
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].new_content, "fresh");
    }

    #[test]
    fn pending_delete_buffer_discards_expired_entries() {
        let retained_message_id = Uuid::new_v4().to_string();
        let expired_message_id = Uuid::new_v4().to_string();
        let mut pending_deletes = HashMap::from([
            (
                retained_message_id.clone(),
                PendingDelete {
                    chat_id: "chat-1".to_string(),
                    sender_id: "sender-1".to_string(),
                    timestamp_ms: 20,
                    expires_at_ms: 101,
                },
            ),
            (
                expired_message_id,
                PendingDelete {
                    chat_id: "chat-2".to_string(),
                    sender_id: "sender-2".to_string(),
                    timestamp_ms: 10,
                    expires_at_ms: 99,
                },
            ),
        ]);

        prune_expired_pending_deletes(&mut pending_deletes, 100);

        assert_eq!(pending_deletes.len(), 1);
        assert!(pending_deletes.contains_key(&retained_message_id));
    }

    #[test]
    fn same_version_requires_newer_timestamp() {
        let message = Message {
            id: Uuid::new_v4(),
            chat_id: ChatId(Uuid::new_v4()),
            sender_id: UserId(Uuid::new_v4()),
            content: "edited".to_string(),
            timestamp_ms: 1,
            status: MessageStatus::Delivered,
            edit_version: 2,
            edited_at: Some(200),
            is_deleted: false,
            deleted_at: None,
            reply_to_id: None,
            reply_to_preview: None,
        };

        assert!(!should_apply_message_edit(&message, 1, 300));
        assert!(!should_apply_message_edit(&message, 2, 200));
        assert!(should_apply_message_edit(&message, 2, 201));
        assert!(should_apply_message_edit(&message, 3, 100));
    }

    #[test]
    fn reply_preview_snapshot_truncates_after_one_hundred_characters() {
        let short = "short preview";
        let long = "0123456789".repeat(11);

        assert_eq!(snapshot_reply_preview(short), short);
        assert_eq!(
            snapshot_reply_preview(&long),
            format!("{}…", "0123456789".repeat(10))
        );
    }

    #[test]
    fn vector_clock_concurrent_compare_distinguishes_equal_less_greater_and_concurrent() {
        let equal_left = HashMap::from([("alice".to_string(), 2), ("bob".to_string(), 1)]);
        let equal_right = HashMap::from([("alice".to_string(), 2), ("bob".to_string(), 1)]);
        let less = HashMap::from([("alice".to_string(), 1), ("bob".to_string(), 1)]);
        let greater = HashMap::from([("alice".to_string(), 3), ("bob".to_string(), 1)]);
        let concurrent = HashMap::from([("alice".to_string(), 3), ("bob".to_string(), 0)]);

        assert_eq!(
            compare_vector_clocks(&equal_left, &equal_right),
            VectorClockRelation::Equal
        );
        assert_eq!(
            compare_vector_clocks(&less, &equal_right),
            VectorClockRelation::Less
        );
        assert_eq!(
            compare_vector_clocks(&greater, &equal_right),
            VectorClockRelation::Greater
        );
        assert_eq!(
            compare_vector_clocks(&concurrent, &equal_right),
            VectorClockRelation::Concurrent
        );
    }
}
