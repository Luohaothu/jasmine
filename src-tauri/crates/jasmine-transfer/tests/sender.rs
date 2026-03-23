use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jasmine_core::{DeviceId, ProtocolMessage};
use jasmine_crypto::decrypt_chunk;
use jasmine_transfer::{
    FileSender, FileSenderConfig, FileSenderError, FileSenderSignal, ManagedTransferSender,
    TransferProgress, TransferProgressReporter, DEFAULT_CHUNK_SIZE,
};
use sha2::{Digest, Sha256};
use tempfile::tempdir;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::sync::{oneshot, Notify};
use tokio::time;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn peer_id() -> DeviceId {
    DeviceId(Uuid::new_v4())
}

fn write_bytes(path: &Path, bytes: &[u8]) -> PathBuf {
    std::fs::write(path, bytes).expect("write temp payload");
    path.to_path_buf()
}

fn write_large_file(path: &Path, size: usize) -> PathBuf {
    let mut file = File::create(path).expect("create temp payload");
    let pattern = vec![0x5A; DEFAULT_CHUNK_SIZE];
    let mut remaining = size;

    while remaining > 0 {
        let chunk = remaining.min(pattern.len());
        file.write_all(&pattern[..chunk])
            .expect("write temp payload chunk");
        remaining -= chunk;
    }

    path.to_path_buf()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn empty_sha256() -> &'static str {
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
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
    file_key_hasher.update(b"sender-test-file-key");
    file_key_hasher.update(file_id.as_bytes());
    file_key.copy_from_slice(&file_key_hasher.finalize());

    let mut nonce_prefix = [0u8; 8];
    let mut nonce_hasher = Sha256::new();
    nonce_hasher.update(b"sender-test-nonce-prefix");
    nonce_hasher.update(file_id.as_bytes());
    nonce_prefix.copy_from_slice(&nonce_hasher.finalize()[..8]);

    (file_key, nonce_prefix)
}

fn decrypt_transfer_stream(_transfer_id: &str, bytes: &[u8]) -> Vec<u8> {
    decrypt_transfer_stream_from_offset(0, bytes)
}

fn decrypt_transfer_stream_from_offset(start_offset: u64, bytes: &[u8]) -> Vec<u8> {
    let file_key = test_file_key();
    let nonce_prefix = test_nonce_prefix();
    let mut cursor = 0_usize;
    let mut chunk_index = (start_offset / DEFAULT_CHUNK_SIZE as u64) as u32;
    let mut plaintext = Vec::new();

    while cursor < bytes.len() {
        assert!(
            bytes.len() - cursor >= 4,
            "encrypted transfer must include 4-byte chunk length"
        );
        let chunk_len =
            u32::from_be_bytes(bytes[cursor..cursor + 4].try_into().expect("chunk prefix"))
                as usize;
        cursor += 4;
        assert!(
            bytes.len() - cursor >= chunk_len,
            "encrypted transfer chunk length exceeds remaining bytes"
        );
        let decrypted = decrypt_chunk(
            &file_key,
            &nonce_prefix,
            chunk_index,
            &bytes[cursor..cursor + chunk_len],
        )
        .expect("decrypt transfer chunk");
        plaintext.extend_from_slice(&decrypted);
        cursor += chunk_len;
        chunk_index += 1;
    }

    plaintext
}

#[derive(Clone, Default)]
struct MockSignal {
    sent_messages: Arc<Mutex<Vec<(DeviceId, ProtocolMessage)>>>,
    response_senders: Arc<Mutex<HashMap<String, oneshot::Sender<ProtocolMessage>>>>,
    response_receivers: Arc<Mutex<HashMap<String, oneshot::Receiver<ProtocolMessage>>>>,
    notify: Arc<Notify>,
}

