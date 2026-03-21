use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use jasmine_core::{
    AppSettings, ChatId, DeviceId, DeviceIdentity, GroupInfo, Message, MessageStatus, PeerInfo,
    TransferRecord, TransferStatus, UserId,
};
use jasmine_transfer::{ManagedTransfer, TransferDirection, TransferProgress};
use tempfile::tempdir;
use uuid::Uuid;

use super::{
    accept_file_impl, accept_folder_transfer_impl, cancel_folder_transfer_impl,
    cancel_transfer_impl, create_group_impl, delete_message_impl, edit_message_impl,
    get_identity_impl, get_messages_impl, get_peers_impl, get_settings_impl, get_transfers_impl,
    reject_file_impl, reject_folder_transfer_impl, send_file_impl, send_folder_impl,
    send_group_message_impl, send_message_impl, setup_app_state_with_factory, start_discovery_impl,
    stop_discovery_impl, transfer_payload_from_managed, transfer_payload_from_record,
    update_avatar_impl, update_display_name_impl, update_settings_impl, AppSetupFactory, AppState,
    ChatMessagePayload, DiscoveryServiceHandle, FrontendEmitter, IdentityServiceHandle,
    MessagingServiceHandle, PeerPayload, SettingsServiceHandle, SetupContext, TransferPayload,
    TransferServiceHandle,
};

fn device_id(seed: u128) -> DeviceId {
    DeviceId(Uuid::from_u128(seed))
}

fn user_id(seed: u128) -> UserId {
    UserId(Uuid::from_u128(seed))
}

fn chat_id(seed: u128) -> ChatId {
    ChatId(Uuid::from_u128(seed))
}

fn peer(seed: u128, display_name: &str) -> PeerInfo {
    PeerInfo {
        device_id: device_id(seed),
        user_id: None,
        display_name: display_name.to_string(),
        ws_port: Some(9735),
        addresses: vec!["127.0.0.1".to_string()],
    }
}

fn message(
    id: u128,
    chat_seed: u128,
    sender_seed: u128,
    content: &str,
    status: MessageStatus,
) -> Message {
    Message {
        id: Uuid::from_u128(id),
        chat_id: chat_id(chat_seed),
        sender_id: user_id(sender_seed),
        content: content.to_string(),
        timestamp_ms: 1_234,
        status,
        edit_version: 0,
        edited_at: None,
        is_deleted: false,
        deleted_at: None,
        reply_to_id: None,
        reply_to_preview: None,
    }
}

fn identity() -> DeviceIdentity {
    DeviceIdentity {
        device_id: Uuid::from_u128(1).to_string(),
        display_name: "Local Device".to_string(),
        avatar_path: Some("/tmp/avatar.png".to_string()),
        created_at: 42,
    }
}

fn settings() -> AppSettings {
    AppSettings {
        download_dir: "/tmp/downloads".to_string(),
        max_concurrent_transfers: 3,
    }
}

fn transfer(id: &str, state: &str) -> TransferPayload {
    TransferPayload {
        id: id.to_string(),
        filename: format!("{id}.bin"),
        size: 2_048,
        progress: 0.5,
        speed: 512,
        state: state.to_string(),
        sender_id: Some(Uuid::from_u128(9).to_string()),
        local_path: Some(format!("/tmp/{id}.bin")),
        thumbnail_path: None,
        direction: None,
        folder_id: None,
        folder_relative_path: None,
    }
}

#[derive(Default)]
struct MockDiscoveryService {
    peers: Mutex<Vec<PeerInfo>>,
    start_calls: AtomicUsize,
    stop_calls: AtomicUsize,
    start_error: Mutex<Option<String>>,
}

impl MockDiscoveryService {
    fn with_peers(self, peers: Vec<PeerInfo>) -> Self {
        *self.peers.lock().expect("lock peers") = peers;
        self
    }

    fn with_start_error(self, error: &str) -> Self {
        *self.start_error.lock().expect("lock start error") = Some(error.to_string());
        self
    }
}

#[async_trait]
impl DiscoveryServiceHandle for MockDiscoveryService {
    async fn start(&self) -> Result<(), String> {
        self.start_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = self.start_error.lock().expect("lock start error").clone() {
            return Err(error);
        }
        Ok(())
    }

