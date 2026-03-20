use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jasmine_core::{
    AckStatus, ChatId, CoreError, GroupInfo, Message, MessageStatus, ProtocolMessage,
    StorageEngine, UserId,
};
use jasmine_storage::{ChatRecord, ChatType, SqliteStorage};
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;
use tokio::time;
use tracing::warn;
use uuid::Uuid;

use crate::{
    MessagingError, Result, WsClient, WsClientEvent, WsPeerIdentity, WsServer, WsServerEvent,
};

pub const DEFAULT_CHAT_ACK_TIMEOUT: Duration = Duration::from_secs(5);

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
        let chat_id = Self::direct_chat_id(peer_id)?;
        let sender_id = user_id_from_peer_id(&self.inner.local_peer.device_id)?;
        let mut message = Message {
            id: Uuid::new_v4(),
            chat_id,
            sender_id,
            content: content.into(),
            timestamp_ms: now_ms(),
            status: MessageStatus::Sent,
        };

        self.inner.save_message(message.clone()).await?;
        self.inner
            .emit_status(peer_id.to_string(), message.id, MessageStatus::Sent);

        let protocol_message = ProtocolMessage::TextMessage {
            id: message.id.to_string(),
            chat_id: peer_id.to_string(),
            sender_id: self.inner.local_peer.device_id.clone(),
            content: message.content.clone(),
            timestamp: message.timestamp_ms,
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
        let group = self
            .inner
            .load_group(group_id.clone())
            .await?
            .ok_or_else(|| MessagingError::Validation(format!("unknown group {}", group_id.0)))?;
        ensure_group_contains_peer(&group, &self.inner.local_peer.device_id)?;

        let recipients = other_group_member_peer_ids(&group, &self.inner.local_peer.device_id);
        let sender_id = user_id_from_peer_id(&self.inner.local_peer.device_id)?;
        let mut message = Message {
            id: Uuid::new_v4(),
            chat_id: group.id.clone(),
            sender_id,
            content: content.into(),
            timestamp_ms: now_ms(),
            status: MessageStatus::Sent,
        };

        self.inner.save_message(message.clone()).await?;

        let mut delivered_to_any = recipients.is_empty();
        for peer_id in recipients {
            let protocol_message = ProtocolMessage::TextMessage {
                id: message.id.to_string(),
                chat_id: group.id.0.to_string(),
                sender_id: self.inner.local_peer.device_id.clone(),
                content: message.content.clone(),
                timestamp: message.timestamp_ms,
            };

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

        self.inner.save_group(updated.clone()).await?;
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
        let stored_chat_id = if text.chat_id == self.local_peer.device_id {
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

        let stored_message = Message {
            id: message_id,
            chat_id: stored_chat_id,
            sender_id: sender_user_id,
            content: text.content,
            timestamp_ms: text.timestamp,
            status: MessageStatus::Delivered,
        };

        self.save_message(stored_message.clone()).await?;
        let _ = self.events.send(ChatServiceEvent::MessageReceived {
            peer_id: peer_id.clone(),
            message: stored_message,
        });

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
        let created_at_ms = self
            .load_group(group_id.clone())
            .await?
            .map(|group| group.created_at_ms)
            .unwrap_or_else(now_ms);
        self.save_group(GroupState {
            id: group_id,
            name: normalize_group_name(name)?,
            member_ids,
            created_at_ms,
        })
        .await
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

    async fn save_message(&self, message: Message) -> Result<()> {
        let storage = Arc::clone(&self.storage);

        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| MessagingError::Storage(error.to_string()))?;

            runtime
                .block_on(async move { storage.save_message(&message).await })
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
}

fn parse_uuid(raw: &str, field: &str) -> Result<Uuid> {
    Uuid::parse_str(raw).map_err(|error| {
        MessagingError::Validation(format!("{field} must be a valid UUID: {error}"))
    })
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

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
