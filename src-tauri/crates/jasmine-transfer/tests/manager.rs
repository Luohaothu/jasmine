use std::collections::HashMap;
use std::io::Cursor;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use image::{DynamicImage, ImageFormat, ImageReader, Rgba, RgbaImage};
use jasmine_core::{
    AppSettings, ChatId, CoreError, DeviceId, Message, MessageStatus, PeerInfo, PeerKeyInfo,
    ProtocolMessage, ResumeInfo, SenderKeyInfo, SettingsService, StorageEngine, TransferRecord,
    TransferStatus,
};
use jasmine_transfer::{
    FileOfferNotification, FileSenderError, FileSenderSignal, FolderTransferCoordinator,
    ManagedTransfer, ManagedTransferReceiver, ManagedTransferSender, TransferAggregateProgress,
    TransferDirection, TransferManager, TransferManagerError, TransferProgress,
    TransferProgressReporter, DEFAULT_CHUNK_SIZE, THUMBNAIL_MAX_EDGE,
};
use tempfile::tempdir;
use tokio::sync::{oneshot, Notify};
use tokio::time;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn device_id() -> DeviceId {
    DeviceId(Uuid::new_v4())
}

fn chat_id() -> ChatId {
    ChatId(Uuid::new_v4())
}

fn test_file_key() -> [u8; 32] {
    [0x31; 32]
}

fn test_nonce_prefix() -> [u8; 8] {
    [0x7a; 8]
}

fn test_resume_token() -> String {
    "mock-transfer-resume-token".to_string()
}

fn save_settings(settings: &SettingsService, max_concurrent_transfers: u8) {
    settings
        .save(&AppSettings {
            download_dir: "~/Downloads/Jasmine/".to_string(),
            max_concurrent_transfers,
        })
        .expect("save test settings");
}

fn write_file(path: &Path, size: usize) -> PathBuf {
    std::fs::write(path, vec![0x5A; size]).expect("write temp file");
    path.to_path_buf()
}

fn encode_image_bytes(format: ImageFormat, width: u32, height: u32) -> Vec<u8> {
    let image = RgbaImage::from_fn(width, height, |x, y| {
        Rgba([(x % 255) as u8, (y % 255) as u8, ((x + y) % 255) as u8, 255])
    });
    let image = DynamicImage::ImageRgba8(image);
    let mut cursor = Cursor::new(Vec::new());
    image
        .write_to(&mut cursor, format)
        .expect("encode test image");
    cursor.into_inner()
}

fn assert_webp_thumbnail(path: &Path) {
    let reader = ImageReader::open(path)
        .expect("open generated thumbnail")
        .with_guessed_format()
        .expect("guess thumbnail format");
    assert_eq!(reader.format(), Some(ImageFormat::WebP));

    let decoded = reader.decode().expect("decode generated thumbnail");
    assert!(decoded.width() <= THUMBNAIL_MAX_EDGE);
    assert!(decoded.height() <= THUMBNAIL_MAX_EDGE);
}

#[derive(Clone, Default)]
struct MockFolderSignal {
    sent_messages: Arc<Mutex<Vec<(DeviceId, ProtocolMessage)>>>,
    response_senders: Arc<Mutex<HashMap<String, oneshot::Sender<ProtocolMessage>>>>,
    response_receivers: Arc<Mutex<HashMap<String, oneshot::Receiver<ProtocolMessage>>>>,
    notify: Arc<Notify>,
}

impl MockFolderSignal {
    async fn wait_for_manifest(&self) -> (DeviceId, ProtocolMessage) {
        loop {
            if let Some(message) = self
                .sent_messages
                .lock()
                .expect("lock sent folder messages")
                .iter()
                .find(|(_, message)| matches!(message, ProtocolMessage::FolderManifest { .. }))
                .cloned()
            {
                return message;
            }

            self.notify.notified().await;
        }
    }

    fn respond(&self, response_id: &str, response: ProtocolMessage) {
        let sender = self
            .response_senders
            .lock()
            .expect("lock folder response senders")
            .remove(response_id)
            .expect("folder response sender for id");
        sender.send(response).expect("deliver folder response");
    }
}

impl FileSenderSignal for MockFolderSignal {
    async fn send_message(
        &self,
        peer_id: &DeviceId,
        message: ProtocolMessage,
    ) -> Result<(), FileSenderError> {
        if let Some(response_id) = match &message {
            ProtocolMessage::FolderManifest {
                folder_transfer_id, ..
            } => Some(folder_transfer_id.clone()),
            ProtocolMessage::FileOffer { id, .. } => Some(id.clone()),
            _ => None,
        } {
            let (tx, rx) = oneshot::channel();
            self.response_senders
                .lock()
                .expect("lock folder response senders")
                .insert(response_id.clone(), tx);
            self.response_receivers
                .lock()
                .expect("lock folder response receivers")
                .insert(response_id, rx);
        }

        self.sent_messages
            .lock()
            .expect("lock sent folder messages")
            .push((peer_id.clone(), message));
        self.notify.notify_waiters();
        Ok(())
    }

    async fn wait_for_response(
        &self,
        response_id: &str,
    ) -> Result<ProtocolMessage, FileSenderError> {
        let receiver = self
            .response_receivers
            .lock()
            .expect("lock folder response receivers")
            .remove(response_id)
            .expect("folder response receiver for id");
        receiver.await.map_err(|_| FileSenderError::SignalClosed)
    }

    async fn file_crypto_material(
        &self,
        _peer_id: &DeviceId,
        _file_id: &str,
    ) -> Result<jasmine_transfer::FileCryptoMaterial, FileSenderError> {
        Ok((test_file_key(), test_nonce_prefix()))
    }
}

#[derive(Debug, Clone)]
enum WorkerOutcome {
    Complete,
}

struct WorkerControl {
    sender: Mutex<Option<oneshot::Sender<WorkerOutcome>>>,
    receiver: Mutex<Option<oneshot::Receiver<WorkerOutcome>>>,
}

impl WorkerControl {
    fn new() -> Self {
        let (tx, rx) = oneshot::channel();
        Self {
            sender: Mutex::new(Some(tx)),
            receiver: Mutex::new(Some(rx)),
        }
    }

    fn take_receiver(&self) -> oneshot::Receiver<WorkerOutcome> {
        self.receiver
            .lock()
            .expect("lock worker receiver")
            .take()
            .expect("worker receiver available once")
    }

    fn complete(&self) {
        if let Some(sender) = self.sender.lock().expect("lock worker sender").take() {
            let _ = sender.send(WorkerOutcome::Complete);
        }
    }
}

#[derive(Clone)]
struct StartedSend {
    transfer_id: String,
}

#[derive(Clone)]
struct MockSender {
    started: Arc<Mutex<Vec<StartedSend>>>,
    controls: Arc<Mutex<HashMap<String, Arc<WorkerControl>>>>,
    notify: Arc<Notify>,
}

impl Default for MockSender {
    fn default() -> Self {
        Self {
            started: Arc::new(Mutex::new(Vec::new())),
            controls: Arc::new(Mutex::new(HashMap::new())),
            notify: Arc::new(Notify::new()),
        }
    }
}

