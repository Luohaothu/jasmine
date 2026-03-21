use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use jasmine_core::protocol::{
    FolderFileEntry, FolderManifestData, ProtocolMessage, MAX_PROTOCOL_MESSAGE_BYTES,
};
use jasmine_core::{CoreError, DeviceId, SettingsService, StorageEngine, TransferStatus};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::time::{self, Duration};
use tokio_util::sync::CancellationToken;
use tracing::warn;
use uuid::Uuid;

use crate::receiver::{
    sanitize_filename, DiskSpaceChecker, FileReceiver, FileReceiverConfig, FileReceiverError,
    FileReceiverSignal,
};
use crate::manager::{
    FolderTransferLink, ManagedTransferReceiver, ManagedTransferSender, TransferManager,
    TransferManagerError,
};
use crate::sender::{
    FileSender, FileSenderConfig, FileSenderError, FileSenderSignal, TransferProgress,
    TransferProgressReporter,
};

pub const DEFAULT_FOLDER_MANIFEST_MAX_FILES: usize = 200;
pub const MAX_MANIFEST_PATH_LENGTH: usize = 260;
const HASH_BUFFER_SIZE: usize = 64 * 1024;
const MANAGED_TRANSFER_POLL_INTERVAL: Duration = Duration::from_millis(10);

pub type FolderManifest = FolderManifestData;
pub type ManifestEntry = FolderFileEntry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderTransferContext {
    pub folder_id: String,
    pub folder_relative_path: String,
}

