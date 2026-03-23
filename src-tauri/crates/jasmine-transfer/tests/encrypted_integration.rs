use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jasmine_core::{
    AppSettings, DeviceId, ProtocolMessage, SettingsService, StorageEngine, TransferRecord,
    TransferStatus,
};
use jasmine_storage::SqliteStorage;
use jasmine_transfer::{
    generate_folder_manifest, DiskSpaceChecker, FileOfferNotification, FileOfferNotifier,
    FileReceiver, FileReceiverConfig, FileReceiverError, FileReceiverSignal, FileSender,
    FileSenderConfig, FileSenderError, FileSenderSignal, FolderFileTransferSender, FolderReceiver,
    FolderTransferCoordinator, FolderTransferStatus, ManagedTransferReceiver,
    ManagedTransferSender, TransferManager, TransferManagerError, TransferProgress,
    TransferProgressReporter, DEFAULT_CHUNK_SIZE, DEFAULT_FOLDER_MANIFEST_MAX_FILES,
};
use sha2::{Digest, Sha256};
use tempfile::tempdir;
use tokio::sync::{oneshot, Notify};
use tokio::time;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn peer_id() -> DeviceId {
    DeviceId(Uuid::new_v4())
}

fn write_bytes(path: &Path, bytes: &[u8]) -> PathBuf {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create test directories");
    }
    std::fs::write(path, bytes).expect("write temp payload");
    path.to_path_buf()
}

