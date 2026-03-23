use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use jasmine_core::{AppSettings, DeviceId, ProtocolMessage, SettingsService};
use jasmine_crypto::encrypt_chunk;
use jasmine_transfer::{
    DiskSpaceChecker, FileOfferNotification, FileOfferNotifier, FileReceiver, FileReceiverConfig,
    FileReceiverError, FileReceiverSignal, ManagedTransferReceiver, TransferProgress,
    TransferProgressReporter, DEFAULT_CHUNK_SIZE,
};
use sha2::{Digest, Sha256};
use tempfile::tempdir;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::{self, Duration};
use uuid::Uuid;

fn peer_id() -> DeviceId {
    DeviceId(Uuid::new_v4())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn offer_message(
    offer_id: &str,
    filename: &str,
    size: u64,
    sha256: &str,
    transfer_port: u16,
) -> ProtocolMessage {
    ProtocolMessage::FileOffer {
        id: offer_id.to_string(),
        filename: filename.to_string(),
        size,
        sha256: sha256.to_string(),
        transfer_port,
    }
}

fn test_file_key() -> [u8; 32] {
    [0x31; 32]
}

fn test_nonce_prefix() -> [u8; 8] {
    [0x7a; 8]
}

fn derived_crypto_material(file_id: &str) -> ([u8; 32], [u8; 8]) {
    let mut file_key = [0u8; 32];
    let mut file_key_hasher = Sha256::new();
    file_key_hasher.update(b"receiver-test-file-key");
    file_key_hasher.update(file_id.as_bytes());
    file_key.copy_from_slice(&file_key_hasher.finalize());

    let mut nonce_prefix = [0u8; 8];
    let mut nonce_hasher = Sha256::new();
    nonce_hasher.update(b"receiver-test-nonce-prefix");
    nonce_hasher.update(file_id.as_bytes());
    nonce_prefix.copy_from_slice(&nonce_hasher.finalize()[..8]);

    (file_key, nonce_prefix)
}

fn receiver_config() -> FileReceiverConfig {
    FileReceiverConfig {
        file_key: Some(test_file_key()),
        nonce_prefix: Some(test_nonce_prefix()),
        ..FileReceiverConfig::default()
    }
}

fn receiver_config_with_chunk_size(chunk_size: usize) -> FileReceiverConfig {
    FileReceiverConfig {
        chunk_size,
        file_key: Some(test_file_key()),
        nonce_prefix: Some(test_nonce_prefix()),
    }
}

fn encrypt_transfer_stream(_offer_id: &str, plaintext: &[u8], chunk_size: usize) -> Vec<u8> {
    encrypt_transfer_stream_from_offset(0, plaintext, chunk_size)
}

fn encrypt_transfer_stream_from_offset(
    start_offset: u64,
    plaintext: &[u8],
    chunk_size: usize,
) -> Vec<u8> {
    let file_key = test_file_key();
    let nonce_prefix = test_nonce_prefix();
    let mut output = Vec::new();
    let starting_chunk_index = start_offset / chunk_size as u64;

    for (chunk_index, chunk) in plaintext.chunks(chunk_size).enumerate() {
        let encrypted = encrypt_chunk(
            &file_key,
            &nonce_prefix,
            (starting_chunk_index + chunk_index as u64) as u32,
            chunk,
        )
        .expect("encrypt transfer chunk");
        output.extend_from_slice(&(encrypted.len() as u32).to_be_bytes());
        output.extend_from_slice(&encrypted);
    }

    output
}

fn first_encrypted_frame_len(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }

    4 + u32::from_be_bytes(bytes[..4].try_into().expect("chunk prefix")) as usize
}

fn tamper_first_encrypted_chunk(bytes: &mut [u8]) {
    if bytes.len() > 4 {
        bytes[4] ^= 0x01;
    }
}

fn save_download_dir(settings: &SettingsService, download_dir: &Path) {
    settings
        .save(&AppSettings {
            download_dir: download_dir.to_string_lossy().into_owned(),
            max_concurrent_transfers: 3,
        })
        .expect("save test settings");
}