    async fn stop(&self) -> Result<(), String> {
        self.stop_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn peers(&self) -> Vec<PeerInfo> {
        self.peers.lock().expect("lock peers").clone()
    }
}

#[derive(Default)]
struct MockMessagingService {
    sent_messages: Mutex<Vec<(String, String)>>,
    group_creations: Mutex<Vec<(String, Vec<String>)>>,
    group_messages: Mutex<Vec<(String, String)>>,
    edit_calls: Mutex<Vec<(String, String, String)>>,
    delete_calls: Mutex<Vec<(String, String)>>,
    messages: Mutex<Vec<Message>>,
    send_error: Mutex<Option<String>>,
}

impl MockMessagingService {
    fn with_messages(self, messages: Vec<Message>) -> Self {
        *self.messages.lock().expect("lock messages") = messages;
        self
    }

    fn with_send_error(self, error: &str) -> Self {
        *self.send_error.lock().expect("lock send error") = Some(error.to_string());
        self
    }
}

#[async_trait]
impl MessagingServiceHandle for MockMessagingService {
    async fn send_message(
        &self,
        peer_id: &str,
        content: &str,
        _reply_to_id: Option<&str>,
    ) -> Result<Message, String> {
        self.sent_messages
            .lock()
            .expect("lock sent messages")
            .push((peer_id.to_string(), content.to_string()));

        if let Some(error) = self.send_error.lock().expect("lock send error").clone() {
            return Err(error);
        }

        Ok(message(11, 2, 1, content, MessageStatus::Sent))
    }

    async fn get_messages(
        &self,
        _chat_id: &str,
        _limit: u32,
        _offset: u32,
    ) -> Result<Vec<Message>, String> {
        Ok(self.messages.lock().expect("lock messages").clone())
    }

    async fn create_group(&self, name: &str, members: &[String]) -> Result<GroupInfo, String> {
        self.group_creations
            .lock()
            .expect("lock group creations")
            .push((name.to_string(), members.to_vec()));

        Ok(GroupInfo {
            id: chat_id(77),
            name: name.to_string(),
            member_ids: vec![user_id(1), user_id(2)],
        })
    }

    async fn send_group_message(
        &self,
        group_id: &str,
        content: &str,
        _reply_to_id: Option<&str>,
    ) -> Result<Message, String> {
        self.group_messages
            .lock()
            .expect("lock group messages")
            .push((group_id.to_string(), content.to_string()));

        Ok(message(12, 77, 1, content, MessageStatus::Delivered))
    }

    async fn edit_message(
        &self,
        message_id: &str,
        new_content: &str,
        sender_id: &str,
    ) -> Result<(), String> {
        self.edit_calls.lock().expect("lock edit calls").push((
            message_id.to_string(),
            new_content.to_string(),
            sender_id.to_string(),
        ));
        Ok(())
    }

    async fn delete_message(&self, message_id: &str, sender_id: &str) -> Result<(), String> {
        self.delete_calls
            .lock()
            .expect("lock delete calls")
            .push((message_id.to_string(), sender_id.to_string()));
        Ok(())
    }

    async fn shutdown(&self) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Default)]
struct MockTransferService {
    sent_files: Mutex<Vec<(String, PathBuf)>>,
    sent_folders: Mutex<Vec<(String, PathBuf)>>,
    accepted_offers: Mutex<Vec<String>>,
    accepted_folders: Mutex<Vec<(String, PathBuf)>>,
    rejected_offers: Mutex<Vec<(String, Option<String>)>>,
    rejected_folders: Mutex<Vec<String>>,
    cancelled_transfers: Mutex<Vec<String>>,
    cancelled_folders: Mutex<Vec<String>>,
    transfers: Mutex<Vec<TransferPayload>>,
    reject_error: Mutex<Option<String>>,
}

impl MockTransferService {
    fn with_transfers(self, transfers: Vec<TransferPayload>) -> Self {
        *self.transfers.lock().expect("lock transfers") = transfers;
        self
    }

    fn with_reject_error(self, error: &str) -> Self {
        *self.reject_error.lock().expect("lock reject error") = Some(error.to_string());
        self
    }
}

#[async_trait]
impl TransferServiceHandle for MockTransferService {
    async fn send_file(&self, peer_id: &str, file_path: &Path) -> Result<String, String> {
        self.sent_files
            .lock()
            .expect("lock sent files")
            .push((peer_id.to_string(), file_path.to_path_buf()));
        Ok("transfer-123".to_string())
    }