impl MockSender {
    async fn wait_started(&self, transfer_id: &str) {
        loop {
            if self
                .started
                .lock()
                .expect("lock sender started")
                .iter()
                .any(|entry| entry.transfer_id == transfer_id)
            {
                return;
            }

            self.notify.notified().await;
        }
    }

    fn started_ids(&self) -> Vec<String> {
        self.started
            .lock()
            .expect("lock sender started")
            .iter()
            .map(|entry| entry.transfer_id.clone())
            .collect()
    }

    fn complete(&self, transfer_id: &str) {
        self.controls
            .lock()
            .expect("lock sender controls")
            .get(transfer_id)
            .expect("sender worker control")
            .complete();
    }
}

impl ManagedTransferSender for MockSender {
    async fn send_file(
        &self,
        transfer_id: &str,
        _peer_id: DeviceId,
        file_path: PathBuf,
        cancellation: CancellationToken,
        progress: Option<Arc<dyn TransferProgressReporter>>,
    ) -> Result<(), TransferManagerError> {
        let total_bytes = std::fs::metadata(&file_path)
            .expect("read mock sender file metadata")
            .len();
        let control = Arc::new(WorkerControl::new());
        let receiver = control.take_receiver();
        self.controls
            .lock()
            .expect("lock sender controls")
            .insert(transfer_id.to_string(), Arc::clone(&control));
        self.started
            .lock()
            .expect("lock sender started")
            .push(StartedSend {
                transfer_id: transfer_id.to_string(),
            });
        self.notify.notify_waiters();

        if let Some(progress) = progress.as_ref() {
            progress.report(TransferProgress {
                transfer_id: transfer_id.to_string(),
                bytes_sent: total_bytes.min(1),
                total_bytes,
                speed_bps: 7,
            });
        }

        match tokio::select! {
            _ = cancellation.cancelled() => Err(TransferManagerError::Cancelled),
            outcome = receiver => outcome.map_err(|_| TransferManagerError::Worker("mock sender channel closed".to_string())),
        }? {
            WorkerOutcome::Complete => {
                if let Some(progress) = progress.as_ref() {
                    progress.report(TransferProgress {
                        transfer_id: transfer_id.to_string(),
                        bytes_sent: total_bytes,
                        total_bytes,
                        speed_bps: 0,
                    });
                }
                Ok(())
            }
        }
    }

    async fn current_resume_token(
        &self,
        _peer_id: &DeviceId,
        _file_id: &str,
    ) -> Result<Option<String>, TransferManagerError> {
        Ok(Some(test_resume_token()))
    }
}

#[derive(Clone)]
struct PendingOfferState {
    filename: String,
    size: u64,
    download_dir: PathBuf,
}

type RejectedOffer = (String, Option<String>);
type ResumeOffsetCall = (String, Option<u64>);

#[derive(Clone)]
struct MockReceiver {
    download_dir: PathBuf,
    pending_offers: Arc<Mutex<HashMap<String, PendingOfferState>>>,
    completed_payloads: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    started: Arc<Mutex<Vec<String>>>,
    resume_offsets: Arc<Mutex<Vec<ResumeOffsetCall>>>,
    current_resume_token: Arc<Mutex<Option<String>>>,
    rejected: Arc<Mutex<Vec<RejectedOffer>>>,
    controls: Arc<Mutex<HashMap<String, Arc<WorkerControl>>>>,
    notify: Arc<Notify>,
}

impl MockReceiver {
    fn new(download_dir: PathBuf) -> Self {
        Self {
            download_dir,
            pending_offers: Arc::new(Mutex::new(HashMap::new())),
            completed_payloads: Arc::new(Mutex::new(HashMap::new())),
            started: Arc::new(Mutex::new(Vec::new())),
            resume_offsets: Arc::new(Mutex::new(Vec::new())),
            current_resume_token: Arc::new(Mutex::new(Some(test_resume_token()))),
            rejected: Arc::new(Mutex::new(Vec::new())),
            controls: Arc::new(Mutex::new(HashMap::new())),
            notify: Arc::new(Notify::new()),
        }
    }

    async fn wait_started(&self, transfer_id: &str) {
        loop {
            if self
                .started
                .lock()
                .expect("lock receiver started")
                .iter()
                .any(|id| id == transfer_id)
            {
                return;
            }

            self.notify.notified().await;
        }
    }

    fn rejected(&self) -> Vec<(String, Option<String>)> {
        self.rejected
            .lock()
            .expect("lock receiver rejected")
            .clone()
    }

    fn resume_offsets(&self) -> Vec<(String, Option<u64>)> {
        self.resume_offsets
            .lock()
            .expect("lock receiver resume offsets")
            .clone()
    }

    fn set_completed_payload(&self, transfer_id: &str, payload: Vec<u8>) {
        self.completed_payloads
            .lock()
            .expect("lock receiver completed payloads")
            .insert(transfer_id.to_string(), payload);
    }

    fn set_current_resume_token(&self, token: Option<String>) {
        *self
            .current_resume_token
            .lock()
            .expect("lock receiver current resume token") = token;
    }

    fn complete(&self, transfer_id: &str) {
        self.controls
            .lock()
            .expect("lock receiver controls")
            .get(transfer_id)
            .expect("receiver worker control")
            .complete();
    }
}

impl ManagedTransferReceiver for MockReceiver {
    async fn receive_offer(
        &self,
        sender_id: DeviceId,
        _sender_address: SocketAddr,
        message: ProtocolMessage,
    ) -> Result<FileOfferNotification, TransferManagerError> {
        match message {
            ProtocolMessage::FileOffer {
                id, filename, size, ..
            } => {
                let offer = PendingOfferState {
                    filename: filename.clone(),
                    size,
                    download_dir: self.download_dir.clone(),
                };
                self.pending_offers
                    .lock()
                    .expect("lock receiver offers")
                    .insert(id.clone(), offer);

                Ok(FileOfferNotification {
                    offer_id: id,
                    sender_id,
                    filename,
                    size,
                    download_dir: self.download_dir.clone(),
                    has_enough_space: true,
                    required_space_bytes: size,
                    available_space_bytes: Some(u64::MAX / 2),
                })
            }
            other => Err(TransferManagerError::Worker(format!(
                "unexpected mock receiver message: {other:?}"
            ))),
        }
    }

