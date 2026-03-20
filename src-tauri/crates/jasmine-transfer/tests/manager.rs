use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jasmine_core::{
    AppSettings, ChatId, CoreError, DeviceId, Message, MessageStatus, PeerInfo, ProtocolMessage,
    SettingsService, StorageEngine, TransferRecord, TransferStatus,
};
use jasmine_transfer::{
    FileOfferNotification, ManagedTransfer, ManagedTransferReceiver, ManagedTransferSender,
    TransferAggregateProgress, TransferDirection, TransferManager, TransferManagerError,
    TransferProgress, TransferProgressReporter,
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
}

#[derive(Clone)]
struct PendingOfferState {
    filename: String,
    size: u64,
    download_dir: PathBuf,
}

type RejectedOffer = (String, Option<String>);

#[derive(Clone)]
struct MockReceiver {
    download_dir: PathBuf,
    pending_offers: Arc<Mutex<HashMap<String, PendingOfferState>>>,
    started: Arc<Mutex<Vec<String>>>,
    rejected: Arc<Mutex<Vec<RejectedOffer>>>,
    controls: Arc<Mutex<HashMap<String, Arc<WorkerControl>>>>,
    notify: Arc<Notify>,
}

impl MockReceiver {
    fn new(download_dir: PathBuf) -> Self {
        Self {
            download_dir,
            pending_offers: Arc::new(Mutex::new(HashMap::new())),
            started: Arc::new(Mutex::new(Vec::new())),
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
}

impl StorageEngine for MockStorage {
    async fn save_message(&self, _message: &Message) -> Result<(), CoreError> {
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

    async fn update_message_status(
        &self,
        _msg_id: &Uuid,
        _status: MessageStatus,
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