impl MockSignal {
    async fn wait_for_offer(&self) -> (DeviceId, ProtocolMessage) {
        loop {
            if let Some(message) = self
                .sent_messages
                .lock()
                .expect("lock sent messages")
                .last()
            {
                return message.clone();
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

    fn accept(&self, offer_id: &str) {
        self.respond(
            offer_id,
            ProtocolMessage::FileAccept {
                offer_id: offer_id.to_string(),
            },
        );
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

impl FileSenderSignal for MockSignal {
    async fn send_message(
        &self,
        peer_id: &DeviceId,
        message: ProtocolMessage,
    ) -> Result<(), FileSenderError> {
        if let ProtocolMessage::FileOffer { id, .. } = &message {
            let (tx, rx) = oneshot::channel();
            self.response_senders
                .lock()
                .expect("lock response senders")
                .insert(id.clone(), tx);
            self.response_receivers
                .lock()
                .expect("lock response receivers")
                .insert(id.clone(), rx);
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
            .response_receivers
            .lock()
            .expect("lock response receivers")
            .remove(offer_id)
            .expect("response receiver for offer");

        receiver.await.map_err(|_| FileSenderError::SignalClosed)
    }

    async fn file_crypto_material(
        &self,
        _peer_id: &DeviceId,
        file_id: &str,
    ) -> Result<jasmine_transfer::FileCryptoMaterial, FileSenderError> {
        Ok(derived_crypto_material(file_id))
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

struct CancelAtThresholdProgress {
    events: Mutex<Vec<TransferProgress>>,
    cancellation: CancellationToken,
    threshold: u64,
}

impl CancelAtThresholdProgress {
    fn new(cancellation: CancellationToken, threshold: u64) -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            cancellation,
            threshold,
        }
    }

    fn snapshot(&self) -> Vec<TransferProgress> {
        self.events.lock().expect("lock progress events").clone()
    }
}

impl TransferProgressReporter for CancelAtThresholdProgress {
    fn report(&self, progress: TransferProgress) {
        self.events
            .lock()
            .expect("lock progress events")
            .push(progress.clone());

        if progress.bytes_sent >= self.threshold {
            self.cancellation.cancel();
        }
    }
}

fn sender_config() -> FileSenderConfig {
    FileSenderConfig {
        file_key: Some(test_file_key()),
        nonce_prefix: Some(test_nonce_prefix()),
        ..FileSenderConfig::default()
    }
}

async fn receive_all(port: u16) -> Vec<u8> {
    let mut stream = time::timeout(
        Duration::from_secs(2),
        TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], port))),
    )
    .await
    .expect("receiver connect timeout")
    .expect("receiver connect");
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await.expect("read transfer");
    bytes
}

async fn receive_all_slow(port: u16) -> Vec<u8> {
    let mut stream = time::timeout(
        Duration::from_secs(2),
        TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], port))),
    )
    .await
    .expect("receiver connect timeout")
    .expect("receiver connect");
    let mut bytes = Vec::new();
    let mut chunk = vec![0; DEFAULT_CHUNK_SIZE];

    loop {
        let read = stream.read(&mut chunk).await.expect("read transfer chunk");
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        time::sleep(Duration::from_millis(1)).await;
    }

    bytes
}

async fn assert_port_closed(port: u16) {
    let result = TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], port))).await;
    assert!(result.is_err(), "port {port} should be closed");
}