#[derive(Debug, Error)]
pub enum FolderManifestError {
    #[error("folder path must point to a directory")]
    InvalidFolderPath,
    #[error("folder path must not be a symlink")]
    InvalidFolderRoot,
    #[error("folder path does not have a valid UTF-8 folder name")]
    InvalidFolderName,
    #[error("manifest path is not valid UTF-8")]
    NonUtf8Path,
    #[error("unsafe manifest path: {0}")]
    UnsafePath(String),
    #[error("folder manifest exceeds file limit of {max_files}")]
    FileLimitExceeded { max_files: usize },
    #[error("folder manifest exceeds protocol size limit of {MAX_PROTOCOL_MESSAGE_BYTES} bytes")]
    ManifestTooLarge,
    #[error("folder total size overflowed u64")]
    TotalSizeOverflow,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(#[from] CoreError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderTransferStatus {
    Pending,
    Sending,
    Completed,
    PartiallyFailed,
    Cancelled,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderProgress {
    pub folder_transfer_id: String,
    pub total_bytes: u64,
    pub sent_bytes: u64,
    pub total_files: usize,
    pub completed_files: usize,
    pub status: FolderTransferStatus,
}

pub trait FolderProgressReporter: Send + Sync {
    fn report(&self, progress: FolderProgress);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderOfferNotification {
    pub folder_transfer_id: String,
    pub sender_id: DeviceId,
    pub folder_name: String,
    pub file_count: usize,
    pub total_size: u64,
}

pub trait FolderOfferNotifier: Send + Sync {
    fn offer_received(&self, offer: FolderOfferNotification);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderFileTransferResult {
    pub transfer_id: String,
    pub relative_path: String,
    pub status: TransferStatus,
    pub bytes_sent: u64,
    pub total_bytes: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderTransferOutcome {
    pub folder_transfer_id: String,
    pub manifest: FolderManifest,
    pub progress: FolderProgress,
    pub file_results: Vec<FolderFileTransferResult>,
    pub rejection_reason: Option<String>,
}

#[derive(Debug, Error)]
pub enum FolderTransferError {
    #[error("folder manifest error: {0}")]
    Manifest(#[from] FolderManifestError),
    #[error("folder signalling error: {0}")]
    Signal(#[from] FileSenderError),
    #[error("unexpected folder transfer response: {0:?}")]
    UnexpectedResponse(ProtocolMessage),
}

#[derive(Debug, Error)]
pub enum FolderReceiverError {
    #[error("folder manifest error: {0}")]
    Manifest(#[from] FolderManifestError),
    #[error("folder transfer offer is not pending: {0}")]
    UnknownOffer(String),
    #[error("sender already has an active folder receive: {0}")]
    SenderBusy(String),
    #[error("unexpected folder signalling message: {0:?}")]
    UnexpectedMessage(Box<ProtocolMessage>),
    #[error("folder file-offer channel closed for transfer {0}")]
    OfferChannelClosed(String),
    #[error("file receiver error: {0}")]
    File(#[from] FileReceiverError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub trait FolderFileTransferSender: Send + Sync {
    async fn send_file_with_id(
        &self,
        transfer_id: String,
        peer_id: DeviceId,
        file_path: PathBuf,
        cancellation: CancellationToken,
        progress: Option<Arc<dyn TransferProgressReporter>>,
    ) -> Result<(), FileSenderError>;

    async fn send_file_with_context(
        &self,
        transfer_id: String,
        peer_id: DeviceId,
        file_path: PathBuf,
        folder_link: Option<FolderTransferContext>,
        cancellation: CancellationToken,
        progress: Option<Arc<dyn TransferProgressReporter>>,
    ) -> Result<(), FileSenderError> {
        let _ = folder_link;
        self.send_file_with_id(transfer_id, peer_id, file_path, cancellation, progress)
            .await
    }
}

impl<S> FolderFileTransferSender for FileSender<S>
where
    S: FileSenderSignal,
{
    async fn send_file_with_id(
        &self,
        transfer_id: String,
        peer_id: DeviceId,
        file_path: PathBuf,
        cancellation: CancellationToken,
        progress: Option<Arc<dyn TransferProgressReporter>>,
    ) -> Result<(), FileSenderError> {
        FileSender::send_file_with_id(self, transfer_id, peer_id, file_path, cancellation, progress)
            .await
    }
}

impl<S, R, St> FolderFileTransferSender for Arc<TransferManager<S, R, St>>
where
    S: ManagedTransferSender,
    R: ManagedTransferReceiver,
    St: StorageEngine + 'static,
{
    async fn send_file_with_id(
        &self,
        transfer_id: String,
        peer_id: DeviceId,
        file_path: PathBuf,
        cancellation: CancellationToken,
        progress: Option<Arc<dyn TransferProgressReporter>>,
    ) -> Result<(), FileSenderError> {
        self.send_file_with_context(
            transfer_id,
            peer_id,
            file_path,
            None,
            cancellation,
            progress,
        )
        .await
    }

    async fn send_file_with_context(
        &self,
        transfer_id: String,
        peer_id: DeviceId,
        file_path: PathBuf,
        folder_link: Option<FolderTransferContext>,
        cancellation: CancellationToken,
        progress: Option<Arc<dyn TransferProgressReporter>>,
    ) -> Result<(), FileSenderError> {
        let folder_link = folder_link.map(|folder_link| FolderTransferLink {
            folder_id: folder_link.folder_id,
            folder_relative_path: folder_link.folder_relative_path,
        });
        self.send_file_with_id_and_context(
            transfer_id.clone(),
            peer_id,
            file_path,
            None,
            folder_link,
            progress,
        )
        .await
        .map_err(map_transfer_manager_error)?;
        tokio::select! {
            result = wait_for_managed_transfer_completion(self.as_ref(), &transfer_id) => result,
            _ = cancellation.cancelled() => {
                self.cancel_transfer(&transfer_id)
                    .await
                    .map_err(map_transfer_manager_error)?;
                wait_for_managed_transfer_completion(self.as_ref(), &transfer_id).await
            }
        }
    }
}

fn map_transfer_manager_error(error: TransferManagerError) -> FileSenderError {
    match error {
        TransferManagerError::Cancelled => FileSenderError::Cancelled,
        TransferManagerError::InvalidFileName => FileSenderError::InvalidFileName,
        other => FileSenderError::Signal(other.to_string()),
    }
}

async fn wait_for_managed_transfer_completion<S, R, St>(
    manager: &TransferManager<S, R, St>,
    transfer_id: &str,
) -> Result<(), FileSenderError>
where
    S: ManagedTransferSender,
    R: ManagedTransferReceiver,
    St: StorageEngine + 'static,
{
    loop {
        let Some(transfer) = manager.transfer(transfer_id) else {
            return Err(FileSenderError::Signal(format!(
                "unknown managed transfer {transfer_id}"
            )));
        };

        match transfer.status {
            TransferStatus::Completed => return Ok(()),
            TransferStatus::Cancelled => return Err(FileSenderError::Cancelled),
            TransferStatus::Failed => {
                return Err(FileSenderError::Signal(format!(
                    "managed transfer failed: {transfer_id}"
                )))
            }
            TransferStatus::Pending | TransferStatus::Active => {
                time::sleep(MANAGED_TRANSFER_POLL_INTERVAL).await;
            }
        }
    }
}

pub struct FolderTransferCoordinator<Sig, Sender>
where
    Sig: FileSenderSignal,
    Sender: FolderFileTransferSender,
{
    signal: Sig,
    file_sender: Sender,
}

impl<Sig, Sender> FolderTransferCoordinator<Sig, Sender>
where
    Sig: FileSenderSignal,
    Sender: FolderFileTransferSender,
{
    pub fn new(signal: Sig, file_sender: Sender) -> Self {
        Self { signal, file_sender }
    }

    pub async fn send_folder<P>(
        &self,
        sender_id: DeviceId,
        peer_id: DeviceId,
        folder_path: P,
        cancellation: CancellationToken,
        progress: Option<Arc<dyn FolderProgressReporter>>,
    ) -> Result<FolderTransferOutcome, FolderTransferError>
    where
        P: AsRef<Path>,
    {
        self.send_folder_with_id(
            sender_id,
            peer_id,
            folder_path,
            Uuid::new_v4().to_string(),
            cancellation,
            progress,
        )
        .await
    }

    pub async fn send_folder_with_id<P>(
        &self,
        sender_id: DeviceId,
        peer_id: DeviceId,
        folder_path: P,
        folder_transfer_id: String,
        cancellation: CancellationToken,
        progress: Option<Arc<dyn FolderProgressReporter>>,
    ) -> Result<FolderTransferOutcome, FolderTransferError>
    where
        P: AsRef<Path>,
    {
        let folder_path = folder_path.as_ref();
        let manifest = generate_folder_manifest(folder_path, DEFAULT_FOLDER_MANIFEST_MAX_FILES)?;
        let progress_state = Arc::new(Mutex::new(FolderProgressState {
            progress: FolderProgress {
                folder_transfer_id: folder_transfer_id.clone(),
                total_bytes: manifest.total_size,
                sent_bytes: 0,
                total_files: manifest.files.len(),
                completed_files: 0,
                status: FolderTransferStatus::Pending,
            },
            settled_bytes_sent: 0,
            active_file_bytes_sent: 0,
        }));

        emit_folder_progress(progress.as_ref(), &progress_snapshot(&progress_state));

        if cancellation.is_cancelled() {
            return Ok(cancelled_outcome(&manifest, &folder_transfer_id, &progress_state, progress.as_ref()));
        }

        self.signal
            .send_message(
                &peer_id,
                ProtocolMessage::FolderManifest {
                    folder_transfer_id: folder_transfer_id.clone(),
                    manifest: manifest.clone(),
                    sender_id: sender_id.0.to_string(),
                },
            )
            .await?;

        let response = tokio::select! {
            _ = cancellation.cancelled() => {
                return Ok(cancelled_outcome(&manifest, &folder_transfer_id, &progress_state, progress.as_ref()));
            }
            response = self.signal.wait_for_response(&folder_transfer_id) => response?,
        };

        match response {
            ProtocolMessage::FolderAccept { folder_transfer_id: accepted_id }
                if accepted_id == folder_transfer_id =>
            {
                update_folder_status(
                    &progress_state,
                    FolderTransferStatus::Sending,
                    progress.as_ref(),
                );
            }
            ProtocolMessage::FolderReject {
                folder_transfer_id: rejected_id,
                reason,
            } if rejected_id == folder_transfer_id => {
                update_folder_status(
                    &progress_state,
                    FolderTransferStatus::Rejected,
                    progress.as_ref(),
                );
                return Ok(FolderTransferOutcome {
                    folder_transfer_id,
                    manifest,
                    progress: progress_snapshot(&progress_state),
                    file_results: Vec::new(),
                    rejection_reason: Some(reason),
                });
            }
            other => return Err(FolderTransferError::UnexpectedResponse(other)),
        }

        let mut file_results = Vec::with_capacity(manifest.files.len());
        let mut had_failures = false;

        for entry in &manifest.files {
            if cancellation.is_cancelled() {
                return Ok(cancelled_outcome_with_files(
                    &manifest,
                    &folder_transfer_id,
                    &progress_state,
                    progress.as_ref(),
                    file_results,
                ));
            }

            let relative_path = sanitize_manifest_path(&entry.relative_path)?;
            let file_path = manifest_entry_path(folder_path, &relative_path);
            let transfer_id = Uuid::new_v4().to_string();
            let progress_proxy: Arc<dyn TransferProgressReporter> = Arc::new(
                FolderAggregateProgressReporter {
                    state: Arc::clone(&progress_state),
                    file_total_bytes: entry.size,
                    reporter: progress.clone(),
                },
            );

            let send_result = self
                .file_sender
                .send_file_with_context(
                    transfer_id.clone(),
                    peer_id.clone(),
                    file_path,
                    Some(FolderTransferContext {
                        folder_id: folder_transfer_id.clone(),
                        folder_relative_path: relative_path.clone(),
                    }),
                    cancellation.clone(),
                    Some(progress_proxy),
                )
                .await;

            let attempted_bytes = settle_active_file_progress(
                &progress_state,
                entry.size,
                send_result.is_ok(),
                progress.as_ref(),
            );

            match send_result {
                Ok(()) => file_results.push(FolderFileTransferResult {
                    transfer_id,
                    relative_path,
                    status: TransferStatus::Completed,
                    bytes_sent: entry.size,
                    total_bytes: entry.size,
                    error: None,
                }),
                Err(FileSenderError::Cancelled) => {
                    file_results.push(FolderFileTransferResult {
                        transfer_id,
                        relative_path,
                        status: TransferStatus::Cancelled,
                        bytes_sent: attempted_bytes,
                        total_bytes: entry.size,
                        error: None,
                    });
                    return Ok(cancelled_outcome_with_files(
                        &manifest,
                        &folder_transfer_id,
                        &progress_state,
                        progress.as_ref(),
                        file_results,
                    ));
                }
                Err(error) => {
                    had_failures = true;
                    file_results.push(FolderFileTransferResult {
                        transfer_id,
                        relative_path,
                        status: TransferStatus::Failed,
                        bytes_sent: attempted_bytes,
                        total_bytes: entry.size,
                        error: Some(error.to_string()),
                    });
                }
            }
        }

        update_folder_status(
            &progress_state,
            if had_failures {
                FolderTransferStatus::PartiallyFailed
            } else {
                FolderTransferStatus::Completed
            },
            progress.as_ref(),
        );

        Ok(FolderTransferOutcome {
            folder_transfer_id,
            manifest,
            progress: progress_snapshot(&progress_state),
            file_results,
            rejection_reason: None,
        })
    }
}

impl<S> FolderTransferCoordinator<S, FileSender<S>>
where
    S: FileSenderSignal + Clone,
{
    pub fn from_signal(signal: S) -> Self {
        Self::with_file_sender_config(signal, FileSenderConfig::default())
    }

    pub fn with_file_sender_config(signal: S, config: FileSenderConfig) -> Self {
        let file_sender = FileSender::with_config(signal.clone(), config);
        Self::new(signal, file_sender)
    }
}

pub struct FolderReceiver<S>
where
    S: FileReceiverSignal + Clone,
{
    signal: S,
    file_receiver: FileReceiver<S>,
    offer_notifier: Option<Arc<dyn FolderOfferNotifier>>,
    state: Mutex<FolderReceiverState>,
}

impl<S> FolderReceiver<S>
where
    S: FileReceiverSignal + Clone,
{
    pub fn new(signal: S, settings: SettingsService) -> Self {
        let file_receiver = FileReceiver::new(signal.clone(), settings);
        Self::with_parts(signal, file_receiver, None)
    }

    pub fn with_config(signal: S, settings: SettingsService, config: FileReceiverConfig) -> Self {
        let file_receiver = FileReceiver::with_config(signal.clone(), settings, config);
        Self::with_parts(signal, file_receiver, None)
    }

    pub fn with_dependencies(
        signal: S,
        settings: SettingsService,
        config: FileReceiverConfig,
        disk_space_checker: Arc<dyn DiskSpaceChecker>,
        offer_notifier: Option<Arc<dyn FolderOfferNotifier>>,
    ) -> Self {
        let file_receiver =
            FileReceiver::with_dependencies(signal.clone(), settings, config, disk_space_checker, None);
        Self::with_parts(signal, file_receiver, offer_notifier)
    }

    fn with_parts(
        signal: S,
        file_receiver: FileReceiver<S>,
        offer_notifier: Option<Arc<dyn FolderOfferNotifier>>,
    ) -> Self {
        Self {
            signal,
            file_receiver,
            offer_notifier,
            state: Mutex::new(FolderReceiverState {
                pending_offers: HashMap::new(),
                active_receives: HashMap::new(),
            }),
        }
    }

    pub async fn handle_signal_message(
        &self,
        sender_id: DeviceId,
        sender_address: SocketAddr,
        message: ProtocolMessage,
    ) -> Result<Option<FolderOfferNotification>, FolderReceiverError> {
        match message {
            ProtocolMessage::FolderManifest {
                folder_transfer_id,
                manifest,
                ..
            } => {
                let notification = FolderOfferNotification {
                    folder_transfer_id: folder_transfer_id.clone(),
                    sender_id: sender_id.clone(),
                    folder_name: manifest.folder_name.clone(),
                    file_count: manifest.files.len(),
                    total_size: manifest.total_size,
                };

                self.state
                    .lock()
                    .expect("lock folder receiver state")
                    .pending_offers
                    .insert(
                        folder_transfer_id,
                        PendingFolderOffer {
                            sender_id,
                            manifest,
                        },
                    );

                if let Some(notifier) = self.offer_notifier.as_ref() {
                    notifier.offer_received(notification.clone());
                }

                Ok(Some(notification))
            }
            offer @ ProtocolMessage::FileOffer { .. } => {
                self.dispatch_file_offer(sender_id, sender_address, offer)?;
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    pub async fn accept_offer<P>(
        &self,
        folder_transfer_id: &str,
        target_dir: P,
        progress: Option<Arc<dyn FolderProgressReporter>>,
    ) -> Result<FolderTransferOutcome, FolderReceiverError>
    where
        P: AsRef<Path>,
    {
        self.accept_offer_with_cancellation(
            folder_transfer_id,
            target_dir,
            CancellationToken::new(),
            progress,
        )
        .await
    }

    pub async fn accept_offer_with_cancellation<P>(
        &self,
        folder_transfer_id: &str,
        target_dir: P,
        cancellation: CancellationToken,
        progress: Option<Arc<dyn FolderProgressReporter>>,
    ) -> Result<FolderTransferOutcome, FolderReceiverError>
    where
        P: AsRef<Path>,
    {
        let (pending, mut file_offer_rx) =
            self.activate_pending_offer(folder_transfer_id, cancellation.clone())?;
        let active_guard = ActiveReceiveGuard::new(self, folder_transfer_id.to_string());
        let folder_name = sanitize_folder_name(&pending.manifest.folder_name)?;
        let root_dir = target_dir.as_ref().join(&folder_name);
        let manifest = pending.manifest.clone();
        let progress_state = Arc::new(Mutex::new(FolderProgressState {
            progress: FolderProgress {
                folder_transfer_id: folder_transfer_id.to_string(),
                total_bytes: manifest.total_size,
                sent_bytes: 0,
                total_files: manifest.files.len(),
                completed_files: 0,
                status: FolderTransferStatus::Pending,
            },
            settled_bytes_sent: 0,
            active_file_bytes_sent: 0,
        }));

        emit_folder_progress(progress.as_ref(), &progress_snapshot(&progress_state));

        if cancellation.is_cancelled() {
            return Ok(cancelled_outcome(
                &manifest,
                folder_transfer_id,
                &progress_state,
                progress.as_ref(),
            ));
        }

        tokio::fs::create_dir_all(&root_dir).await?;
        self.signal
            .send_message(
                &pending.sender_id,
                ProtocolMessage::FolderAccept {
                    folder_transfer_id: folder_transfer_id.to_string(),
                },
            )
            .await?;

        update_folder_status(
            &progress_state,
            FolderTransferStatus::Sending,
            progress.as_ref(),
        );

        let mut file_results = Vec::with_capacity(manifest.files.len());
        let mut had_failures = false;

        for entry in &manifest.files {
            let queued_offer = tokio::select! {
                _ = cancellation.cancelled() => {
                    return Ok(cancelled_outcome_with_files(
                        &manifest,
                        folder_transfer_id,
                        &progress_state,
                        progress.as_ref(),
                        file_results,
                    ));
                }
                offer = file_offer_rx.recv() => offer,
            }
            .ok_or_else(|| FolderReceiverError::OfferChannelClosed(folder_transfer_id.to_string()))?;

            let file_result = self
                .receive_manifest_file(
                    entry,
                    queued_offer,
                    &root_dir,
                    cancellation.clone(),
                    &progress_state,
                    progress.as_ref(),
                )
                .await?;

            let was_cancelled = file_result.status == TransferStatus::Cancelled;
            had_failures |= file_result.status == TransferStatus::Failed;
            file_results.push(file_result);

            if was_cancelled {
                return Ok(cancelled_outcome_with_files(
                    &manifest,
                    folder_transfer_id,
                    &progress_state,
                    progress.as_ref(),
                    file_results,
                ));
            }
        }

        drop(active_guard);
        update_folder_status(
            &progress_state,
            if had_failures {
                FolderTransferStatus::PartiallyFailed
            } else {
                FolderTransferStatus::Completed
            },
            progress.as_ref(),
        );

        Ok(FolderTransferOutcome {
            folder_transfer_id: folder_transfer_id.to_string(),
            manifest,
            progress: progress_snapshot(&progress_state),
            file_results,
            rejection_reason: None,
        })
    }

    pub async fn reject_offer(
        &self,
        folder_transfer_id: &str,
        reason: Option<String>,
    ) -> Result<(), FolderReceiverError> {
        let pending = self
            .state
            .lock()
            .expect("lock folder receiver state")
            .pending_offers
            .remove(folder_transfer_id)
            .ok_or_else(|| FolderReceiverError::UnknownOffer(folder_transfer_id.to_string()))?;
        self.signal
            .send_message(
                &pending.sender_id,
                ProtocolMessage::FolderReject {
                    folder_transfer_id: folder_transfer_id.to_string(),
                    reason: reason.unwrap_or_else(|| "receiver rejected folder transfer".to_string()),
                },
            )
            .await?;
        Ok(())
    }

    pub async fn cancel_transfer(&self, folder_transfer_id: &str) -> Result<(), FolderReceiverError> {
        if self
            .state
            .lock()
            .expect("lock folder receiver state")
            .pending_offers
            .contains_key(folder_transfer_id)
        {
            return self
                .reject_offer(folder_transfer_id, Some("cancelled".to_string()))
                .await;
        }

        let cancellation = self
            .state
            .lock()
            .expect("lock folder receiver state")
            .active_receives
            .get(folder_transfer_id)
            .map(|active| active.cancellation.clone())
            .ok_or_else(|| FolderReceiverError::UnknownOffer(folder_transfer_id.to_string()))?;
        cancellation.cancel();
        Ok(())
    }

    fn activate_pending_offer(
        &self,
        folder_transfer_id: &str,
        cancellation: CancellationToken,
    ) -> Result<(PendingFolderOffer, mpsc::UnboundedReceiver<QueuedFolderFileOffer>), FolderReceiverError> {
        let mut state = self.state.lock().expect("lock folder receiver state");
        let pending = state
            .pending_offers
            .remove(folder_transfer_id)
            .ok_or_else(|| FolderReceiverError::UnknownOffer(folder_transfer_id.to_string()))?;

        if state
            .active_receives
            .values()
            .any(|active| active.sender_id == pending.sender_id)
        {
            let sender = pending.sender_id.0.to_string();
            state
                .pending_offers
                .insert(folder_transfer_id.to_string(), pending);
            return Err(FolderReceiverError::SenderBusy(sender));
        }

        let (file_offer_tx, file_offer_rx) = mpsc::unbounded_channel();
        state.active_receives.insert(
            folder_transfer_id.to_string(),
            ActiveFolderReceive {
                sender_id: pending.sender_id.clone(),
                file_offer_tx,
                cancellation,
            },
        );

        Ok((pending, file_offer_rx))
    }

    fn dispatch_file_offer(
        &self,
        sender_id: DeviceId,
        sender_address: SocketAddr,
        message: ProtocolMessage,
    ) -> Result<(), FolderReceiverError> {
        let routed = {
            let state = self.state.lock().expect("lock folder receiver state");
            state
                .active_receives
                .iter()
                .find(|(_, active)| active.sender_id == sender_id)
                .map(|(transfer_id, active)| (transfer_id.clone(), active.file_offer_tx.clone()))
        };

        let Some((folder_transfer_id, sender)) = routed else {
            return Ok(());
        };

        sender
            .send(QueuedFolderFileOffer {
                sender_id,
                sender_address,
                message,
            })
            .map_err(|_| FolderReceiverError::OfferChannelClosed(folder_transfer_id))
    }

    async fn receive_manifest_file(
        &self,
        entry: &ManifestEntry,
        queued_offer: QueuedFolderFileOffer,
        root_dir: &Path,
        cancellation: CancellationToken,
        progress_state: &Arc<Mutex<FolderProgressState>>,
        progress: Option<&Arc<dyn FolderProgressReporter>>,
    ) -> Result<FolderFileTransferResult, FolderReceiverError> {
        let offer = parse_folder_file_offer(queued_offer.message)?;
        let relative_path = match sanitize_manifest_path(&entry.relative_path) {
            Ok(relative_path) => relative_path,
            Err(error) => {
                return self
                    .reject_manifest_file(ManifestFileRejection {
                        sender_id: &queued_offer.sender_id,
                        offer_id: &offer.offer_id,
                        relative_path: &entry.relative_path,
                        total_bytes: entry.size,
                        reason: error.to_string(),
                        progress_state,
                        progress,
                    })
                    .await;
            }
        };

        let expected_filename = file_name_from_relative_path(&relative_path)?;
        let offered_filename = match sanitize_filename(&offer.filename) {
            Ok(filename) => filename,
            Err(error) => {
                return self
                    .reject_manifest_file(ManifestFileRejection {
                        sender_id: &queued_offer.sender_id,
                        offer_id: &offer.offer_id,
                        relative_path: &relative_path,
                        total_bytes: entry.size,
                        reason: error.to_string(),
                        progress_state,
                        progress,
                    })
                    .await;
            }
        };

        if offered_filename != expected_filename {
            return self
                .reject_manifest_file(ManifestFileRejection {
                    sender_id: &queued_offer.sender_id,
                    offer_id: &offer.offer_id,
                    relative_path: &relative_path,
                    total_bytes: entry.size,
                    reason: format!(
                        "file offer did not match manifest path: expected {expected_filename}, got {offered_filename}"
                    ),
                    progress_state,
                    progress,
                })
                .await;
        }

        if offer.size != entry.size {
            return self
                .reject_manifest_file(ManifestFileRejection {
                    sender_id: &queued_offer.sender_id,
                    offer_id: &offer.offer_id,
                    relative_path: &relative_path,
                    total_bytes: entry.size,
                    reason: format!(
                        "file offer size did not match manifest: expected {}, got {}",
                        entry.size, offer.size
                    ),
                    progress_state,
                    progress,
                })
                .await;
        }

        if !offer.sha256.eq_ignore_ascii_case(&entry.sha256) {
            return self
                .reject_manifest_file(ManifestFileRejection {
                    sender_id: &queued_offer.sender_id,
                    offer_id: &offer.offer_id,
                    relative_path: &relative_path,
                    total_bytes: entry.size,
                    reason: format!(
                        "file offer hash did not match manifest: expected {}, got {}",
                        entry.sha256, offer.sha256
                    ),
                    progress_state,
                    progress,
                })
                .await;
        }

        let final_path = manifest_entry_path(root_dir, &relative_path);
        let progress_proxy: Arc<dyn TransferProgressReporter> = Arc::new(FolderAggregateProgressReporter {
            state: Arc::clone(progress_state),
            file_total_bytes: entry.size,
            reporter: progress.cloned(),
        });
        let receive_result = match self
            .file_receiver
            .receive_offer_to_path(
                queued_offer.sender_id,
                queued_offer.sender_address,
                offer.to_protocol_message(&entry.sha256),
                final_path,
            )
            .await
        {
            Ok(_) => {
                self.file_receiver
                    .accept_offer_with_cancellation(
                        &offer.offer_id,
                        cancellation,
                        Some(progress_proxy),
                    )
                    .await
            }
            Err(error) => Err(error),
        };
        let attempted_bytes = settle_active_file_progress(
            progress_state,
            entry.size,
            receive_result.is_ok(),
            progress,
        );

        Ok(match receive_result {
            Ok(_) => FolderFileTransferResult {
                transfer_id: offer.offer_id,
                relative_path,
                status: TransferStatus::Completed,
                bytes_sent: entry.size,
                total_bytes: entry.size,
                error: None,
            },
            Err(FileReceiverError::Cancelled) => FolderFileTransferResult {
                transfer_id: offer.offer_id,
                relative_path,
                status: TransferStatus::Cancelled,
                bytes_sent: attempted_bytes,
                total_bytes: entry.size,
                error: None,
            },
            Err(error) => FolderFileTransferResult {
                transfer_id: offer.offer_id,
                relative_path,
                status: TransferStatus::Failed,
                bytes_sent: attempted_bytes,
                total_bytes: entry.size,
                error: Some(error.to_string()),
            },
        })
    }

    async fn reject_manifest_file(
        &self,
        rejection: ManifestFileRejection<'_>,
    ) -> Result<FolderFileTransferResult, FolderReceiverError> {
        self.signal
            .send_message(
                rejection.sender_id,
                ProtocolMessage::FileReject {
                    offer_id: rejection.offer_id.to_string(),
                    reason: Some(rejection.reason.clone()),
                },
            )
            .await?;
        let attempted_bytes = settle_active_file_progress(
            rejection.progress_state,
            rejection.total_bytes,
            false,
            rejection.progress,
        );
        Ok(FolderFileTransferResult {
            transfer_id: rejection.offer_id.to_string(),
            relative_path: rejection.relative_path.to_string(),
            status: TransferStatus::Failed,
            bytes_sent: attempted_bytes,
            total_bytes: rejection.total_bytes,
            error: Some(rejection.reason),
        })
    }

    fn remove_active_receive(&self, folder_transfer_id: &str) {
        self.state
            .lock()
            .expect("lock folder receiver state")
            .active_receives
            .remove(folder_transfer_id);
    }
}

struct FolderReceiverState {
    pending_offers: HashMap<String, PendingFolderOffer>,
    active_receives: HashMap<String, ActiveFolderReceive>,
}

struct PendingFolderOffer {
    sender_id: DeviceId,
    manifest: FolderManifest,
}

struct ActiveFolderReceive {
    sender_id: DeviceId,
    file_offer_tx: mpsc::UnboundedSender<QueuedFolderFileOffer>,
    cancellation: CancellationToken,
}

struct QueuedFolderFileOffer {
    sender_id: DeviceId,
    sender_address: SocketAddr,
    message: ProtocolMessage,
}

struct ParsedFolderFileOffer {
    offer_id: String,
    filename: String,
    size: u64,
    sha256: String,
    transfer_port: u16,
}

struct ManifestFileRejection<'a> {
    sender_id: &'a DeviceId,
    offer_id: &'a str,
    relative_path: &'a str,
    total_bytes: u64,
    reason: String,
    progress_state: &'a Arc<Mutex<FolderProgressState>>,
    progress: Option<&'a Arc<dyn FolderProgressReporter>>,
}

impl ParsedFolderFileOffer {
    fn to_protocol_message(&self, expected_sha256: &str) -> ProtocolMessage {
        ProtocolMessage::FileOffer {
            id: self.offer_id.clone(),
            filename: self.filename.clone(),
            size: self.size,
            sha256: expected_sha256.to_string(),
            transfer_port: self.transfer_port,
        }
    }
}

struct ActiveReceiveGuard<'a, S>
where
    S: FileReceiverSignal + Clone,
{
    receiver: &'a FolderReceiver<S>,
    folder_transfer_id: String,
}

impl<'a, S> ActiveReceiveGuard<'a, S>
where
    S: FileReceiverSignal + Clone,
{
    fn new(receiver: &'a FolderReceiver<S>, folder_transfer_id: String) -> Self {
        Self {
            receiver,
            folder_transfer_id,
        }
    }
}

impl<S> Drop for ActiveReceiveGuard<'_, S>
where
    S: FileReceiverSignal + Clone,
{
    fn drop(&mut self) {
        self.receiver.remove_active_receive(&self.folder_transfer_id);
    }
}

fn parse_folder_file_offer(message: ProtocolMessage) -> Result<ParsedFolderFileOffer, FolderReceiverError> {
    match message {
        ProtocolMessage::FileOffer {
            id,
            filename,
            size,
            sha256,
            transfer_port,
        } => Ok(ParsedFolderFileOffer {
            offer_id: id,
            filename,
            size,
            sha256,
            transfer_port,
        }),
        other => Err(FolderReceiverError::UnexpectedMessage(Box::new(other))),
    }
}

fn sanitize_folder_name(folder_name: &str) -> Result<String, FolderManifestError> {
    let sanitized = sanitize_manifest_path(folder_name)?;
    if sanitized.contains('/') {
        return Err(FolderManifestError::UnsafePath(
            "folder name must be a single path segment".to_string(),
        ));
    }
    Ok(sanitized)
}

fn file_name_from_relative_path(relative_path: &str) -> Result<String, FolderManifestError> {
    Path::new(relative_path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            FolderManifestError::UnsafePath(
                "relative path must end with a valid file name".to_string(),
            )
        })
}

pub fn generate_folder_manifest(
    folder_path: &Path,
    max_files: usize,
) -> Result<FolderManifest, FolderManifestError> {
    let metadata = fs::symlink_metadata(folder_path)?;
    if metadata.file_type().is_symlink() {
        return Err(FolderManifestError::InvalidFolderRoot);
    }
    if !metadata.is_dir() {
        return Err(FolderManifestError::InvalidFolderPath);
    }

    let folder_name = folder_name_from_path(folder_path)?;
    let mut pending_files = Vec::new();
    collect_files(folder_path, folder_path, max_files, &mut pending_files)?;
    pending_files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));

    let mut total_size = 0_u64;
    let mut files = Vec::with_capacity(pending_files.len());

    for pending in pending_files {
        total_size = total_size
            .checked_add(pending.size)
            .ok_or(FolderManifestError::TotalSizeOverflow)?;
        files.push(FolderFileEntry {
            relative_path: pending.relative_path,
            size: pending.size,
            sha256: hash_file(&pending.absolute_path)?,
        });
    }

    let manifest = FolderManifestData {
        folder_name,
        files,
        total_size,
    };
    ensure_manifest_within_protocol_limit(&manifest)?;

    Ok(manifest)
}

pub fn sanitize_manifest_path(path: &str) -> Result<String, FolderManifestError> {
    if path.is_empty() {
        return Err(FolderManifestError::UnsafePath(
            "path must not be empty".to_string(),
        ));
    }
    if path.chars().count() > MAX_MANIFEST_PATH_LENGTH {
        return Err(FolderManifestError::UnsafePath(format!(
            "path exceeds {MAX_MANIFEST_PATH_LENGTH} characters"
        )));
    }
    if path.chars().any(|ch| ch == '\0' || ch.is_control()) {
        return Err(FolderManifestError::UnsafePath(
            "path contains null bytes or control characters".to_string(),
        ));
    }

    let normalized = path.replace('\\', "/");
    if normalized.starts_with('/') {
        return Err(FolderManifestError::UnsafePath(
            "absolute paths are not allowed".to_string(),
        ));
    }
    if starts_with_windows_drive_prefix(&normalized) {
        return Err(FolderManifestError::UnsafePath(
            "windows drive-prefixed paths are not allowed".to_string(),
        ));
    }

    let mut sanitized_segments = Vec::new();
    for segment in normalized.split('/') {
        if segment.is_empty() {
            return Err(FolderManifestError::UnsafePath(
                "path contains empty segments".to_string(),
            ));
        }
        if matches!(segment, "." | "..") {
            return Err(FolderManifestError::UnsafePath(
                "path traversal segments are not allowed".to_string(),
            ));
        }
        sanitized_segments.push(segment);
    }

    Ok(sanitized_segments.join("/"))
}

fn folder_name_from_path(folder_path: &Path) -> Result<String, FolderManifestError> {
    folder_path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or(FolderManifestError::InvalidFolderName)
}

fn collect_files(
    root: &Path,
    current_dir: &Path,
    max_files: usize,
    pending_files: &mut Vec<PendingFile>,
) -> Result<(), FolderManifestError> {
    let mut entries = fs::read_dir(current_dir)?.collect::<Result<Vec<_>, std::io::Error>>()?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        let file_type = metadata.file_type();

        if file_type.is_symlink() {
            warn!(path = %path.display(), "skipping symlink while generating folder manifest");
            continue;
        }
        if file_type.is_dir() {
            collect_files(root, &path, max_files, pending_files)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        if pending_files.len() >= max_files {
            return Err(FolderManifestError::FileLimitExceeded { max_files });
        }

        pending_files.push(PendingFile {
            absolute_path: path.clone(),
            relative_path: sanitize_manifest_path(&relative_path_string(root, &path)?)?,
            size: metadata.len(),
        });
    }

    Ok(())
}

fn relative_path_string(root: &Path, path: &Path) -> Result<String, FolderManifestError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| FolderManifestError::InvalidFolderPath)?;
    let mut parts = Vec::new();

    for component in relative.components() {
        match component {
            Component::Normal(value) => {
                let part = value.to_str().ok_or(FolderManifestError::NonUtf8Path)?;
                if part.contains('\\') {
                    return Err(FolderManifestError::UnsafePath(
                        "path components must not contain backslashes".to_string(),
                    ));
                }
                parts.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(FolderManifestError::UnsafePath(
                    "path must stay relative to the selected folder".to_string(),
                ));
            }
        }
    }

    if parts.is_empty() {
        return Err(FolderManifestError::UnsafePath(
            "path must refer to a file inside the selected folder".to_string(),
        ));
    }

    Ok(parts.join("/"))
}

fn hash_file(path: &Path) -> Result<String, FolderManifestError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; HASH_BUFFER_SIZE];

    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(hex::encode(hasher.finalize()))
}

fn ensure_manifest_within_protocol_limit(manifest: &FolderManifest) -> Result<(), FolderManifestError> {
    let message = ProtocolMessage::FolderManifest {
        folder_transfer_id: Uuid::nil().to_string(),
        manifest: manifest.clone(),
        sender_id: Uuid::nil().to_string(),
    };

    match message.to_json() {
        Ok(_) => Ok(()),
        Err(CoreError::Validation(_)) => Err(FolderManifestError::ManifestTooLarge),
        Err(error) => Err(FolderManifestError::Protocol(error)),
    }
}

fn starts_with_windows_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

struct PendingFile {
    absolute_path: PathBuf,
    relative_path: String,
    size: u64,
}

struct FolderProgressState {
    progress: FolderProgress,
    settled_bytes_sent: u64,
    active_file_bytes_sent: u64,
}

struct FolderAggregateProgressReporter {
    state: Arc<Mutex<FolderProgressState>>,
    file_total_bytes: u64,
    reporter: Option<Arc<dyn FolderProgressReporter>>,
}

impl TransferProgressReporter for FolderAggregateProgressReporter {
    fn report(&self, progress: TransferProgress) {
        let snapshot = {
            let mut state = self.state.lock().expect("lock folder progress state");
            let bytes_sent = progress.bytes_sent.min(self.file_total_bytes);
            if bytes_sent > state.active_file_bytes_sent {
                state.active_file_bytes_sent = bytes_sent;
                state.progress.sent_bytes = state.settled_bytes_sent + state.active_file_bytes_sent;
            }
            state.progress.clone()
        };

        emit_folder_progress(self.reporter.as_ref(), &snapshot);
    }
}

fn manifest_entry_path(root: &Path, relative_path: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for segment in relative_path.split('/') {
        path.push(segment);
    }
    path
}

fn progress_snapshot(state: &Arc<Mutex<FolderProgressState>>) -> FolderProgress {
    state
        .lock()
        .expect("lock folder progress state")
        .progress
        .clone()
}

fn emit_folder_progress(
    reporter: Option<&Arc<dyn FolderProgressReporter>>,
    progress: &FolderProgress,
) {
    if let Some(reporter) = reporter {
        reporter.report(progress.clone());
    }
}

fn update_folder_status(
    state: &Arc<Mutex<FolderProgressState>>,
    status: FolderTransferStatus,
    reporter: Option<&Arc<dyn FolderProgressReporter>>,
) {
    let snapshot = {
        let mut state = state.lock().expect("lock folder progress state");
        state.progress.status = status;
        state.progress.clone()
    };
    emit_folder_progress(reporter, &snapshot);
}

fn settle_active_file_progress(
    state: &Arc<Mutex<FolderProgressState>>,
    file_size: u64,
    completed: bool,
    reporter: Option<&Arc<dyn FolderProgressReporter>>,
) -> u64 {
    let snapshot = {
        let mut state = state.lock().expect("lock folder progress state");
        let attempted_bytes = state.active_file_bytes_sent.min(file_size);
        let settled_bytes = if completed { file_size } else { attempted_bytes };
        state.settled_bytes_sent += settled_bytes;
        state.active_file_bytes_sent = 0;
        state.progress.sent_bytes = state.settled_bytes_sent;
        if completed {
            state.progress.completed_files += 1;
        }
        (state.progress.clone(), attempted_bytes)
    };
    emit_folder_progress(reporter, &snapshot.0);
    snapshot.1
}

fn cancelled_outcome(
    manifest: &FolderManifest,
    folder_transfer_id: &str,
    state: &Arc<Mutex<FolderProgressState>>,
    reporter: Option<&Arc<dyn FolderProgressReporter>>,
) -> FolderTransferOutcome {
    cancelled_outcome_with_files(
        manifest,
        folder_transfer_id,
        state,
        reporter,
        Vec::new(),
    )
}

fn cancelled_outcome_with_files(
    manifest: &FolderManifest,
    folder_transfer_id: &str,
    state: &Arc<Mutex<FolderProgressState>>,
    reporter: Option<&Arc<dyn FolderProgressReporter>>,
    file_results: Vec<FolderFileTransferResult>,
) -> FolderTransferOutcome {
    update_folder_status(state, FolderTransferStatus::Cancelled, reporter);
    FolderTransferOutcome {
        folder_transfer_id: folder_transfer_id.to_string(),
        manifest: manifest.clone(),
        progress: progress_snapshot(state),
        file_results,
        rejection_reason: None,
    }
}
