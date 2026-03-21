#![allow(async_fn_in_trait)]

pub mod folder;
pub mod manager;
pub mod receiver;
pub mod sender;
pub mod thumbnail;

use jasmine_core::{FileTransfer, TransferOffer};

pub use folder::{
    generate_folder_manifest, sanitize_manifest_path, FolderFileTransferResult,
    FolderFileTransferSender, FolderManifest, FolderManifestError, FolderOfferNotification,
    FolderOfferNotifier, FolderProgress, FolderProgressReporter, FolderReceiver,
    FolderReceiverError, FolderTransferCoordinator, FolderTransferError,
    FolderTransferOutcome, FolderTransferStatus, ManifestEntry,
    DEFAULT_FOLDER_MANIFEST_MAX_FILES, MAX_MANIFEST_PATH_LENGTH,
};
pub use manager::{
    ManagedTransfer, ManagedTransferReceiver, ManagedTransferSender, TransferAggregateProgress,
    TransferDirection, TransferManager, TransferManagerError, TransferManagerSnapshot,
};
pub use receiver::{
    DiskSpaceChecker, FileOfferNotification, FileOfferNotifier, FileReceiver, FileReceiverConfig,
    FileReceiverError, FileReceiverSignal,
};
#[cfg(feature = "tauri-channel")]
pub use sender::TauriChannelProgressReporter;
pub use sender::{
    FileSender, FileSenderConfig, FileSenderError, FileSenderSignal, TransferProgress,
    TransferProgressReporter, DEFAULT_CHUNK_SIZE,
};
pub use thumbnail::{generate_thumbnail, ThumbnailError, THUMBNAIL_MAX_EDGE};

pub struct TransferModule<T: FileTransfer> {
    service: T,
    pending_offer: Option<TransferOffer>,
}

impl<T: FileTransfer> TransferModule<T> {
    pub fn new(service: T) -> Self {
        Self {
            service,
            pending_offer: None,
        }
    }

    pub fn service(&self) -> &T {
        &self.service
    }

    pub fn pending_offer(&self) -> Option<&TransferOffer> {
        self.pending_offer.as_ref()
    }
}