    async fn accept_offer(
        &self,
        offer_id: &str,
        resume_offset: Option<u64>,
        cancellation: CancellationToken,
        progress: Option<Arc<dyn TransferProgressReporter>>,
    ) -> Result<PathBuf, TransferManagerError> {
        let pending = self
            .pending_offers
            .lock()
            .expect("lock receiver offers")
            .remove(offer_id)
            .expect("pending offer exists");
        let final_path = pending.download_dir.join(&pending.filename);
        let control = Arc::new(WorkerControl::new());
        let receiver = control.take_receiver();
        self.controls
            .lock()
            .expect("lock receiver controls")
            .insert(offer_id.to_string(), Arc::clone(&control));
        self.resume_offsets
            .lock()
            .expect("lock receiver resume offsets")
            .push((offer_id.to_string(), resume_offset));
        self.started
            .lock()
            .expect("lock receiver started")
            .push(offer_id.to_string());
        self.notify.notify_waiters();

        if let Some(progress) = progress.as_ref() {
            progress.report(TransferProgress {
                transfer_id: offer_id.to_string(),
                bytes_sent: pending.size.min(1),
                total_bytes: pending.size,
                speed_bps: 11,
            });
        }

        match tokio::select! {
            _ = cancellation.cancelled() => Err(TransferManagerError::Cancelled),
            outcome = receiver => outcome.map_err(|_| TransferManagerError::Worker("mock receiver channel closed".to_string())),
        }? {
            WorkerOutcome::Complete => {
                if let Some(payload) = self
                    .completed_payloads
                    .lock()
                    .expect("lock receiver completed payloads")
                    .remove(offer_id)
                {
                    std::fs::create_dir_all(&pending.download_dir)
                        .expect("create mock receiver download dir");
                    std::fs::write(&final_path, payload).expect("write mock receiver payload");
                }

                if let Some(progress) = progress.as_ref() {
                    progress.report(TransferProgress {
                        transfer_id: offer_id.to_string(),
                        bytes_sent: pending.size,
                        total_bytes: pending.size,
                        speed_bps: 0,
                    });
                }
                Ok(final_path)
            }
        }
    }

    async fn current_resume_token(
        &self,
        _peer_id: &DeviceId,
        _file_id: &str,
    ) -> Result<Option<String>, TransferManagerError> {
        Ok(self
            .current_resume_token
            .lock()
            .expect("lock receiver current resume token")
            .clone())
    }

