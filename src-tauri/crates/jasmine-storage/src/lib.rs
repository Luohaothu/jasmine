mod sqlite;

use jasmine_core::{Message, PeerInfo, StorageEngine, TransferRecord};

pub use sqlite::{CachedOgMetadata, ChatRecord, ChatType, SqliteStorage};

pub struct StorageModule<T: StorageEngine> {
    engine: T,
    cached_messages: Vec<Message>,
    cached_peers: Vec<PeerInfo>,
    cached_transfers: Vec<TransferRecord>,
}

impl<T: StorageEngine> StorageModule<T> {
    pub fn new(engine: T) -> Self {
        Self {
            engine,
            cached_messages: Vec::new(),
            cached_peers: Vec::new(),
            cached_transfers: Vec::new(),
        }
    }

    pub fn engine(&self) -> &T {
        &self.engine
    }

    pub fn cached_messages(&self) -> &[Message] {
        &self.cached_messages
    }

    pub fn cached_peers(&self) -> &[PeerInfo] {
        &self.cached_peers
    }

    pub fn cached_transfers(&self) -> &[TransferRecord] {
        &self.cached_transfers
    }
}