#[derive(Clone, Default)]
struct MockSignal {
    sent_messages: Arc<Mutex<Vec<(DeviceId, ProtocolMessage)>>>,
    response_senders: Arc<Mutex<HashMap<String, oneshot::Sender<ProtocolMessage>>>>,
    response_receivers: Arc<Mutex<HashMap<String, oneshot::Receiver<ProtocolMessage>>>>,
}

impl MockSignal {
    fn snapshot(&self) -> Vec<(DeviceId, ProtocolMessage)> {
        self.sent_messages
            .lock()
            .expect("lock sent messages")
            .clone()
    }

    async fn wait_for_resume_request(&self, offer_id: &str) -> (DeviceId, u64) {
        time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some((peer_id, offset)) =
                    self.snapshot()
                        .into_iter()
                        .find_map(|(peer_id, message)| match message {
                            ProtocolMessage::FileResumeRequest {
                                offer_id: actual_offer_id,
                                offset,
                            } if actual_offer_id == offer_id => Some((peer_id, offset)),
                            _ => None,
                        })
                {
                    return (peer_id, offset);
                }

                time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("wait for resume request timeout")
    }

    fn respond(&self, offer_id: &str, response: ProtocolMessage) {
        let sender = self
            .response_senders
            .lock()
            .expect("lock response senders")
            .remove(offer_id)
            .expect("response sender for offer");
        sender.send(response).expect("deliver mocked response");
    }
}

impl FileReceiverSignal for MockSignal {
    async fn send_message(
        &self,
        peer_id: &DeviceId,
        message: ProtocolMessage,
    ) -> Result<(), FileReceiverError> {
        if let ProtocolMessage::FileResumeRequest { offer_id, .. } = &message {
            let (tx, rx) = oneshot::channel();
            self.response_senders
                .lock()
                .expect("lock response senders")
                .insert(offer_id.clone(), tx);
            self.response_receivers
                .lock()
                .expect("lock response receivers")
                .insert(offer_id.clone(), rx);
        }

        self.sent_messages
            .lock()
            .expect("lock sent messages")
            .push((peer_id.clone(), message));
        Ok(())
    }

    async fn wait_for_response(
        &self,
        offer_id: &str,
    ) -> Result<ProtocolMessage, FileReceiverError> {
        let receiver = self
            .response_receivers
            .lock()
            .expect("lock response receivers")
            .remove(offer_id)
            .expect("response receiver for offer");
        receiver
            .await
            .map_err(|_| FileReceiverError::Signal("response channel closed".to_string()))
    }

    async fn file_crypto_material(
        &self,
        _peer_id: &DeviceId,
        file_id: &str,
    ) -> Result<jasmine_transfer::FileCryptoMaterial, FileReceiverError> {
        Ok(derived_crypto_material(file_id))
    }
}

#[derive(Default)]
struct OfferCollector {
    offers: Mutex<Vec<FileOfferNotification>>,
}

impl OfferCollector {
    fn snapshot(&self) -> Vec<FileOfferNotification> {
        self.offers.lock().expect("lock offers").clone()
    }
}

impl FileOfferNotifier for OfferCollector {
    fn offer_received(&self, offer: FileOfferNotification) {
        self.offers.lock().expect("lock offers").push(offer);
    }
}

struct FixedDiskSpaceChecker {
    available_bytes: u64,
}

impl FixedDiskSpaceChecker {
    fn new(available_bytes: u64) -> Self {
        Self { available_bytes }
    }
}

impl DiskSpaceChecker for FixedDiskSpaceChecker {
    fn available_space(&self, _path: &Path) -> io::Result<u64> {
        Ok(self.available_bytes)
    }
}

#[derive(Default)]
struct ProgressCollector {
    events: Mutex<Vec<TransferProgress>>,
}

impl ProgressCollector {
    fn snapshot(&self) -> Vec<TransferProgress> {
        self.events.lock().expect("lock progress events").clone()
    }
}

impl TransferProgressReporter for ProgressCollector {
    fn report(&self, progress: TransferProgress) {
        self.events
            .lock()
            .expect("lock progress events")
            .push(progress);
    }
}

