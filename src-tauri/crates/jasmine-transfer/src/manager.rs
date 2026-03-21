use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use jasmine_core::{
    ChatId, CoreError, DeviceId, ProtocolMessage, SettingsService, StorageEngine, TransferRecord,
    TransferStatus,
};
use thiserror::Error;
use tokio::{runtime::Handle, sync::Semaphore};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    generate_thumbnail, FileOfferNotification, FileReceiver, FileReceiverError, FileReceiverSignal,
    FileSender, FileSenderError, FileSenderSignal, TransferProgress, TransferProgressReporter,
};
use tracing::{debug, warn};

const THUMBNAIL_TASK_LIMIT: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    Send,
    Receive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedTransfer {
    pub id: String,
    pub peer_id: DeviceId,
    pub chat_id: Option<ChatId>,
    pub file_name: String,
    pub local_path: PathBuf,
    pub direction: TransferDirection,
    pub status: TransferStatus,
    pub total_bytes: u64,
    pub progress: Option<TransferProgress>,
    pub folder_id: Option<String>,
    pub folder_relative_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TransferAggregateProgress {
    pub active_transfers: usize,
    pub queued_transfers: usize,
    pub bytes_sent: u64,
    pub total_bytes: u64,
    pub speed_bps: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferManagerSnapshot {
    pub active_limit: usize,
    pub pending_offers: Vec<FileOfferNotification>,
    pub active_transfers: Vec<ManagedTransfer>,
    pub queued_transfers: Vec<ManagedTransfer>,
}

#[derive(Debug, Error)]
pub enum TransferManagerError {
    #[error("path does not have a valid file name")]
    InvalidFileName,
    #[error("unknown transfer offer: {0}")]
    UnknownOffer(String),
    #[error("unknown transfer: {0}")]
    UnknownTransfer(String),
    #[error("invalid transfer id: {0}")]
    InvalidTransferId(String),
    #[error("settings error: {0}")]
    Settings(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("transfer worker error: {0}")]
    Worker(String),
    #[error("file transfer was cancelled")]
    Cancelled,
}

pub trait ManagedTransferSender: Send + Sync + 'static {
    async fn send_file(
        &self,
        transfer_id: &str,
        peer_id: DeviceId,
        file_path: PathBuf,
        cancellation: CancellationToken,
        progress: Option<Arc<dyn TransferProgressReporter>>,
    ) -> Result<(), TransferManagerError>;
}

pub trait ManagedTransferReceiver: Send + Sync + 'static {
    async fn receive_offer(
        &self,
        sender_id: DeviceId,
        sender_address: SocketAddr,
        message: ProtocolMessage,
    ) -> Result<FileOfferNotification, TransferManagerError>;

    async fn accept_offer(
        &self,
        offer_id: &str,
        cancellation: CancellationToken,
        progress: Option<Arc<dyn TransferProgressReporter>>,
    ) -> Result<PathBuf, TransferManagerError>;

    async fn reject_offer(
        &self,
        offer_id: &str,
        reason: Option<String>,
    ) -> Result<(), TransferManagerError>;
}

impl<S> ManagedTransferSender for FileSender<S>
where
    S: FileSenderSignal + 'static,
{
    async fn send_file(
        &self,
        transfer_id: &str,
        peer_id: DeviceId,
        file_path: PathBuf,
        cancellation: CancellationToken,
        progress: Option<Arc<dyn TransferProgressReporter>>,
    ) -> Result<(), TransferManagerError> {
        self.send_file_with_id(
            transfer_id.to_string(),
            peer_id,
            file_path,
            cancellation,
            progress,
        )
        .await
        .map_err(TransferManagerError::from)
    }
}

impl<S> ManagedTransferReceiver for FileReceiver<S>
where
    S: FileReceiverSignal + 'static,
{
    async fn receive_offer(
        &self,
        sender_id: DeviceId,
        sender_address: SocketAddr,
        message: ProtocolMessage,
    ) -> Result<FileOfferNotification, TransferManagerError> {
        FileReceiver::receive_offer(self, sender_id, sender_address, message)
            .await
            .map_err(TransferManagerError::from)
    }

    async fn accept_offer(
        &self,
        offer_id: &str,
        cancellation: CancellationToken,
        progress: Option<Arc<dyn TransferProgressReporter>>,
    ) -> Result<PathBuf, TransferManagerError> {
        self.accept_offer_with_cancellation(offer_id, cancellation, progress)
            .await
            .map_err(TransferManagerError::from)
    }

    async fn reject_offer(
        &self,
        offer_id: &str,
        reason: Option<String>,
    ) -> Result<(), TransferManagerError> {
        FileReceiver::reject_offer(self, offer_id, reason)
            .await
            .map_err(TransferManagerError::from)
    }
}

