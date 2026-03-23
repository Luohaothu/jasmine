use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jasmine_core::{DeviceId, ProtocolMessage, TransferStatus};
use jasmine_transfer::{
    FileSenderError, FileSenderSignal, FolderFileTransferSender, FolderProgress,
    FolderProgressReporter, FolderTransferCoordinator, FolderTransferStatus, TransferProgress,
    TransferProgressReporter,
};
use tempfile::tempdir;
use tokio::sync::{oneshot, Notify};
use tokio::time;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn device_id() -> DeviceId {
    DeviceId(Uuid::new_v4())
}

fn write_file(path: &Path, bytes: &[u8]) -> PathBuf {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create test directories");
    }
    std::fs::write(path, bytes).expect("write test file");
    path.to_path_buf()
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

fn test_file_key() -> [u8; 32] {
    [0x31; 32]
}

fn test_nonce_prefix() -> [u8; 8] {
    [0x7a; 8]
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

#[derive(Debug, Clone)]
enum MockFileBehavior {
    Complete {
        progress_points: Vec<u64>,
    },
    Fail {
        progress_points: Vec<u64>,
        message: String,
    },
    WaitForCancellation {
        progress_point: u64,
    },
}

#[derive(Clone)]
struct MockFolderFileSender {
    root: PathBuf,
    started_paths: Arc<Mutex<Vec<String>>>,
    behaviors: Arc<Mutex<HashMap<String, MockFileBehavior>>>,
}

impl MockFolderFileSender {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            started_paths: Arc::new(Mutex::new(Vec::new())),
            behaviors: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn set_behavior(&self, relative_path: &str, behavior: MockFileBehavior) {
        self.behaviors
            .lock()
            .expect("lock mock file behaviors")
            .insert(relative_path.to_string(), behavior);
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
        transfer_id: String,
        _peer_id: DeviceId,
        file_path: PathBuf,
        cancellation: CancellationToken,
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

        let behavior = self
            .behaviors
            .lock()
            .expect("lock mock file behaviors")
            .remove(&relative_path)
            .unwrap_or_else(|| MockFileBehavior::Complete {
                progress_points: vec![total_bytes],
            });

        match behavior {
            MockFileBehavior::Complete { progress_points } => {
                report_points(
                    progress.as_ref(),
                    &transfer_id,
                    total_bytes,
                    &progress_points,
                )
                .await;
                Ok(())
            }
            MockFileBehavior::Fail {
                progress_points,
                message,
            } => {
                report_points(
                    progress.as_ref(),
                    &transfer_id,
                    total_bytes,
                    &progress_points,
                )
                .await;
                Err(FileSenderError::Signal(message))
            }
            MockFileBehavior::WaitForCancellation { progress_point } => {
                report_points(
                    progress.as_ref(),
                    &transfer_id,
                    total_bytes,
                    &[progress_point],
                )
                .await;
                time::timeout(Duration::from_secs(2), cancellation.cancelled())
                    .await
                    .expect("folder cancellation should arrive");
                Err(FileSenderError::Cancelled)
            }
        }
    }
}

async fn report_points(
    progress: Option<&Arc<dyn TransferProgressReporter>>,
    transfer_id: &str,
    total_bytes: u64,
    points: &[u64],
) {
    for point in points {
        if let Some(progress) = progress {
            progress.report(TransferProgress {
                transfer_id: transfer_id.to_string(),
                bytes_sent: (*point).min(total_bytes),
                total_bytes,
                speed_bps: 1,
            });
        }
        tokio::task::yield_now().await;
    }
}

#[derive(Default)]
struct FolderProgressCollector {
    events: Mutex<Vec<FolderProgress>>,
}

impl FolderProgressCollector {
    fn snapshot(&self) -> Vec<FolderProgress> {
        self.events
            .lock()
            .expect("lock folder progress events")
            .clone()
    }
}

impl FolderProgressReporter for FolderProgressCollector {
    fn report(&self, progress: FolderProgress) {
        self.events
            .lock()
            .expect("lock folder progress events")
            .push(progress);
    }
}

struct CancelOnProgress {
    cancellation: CancellationToken,
    events: Mutex<Vec<FolderProgress>>,
    threshold: u64,
}

impl CancelOnProgress {
    fn new(cancellation: CancellationToken, threshold: u64) -> Self {
        Self {
            cancellation,
            events: Mutex::new(Vec::new()),
            threshold,
        }
    }

    fn snapshot(&self) -> Vec<FolderProgress> {
        self.events
            .lock()
            .expect("lock cancel progress events")
            .clone()
    }
}

impl FolderProgressReporter for CancelOnProgress {
    fn report(&self, progress: FolderProgress) {
        self.events
            .lock()
            .expect("lock cancel progress events")
            .push(progress.clone());

        if progress.status == FolderTransferStatus::Sending && progress.sent_bytes >= self.threshold
        {
            self.cancellation.cancel();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn folder_coordinator_sends_manifest_before_queueing_files_in_manifest_order() {
    let temp = tempdir().expect("tempdir");
    let folder = temp.path().join("notes");
    write_file(&folder.join("z-last.txt"), b"zzz");
    write_file(&folder.join("docs/b-middle.txt"), b"bbbb");
    write_file(&folder.join("a-first.txt"), b"aa");

    let local_id = device_id();
    let peer_id = device_id();
    let signal = MockFolderSignal::default();
    let file_sender = MockFolderFileSender::new(folder.clone());
    let coordinator = FolderTransferCoordinator::new(signal.clone(), file_sender.clone());
    let cancellation = CancellationToken::new();
    let task_local_id = local_id.clone();
    let task_peer_id = peer_id.clone();

    let send_task = tokio::spawn(async move {
        coordinator
            .send_folder(task_local_id, task_peer_id, folder, cancellation, None)
            .await
    });

    let (sent_peer, message) = signal.wait_for_manifest().await;
    assert_eq!(sent_peer, peer_id);
    assert!(file_sender.started_paths().is_empty());

    let folder_transfer_id = match message {
        ProtocolMessage::FolderManifest {
            folder_transfer_id,
            manifest,
            sender_id,
        } => {
            assert_eq!(sender_id, local_id.0.to_string());
            assert_eq!(
                manifest
                    .files
                    .iter()
                    .map(|entry| entry.relative_path.clone())
                    .collect::<Vec<_>>(),
                vec![
                    "a-first.txt".to_string(),
                    "docs/b-middle.txt".to_string(),
                    "z-last.txt".to_string(),
                ]
            );
            folder_transfer_id
        }
        other => panic!("expected FolderManifest, got {other:?}"),
    };

    signal.respond(
        &folder_transfer_id,
        ProtocolMessage::FolderAccept {
            folder_transfer_id: folder_transfer_id.clone(),
        },
    );

    let outcome = send_task
        .await
        .expect("join folder send task")
        .expect("send folder successfully");

    assert_eq!(
        file_sender.started_paths(),
        vec![
            "a-first.txt".to_string(),
            "docs/b-middle.txt".to_string(),
            "z-last.txt".to_string(),
        ]
    );
    assert_eq!(outcome.progress.status, FolderTransferStatus::Completed);
    assert_eq!(outcome.progress.completed_files, 3);
    assert!(outcome.rejection_reason.is_none());
    assert!(outcome
        .file_results
        .iter()
        .all(|result| result.status == TransferStatus::Completed));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn folder_coordinator_aggregates_progress_across_completed_files_and_bytes() {
    let temp = tempdir().expect("tempdir");
    let folder = temp.path().join("project");
    write_file(&folder.join("a.txt"), b"abcd");
    write_file(&folder.join("b.txt"), b"efghij");

    let signal = MockFolderSignal::default();
    let file_sender = MockFolderFileSender::new(folder.clone());
    file_sender.set_behavior(
        "a.txt",
        MockFileBehavior::Complete {
            progress_points: vec![2, 4],
        },
    );
    file_sender.set_behavior(
        "b.txt",
        MockFileBehavior::Complete {
            progress_points: vec![3, 6],
        },
    );

    let progress = Arc::new(FolderProgressCollector::default());
    let coordinator = FolderTransferCoordinator::new(signal.clone(), file_sender);
    let local_id = device_id();
    let peer_id = device_id();

    let send_task = tokio::spawn({
        let progress = Arc::clone(&progress);
        async move {
            coordinator
                .send_folder(
                    local_id,
                    peer_id,
                    folder,
                    CancellationToken::new(),
                    Some(progress),
                )
                .await
        }
    });

    let (_, message) = signal.wait_for_manifest().await;
    let folder_transfer_id = match message {
        ProtocolMessage::FolderManifest {
            folder_transfer_id, ..
        } => folder_transfer_id,
        other => panic!("expected FolderManifest, got {other:?}"),
    };
    signal.respond(
        &folder_transfer_id,
        ProtocolMessage::FolderAccept {
            folder_transfer_id: folder_transfer_id.clone(),
        },
    );

    let outcome = send_task
        .await
        .expect("join folder send task")
        .expect("send folder successfully");
    let events = progress.snapshot();

    assert_eq!(outcome.progress.status, FolderTransferStatus::Completed);
    assert_eq!(outcome.progress.sent_bytes, 10);
    assert_eq!(outcome.progress.completed_files, 2);
    assert_eq!(
        events.first().expect("pending progress").status,
        FolderTransferStatus::Pending
    );
    assert!(events.iter().any(|event| {
        event.status == FolderTransferStatus::Sending
            && event.sent_bytes == 2
            && event.completed_files == 0
    }));
    assert!(events.iter().any(|event| {
        event.status == FolderTransferStatus::Sending
            && event.sent_bytes == 4
            && event.completed_files == 1
    }));
    assert_eq!(
        events.last().expect("final progress event"),
        &FolderProgress {
            folder_transfer_id,
            total_bytes: 10,
            sent_bytes: 10,
            total_files: 2,
            completed_files: 2,
            status: FolderTransferStatus::Completed,
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn folder_coordinator_returns_rejected_without_queueing_any_files() {
    let temp = tempdir().expect("tempdir");
    let folder = temp.path().join("declined");
    write_file(&folder.join("a.txt"), b"hello");

    let signal = MockFolderSignal::default();
    let file_sender = MockFolderFileSender::new(folder.clone());
    let coordinator = FolderTransferCoordinator::new(signal.clone(), file_sender.clone());

    let send_task = tokio::spawn(async move {
        coordinator
            .send_folder(
                device_id(),
                device_id(),
                folder,
                CancellationToken::new(),
                None,
            )
            .await
    });

    let (_, message) = signal.wait_for_manifest().await;
    let folder_transfer_id = match message {
        ProtocolMessage::FolderManifest {
            folder_transfer_id, ..
        } => folder_transfer_id,
        other => panic!("expected FolderManifest, got {other:?}"),
    };
    signal.respond(
        &folder_transfer_id,
        ProtocolMessage::FolderReject {
            folder_transfer_id: folder_transfer_id.clone(),
            reason: "receiver declined".to_string(),
        },
    );

    let outcome = send_task
        .await
        .expect("join folder send task")
        .expect("folder rejection should be an outcome");

    assert_eq!(outcome.progress.status, FolderTransferStatus::Rejected);
    assert_eq!(
        outcome.rejection_reason.as_deref(),
        Some("receiver declined")
    );
    assert!(file_sender.started_paths().is_empty());
    assert!(outcome.file_results.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn folder_coordinator_continues_after_a_single_file_failure() {
    let temp = tempdir().expect("tempdir");
    let folder = temp.path().join("partial");
    write_file(&folder.join("a.txt"), b"a");
    write_file(&folder.join("b.txt"), b"bb");
    write_file(&folder.join("c.txt"), b"ccc");

    let signal = MockFolderSignal::default();
    let file_sender = MockFolderFileSender::new(folder.clone());
    file_sender.set_behavior(
        "a.txt",
        MockFileBehavior::Complete {
            progress_points: vec![1],
        },
    );
    file_sender.set_behavior(
        "b.txt",
        MockFileBehavior::Fail {
            progress_points: Vec::new(),
            message: "mock sender failure".to_string(),
        },
    );
    file_sender.set_behavior(
        "c.txt",
        MockFileBehavior::Complete {
            progress_points: vec![3],
        },
    );

    let coordinator = FolderTransferCoordinator::new(signal.clone(), file_sender.clone());
    let send_task = tokio::spawn(async move {
        coordinator
            .send_folder(
                device_id(),
                device_id(),
                folder,
                CancellationToken::new(),
                None,
            )
            .await
    });

    let (_, message) = signal.wait_for_manifest().await;
    let folder_transfer_id = match message {
        ProtocolMessage::FolderManifest {
            folder_transfer_id, ..
        } => folder_transfer_id,
        other => panic!("expected FolderManifest, got {other:?}"),
    };
    signal.respond(
        &folder_transfer_id,
        ProtocolMessage::FolderAccept {
            folder_transfer_id: folder_transfer_id.clone(),
        },
    );

    let outcome = send_task
        .await
        .expect("join folder send task")
        .expect("send folder successfully with partial failures");

    assert_eq!(
        outcome.progress.status,
        FolderTransferStatus::PartiallyFailed
    );
    assert_eq!(outcome.progress.completed_files, 2);
    assert_eq!(outcome.progress.total_files, 3);
    assert_eq!(outcome.progress.sent_bytes, 4);
    assert_eq!(
        file_sender.started_paths(),
        vec![
            "a.txt".to_string(),
            "b.txt".to_string(),
            "c.txt".to_string()
        ]
    );
    assert_eq!(
        outcome
            .file_results
            .iter()
            .map(|result| result.status.clone())
            .collect::<Vec<_>>(),
        vec![
            TransferStatus::Completed,
            TransferStatus::Failed,
            TransferStatus::Completed,
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn folder_coordinator_cancellation_stops_remaining_queued_work_cleanly() {
    let temp = tempdir().expect("tempdir");
    let folder = temp.path().join("cancelled");
    write_file(&folder.join("a.txt"), b"aaaa");
    write_file(&folder.join("b.txt"), b"bbbb");
    write_file(&folder.join("c.txt"), b"cccc");

    let signal = MockFolderSignal::default();
    let file_sender = MockFolderFileSender::new(folder.clone());
    file_sender.set_behavior(
        "a.txt",
        MockFileBehavior::WaitForCancellation { progress_point: 1 },
    );
    file_sender.set_behavior(
        "b.txt",
        MockFileBehavior::Complete {
            progress_points: vec![4],
        },
    );
    file_sender.set_behavior(
        "c.txt",
        MockFileBehavior::Complete {
            progress_points: vec![4],
        },
    );

    let cancellation = CancellationToken::new();
    let progress = Arc::new(CancelOnProgress::new(cancellation.clone(), 1));
    let coordinator = FolderTransferCoordinator::new(signal.clone(), file_sender.clone());
    let send_task = tokio::spawn({
        let progress = Arc::clone(&progress);
        async move {
            coordinator
                .send_folder(
                    device_id(),
                    device_id(),
                    folder,
                    cancellation,
                    Some(progress),
                )
                .await
        }
    });

    let (_, message) = signal.wait_for_manifest().await;
    let folder_transfer_id = match message {
        ProtocolMessage::FolderManifest {
            folder_transfer_id, ..
        } => folder_transfer_id,
        other => panic!("expected FolderManifest, got {other:?}"),
    };
    signal.respond(
        &folder_transfer_id,
        ProtocolMessage::FolderAccept {
            folder_transfer_id: folder_transfer_id.clone(),
        },
    );

    let outcome = send_task
        .await
        .expect("join folder send task")
        .expect("cancelled folder transfer should return outcome");

    assert_eq!(outcome.progress.status, FolderTransferStatus::Cancelled);
    assert_eq!(outcome.progress.completed_files, 0);
    assert_eq!(outcome.progress.sent_bytes, 1);
    assert_eq!(file_sender.started_paths(), vec!["a.txt".to_string()]);
    assert_eq!(outcome.file_results.len(), 1);
    assert_eq!(outcome.file_results[0].status, TransferStatus::Cancelled);
    assert!(progress
        .snapshot()
        .iter()
        .any(|event| event.status == FolderTransferStatus::Cancelled));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn folder_resume_skip_skips_completed_files_and_sends_partial_then_remaining_in_order() {
    let temp = tempdir().expect("tempdir");
    let folder = temp.path().join("resume-skip");
    write_file(&folder.join("a.txt"), b"aaaa");
    write_file(&folder.join("b.txt"), b"bbbb");
    write_file(&folder.join("c.txt"), b"cccccccc");
    write_file(&folder.join("d.txt"), b"dddd");
    write_file(&folder.join("e.txt"), b"eeee");

    let signal = MockFolderSignal::default();
    let file_sender = MockFolderFileSender::new(folder.clone());
    let coordinator = FolderTransferCoordinator::new(signal.clone(), file_sender.clone());

    let send_task = tokio::spawn(async move {
        coordinator
            .send_folder(
                device_id(),
                device_id(),
                folder,
                CancellationToken::new(),
                None,
            )
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
            completed_files: vec!["a.txt".to_string(), "b.txt".to_string()],
            partial_files: vec![("c.txt".to_string(), 4)],
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
                    "c.txt".to_string(),
                    "d.txt".to_string(),
                    "e.txt".to_string()
                ]
            );
        }
        other => panic!("expected FolderResumeAccept, got {other:?}"),
    }

    let outcome = send_task
        .await
        .expect("join folder resume send task")
        .expect("folder resume send succeeds");

    assert_eq!(
        file_sender.started_paths(),
        vec![
            "c.txt".to_string(),
            "d.txt".to_string(),
            "e.txt".to_string()
        ]
    );
    assert_eq!(outcome.progress.status, FolderTransferStatus::Completed);
    assert_eq!(outcome.progress.completed_files, 5);
    assert_eq!(outcome.progress.sent_bytes, 24);
    assert_eq!(
        outcome
            .file_results
            .iter()
            .map(|result| (result.relative_path.clone(), result.status.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("a.txt".to_string(), TransferStatus::Completed),
            ("b.txt".to_string(), TransferStatus::Completed),
            ("c.txt".to_string(), TransferStatus::Completed),
            ("d.txt".to_string(), TransferStatus::Completed),
            ("e.txt".to_string(), TransferStatus::Completed),
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn folder_resume_all_complete_returns_noop_accept_and_sends_nothing() {
    let temp = tempdir().expect("tempdir");
    let folder = temp.path().join("resume-noop");
    write_file(&folder.join("a.txt"), b"aaaa");
    write_file(&folder.join("b.txt"), b"bbbb");

    let signal = MockFolderSignal::default();
    let file_sender = MockFolderFileSender::new(folder.clone());
    let coordinator = FolderTransferCoordinator::new(signal.clone(), file_sender.clone());

    let send_task = tokio::spawn(async move {
        coordinator
            .send_folder(
                device_id(),
                device_id(),
                folder,
                CancellationToken::new(),
                None,
            )
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
            completed_files: vec!["a.txt".to_string(), "b.txt".to_string()],
            partial_files: Vec::new(),
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
            assert!(files_to_send.is_empty());
        }
        other => panic!("expected FolderResumeAccept, got {other:?}"),
    }

    let outcome = send_task
        .await
        .expect("join no-op folder resume task")
        .expect("folder no-op resume succeeds");

    assert!(file_sender.started_paths().is_empty());
    assert_eq!(outcome.progress.status, FolderTransferStatus::Completed);
    assert_eq!(outcome.progress.completed_files, 2);
    assert_eq!(outcome.progress.sent_bytes, 8);
    assert_eq!(
        outcome
            .file_results
            .iter()
            .map(|result| result.relative_path.clone())
            .collect::<Vec<_>>(),
        vec!["a.txt".to_string(), "b.txt".to_string()]
    );
}