    async fn send_folder(&self, peer_id: &str, folder_path: &Path) -> Result<String, String> {
        self.sent_folders
            .lock()
            .expect("lock sent folders")
            .push((peer_id.to_string(), folder_path.to_path_buf()));
        Ok("folder-transfer-123".to_string())
    }

    async fn accept_file(&self, offer_id: &str) -> Result<String, String> {
        self.accepted_offers
            .lock()
            .expect("lock accepted offers")
            .push(offer_id.to_string());
        Ok(offer_id.to_string())
    }

    async fn reject_file(&self, offer_id: &str, reason: Option<String>) -> Result<(), String> {
        self.rejected_offers
            .lock()
            .expect("lock rejected offers")
            .push((offer_id.to_string(), reason.clone()));

        if let Some(error) = self.reject_error.lock().expect("lock reject error").clone() {
            return Err(error);
        }

        Ok(())
    }

    async fn cancel_transfer(&self, transfer_id: &str) -> Result<(), String> {
        self.cancelled_transfers
            .lock()
            .expect("lock cancelled transfers")
            .push(transfer_id.to_string());
        Ok(())
    }

    async fn accept_folder_transfer(
        &self,
        _folder_id: &str,
        _target_dir: &Path,
    ) -> Result<(), String> {
        self.accepted_folders
            .lock()
            .expect("lock accepted folders")
            .push((_folder_id.to_string(), _target_dir.to_path_buf()));
        Ok(())
    }

    async fn reject_folder_transfer(&self, _folder_id: &str) -> Result<(), String> {
        self.rejected_folders
            .lock()
            .expect("lock rejected folders")
            .push(_folder_id.to_string());
        Ok(())
    }

    async fn cancel_folder_transfer(&self, _folder_id: &str) -> Result<(), String> {
        self.cancelled_folders
            .lock()
            .expect("lock cancelled folders")
            .push(_folder_id.to_string());
        Ok(())
    }

    async fn get_transfers(&self) -> Result<Vec<TransferPayload>, String> {
        Ok(self.transfers.lock().expect("lock transfers").clone())
    }
}

#[derive(Default)]
struct MockIdentityService {
    update_names: Mutex<Vec<String>>,
    update_avatars: Mutex<Vec<String>>,
}

impl IdentityServiceHandle for MockIdentityService {
    fn get_identity(&self) -> Result<DeviceIdentity, String> {
        Ok(identity())
    }

    fn update_display_name(&self, name: &str) -> Result<DeviceIdentity, String> {
        self.update_names
            .lock()
            .expect("lock update names")
            .push(name.to_string());
        Ok(DeviceIdentity {
            display_name: name.to_string(),
            ..identity()
        })
    }

    fn update_avatar(&self, path: &str) -> Result<DeviceIdentity, String> {
        self.update_avatars
            .lock()
            .expect("lock update avatars")
            .push(path.to_string());
        Ok(DeviceIdentity {
            avatar_path: Some(path.to_string()),
            ..identity()
        })
    }
}

#[derive(Default)]
struct MockSettingsService {
    updated_settings: Mutex<Vec<AppSettings>>,
    update_error: Mutex<Option<String>>,
}

impl MockSettingsService {
    fn with_update_error(self, error: &str) -> Self {
        *self.update_error.lock().expect("lock update error") = Some(error.to_string());
        self
    }
}

#[async_trait]
impl SettingsServiceHandle for MockSettingsService {
    fn get_settings(&self) -> Result<AppSettings, String> {
        Ok(settings())
    }

    async fn update_settings(&self, settings: AppSettings) -> Result<AppSettings, String> {
        self.updated_settings
            .lock()
            .expect("lock updated settings")
            .push(settings.clone());

        if let Some(error) = self.update_error.lock().expect("lock update error").clone() {
            return Err(error);
        }

        Ok(settings)
    }
}

#[derive(Default)]
struct MockEmitter;

impl FrontendEmitter for MockEmitter {
    fn emit_json(&self, _event: &str, _payload: serde_json::Value) -> Result<(), String> {
        Ok(())
    }
}

fn app_state(
    discovery: Arc<dyn DiscoveryServiceHandle>,
    messaging: Arc<dyn MessagingServiceHandle>,
    transfers: Arc<dyn TransferServiceHandle>,
    identity_service: Arc<dyn IdentityServiceHandle>,
    settings_service: Arc<dyn SettingsServiceHandle>,
) -> AppState {
    AppState::new(
        identity().device_id,
        discovery,
        messaging,
        transfers,
        identity_service,
        settings_service,
    )
}