    async fn reject_offer(
        &self,
        offer_id: &str,
        reason: Option<String>,
    ) -> Result<(), TransferManagerError> {
        self.pending_offers
            .lock()
            .expect("lock receiver offers")
            .remove(offer_id);
        self.rejected
            .lock()
            .expect("lock receiver rejected")
            .push((offer_id.to_string(), reason));
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StorageEvent {
    Save {
        id: Uuid,
        status: TransferStatus,
        local_path: PathBuf,
    },
    Update {
        id: Uuid,
        status: TransferStatus,
    },
}

#[derive(Default)]
struct MockStorage {
    transfers: Mutex<HashMap<Uuid, TransferRecord>>,
    events: Mutex<Vec<StorageEvent>>,
}

impl MockStorage {
    fn transfer(&self, transfer_id: &Uuid) -> Option<TransferRecord> {
        self.transfers
            .lock()
            .expect("lock storage transfers")
            .get(transfer_id)
            .cloned()
    }

    fn thumbnail_path(&self, transfer_id: &Uuid) -> Option<String> {
        self.transfers
            .lock()
            .expect("lock storage transfers")
            .get(transfer_id)
            .and_then(|transfer| transfer.thumbnail_path.clone())
    }

    fn seed_transfer(&self, transfer: TransferRecord) {
        self.transfers
            .lock()
            .expect("lock storage transfers")
            .insert(transfer.id, transfer);
    }
}

impl StorageEngine for MockStorage {
    async fn save_message(&self, _message: &Message) -> Result<(), CoreError> {
        Err(CoreError::NotImplemented(
            "messages not used in manager tests",
        ))
    }

    async fn get_message(&self, _message_id: &str) -> Result<Option<Message>, CoreError> {
        Err(CoreError::NotImplemented(
            "messages not used in manager tests",
        ))
    }

    async fn get_messages(
        &self,
        _chat_id: &ChatId,
        _limit: usize,
        _offset: usize,
    ) -> Result<Vec<Message>, CoreError> {
        Err(CoreError::NotImplemented(
            "messages not used in manager tests",
        ))
    }

    async fn save_peer(&self, _peer: &PeerInfo) -> Result<(), CoreError> {
        Err(CoreError::NotImplemented("peers not used in manager tests"))
    }

    async fn save_peer_key(
        &self,
        _device_id: &str,
        _public_key: &[u8],
        _fingerprint: &str,
    ) -> Result<(), CoreError> {
        Err(CoreError::NotImplemented(
            "peer keys not used in manager tests",
        ))
    }

    async fn get_peer_key(&self, _device_id: &str) -> Result<Option<PeerKeyInfo>, CoreError> {
        Err(CoreError::NotImplemented(
            "peer keys not used in manager tests",
        ))
    }

    async fn get_all_peer_keys(&self) -> Result<Vec<PeerKeyInfo>, CoreError> {
        Err(CoreError::NotImplemented(
            "peer keys not used in manager tests",
        ))
    }

    async fn set_peer_verified(&self, _device_id: &str, _verified: bool) -> Result<(), CoreError> {
        Err(CoreError::NotImplemented(
            "peer keys not used in manager tests",
        ))
    }

    async fn update_peer_key_last_seen(&self, _device_id: &str) -> Result<(), CoreError> {
        Err(CoreError::NotImplemented(
            "peer keys not used in manager tests",
        ))
    }

    async fn save_sender_key(
        &self,
        _group_id: &str,
        _sender_device_id: &str,
        _key_id: &Uuid,
        _key_data: &[u8],
        _epoch: u32,
        _created_at_ms: u64,
    ) -> Result<(), CoreError> {
        Err(CoreError::NotImplemented(
            "sender keys not used in manager tests",
        ))
    }

    async fn get_sender_key(
        &self,
        _group_id: &str,
        _sender_device_id: &str,
    ) -> Result<Option<SenderKeyInfo>, CoreError> {
        Err(CoreError::NotImplemented(
            "sender keys not used in manager tests",
        ))
    }

    async fn get_sender_key_by_epoch(
        &self,
        _group_id: &str,
        _sender_device_id: &str,
        _epoch: u32,
    ) -> Result<Option<SenderKeyInfo>, CoreError> {
        Err(CoreError::NotImplemented(
            "sender keys not used in manager tests",
        ))
    }

    async fn delete_sender_keys_for_group(&self, _group_id: &str) -> Result<(), CoreError> {
        Err(CoreError::NotImplemented(
            "sender keys not used in manager tests",
        ))
    }

    async fn update_message_status(
        &self,
        _msg_id: &Uuid,
        _status: MessageStatus,
    ) -> Result<(), CoreError> {
        Err(CoreError::NotImplemented(
            "messages not used in manager tests",
        ))
    }

    async fn update_message_content(
        &self,
        _message_id: &str,
        _new_content: &str,
        _edit_version: u32,
        _edited_at_ms: u64,
    ) -> Result<(), CoreError> {
        Err(CoreError::NotImplemented(
            "messages not used in manager tests",
        ))
    }

    async fn mark_message_deleted(
        &self,
        _message_id: &str,
        _deleted_at_ms: u64,
    ) -> Result<(), CoreError> {
        Err(CoreError::NotImplemented(
            "messages not used in manager tests",
        ))
    }

    async fn save_transfer(&self, transfer: &TransferRecord) -> Result<(), CoreError> {
        self.transfers
            .lock()
            .expect("lock storage transfers")
            .insert(transfer.id, transfer.clone());
        self.events
            .lock()
            .expect("lock storage events")
            .push(StorageEvent::Save {
                id: transfer.id,
                status: transfer.status.clone(),
                local_path: transfer.local_path.clone(),
            });
        Ok(())
    }

    async fn get_transfers(
        &self,
        _limit: usize,
        _offset: usize,
    ) -> Result<Vec<TransferRecord>, CoreError> {
        let mut values = self
            .transfers
            .lock()
            .expect("lock storage transfers")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        values.sort_by_key(|transfer| transfer.id);
        Ok(values)
    }

    async fn save_thumbnail_path(
        &self,
        transfer_id: &str,
        thumbnail_path: &str,
    ) -> Result<(), CoreError> {
        let transfer_id = Uuid::parse_str(transfer_id)
            .map_err(|error| CoreError::Persistence(error.to_string()))?;
        let mut transfers = self.transfers.lock().expect("lock storage transfers");
        let transfer = transfers
            .get_mut(&transfer_id)
            .ok_or_else(|| CoreError::Persistence("missing transfer".to_string()))?;
        transfer.thumbnail_path = Some(thumbnail_path.to_string());
        Ok(())
    }

    async fn get_thumbnail_path(&self, transfer_id: &str) -> Result<Option<String>, CoreError> {
        let transfer_id = Uuid::parse_str(transfer_id)
            .map_err(|error| CoreError::Persistence(error.to_string()))?;
        Ok(self.thumbnail_path(&transfer_id))
    }

    async fn update_bytes_transferred(
        &self,
        transfer_id: &str,
        bytes: u64,
    ) -> Result<(), CoreError> {
        let transfer_id = Uuid::parse_str(transfer_id)
            .map_err(|error| CoreError::Persistence(error.to_string()))?;
        let mut transfers = self.transfers.lock().expect("lock storage transfers");
        let transfer = transfers
            .get_mut(&transfer_id)
            .ok_or_else(|| CoreError::Persistence("missing transfer".to_string()))?;
        transfer.bytes_transferred = bytes;
        Ok(())
    }

    async fn get_transfer_resume_info(
        &self,
        transfer_id: &str,
    ) -> Result<Option<ResumeInfo>, CoreError> {
        let transfer_id = Uuid::parse_str(transfer_id)
            .map_err(|error| CoreError::Persistence(error.to_string()))?;
        Ok(self
            .transfers
            .lock()
            .expect("lock storage transfers")
            .get(&transfer_id)
            .map(|transfer| ResumeInfo {
                bytes_transferred: transfer.bytes_transferred,
                resume_token: transfer.resume_token.clone(),
            }))
    }

    async fn update_transfer_status(
        &self,
        transfer_id: &Uuid,
        status: TransferStatus,
    ) -> Result<(), CoreError> {
        let mut transfers = self.transfers.lock().expect("lock storage transfers");
        let transfer = transfers
            .get_mut(transfer_id)
            .ok_or_else(|| CoreError::Persistence("missing transfer".to_string()))?;
        transfer.status = status.clone();
        self.events
            .lock()
            .expect("lock storage events")
            .push(StorageEvent::Update {
                id: *transfer_id,
                status,
            });
        Ok(())
    }
}

fn assert_transfer_state(
    transfer: &ManagedTransfer,
    status: TransferStatus,
    direction: TransferDirection,
) {
    assert_eq!(transfer.status, status);
    assert_eq!(transfer.direction, direction);
}

async fn wait_for_started_count(sender: &MockSender, expected: usize) -> Vec<String> {
    time::timeout(Duration::from_secs(2), async {
        loop {
            let started = sender.started_ids();
            if started.len() >= expected {
                return started;
            }

            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("wait for started transfer count timeout")
}

async fn wait_for_transfer_status<S, R, St>(
    manager: &TransferManager<S, R, St>,
    transfer_id: &str,
    expected: TransferStatus,
) where
    S: ManagedTransferSender,
    R: ManagedTransferReceiver,
    St: StorageEngine,
{
    time::timeout(Duration::from_secs(2), async {
        loop {
            if manager
                .transfer(transfer_id)
                .map(|transfer| transfer.status == expected)
                .unwrap_or(false)
            {
                return;
            }

            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("wait for transfer status timeout");
}

async fn wait_for_thumbnail_path(storage: &MockStorage, transfer_id: &Uuid) -> String {
    time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(path) = storage.thumbnail_path(transfer_id) {
                return path;
            }

            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("wait for thumbnail path timeout")
}

fn partial_path_for(download_dir: &Path, file_name: &str) -> PathBuf {
    let mut partial_name = std::ffi::OsString::from(file_name);
    partial_name.push(".jasmine-partial");
    download_dir.join(partial_name)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_transfer_manager_persists_bytes_transferred_on_cancellation() {
    let temp = tempdir().expect("tempdir");
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_settings(&settings, 1);

    let sender = Arc::new(MockSender::default());
    let receiver = Arc::new(MockReceiver::new(temp.path().join("downloads")));
    let storage = Arc::new(MockStorage::default());
    let manager = TransferManager::new(
        Arc::clone(&sender),
        Arc::clone(&receiver),
        settings,
        Arc::clone(&storage),
    )
    .expect("create transfer manager");

    let transfer_id = manager
        .send_file(
            device_id(),
            write_file(&temp.path().join("resume-source.bin"), 1024 * 1024),
            None,
        )
        .await
        .expect("queue resumable send");
    sender.wait_started(&transfer_id).await;

    manager
        .cancel_transfer(&transfer_id)
        .await
        .expect("cancel active transfer");
    wait_for_transfer_status(&manager, &transfer_id, TransferStatus::Cancelled).await;

    let stored = storage
        .transfer(&Uuid::parse_str(&transfer_id).expect("transfer uuid"))
        .expect("stored cancelled transfer exists");
    assert_eq!(stored.status, TransferStatus::Cancelled);
    assert_eq!(stored.bytes_transferred, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_transfer_manager_reuses_stored_offset_for_matching_offer_id() {
    let temp = tempdir().expect("tempdir");
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_settings(&settings, 1);

    let sender = Arc::new(MockSender::default());
    let receiver = Arc::new(MockReceiver::new(temp.path().join("downloads")));
    let storage = Arc::new(MockStorage::default());
    let manager = TransferManager::new(
        Arc::clone(&sender),
        Arc::clone(&receiver),
        settings,
        Arc::clone(&storage),
    )
    .expect("create transfer manager");
    let offer_id = Uuid::new_v4().to_string();
    let resume_offset = (2 * DEFAULT_CHUNK_SIZE) as u64;

    std::fs::create_dir_all(temp.path().join("downloads")).expect("create download dir");
    std::fs::write(
        partial_path_for(&temp.path().join("downloads"), "resume.bin"),
        vec![0x55; resume_offset as usize],
    )
    .expect("write resume partial");
    storage.seed_transfer(TransferRecord {
        id: Uuid::parse_str(&offer_id).expect("offer uuid"),
        peer_id: device_id(),
        chat_id: None,
        file_name: "resume.bin".to_string(),
        local_path: temp.path().join("downloads").join("resume.bin"),
        status: TransferStatus::Cancelled,
        thumbnail_path: None,
        folder_id: None,
        folder_relative_path: None,
        bytes_transferred: resume_offset,
        resume_token: Some(test_resume_token()),
    });

    manager
        .handle_signal_message(
            device_id(),
            SocketAddr::from(([127, 0, 0, 1], 9735)),
            ProtocolMessage::FileOffer {
                id: offer_id.clone(),
                filename: "resume.bin".to_string(),
                size: 4 * DEFAULT_CHUNK_SIZE as u64,
                sha256: "ignored".to_string(),
                transfer_port: 4045,
            },
        )
        .await
        .expect("handle resumable offer");

    manager
        .accept_offer(&offer_id, None)
        .await
        .expect("accept resumable offer");
    receiver.wait_started(&offer_id).await;

    assert!(receiver
        .resume_offsets()
        .contains(&(offer_id.clone(), Some(resume_offset))));
    receiver.complete(&offer_id);
    wait_for_transfer_status(&manager, &offer_id, TransferStatus::Completed).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn encrypted_resume_epoch_mismatch_manager_restarts_from_byte_zero() {
    let temp = tempdir().expect("tempdir");
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_settings(&settings, 1);

    let sender = Arc::new(MockSender::default());
    let receiver = Arc::new(MockReceiver::new(temp.path().join("downloads")));
    receiver.set_current_resume_token(Some("rotated-transfer-resume-token".to_string()));
    let storage = Arc::new(MockStorage::default());
    let manager = TransferManager::new(
        Arc::clone(&sender),
        Arc::clone(&receiver),
        settings,
        Arc::clone(&storage),
    )
    .expect("create transfer manager");
    let offer_id = Uuid::new_v4().to_string();
    let resume_offset = (2 * DEFAULT_CHUNK_SIZE) as u64;
    let partial_path = partial_path_for(&temp.path().join("downloads"), "epoch.bin");

    std::fs::create_dir_all(temp.path().join("downloads")).expect("create download dir");
    std::fs::write(&partial_path, vec![0x44; resume_offset as usize])
        .expect("write stale encrypted partial");
    storage.seed_transfer(TransferRecord {
        id: Uuid::parse_str(&offer_id).expect("offer uuid"),
        peer_id: device_id(),
        chat_id: None,
        file_name: "epoch.bin".to_string(),
        local_path: temp.path().join("downloads").join("epoch.bin"),
        status: TransferStatus::Cancelled,
        thumbnail_path: None,
        folder_id: None,
        folder_relative_path: None,
        bytes_transferred: resume_offset,
        resume_token: Some(test_resume_token()),
    });

    manager
        .handle_signal_message(
            device_id(),
            SocketAddr::from(([127, 0, 0, 1], 9735)),
            ProtocolMessage::FileOffer {
                id: offer_id.clone(),
                filename: "epoch.bin".to_string(),
                size: 4 * DEFAULT_CHUNK_SIZE as u64,
                sha256: "ignored".to_string(),
                transfer_port: 4047,
            },
        )
        .await
        .expect("handle resumable encrypted offer");

    manager
        .accept_offer(&offer_id, None)
        .await
        .expect("accept epoch-mismatched offer");
    receiver.wait_started(&offer_id).await;

    assert!(receiver
        .resume_offsets()
        .contains(&(offer_id.clone(), None)));
    assert!(
        !partial_path.exists(),
        "epoch mismatch should clear the stale partial and restart from byte 0"
    );
    let stored = storage
        .transfer(&Uuid::parse_str(&offer_id).expect("offer uuid"))
        .expect("stored transfer exists");
    assert!(
        stored.bytes_transferred <= 1,
        "restarted transfer should begin from byte 0 and only advance through the initial progress tick"
    );
    assert_eq!(
        stored.resume_token,
        Some("rotated-transfer-resume-token".to_string())
    );

    receiver.complete(&offer_id);
    wait_for_transfer_status(&manager, &offer_id, TransferStatus::Completed).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_fresh_transfer_manager_different_offer_id_starts_from_zero() {
    let temp = tempdir().expect("tempdir");
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_settings(&settings, 1);

    let sender = Arc::new(MockSender::default());
    let receiver = Arc::new(MockReceiver::new(temp.path().join("downloads")));
    let storage = Arc::new(MockStorage::default());
    let manager = TransferManager::new(
        Arc::clone(&sender),
        Arc::clone(&receiver),
        settings,
        Arc::clone(&storage),
    )
    .expect("create transfer manager");
    let old_offer_id = Uuid::new_v4().to_string();
    let new_offer_id = Uuid::new_v4().to_string();
    let resume_offset = DEFAULT_CHUNK_SIZE as u64;

    std::fs::create_dir_all(temp.path().join("downloads")).expect("create download dir");
    std::fs::write(
        partial_path_for(&temp.path().join("downloads"), "fresh.bin"),
        vec![0x22; resume_offset as usize],
    )
    .expect("write stale partial");
    storage.seed_transfer(TransferRecord {
        id: Uuid::parse_str(&old_offer_id).expect("old offer uuid"),
        peer_id: device_id(),
        chat_id: None,
        file_name: "fresh.bin".to_string(),
        local_path: temp.path().join("downloads").join("fresh.bin"),
        status: TransferStatus::Cancelled,
        thumbnail_path: None,
        folder_id: None,
        folder_relative_path: None,
        bytes_transferred: resume_offset,
        resume_token: None,
    });

    manager
        .handle_signal_message(
            device_id(),
            SocketAddr::from(([127, 0, 0, 1], 9735)),
            ProtocolMessage::FileOffer {
                id: new_offer_id.clone(),
                filename: "fresh.bin".to_string(),
                size: 2 * DEFAULT_CHUNK_SIZE as u64,
                sha256: "ignored".to_string(),
                transfer_port: 4046,
            },
        )
        .await
        .expect("handle fresh offer");

    manager
        .accept_offer(&new_offer_id, None)
        .await
        .expect("accept fresh offer");
    receiver.wait_started(&new_offer_id).await;

    assert!(receiver
        .resume_offsets()
        .contains(&(new_offer_id.clone(), None)));
    receiver.complete(&new_offer_id);
    wait_for_transfer_status(&manager, &new_offer_id, TransferStatus::Completed).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manager_tracks_offer_acceptance_and_persists_completed_receive_history() {
    let temp = tempdir().expect("tempdir");
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_settings(&settings, 1);

    let sender = Arc::new(MockSender::default());
    let receiver = Arc::new(MockReceiver::new(temp.path().join("downloads")));
    let storage = Arc::new(MockStorage::default());
    let manager = TransferManager::new(
        Arc::clone(&sender),
        Arc::clone(&receiver),
        settings,
        Arc::clone(&storage),
    )
    .expect("create transfer manager");
    let peer_id = device_id();
    let offer_id = Uuid::new_v4().to_string();

    let offer = manager
        .handle_signal_message(
            peer_id.clone(),
            SocketAddr::from(([127, 0, 0, 1], 9735)),
            ProtocolMessage::FileOffer {
                id: offer_id.clone(),
                filename: "report.pdf".to_string(),
                size: 8,
                sha256: "ignored".to_string(),
                transfer_port: 4040,
            },
        )
        .await
        .expect("handle file offer")
        .expect("offer notification");

    assert_eq!(offer.offer_id, offer_id);
    assert_eq!(offer.filename, "report.pdf");
    assert_eq!(manager.snapshot().pending_offers.len(), 1);

    let transfer_id = manager
        .accept_offer(&offer_id, Some(chat_id()))
        .await
        .expect("accept offer");
    assert_eq!(transfer_id, offer_id);
    receiver.wait_started(&offer_id).await;

    let aggregate = manager.aggregate_progress();
    assert_eq!(
        aggregate,
        TransferAggregateProgress {
            active_transfers: 1,
            queued_transfers: 0,
            bytes_sent: 1,
            total_bytes: 8,
            speed_bps: 11,
        }
    );

    receiver.complete(&offer_id);
    wait_for_transfer_status(&manager, &offer_id, TransferStatus::Completed).await;

    let transfer = manager
        .transfer(&offer_id)
        .expect("completed transfer exists");
    assert_transfer_state(
        &transfer,
        TransferStatus::Completed,
        TransferDirection::Receive,
    );
    assert_eq!(
        transfer.local_path,
        temp.path().join("downloads").join("report.pdf")
    );
    assert!(manager.snapshot().pending_offers.is_empty());

    let transfer_uuid = Uuid::parse_str(&offer_id).expect("offer uuid");
    let stored = storage
        .transfer(&transfer_uuid)
        .expect("stored transfer exists");
    assert_eq!(stored.status, TransferStatus::Completed);
    assert_eq!(stored.local_path, transfer.local_path);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manager_reject_offer_skips_transfer_history_and_clears_pending_offer() {
    let temp = tempdir().expect("tempdir");
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_settings(&settings, 1);

    let manager = TransferManager::new(
        Arc::new(MockSender::default()),
        Arc::new(MockReceiver::new(temp.path().join("downloads"))),
        settings,
        Arc::new(MockStorage::default()),
    )
    .expect("create transfer manager");
    let offer_id = Uuid::new_v4().to_string();

    manager
        .handle_signal_message(
            device_id(),
            SocketAddr::from(([127, 0, 0, 1], 9735)),
            ProtocolMessage::FileOffer {
                id: offer_id.clone(),
                filename: "notes.txt".to_string(),
                size: 16,
                sha256: "ignored".to_string(),
                transfer_port: 4041,
            },
        )
        .await
        .expect("handle file offer");

    manager
        .reject_offer(&offer_id, Some("not now".to_string()))
        .await
        .expect("reject offer");

    assert!(manager.snapshot().pending_offers.is_empty());
    assert!(manager.transfer(&offer_id).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn folder_send_persists_parent_child_linkage_in_transfer_records() {
    let temp = tempdir().expect("tempdir");
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_settings(&settings, 1);

    let signal = MockFolderSignal::default();
    let sender = Arc::new(MockSender::default());
    let receiver = Arc::new(MockReceiver::new(temp.path().join("downloads")));
    let storage = Arc::new(MockStorage::default());
    let manager = Arc::new(
        TransferManager::new(
            Arc::clone(&sender),
            Arc::clone(&receiver),
            settings,
            Arc::clone(&storage),
        )
        .expect("create transfer manager"),
    );
    let coordinator = FolderTransferCoordinator::new(signal.clone(), Arc::clone(&manager));

    let folder_root = temp.path().join("project-folder");
    std::fs::create_dir_all(folder_root.join("docs")).expect("create docs directory");
    std::fs::create_dir_all(folder_root.join("images")).expect("create images directory");
    write_file(&folder_root.join("docs/report.txt"), 8);
    write_file(&folder_root.join("images/icon.bin"), 4);

    let send_task = tokio::spawn({
        let folder_root = folder_root.clone();
        async move {
            coordinator
                .send_folder(
                    device_id(),
                    device_id(),
                    folder_root,
                    CancellationToken::new(),
                    None,
                )
                .await
        }
    });

    let (_, manifest_message) = signal.wait_for_manifest().await;
    let folder_transfer_id = match manifest_message {
        ProtocolMessage::FolderManifest {
            folder_transfer_id,
            manifest,
            ..
        } => {
            assert_eq!(manifest.folder_name, "project-folder");
            assert_eq!(
                manifest
                    .files
                    .iter()
                    .map(|entry| entry.relative_path.clone())
                    .collect::<Vec<_>>(),
                vec!["docs/report.txt".to_string(), "images/icon.bin".to_string()]
            );
            folder_transfer_id
        }
        other => panic!("unexpected manifest message: {other:?}"),
    };
    signal.respond(
        &folder_transfer_id,
        ProtocolMessage::FolderAccept {
            folder_transfer_id: folder_transfer_id.clone(),
        },
    );

    let started = wait_for_started_count(&sender, 1).await;
    sender.complete(&started[0]);
    let started = wait_for_started_count(&sender, 2).await;
    sender.complete(&started[1]);

    let outcome = send_task
        .await
        .expect("folder send task joins")
        .expect("folder send succeeds");
    assert_eq!(outcome.folder_transfer_id, folder_transfer_id);
    assert_eq!(
        outcome.progress.status,
        jasmine_transfer::FolderTransferStatus::Completed
    );

    let mut stored = storage
        .get_transfers(10, 0)
        .await
        .expect("load stored transfers")
        .into_iter()
        .filter(|transfer| transfer.folder_id.as_deref() == Some(folder_transfer_id.as_str()))
        .collect::<Vec<_>>();
    stored.sort_by(|left, right| left.folder_relative_path.cmp(&right.folder_relative_path));

    assert_eq!(stored.len(), 2);
    assert_eq!(
        stored
            .iter()
            .map(|transfer| {
                (
                    transfer.folder_relative_path.clone(),
                    transfer
                        .local_path
                        .strip_prefix(&folder_root)
                        .ok()
                        .map(PathBuf::from),
                    transfer.status.clone(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                Some("docs/report.txt".to_string()),
                Some(PathBuf::from("docs/report.txt")),
                TransferStatus::Completed,
            ),
            (
                Some("images/icon.bin".to_string()),
                Some(PathBuf::from("images/icon.bin")),
                TransferStatus::Completed,
            ),
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manager_receive_image_generates_thumbnail_and_persists_path() {
    let temp = tempdir().expect("tempdir");
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_settings(&settings, 1);

    let sender = Arc::new(MockSender::default());
    let receiver = Arc::new(MockReceiver::new(temp.path().join("downloads")));
    let storage = Arc::new(MockStorage::default());
    let manager = TransferManager::new(
        Arc::clone(&sender),
        Arc::clone(&receiver),
        settings,
        Arc::clone(&storage),
    )
    .expect("create transfer manager");
    let offer_id = Uuid::new_v4().to_string();
    let payload = encode_image_bytes(ImageFormat::Png, 900, 600);
    receiver.set_completed_payload(&offer_id, payload.clone());

    manager
        .handle_signal_message(
            device_id(),
            SocketAddr::from(([127, 0, 0, 1], 9735)),
            ProtocolMessage::FileOffer {
                id: offer_id.clone(),
                filename: "preview-source.png".to_string(),
                size: payload.len() as u64,
                sha256: "ignored".to_string(),
                transfer_port: 4043,
            },
        )
        .await
        .expect("handle image offer");

    manager
        .accept_offer(&offer_id, Some(chat_id()))
        .await
        .expect("accept image offer");
    receiver.wait_started(&offer_id).await;
    receiver.complete(&offer_id);
    wait_for_transfer_status(&manager, &offer_id, TransferStatus::Completed).await;

    let transfer_uuid = Uuid::parse_str(&offer_id).expect("offer uuid");
    let thumbnail_path = wait_for_thumbnail_path(&storage, &transfer_uuid).await;
    let stored = storage
        .transfer(&transfer_uuid)
        .expect("stored transfer exists");

    assert_eq!(stored.status, TransferStatus::Completed);
    assert_eq!(
        stored.thumbnail_path.as_deref(),
        Some(thumbnail_path.as_str())
    );
    assert!(thumbnail_path.ends_with(".webp"));
    assert!(
        thumbnail_path.contains("app-data/thumbnails"),
        "thumbnail should be stored under app-data/thumbnails"
    );
    assert_webp_thumbnail(Path::new(&thumbnail_path));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manager_receive_multiple_images_persists_each_thumbnail_path() {
    let temp = tempdir().expect("tempdir");
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_settings(&settings, 2);

    let sender = Arc::new(MockSender::default());
    let receiver = Arc::new(MockReceiver::new(temp.path().join("downloads")));
    let storage = Arc::new(MockStorage::default());
    let manager = TransferManager::new(
        Arc::clone(&sender),
        Arc::clone(&receiver),
        settings,
        Arc::clone(&storage),
    )
    .expect("create transfer manager");
    let offers = [
        (Uuid::new_v4().to_string(), "first.png"),
        (Uuid::new_v4().to_string(), "second.png"),
    ];
    let payload = encode_image_bytes(ImageFormat::Png, 640, 480);

    for (offer_id, filename) in &offers {
        receiver.set_completed_payload(offer_id, payload.clone());
        manager
            .handle_signal_message(
                device_id(),
                SocketAddr::from(([127, 0, 0, 1], 9735)),
                ProtocolMessage::FileOffer {
                    id: offer_id.clone(),
                    filename: (*filename).to_string(),
                    size: payload.len() as u64,
                    sha256: "ignored".to_string(),
                    transfer_port: 4043,
                },
            )
            .await
            .expect("handle image offer");
        manager
            .accept_offer(offer_id, Some(chat_id()))
            .await
            .expect("accept image offer");
    }

    for (offer_id, _) in &offers {
        receiver.wait_started(offer_id).await;
    }
    for (offer_id, _) in &offers {
        receiver.complete(offer_id);
    }
    for (offer_id, _) in &offers {
        wait_for_transfer_status(&manager, offer_id, TransferStatus::Completed).await;
    }

    for (offer_id, _) in &offers {
        let transfer_uuid = Uuid::parse_str(offer_id).expect("offer uuid");
        let thumbnail_path = wait_for_thumbnail_path(&storage, &transfer_uuid).await;
        let stored = storage
            .transfer(&transfer_uuid)
            .expect("stored transfer exists");

        assert_eq!(stored.status, TransferStatus::Completed);
        assert_eq!(
            stored.thumbnail_path.as_deref(),
            Some(thumbnail_path.as_str())
        );
        assert!(thumbnail_path.ends_with(".webp"));
        assert_webp_thumbnail(Path::new(&thumbnail_path));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manager_non_image_thumbnail_failure_does_not_block_receive_completion() {
    let temp = tempdir().expect("tempdir");
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_settings(&settings, 1);

    let sender = Arc::new(MockSender::default());
    let receiver = Arc::new(MockReceiver::new(temp.path().join("downloads")));
    let storage = Arc::new(MockStorage::default());
    let manager = TransferManager::new(
        Arc::clone(&sender),
        Arc::clone(&receiver),
        settings,
        Arc::clone(&storage),
    )
    .expect("create transfer manager");
    let offer_id = Uuid::new_v4().to_string();
    let payload = b"plain text is not an image".to_vec();
    receiver.set_completed_payload(&offer_id, payload.clone());

    manager
        .handle_signal_message(
            device_id(),
            SocketAddr::from(([127, 0, 0, 1], 9735)),
            ProtocolMessage::FileOffer {
                id: offer_id.clone(),
                filename: "notes.txt".to_string(),
                size: payload.len() as u64,
                sha256: "ignored".to_string(),
                transfer_port: 4044,
            },
        )
        .await
        .expect("handle text offer");

    manager
        .accept_offer(&offer_id, None)
        .await
        .expect("accept text offer");
    receiver.wait_started(&offer_id).await;
    receiver.complete(&offer_id);
    wait_for_transfer_status(&manager, &offer_id, TransferStatus::Completed).await;
    time::sleep(Duration::from_millis(100)).await;

    let transfer_uuid = Uuid::parse_str(&offer_id).expect("offer uuid");
    let stored = storage
        .transfer(&transfer_uuid)
        .expect("stored transfer exists");
    assert_eq!(stored.status, TransferStatus::Completed);
    assert_eq!(storage.thumbnail_path(&transfer_uuid), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manager_concurrent_enforces_limit_and_promotes_fifo_queue() {
    let temp = tempdir().expect("tempdir");
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_settings(&settings, 3);

    let sender = Arc::new(MockSender::default());
    let receiver = Arc::new(MockReceiver::new(temp.path().join("downloads")));
    let storage = Arc::new(MockStorage::default());
    let manager = TransferManager::new(
        Arc::clone(&sender),
        Arc::clone(&receiver),
        settings,
        storage,
    )
    .expect("create transfer manager");
    let peer_id = device_id();

    let transfer_ids = [
        manager
            .send_file(
                peer_id.clone(),
                write_file(&temp.path().join("one.bin"), 10),
                None,
            )
            .await
            .expect("send 1"),
        manager
            .send_file(
                peer_id.clone(),
                write_file(&temp.path().join("two.bin"), 20),
                None,
            )
            .await
            .expect("send 2"),
        manager
            .send_file(
                peer_id.clone(),
                write_file(&temp.path().join("three.bin"), 30),
                None,
            )
            .await
            .expect("send 3"),
        manager
            .send_file(
                peer_id.clone(),
                write_file(&temp.path().join("four.bin"), 40),
                None,
            )
            .await
            .expect("send 4"),
        manager
            .send_file(peer_id, write_file(&temp.path().join("five.bin"), 50), None)
            .await
            .expect("send 5"),
    ];

    sender.wait_started(&transfer_ids[0]).await;
    sender.wait_started(&transfer_ids[1]).await;
    sender.wait_started(&transfer_ids[2]).await;

    let snapshot = manager.snapshot();
    assert_eq!(snapshot.active_limit, 3);
    assert_eq!(snapshot.active_transfers.len(), 3);
    assert_eq!(snapshot.queued_transfers.len(), 2);
    assert_eq!(
        snapshot
            .queued_transfers
            .iter()
            .map(|transfer| transfer.id.clone())
            .collect::<Vec<_>>(),
        vec![transfer_ids[3].clone(), transfer_ids[4].clone()]
    );

    sender.complete(&transfer_ids[0]);
    sender.wait_started(&transfer_ids[3]).await;
    wait_for_transfer_status(&manager, &transfer_ids[0], TransferStatus::Completed).await;

    let after_promotion = manager.snapshot();
    assert_eq!(after_promotion.active_transfers.len(), 3);
    assert_eq!(after_promotion.queued_transfers.len(), 1);
    assert_eq!(after_promotion.queued_transfers[0].id, transfer_ids[4]);
    assert_eq!(sender.started_ids()[3], transfer_ids[3]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manager_cancel_cancels_queued_and_active_transfers_and_releases_slots() {
    let temp = tempdir().expect("tempdir");
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_settings(&settings, 1);

    let sender = Arc::new(MockSender::default());
    let receiver = Arc::new(MockReceiver::new(temp.path().join("downloads")));
    let storage = Arc::new(MockStorage::default());
    let manager = TransferManager::new(
        Arc::clone(&sender),
        Arc::clone(&receiver),
        settings,
        Arc::clone(&storage),
    )
    .expect("create transfer manager");
    let peer_id = device_id();

    let active_id = manager
        .send_file(
            peer_id.clone(),
            write_file(&temp.path().join("active.bin"), 10),
            None,
        )
        .await
        .expect("queue active send");
    let queued_send_id = manager
        .send_file(
            peer_id,
            write_file(&temp.path().join("queued.bin"), 20),
            None,
        )
        .await
        .expect("queue second send");
    sender.wait_started(&active_id).await;

    let queued_offer_id = Uuid::new_v4().to_string();
    manager
        .handle_signal_message(
            device_id(),
            SocketAddr::from(([127, 0, 0, 1], 9735)),
            ProtocolMessage::FileOffer {
                id: queued_offer_id.clone(),
                filename: "queued-recv.bin".to_string(),
                size: 12,
                sha256: "ignored".to_string(),
                transfer_port: 4042,
            },
        )
        .await
        .expect("handle queued offer");
    let queued_receive_id = manager
        .accept_offer(&queued_offer_id, None)
        .await
        .expect("queue accepted receive");

    assert_eq!(manager.snapshot().queued_transfers.len(), 2);

    manager
        .cancel_transfer(&queued_receive_id)
        .await
        .expect("cancel queued receive");
    wait_for_transfer_status(&manager, &queued_receive_id, TransferStatus::Cancelled).await;
    assert_eq!(
        receiver.rejected(),
        vec![(queued_receive_id.clone(), Some("cancelled".to_string()))]
    );

    manager
        .cancel_transfer(&active_id)
        .await
        .expect("cancel active send");
    wait_for_transfer_status(&manager, &active_id, TransferStatus::Cancelled).await;
    sender.wait_started(&queued_send_id).await;

    let active = manager
        .transfer(&active_id)
        .expect("cancelled active transfer");
    let queued = manager
        .transfer(&queued_receive_id)
        .expect("cancelled queued transfer");
    assert_transfer_state(&active, TransferStatus::Cancelled, TransferDirection::Send);
    assert_transfer_state(
        &queued,
        TransferStatus::Cancelled,
        TransferDirection::Receive,
    );

    let transfer_uuid = Uuid::parse_str(&queued_receive_id).expect("queued receive uuid");
    let stored = storage
        .transfer(&transfer_uuid)
        .expect("stored cancelled transfer");
    assert_eq!(stored.status, TransferStatus::Cancelled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manager_settings_update_promotes_queued_work_when_limit_increases() {
    let temp = tempdir().expect("tempdir");
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_settings(&settings, 3);

    let sender = Arc::new(MockSender::default());
    let receiver = Arc::new(MockReceiver::new(temp.path().join("downloads")));
    let manager = TransferManager::new(
        Arc::clone(&sender),
        Arc::clone(&receiver),
        settings.clone(),
        Arc::new(MockStorage::default()),
    )
    .expect("create transfer manager");
    let peer_id = device_id();

    let transfer_ids = [
        manager
            .send_file(
                peer_id.clone(),
                write_file(&temp.path().join("one.bin"), 10),
                None,
            )
            .await
            .expect("send 1"),
        manager
            .send_file(
                peer_id.clone(),
                write_file(&temp.path().join("two.bin"), 20),
                None,
            )
            .await
            .expect("send 2"),
        manager
            .send_file(
                peer_id.clone(),
                write_file(&temp.path().join("three.bin"), 30),
                None,
            )
            .await
            .expect("send 3"),
        manager
            .send_file(
                peer_id.clone(),
                write_file(&temp.path().join("four.bin"), 40),
                None,
            )
            .await
            .expect("send 4"),
        manager
            .send_file(peer_id, write_file(&temp.path().join("five.bin"), 50), None)
            .await
            .expect("send 5"),
    ];

    sender.wait_started(&transfer_ids[0]).await;
    sender.wait_started(&transfer_ids[1]).await;
    sender.wait_started(&transfer_ids[2]).await;

    assert_eq!(manager.snapshot().active_transfers.len(), 3);
    assert_eq!(manager.snapshot().queued_transfers.len(), 2);

    save_settings(&settings, 5);
    manager
        .on_settings_changed()
        .await
        .expect("apply settings update");

    sender.wait_started(&transfer_ids[3]).await;
    sender.wait_started(&transfer_ids[4]).await;

    let snapshot = manager.snapshot();
    assert_eq!(snapshot.active_limit, 5);
    assert_eq!(snapshot.active_transfers.len(), 5);
    assert!(snapshot.queued_transfers.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manager_aggregates_progress_across_active_transfers() {
    let temp = tempdir().expect("tempdir");
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_settings(&settings, 2);

    let sender = Arc::new(MockSender::default());
    let receiver = Arc::new(MockReceiver::new(temp.path().join("downloads")));
    let manager = TransferManager::new(
        Arc::clone(&sender),
        Arc::clone(&receiver),
        settings,
        Arc::new(MockStorage::default()),
    )
    .expect("create transfer manager");
    let peer_id = device_id();

    let first_id = manager
        .send_file(
            peer_id.clone(),
            write_file(&temp.path().join("first.bin"), 10),
            None,
        )
        .await
        .expect("send first");
    let second_id = manager
        .send_file(
            peer_id,
            write_file(&temp.path().join("second.bin"), 20),
            None,
        )
        .await
        .expect("send second");

    sender.wait_started(&first_id).await;
    sender.wait_started(&second_id).await;

    let aggregate = manager.aggregate_progress();
    assert_eq!(
        aggregate,
        TransferAggregateProgress {
            active_transfers: 2,
            queued_transfers: 0,
            bytes_sent: 2,
            total_bytes: 30,
            speed_bps: 14,
        }
    );
}

#[test]
fn image_smoke() {
    let tempdir = tempdir().expect("tempdir");
    let mut path = PathBuf::from(tempdir.path());
    path.push("smoke.png");

    let image = RgbaImage::new(1, 1);
    image
        .save(&path)
        .expect("write 1x1 png for image smoke test");

    let decoded = image::open(&path).expect("read 1x1 png for image smoke test");
    assert_eq!(decoded.width(), 1);
    assert_eq!(decoded.height(), 1);
}