fn extract_offer(message: &ProtocolMessage) -> (&str, &str, u64, &str, u16) {
    match message {
        ProtocolMessage::FileOffer {
            id,
            filename,
            size,
            sha256,
            transfer_port,
        } => (id, filename, *size, sha256, *transfer_port),
        other => panic!("expected FileOffer, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn encrypted_transfer_sender_hashes_1kb_file_and_waits_for_accept_before_streaming() {
    let temp = tempdir().expect("tempdir");
    let payload = vec![0xAB; 1024];
    let path = write_bytes(&temp.path().join("hello.bin"), &payload);
    let signal = MockSignal::default();
    let progress = Arc::new(ProgressCollector::default());
    let sender = FileSender::with_config(signal.clone(), sender_config());
    let peer_id = peer_id();
    let cancellation = CancellationToken::new();

    let send_task = tokio::spawn({
        let progress = progress.clone();
        let path = path.clone();
        let peer_id = peer_id.clone();
        let cancellation = cancellation.clone();
        async move {
            sender
                .send_file(peer_id, path, cancellation, Some(progress))
                .await
        }
    });

    let (sent_peer, offer) = signal.wait_for_offer().await;
    let (offer_id, filename, size, sha256, port) = extract_offer(&offer);

    assert_eq!(sent_peer, peer_id);
    assert_eq!(filename, "hello.bin");
    assert_eq!(size, payload.len() as u64);
    assert_eq!(sha256, sha256_hex(&payload));
    assert!(port > 0);
    time::sleep(Duration::from_millis(50)).await;
    assert!(
        !send_task.is_finished(),
        "sender should wait for FileAccept"
    );

    signal.accept(offer_id);
    let received = receive_all(port).await;
    send_task
        .await
        .expect("sender task join")
        .expect("send file");

    assert!(
        received.len() > payload.len(),
        "encrypted stream should add framing/tag overhead"
    );
    assert_eq!(decrypt_transfer_stream(offer_id, &received), payload);
    let events = progress.snapshot();
    assert_eq!(events.first().expect("initial progress").bytes_sent, 0);
    assert_eq!(
        events.last().expect("final progress").bytes_sent,
        payload.len() as u64
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sender_zero_byte_file_transfers_successfully() {
    let temp = tempdir().expect("tempdir");
    let path = write_bytes(&temp.path().join("empty.txt"), b"");
    let signal = MockSignal::default();
    let progress = Arc::new(ProgressCollector::default());
    let sender = FileSender::with_config(signal.clone(), sender_config());
    let cancellation = CancellationToken::new();

    let send_task = tokio::spawn({
        let path = path.clone();
        let progress = progress.clone();
        let cancellation = cancellation.clone();
        async move {
            sender
                .send_file(peer_id(), path, cancellation, Some(progress))
                .await
        }
    });

    let (_, offer) = signal.wait_for_offer().await;
    let (offer_id, filename, size, sha256, port) = extract_offer(&offer);
    assert_eq!(filename, "empty.txt");
    assert_eq!(size, 0);
    assert_eq!(sha256, empty_sha256());

    signal.accept(offer_id);
    let received = receive_all(port).await;
    send_task
        .await
        .expect("sender task join")
        .expect("send empty file");

    assert!(received.is_empty());
    assert!(decrypt_transfer_stream(offer_id, &received).is_empty());
    let events = progress.snapshot();
    assert_eq!(events.first().expect("initial progress").total_bytes, 0);
    assert!(events.iter().all(|event| event.bytes_sent == 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sender_progress_reports_start_midpoint_and_completion() {
    let temp = tempdir().expect("tempdir");
    let payload = vec![0x44; 1024 * 1024];
    let path = write_bytes(&temp.path().join("progress.bin"), &payload);
    let signal = MockSignal::default();
    let progress = Arc::new(ProgressCollector::default());
    let sender = FileSender::with_config(signal.clone(), sender_config());
    let cancellation = CancellationToken::new();
    let total = payload.len() as u64;

    let send_task = tokio::spawn({
        let path = path.clone();
        let progress = progress.clone();
        let cancellation = cancellation.clone();
        async move {
            sender
                .send_file(peer_id(), path, cancellation, Some(progress))
                .await
        }
    });

    let (_, offer) = signal.wait_for_offer().await;
    let (offer_id, _, size, sha256, port) = extract_offer(&offer);
    assert_eq!(size, total);
    assert_eq!(sha256, sha256_hex(&payload));

    signal.accept(offer_id);
    let received = receive_all(port).await;
    send_task
        .await
        .expect("sender task join")
        .expect("send file");

    assert_eq!(decrypt_transfer_stream(offer_id, &received), payload);
    let events = progress.snapshot();
    assert_eq!(events.first().expect("initial progress").bytes_sent, 0);
    assert_eq!(events.last().expect("final progress").bytes_sent, total);
    assert!(
        events
            .iter()
            .any(|event| event.bytes_sent > 0 && event.bytes_sent < total),
        "expected intermediate progress event"
    );
    assert!(
        events.windows(2).skip(1).all(|window| {
            window[1].bytes_sent >= window[0].bytes_sent
                && window[1].bytes_sent - window[0].bytes_sent <= DEFAULT_CHUNK_SIZE as u64
        }),
        "progress deltas should reflect 64KB chunking"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sender_cancel_stops_large_transfer_and_closes_listener() {
    let temp = tempdir().expect("tempdir");
    let total_size = 100 * 1024 * 1024;
    let path = write_large_file(&temp.path().join("large.bin"), total_size);
    let signal = MockSignal::default();
    let cancellation = CancellationToken::new();
    let progress = Arc::new(CancelAtThresholdProgress::new(
        cancellation.clone(),
        10 * 1024 * 1024,
    ));
    let sender = FileSender::with_config(signal.clone(), sender_config());

    let send_task = tokio::spawn({
        let path = path.clone();
        let progress = progress.clone();
        let cancellation = cancellation.clone();
        async move {
            sender
                .send_file(peer_id(), path, cancellation, Some(progress))
                .await
        }
    });

    let (_, offer) = signal.wait_for_offer().await;
    let (offer_id, _, size, _, port) = extract_offer(&offer);
    assert_eq!(size, total_size as u64);

    signal.accept(offer_id);
    let receiver_task = tokio::spawn(receive_all_slow(port));

    let error = send_task
        .await
        .expect("sender task join")
        .expect_err("send should be cancelled");
    assert!(matches!(error, FileSenderError::Cancelled));

    let received = receiver_task.await.expect("receiver task join");
    assert!(received.len() < total_size);
    assert_port_closed(port).await;

    let events = progress.snapshot();
    assert_eq!(events.first().expect("initial progress").bytes_sent, 0);
    assert!(
        events.last().expect("cancel progress").bytes_sent < total_size as u64,
        "cancelled transfer must stop before completion"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_sender_resumes_from_requested_offset_with_nonce_continuity() {
    let temp = tempdir().expect("tempdir");
    let payload = vec![0x66; 4 * DEFAULT_CHUNK_SIZE];
    let path = write_bytes(&temp.path().join("resume.bin"), &payload);
    let signal = MockSignal::default();
    let sender = FileSender::with_config(signal.clone(), sender_config());
    let peer_id = peer_id();
    let cancellation = CancellationToken::new();

    let send_task = tokio::spawn({
        let path = path.clone();
        let peer_id = peer_id.clone();
        let cancellation = cancellation.clone();
        async move { sender.send_file(peer_id, path, cancellation, None).await }
    });

    let (_, offer) = signal.wait_for_offer().await;
    let (offer_id, _, _, _, port) = extract_offer(&offer);
    let resume_offset = (2 * DEFAULT_CHUNK_SIZE) as u64;

    signal.respond(
        offer_id,
        ProtocolMessage::FileResumeRequest {
            offer_id: offer_id.to_string(),
            offset: resume_offset,
        },
    );

    let received = receive_all(port).await;
    send_task
        .await
        .expect("sender task join")
        .expect("resume send should succeed");

    assert_eq!(
        decrypt_transfer_stream_from_offset(resume_offset, &received),
        payload[resume_offset as usize..],
    );
    assert!(
        signal.snapshot().iter().any(|(_, message)| matches!(
            message,
            ProtocolMessage::FileResumeAccept { offer_id: actual, offset }
                if actual == offer_id && *offset == resume_offset
        )),
        "sender should acknowledge resume request before streaming",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sender_accept_timeout_cleans_up_listener() {
    let temp = tempdir().expect("tempdir");
    let payload = vec![0x33; 1024];
    let path = write_bytes(&temp.path().join("timeout.bin"), &payload);
    let signal = MockSignal::default();
    let mut config = sender_config();
    config.accept_timeout = Duration::from_millis(150);
    let sender = FileSender::with_config(signal.clone(), config);
    let cancellation = CancellationToken::new();

    let send_task = tokio::spawn({
        let path = path.clone();
        let cancellation = cancellation.clone();
        async move { sender.send_file(peer_id(), path, cancellation, None).await }
    });

    let (_, offer) = signal.wait_for_offer().await;
    let (offer_id, _, _, _, port) = extract_offer(&offer);
    signal.accept(offer_id);

    let error = send_task
        .await
        .expect("sender task join")
        .expect_err("send should time out waiting for receiver connect");
    assert!(matches!(error, FileSenderError::AcceptTimedOut));
    assert_port_closed(port).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sender_resume_token_uses_file_specific_crypto_material() {
    let signal = MockSignal::default();
    let sender = FileSender::new(signal);
    let peer = peer_id();

    let first = ManagedTransferSender::current_resume_token(&sender, &peer, "offer-alpha")
        .await
        .expect("first token")
        .expect("resume token should be present");
    let first_again = ManagedTransferSender::current_resume_token(&sender, &peer, "offer-alpha")
        .await
        .expect("first token repeat")
        .expect("resume token should be present");
    let second = ManagedTransferSender::current_resume_token(&sender, &peer, "offer-beta")
        .await
        .expect("second token")
        .expect("resume token should be present");

    assert_eq!(first, first_again);
    assert_ne!(first, second);
}