mod commands {
    use super::*;

    #[tokio::test]
    async fn start_discovery_dispatches_to_discovery_service() {
        let discovery = Arc::new(MockDiscoveryService::default());
        let state = app_state(
            discovery.clone(),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        start_discovery_impl(&state)
            .await
            .expect("start discovery command");

        assert_eq!(discovery.start_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stop_discovery_dispatches_to_discovery_service() {
        let discovery = Arc::new(MockDiscoveryService::default());
        let state = app_state(
            discovery.clone(),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        stop_discovery_impl(&state)
            .await
            .expect("stop discovery command");

        assert_eq!(discovery.stop_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn get_peers_returns_frontend_peer_shape() {
        let state = app_state(
            Arc::new(MockDiscoveryService::default().with_peers(vec![peer(2, "Alice")])),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let peers = get_peers_impl(&state).await.expect("get peers command");

        assert_eq!(
            peers,
            vec![PeerPayload {
                id: Uuid::from_u128(2).to_string(),
                name: "Alice".to_string(),
                status: "online".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn send_message_returns_message_id_and_passes_arguments() {
        let messaging = Arc::new(MockMessagingService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            messaging.clone(),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let message_id = send_message_impl(
            &state,
            Uuid::from_u128(2).to_string(),
            "hello".to_string(),
            None,
        )
        .await
        .expect("send message command");

        assert_eq!(message_id, Uuid::from_u128(11).to_string());
        assert_eq!(
            messaging
                .sent_messages
                .lock()
                .expect("lock sent messages")
                .clone(),
            vec![(Uuid::from_u128(2).to_string(), "hello".to_string())]
        );
    }

    #[tokio::test]
    async fn get_messages_maps_local_and_remote_messages() {
        let local_uuid = identity().device_id;
        let local_user_uuid = Uuid::parse_str(&local_uuid).expect("parse local uuid");
        let peer_uuid = Uuid::from_u128(2);
        let state = AppState::new(
            local_uuid,
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default().with_messages(vec![
                Message {
                    id: Uuid::from_u128(10),
                    chat_id: ChatId(peer_uuid),
                    sender_id: UserId(local_user_uuid),
                    content: "from local".to_string(),
                    timestamp_ms: 1_000,
                    status: MessageStatus::Sent,
                    edit_version: 0,
                    edited_at: None,
                    is_deleted: false,
                    deleted_at: None,
                    reply_to_id: None,
                    reply_to_preview: None,
                },
                Message {
                    id: Uuid::from_u128(11),
                    chat_id: ChatId(peer_uuid),
                    sender_id: UserId(peer_uuid),
                    content: "from peer".to_string(),
                    timestamp_ms: 2_000,
                    status: MessageStatus::Delivered,
                    edit_version: 0,
                    edited_at: None,
                    is_deleted: false,
                    deleted_at: None,
                    reply_to_id: None,
                    reply_to_preview: None,
                },
            ])),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let messages = get_messages_impl(&state, peer_uuid.to_string(), 20, 0)
            .await
            .expect("get messages command");

        assert_eq!(
            messages,
            vec![
                ChatMessagePayload {
                    id: Uuid::from_u128(10).to_string(),
                    sender_id: "local".to_string(),
                    receiver_id: peer_uuid.to_string(),
                    content: "from local".to_string(),
                    timestamp: 1_000,
                    status: "sent".to_string(),
                    edit_version: 0,
                    edited_at: None,
                    is_deleted: false,
                    deleted_at: None,
                    reply_to_id: None,
                    reply_to_preview: None,
                },
                ChatMessagePayload {
                    id: Uuid::from_u128(11).to_string(),
                    sender_id: peer_uuid.to_string(),
                    receiver_id: "local".to_string(),
                    content: "from peer".to_string(),
                    timestamp: 2_000,
                    status: "delivered".to_string(),
                    edit_version: 0,
                    edited_at: None,
                    is_deleted: false,
                    deleted_at: None,
                    reply_to_id: None,
                    reply_to_preview: None,
                },
            ]
        );
    }

    #[tokio::test]
    async fn create_group_returns_group_id() {
        let messaging = Arc::new(MockMessagingService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            messaging.clone(),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let group_id = create_group_impl(
            &state,
            "Project Room".to_string(),
            vec![Uuid::from_u128(2).to_string()],
        )
        .await
        .expect("create group command");

        assert_eq!(group_id, Uuid::from_u128(77).to_string());
        assert_eq!(
            messaging
                .group_creations
                .lock()
                .expect("lock group creations")
                .clone(),
            vec![(
                "Project Room".to_string(),
                vec![Uuid::from_u128(2).to_string()]
            )]
        );
    }

    #[tokio::test]
    async fn send_group_message_returns_message_id() {
        let messaging = Arc::new(MockMessagingService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            messaging.clone(),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let message_id = send_group_message_impl(
            &state,
            Uuid::from_u128(77).to_string(),
            "hello group".to_string(),
            None,
        )
        .await
        .expect("send group message command");

        assert_eq!(message_id, Uuid::from_u128(12).to_string());
        assert_eq!(
            messaging
                .group_messages
                .lock()
                .expect("lock group messages")
                .clone(),
            vec![(Uuid::from_u128(77).to_string(), "hello group".to_string())]
        );
    }

    #[tokio::test]
    async fn edit_message_dispatches_to_messaging_service() {
        let messaging = Arc::new(MockMessagingService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            messaging.clone(),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        edit_message_impl(&state, "msg-1".to_string(), "updated".to_string())
            .await
            .expect("edit message command");

        assert_eq!(
            messaging
                .edit_calls
                .lock()
                .expect("lock edit calls")
                .clone(),
            vec![(
                "msg-1".to_string(),
                "updated".to_string(),
                identity().device_id
            )]
        );
    }

    #[tokio::test]
    async fn delete_message_dispatches_to_messaging_service() {
        let messaging = Arc::new(MockMessagingService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            messaging.clone(),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        delete_message_impl(&state, "msg-1".to_string())
            .await
            .expect("delete message command");

        assert_eq!(
            messaging
                .delete_calls
                .lock()
                .expect("lock delete calls")
                .clone(),
            vec![("msg-1".to_string(), identity().device_id)]
        );
    }

    #[tokio::test]
    async fn send_file_returns_transfer_id() {
        let transfers = Arc::new(MockTransferService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            transfers.clone(),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let transfer_id = send_file_impl(
            &state,
            Uuid::from_u128(2).to_string(),
            "/tmp/file.txt".to_string(),
        )
        .await
        .expect("send file command");

        assert_eq!(transfer_id, "transfer-123");
        assert_eq!(
            transfers
                .sent_files
                .lock()
                .expect("lock sent files")
                .clone(),
            vec![(
                Uuid::from_u128(2).to_string(),
                PathBuf::from("/tmp/file.txt")
            )]
        );
    }

    #[tokio::test]
    async fn send_folder_returns_transfer_id() {
        let transfers = Arc::new(MockTransferService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            transfers.clone(),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let transfer_id = send_folder_impl(
            &state,
            Uuid::from_u128(2).to_string(),
            "/tmp/folder".to_string(),
        )
        .await
        .expect("send folder command");

        assert_eq!(transfer_id, "folder-transfer-123");
        assert_eq!(
            transfers
                .sent_folders
                .lock()
                .expect("lock sent folders")
                .clone(),
            vec![(Uuid::from_u128(2).to_string(), PathBuf::from("/tmp/folder"))]
        );
    }

    #[tokio::test]
    async fn accept_file_dispatches_to_transfer_service() {
        let transfers = Arc::new(MockTransferService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            transfers.clone(),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        accept_file_impl(&state, "offer-1".to_string())
            .await
            .expect("accept file command");

        assert_eq!(
            transfers
                .accepted_offers
                .lock()
                .expect("lock accepted offers")
                .clone(),
            vec!["offer-1".to_string()]
        );
    }

    #[tokio::test]
    async fn reject_file_passes_reason_through() {
        let transfers = Arc::new(MockTransferService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            transfers.clone(),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        reject_file_impl(&state, "offer-2".to_string(), Some("busy".to_string()))
            .await
            .expect("reject file command");

        assert_eq!(
            transfers
                .rejected_offers
                .lock()
                .expect("lock rejected offers")
                .clone(),
            vec![("offer-2".to_string(), Some("busy".to_string()))]
        );
    }

    #[tokio::test]
    async fn cancel_transfer_dispatches_to_transfer_service() {
        let transfers = Arc::new(MockTransferService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            transfers.clone(),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        cancel_transfer_impl(&state, "transfer-9".to_string())
            .await
            .expect("cancel transfer command");

        assert_eq!(
            transfers
                .cancelled_transfers
                .lock()
                .expect("lock cancelled transfers")
                .clone(),
            vec!["transfer-9".to_string()]
        );
    }

    #[tokio::test]
    async fn accept_folder_transfer_dispatches_to_transfer_service() {
        let transfers = Arc::new(MockTransferService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            transfers.clone(),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        accept_folder_transfer_impl(&state, "folder-1".to_string(), "/tmp/downloads".to_string())
            .await
            .expect("accept folder transfer command");

        assert_eq!(
            transfers
                .accepted_folders
                .lock()
                .expect("lock accepted folders")
                .clone(),
            vec![("folder-1".to_string(), PathBuf::from("/tmp/downloads"))]
        );
    }

    #[tokio::test]
    async fn reject_folder_transfer_dispatches_to_transfer_service() {
        let transfers = Arc::new(MockTransferService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            transfers.clone(),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        reject_folder_transfer_impl(&state, "folder-1".to_string())
            .await
            .expect("reject folder transfer command");

        assert_eq!(
            transfers
                .rejected_folders
                .lock()
                .expect("lock rejected folders")
                .clone(),
            vec!["folder-1".to_string()]
        );
    }

    #[tokio::test]
    async fn cancel_folder_transfer_dispatches_to_transfer_service() {
        let transfers = Arc::new(MockTransferService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            transfers.clone(),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        cancel_folder_transfer_impl(&state, "folder-1".to_string())
            .await
            .expect("cancel folder transfer command");

        assert_eq!(
            transfers
                .cancelled_folders
                .lock()
                .expect("lock cancelled folders")
                .clone(),
            vec!["folder-1".to_string()]
        );
    }

    #[tokio::test]
    async fn get_transfers_returns_payloads() {
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(
                MockTransferService::default()
                    .with_transfers(vec![transfer("transfer-1", "active")]),
            ),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let transfers = get_transfers_impl(&state)
            .await
            .expect("get transfers command");

        assert_eq!(transfers, vec![transfer("transfer-1", "active")]);
    }

    #[test]
    fn transfer_payload_from_managed_includes_folder_linkage_metadata() {
        let transfer = ManagedTransfer {
            id: "transfer-1".to_string(),
            peer_id: device_id(9),
            chat_id: None,
            file_name: "report.txt".to_string(),
            local_path: PathBuf::from("/tmp/report.txt"),
            direction: TransferDirection::Send,
            status: TransferStatus::Active,
            total_bytes: 200,
            progress: Some(TransferProgress {
                transfer_id: "transfer-1".to_string(),
                bytes_sent: 50,
                total_bytes: 200,
                speed_bps: 25,
            }),
            folder_id: Some("folder-1".to_string()),
            folder_relative_path: Some("docs/report.txt".to_string()),
        };

        let payload = transfer_payload_from_managed(&transfer);

        assert_eq!(payload.folder_id.as_deref(), Some("folder-1"));
        assert_eq!(
            payload.folder_relative_path.as_deref(),
            Some("docs/report.txt")
        );
        assert_eq!(payload.direction.as_deref(), Some("send"));
        assert_eq!(payload.progress, 0.25);
        assert_eq!(payload.speed, 25);
    }

    #[test]
    fn transfer_payload_from_record_includes_persisted_folder_linkage_metadata() {
        let temp = tempdir().expect("tempdir");
        let local_path = temp.path().join("report.txt");
        std::fs::write(&local_path, vec![0_u8; 64]).expect("write persisted transfer file");

        let record = TransferRecord {
            id: Uuid::from_u128(55),
            peer_id: device_id(9),
            chat_id: None,
            file_name: "report.txt".to_string(),
            local_path,
            status: TransferStatus::Completed,
            thumbnail_path: None,
            folder_id: Some("folder-1".to_string()),
            folder_relative_path: Some("docs/report.txt".to_string()),
        };

        let payload = transfer_payload_from_record(&record);

        assert_eq!(payload.folder_id.as_deref(), Some("folder-1"));
        assert_eq!(
            payload.folder_relative_path.as_deref(),
            Some("docs/report.txt")
        );
        assert_eq!(payload.progress, 1.0);
        assert_eq!(payload.size, 64);
    }

    #[test]
    fn get_identity_returns_device_identity() {
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        assert_eq!(
            get_identity_impl(&state).expect("get identity command"),
            identity()
        );
    }

    #[test]
    fn update_display_name_dispatches_to_identity_service() {
        let identities = Arc::new(MockIdentityService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockTransferService::default()),
            identities.clone(),
            Arc::new(MockSettingsService::default()),
        );

        update_display_name_impl(&state, "New Name".to_string())
            .expect("update display name command");

        assert_eq!(
            identities
                .update_names
                .lock()
                .expect("lock update names")
                .clone(),
            vec!["New Name".to_string()]
        );
    }

    #[test]
    fn update_avatar_dispatches_path_to_identity_service() {
        let identities = Arc::new(MockIdentityService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockTransferService::default()),
            identities.clone(),
            Arc::new(MockSettingsService::default()),
        );

        update_avatar_impl(&state, "/tmp/new-avatar.png".to_string())
            .expect("update avatar command");

        assert_eq!(
            identities
                .update_avatars
                .lock()
                .expect("lock update avatars")
                .clone(),
            vec!["/tmp/new-avatar.png".to_string()]
        );
    }

    #[test]
    fn get_settings_returns_current_settings() {
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        assert_eq!(
            get_settings_impl(&state).expect("get settings command"),
            settings()
        );
    }

    #[tokio::test]
    async fn update_settings_dispatches_and_returns_normalized_settings() {
        let settings_service = Arc::new(MockSettingsService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            settings_service.clone(),
        );
        let next = AppSettings {
            download_dir: "/tmp/other".to_string(),
            max_concurrent_transfers: 5,
        };

        let saved = update_settings_impl(&state, next.clone())
            .await
            .expect("update settings command");

        assert_eq!(saved, next);
        assert_eq!(
            settings_service
                .updated_settings
                .lock()
                .expect("lock updated settings")
                .clone(),
            vec![next]
        );
    }
}

mod commands_error {
    use super::*;

    #[tokio::test]
    async fn send_message_returns_serializable_error_instead_of_panicking() {
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default().with_send_error("peer offline")),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let error = send_message_impl(
            &state,
            Uuid::from_u128(2).to_string(),
            "hello".to_string(),
            None,
        )
        .await
        .expect_err("send message should fail");

        assert!(error.contains("peer offline"));
    }

    #[tokio::test]
    async fn reject_file_propagates_service_errors() {
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockTransferService::default().with_reject_error("unknown offer")),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let error = reject_file_impl(&state, "offer-404".to_string(), None)
            .await
            .expect_err("reject file should fail");

        assert!(error.contains("unknown offer"));
    }

    #[tokio::test]
    async fn update_settings_propagates_dependency_errors() {
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default().with_update_error("transfer manager failed")),
        );

        let error = update_settings_impl(&state, settings())
            .await
            .expect_err("update settings should fail");

        assert!(error.contains("transfer manager failed"));
    }

    #[tokio::test]
    async fn start_discovery_propagates_start_errors() {
        let state = app_state(
            Arc::new(MockDiscoveryService::default().with_start_error("mdns unavailable")),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let error = start_discovery_impl(&state)
            .await
            .expect_err("start discovery should fail");

        assert!(error.contains("mdns unavailable"));
    }
}

mod setup_init {
    use super::*;

    struct RecordingFactory {
        called: AtomicUsize,
        started_discovery: AtomicBool,
        last_context: Mutex<Option<SetupContext>>,
    }

    impl RecordingFactory {
        fn new() -> Self {
            Self {
                called: AtomicUsize::new(0),
                started_discovery: AtomicBool::new(false),
                last_context: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl AppSetupFactory for RecordingFactory {
        async fn build(
            &self,
            context: SetupContext,
            _emitter: Arc<dyn FrontendEmitter>,
        ) -> Result<AppState, String> {
            self.called.fetch_add(1, Ordering::SeqCst);
            *self.last_context.lock().expect("lock last context") = Some(context.clone());
            std::fs::create_dir_all(&context.app_data_dir).map_err(|error| error.to_string())?;
            std::fs::write(&context.database_path, b"").map_err(|error| error.to_string())?;

            let discovery = Arc::new(MockDiscoveryService::default());
            discovery.start().await?;
            self.started_discovery.store(true, Ordering::SeqCst);

            Ok(app_state(
                discovery,
                Arc::new(MockMessagingService::default()),
                Arc::new(MockTransferService::default()),
                Arc::new(MockIdentityService::default()),
                Arc::new(MockSettingsService::default()),
            ))
        }
    }

    #[tokio::test]
    async fn setup_init_creates_database_path_and_returns_state() {
        let dir = tempdir().expect("create temp dir");
        let factory = Arc::new(RecordingFactory::new());
        let state =
            setup_app_state_with_factory(dir.path(), Arc::new(MockEmitter), factory.clone())
                .await
                .expect("setup app state");

        let context = factory
            .last_context
            .lock()
            .expect("lock last context")
            .clone()
            .expect("setup context captured");

        assert_eq!(factory.called.load(Ordering::SeqCst), 1);
        assert_eq!(context.app_data_dir, dir.path().to_path_buf());
        assert_eq!(context.database_path, dir.path().join("jasmine.db"));
        assert!(context.database_path.exists());
        assert_eq!(state.local_device_id(), identity().device_id);
    }

    #[tokio::test]
    async fn setup_init_starts_discovery_during_factory_build() {
        let dir = tempdir().expect("create temp dir");
        let factory = Arc::new(RecordingFactory::new());

        setup_app_state_with_factory(dir.path(), Arc::new(MockEmitter), factory.clone())
            .await
            .expect("setup app state");

        assert!(factory.started_discovery.load(Ordering::SeqCst));
    }
}

mod chat_bridge {
    use super::*;
    use crate::commands::emit_chat_service_event;
    use jasmine_core::StorageEngine;
    use jasmine_messaging::ChatServiceEvent;
    use jasmine_storage::SqliteStorage;
    use serde_json::json;

    struct RecordingEmitter {
        events: Mutex<Vec<(String, serde_json::Value)>>,
    }

    impl RecordingEmitter {
        fn new() -> Self {
            Self {
                events: Mutex::new(Vec::new()),
            }
        }

        fn snapshot(&self) -> Vec<(String, serde_json::Value)> {
            self.events.lock().expect("lock emitted events").clone()
        }
    }

    impl FrontendEmitter for RecordingEmitter {
        fn emit_json(&self, event: &str, payload: serde_json::Value) -> Result<(), String> {
            self.events
                .lock()
                .expect("lock emitted events")
                .push((event.to_string(), payload));
            Ok(())
        }
    }

    #[tokio::test]
    async fn emit_chat_service_event_bridges_edit_delete_and_mention_payloads() {
        let dir = tempdir().expect("create temp dir");
        let storage =
            SqliteStorage::open(dir.path().join("commands.db")).expect("open sqlite storage");
        let emitter = RecordingEmitter::new();
        let mut stored_message = message(10, 2, 2, "before", MessageStatus::Delivered);
        stored_message.edit_version = 2;
        stored_message.edited_at = Some(7_654);
        storage
            .save_message(&stored_message)
            .await
            .expect("save bridge test message");

        emit_chat_service_event(
            ChatServiceEvent::MessageEdited {
                message_id: stored_message.id,
                new_content: "after".to_string(),
                edit_version: 2,
            },
            &storage,
            &emitter,
            &identity().device_id,
        )
        .await;
        emit_chat_service_event(
            ChatServiceEvent::MessageDeleted {
                message_id: stored_message.id,
            },
            &storage,
            &emitter,
            &identity().device_id,
        )
        .await;
        emit_chat_service_event(
            ChatServiceEvent::MentionReceived {
                message_id: stored_message.id,
                mentioned_user_id: "user-1".to_string(),
                sender_name: "Alice".to_string(),
            },
            &storage,
            &emitter,
            &identity().device_id,
        )
        .await;

        assert_eq!(
            emitter.snapshot(),
            vec![
                (
                    "message-edited".to_string(),
                    json!({
                        "messageId": stored_message.id.to_string(),
                        "newContent": "after",
                        "editVersion": 2,
                        "editedAt": 7654
                    })
                ),
                (
                    "message-deleted".to_string(),
                    json!({
                        "messageId": stored_message.id.to_string()
                    })
                ),
                (
                    "mention-received".to_string(),
                    json!({
                        "messageId": stored_message.id.to_string(),
                        "mentionedUserId": "user-1",
                        "senderName": "Alice"
                    })
                )
            ]
        );
    }
}
