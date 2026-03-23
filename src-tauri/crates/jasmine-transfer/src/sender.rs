use std::io::SeekFrom;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jasmine_core::{DeviceId, ProtocolMessage};
use jasmine_crypto::{encrypt_chunk, FILE_KEY_LEN, NONCE_PREFIX_LEN};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::FileCryptoMaterial;

pub const DEFAULT_CHUNK_SIZE: usize = 64 * 1024;
const DEFAULT_ACCEPT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct FileSenderConfig {
    pub bind_address: String,
    pub accept_timeout: Duration,
    pub chunk_size: usize,
    pub file_key: Option<[u8; FILE_KEY_LEN]>,
    pub nonce_prefix: Option<[u8; NONCE_PREFIX_LEN]>,
}

impl Default for FileSenderConfig {
    fn default() -> Self {
        Self {
            bind_address: "0.0.0.0:0".to_string(),
            accept_timeout: DEFAULT_ACCEPT_TIMEOUT,
            chunk_size: DEFAULT_CHUNK_SIZE,
            file_key: None,
            nonce_prefix: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferProgress {
    pub transfer_id: String,
    pub bytes_sent: u64,
    pub total_bytes: u64,
    pub speed_bps: u64,
}

pub trait TransferProgressReporter: Send + Sync {
    fn report(&self, progress: TransferProgress);
}

pub trait SignalResponseRouter: Send + Sync {
    fn handle_signal_response(&self, _message: ProtocolMessage) -> bool {
        false
    }
}

#[cfg(feature = "tauri-channel")]
pub struct TauriChannelProgressReporter(pub tauri::ipc::Channel<TransferProgress>);

#[cfg(feature = "tauri-channel")]
impl TransferProgressReporter for TauriChannelProgressReporter {
    fn report(&self, progress: TransferProgress) {
        let _ = self.0.send(progress);
    }
}

#[derive(Debug, Error)]
pub enum FileSenderError {
    #[error("path does not have a valid file name")]
    InvalidFileName,
    #[error("sender config is invalid: {0}")]
    InvalidConfig(&'static str),
    #[error("file sender signalling channel closed")]
    SignalClosed,
    #[error("file sender signalling error: {0}")]
    Signal(String),
    #[error("unexpected signalling response: {0:?}")]
    UnexpectedResponse(Box<ProtocolMessage>),
    #[error("file transfer was rejected: {0}")]
    Rejected(String),
    #[error("receiver did not connect within the accept timeout")]
    AcceptTimedOut,
    #[error("file transfer was cancelled")]
    Cancelled,
    #[error("file transfer crypto error: {0}")]
    Crypto(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub trait FileSenderSignal: Send + Sync {
    async fn send_message(
        &self,
        peer_id: &DeviceId,
        message: ProtocolMessage,
    ) -> Result<(), FileSenderError>;

    async fn wait_for_response(&self, offer_id: &str) -> Result<ProtocolMessage, FileSenderError>;

    async fn file_crypto_material(
        &self,
        peer_id: &DeviceId,
        file_id: &str,
    ) -> Result<FileCryptoMaterial, FileSenderError>;
}

pub struct FileSender<S> {
    signal: S,
    config: FileSenderConfig,
}

impl<S> FileSender<S>
where
    S: FileSenderSignal,
{
    pub(crate) async fn current_resume_token(
        &self,
        peer_id: &DeviceId,
        file_id: &str,
    ) -> Result<Option<String>, FileSenderError> {
        let (file_key, nonce_prefix) =
            resolve_file_crypto_material(&self.signal, &self.config, peer_id, file_id).await?;
        Ok(Some(resume_token_for_file_crypto_material(
            &file_key,
            &nonce_prefix,
        )))
    }

    pub fn new(signal: S) -> Self {
        Self::with_config(signal, FileSenderConfig::default())
    }

    pub fn with_config(signal: S, config: FileSenderConfig) -> Self {
        Self { signal, config }
    }

    pub async fn send_file<P>(
        &self,
        peer_id: DeviceId,
        file_path: P,
        cancellation: CancellationToken,
        progress: Option<Arc<dyn TransferProgressReporter>>,
    ) -> Result<(), FileSenderError>
    where
        P: AsRef<Path>,
    {
        self.send_file_with_id(
            Uuid::new_v4().to_string(),
            peer_id,
            file_path,
            cancellation,
            progress,
        )
        .await
    }

    pub async fn send_file_with_id<P>(
        &self,
        transfer_id: String,
        peer_id: DeviceId,
        file_path: P,
        cancellation: CancellationToken,
        progress: Option<Arc<dyn TransferProgressReporter>>,
    ) -> Result<(), FileSenderError>
    where
        P: AsRef<Path>,
    {
        let (file_key, nonce_prefix) =
            resolve_file_crypto_material(&self.signal, &self.config, &peer_id, &transfer_id)
                .await?;

        if self.config.chunk_size == 0 {
            return Err(FileSenderError::InvalidConfig(
                "chunk_size must be greater than zero",
            ));
        }

        let file_path = file_path.as_ref();
        let filename = file_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(FileSenderError::InvalidFileName)?
            .to_string();
        let total_bytes = tokio::fs::metadata(file_path).await?.len();
        let sha256 = hash_file(file_path, self.config.chunk_size, &cancellation).await?;
        let listener = TcpListener::bind(&self.config.bind_address).await?;
        let transfer_port = listener.local_addr()?.port();

        self.signal
            .send_message(
                &peer_id,
                ProtocolMessage::FileOffer {
                    id: transfer_id.clone(),
                    filename,
                    size: total_bytes,
                    sha256,
                    transfer_port,
                },
            )
            .await?;

        let response = tokio::select! {
            _ = cancellation.cancelled() => return Err(FileSenderError::Cancelled),
            response = self.signal.wait_for_response(&transfer_id) => response?,
        };

        let start_offset = match response {
            ProtocolMessage::FileAccept { offer_id } if offer_id == transfer_id => 0,
            ProtocolMessage::FileResumeRequest { offer_id, offset } if offer_id == transfer_id => {
                validate_resume_offset(offset, total_bytes, self.config.chunk_size)?;
                self.signal
                    .send_message(
                        &peer_id,
                        ProtocolMessage::FileResumeAccept {
                            offer_id: transfer_id.clone(),
                            offset,
                        },
                    )
                    .await?;
                offset
            }
            ProtocolMessage::FileReject { offer_id, reason } if offer_id == transfer_id => {
                return Err(FileSenderError::Rejected(
                    reason.unwrap_or_else(|| "receiver rejected the transfer".to_string()),
                ));
            }
            other => return Err(FileSenderError::UnexpectedResponse(Box::new(other))),
        };

        report_progress(
            progress.as_deref(),
            &transfer_id,
            start_offset,
            total_bytes,
            0,
        );

        let mut stream =
            wait_for_receiver(listener, self.config.accept_timeout, &cancellation).await?;
        let stream_context = StreamFileContext {
            transfer_id: &transfer_id,
            total_bytes,
            chunk_size: self.config.chunk_size,
            start_offset,
            file_key: &file_key,
            nonce_prefix: &nonce_prefix,
            cancellation: &cancellation,
            progress: progress.as_deref(),
        };
        stream_file(file_path, &stream_context, &mut stream).await
    }

    pub fn handle_signal_response(&self, message: ProtocolMessage) -> bool
    where
        S: SignalResponseRouter,
    {
        self.signal.handle_signal_response(message)
    }
}

async fn hash_file(
    file_path: &Path,
    chunk_size: usize,
    cancellation: &CancellationToken,
) -> Result<String, FileSenderError> {
    let mut file = File::open(file_path).await?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; chunk_size];

    loop {
        let read = tokio::select! {
            _ = cancellation.cancelled() => return Err(FileSenderError::Cancelled),
            read = file.read(&mut buffer) => read?,
        };

        if read == 0 {
            break;
        }

        hasher.update(&buffer[..read]);
    }

    Ok(hex::encode(hasher.finalize()))
}

async fn wait_for_receiver(
    listener: TcpListener,
    accept_timeout: Duration,
    cancellation: &CancellationToken,
) -> Result<TcpStream, FileSenderError> {
    let accept = async {
        let (stream, _) = listener.accept().await?;
        Ok::<TcpStream, std::io::Error>(stream)
    };

    tokio::select! {
        _ = cancellation.cancelled() => Err(FileSenderError::Cancelled),
        result = time::timeout(accept_timeout, accept) => match result {
            Ok(stream) => Ok(stream?),
            Err(_) => Err(FileSenderError::AcceptTimedOut),
        },
    }
}

struct StreamFileContext<'a> {
    transfer_id: &'a str,
    total_bytes: u64,
    chunk_size: usize,
    start_offset: u64,
    file_key: &'a [u8; FILE_KEY_LEN],
    nonce_prefix: &'a [u8; NONCE_PREFIX_LEN],
    cancellation: &'a CancellationToken,
    progress: Option<&'a dyn TransferProgressReporter>,
}

async fn stream_file(
    file_path: &Path,
    context: &StreamFileContext<'_>,
    stream: &mut TcpStream,
) -> Result<(), FileSenderError> {
    let mut file = File::open(file_path).await?;
    if context.start_offset > 0 {
        file.seek(SeekFrom::Start(context.start_offset)).await?;
    }
    let mut buffer = vec![0; context.chunk_size];
    let started_at = Instant::now();
    let mut bytes_sent = context.start_offset;
    let mut chunk_index = chunk_index_for_offset(context.start_offset, context.chunk_size)?;

    loop {
        let read = tokio::select! {
            _ = context.cancellation.cancelled() => return Err(FileSenderError::Cancelled),
            read = file.read(&mut buffer) => read?,
        };

        if read == 0 {
            break;
        }

        let encrypted_chunk = encrypt_chunk(
            context.file_key,
            context.nonce_prefix,
            chunk_index,
            &buffer[..read],
        )
        .map_err(|error| FileSenderError::Crypto(error.to_string()))?;
        let encrypted_len = u32::try_from(encrypted_chunk.len()).map_err(|_| {
            FileSenderError::Crypto(format!(
                "encrypted chunk too large: {} bytes",
                encrypted_chunk.len()
            ))
        })?;
        let length_prefix = encrypted_len.to_be_bytes();

        tokio::select! {
            _ = context.cancellation.cancelled() => return Err(FileSenderError::Cancelled),
            result = stream.write_all(&length_prefix) => result?,
        }

        tokio::select! {
            _ = context.cancellation.cancelled() => return Err(FileSenderError::Cancelled),
            result = stream.write_all(&encrypted_chunk) => result?,
        }

        bytes_sent += read as u64;
        chunk_index = chunk_index
            .checked_add(1)
            .ok_or_else(|| FileSenderError::Crypto("chunk index overflow".to_string()))?;
        report_progress(
            context.progress,
            context.transfer_id,
            bytes_sent,
            context.total_bytes,
            calculate_speed(bytes_sent, started_at.elapsed()),
        );
    }

    if context.total_bytes == 0 {
        report_progress(context.progress, context.transfer_id, 0, 0, 0);
    }

    stream.shutdown().await?;
    Ok(())
}

async fn resolve_file_crypto_material<S>(
    signal: &S,
    config: &FileSenderConfig,
    peer_id: &DeviceId,
    file_id: &str,
) -> Result<FileCryptoMaterial, FileSenderError>
where
    S: FileSenderSignal,
{
    match (config.file_key, config.nonce_prefix) {
        (Some(file_key), Some(nonce_prefix)) => Ok((file_key, nonce_prefix)),
        (std::option::Option::None, std::option::Option::None) => {
            signal.file_crypto_material(peer_id, file_id).await
        }
        _ => Err(FileSenderError::InvalidConfig(
            "file_key and nonce_prefix must be provided together",
        )),
    }
}

pub(crate) fn resume_token_for_file_crypto_material(
    file_key: &[u8; FILE_KEY_LEN],
    nonce_prefix: &[u8; NONCE_PREFIX_LEN],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"jasmine-transfer-resume/v1");
    hasher.update(file_key);
    hasher.update(nonce_prefix);
    hex::encode(hasher.finalize())
}

fn validate_resume_offset(
    offset: u64,
    total_bytes: u64,
    chunk_size: usize,
) -> Result<(), FileSenderError> {
    if offset > total_bytes {
        return Err(FileSenderError::Signal(format!(
            "resume offset {offset} exceeds total file size {total_bytes}"
        )));
    }

    let chunk_size = chunk_size as u64;
    if !offset.is_multiple_of(chunk_size) {
        return Err(FileSenderError::Signal(format!(
            "resume offset {offset} is not aligned to chunk size {chunk_size}"
        )));
    }

    let _ = chunk_index_for_offset(offset, chunk_size as usize)?;
    Ok(())
}

fn chunk_index_for_offset(offset: u64, chunk_size: usize) -> Result<u32, FileSenderError> {
    let chunk_size = chunk_size as u64;
    let chunk_index = offset / chunk_size;
    u32::try_from(chunk_index)
        .map_err(|_| FileSenderError::Crypto(format!("chunk index overflow for offset {offset}")))
}

fn calculate_speed(bytes_sent: u64, elapsed: Duration) -> u64 {
    let elapsed_secs = elapsed.as_secs_f64();
    if elapsed_secs <= f64::EPSILON {
        0
    } else {
        (bytes_sent as f64 / elapsed_secs).round() as u64
    }
}

fn report_progress(
    reporter: Option<&dyn TransferProgressReporter>,
    transfer_id: &str,
    bytes_sent: u64,
    total_bytes: u64,
    speed_bps: u64,
) {
    if let Some(reporter) = reporter {
        reporter.report(TransferProgress {
            transfer_id: transfer_id.to_string(),
            bytes_sent,
            total_bytes,
            speed_bps,
        });
    }
}
