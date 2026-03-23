use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jasmine_core::{DeviceId, ProtocolMessage, SettingsService};
use jasmine_crypto::{decrypt_chunk, FILE_KEY_LEN, NONCE_PREFIX_LEN};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

use crate::sender::resume_token_for_file_crypto_material;
use crate::sender::{TransferProgress, TransferProgressReporter, DEFAULT_CHUNK_SIZE};
use crate::FileCryptoMaterial;

const PARTIAL_SUFFIX: &str = ".jasmine-partial";

#[derive(Debug, Clone)]
pub struct FileReceiverConfig {
    pub chunk_size: usize,
    pub file_key: Option<[u8; FILE_KEY_LEN]>,
    pub nonce_prefix: Option<[u8; NONCE_PREFIX_LEN]>,
}

impl Default for FileReceiverConfig {
    fn default() -> Self {
        Self {
            chunk_size: DEFAULT_CHUNK_SIZE,
            file_key: None,
            nonce_prefix: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileOfferNotification {
    pub offer_id: String,
    pub sender_id: DeviceId,
    pub filename: String,
    pub size: u64,
    pub download_dir: PathBuf,
    pub has_enough_space: bool,
    pub required_space_bytes: u64,
    pub available_space_bytes: Option<u64>,
}

pub trait FileOfferNotifier: Send + Sync {
    fn offer_received(&self, offer: FileOfferNotification);
}

pub trait DiskSpaceChecker: Send + Sync {
    fn available_space(&self, path: &Path) -> std::io::Result<u64>;
}

#[derive(Debug, Error)]
pub enum FileReceiverError {
    #[error("path does not have a valid file name")]
    InvalidFileName,
    #[error("receiver config is invalid: {0}")]
    InvalidConfig(&'static str),
    #[error("unexpected signalling message: {0:?}")]
    UnexpectedMessage(Box<ProtocolMessage>),
    #[error("file offer is not pending: {0}")]
    UnknownOffer(String),
    #[error("could not resolve the default download directory")]
    HomeDirectoryUnavailable,
    #[error("insufficient disk space: required {required_bytes} bytes, available {available_bytes} bytes")]
    InsufficientDiskSpace {
        required_bytes: u64,
        available_bytes: u64,
    },
    #[error("file integrity check failed: expected {expected_sha256}, got {actual_sha256}")]
    IntegrityMismatch {
        expected_sha256: String,
        actual_sha256: String,
    },
    #[error("file size mismatch: expected {expected_bytes} bytes, received {actual_bytes} bytes")]
    SizeMismatch {
        expected_bytes: u64,
        actual_bytes: u64,
    },
    #[error("settings error: {0}")]
    Settings(#[from] jasmine_core::CoreError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("file receiver signalling error: {0}")]
    Signal(String),
    #[error("file transfer was cancelled")]
    Cancelled,
    #[error("file transfer crypto error: {0}")]
    Crypto(String),
}

pub trait FileReceiverSignal: Send + Sync {
    async fn send_message(
        &self,
        peer_id: &DeviceId,
        message: ProtocolMessage,
    ) -> Result<(), FileReceiverError>;

    async fn wait_for_response(&self, offer_id: &str)
        -> Result<ProtocolMessage, FileReceiverError>;

    async fn file_crypto_material(
        &self,
        peer_id: &DeviceId,
        file_id: &str,
    ) -> Result<FileCryptoMaterial, FileReceiverError>;
}

pub struct FileReceiver<S> {
    signal: S,
    settings: SettingsService,
    config: FileReceiverConfig,
    disk_space_checker: Arc<dyn DiskSpaceChecker>,
    offer_notifier: Option<Arc<dyn FileOfferNotifier>>,
    pending_offers: Mutex<HashMap<String, PendingOffer>>,
}

impl<S> FileReceiver<S>
where
    S: FileReceiverSignal,
{
    pub(crate) fn chunk_size(&self) -> usize {
        self.config.chunk_size
    }

    pub(crate) async fn current_resume_token(
        &self,
        peer_id: &DeviceId,
        file_id: &str,
    ) -> Result<Option<String>, FileReceiverError> {
        let (file_key, nonce_prefix) =
            resolve_file_crypto_material(&self.signal, &self.config, peer_id, file_id).await?;
        Ok(Some(resume_token_for_file_crypto_material(
            &file_key,
            &nonce_prefix,
        )))
    }

    pub fn new(signal: S, settings: SettingsService) -> Self {
        Self::with_dependencies(
            signal,
            settings,
            FileReceiverConfig::default(),
            Arc::new(SystemDiskSpaceChecker),
            None,
        )
    }

    pub fn with_config(signal: S, settings: SettingsService, config: FileReceiverConfig) -> Self {
        Self::with_dependencies(
            signal,
            settings,
            config,
            Arc::new(SystemDiskSpaceChecker),
            None,
        )
    }

    pub fn with_dependencies(
        signal: S,
        settings: SettingsService,
        config: FileReceiverConfig,
        disk_space_checker: Arc<dyn DiskSpaceChecker>,
        offer_notifier: Option<Arc<dyn FileOfferNotifier>>,
    ) -> Self {
        Self {
            signal,
            settings,
            config,
            disk_space_checker,
            offer_notifier,
            pending_offers: Mutex::new(HashMap::new()),
        }
    }

    pub async fn receive_offer(
        &self,
        sender_id: DeviceId,
        sender_address: SocketAddr,
        message: ProtocolMessage,
    ) -> Result<FileOfferNotification, FileReceiverError> {
        let download_dir = resolve_download_dir(&self.settings.load()?.download_dir)?;
        self.receive_offer_with_target(
            sender_id,
            sender_address,
            message,
            PendingSaveTarget::Directory(download_dir.clone()),
            download_dir,
        )
        .await
    }

    pub(crate) async fn receive_offer_to_path(
        &self,
        sender_id: DeviceId,
        sender_address: SocketAddr,
        message: ProtocolMessage,
        final_path: PathBuf,
    ) -> Result<FileOfferNotification, FileReceiverError> {
        let download_dir = final_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        self.receive_offer_with_target(
            sender_id,
            sender_address,
            message,
            PendingSaveTarget::ExactPath(final_path),
            download_dir,
        )
        .await
    }

    async fn receive_offer_with_target(
        &self,
        sender_id: DeviceId,
        sender_address: SocketAddr,
        message: ProtocolMessage,
        target: PendingSaveTarget,
        download_dir: PathBuf,
    ) -> Result<FileOfferNotification, FileReceiverError> {
        let (offer_id, filename, size, sha256, transfer_port) = match message {
            ProtocolMessage::FileOffer {
                id,
                filename,
                size,
                sha256,
                transfer_port,
            } => (
                id,
                sanitize_filename(&filename)?,
                size,
                sha256,
                transfer_port,
            ),
            other => return Err(FileReceiverError::UnexpectedMessage(Box::new(other))),
        };
        let (available_space_bytes, required_space_bytes) =
            self.query_disk_space(&download_dir, size)?;
        let notification = FileOfferNotification {
            offer_id: offer_id.clone(),
            sender_id: sender_id.clone(),
            filename: filename.clone(),
            size,
            download_dir: download_dir.clone(),
            has_enough_space: available_space_bytes >= required_space_bytes,
            required_space_bytes,
            available_space_bytes: Some(available_space_bytes),
        };

        self.pending_offers
            .lock()
            .expect("lock pending offers")
            .insert(
                offer_id.clone(),
                PendingOffer {
                    offer_id,
                    sender_id,
                    sender_address,
                    filename,
                    size,
                    sha256,
                    transfer_port,
                    target,
                },
            );

        if let Some(notifier) = self.offer_notifier.as_ref() {
            notifier.offer_received(notification.clone());
        }

        Ok(notification)
    }

    pub async fn accept_offer(
        &self,
        offer_id: &str,
        progress: Option<Arc<dyn TransferProgressReporter>>,
    ) -> Result<PathBuf, FileReceiverError> {
        self.accept_offer_with_cancellation(offer_id, CancellationToken::new(), progress)
            .await
    }

    pub async fn accept_offer_with_resume_offset(
        &self,
        offer_id: &str,
        resume_offset: Option<u64>,
        progress: Option<Arc<dyn TransferProgressReporter>>,
    ) -> Result<PathBuf, FileReceiverError> {
        self.accept_offer_with_resume_offset_and_cancellation(
            offer_id,
            resume_offset,
            CancellationToken::new(),
            progress,
        )
        .await
    }

    pub async fn accept_offer_with_cancellation(
        &self,
        offer_id: &str,
        cancellation: CancellationToken,
        progress: Option<Arc<dyn TransferProgressReporter>>,
    ) -> Result<PathBuf, FileReceiverError> {
        self.accept_offer_with_resume_offset_and_cancellation(
            offer_id,
            None,
            cancellation,
            progress,
        )
        .await
    }

    pub(crate) async fn accept_offer_with_resume_offset_and_cancellation(
        &self,
        offer_id: &str,
        resume_offset: Option<u64>,
        cancellation: CancellationToken,
        progress: Option<Arc<dyn TransferProgressReporter>>,
    ) -> Result<PathBuf, FileReceiverError> {
        if self.config.chunk_size == 0 {
            return Err(FileReceiverError::InvalidConfig(
                "chunk_size must be greater than zero",
            ));
        }

        let pending = self.take_pending_offer(offer_id)?;
        let (file_key, nonce_prefix) = resolve_file_crypto_material(
            &self.signal,
            &self.config,
            &pending.sender_id,
            &pending.offer_id,
        )
        .await?;
        let receive_target =
            determine_receive_target(&pending, resume_offset, self.config.chunk_size).await?;
        let (available_space_bytes, required_space_bytes) =
            self.query_disk_space(&pending.target.parent_dir()?, pending.size)?;

        if available_space_bytes < required_space_bytes {
            self.signal
                .send_message(
                    &pending.sender_id,
                    ProtocolMessage::FileReject {
                        offer_id: pending.offer_id.clone(),
                        reason: Some("insufficient disk space".to_string()),
                    },
                )
                .await?;

            return Err(FileReceiverError::InsufficientDiskSpace {
                required_bytes: required_space_bytes,
                available_bytes: available_space_bytes,
            });
        }

        let start_offset = if let Some(offset) = receive_target.resume_offset {
            self.signal
                .send_message(
                    &pending.sender_id,
                    ProtocolMessage::FileResumeRequest {
                        offer_id: pending.offer_id.clone(),
                        offset,
                    },
                )
                .await?;

            match self.signal.wait_for_response(&pending.offer_id).await? {
                ProtocolMessage::FileResumeAccept {
                    offer_id,
                    offset: accepted_offset,
                } if offer_id == pending.offer_id && accepted_offset == offset => accepted_offset,
                other => return Err(FileReceiverError::UnexpectedMessage(Box::new(other))),
            }
        } else {
            self.signal
                .send_message(
                    &pending.sender_id,
                    ProtocolMessage::FileAccept {
                        offer_id: pending.offer_id.clone(),
                    },
                )
                .await?;
            0
        };

        self.receive_file(
            pending,
            ReceiveFileContext {
                final_path: &receive_target.final_path,
                partial_path: &receive_target.partial_path,
                start_offset,
                file_key: &file_key,
                nonce_prefix: &nonce_prefix,
                cancellation: &cancellation,
                progress: progress.as_deref(),
            },
        )
        .await
    }

    pub async fn reject_offer(
        &self,
        offer_id: &str,
        reason: Option<String>,
    ) -> Result<(), FileReceiverError> {
        let pending = self.take_pending_offer(offer_id)?;
        self.signal
            .send_message(
                &pending.sender_id,
                ProtocolMessage::FileReject {
                    offer_id: pending.offer_id,
                    reason,
                },
            )
            .await
    }

    fn take_pending_offer(&self, offer_id: &str) -> Result<PendingOffer, FileReceiverError> {
        self.pending_offers
            .lock()
            .expect("lock pending offers")
            .remove(offer_id)
            .ok_or_else(|| FileReceiverError::UnknownOffer(offer_id.to_string()))
    }

    fn query_disk_space(
        &self,
        download_dir: &Path,
        file_size: u64,
    ) -> Result<(u64, u64), FileReceiverError> {
        let available_space = self
            .disk_space_checker
            .available_space(&existing_space_check_path(download_dir))?;
        Ok((available_space, required_space(file_size)))
    }

    async fn receive_file(
        &self,
        pending: PendingOffer,
        context: ReceiveFileContext<'_>,
    ) -> Result<PathBuf, FileReceiverError> {
        tokio::select! {
            _ = context.cancellation.cancelled() => return Err(FileReceiverError::Cancelled),
            create_dir_outcome = fs::create_dir_all(pending.target.parent_dir()?) => create_dir_outcome?,
        }
        let endpoint = SocketAddr::new(pending.sender_address.ip(), pending.transfer_port);
        let mut stream = tokio::select! {
            _ = context.cancellation.cancelled() => return Err(FileReceiverError::Cancelled),
            stream = TcpStream::connect(endpoint) => stream?,
        };
        let receive_outcome = self
            .receive_into_paths(&pending, &context, &mut stream)
            .await;

        if receive_outcome
            .as_ref()
            .err()
            .is_some_and(should_remove_partial_on_error)
        {
            let _ = fs::remove_file(context.partial_path).await;
        }

        receive_outcome
    }

    async fn receive_into_paths(
        &self,
        pending: &PendingOffer,
        context: &ReceiveFileContext<'_>,
        stream: &mut TcpStream,
    ) -> Result<PathBuf, FileReceiverError> {
        let (mut file, mut hasher, mut bytes_received) = if context.start_offset > 0 {
            let (hasher, existing_bytes) = hash_existing_partial(
                context.partial_path,
                self.config.chunk_size,
                context.cancellation,
            )
            .await?;
            if existing_bytes != context.start_offset {
                return Err(FileReceiverError::SizeMismatch {
                    expected_bytes: context.start_offset,
                    actual_bytes: existing_bytes,
                });
            }

            let mut options = OpenOptions::new();
            options.create(true).append(true);
            let file = tokio::select! {
                _ = context.cancellation.cancelled() => return Err(FileReceiverError::Cancelled),
                file = options.open(context.partial_path) => file?,
            };
            (file, hasher, existing_bytes)
        } else {
            let file = tokio::select! {
                _ = context.cancellation.cancelled() => return Err(FileReceiverError::Cancelled),
                file = File::create(context.partial_path) => file?,
            };
            (file, Sha256::new(), 0)
        };
        let started_at = Instant::now();
        let mut chunk_index = chunk_index_for_offset(context.start_offset, self.config.chunk_size)?;

        report_progress(
            context.progress,
            &pending.offer_id,
            bytes_received,
            pending.size,
            0,
        );

        loop {
            let encrypted_chunk = tokio::select! {
                _ = context.cancellation.cancelled() => return Err(FileReceiverError::Cancelled),
                read = read_encrypted_chunk(stream) => read?,
            };
            let Some(encrypted_chunk) = encrypted_chunk else {
                break;
            };

            let plaintext = decrypt_chunk(
                context.file_key,
                context.nonce_prefix,
                chunk_index,
                &encrypted_chunk,
            )
            .map_err(|error| FileReceiverError::Crypto(error.to_string()))?;

            tokio::select! {
                _ = context.cancellation.cancelled() => return Err(FileReceiverError::Cancelled),
                write_outcome = file.write_all(&plaintext) => write_outcome?,
            }
            hasher.update(&plaintext);
            bytes_received += plaintext.len() as u64;
            chunk_index = chunk_index
                .checked_add(1)
                .ok_or_else(|| FileReceiverError::Crypto("chunk index overflow".to_string()))?;
            report_progress(
                context.progress,
                &pending.offer_id,
                bytes_received,
                pending.size,
                calculate_speed(bytes_received, started_at.elapsed()),
            );
        }

        tokio::select! {
            _ = context.cancellation.cancelled() => return Err(FileReceiverError::Cancelled),
            flush_outcome = file.flush() => flush_outcome?,
        }
        tokio::select! {
            _ = context.cancellation.cancelled() => return Err(FileReceiverError::Cancelled),
            sync_outcome = file.sync_all() => sync_outcome?,
        }
        drop(file);

        if bytes_received != pending.size {
            return Err(FileReceiverError::SizeMismatch {
                expected_bytes: pending.size,
                actual_bytes: bytes_received,
            });
        }

        let actual_sha256 = hex::encode(hasher.finalize());
        if actual_sha256 != pending.sha256 {
            return Err(FileReceiverError::IntegrityMismatch {
                expected_sha256: pending.sha256.clone(),
                actual_sha256,
            });
        }

        tokio::select! {
            _ = context.cancellation.cancelled() => return Err(FileReceiverError::Cancelled),
            rename_outcome = fs::rename(context.partial_path, context.final_path) => rename_outcome?,
        }
        Ok(context.final_path.to_path_buf())
    }
}

struct PendingOffer {
    offer_id: String,
    sender_id: DeviceId,
    sender_address: SocketAddr,
    filename: String,
    size: u64,
    sha256: String,
    transfer_port: u16,
    target: PendingSaveTarget,
}

enum PendingSaveTarget {
    Directory(PathBuf),
    ExactPath(PathBuf),
}

struct ReceiveTarget {
    final_path: PathBuf,
    partial_path: PathBuf,
    resume_offset: Option<u64>,
}

struct ReceiveFileContext<'a> {
    final_path: &'a Path,
    partial_path: &'a Path,
    start_offset: u64,
    file_key: &'a [u8; FILE_KEY_LEN],
    nonce_prefix: &'a [u8; NONCE_PREFIX_LEN],
    cancellation: &'a CancellationToken,
    progress: Option<&'a dyn TransferProgressReporter>,
}

impl PendingSaveTarget {
    fn parent_dir(&self) -> Result<PathBuf, FileReceiverError> {
        match self {
            Self::Directory(path) => Ok(path.clone()),
            Self::ExactPath(path) => Ok(path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."))),
        }
    }

    fn resolve_final_path(&self, filename: &str) -> Result<PathBuf, FileReceiverError> {
        match self {
            Self::Directory(download_dir) => resolve_target_path(download_dir, filename),
            Self::ExactPath(path) => Ok(path.clone()),
        }
    }

    fn resolve_resume_path(&self, filename: &str) -> Result<PathBuf, FileReceiverError> {
        match self {
            Self::Directory(download_dir) => Ok(download_dir.join(filename)),
            Self::ExactPath(path) => Ok(path.clone()),
        }
    }
}

struct SystemDiskSpaceChecker;

impl DiskSpaceChecker for SystemDiskSpaceChecker {
    fn available_space(&self, path: &Path) -> std::io::Result<u64> {
        fs4::available_space(path)
    }
}

pub(crate) fn sanitize_filename(filename: &str) -> Result<String, FileReceiverError> {
    Path::new(filename)
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or(FileReceiverError::InvalidFileName)
}

fn resolve_download_dir(raw: &str) -> Result<PathBuf, FileReceiverError> {
    if raw == "~" {
        home_dir().ok_or(FileReceiverError::HomeDirectoryUnavailable)
    } else if let Some(stripped) = raw.strip_prefix("~/") {
        Ok(home_dir()
            .ok_or(FileReceiverError::HomeDirectoryUnavailable)?
            .join(stripped))
    } else {
        Ok(PathBuf::from(raw))
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn existing_space_check_path(download_dir: &Path) -> PathBuf {
    let mut candidate = download_dir.to_path_buf();

    while !candidate.exists() {
        if !candidate.pop() {
            return PathBuf::from(".");
        }
    }

    candidate
}

fn required_space(file_size: u64) -> u64 {
    file_size.saturating_add(file_size.div_ceil(10))
}

fn resolve_target_path(download_dir: &Path, filename: &str) -> Result<PathBuf, FileReceiverError> {
    let candidate = download_dir.join(filename);
    if !candidate.exists() && !partial_path_for(&candidate)?.exists() {
        return Ok(candidate);
    }

    let path = Path::new(filename);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or(FileReceiverError::InvalidFileName)?;
    let extension = path.extension().and_then(|value| value.to_str());

    for index in 1..u32::MAX {
        let renamed = match extension {
            Some(extension) if !extension.is_empty() => format!("{stem} ({index}).{extension}"),
            _ => format!("{stem} ({index})"),
        };
        let candidate = download_dir.join(renamed);
        if !candidate.exists() && !partial_path_for(&candidate)?.exists() {
            return Ok(candidate);
        }
    }

    Err(FileReceiverError::InvalidFileName)
}

fn partial_path_for(final_path: &Path) -> Result<PathBuf, FileReceiverError> {
    let file_name = final_path
        .file_name()
        .ok_or(FileReceiverError::InvalidFileName)?;
    let mut partial_name = file_name.to_os_string();
    partial_name.push(PARTIAL_SUFFIX);
    Ok(final_path.with_file_name(partial_name))
}

async fn read_encrypted_chunk(stream: &mut TcpStream) -> Result<Option<Vec<u8>>, std::io::Error> {
    let mut first_prefix_byte = [0_u8; 1];
    let first_read = stream.read(&mut first_prefix_byte).await?;
    if first_read == 0 {
        return Ok(None);
    }

    let mut remaining_prefix = [0_u8; 3];
    stream.read_exact(&mut remaining_prefix).await?;
    let chunk_len = u32::from_be_bytes([
        first_prefix_byte[0],
        remaining_prefix[0],
        remaining_prefix[1],
        remaining_prefix[2],
    ]) as usize;

    let mut encrypted_chunk = vec![0_u8; chunk_len];
    stream.read_exact(&mut encrypted_chunk).await?;
    Ok(Some(encrypted_chunk))
}

async fn determine_receive_target(
    pending: &PendingOffer,
    requested_resume_offset: Option<u64>,
    chunk_size: usize,
) -> Result<ReceiveTarget, FileReceiverError> {
    if let Some(offset) = requested_resume_offset {
        let final_path = pending.target.resolve_resume_path(&pending.filename)?;
        let partial_path = partial_path_for(&final_path)?;
        if is_resumable_partial(&partial_path, offset, pending.size, chunk_size).await? {
            return Ok(ReceiveTarget {
                final_path,
                partial_path,
                resume_offset: Some(offset),
            });
        }

        return Ok(ReceiveTarget {
            final_path,
            partial_path,
            resume_offset: None,
        });
    }

    let final_path = pending.target.resolve_final_path(&pending.filename)?;
    let partial_path = partial_path_for(&final_path)?;
    Ok(ReceiveTarget {
        final_path,
        partial_path,
        resume_offset: None,
    })
}

async fn is_resumable_partial(
    partial_path: &Path,
    offset: u64,
    total_bytes: u64,
    chunk_size: usize,
) -> Result<bool, FileReceiverError> {
    if offset == 0 || offset > total_bytes {
        return Ok(false);
    }

    let chunk_size = chunk_size as u64;
    if !offset.is_multiple_of(chunk_size) {
        return Ok(false);
    }

    let metadata = match fs::metadata(partial_path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(FileReceiverError::Io(error)),
    };

    Ok(metadata.len() == offset)
}

async fn hash_existing_partial(
    partial_path: &Path,
    chunk_size: usize,
    cancellation: &CancellationToken,
) -> Result<(Sha256, u64), FileReceiverError> {
    let mut file = tokio::select! {
        _ = cancellation.cancelled() => return Err(FileReceiverError::Cancelled),
        file = File::open(partial_path) => file?,
    };
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; chunk_size];
    let mut bytes_hashed = 0_u64;

    loop {
        let read = tokio::select! {
            _ = cancellation.cancelled() => return Err(FileReceiverError::Cancelled),
            read = file.read(&mut buffer) => read?,
        };

        if read == 0 {
            break;
        }

        hasher.update(&buffer[..read]);
        bytes_hashed += read as u64;
    }

    Ok((hasher, bytes_hashed))
}

fn chunk_index_for_offset(offset: u64, chunk_size: usize) -> Result<u32, FileReceiverError> {
    let chunk_size = chunk_size as u64;
    if offset != 0 && !offset.is_multiple_of(chunk_size) {
        return Err(FileReceiverError::Crypto(format!(
            "resume offset {offset} is not aligned to chunk size {chunk_size}"
        )));
    }

    let chunk_index = offset / chunk_size;
    u32::try_from(chunk_index)
        .map_err(|_| FileReceiverError::Crypto(format!("chunk index overflow for offset {offset}")))
}

fn should_remove_partial_on_error(error: &FileReceiverError) -> bool {
    matches!(
        error,
        FileReceiverError::IntegrityMismatch { .. } | FileReceiverError::Crypto(_)
    )
}

async fn resolve_file_crypto_material<S>(
    signal: &S,
    config: &FileReceiverConfig,
    peer_id: &DeviceId,
    file_id: &str,
) -> Result<FileCryptoMaterial, FileReceiverError>
where
    S: FileReceiverSignal,
{
    match (config.file_key, config.nonce_prefix) {
        (Some(file_key), Some(nonce_prefix)) => Ok((file_key, nonce_prefix)),
        (std::option::Option::None, std::option::Option::None) => {
            signal.file_crypto_material(peer_id, file_id).await
        }
        _ => Err(FileReceiverError::InvalidConfig(
            "file_key and nonce_prefix must be provided together",
        )),
    }
}

fn calculate_speed(bytes_transferred: u64, elapsed: Duration) -> u64 {
    let elapsed_secs = elapsed.as_secs_f64();
    if elapsed_secs <= f64::EPSILON {
        0
    } else {
        (bytes_transferred as f64 / elapsed_secs).round() as u64
    }
}

fn report_progress(
    reporter: Option<&dyn TransferProgressReporter>,
    transfer_id: &str,
    bytes_transferred: u64,
    total_bytes: u64,
    speed_bps: u64,
) {
    if let Some(reporter) = reporter {
        reporter.report(TransferProgress {
            transfer_id: transfer_id.to_string(),
            bytes_sent: bytes_transferred,
            total_bytes,
            speed_bps,
        });
    }
}