fn save_download_dir(settings: &SettingsService, download_dir: &Path) {
    settings
        .save(&AppSettings {
            download_dir: download_dir.to_string_lossy().into_owned(),
            max_concurrent_transfers: 3,
        })
        .expect("save test settings");
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn test_file_key() -> [u8; 32] {
    [0x31; 32]
}

fn test_nonce_prefix() -> [u8; 8] {
    [0x7a; 8]
}

fn sender_config() -> FileSenderConfig {
    FileSenderConfig {
        file_key: Some(test_file_key()),
        nonce_prefix: Some(test_nonce_prefix()),
        ..FileSenderConfig::default()
    }
}

fn receiver_config() -> FileReceiverConfig {
    FileReceiverConfig {
        file_key: Some(test_file_key()),
        nonce_prefix: Some(test_nonce_prefix()),
        ..FileReceiverConfig::default()
    }
}

#[derive(Clone, Default)]
struct IntegratedSignal {
    sent_messages: Arc<Mutex<Vec<(DeviceId, ProtocolMessage)>>>,
    sender_response_senders: Arc<Mutex<HashMap<String, oneshot::Sender<ProtocolMessage>>>>,
    sender_response_receivers: Arc<Mutex<HashMap<String, oneshot::Receiver<ProtocolMessage>>>>,
    receiver_response_senders: Arc<Mutex<HashMap<String, oneshot::Sender<ProtocolMessage>>>>,
    receiver_response_receivers: Arc<Mutex<HashMap<String, oneshot::Receiver<ProtocolMessage>>>>,
    notify: Arc<Notify>,
}

impl IntegratedSignal {
    async fn wait_for_file_offer(
        &self,
        offer_id: &str,
        occurrence: usize,
    ) -> (DeviceId, ProtocolMessage) {
        loop {
            let matches = self
                .sent_messages
                .lock()
                .expect("lock sent messages")
                .iter()
                .filter(|(_, message)| {
                    matches!(message, ProtocolMessage::FileOffer { id, .. } if id == offer_id)
                })
                .cloned()
                .collect::<Vec<_>>();
            if matches.len() >= occurrence {
                return matches[occurrence - 1].clone();
            }
            self.notify.notified().await;
        }
    }

    fn snapshot(&self) -> Vec<(DeviceId, ProtocolMessage)> {
        self.sent_messages
            .lock()
            .expect("lock sent messages")
            .clone()
    }

    async fn wait_for_matching_message<P>(&self, predicate: P) -> (DeviceId, ProtocolMessage)
    where
        P: Fn(&ProtocolMessage) -> bool,
    {
        loop {
            if let Some(message) = self
                .sent_messages
                .lock()
                .expect("lock sent messages")
                .iter()
                .find(|(_, message)| predicate(message))
                .cloned()
            {
                return message;
            }
            self.notify.notified().await;
        }
    }

    fn respond_receiver(&self, response_id: &str, response: ProtocolMessage) {
        let sender = self
            .receiver_response_senders
            .lock()
            .expect("lock receiver response senders")
            .remove(response_id)
            .expect("receiver response sender for id");
        sender.send(response).expect("deliver receiver response");
    }
}

impl FileSenderSignal for IntegratedSignal {
    async fn send_message(
        &self,
        peer_id: &DeviceId,
        message: ProtocolMessage,
    ) -> Result<(), FileSenderError> {
        match &message {
            ProtocolMessage::FileOffer { id, .. } => {
                let (tx, rx) = oneshot::channel();
                self.sender_response_senders
                    .lock()
                    .expect("lock sender response senders")
                    .insert(id.clone(), tx);
                self.sender_response_receivers
                    .lock()
                    .expect("lock sender response receivers")
                    .insert(id.clone(), rx);
            }
            ProtocolMessage::FileResumeAccept { offer_id, .. } => {
                if let Some(sender) = self
                    .receiver_response_senders
                    .lock()
                    .expect("lock receiver response senders")
                    .remove(offer_id)
                {
                    sender.send(message.clone()).expect("deliver resume accept");
                }
            }
            ProtocolMessage::FileAccept { offer_id }
            | ProtocolMessage::FileReject { offer_id, .. }
            | ProtocolMessage::FileResumeRequest { offer_id, .. } => {
                if let Some(sender) = self
                    .sender_response_senders
                    .lock()
                    .expect("lock sender response senders")
                    .remove(offer_id)
                {
                    sender
                        .send(message.clone())
                        .expect("deliver sender response");
                }
            }
            _ => {}
        }

        self.sent_messages
            .lock()
            .expect("lock sent messages")
            .push((peer_id.clone(), message));
        self.notify.notify_waiters();
        Ok(())
    }

    async fn wait_for_response(&self, offer_id: &str) -> Result<ProtocolMessage, FileSenderError> {
        let receiver = self
            .sender_response_receivers
            .lock()
            .expect("lock sender response receivers")
            .remove(offer_id)
            .expect("sender response receiver for offer");
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

impl FileReceiverSignal for IntegratedSignal {
    async fn send_message(
        &self,
        peer_id: &DeviceId,
        message: ProtocolMessage,
    ) -> Result<(), FileReceiverError> {
        if let Some(response_id) = match &message {
            ProtocolMessage::FileResumeRequest { offer_id, .. } => Some(offer_id.clone()),
            ProtocolMessage::FolderResumeRequest { folder_id, .. } => Some(folder_id.clone()),
            _ => None,
        } {
            let (tx, rx) = oneshot::channel();
            self.receiver_response_senders
                .lock()
                .expect("lock receiver response senders")
                .insert(response_id.clone(), tx);
            self.receiver_response_receivers
                .lock()
                .expect("lock receiver response receivers")
                .insert(response_id, rx);
        }

        match &message {
            ProtocolMessage::FileAccept { offer_id }
            | ProtocolMessage::FileReject { offer_id, .. }
            | ProtocolMessage::FileResumeRequest { offer_id, .. } => {
                if let Some(sender) = self
                    .sender_response_senders
                    .lock()
                    .expect("lock sender response senders")
                    .remove(offer_id)
                {
                    sender
                        .send(message.clone())
                        .expect("deliver sender response");
                }
            }
            _ => {}
        }

        self.sent_messages
            .lock()
            .expect("lock sent messages")
            .push((peer_id.clone(), message));
        self.notify.notify_waiters();
        Ok(())
    }

    async fn wait_for_response(
        &self,
        offer_id: &str,
    ) -> Result<ProtocolMessage, FileReceiverError> {
        let receiver = self
            .receiver_response_receivers
            .lock()
            .expect("lock receiver response receivers")
            .remove(offer_id)
            .expect("receiver response receiver for offer");
        receiver
            .await
            .map_err(|_| FileReceiverError::Signal("response channel closed".to_string()))
    }

    async fn file_crypto_material(
        &self,
        _peer_id: &DeviceId,
        _file_id: &str,
    ) -> Result<jasmine_transfer::FileCryptoMaterial, FileReceiverError> {
        Ok((test_file_key(), test_nonce_prefix()))
    }
}

struct FixedDiskSpaceChecker;

impl DiskSpaceChecker for FixedDiskSpaceChecker {
    fn available_space(&self, _path: &Path) -> io::Result<u64> {
        Ok(u64::MAX / 2)
    }
}

struct CancelAtThresholdProgress {
    threshold: u64,
    cancellation: CancellationToken,
}

impl TransferProgressReporter for CancelAtThresholdProgress {
    fn report(&self, progress: TransferProgress) {
        if progress.bytes_sent >= self.threshold {
            self.cancellation.cancel();
        }
    }
}

struct WorkerControl {
    sender: Mutex<Option<oneshot::Sender<()>>>,
    receiver: Mutex<Option<oneshot::Receiver<()>>>,
}

impl WorkerControl {
    fn new() -> Self {
        let (tx, rx) = oneshot::channel();
        Self {
            sender: Mutex::new(Some(tx)),
            receiver: Mutex::new(Some(rx)),
        }
    }

    fn take_receiver(&self) -> oneshot::Receiver<()> {
        self.receiver
            .lock()
            .expect("lock worker receiver")
            .take()
            .expect("worker receiver available once")
    }

    fn complete(&self) {
        if let Some(sender) = self.sender.lock().expect("lock worker sender").take() {
            let _ = sender.send(());
        }
    }
}

#[derive(Default)]
struct LimitedDiskSpaceChecker {
    available_bytes: u64,
}

impl LimitedDiskSpaceChecker {
    fn with_available_bytes(available_bytes: u64) -> Self {
        Self { available_bytes }
    }
}

impl DiskSpaceChecker for LimitedDiskSpaceChecker {
    fn available_space(&self, _path: &Path) -> io::Result<u64> {
        Ok(self.available_bytes)
    }
}

#[derive(Default)]
struct OfferCapture {
    offers: Arc<Mutex<Vec<FileOfferNotification>>>,
    notify: Arc<Notify>,
}

impl OfferCapture {
    async fn wait_for_offer(&self, offer_id: &str) -> FileOfferNotification {
        loop {
            if let Some(offer) = self
                .offers
                .lock()
                .expect("lock offer capture")
                .iter()
                .find(|offer| offer.offer_id == offer_id)
                .cloned()
            {
                return offer;
            }
            self.notify.notified().await;
        }
    }
}

impl FileOfferNotifier for OfferCapture {
    fn offer_received(&self, offer: FileOfferNotification) {
        self.offers.lock().expect("lock offer capture").push(offer);
        self.notify.notify_waiters();
    }
}

#[derive(Clone, Default)]
struct DummySender;

impl ManagedTransferSender for DummySender {
    async fn send_file(
        &self,
        _transfer_id: &str,
        _peer_id: DeviceId,
        _file_path: PathBuf,
        _cancellation: CancellationToken,
        _progress: Option<Arc<dyn TransferProgressReporter>>,
    ) -> Result<(), TransferManagerError> {
        Err(TransferManagerError::Worker(
            "dummy sender should not be used in receive integration".to_string(),
        ))
    }

    async fn current_resume_token(
        &self,
        _peer_id: &DeviceId,
        _file_id: &str,
    ) -> Result<Option<String>, TransferManagerError> {
        Ok(Some("mock-transfer-resume-token".to_string()))
    }
}

#[derive(Clone)]
struct IntegrationMockReceiver {
    download_dir: PathBuf,
    pending_offers: Arc<Mutex<HashMap<String, PendingOfferState>>>,
    resume_offsets: Arc<Mutex<Vec<ResumeOffsetCall>>>,
    current_resume_token: Arc<Mutex<Option<String>>>,
    controls: Arc<Mutex<HashMap<String, Arc<WorkerControl>>>>,
    notify: Arc<Notify>,
}

#[derive(Clone)]
struct PendingOfferState {
    filename: String,
    size: u64,
    download_dir: PathBuf,
}

type ResumeOffsetCall = (String, Option<u64>);

impl IntegrationMockReceiver {
    fn new(download_dir: PathBuf) -> Self {
        Self {
            download_dir,
            pending_offers: Arc::new(Mutex::new(HashMap::new())),
            resume_offsets: Arc::new(Mutex::new(Vec::new())),
            current_resume_token: Arc::new(Mutex::new(Some(
                "mock-transfer-resume-token".to_string(),
            ))),
            controls: Arc::new(Mutex::new(HashMap::new())),
            notify: Arc::new(Notify::new()),
        }
    }

    async fn wait_started(&self, transfer_id: &str) {
        loop {
            if self
                .resume_offsets
                .lock()
                .expect("lock receiver resume offsets")
                .iter()
                .any(|(id, _)| id == transfer_id)
            {
                return;
            }
            self.notify.notified().await;
        }
    }

    fn resume_offsets(&self) -> Vec<(String, Option<u64>)> {
        self.resume_offsets
            .lock()
            .expect("lock receiver resume offsets")
            .clone()
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

impl ManagedTransferReceiver for IntegrationMockReceiver {
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
                self.pending_offers
                    .lock()
                    .expect("lock receiver offers")
                    .insert(
                        id.clone(),
                        PendingOfferState {
                            filename: filename.clone(),
                            size,
                            download_dir: self.download_dir.clone(),
                        },
                    );

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
        self.notify.notify_waiters();

        if let Some(progress) = progress.as_ref() {
            progress.report(TransferProgress {
                transfer_id: offer_id.to_string(),
                bytes_sent: pending.size.min(1),
                total_bytes: pending.size,
                speed_bps: 11,
            });
        }

        tokio::select! {
            _ = cancellation.cancelled() => Err(TransferManagerError::Cancelled),
            outcome = receiver => outcome.map_err(|_| TransferManagerError::Worker("mock receiver channel closed".to_string())).map(|_| final_path),
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
        _offer_id: &str,
        _reason: Option<String>,
    ) -> Result<(), TransferManagerError> {
        Ok(())
    }
}

fn transfer_record(
    id: Uuid,
    peer_id: DeviceId,
    file_name: &str,
    local_path: PathBuf,
    bytes_transferred: u64,
    resume_token: Option<String>,
) -> TransferRecord {
    TransferRecord {
        id,
        peer_id,
        chat_id: None,
        file_name: file_name.to_string(),
        local_path,
        status: TransferStatus::Failed,
        thumbnail_path: None,
        folder_id: None,
        folder_relative_path: None,
        bytes_transferred,
        resume_token,
    }
}

fn partial_path_for(final_path: &Path) -> PathBuf {
    let mut partial_name = final_path
        .file_name()
        .expect("path must have file name")
        .to_os_string();
    partial_name.push(".jasmine-partial");
    final_path.with_file_name(partial_name)
}

fn normalize_relative_path(root: &Path, file_path: &Path) -> String {
    file_path
        .strip_prefix(root)
        .expect("file should stay under root")
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn folder_file_transfer_id(folder_transfer_id: &str, relative_path: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(folder_transfer_id.as_bytes());
    hasher.update([0]);
    hasher.update(relative_path.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).to_string()
}

async fn load_folder_file_statuses(
    db_path: &Path,
    folder_transfer_id: &str,
) -> HashMap<String, (String, u64)> {
    SqliteStorage::open(db_path)
        .expect("open folder status db")
        .get_folder_file_statuses(folder_transfer_id)
        .await
        .expect("load folder file statuses")
        .into_iter()
        .map(|(file_path, status, bytes_transferred)| (file_path, (status, bytes_transferred)))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn encrypted_integration_offer_notification_rejects_disk_space_limited_offer() {
    let temp = tempdir().expect("tempdir");
    let source_payload = vec![0x5a; 1024];
    let source = write_bytes(&temp.path().join("big-transfer.bin"), &source_payload);
    let download_dir = temp.path().join("downloads");
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_download_dir(&settings, &download_dir);
    let available_bytes = 500_u64;

    let signal = IntegratedSignal::default();
    let sender = Arc::new(FileSender::with_config(signal.clone(), sender_config()));
    let offer_capture = Arc::new(OfferCapture::default());
    let receiver = Arc::new(FileReceiver::with_dependencies(
        signal.clone(),
        settings,
        receiver_config(),
        Arc::new(LimitedDiskSpaceChecker::with_available_bytes(
            available_bytes,
        )),
        Some(offer_capture.clone()),
    ));

    let offer_id = Uuid::new_v4().to_string();
    let sender_peer = peer_id();
    let send_offer_id = offer_id.clone();

    let send_task = tokio::spawn({
        let source = source.clone();
        let sender = Arc::clone(&sender);
        let sender_peer = sender_peer.clone();
        let send_offer_id = send_offer_id.clone();
        async move {
            sender
                .send_file_with_id(
                    send_offer_id,
                    sender_peer,
                    source,
                    CancellationToken::new(),
                    None,
                )
                .await
        }
    });

    let (_, offer_message) = signal.wait_for_file_offer(&offer_id, 1).await;
    receiver
        .receive_offer(
            sender_peer.clone(),
            SocketAddr::from(([127, 0, 0, 1], 9735)),
            offer_message,
        )
        .await
        .expect("receive limited-space offer");

    let offer = offer_capture.wait_for_offer(&offer_id).await;
    assert_eq!(offer.offer_id, offer_id);
    assert_eq!(offer.sender_id, sender_peer);
    assert_eq!(offer.filename, "big-transfer.bin");
    assert_eq!(offer.size, source_payload.len() as u64);
    assert!(!offer.has_enough_space);
    assert_eq!(offer.available_space_bytes, Some(available_bytes));
    assert!(offer.required_space_bytes > available_bytes);

    let reject_result = receiver.accept_offer(&offer_id, None).await;
    assert!(matches!(
        reject_result,
        Err(FileReceiverError::InsufficientDiskSpace {
            required_bytes,
            ..
        }) if required_bytes > available_bytes
    ));

    let final_result = send_task
        .await
        .expect("sender task join")
        .expect_err("sender should see reject");
    assert!(matches!(
        final_result,
        FileSenderError::Rejected(reason) if reason == "insufficient disk space"
    ));

    let messages = signal.snapshot();
    assert!(messages.iter().any(|(_, message)| matches!(
        message,
        ProtocolMessage::FileReject {
            offer_id: id,
            reason: Some(reason),
        } if id == &offer_id && reason == "insufficient disk space"
    )));

    let partial_path = download_dir.join("big-transfer.bin.jasmine-partial");
    let final_path = download_dir.join("big-transfer.bin");
    assert!(!partial_path.exists());
    assert!(!final_path.exists());
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
                .expect("lock sent messages")
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
            .expect("lock response senders")
            .remove(response_id)
            .expect("response sender for id");
        sender.send(response).expect("deliver mocked response");
    }

    async fn wait_for_matching_message<P>(&self, predicate: P) -> (DeviceId, ProtocolMessage)
    where
        P: Fn(&ProtocolMessage) -> bool,
    {
        loop {
            if let Some(message) = self
                .sent_messages
                .lock()
                .expect("lock sent messages")
                .iter()
                .find(|(_, message)| predicate(message))
                .cloned()
            {
                return message;
            }
            self.notify.notified().await;
        }
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
                .expect("lock response senders")
                .insert(response_id.clone(), tx);
            self.response_receivers
                .lock()
                .expect("lock response receivers")
                .insert(response_id, rx);
        }

        self.sent_messages
            .lock()
            .expect("lock sent messages")
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
            .expect("lock response receivers")
            .remove(response_id)
            .expect("response receiver for id");
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

#[derive(Clone)]
struct MockFolderFileSender {
    root: PathBuf,
    started_paths: Arc<Mutex<Vec<String>>>,
}

impl MockFolderFileSender {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            started_paths: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn started_paths(&self) -> Vec<String> {
        self.started_paths
            .lock()
            .expect("lock started paths")
            .clone()
    }
}

impl FolderFileTransferSender for MockFolderFileSender {
    async fn send_file_with_id(
        &self,
        _transfer_id: String,
        _peer_id: DeviceId,
        file_path: PathBuf,
        _cancellation: CancellationToken,
        progress: Option<Arc<dyn TransferProgressReporter>>,
    ) -> Result<(), FileSenderError> {
        let relative_path = normalize_relative_path(&self.root, &file_path);
        let total_bytes = std::fs::metadata(&file_path)
            .expect("read mock file metadata")
            .len();
        self.started_paths
            .lock()
            .expect("lock started paths")
            .push(relative_path.clone());

        if let Some(progress) = progress {
            progress.report(TransferProgress {
                transfer_id: relative_path,
                bytes_sent: total_bytes,
                total_bytes,
                speed_bps: 1,
            });
        }
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn encrypted_integration_single_file_roundtrip_matches_plaintext_sha256() {
    let temp = tempdir().expect("tempdir");
    let source = write_bytes(&temp.path().join("source.bin"), &vec![0x5a; 128 * 1024]);
    let download_dir = temp.path().join("downloads");
    let signal = IntegratedSignal::default();
    let sender = Arc::new(FileSender::with_config(signal.clone(), sender_config()));
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_download_dir(&settings, &download_dir);
    let receiver = Arc::new(FileReceiver::with_dependencies(
        signal.clone(),
        settings,
        receiver_config(),
        Arc::new(FixedDiskSpaceChecker),
        None::<Arc<dyn FileOfferNotifier>>,
    ));
    let offer_id = Uuid::new_v4().to_string();
    let sender_peer = peer_id();

    let send_task = tokio::spawn({
        let source = source.clone();
        let sender = Arc::clone(&sender);
        let sender_peer = sender_peer.clone();
        let offer_id = offer_id.clone();
        async move {
            sender
                .send_file_with_id(
                    offer_id,
                    sender_peer,
                    source,
                    CancellationToken::new(),
                    None,
                )
                .await
        }
    });

    let (_, offer_message) = signal.wait_for_file_offer(&offer_id, 1).await;
    receiver
        .receive_offer(
            sender_peer.clone(),
            SocketAddr::from(([127, 0, 0, 1], 9735)),
            offer_message,
        )
        .await
        .expect("receive file offer");

    let receive_task = tokio::spawn({
        let receiver = Arc::clone(&receiver);
        let offer_id = offer_id.clone();
        async move { receiver.accept_offer(&offer_id, None).await }
    });

    send_task
        .await
        .expect("sender task join")
        .expect("send file succeeds");
    let saved_path = receive_task
        .await
        .expect("receiver task join")
        .expect("receive file succeeds");

    let source_bytes = std::fs::read(&source).expect("read source bytes");
    let saved_bytes = std::fs::read(&saved_path).expect("read received bytes");
    assert_eq!(saved_path, download_dir.join("source.bin"));
    assert_eq!(saved_bytes, source_bytes);
    assert_eq!(sha256_hex(&saved_bytes), sha256_hex(&source_bytes));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn encrypted_integration_resume_roundtrip_matches_plaintext_sha256() {
    let temp = tempdir().expect("tempdir");
    let payload = vec![0x7c; 1024 * 1024];
    let source = write_bytes(&temp.path().join("resume-source.bin"), &payload);
    let download_dir = temp.path().join("downloads");
    let signal = IntegratedSignal::default();
    let sender = Arc::new(FileSender::with_config(signal.clone(), sender_config()));
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_download_dir(&settings, &download_dir);
    let receiver = Arc::new(FileReceiver::with_dependencies(
        signal.clone(),
        settings,
        receiver_config(),
        Arc::new(FixedDiskSpaceChecker),
        None::<Arc<dyn FileOfferNotifier>>,
    ));
    let offer_id = Uuid::new_v4().to_string();
    let sender_peer = peer_id();
    let resume_offset = 8 * DEFAULT_CHUNK_SIZE as u64;

    let first_send_task = tokio::spawn({
        let source = source.clone();
        let sender = Arc::clone(&sender);
        let sender_peer = sender_peer.clone();
        let offer_id = offer_id.clone();
        async move {
            sender
                .send_file_with_id(
                    offer_id,
                    sender_peer,
                    source,
                    CancellationToken::new(),
                    None,
                )
                .await
        }
    });

    let (_, first_offer) = signal.wait_for_file_offer(&offer_id, 1).await;
    receiver
        .receive_offer(
            sender_peer.clone(),
            SocketAddr::from(([127, 0, 0, 1], 9735)),
            first_offer,
        )
        .await
        .expect("receive initial file offer");

    let cancellation = CancellationToken::new();
    let first_receive_task = tokio::spawn({
        let receiver = Arc::clone(&receiver);
        let offer_id = offer_id.clone();
        let cancellation = cancellation.clone();
        async move {
            receiver
                .accept_offer_with_cancellation(
                    &offer_id,
                    cancellation.clone(),
                    Some(Arc::new(CancelAtThresholdProgress {
                        threshold: resume_offset,
                        cancellation,
                    })),
                )
                .await
        }
    });

    let first_receive_result = first_receive_task.await.expect("first receiver task join");
    assert!(matches!(
        first_receive_result,
        Err(FileReceiverError::Cancelled)
    ));
    let _ = first_send_task.await.expect("first sender task join");

    let partial_path = partial_path_for(&download_dir.join("resume-source.bin"));
    assert!(partial_path.exists());
    let actual_resume_offset = std::fs::metadata(&partial_path)
        .expect("partial metadata")
        .len();
    assert!(
        actual_resume_offset >= resume_offset,
        "persisted partial offset should be at least the cancellation threshold"
    );
    assert_eq!(
        actual_resume_offset % DEFAULT_CHUNK_SIZE as u64,
        0,
        "persisted partial offset must stay chunk-aligned for encrypted resume"
    );
    assert!(
        actual_resume_offset < payload.len() as u64,
        "persisted partial offset should still represent an incomplete transfer"
    );

    let second_send_task = tokio::spawn({
        let source = source.clone();
        let sender = Arc::clone(&sender);
        let sender_peer = sender_peer.clone();
        let offer_id = offer_id.clone();
        async move {
            sender
                .send_file_with_id(
                    offer_id,
                    sender_peer,
                    source,
                    CancellationToken::new(),
                    None,
                )
                .await
        }
    });

    let (_, second_offer) = signal.wait_for_file_offer(&offer_id, 2).await;
    receiver
        .receive_offer(
            sender_peer.clone(),
            SocketAddr::from(([127, 0, 0, 1], 9735)),
            second_offer,
        )
        .await
        .expect("receive resume file offer");

    let second_receive_task = tokio::spawn({
        let receiver = Arc::clone(&receiver);
        let offer_id = offer_id.clone();
        async move {
            receiver
                .accept_offer_with_resume_offset(&offer_id, Some(actual_resume_offset), None)
                .await
        }
    });

    second_send_task
        .await
        .expect("second sender task join")
        .expect("resume sender succeeds");
    let saved_path = second_receive_task
        .await
        .expect("second receiver task join")
        .expect("resume receiver succeeds");

    let saved_bytes = std::fs::read(&saved_path).expect("read resumed bytes");
    assert_eq!(saved_path, download_dir.join("resume-source.bin"));
    assert_eq!(saved_bytes, payload);
    assert_eq!(sha256_hex(&saved_bytes), sha256_hex(&payload));

    let snapshot = signal.snapshot();
    assert!(snapshot.iter().any(|(_, message)| {
        matches!(message, ProtocolMessage::FileResumeRequest { offer_id: id, offset } if id == &offer_id && *offset == actual_resume_offset)
    }));
    assert!(snapshot.iter().any(|(_, message)| {
        matches!(message, ProtocolMessage::FileResumeAccept { offer_id: id, offset } if id == &offer_id && *offset == actual_resume_offset)
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn encrypted_integration_epoch_mismatch_forces_restart_from_zero() {
    let temp = tempdir().expect("tempdir");
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_download_dir(&settings, &temp.path().join("downloads"));
    let sender = Arc::new(DummySender);
    let receiver = Arc::new(IntegrationMockReceiver::new(temp.path().join("downloads")));
    receiver.set_current_resume_token(Some("rotated-transfer-resume-token".to_string()));
    let storage = Arc::new(
        SqliteStorage::open(temp.path().join("messages.sqlite3")).expect("open sqlite storage"),
    );
    let manager = TransferManager::new(sender, receiver.clone(), settings, storage.clone())
        .expect("create transfer manager");

    let offer_id = Uuid::new_v4();
    let resume_offset = (2 * DEFAULT_CHUNK_SIZE) as u64;
    let final_path = temp.path().join("downloads").join("epoch.bin");
    let partial_path = partial_path_for(&final_path);
    std::fs::create_dir_all(final_path.parent().expect("download dir"))
        .expect("create download dir");
    std::fs::write(&partial_path, vec![0x44; resume_offset as usize]).expect("write stale partial");

    storage
        .save_transfer(&transfer_record(
            offer_id,
            peer_id(),
            "epoch.bin",
            final_path.clone(),
            resume_offset,
            Some("mock-transfer-resume-token".to_string()),
        ))
        .await
        .expect("seed failed resumable transfer");

    manager
        .handle_signal_message(
            peer_id(),
            SocketAddr::from(([127, 0, 0, 1], 9735)),
            ProtocolMessage::FileOffer {
                id: offer_id.to_string(),
                filename: "epoch.bin".to_string(),
                size: 4 * DEFAULT_CHUNK_SIZE as u64,
                sha256: "ignored".to_string(),
                transfer_port: 4047,
            },
        )
        .await
        .expect("handle resumable encrypted offer");

    manager
        .accept_offer(&offer_id.to_string(), None)
        .await
        .expect("accept epoch-mismatched offer");
    receiver.wait_started(&offer_id.to_string()).await;

    assert!(receiver
        .resume_offsets()
        .contains(&(offer_id.to_string(), None)));
    assert!(!partial_path.exists());

    let stored = storage
        .get_transfers(10, 0)
        .await
        .expect("load stored transfers")
        .into_iter()
        .find(|record| record.id == offer_id)
        .expect("stored transfer exists");
    assert_eq!(stored.bytes_transferred, 0);
    assert_eq!(
        stored.resume_token.as_deref(),
        Some("rotated-transfer-resume-token")
    );

    receiver.complete(&offer_id.to_string());
    time::timeout(Duration::from_secs(2), async {
        loop {
            if manager
                .transfer(&offer_id.to_string())
                .map(|transfer| transfer.status == TransferStatus::Completed)
                .unwrap_or(false)
            {
                return;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("manager should finish restarted transfer");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn encrypted_integration_folder_receive_persists_sqlite_status_and_resumes_from_db() {
    let temp = tempdir().expect("tempdir");
    let source_folder = temp.path().join("resume-db-folder");
    let completed_bytes = b"done";
    let partial_payload = vec![0x5b; 5 * DEFAULT_CHUNK_SIZE];
    write_bytes(&source_folder.join("done.txt"), &completed_bytes[..]);
    write_bytes(&source_folder.join("resume.bin"), &partial_payload);

    let manifest = generate_folder_manifest(&source_folder, DEFAULT_FOLDER_MANIFEST_MAX_FILES)
        .expect("generate folder manifest");
    let folder_transfer_id = Uuid::new_v4().to_string();
    let done_transfer_id = folder_file_transfer_id(&folder_transfer_id, "done.txt");
    let partial_transfer_id = folder_file_transfer_id(&folder_transfer_id, "resume.bin");

    let app_data_dir = temp.path().join("app-data");
    let download_dir = temp.path().join("downloads");
    let settings = SettingsService::new(&app_data_dir);
    save_download_dir(&settings, &download_dir);
    let db_path = app_data_dir.join("jasmine.db");

    let signal = IntegratedSignal::default();
    let sender = Arc::new(FileSender::with_config(signal.clone(), sender_config()));
    let folder_receiver = Arc::new(FolderReceiver::with_config(
        signal.clone(),
        settings,
        receiver_config(),
    ));
    let sender_peer = peer_id();
    let sender_address = SocketAddr::from(([127, 0, 0, 1], 9735));

    folder_receiver
        .handle_signal_message(
            sender_peer.clone(),
            sender_address,
            ProtocolMessage::FolderManifest {
                folder_transfer_id: folder_transfer_id.clone(),
                manifest: manifest.clone(),
                sender_id: sender_peer.0.to_string(),
            },
        )
        .await
        .expect("queue initial folder manifest");

    let first_receive_task = tokio::spawn({
        let folder_receiver = Arc::clone(&folder_receiver);
        let download_dir = download_dir.clone();
        let folder_transfer_id = folder_transfer_id.clone();
        async move {
            folder_receiver
                .accept_offer(&folder_transfer_id, download_dir, None)
                .await
        }
    });

    time::timeout(
        Duration::from_secs(10),
        signal.wait_for_matching_message(|message| {
            matches!(
                message,
                ProtocolMessage::FolderAccept {
                    folder_transfer_id: id,
                } if id == &folder_transfer_id
            )
        }),
    )
    .await
    .expect("receive initial folder accept");

    let done_send_task = tokio::spawn({
        let sender = Arc::clone(&sender);
        let sender_peer = sender_peer.clone();
        let path = source_folder.join("done.txt");
        let transfer_id = done_transfer_id.clone();
        async move {
            sender
                .send_file_with_id(
                    transfer_id,
                    sender_peer,
                    path,
                    CancellationToken::new(),
                    None,
                )
                .await
        }
    });
    let (_, done_offer) = time::timeout(
        Duration::from_secs(10),
        signal.wait_for_file_offer(&done_transfer_id, 1),
    )
    .await
    .expect("receive completed file offer");
    folder_receiver
        .handle_signal_message(sender_peer.clone(), sender_address, done_offer)
        .await
        .expect("route completed file offer");
    time::timeout(Duration::from_secs(10), done_send_task)
        .await
        .expect("completed sender task finishes")
        .expect("join completed sender task")
        .expect("completed sender succeeds");

    let partial_send_cancellation = CancellationToken::new();
    let partial_send_task = tokio::spawn({
        let sender = Arc::clone(&sender);
        let sender_peer = sender_peer.clone();
        let path = source_folder.join("resume.bin");
        let transfer_id = partial_transfer_id.clone();
        let partial_send_cancellation = partial_send_cancellation.clone();
        async move {
            sender
                .send_file_with_id(
                    transfer_id,
                    sender_peer,
                    path,
                    partial_send_cancellation.clone(),
                    Some(Arc::new(CancelAtThresholdProgress {
                        threshold: (2 * DEFAULT_CHUNK_SIZE) as u64,
                        cancellation: partial_send_cancellation,
                    })),
                )
                .await
        }
    });
    let (_, partial_offer) = time::timeout(
        Duration::from_secs(10),
        signal.wait_for_file_offer(&partial_transfer_id, 1),
    )
    .await
    .expect("receive initial partial file offer");
    folder_receiver
        .handle_signal_message(sender_peer.clone(), sender_address, partial_offer)
        .await
        .expect("route partial file offer");

    let first_outcome = time::timeout(Duration::from_secs(10), first_receive_task)
        .await
        .expect("first folder receive task finishes")
        .expect("join first folder receive task")
        .expect("failed first folder receive returns outcome");
    assert_eq!(
        first_outcome.progress.status,
        FolderTransferStatus::PartiallyFailed
    );
    assert_eq!(
        first_outcome
            .file_results
            .iter()
            .map(|result| (result.relative_path.clone(), result.status.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("done.txt".to_string(), TransferStatus::Completed),
            ("resume.bin".to_string(), TransferStatus::Failed),
        ]
    );
    let partial_send_result = time::timeout(Duration::from_secs(10), partial_send_task)
        .await
        .expect("partial sender task finishes")
        .expect("join partial sender task");
    assert!(matches!(
        partial_send_result,
        Err(FileSenderError::Cancelled)
    ));

    let stored_after_cancel = load_folder_file_statuses(&db_path, &folder_transfer_id).await;
    assert_eq!(
        stored_after_cancel.get("done.txt"),
        Some(&(SqliteStorage::FOLDER_FILE_STATUS_COMPLETED.to_string(), 4))
    );
    let (partial_status, partial_offset) = stored_after_cancel
        .get("resume.bin")
        .expect("partial file status stored");
    assert_eq!(partial_status, SqliteStorage::FOLDER_FILE_STATUS_FAILED);
    assert_eq!(*partial_offset, (2 * DEFAULT_CHUNK_SIZE) as u64);

    folder_receiver
        .handle_signal_message(
            sender_peer.clone(),
            sender_address,
            ProtocolMessage::FolderManifest {
                folder_transfer_id: folder_transfer_id.clone(),
                manifest: manifest.clone(),
                sender_id: sender_peer.0.to_string(),
            },
        )
        .await
        .expect("queue resumed folder manifest");

    let second_receive_task = tokio::spawn({
        let folder_receiver = Arc::clone(&folder_receiver);
        let download_dir = download_dir.clone();
        let folder_transfer_id = folder_transfer_id.clone();
        async move {
            folder_receiver
                .accept_offer(&folder_transfer_id, download_dir, None)
                .await
        }
    });

    let (_, resume_request) = time::timeout(
        Duration::from_secs(10),
        signal.wait_for_matching_message(|message| {
            matches!(
                message,
                ProtocolMessage::FolderResumeRequest {
                    folder_id,
                    ..
                } if folder_id == &folder_transfer_id
            )
        }),
    )
    .await
    .expect("receive folder resume request");
    match resume_request {
        ProtocolMessage::FolderResumeRequest {
            folder_id,
            completed_files,
            partial_files,
        } => {
            assert_eq!(folder_id, folder_transfer_id);
            assert_eq!(completed_files, vec!["done.txt".to_string()]);
            assert_eq!(
                partial_files,
                vec![("resume.bin".to_string(), *partial_offset)]
            );
        }
        other => panic!("expected FolderResumeRequest, got {other:?}"),
    }

    signal.respond_receiver(
        &folder_transfer_id,
        ProtocolMessage::FolderResumeAccept {
            folder_id: folder_transfer_id.clone(),
            files_to_send: manifest
                .files
                .iter()
                .filter(|entry| entry.relative_path == "resume.bin")
                .cloned()
                .collect(),
        },
    );

    let resumed_send_task = tokio::spawn({
        let sender = Arc::clone(&sender);
        let sender_peer = sender_peer.clone();
        let path = source_folder.join("resume.bin");
        let transfer_id = partial_transfer_id.clone();
        async move {
            sender
                .send_file_with_id(
                    transfer_id,
                    sender_peer,
                    path,
                    CancellationToken::new(),
                    None,
                )
                .await
        }
    });
    let (_, resumed_offer) = time::timeout(
        Duration::from_secs(10),
        signal.wait_for_file_offer(&partial_transfer_id, 2),
    )
    .await
    .expect("receive resumed partial file offer");
    folder_receiver
        .handle_signal_message(sender_peer.clone(), sender_address, resumed_offer)
        .await
        .expect("route resumed partial offer");

    time::timeout(Duration::from_secs(10), resumed_send_task)
        .await
        .expect("resumed sender task finishes")
        .expect("join resumed sender task")
        .expect("resumed sender succeeds");

    let second_outcome = time::timeout(Duration::from_secs(10), second_receive_task)
        .await
        .expect("resumed folder receive task finishes")
        .expect("join resumed folder receive task")
        .expect("resumed folder receive succeeds");
    assert_eq!(
        second_outcome.progress.status,
        FolderTransferStatus::Completed
    );
    assert_eq!(second_outcome.progress.completed_files, 2);

    let saved_folder = download_dir.join(&manifest.folder_name);
    assert_eq!(
        std::fs::read(saved_folder.join("done.txt")).expect("read completed file"),
        completed_bytes
    );
    assert_eq!(
        std::fs::read(saved_folder.join("resume.bin")).expect("read resumed file"),
        partial_payload
    );

    let stored_after_resume = load_folder_file_statuses(&db_path, &folder_transfer_id).await;
    assert_eq!(
        stored_after_resume.get("done.txt"),
        Some(&(SqliteStorage::FOLDER_FILE_STATUS_COMPLETED.to_string(), 4))
    );
    assert_eq!(
        stored_after_resume.get("resume.bin"),
        Some(&(
            SqliteStorage::FOLDER_FILE_STATUS_COMPLETED.to_string(),
            partial_payload.len() as u64,
        ))
    );

    let snapshot = signal.snapshot();
    assert!(snapshot.iter().any(|(_, message)| {
        matches!(
            message,
            ProtocolMessage::FileResumeRequest {
                offer_id,
                offset,
            } if offer_id == &partial_transfer_id && *offset == *partial_offset
        )
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn encrypted_integration_folder_resume_skip_and_all_complete_behaviors() {
    let temp = tempdir().expect("tempdir");
    let folder = temp.path().join("resume-folder");
    write_bytes(&folder.join("a.txt"), &b"aaaa"[..]);
    write_bytes(&folder.join("b.txt"), &b"bbbb"[..]);
    write_bytes(&folder.join("c.txt"), &b"cccccccc"[..]);
    write_bytes(&folder.join("d.txt"), &b"dddd"[..]);

    let signal = MockFolderSignal::default();
    let file_sender = MockFolderFileSender::new(folder.clone());
    let coordinator = FolderTransferCoordinator::new(signal.clone(), file_sender.clone());

    let send_task = tokio::spawn(async move {
        coordinator
            .send_folder(peer_id(), peer_id(), folder, CancellationToken::new(), None)
            .await
    });

    let (_, manifest_message) = signal.wait_for_manifest().await;
    let folder_transfer_id = match manifest_message {
        ProtocolMessage::FolderManifest {
            folder_transfer_id, ..
        } => folder_transfer_id,
        other => panic!("expected FolderManifest, got {other:?}"),
    };

    signal.respond(
        &folder_transfer_id,
        ProtocolMessage::FolderResumeRequest {
            folder_id: folder_transfer_id.clone(),
            completed_files: vec!["a.txt".to_string()],
            partial_files: vec![("b.txt".to_string(), 2)],
        },
    );

    let (_, resume_accept_message) = signal
        .wait_for_matching_message(|message| {
            matches!(message, ProtocolMessage::FolderResumeAccept { .. })
        })
        .await;
    match resume_accept_message {
        ProtocolMessage::FolderResumeAccept {
            folder_id,
            files_to_send,
        } => {
            assert_eq!(folder_id, folder_transfer_id);
            assert_eq!(
                files_to_send
                    .iter()
                    .map(|entry| entry.relative_path.clone())
                    .collect::<Vec<_>>(),
                vec![
                    "b.txt".to_string(),
                    "c.txt".to_string(),
                    "d.txt".to_string()
                ]
            );
        }
        other => panic!("expected FolderResumeAccept, got {other:?}"),
    }

    let outcome = send_task
        .await
        .expect("join folder integration task")
        .expect("folder integration succeeds");

    assert_eq!(
        file_sender.started_paths(),
        vec![
            "b.txt".to_string(),
            "c.txt".to_string(),
            "d.txt".to_string()
        ]
    );
    assert_eq!(outcome.progress.completed_files, 4);

    let noop_folder = temp.path().join("resume-noop");
    write_bytes(&noop_folder.join("a.txt"), &b"aaaa"[..]);
    write_bytes(&noop_folder.join("b.txt"), &b"bbbb"[..]);
    let noop_signal = MockFolderSignal::default();
    let noop_sender = MockFolderFileSender::new(noop_folder.clone());
    let noop_coordinator = FolderTransferCoordinator::new(noop_signal.clone(), noop_sender.clone());

    let noop_task = tokio::spawn(async move {
        noop_coordinator
            .send_folder(
                peer_id(),
                peer_id(),
                noop_folder,
                CancellationToken::new(),
                None,
            )
            .await
    });

    let (_, manifest_message) = noop_signal.wait_for_manifest().await;
    let noop_transfer_id = match manifest_message {
        ProtocolMessage::FolderManifest {
            folder_transfer_id, ..
        } => folder_transfer_id,
        other => panic!("expected FolderManifest, got {other:?}"),
    };

    noop_signal.respond(
        &noop_transfer_id,
        ProtocolMessage::FolderResumeRequest {
            folder_id: noop_transfer_id.clone(),
            completed_files: vec!["a.txt".to_string(), "b.txt".to_string()],
            partial_files: Vec::new(),
        },
    );

    let (_, noop_resume_accept) = noop_signal
        .wait_for_matching_message(|message| {
            matches!(message, ProtocolMessage::FolderResumeAccept { .. })
        })
        .await;
    match noop_resume_accept {
        ProtocolMessage::FolderResumeAccept {
            folder_id,
            files_to_send,
        } => {
            assert_eq!(folder_id, noop_transfer_id);
            assert!(files_to_send.is_empty());
        }
        other => panic!("expected FolderResumeAccept, got {other:?}"),
    }

    let noop_outcome = noop_task
        .await
        .expect("join folder noop integration task")
        .expect("folder noop integration succeeds");
    assert!(noop_sender.started_paths().is_empty());
    assert_eq!(noop_outcome.progress.completed_files, 2);
}