struct MockTcpSender {
    port: u16,
    connected: Arc<AtomicBool>,
    first_chunk_written: Option<Mutex<Option<oneshot::Receiver<()>>>>,
    release_rest: Option<Mutex<Option<oneshot::Sender<()>>>>,
    task: tokio::task::JoinHandle<()>,
}

impl MockTcpSender {
    async fn spawn(payload: Vec<u8>) -> Self {
        Self::spawn_inner(payload, None).await
    }

    async fn spawn_paused(payload: Vec<u8>, first_chunk_len: usize) -> Self {
        Self::spawn_inner(payload, Some(first_chunk_len)).await
    }

    async fn spawn_inner(payload: Vec<u8>, pause_after: Option<usize>) -> Self {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind mock sender");
        let port = listener.local_addr().expect("mock sender addr").port();
        let connected = Arc::new(AtomicBool::new(false));
        let (task_first_chunk, first_chunk_written) = if pause_after.is_some() {
            let (tx, rx) = oneshot::channel();
            (Some(tx), Some(Mutex::new(Some(rx))))
        } else {
            (None, None)
        };
        let (release_rest, task_release) = if pause_after.is_some() {
            let (tx, rx) = oneshot::channel();
            (Some(Mutex::new(Some(tx))), Some(rx))
        } else {
            (None, None)
        };
        let task_connected = connected.clone();

        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept receiver connection");
            task_connected.store(true, Ordering::SeqCst);

            if let Some(first_len) = pause_after {
                let split_at = first_len.min(payload.len());
                let (first, rest) = payload.split_at(split_at);
                stream
                    .write_all(first)
                    .await
                    .expect("write first payload chunk");
                let _ = task_first_chunk.expect("first chunk sender").send(());

                if !rest.is_empty() {
                    let release_rx = task_release.expect("release receiver");
                    let _ = release_rx.await;
                    stream
                        .write_all(rest)
                        .await
                        .expect("write remaining payload");
                }
            } else {
                stream.write_all(&payload).await.expect("write payload");
            }

            stream.shutdown().await.expect("shutdown sender stream");
        });

        Self {
            port,
            connected,
            first_chunk_written,
            release_rest,
            task,
        }
    }

    fn port(&self) -> u16 {
        self.port
    }

    fn connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    async fn wait_for_first_chunk(&self) {
        let receiver = self
            .first_chunk_written
            .as_ref()
            .expect("paused sender first chunk receiver")
            .lock()
            .expect("lock first chunk receiver")
            .take()
            .expect("first chunk receiver available once");
        time::timeout(Duration::from_secs(2), receiver)
            .await
            .expect("wait for first chunk timeout")
            .expect("first chunk signal should arrive");
    }

    async fn wait_for_path(path: &Path) {
        time::timeout(Duration::from_secs(2), async {
            loop {
                if path.exists() {
                    return;
                }

                time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("wait for path timeout");
    }

    fn release(&self) {
        let sender = self
            .release_rest
            .as_ref()
            .expect("paused sender release sender")
            .lock()
            .expect("lock release sender")
            .take()
            .expect("release sender available once");
        let _ = sender.send(());
    }

    async fn join(self) {
        self.task.await.expect("mock sender task join");
    }
}

fn assert_accept_message(
    message: &(DeviceId, ProtocolMessage),
    expected_peer: &DeviceId,
    offer_id: &str,
) {
    assert_eq!(&message.0, expected_peer);
    match &message.1 {
        ProtocolMessage::FileAccept { offer_id: actual } => assert_eq!(actual, offer_id),
        other => panic!("expected FileAccept, got {other:?}"),
    }
}

fn assert_reject_message(
    message: &(DeviceId, ProtocolMessage),
    expected_peer: &DeviceId,
    offer_id: &str,
    reason: Option<&str>,
) {
    assert_eq!(&message.0, expected_peer);
    match &message.1 {
        ProtocolMessage::FileReject {
            offer_id: actual_offer_id,
            reason: actual_reason,
        } => {
            assert_eq!(actual_offer_id, offer_id);
            assert_eq!(actual_reason.as_deref(), reason);
        }
        other => panic!("expected FileReject, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receiver_offer_flow_waits_for_explicit_accept_and_uses_latest_settings_dir() {
    let temp = tempdir().expect("tempdir");
    let settings = SettingsService::new(temp.path().join("app-data"));
    let first_dir = temp.path().join("downloads-a");
    let second_dir = temp.path().join("downloads-b");
    save_download_dir(&settings, &first_dir);

    let signal = MockSignal::default();
    let offers = Arc::new(OfferCollector::default());
    let receiver = Arc::new(FileReceiver::with_dependencies(
        signal.clone(),
        settings.clone(),
        receiver_config(),
        Arc::new(FixedDiskSpaceChecker::new(u64::MAX / 4)),
        Some(offers.clone()),
    ));
    let sender_id = peer_id();
    let sender_addr = SocketAddr::from(([127, 0, 0, 1], 9735));
    let offer_id = Uuid::new_v4().to_string();
    let sender = MockTcpSender::spawn(encrypt_transfer_stream(
        &offer_id,
        &b"first payload"[..],
        DEFAULT_CHUNK_SIZE,
    ))
    .await;

    let first_offer = receiver
        .receive_offer(
            sender_id.clone(),
            sender_addr,
            offer_message(
                &offer_id,
                "report.pdf",
                13,
                &sha256_hex(&b"first payload"[..]),
                sender.port(),
            ),
        )
        .await
        .expect("receive first offer");

    assert_eq!(first_offer.offer_id, offer_id);
    assert_eq!(first_offer.filename, "report.pdf");
    assert_eq!(first_offer.size, 13);
    assert_eq!(first_offer.sender_id, sender_id);
    assert_eq!(first_offer.download_dir, first_dir);
    assert!(first_offer.has_enough_space);
    assert!(
        signal.snapshot().is_empty(),
        "receiver must not auto-accept"
    );

    time::sleep(Duration::from_millis(50)).await;
    assert!(
        !sender.connected(),
        "tcp receive must wait for explicit accept"
    );

    save_download_dir(&settings, &second_dir);
    let second_offer = receiver
        .receive_offer(
            sender_id.clone(),
            sender_addr,
            offer_message(
                &Uuid::new_v4().to_string(),
                "other.txt",
                4,
                &sha256_hex(&b"next"[..]),
                sender.port(),
            ),
        )
        .await
        .expect("receive second offer");
    assert_eq!(second_offer.download_dir, second_dir);

    let saved_path = receiver
        .accept_offer(&offer_id, None)
        .await
        .expect("accept first offer");
    sender.join().await;

    assert_eq!(saved_path, first_dir.join("report.pdf"));
    assert_eq!(
        std::fs::read(&saved_path).expect("read saved file"),
        b"first payload"
    );

    let recorded_offers = offers.snapshot();
    assert_eq!(recorded_offers.len(), 2);
    assert_eq!(recorded_offers[0].download_dir, first_dir);
    assert_eq!(recorded_offers[1].download_dir, second_dir);

    let messages = signal.snapshot();
    assert_eq!(messages.len(), 1);
    assert_accept_message(&messages[0], &sender_id, &offer_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receiver_reject_flow_sends_reject_without_allocating_resources() {
    let temp = tempdir().expect("tempdir");
    let download_dir = temp.path().join("downloads");
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_download_dir(&settings, &download_dir);

    let signal = MockSignal::default();
    let receiver = FileReceiver::with_dependencies(
        signal.clone(),
        settings,
        receiver_config(),
        Arc::new(FixedDiskSpaceChecker::new(u64::MAX / 4)),
        None,
    );
    let sender_id = peer_id();
    let offer_id = Uuid::new_v4().to_string();

    receiver
        .receive_offer(
            sender_id.clone(),
            SocketAddr::from(([127, 0, 0, 1], 9735)),
            offer_message(&offer_id, "decline.txt", 8, &sha256_hex(b"declined"), 9000),
        )
        .await
        .expect("receive offer");

    receiver
        .reject_offer(&offer_id, Some("not needed".to_string()))
        .await
        .expect("reject offer");

    let messages = signal.snapshot();
    assert_eq!(messages.len(), 1);
    assert_reject_message(&messages[0], &sender_id, &offer_id, Some("not needed"));
    assert!(
        !download_dir.exists(),
        "reject must not create download directory"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn encrypted_transfer_receiver_writes_partial_verifies_hash_and_reports_progress() {
    let temp = tempdir().expect("tempdir");
    let download_dir = temp.path().join("downloads");
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_download_dir(&settings, &download_dir);

    let payload = vec![0x55; 128 * 1024];
    let signal = MockSignal::default();
    let progress = Arc::new(ProgressCollector::default());
    let config = receiver_config_with_chunk_size(1024);
    let offer_id = Uuid::new_v4().to_string();
    let encrypted_payload = encrypt_transfer_stream(&offer_id, &payload, config.chunk_size);
    let sender = MockTcpSender::spawn_paused(
        encrypted_payload.clone(),
        first_encrypted_frame_len(&encrypted_payload),
    )
    .await;
    let receiver = Arc::new(FileReceiver::with_dependencies(
        signal.clone(),
        settings,
        config,
        Arc::new(FixedDiskSpaceChecker::new(u64::MAX / 4)),
        None,
    ));
    let sender_id = peer_id();

    receiver
        .receive_offer(
            sender_id.clone(),
            SocketAddr::from(([127, 0, 0, 1], 9735)),
            offer_message(
                &offer_id,
                "archive.bin",
                payload.len() as u64,
                &sha256_hex(&payload),
                sender.port(),
            ),
        )
        .await
        .expect("receive offer");

    let accept_task = tokio::spawn({
        let receiver = receiver.clone();
        let progress = progress.clone();
        let offer_id = offer_id.clone();
        async move { receiver.accept_offer(&offer_id, Some(progress)).await }
    });

    sender.wait_for_first_chunk().await;
    let partial_path = download_dir.join("archive.bin.jasmine-partial");
    let final_path = download_dir.join("archive.bin");
    MockTcpSender::wait_for_path(&partial_path).await;
    assert!(
        partial_path.exists(),
        "transfer must write into partial file first"
    );
    assert!(
        !final_path.exists(),
        "final file must not exist before integrity succeeds"
    );

    sender.release();
    let saved_path = accept_task
        .await
        .expect("accept task join")
        .expect("accept offer");
    sender.join().await;

    assert_eq!(saved_path, final_path);
    assert_eq!(
        std::fs::read(&saved_path).expect("read final file"),
        payload
    );
    assert!(
        !partial_path.exists(),
        "partial file must be removed after rename"
    );

    let events = progress.snapshot();
    assert_eq!(events.first().expect("initial progress").bytes_sent, 0);
    assert_eq!(
        events.last().expect("final progress").bytes_sent,
        128 * 1024
    );
    assert!(
        events
            .iter()
            .any(|event| event.bytes_sent > 0 && event.bytes_sent < 128 * 1024),
        "expected intermediate progress events"
    );

    let messages = signal.snapshot();
    assert_eq!(messages.len(), 1);
    assert_accept_message(&messages[0], &sender_id, &offer_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn encrypted_resume_roundtrip_receiver_completes_1mb_file_after_interrupted_midpoint() {
    let temp = tempdir().expect("tempdir");
    let download_dir = temp.path().join("downloads");
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_download_dir(&settings, &download_dir);

    let payload = vec![0x77; 1024 * 1024];
    let resume_offset = (payload.len() / 2) as u64;
    let offer_id = Uuid::new_v4().to_string();
    let signal = MockSignal::default();
    let receiver = FileReceiver::with_dependencies(
        signal.clone(),
        settings,
        receiver_config(),
        Arc::new(FixedDiskSpaceChecker::new(u64::MAX / 4)),
        None,
    );
    let sender_id = peer_id();

    let initial_sender = MockTcpSender::spawn(encrypt_transfer_stream_from_offset(
        0,
        &payload[..resume_offset as usize],
        DEFAULT_CHUNK_SIZE,
    ))
    .await;

    receiver
        .receive_offer(
            sender_id.clone(),
            SocketAddr::from(([127, 0, 0, 1], 9735)),
            offer_message(
                &offer_id,
                "resume.bin",
                payload.len() as u64,
                &sha256_hex(&payload),
                initial_sender.port(),
            ),
        )
        .await
        .expect("receive initial offer");

    let first_error = receiver
        .accept_offer(&offer_id, None)
        .await
        .expect_err("interrupted transfer should fail before completion");
    initial_sender.join().await;
    assert!(matches!(
        first_error,
        FileReceiverError::SizeMismatch { .. }
    ));

    let partial_path = download_dir.join("resume.bin.jasmine-partial");
    assert!(
        partial_path.exists(),
        "interrupted transfer should keep partial for resume"
    );
    assert_eq!(
        std::fs::metadata(&partial_path)
            .expect("partial metadata after interruption")
            .len(),
        resume_offset,
    );

    let resumed_stream = encrypt_transfer_stream_from_offset(
        resume_offset,
        &payload[resume_offset as usize..],
        DEFAULT_CHUNK_SIZE,
    );
    let restarted_stream = encrypt_transfer_stream_from_offset(
        0,
        &payload[resume_offset as usize..],
        DEFAULT_CHUNK_SIZE,
    );
    assert_ne!(
        &resumed_stream[..first_encrypted_frame_len(&resumed_stream)],
        &restarted_stream[..first_encrypted_frame_len(&restarted_stream)],
        "resumed encrypted stream should differ from a fresh restart because nonce continuity advances chunk indices",
    );

    let resumed_sender = MockTcpSender::spawn(resumed_stream).await;
    receiver
        .receive_offer(
            sender_id.clone(),
            SocketAddr::from(([127, 0, 0, 1], 9735)),
            offer_message(
                &offer_id,
                "resume.bin",
                payload.len() as u64,
                &sha256_hex(&payload),
                resumed_sender.port(),
            ),
        )
        .await
        .expect("receive resume offer");

    let resume_task = tokio::spawn({
        let receiver = receiver;
        let offer_id = offer_id.clone();
        async move {
            receiver
                .accept_offer_with_resume_offset(&offer_id, Some(resume_offset), None)
                .await
        }
    });

    let (resume_peer, requested_offset) = signal.wait_for_resume_request(&offer_id).await;
    assert_eq!(resume_peer, sender_id);
    assert_eq!(requested_offset, resume_offset);
    signal.respond(
        &offer_id,
        ProtocolMessage::FileResumeAccept {
            offer_id: offer_id.clone(),
            offset: resume_offset,
        },
    );

    let saved_path = resume_task
        .await
        .expect("resume task join")
        .expect("resume transfer should complete");
    resumed_sender.join().await;

    assert_eq!(saved_path, download_dir.join("resume.bin"));
    assert_eq!(
        std::fs::read(&saved_path).expect("read resumed file"),
        payload
    );
    assert_eq!(
        sha256_hex(&std::fs::read(&saved_path).expect("read resumed file for sha")),
        sha256_hex(&payload),
        "resumed encrypted transfer must preserve final plaintext integrity",
    );
    assert!(
        !partial_path.exists(),
        "partial file should be renamed away on completion"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receiver_integrity_hash_mismatch_cleans_up_partial_file() {
    let temp = tempdir().expect("tempdir");
    let download_dir = temp.path().join("downloads");
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_download_dir(&settings, &download_dir);

    let signal = MockSignal::default();
    let receiver = FileReceiver::with_dependencies(
        signal.clone(),
        settings,
        receiver_config(),
        Arc::new(FixedDiskSpaceChecker::new(u64::MAX / 4)),
        None,
    );
    let sender_id = peer_id();
    let offer_id = Uuid::new_v4().to_string();
    let sender = MockTcpSender::spawn(encrypt_transfer_stream(
        &offer_id,
        &b"actual payload"[..],
        DEFAULT_CHUNK_SIZE,
    ))
    .await;

    receiver
        .receive_offer(
            sender_id.clone(),
            SocketAddr::from(([127, 0, 0, 1], 9735)),
            offer_message(
                &offer_id,
                "broken.bin",
                14,
                &sha256_hex(&b"expected data"[..]),
                sender.port(),
            ),
        )
        .await
        .expect("receive offer");

    let error = receiver
        .accept_offer(&offer_id, None)
        .await
        .expect_err("accept should fail integrity check");
    sender.join().await;

    assert!(matches!(error, FileReceiverError::IntegrityMismatch { .. }));
    assert!(!download_dir.join("broken.bin").exists());
    assert!(!download_dir.join("broken.bin.jasmine-partial").exists());

    let messages = signal.snapshot();
    assert_eq!(messages.len(), 1);
    assert_accept_message(&messages[0], &sender_id, &offer_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receiver_conflict_auto_renames_conflicting_targets() {
    let temp = tempdir().expect("tempdir");
    let download_dir = temp.path().join("downloads");
    std::fs::create_dir_all(&download_dir).expect("create download dir");
    std::fs::write(download_dir.join("test.txt"), b"original").expect("seed existing file");
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_download_dir(&settings, &download_dir);

    let signal = MockSignal::default();
    let receiver = FileReceiver::with_dependencies(
        signal,
        settings,
        receiver_config(),
        Arc::new(FixedDiskSpaceChecker::new(u64::MAX / 4)),
        None,
    );
    let sender_id = peer_id();

    let first_offer_id = Uuid::new_v4().to_string();
    let first_payload = b"first conflict".to_vec();
    let first_sender = MockTcpSender::spawn(encrypt_transfer_stream(
        &first_offer_id,
        &first_payload,
        DEFAULT_CHUNK_SIZE,
    ))
    .await;
    receiver
        .receive_offer(
            sender_id.clone(),
            SocketAddr::from(([127, 0, 0, 1], 9735)),
            offer_message(
                &first_offer_id,
                "test.txt",
                first_payload.len() as u64,
                &sha256_hex(&first_payload),
                first_sender.port(),
            ),
        )
        .await
        .expect("receive first offer");
    let first_saved = receiver
        .accept_offer(&first_offer_id, None)
        .await
        .expect("accept first conflicting offer");
    first_sender.join().await;

    let second_offer_id = Uuid::new_v4().to_string();
    let second_payload = b"second conflict".to_vec();
    let second_sender = MockTcpSender::spawn(encrypt_transfer_stream(
        &second_offer_id,
        &second_payload,
        DEFAULT_CHUNK_SIZE,
    ))
    .await;
    receiver
        .receive_offer(
            sender_id,
            SocketAddr::from(([127, 0, 0, 1], 9735)),
            offer_message(
                &second_offer_id,
                "test.txt",
                second_payload.len() as u64,
                &sha256_hex(&second_payload),
                second_sender.port(),
            ),
        )
        .await
        .expect("receive second offer");
    let second_saved = receiver
        .accept_offer(&second_offer_id, None)
        .await
        .expect("accept second conflicting offer");
    second_sender.join().await;

    assert_eq!(first_saved, download_dir.join("test (1).txt"));
    assert_eq!(second_saved, download_dir.join("test (2).txt"));
    assert_eq!(
        std::fs::read(download_dir.join("test.txt")).expect("read original"),
        b"original"
    );
    assert_eq!(
        std::fs::read(first_saved).expect("read first renamed file"),
        first_payload
    );
    assert_eq!(
        std::fs::read(second_saved).expect("read second renamed file"),
        second_payload
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receiver_disk_full_warns_on_offer_and_rejects_on_accept() {
    let temp = tempdir().expect("tempdir");
    let download_dir = temp.path().join("downloads");
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_download_dir(&settings, &download_dir);

    let available_bytes = 500_u64 * 1024 * 1024;
    let file_size = 600_u64 * 1024 * 1024;
    let signal = MockSignal::default();
    let offers = Arc::new(OfferCollector::default());
    let receiver = FileReceiver::with_dependencies(
        signal.clone(),
        settings,
        receiver_config(),
        Arc::new(FixedDiskSpaceChecker::new(available_bytes)),
        Some(offers.clone()),
    );
    let sender_id = peer_id();
    let offer_id = Uuid::new_v4().to_string();

    let offer = receiver
        .receive_offer(
            sender_id.clone(),
            SocketAddr::from(([127, 0, 0, 1], 9735)),
            offer_message(&offer_id, "huge.iso", file_size, &"0".repeat(64), 9000),
        )
        .await
        .expect("receive offer");

    assert!(!offer.has_enough_space);
    assert_eq!(offer.available_space_bytes, Some(available_bytes));
    assert!(offer.required_space_bytes > available_bytes);

    let error = receiver
        .accept_offer(&offer_id, None)
        .await
        .expect_err("accept should reject disk-full offer");

    match error {
        FileReceiverError::InsufficientDiskSpace {
            available_bytes: actual_available,
            required_bytes,
        } => {
            assert_eq!(actual_available, available_bytes);
            assert!(required_bytes > available_bytes);
        }
        other => panic!("expected insufficient disk space error, got {other:?}"),
    }

    let messages = signal.snapshot();
    assert_eq!(messages.len(), 1);
    assert_reject_message(
        &messages[0],
        &sender_id,
        &offer_id,
        Some("insufficient disk space"),
    );
    assert_eq!(offers.snapshot().len(), 1);
    assert!(
        !download_dir.exists(),
        "disk-full path must not create temp files or directories"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chunk_tamper_receiver_rejects_tampered_encrypted_chunk_and_cleans_partial() {
    let temp = tempdir().expect("tempdir");
    let download_dir = temp.path().join("downloads");
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_download_dir(&settings, &download_dir);

    let signal = MockSignal::default();
    let receiver = FileReceiver::with_dependencies(
        signal.clone(),
        settings,
        receiver_config(),
        Arc::new(FixedDiskSpaceChecker::new(u64::MAX / 4)),
        None,
    );
    let sender_id = peer_id();
    let offer_id = Uuid::new_v4().to_string();
    let payload = b"tampered encrypted payload".to_vec();
    let mut encrypted_payload = encrypt_transfer_stream(&offer_id, &payload, DEFAULT_CHUNK_SIZE);
    tamper_first_encrypted_chunk(&mut encrypted_payload);
    let sender = MockTcpSender::spawn(encrypted_payload).await;

    receiver
        .receive_offer(
            sender_id.clone(),
            SocketAddr::from(([127, 0, 0, 1], 9735)),
            offer_message(
                &offer_id,
                "tampered.bin",
                payload.len() as u64,
                &sha256_hex(&payload),
                sender.port(),
            ),
        )
        .await
        .expect("receive tampered offer");

    let error = receiver
        .accept_offer(&offer_id, None)
        .await
        .expect_err("tampered chunk must fail decryption");
    sender.join().await;

    assert!(matches!(error, FileReceiverError::Crypto(_)));
    assert!(!download_dir.join("tampered.bin").exists());
    assert!(!download_dir.join("tampered.bin.jasmine-partial").exists());

    let messages = signal.snapshot();
    assert_eq!(messages.len(), 1);
    assert_accept_message(&messages[0], &sender_id, &offer_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receiver_resume_token_uses_offer_specific_crypto_material() {
    let temp = tempdir().expect("tempdir");
    let settings = SettingsService::new(temp.path().join("app-data"));
    save_download_dir(&settings, &temp.path().join("downloads"));
    let receiver = FileReceiver::new(MockSignal::default(), settings);
    let peer = peer_id();

    let first = ManagedTransferReceiver::current_resume_token(&receiver, &peer, "offer-alpha")
        .await
        .expect("first token")
        .expect("resume token should be present");
    let first_again =
        ManagedTransferReceiver::current_resume_token(&receiver, &peer, "offer-alpha")
            .await
            .expect("first token repeat")
            .expect("resume token should be present");
    let second = ManagedTransferReceiver::current_resume_token(&receiver, &peer, "offer-beta")
        .await
        .expect("second token")
        .expect("resume token should be present");

    assert_eq!(first, first_again);
    assert_ne!(first, second);
}
