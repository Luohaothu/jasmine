#![allow(async_fn_in_trait)]

pub mod manager;
pub mod receiver;
pub mod sender;

use jasmine_core::{FileTransfer, TransferOffer};

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