pub struct TransferManager<S, R, St>
where
    S: ManagedTransferSender,
    R: ManagedTransferReceiver,
    St: StorageEngine + 'static,
{
    inner: Arc<TransferManagerInner<S, R, St>>,
}

impl<S, R, St> Clone for TransferManager<S, R, St>
where
    S: ManagedTransferSender,
    R: ManagedTransferReceiver,
    St: StorageEngine + 'static,
{
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<S, R, St> TransferManager<S, R, St>
where
    S: ManagedTransferSender,
    R: ManagedTransferReceiver,
    St: StorageEngine + 'static,
{
    pub fn new(
        sender: Arc<S>,
        receiver: Arc<R>,
        settings: SettingsService,
        storage: Arc<St>,
    ) -> Result<Self, TransferManagerError> {
        let active_limit = settings
            .load()
            .map_err(|error| TransferManagerError::Settings(error.to_string()))?
            .max_concurrent_transfers as usize;

        Ok(Self {
            inner: Arc::new(TransferManagerInner {
                sender,
                receiver,
                settings,
                storage,
                runtime: Handle::try_current().map_err(|error| {
                    TransferManagerError::Worker(format!(
                        "transfer manager requires an active tokio runtime: {error}"
                    ))
                })?,
                thumbnail_tasks: Arc::new(Semaphore::new(THUMBNAIL_TASK_LIMIT)),
                state: Mutex::new(TransferManagerState {
                    active_limit,
                    active_ids: VecDeque::new(),
                    queue: VecDeque::new(),
                    transfers: HashMap::new(),
                    pending_offers: HashMap::new(),
                }),
            }),
        })
    }

    pub async fn send_file(
        &self,
        peer_id: DeviceId,
        file_path: PathBuf,
        chat_id: Option<ChatId>,
    ) -> Result<String, TransferManagerError> {
        self.send_file_with_id_and_context(
            Uuid::new_v4().to_string(),
            peer_id,
            file_path,
            chat_id,
            None,
            None,
        )
        .await
    }

    pub(crate) async fn send_file_with_id_and_context(
        &self,
        transfer_id: String,
        peer_id: DeviceId,
        file_path: PathBuf,
        chat_id: Option<ChatId>,
        folder_link: Option<FolderTransferLink>,
        progress_reporter: Option<Arc<dyn TransferProgressReporter>>,
    ) -> Result<String, TransferManagerError> {
        let record_id = Uuid::parse_str(&transfer_id)
            .map_err(|_| TransferManagerError::InvalidTransferId(transfer_id.clone()))?;
        let file_name = file_name_from_path(&file_path)?;
        let total_bytes = std::fs::metadata(&file_path)
            .map_err(|error| TransferManagerError::Worker(error.to_string()))?
            .len();
        let folder_id = folder_link.as_ref().map(|link| link.folder_id.clone());
        let folder_relative_path = folder_link
            .as_ref()
            .map(|link| link.folder_relative_path.clone());

        let transfer = ManagedTransfer {
            id: transfer_id.clone(),
            peer_id: peer_id.clone(),
            chat_id,
            file_name,
            local_path: file_path.clone(),
            direction: TransferDirection::Send,
            status: TransferStatus::Pending,
            total_bytes,
            progress: None,
            folder_id,
            folder_relative_path,
        };

        self.inner.enqueue_transfer(ManagedTransferEntry {
            record_id,
            transfer,
            source: TransferSource::Send { peer_id, file_path },
            cancellation: CancellationToken::new(),
            progress_reporter,
        });
        self.inner.persist_transfer_async(&transfer_id).await?;
        self.inner.start_ready_transfers_async().await?;

        Ok(transfer_id)
    }

    pub async fn handle_signal_message(
        &self,
        sender_id: DeviceId,
        sender_address: SocketAddr,
        message: ProtocolMessage,
    ) -> Result<Option<FileOfferNotification>, TransferManagerError> {
        match message {
            offer @ ProtocolMessage::FileOffer { .. } => {
                let offer_id = match &offer {
                    ProtocolMessage::FileOffer { id, .. } => id.clone(),
                    _ => unreachable!(),
                };
                let notification = self
                    .inner
                    .receiver
                    .receive_offer(sender_id, sender_address, offer)
                    .await?;
                self.inner.remember_pending_offer(
                    offer_id,
                    PendingOfferEntry {
                        notification: notification.clone(),
                    },
                );
                Ok(Some(notification))
            }
            _ => Ok(None),
        }
    }

    pub async fn accept_offer(
        &self,
        offer_id: &str,
        chat_id: Option<ChatId>,
    ) -> Result<String, TransferManagerError> {
        let pending_offer = self
            .inner
            .take_pending_offer(offer_id)
            .ok_or_else(|| TransferManagerError::UnknownOffer(offer_id.to_string()))?;
        let record_id = Uuid::parse_str(offer_id)
            .map_err(|_| TransferManagerError::InvalidTransferId(offer_id.to_string()))?;
        let predicted_path = pending_offer
            .notification
            .download_dir
            .join(&pending_offer.notification.filename);
        let transfer = ManagedTransfer {
            id: offer_id.to_string(),
            peer_id: pending_offer.notification.sender_id.clone(),
            chat_id,
            file_name: pending_offer.notification.filename.clone(),
            local_path: predicted_path,
            direction: TransferDirection::Receive,
            status: TransferStatus::Pending,
            total_bytes: pending_offer.notification.size,
            progress: None,
            folder_id: None,
            folder_relative_path: None,
        };

        self.inner.enqueue_transfer(ManagedTransferEntry {
            record_id,
            transfer,
            source: TransferSource::Receive {
                offer_id: offer_id.to_string(),
            },
            cancellation: CancellationToken::new(),
            progress_reporter: None,
        });
        self.inner.persist_transfer_async(offer_id).await?;
        self.inner.start_ready_transfers_async().await?;

        Ok(offer_id.to_string())
    }

    pub async fn reject_offer(
        &self,
        offer_id: &str,
        reason: Option<String>,
    ) -> Result<(), TransferManagerError> {
        self.inner
            .take_pending_offer(offer_id)
            .ok_or_else(|| TransferManagerError::UnknownOffer(offer_id.to_string()))?;
        self.inner.receiver.reject_offer(offer_id, reason).await
    }

    pub async fn cancel_transfer(&self, transfer_id: &str) -> Result<(), TransferManagerError> {
        let cancellation = self
            .inner
            .prepare_cancel(transfer_id)
            .ok_or_else(|| TransferManagerError::UnknownTransfer(transfer_id.to_string()))?;

        if let Some(offer_id) = cancellation.reject_offer_id.as_ref() {
            self.inner
                .receiver
                .reject_offer(offer_id, Some("cancelled".to_string()))
                .await?;
        }

        if cancellation.cancel_active {
            cancellation.cancellation.cancel();
        }

        self.inner.persist_transfer_async(transfer_id).await?;
        self.inner.start_ready_transfers_async().await?;
        Ok(())
    }

    pub async fn on_settings_changed(&self) -> Result<(), TransferManagerError> {
        let active_limit = self
            .inner
            .settings
            .load()
            .map_err(|error| TransferManagerError::Settings(error.to_string()))?
            .max_concurrent_transfers as usize;
        self.inner.update_active_limit(active_limit);
        self.inner.start_ready_transfers_async().await
    }

    pub fn transfer(&self, transfer_id: &str) -> Option<ManagedTransfer> {
        self.inner.transfer(transfer_id)
    }

    pub fn snapshot(&self) -> TransferManagerSnapshot {
        self.inner.snapshot()
    }

    pub fn aggregate_progress(&self) -> TransferAggregateProgress {
        self.inner.aggregate_progress()
    }
}

struct TransferManagerInner<S, R, St>
where
    S: ManagedTransferSender,
    R: ManagedTransferReceiver,
    St: StorageEngine + 'static,
{
    sender: Arc<S>,
    receiver: Arc<R>,
    settings: SettingsService,
    storage: Arc<St>,
    runtime: Handle,
    thumbnail_tasks: Arc<Semaphore>,
    state: Mutex<TransferManagerState>,
}

struct TransferManagerState {
    active_limit: usize,
    active_ids: VecDeque<String>,
    queue: VecDeque<String>,
    transfers: HashMap<String, ManagedTransferEntry>,
    pending_offers: HashMap<String, PendingOfferEntry>,
}

struct ManagedTransferEntry {
    record_id: Uuid,
    transfer: ManagedTransfer,
    source: TransferSource,
    cancellation: CancellationToken,
    progress_reporter: Option<Arc<dyn TransferProgressReporter>>,
}

#[derive(Clone)]
enum TransferSource {
    Send {
        peer_id: DeviceId,
        file_path: PathBuf,
    },
    Receive {
        offer_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FolderTransferLink {
    pub folder_id: String,
    pub folder_relative_path: String,
}

struct PendingOfferEntry {
    notification: FileOfferNotification,
}

struct CancellationPlan {
    cancellation: CancellationToken,
    cancel_active: bool,
    reject_offer_id: Option<String>,
}

struct StartSpec {
    transfer_id: String,
    source: TransferSource,
    cancellation: CancellationToken,
}

struct PersistedTransferRecord {
    transfer_id: String,
    record: TransferRecord,
    updated_transfer: ManagedTransfer,
}

enum WorkerResult {
    SendCompleted,
    ReceiveCompleted(PathBuf),
}

struct ManagerProgressReporter<S, R, St>
where
    S: ManagedTransferSender,
    R: ManagedTransferReceiver,
    St: StorageEngine + 'static,
{
    inner: Weak<TransferManagerInner<S, R, St>>,
    transfer_id: String,
}

impl<S, R, St> TransferProgressReporter for ManagerProgressReporter<S, R, St>
where
    S: ManagedTransferSender,
    R: ManagedTransferReceiver,
    St: StorageEngine + 'static,
{
    fn report(&self, progress: TransferProgress) {
        if let Some(inner) = self.inner.upgrade() {
            inner.record_progress(&self.transfer_id, progress);
        }
    }
}

impl<S, R, St> TransferManagerInner<S, R, St>
where
    S: ManagedTransferSender,
    R: ManagedTransferReceiver,
    St: StorageEngine + 'static,
{
    fn enqueue_transfer(&self, entry: ManagedTransferEntry) {
        let transfer_id = entry.transfer.id.clone();
        let mut state = self.state.lock().expect("lock transfer manager state");
        state.queue.push_back(transfer_id.clone());
        state.transfers.insert(transfer_id, entry);
    }

    fn remember_pending_offer(&self, offer_id: String, pending_offer: PendingOfferEntry) {
        self.state
            .lock()
            .expect("lock transfer manager state")
            .pending_offers
            .insert(offer_id, pending_offer);
    }

    fn take_pending_offer(&self, offer_id: &str) -> Option<PendingOfferEntry> {
        self.state
            .lock()
            .expect("lock transfer manager state")
            .pending_offers
            .remove(offer_id)
    }

    fn update_active_limit(&self, active_limit: usize) {
        self.state
            .lock()
            .expect("lock transfer manager state")
            .active_limit = active_limit;
    }

    fn transfer(&self, transfer_id: &str) -> Option<ManagedTransfer> {
        self.state
            .lock()
            .expect("lock transfer manager state")
            .transfers
            .get(transfer_id)
            .map(|entry| entry.transfer.clone())
    }

    fn snapshot(&self) -> TransferManagerSnapshot {
        let state = self.state.lock().expect("lock transfer manager state");
        let active_transfers = state
            .active_ids
            .iter()
            .filter_map(|transfer_id| state.transfers.get(transfer_id))
            .map(|entry| entry.transfer.clone())
            .collect();
        let queued_transfers = state
            .queue
            .iter()
            .filter_map(|transfer_id| state.transfers.get(transfer_id))
            .map(|entry| entry.transfer.clone())
            .collect();
        let mut pending_offers = state
            .pending_offers
            .values()
            .map(|pending| pending.notification.clone())
            .collect::<Vec<_>>();
        pending_offers.sort_by(|left, right| left.offer_id.cmp(&right.offer_id));

        TransferManagerSnapshot {
            active_limit: state.active_limit,
            pending_offers,
            active_transfers,
            queued_transfers,
        }
    }

    fn aggregate_progress(&self) -> TransferAggregateProgress {
        let state = self.state.lock().expect("lock transfer manager state");
        let mut aggregate = TransferAggregateProgress {
            active_transfers: state.active_ids.len(),
            queued_transfers: state.queue.len(),
            ..TransferAggregateProgress::default()
        };

        for transfer_id in &state.active_ids {
            let Some(entry) = state.transfers.get(transfer_id) else {
                continue;
            };

            if let Some(progress) = entry.transfer.progress.as_ref() {
                aggregate.bytes_sent += progress.bytes_sent;
                aggregate.total_bytes += progress.total_bytes;
                aggregate.speed_bps += progress.speed_bps;
            } else {
                aggregate.total_bytes += entry.transfer.total_bytes;
            }
        }

        aggregate
    }

    fn record_progress(&self, transfer_id: &str, progress: TransferProgress) {
        let reporter = {
            let mut state = self.state.lock().expect("lock transfer manager state");
            let Some(entry) = state.transfers.get_mut(transfer_id) else {
                return;
            };
            entry.transfer.progress = Some(progress.clone());
            entry.progress_reporter.clone()
        };

        if let Some(reporter) = reporter {
            reporter.report(progress);
        }
    }

    fn prepare_cancel(&self, transfer_id: &str) -> Option<CancellationPlan> {
        let mut state = self.state.lock().expect("lock transfer manager state");
        let (status, cancellation, reject_offer_id) = {
            let entry = state.transfers.get(transfer_id)?;
            (
                entry.transfer.status.clone(),
                entry.cancellation.clone(),
                match &entry.source {
                    TransferSource::Receive { offer_id } => Some(offer_id.clone()),
                    TransferSource::Send { .. } => None,
                },
            )
        };

        match status {
            TransferStatus::Pending => {
                remove_from_vecdeque(&mut state.queue, transfer_id);
                if let Some(entry) = state.transfers.get_mut(transfer_id) {
                    entry.transfer.status = TransferStatus::Cancelled;
                }
                Some(CancellationPlan {
                    cancellation,
                    cancel_active: false,
                    reject_offer_id,
                })
            }
            TransferStatus::Active => {
                remove_from_vecdeque(&mut state.active_ids, transfer_id);
                if let Some(entry) = state.transfers.get_mut(transfer_id) {
                    entry.transfer.status = TransferStatus::Cancelled;
                }
                Some(CancellationPlan {
                    cancellation,
                    cancel_active: true,
                    reject_offer_id: None,
                })
            }
            TransferStatus::Completed | TransferStatus::Failed | TransferStatus::Cancelled => {
                Some(CancellationPlan {
                    cancellation,
                    cancel_active: false,
                    reject_offer_id: None,
                })
            }
        }
    }

    async fn persist_transfer_async(&self, transfer_id: &str) -> Result<(), TransferManagerError> {
        let record = self
            .transfer_record(transfer_id)
            .ok_or_else(|| TransferManagerError::UnknownTransfer(transfer_id.to_string()))?;
        self.storage
            .save_transfer(&record)
            .await
            .map_err(|error| TransferManagerError::Storage(error.to_string()))
    }

    fn persist_transfer_blocking(&self, transfer_id: &str) {
        let Some(record) = self.transfer_record(transfer_id) else {
            return;
        };

        self.persist_record_blocking(record);
    }

    fn persist_record_blocking(&self, record: TransferRecord) {
        let storage = Arc::clone(&self.storage);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build transfer storage runtime");
        let _ = runtime.block_on(async move { storage.save_transfer(&record).await });
    }

    fn transfer_record(&self, transfer_id: &str) -> Option<TransferRecord> {
        let state = self.state.lock().expect("lock transfer manager state");
        let entry = state.transfers.get(transfer_id)?;

        Some(build_transfer_record(entry, &entry.transfer))
    }

    async fn start_ready_transfers_async(self: &Arc<Self>) -> Result<(), TransferManagerError> {
        for start in self.collect_startable() {
            self.persist_transfer_async(&start.transfer_id).await?;
            self.spawn_worker(start);
        }

        Ok(())
    }

    fn start_ready_transfers_blocking(self: &Arc<Self>) {
        for start in self.collect_startable() {
            self.persist_transfer_blocking(&start.transfer_id);
            self.spawn_worker(start);
        }
    }

    fn collect_startable(&self) -> Vec<StartSpec> {
        let mut state = self.state.lock().expect("lock transfer manager state");
        let mut starts = Vec::new();

        while state.active_ids.len() < state.active_limit {
            let Some(transfer_id) = state.queue.pop_front() else {
                break;
            };
            let (source, cancellation) = {
                let Some(entry) = state.transfers.get_mut(&transfer_id) else {
                    continue;
                };
                if entry.transfer.status != TransferStatus::Pending {
                    continue;
                }

                entry.transfer.status = TransferStatus::Active;
                (entry.source.clone(), entry.cancellation.clone())
            };
            state.active_ids.push_back(transfer_id.clone());
            starts.push(StartSpec {
                transfer_id,
                source,
                cancellation,
            });
        }

        starts
    }

    fn spawn_worker(self: &Arc<Self>, start: StartSpec) {
        let inner = Arc::clone(self);
        std::thread::spawn(move || {
            let reporter: Arc<dyn TransferProgressReporter> = Arc::new(ManagerProgressReporter {
                inner: Arc::downgrade(&inner),
                transfer_id: start.transfer_id.clone(),
            });
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build transfer worker runtime");
            let transfer_id = start.transfer_id.clone();
            let worker_outcome = runtime.block_on(async {
                match start.source {
                    TransferSource::Send { peer_id, file_path } => inner
                        .sender
                        .send_file(
                            &transfer_id,
                            peer_id,
                            file_path,
                            start.cancellation,
                            Some(reporter),
                        )
                        .await
                        .map(|_| WorkerResult::SendCompleted),
                    TransferSource::Receive { offer_id } => inner
                        .receiver
                        .accept_offer(&offer_id, start.cancellation, Some(reporter))
                        .await
                        .map(WorkerResult::ReceiveCompleted),
                }
            });

            inner.finish_transfer(&transfer_id, worker_outcome);
        });
    }

    fn finish_transfer(
        self: &Arc<Self>,
        transfer_id: &str,
        worker_outcome: Result<WorkerResult, TransferManagerError>,
    ) {
        let persisted = {
            let mut state = self.state.lock().expect("lock transfer manager state");
            remove_from_vecdeque(&mut state.active_ids, transfer_id);

            let Some(entry) = state.transfers.get(transfer_id) else {
                return;
            };
            if entry.transfer.status == TransferStatus::Cancelled {
                None
            } else {
                let mut updated_transfer = entry.transfer.clone();
                match worker_outcome {
                    Ok(WorkerResult::SendCompleted) => {
                        updated_transfer.status = TransferStatus::Completed;
                    }
                    Ok(WorkerResult::ReceiveCompleted(path)) => {
                        updated_transfer.status = TransferStatus::Completed;
                        updated_transfer.local_path = path;
                    }
                    Err(TransferManagerError::Cancelled) => {
                        updated_transfer.status = TransferStatus::Cancelled;
                    }
                    Err(_) => {
                        updated_transfer.status = TransferStatus::Failed;
                    }
                }

                Some(PersistedTransferRecord {
                    transfer_id: transfer_id.to_string(),
                    record: build_transfer_record(entry, &updated_transfer),
                    updated_transfer,
                })
            }
        };

        if let Some(persisted) = persisted {
            let thumbnail_job = (
                persisted.transfer_id.clone(),
                persisted.updated_transfer.direction,
                persisted.updated_transfer.status.clone(),
                persisted.updated_transfer.local_path.clone(),
            );
            self.persist_record_blocking(persisted.record);

            let mut state = self.state.lock().expect("lock transfer manager state");
            if let Some(entry) = state.transfers.get_mut(&persisted.transfer_id) {
                if entry.transfer.status != TransferStatus::Cancelled {
                    entry.transfer = persisted.updated_transfer.clone();
                }
            }

            drop(state);
            self.spawn_thumbnail_generation(
                thumbnail_job.0,
                thumbnail_job.1,
                thumbnail_job.2,
                thumbnail_job.3,
            );
        }

        self.start_ready_transfers_blocking();
    }

    fn spawn_thumbnail_generation(
        self: &Arc<Self>,
        transfer_id: String,
        direction: TransferDirection,
        status: TransferStatus,
        local_path: PathBuf,
    ) {
        if direction != TransferDirection::Receive
            || status != TransferStatus::Completed
            || !local_path.is_file()
        {
            return;
        }

        let Some(app_data_dir) = self
            .settings
            .settings_path()
            .parent()
            .map(Path::to_path_buf)
        else {
            warn!(
                transfer_id = %transfer_id,
                "skipping thumbnail generation because app data directory is unavailable"
            );
            return;
        };

        let storage = Arc::clone(&self.storage);
        let runtime = self.runtime.clone();
        let blocking_runtime = runtime.clone();
        let thumbnail_tasks = Arc::clone(&self.thumbnail_tasks);
        let thumbnail_dir = app_data_dir.join("thumbnails");
        runtime.spawn_blocking(move || {
            let Ok(_permit) = blocking_runtime.block_on(thumbnail_tasks.acquire_owned()) else {
                return;
            };

            match generate_thumbnail(&local_path, &thumbnail_dir, &transfer_id) {
                Ok(thumbnail_path) => {
                    let thumbnail_path = thumbnail_path.to_string_lossy().into_owned();
                    let transfer_id_for_save = transfer_id.clone();
                    if let Err(error) = blocking_runtime.block_on(async move {
                        storage
                            .save_thumbnail_path(&transfer_id_for_save, &thumbnail_path)
                            .await
                    }) {
                        warn!(
                            transfer_id = %transfer_id,
                            path = %local_path.display(),
                            error = %error,
                            "failed to persist transfer thumbnail path"
                        );
                    }
                }
                Err(crate::ThumbnailError::UnsupportedFormat) => {
                    debug!(
                        transfer_id = %transfer_id,
                        path = %local_path.display(),
                        "skipping thumbnail generation for unsupported file"
                    );
                }
                Err(error) => {
                    warn!(
                        transfer_id = %transfer_id,
                        path = %local_path.display(),
                        error = %error,
                        "thumbnail generation failed"
                    );
                }
            }
        });
    }
}

fn remove_from_vecdeque(values: &mut VecDeque<String>, needle: &str) {
    if let Some(index) = values.iter().position(|value| value == needle) {
        values.remove(index);
    }
}

fn file_name_from_path(path: &Path) -> Result<String, TransferManagerError> {
    path.file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or(TransferManagerError::InvalidFileName)
}

fn build_transfer_record(
    entry: &ManagedTransferEntry,
    transfer: &ManagedTransfer,
) -> TransferRecord {
    TransferRecord {
        id: entry.record_id,
        peer_id: transfer.peer_id.clone(),
        chat_id: transfer.chat_id.clone(),
        file_name: transfer.file_name.clone(),
        local_path: transfer.local_path.clone(),
        status: transfer.status.clone(),
        thumbnail_path: None,
        folder_id: transfer.folder_id.clone(),
        folder_relative_path: transfer.folder_relative_path.clone(),
    }
}

impl From<FileSenderError> for TransferManagerError {
    fn from(value: FileSenderError) -> Self {
        match value {
            FileSenderError::Cancelled => Self::Cancelled,
            FileSenderError::InvalidFileName => Self::InvalidFileName,
            other => Self::Worker(other.to_string()),
        }
    }
}

impl From<FileReceiverError> for TransferManagerError {
    fn from(value: FileReceiverError) -> Self {
        match value {
            FileReceiverError::Cancelled => Self::Cancelled,
            FileReceiverError::InvalidFileName => Self::InvalidFileName,
            FileReceiverError::UnknownOffer(offer_id) => Self::UnknownOffer(offer_id),
            other => Self::Worker(other.to_string()),
        }
    }
}

impl From<CoreError> for TransferManagerError {
    fn from(value: CoreError) -> Self {
        Self::Storage(value.to_string())
    }
}
