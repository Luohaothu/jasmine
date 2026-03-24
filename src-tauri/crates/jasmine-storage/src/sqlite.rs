use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jasmine_core::{
    ChatId, CoreError, DeviceId, Message, MessageStatus, OgMetadata, PeerInfo, PeerKeyInfo, Result,
    ResumeInfo, SenderKeyInfo, StorageEngine, TransferRecord, TransferStatus, UserId,
};
use r2d2::{ManageConnection, Pool};
use rusqlite::types::Type;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};
use tokio::task;
use uuid::Uuid;

const POOL_SIZE: u32 = 4;
const BUSY_TIMEOUT_MS: u64 = 5_000;
const SQLITE_MAX_VARIABLES: usize = 900;
const TRANSFER_DIRECTION_SEND: &str = "send";
const MESSAGE_SELECT_COLUMNS: &str =
    "id, chat_id, sender_id, content, timestamp, status, edit_version, edited_at, is_deleted, deleted_at, reply_to_id, reply_to_preview, created_at, vector_clock";
const MESSAGE_EDIT_WINNER_EXPR: &str =
    "excluded.edit_version > messages.edit_version OR (excluded.edit_version = messages.edit_version AND COALESCE(excluded.edited_at, -1) > COALESCE(messages.edited_at, -1))";
const MESSAGE_DELETE_WINNER_EXPR: &str =
    "excluded.is_deleted > messages.is_deleted OR (excluded.is_deleted = messages.is_deleted AND excluded.is_deleted = 1 AND COALESCE(excluded.deleted_at, -1) > COALESCE(messages.deleted_at, -1))";

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: include_str!("../migrations/V1__initial.sql"),
    },
    Migration {
        version: 2,
        sql: include_str!("../migrations/V2__edit_delete_richtext.sql"),
    },
    Migration {
        version: 3,
        sql: include_str!("../migrations/V3__crypto_and_resume.sql"),
    },
    Migration {
        version: 4,
        sql: include_str!("../migrations/V4__sender_key_epochs.sql"),
    },
    Migration {
        version: 5,
        sql: include_str!("../migrations/V5__folder_file_status.sql"),
    },
    Migration {
        version: 6,
        sql: include_str!("../migrations/V6__vector_clock_og_cache_reply_index.sql"),
    },
];

struct Migration {
    version: i64,
    sql: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatType {
    Direct,
    Group,
}

impl ChatType {
    fn as_db_value(&self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Group => "group",
        }
    }

    fn from_db_value(value: &str) -> rusqlite::Result<Self> {
        match value {
            "direct" => Ok(Self::Direct),
            "group" => Ok(Self::Group),
            _ => Err(rusqlite::Error::InvalidQuery),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatRecord {
    pub id: ChatId,
    pub chat_type: ChatType,
    pub name: Option<String>,
    pub created_at_ms: i64,
}

struct StoredMessageRecord {
    message: Message,
    created_at_ms: i64,
    vector_clock: Option<HashMap<String, u64>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedOgMetadata {
    metadata: OgMetadata,
    pub fetched_at_ms: i64,
    pub ttl_seconds: i64,
}

impl CachedOgMetadata {
    pub fn new(metadata: OgMetadata, fetched_at_ms: i64, ttl_seconds: i64) -> Self {
        Self {
            metadata,
            fetched_at_ms,
            ttl_seconds,
        }
    }

    pub fn metadata(&self) -> &OgMetadata {
        &self.metadata
    }

    pub fn into_metadata(self) -> OgMetadata {
        self.metadata
    }
}

#[derive(Clone)]
pub struct SqliteStorage {
    db_path: PathBuf,
    pool: Pool<SqliteConnectionManager>,
}

impl SqliteStorage {
    pub const FOLDER_FILE_STATUS_PENDING: &str = "pending";
    pub const FOLDER_FILE_STATUS_COMPLETED: &str = "completed";
    pub const FOLDER_FILE_STATUS_FAILED: &str = "failed";

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db_path = path.as_ref().to_path_buf();

        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent).map_err(|_| {
                CoreError::NotImplemented("sqlite storage directory creation failed")
            })?;
        }

        let manager = SqliteConnectionManager {
            path: db_path.clone(),
        };
        let pool = Pool::builder()
            .max_size(POOL_SIZE)
            .build(manager)
            .map_err(|_| CoreError::NotImplemented("sqlite pool initialization failed"))?;

        {
            let mut conn = pool
                .get()
                .map_err(|_| CoreError::NotImplemented("sqlite pool acquisition failed"))?;
            run_migrations(&mut conn)
                .map_err(|_| CoreError::NotImplemented("sqlite migration failed"))?;
        }

        Ok(Self { db_path, pool })
    }

    pub fn path(&self) -> &Path {
        &self.db_path
    }

    pub fn pool_size(&self) -> u32 {
        POOL_SIZE
    }

    pub async fn save_message_with_vector_clock(
        &self,
        message: &Message,
        vector_clock: Option<&HashMap<String, u64>>,
    ) -> Result<()> {
        let vector_clock_json = vector_clock.map(serialize_vector_clock);
        self.save_message_record(message.clone(), vector_clock_json)
            .await
    }

    pub async fn get_message_vector_clock(
        &self,
        message_id: &str,
    ) -> Result<Option<HashMap<String, u64>>> {
        let message_id = parse_uuid_arg(message_id, "message_id")?.to_string();

        self.with_connection_result(move |conn| {
            conn.query_row(
                "SELECT vector_clock FROM messages WHERE id = ?1",
                params![message_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(|value| value.flatten().as_deref().and_then(parse_vector_clock))
            .map_err(|error| sqlite_persistence("sqlite get message vector clock failed", error))
        })
        .await
    }

    pub async fn get_reply_count(&self, message_id: &str) -> Result<i64> {
        let message_id = parse_uuid_arg(message_id, "message_id")?.to_string();

        self.with_connection_result(move |conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM messages WHERE reply_to_id = ?1",
                params![message_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| sqlite_persistence("sqlite get reply count failed", error))
        })
        .await
    }

    pub async fn get_reply_counts(&self, message_ids: &[String]) -> Result<HashMap<String, i64>> {
        let message_ids = message_ids
            .iter()
            .map(|message_id| parse_uuid_arg(message_id, "message_id").map(|id| id.to_string()))
            .collect::<Result<Vec<_>>>()?;

        if message_ids.is_empty() {
            return Ok(HashMap::new());
        }

        self.with_connection_result(move |conn| load_reply_counts(conn, &message_ids))
            .await
    }

    pub async fn get_cached_og_metadata(&self, url: &str) -> Result<Option<CachedOgMetadata>> {
        let url = url.to_string();

        self.with_connection_result(move |conn| {
            conn.query_row(
                "SELECT url, title, description, image_url, site_name, fetched_at, ttl_seconds
                 FROM og_cache
                 WHERE url = ?1",
                params![url],
                map_og_cache_row,
            )
            .optional()
            .map_err(|error| sqlite_persistence("sqlite get cached og metadata failed", error))
        })
        .await
    }

    pub async fn save_og_metadata(&self, metadata: &OgMetadata, ttl_seconds: u64) -> Result<()> {
        let metadata = metadata.clone();
        let ttl_seconds = u64_to_db_i64(ttl_seconds, "ttl_seconds")?;
        let fetched_at = now_ms();

        self.with_connection("sqlite save og metadata failed", move |conn| {
            conn.execute(
                "INSERT INTO og_cache (
                    url, title, description, image_url, site_name, fetched_at, ttl_seconds
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(url) DO UPDATE SET
                    title = excluded.title,
                    description = excluded.description,
                    image_url = excluded.image_url,
                    site_name = excluded.site_name,
                    fetched_at = excluded.fetched_at,
                    ttl_seconds = excluded.ttl_seconds",
                params![
                    metadata.url,
                    metadata.title,
                    metadata.description,
                    metadata.image_url,
                    metadata.site_name,
                    fetched_at,
                    ttl_seconds
                ],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn save_folder_file_status(
        &self,
        folder_id: &str,
        file_path: &str,
        status: &str,
        bytes_transferred: u64,
    ) -> Result<()> {
        validate_folder_file_status(status)?;

        let folder_id = folder_id.to_string();
        let file_path = file_path.to_string();
        let status = status.to_string();
        let bytes_transferred = u64_to_db_i64(bytes_transferred, "bytes_transferred")?;

        self.with_connection("sqlite save folder file status failed", move |conn| {
            conn.execute(
                "INSERT INTO folder_file_status (folder_id, file_path, status, bytes_transferred)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(folder_id, file_path) DO UPDATE SET
                     status = excluded.status,
                     bytes_transferred = excluded.bytes_transferred",
                params![folder_id, file_path, status, bytes_transferred],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn get_folder_file_statuses(
        &self,
        folder_id: &str,
    ) -> Result<Vec<(String, String, u64)>> {
        let folder_id = folder_id.to_string();

        self.with_connection("sqlite get folder file statuses failed", move |conn| {
            let mut statement = conn.prepare(
                "SELECT file_path, status, bytes_transferred
                 FROM folder_file_status
                 WHERE folder_id = ?1
                 ORDER BY file_path ASC",
            )?;
            let rows = statement.query_map(params![folder_id], |row| {
                let bytes_transferred: i64 = row.get(2)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    db_i64_to_u64(bytes_transferred, 2)?,
                ))
            })?;

            let mut statuses = Vec::new();
            for row in rows {
                statuses.push(row?);
            }

            Ok(statuses)
        })
        .await
    }

    pub async fn save_chat(&self, chat: &ChatRecord) -> Result<()> {
        let chat = chat.clone();

        self.with_connection("sqlite save chat failed", move |conn| {
            conn.execute(
                "INSERT INTO chats (id, type, name, created_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(id) DO UPDATE SET
                     type = excluded.type,
                     name = excluded.name,
                     created_at = excluded.created_at",
                params![
                    chat.id.0.to_string(),
                    chat.chat_type.as_db_value(),
                    chat.name,
                    chat.created_at_ms
                ],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn get_chat(&self, chat_id: &ChatId) -> Result<Option<ChatRecord>> {
        let chat_id = chat_id.clone();

        self.with_connection("sqlite get chat failed", move |conn| {
            conn.query_row(
                "SELECT id, type, name, created_at FROM chats WHERE id = ?1",
                params![chat_id.0.to_string()],
                map_chat_row,
            )
            .optional()
        })
        .await
    }

    pub async fn get_chats(&self) -> Result<Vec<ChatRecord>> {
        self.with_connection("sqlite list chats failed", move |conn| {
            let mut statement = conn.prepare(
                "SELECT id, type, name, created_at
                 FROM chats
                 ORDER BY created_at DESC, id DESC",
            )?;
            let rows = statement.query_map([], map_chat_row)?;

            let mut chats = Vec::new();
            for row in rows {
                chats.push(row?);
            }

            Ok(chats)
        })
        .await
    }

    pub async fn replace_chat_members(
        &self,
        chat_id: &ChatId,
        member_ids: &[UserId],
        joined_at_ms: i64,
    ) -> Result<()> {
        let chat_id = chat_id.clone();
        let member_ids = member_ids.to_vec();

        self.with_connection("sqlite replace chat members failed", move |conn| {
            let transaction = conn.transaction()?;
            transaction.execute(
                "DELETE FROM chat_members WHERE chat_id = ?1",
                params![chat_id.0.to_string()],
            )?;

            for member_id in member_ids {
                transaction.execute(
                    "INSERT INTO chat_members (chat_id, member_id, joined_at)
                     VALUES (?1, ?2, ?3)",
                    params![chat_id.0.to_string(), member_id.0.to_string(), joined_at_ms],
                )?;
            }

            transaction.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn get_chat_members(&self, chat_id: &ChatId) -> Result<Vec<UserId>> {
        let chat_id = chat_id.clone();

        self.with_connection("sqlite get chat members failed", move |conn| {
            let mut statement = conn.prepare(
                "SELECT member_id
                 FROM chat_members
                 WHERE chat_id = ?1
                 ORDER BY rowid ASC",
            )?;
            let rows = statement.query_map(params![chat_id.0.to_string()], |row| {
                let member_id: String = row.get(0)?;
                Ok(UserId(parse_uuid(&member_id)?))
            })?;

            let mut members = Vec::new();
            for row in rows {
                members.push(row?);
            }

            Ok(members)
        })
        .await
    }

    pub async fn get_peers(&self) -> Result<Vec<PeerInfo>> {
        self.with_connection("sqlite list peers failed", move |conn| {
            let mut statement = conn.prepare(
                "SELECT device_id, display_name, ws_port
                 FROM peers
                 ORDER BY last_seen DESC, device_id ASC",
            )?;
            let rows = statement.query_map([], |row| {
                let device_id: String = row.get(0)?;
                Ok(PeerInfo {
                    device_id: DeviceId(parse_uuid(&device_id)?),
                    user_id: None,
                    display_name: row.get(1)?,
                    ws_port: row.get(2)?,
                    addresses: Vec::new(),
                })
            })?;

            let mut peers = Vec::new();
            for row in rows {
                peers.push(row?);
            }

            Ok(peers)
        })
        .await
    }

    async fn with_connection<T, F>(&self, operation_error: &'static str, op: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        let pool = self.pool.clone();

        task::spawn_blocking(move || {
            let mut conn = pool
                .get()
                .map_err(|_| CoreError::NotImplemented("sqlite pool acquisition failed"))?;
            op(&mut conn).map_err(|_| CoreError::NotImplemented(operation_error))
        })
        .await
        .map_err(|_| CoreError::NotImplemented("sqlite task join failed"))?
    }

    async fn with_connection_result<T, F>(&self, op: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let pool = self.pool.clone();

        task::spawn_blocking(move || {
            let mut conn = pool
                .get()
                .map_err(|_| CoreError::NotImplemented("sqlite pool acquisition failed"))?;
            op(&mut conn)
        })
        .await
        .map_err(|_| CoreError::NotImplemented("sqlite task join failed"))?
    }

    async fn save_message_record(
        &self,
        message: Message,
        vector_clock_json: Option<String>,
    ) -> Result<()> {
        let edited_at = option_u64_to_db_i64(message.edited_at, "edited_at")?;
        let deleted_at = option_u64_to_db_i64(message.deleted_at, "deleted_at")?;
        let is_deleted = bool_to_db(message.is_deleted);
        let upsert_sql = format!(
            "INSERT INTO messages (
                id, chat_id, sender_id, content, timestamp, status, created_at,
                edit_version, edited_at, is_deleted, deleted_at, reply_to_id, reply_to_preview,
                vector_clock
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(id) DO UPDATE SET
                 chat_id = excluded.chat_id,
                 sender_id = excluded.sender_id,
                 content = CASE
                     WHEN {MESSAGE_EDIT_WINNER_EXPR} THEN excluded.content
                     ELSE messages.content
                 END,
                 timestamp = excluded.timestamp,
                 status = excluded.status,
                 created_at = excluded.created_at,
                 edit_version = MAX(messages.edit_version, excluded.edit_version),
                 edited_at = CASE
                     WHEN {MESSAGE_EDIT_WINNER_EXPR} THEN COALESCE(excluded.edited_at, messages.edited_at)
                     ELSE messages.edited_at
                 END,
                 is_deleted = CASE
                     WHEN {MESSAGE_DELETE_WINNER_EXPR} THEN excluded.is_deleted
                     ELSE messages.is_deleted
                 END,
                 deleted_at = CASE
                     WHEN {MESSAGE_DELETE_WINNER_EXPR} THEN COALESCE(excluded.deleted_at, messages.deleted_at)
                     ELSE messages.deleted_at
                 END,
                 reply_to_id = CASE
                     WHEN {MESSAGE_EDIT_WINNER_EXPR} THEN COALESCE(excluded.reply_to_id, messages.reply_to_id)
                     ELSE messages.reply_to_id
                 END,
                 reply_to_preview = CASE
                     WHEN {MESSAGE_EDIT_WINNER_EXPR} THEN COALESCE(excluded.reply_to_preview, messages.reply_to_preview)
                     ELSE messages.reply_to_preview
                 END,
                 vector_clock = COALESCE(excluded.vector_clock, messages.vector_clock)"
        );

        self.with_connection("sqlite save message failed", move |conn| {
            conn.execute(
                &upsert_sql,
                params![
                    message.id.to_string(),
                    message.chat_id.0.to_string(),
                    message.sender_id.0.to_string(),
                    message.content,
                    message.timestamp_ms,
                    message_status_to_db(&message.status),
                    message.timestamp_ms,
                    i64::from(message.edit_version),
                    edited_at,
                    is_deleted,
                    deleted_at,
                    message.reply_to_id,
                    message.reply_to_preview,
                    vector_clock_json
                ],
            )?;
            Ok(())
        })
        .await
    }
}

impl StorageEngine for SqliteStorage {
    async fn save_message(&self, message: &Message) -> Result<()> {
        self.save_message_record(message.clone(), None).await
    }

    async fn get_message(&self, message_id: &str) -> Result<Option<Message>> {
        let message_id = parse_uuid_arg(message_id, "message_id")?.to_string();

        self.with_connection_result(move |conn| {
            conn.query_row(
                &format!("SELECT {MESSAGE_SELECT_COLUMNS} FROM messages WHERE id = ?1"),
                params![message_id],
                map_message_row,
            )
            .optional()
            .map_err(|error| sqlite_persistence("sqlite get message failed", error))
        })
        .await
    }

    async fn get_messages(
        &self,
        chat_id: &ChatId,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Message>> {
        let chat_id = chat_id.clone();

        self.with_connection("sqlite get messages failed", move |conn| {
            let mut statement = conn.prepare(&format!(
                "SELECT {MESSAGE_SELECT_COLUMNS}
                 FROM messages
                 WHERE chat_id = ?1
                 ORDER BY timestamp DESC, created_at DESC, id DESC"
            ))?;
            let rows =
                statement.query_map(params![chat_id.0.to_string()], map_stored_message_row)?;

            let mut records = Vec::new();
            for row in rows {
                records.push(row?);
            }

            records.sort_by(compare_stored_messages_for_query);

            Ok(records
                .into_iter()
                .skip(offset)
                .take(limit)
                .map(|record| record.message)
                .collect())
        })
        .await
    }

    async fn save_peer(&self, peer: &PeerInfo) -> Result<()> {
        let peer = peer.clone();
        let last_seen = now_ms();

        self.with_connection("sqlite save peer failed", move |conn| {
            conn.execute(
                "INSERT INTO peers (device_id, display_name, avatar_hash, last_seen, ws_port)
                 VALUES (?1, ?2, NULL, ?3, ?4)
                 ON CONFLICT(device_id) DO UPDATE SET
                     display_name = excluded.display_name,
                     last_seen = excluded.last_seen,
                     ws_port = excluded.ws_port",
                params![
                    peer.device_id.0.to_string(),
                    peer.display_name,
                    last_seen,
                    peer.ws_port
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn save_peer_key(
        &self,
        device_id: &str,
        public_key: &[u8],
        fingerprint: &str,
    ) -> Result<()> {
        let device_id = device_id.to_string();
        let public_key = public_key.to_vec();
        let fingerprint = fingerprint.to_string();
        let seen_at = now_ms();

        self.with_connection("sqlite save peer key failed", move |conn| {
            conn.execute(
                "INSERT INTO peer_keys (
                    device_id, public_key, fingerprint, verified, first_seen, last_seen
                 ) VALUES (?1, ?2, ?3, 0, ?4, ?5)
                 ON CONFLICT(device_id) DO UPDATE SET
                     public_key = excluded.public_key,
                     fingerprint = excluded.fingerprint,
                     last_seen = excluded.last_seen",
                params![device_id, public_key, fingerprint, seen_at, seen_at],
            )?;
            Ok(())
        })
        .await
    }

    async fn get_peer_key(&self, device_id: &str) -> Result<Option<PeerKeyInfo>> {
        let device_id = device_id.to_string();

        self.with_connection_result(move |conn| {
            conn.query_row(
                "SELECT device_id, public_key, fingerprint, verified, first_seen, last_seen
                 FROM peer_keys
                 WHERE device_id = ?1",
                params![device_id],
                map_peer_key_row,
            )
            .optional()
            .map_err(|error| sqlite_persistence("sqlite get peer key failed", error))
        })
        .await
    }

    async fn get_all_peer_keys(&self) -> Result<Vec<PeerKeyInfo>> {
        self.with_connection("sqlite list peer keys failed", move |conn| {
            let mut statement = conn.prepare(
                "SELECT device_id, public_key, fingerprint, verified, first_seen, last_seen
                 FROM peer_keys
                 ORDER BY last_seen DESC, device_id ASC",
            )?;
            let rows = statement.query_map([], map_peer_key_row)?;

            let mut keys = Vec::new();
            for row in rows {
                keys.push(row?);
            }

            Ok(keys)
        })
        .await
    }

    async fn set_peer_verified(&self, device_id: &str, verified: bool) -> Result<()> {
        let device_id = device_id.to_string();
        let verified = bool_to_db(verified);

        self.with_connection_result(move |conn| {
            let rows_updated = conn
                .execute(
                    "UPDATE peer_keys SET verified = ?2 WHERE device_id = ?1",
                    params![device_id.clone(), verified],
                )
                .map_err(|error| sqlite_persistence("sqlite set peer verified failed", error))?;
            if rows_updated == 0 {
                return Err(not_found_error("peer key", &device_id));
            }

            Ok(())
        })
        .await
    }

    async fn update_peer_key_last_seen(&self, device_id: &str) -> Result<()> {
        let device_id = device_id.to_string();
        let seen_at = now_ms();

        self.with_connection("sqlite update peer key last seen failed", move |conn| {
            conn.execute(
                "UPDATE peer_keys SET last_seen = ?2 WHERE device_id = ?1",
                params![device_id, seen_at],
            )?;
            Ok(())
        })
        .await
    }

    async fn save_sender_key(
        &self,
        group_id: &str,
        sender_device_id: &str,
        key_id: &Uuid,
        key_data: &[u8],
        epoch: u32,
        created_at_ms: u64,
    ) -> Result<()> {
        let group_id = group_id.to_string();
        let sender_device_id = sender_device_id.to_string();
        let key_id = key_id.to_string();
        let key_data = key_data.to_vec();
        let created_at = u64_to_db_i64(created_at_ms, "created_at_ms")?;

        self.with_connection("sqlite save sender key failed", move |conn| {
            conn.execute(
                "INSERT INTO sender_keys (
                    group_id, sender_device_id, epoch, key_id, key_data, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(group_id, sender_device_id, epoch) DO UPDATE SET
                     key_id = excluded.key_id,
                     key_data = excluded.key_data,
                     created_at = excluded.created_at",
                params![
                    group_id,
                    sender_device_id,
                    i64::from(epoch),
                    key_id,
                    key_data,
                    created_at
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn get_sender_key(
        &self,
        group_id: &str,
        sender_device_id: &str,
    ) -> Result<Option<SenderKeyInfo>> {
        let group_id = group_id.to_string();
        let sender_device_id = sender_device_id.to_string();

        self.with_connection_result(move |conn| {
            conn.query_row(
                "SELECT group_id, sender_device_id, epoch, key_id, key_data, created_at
                 FROM sender_keys
                 WHERE group_id = ?1 AND sender_device_id = ?2
                 ORDER BY epoch DESC, created_at DESC
                 LIMIT 1",
                params![group_id, sender_device_id],
                map_sender_key_row,
            )
            .optional()
            .map_err(|error| sqlite_persistence("sqlite get sender key failed", error))
        })
        .await
    }

    async fn get_sender_key_by_epoch(
        &self,
        group_id: &str,
        sender_device_id: &str,
        epoch: u32,
    ) -> Result<Option<SenderKeyInfo>> {
        let group_id = group_id.to_string();
        let sender_device_id = sender_device_id.to_string();

        self.with_connection_result(move |conn| {
            conn.query_row(
                "SELECT group_id, sender_device_id, epoch, key_id, key_data, created_at
                 FROM sender_keys
                 WHERE group_id = ?1 AND sender_device_id = ?2 AND epoch = ?3",
                params![group_id, sender_device_id, i64::from(epoch)],
                map_sender_key_row,
            )
            .optional()
            .map_err(|error| sqlite_persistence("sqlite get sender key by epoch failed", error))
        })
        .await
    }

    async fn delete_sender_keys_for_group(&self, group_id: &str) -> Result<()> {
        let group_id = group_id.to_string();

        self.with_connection("sqlite delete sender keys for group failed", move |conn| {
            conn.execute(
                "DELETE FROM sender_keys WHERE group_id = ?1",
                params![group_id],
            )?;
            Ok(())
        })
        .await
    }

    async fn update_message_status(&self, msg_id: &Uuid, status: MessageStatus) -> Result<()> {
        let msg_id = *msg_id;

        self.with_connection("sqlite update message status failed", move |conn| {
            conn.execute(
                "UPDATE messages SET status = ?2 WHERE id = ?1",
                params![msg_id.to_string(), message_status_to_db(&status)],
            )?;
            Ok(())
        })
        .await
    }

    async fn update_message_content(
        &self,
        message_id: &str,
        new_content: &str,
        edit_version: u32,
        edited_at_ms: u64,
    ) -> Result<()> {
        let message_id = parse_uuid_arg(message_id, "message_id")?.to_string();
        let new_content = new_content.to_string();
        let edited_at_ms = u64_to_db_i64(edited_at_ms, "edited_at_ms")?;

        self.with_connection_result(move |conn| {
            let rows_updated = conn
                .execute(
                    "UPDATE messages
                     SET content = ?2, edit_version = ?3, edited_at = ?4
                     WHERE id = ?1",
                    params![
                        message_id.clone(),
                        new_content,
                        i64::from(edit_version),
                        edited_at_ms
                    ],
                )
                .map_err(|error| {
                    sqlite_persistence("sqlite update message content failed", error)
                })?;
            if rows_updated == 0 {
                return Err(not_found_error("message", &message_id));
            }

            Ok(())
        })
        .await
    }

    async fn mark_message_deleted(&self, message_id: &str, deleted_at_ms: u64) -> Result<()> {
        let message_id = parse_uuid_arg(message_id, "message_id")?.to_string();
        let deleted_at_ms = u64_to_db_i64(deleted_at_ms, "deleted_at_ms")?;

        self.with_connection_result(move |conn| {
            let rows_updated = conn
                .execute(
                    "UPDATE messages
                     SET is_deleted = 1, deleted_at = ?2
                     WHERE id = ?1",
                    params![message_id.clone(), deleted_at_ms],
                )
                .map_err(|error| sqlite_persistence("sqlite mark message deleted failed", error))?;
            if rows_updated == 0 {
                return Err(not_found_error("message", &message_id));
            }

            Ok(())
        })
        .await
    }

    async fn save_transfer(&self, transfer: &TransferRecord) -> Result<()> {
        let transfer = transfer.clone();
        let size = match fs::metadata(&transfer.local_path) {
            Ok(metadata) => i64::try_from(metadata.len())
                .map_err(|_| CoreError::NotImplemented("sqlite transfer file size overflow"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(_) => {
                return Err(CoreError::NotImplemented(
                    "sqlite transfer file metadata failed",
                ))
            }
        };
        let bytes_transferred = u64_to_db_i64(transfer.bytes_transferred, "bytes_transferred")?;

        self.with_connection("sqlite save transfer failed", move |conn| {
            let created_at = now_ms();
            let completed_at =
                matches!(transfer.status, TransferStatus::Completed).then_some(created_at);

            let transaction = conn.transaction()?;
            transaction.execute(
                "INSERT INTO transfers (
                    id, peer_id, filename, size, direction, status, sha256, created_at, completed_at,
                    thumbnail_path, folder_id, folder_relative_path, bytes_transferred, resume_token
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT(id) DO UPDATE SET
                     peer_id = excluded.peer_id,
                     filename = excluded.filename,
                     size = excluded.size,
                     direction = excluded.direction,
                     status = excluded.status,
                     sha256 = excluded.sha256,
                     created_at = excluded.created_at,
                     completed_at = excluded.completed_at,
                     thumbnail_path = COALESCE(excluded.thumbnail_path, transfers.thumbnail_path),
                     folder_id = COALESCE(excluded.folder_id, transfers.folder_id),
                     folder_relative_path = COALESCE(excluded.folder_relative_path, transfers.folder_relative_path),
                     bytes_transferred = MAX(transfers.bytes_transferred, excluded.bytes_transferred),
                     resume_token = COALESCE(excluded.resume_token, transfers.resume_token)",
                params![
                    transfer.id.to_string(),
                    transfer.peer_id.0.to_string(),
                    transfer.file_name,
                    size,
                    TRANSFER_DIRECTION_SEND,
                    transfer_status_to_db(&transfer.status),
                    created_at,
                    completed_at,
                    transfer.thumbnail_path,
                    transfer.folder_id,
                    transfer.folder_relative_path,
                    bytes_transferred,
                    transfer.resume_token
                ],
            )?;
            transaction.execute(
                "INSERT INTO transfer_context (id, chat_id, local_path)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(id) DO UPDATE SET
                     chat_id = excluded.chat_id,
                     local_path = excluded.local_path",
                params![
                    transfer.id.to_string(),
                    transfer
                        .chat_id
                        .as_ref()
                        .map(|chat_id| chat_id.0.to_string()),
                    transfer.local_path.to_string_lossy().into_owned()
                ],
            )?;
            transaction.commit()?;
            Ok(())
        })
        .await
    }

    async fn get_transfers(&self, limit: usize, offset: usize) -> Result<Vec<TransferRecord>> {
        self.with_connection("sqlite get transfers failed", move |conn| {
            let mut statement = conn.prepare(
                "SELECT
                    t.id,
                    t.peer_id,
                    tc.chat_id,
                    t.filename,
                    tc.local_path,
                    t.status,
                    t.thumbnail_path,
                    t.folder_id,
                    t.folder_relative_path,
                    t.bytes_transferred,
                    t.resume_token
                 FROM transfers t
                 LEFT JOIN transfer_context tc ON tc.id = t.id
                 ORDER BY t.created_at DESC, t.id DESC
                 LIMIT ?1 OFFSET ?2",
            )?;
            let rows = statement.query_map(params![limit as i64, offset as i64], |row| {
                let transfer_id: String = row.get(0)?;
                let peer_id: String = row.get(1)?;
                let chat_id: Option<String> = row.get(2)?;
                let file_name: String = row.get(3)?;
                let local_path: Option<String> = row.get(4)?;
                let status: String = row.get(5)?;
                let thumbnail_path: Option<String> = row.get(6)?;
                let folder_id: Option<String> = row.get(7)?;
                let folder_relative_path: Option<String> = row.get(8)?;
                let bytes_transferred: i64 = row.get(9)?;
                let resume_token: Option<String> = row.get(10)?;

                Ok(TransferRecord {
                    id: parse_uuid(&transfer_id)?,
                    peer_id: DeviceId(parse_uuid(&peer_id)?),
                    chat_id: chat_id.as_deref().map(parse_uuid).transpose()?.map(ChatId),
                    file_name: file_name.clone(),
                    local_path: local_path
                        .map(PathBuf::from)
                        .unwrap_or_else(|| PathBuf::from(file_name)),
                    status: transfer_status_from_db(&status)?,
                    thumbnail_path,
                    folder_id,
                    folder_relative_path,
                    bytes_transferred: db_i64_to_u64(bytes_transferred, 9)?,
                    resume_token,
                })
            })?;

            let mut transfers = Vec::new();
            for row in rows {
                transfers.push(row?);
            }

            Ok(transfers)
        })
        .await
    }

    async fn update_transfer_status(
        &self,
        transfer_id: &Uuid,
        status: TransferStatus,
    ) -> Result<()> {
        let transfer_id = *transfer_id;

        self.with_connection("sqlite update transfer status failed", move |conn| {
            let completed_at = matches!(status, TransferStatus::Completed).then_some(now_ms());

            conn.execute(
                "UPDATE transfers
                 SET status = ?2, completed_at = ?3
                 WHERE id = ?1",
                params![
                    transfer_id.to_string(),
                    transfer_status_to_db(&status),
                    completed_at
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn save_thumbnail_path(&self, transfer_id: &str, thumbnail_path: &str) -> Result<()> {
        let transfer_id = parse_uuid_arg(transfer_id, "transfer_id")?.to_string();
        let thumbnail_path = thumbnail_path.to_string();

        self.with_connection_result(move |conn| {
            let rows_updated = conn
                .execute(
                    "UPDATE transfers SET thumbnail_path = ?2 WHERE id = ?1",
                    params![transfer_id.clone(), thumbnail_path],
                )
                .map_err(|error| sqlite_persistence("sqlite save thumbnail path failed", error))?;
            if rows_updated == 0 {
                return Err(not_found_error("transfer", &transfer_id));
            }

            Ok(())
        })
        .await
    }

    async fn get_thumbnail_path(&self, transfer_id: &str) -> Result<Option<String>> {
        let transfer_id = parse_uuid_arg(transfer_id, "transfer_id")?.to_string();

        self.with_connection_result(move |conn| {
            conn.query_row(
                "SELECT thumbnail_path FROM transfers WHERE id = ?1",
                params![transfer_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(|value| value.flatten())
            .map_err(|error| sqlite_persistence("sqlite get thumbnail path failed", error))
        })
        .await
    }

    async fn update_bytes_transferred(&self, transfer_id: &str, bytes: u64) -> Result<()> {
        let transfer_id = parse_uuid_arg(transfer_id, "transfer_id")?.to_string();
        let bytes = u64_to_db_i64(bytes, "bytes")?;

        self.with_connection_result(move |conn| {
            let rows_updated = conn
                .execute(
                    "UPDATE transfers SET bytes_transferred = ?2 WHERE id = ?1",
                    params![transfer_id.clone(), bytes],
                )
                .map_err(|error| {
                    sqlite_persistence("sqlite update bytes transferred failed", error)
                })?;
            if rows_updated == 0 {
                return Err(not_found_error("transfer", &transfer_id));
            }

            Ok(())
        })
        .await
    }

    async fn get_transfer_resume_info(&self, transfer_id: &str) -> Result<Option<ResumeInfo>> {
        let transfer_id = parse_uuid_arg(transfer_id, "transfer_id")?.to_string();

        self.with_connection_result(move |conn| {
            conn.query_row(
                "SELECT bytes_transferred, resume_token FROM transfers WHERE id = ?1",
                params![transfer_id],
                |row| {
                    let bytes_transferred: i64 = row.get(0)?;

                    Ok(ResumeInfo {
                        bytes_transferred: db_i64_to_u64(bytes_transferred, 0)?,
                        resume_token: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(|error| sqlite_persistence("sqlite get transfer resume info failed", error))
        })
        .await
    }
}

#[derive(Clone)]
struct SqliteConnectionManager {
    path: PathBuf,
}

impl ManageConnection for SqliteConnectionManager {
    type Connection = Connection;
    type Error = rusqlite::Error;

    fn connect(&self) -> std::result::Result<Self::Connection, Self::Error> {
        let connection = Connection::open(&self.path)?;
        configure_connection(&connection)?;
        Ok(connection)
    }

    fn is_valid(&self, conn: &mut Self::Connection) -> std::result::Result<(), Self::Error> {
        conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))?;
        Ok(())
    }

    fn has_broken(&self, _conn: &mut Self::Connection) -> bool {
        false
    }
}

fn configure_connection(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS))?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;

    let journal_mode: String =
        conn.query_row("PRAGMA journal_mode = WAL;", [], |row| row.get(0))?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(rusqlite::Error::InvalidQuery);
    }

    Ok(())
}

fn run_migrations(conn: &mut Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _migrations (version INTEGER, applied_at TEXT);",
    )?;

    let mut statement = conn.prepare("SELECT version FROM _migrations ORDER BY version")?;
    let applied_rows = statement.query_map([], |row| row.get::<_, i64>(0))?;
    let mut applied_versions = Vec::new();
    for row in applied_rows {
        applied_versions.push(row?);
    }
    drop(statement);

    let mut current_user_version = read_user_version(conn)?;
    let mut target_user_version = applied_versions
        .iter()
        .copied()
        .max()
        .unwrap_or(current_user_version);

    for migration in MIGRATIONS {
        if applied_versions.contains(&migration.version) {
            target_user_version = target_user_version.max(migration.version);
            continue;
        }

        if current_user_version >= migration.version {
            record_applied_migration(conn, migration.version)?;
            applied_versions.push(migration.version);
            target_user_version = target_user_version.max(migration.version);
            continue;
        }

        let transaction = conn.transaction()?;
        transaction.execute_batch(migration.sql)?;
        transaction.execute(
            "INSERT INTO _migrations (version, applied_at)
             VALUES (?1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            params![migration.version],
        )?;
        transaction.commit()?;

        applied_versions.push(migration.version);
        current_user_version = read_user_version(conn)?.max(migration.version);
        target_user_version = target_user_version.max(migration.version);
    }

    if read_user_version(conn)? < target_user_version {
        conn.execute_batch(&format!("PRAGMA user_version = {target_user_version};"))?;
    }

    Ok(())
}

fn record_applied_migration(conn: &Connection, version: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO _migrations (version, applied_at)
         VALUES (?1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        params![version],
    )?;
    Ok(())
}

fn read_user_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("PRAGMA user_version;", [], |row| row.get(0))
}

fn map_chat_row(row: &Row<'_>) -> rusqlite::Result<ChatRecord> {
    let chat_id: String = row.get(0)?;
    let chat_type: String = row.get(1)?;

    Ok(ChatRecord {
        id: ChatId(parse_uuid(&chat_id)?),
        chat_type: ChatType::from_db_value(&chat_type)?,
        name: row.get(2)?,
        created_at_ms: row.get(3)?,
    })
}

fn map_peer_key_row(row: &Row<'_>) -> rusqlite::Result<PeerKeyInfo> {
    let verified: i64 = row.get(3)?;
    let first_seen: i64 = row.get(4)?;
    let last_seen: i64 = row.get(5)?;

    Ok(PeerKeyInfo {
        device_id: row.get(0)?,
        public_key: row.get(1)?,
        fingerprint: row.get(2)?,
        verified: db_to_bool(verified),
        first_seen: db_i64_to_u64(first_seen, 4)?,
        last_seen: db_i64_to_u64(last_seen, 5)?,
    })
}

fn map_sender_key_row(row: &Row<'_>) -> rusqlite::Result<SenderKeyInfo> {
    let epoch: i64 = row.get(2)?;
    let key_id: String = row.get(3)?;
    let created_at: i64 = row.get(5)?;

    Ok(SenderKeyInfo {
        group_id: row.get(0)?,
        sender_device_id: row.get(1)?,
        epoch: db_i64_to_u32(epoch, 2)?,
        key_id: parse_uuid(&key_id)?,
        key_data: row.get(4)?,
        created_at: db_i64_to_u64(created_at, 5)?,
    })
}

fn map_message_row(row: &Row<'_>) -> rusqlite::Result<Message> {
    let message_id: String = row.get(0)?;
    let chat_id: String = row.get(1)?;
    let sender_id: String = row.get(2)?;
    let status: String = row.get(5)?;
    let edit_version: i64 = row.get(6)?;
    let edited_at: Option<i64> = row.get(7)?;
    let is_deleted: i64 = row.get(8)?;
    let deleted_at: Option<i64> = row.get(9)?;

    Ok(Message {
        id: parse_uuid(&message_id)?,
        chat_id: ChatId(parse_uuid(&chat_id)?),
        sender_id: UserId(parse_uuid(&sender_id)?),
        content: row.get(3)?,
        timestamp_ms: row.get(4)?,
        status: message_status_from_db(&status)?,
        edit_version: db_i64_to_u32(edit_version, 6)?,
        edited_at: option_db_i64_to_u64(edited_at, 7)?,
        is_deleted: db_to_bool(is_deleted),
        deleted_at: option_db_i64_to_u64(deleted_at, 9)?,
        reply_to_id: row.get(10)?,
        reply_to_preview: row.get(11)?,
    })
}

fn map_stored_message_row(row: &Row<'_>) -> rusqlite::Result<StoredMessageRecord> {
    let vector_clock_json: Option<String> = row.get(13)?;

    Ok(StoredMessageRecord {
        message: map_message_row(row)?,
        created_at_ms: row.get(12)?,
        vector_clock: vector_clock_json.as_deref().and_then(parse_vector_clock),
    })
}

fn map_og_cache_row(row: &Row<'_>) -> rusqlite::Result<CachedOgMetadata> {
    Ok(CachedOgMetadata {
        metadata: OgMetadata {
            url: row.get(0)?,
            title: row.get(1)?,
            description: row.get(2)?,
            image_url: row.get(3)?,
            site_name: row.get(4)?,
        },
        fetched_at_ms: row.get(5)?,
        ttl_seconds: row.get(6)?,
    })
}

fn compare_stored_messages_for_query(
    left: &StoredMessageRecord,
    right: &StoredMessageRecord,
) -> Ordering {
    if let (Some(left_clock), Some(right_clock)) = (&left.vector_clock, &right.vector_clock) {
        if let Some(ordering) = compare_vector_clocks(left_clock, right_clock) {
            if ordering != Ordering::Equal {
                return ordering.reverse();
            }
        }
    }

    compare_messages_by_timestamp(left, right)
}

fn compare_messages_by_timestamp(
    left: &StoredMessageRecord,
    right: &StoredMessageRecord,
) -> Ordering {
    right
        .message
        .timestamp_ms
        .cmp(&left.message.timestamp_ms)
        .then_with(|| right.created_at_ms.cmp(&left.created_at_ms))
        .then_with(|| right.message.id.as_bytes().cmp(left.message.id.as_bytes()))
}

fn compare_vector_clocks(
    left: &HashMap<String, u64>,
    right: &HashMap<String, u64>,
) -> Option<Ordering> {
    let mut left_is_less = false;
    let mut left_is_greater = false;

    for key in left.keys().chain(right.keys()) {
        let left_value = left.get(key).copied().unwrap_or(0);
        let right_value = right.get(key).copied().unwrap_or(0);

        left_is_less |= left_value < right_value;
        left_is_greater |= left_value > right_value;

        if left_is_less && left_is_greater {
            return None;
        }
    }

    match (left_is_less, left_is_greater) {
        (true, false) => Some(Ordering::Less),
        (false, true) => Some(Ordering::Greater),
        (false, false) => Some(Ordering::Equal),
        (true, true) => None,
    }
}

fn serialize_vector_clock(clock: &HashMap<String, u64>) -> String {
    let sorted_clock = clock
        .iter()
        .map(|(device_id, counter)| (device_id.clone(), *counter))
        .collect::<BTreeMap<_, _>>();
    serde_json::to_string(&sorted_clock).expect("vector clock serialization should succeed")
}

fn parse_vector_clock(json: &str) -> Option<HashMap<String, u64>> {
    serde_json::from_str(json).ok()
}

fn load_reply_counts(conn: &Connection, message_ids: &[String]) -> Result<HashMap<String, i64>> {
    let mut reply_counts = message_ids
        .iter()
        .cloned()
        .map(|message_id| (message_id, 0))
        .collect::<HashMap<_, _>>();

    for chunk in message_ids.chunks(SQLITE_MAX_VARIABLES) {
        let placeholders = chunk
            .iter()
            .enumerate()
            .map(|(index, _)| format!("?{}", index + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT reply_to_id, COUNT(*) \
             FROM messages \
             WHERE reply_to_id IN ({placeholders}) \
             GROUP BY reply_to_id"
        );
        let mut statement = conn.prepare(&sql).map_err(|error| {
            sqlite_persistence("sqlite prepare reply count query failed", error)
        })?;
        let rows = statement
            .query_map(params_from_iter(chunk.iter()), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|error| sqlite_persistence("sqlite query reply counts failed", error))?;

        for row in rows {
            let (reply_to_id, count) = row
                .map_err(|error| sqlite_persistence("sqlite read reply count row failed", error))?;
            reply_counts.insert(reply_to_id, count);
        }
    }

    Ok(reply_counts)
}

fn parse_uuid(raw: &str) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(raw)
        .map_err(|error| rusqlite::Error::FromSqlConversionFailure(0, Type::Text, Box::new(error)))
}

fn validate_folder_file_status(status: &str) -> Result<()> {
    match status {
        SqliteStorage::FOLDER_FILE_STATUS_PENDING
        | SqliteStorage::FOLDER_FILE_STATUS_COMPLETED
        | SqliteStorage::FOLDER_FILE_STATUS_FAILED => Ok(()),
        _ => Err(CoreError::Validation(format!(
            "invalid folder file status: {status}"
        ))),
    }
}

fn message_status_to_db(status: &MessageStatus) -> &'static str {
    match status {
        MessageStatus::Sent => "sent",
        MessageStatus::Delivered => "delivered",
        MessageStatus::Failed => "failed",
    }
}

fn message_status_from_db(raw: &str) -> rusqlite::Result<MessageStatus> {
    match raw {
        "sent" => Ok(MessageStatus::Sent),
        "delivered" => Ok(MessageStatus::Delivered),
        "failed" => Ok(MessageStatus::Failed),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn transfer_status_to_db(status: &TransferStatus) -> &'static str {
    match status {
        TransferStatus::Pending => "pending",
        TransferStatus::Active => "active",
        TransferStatus::Completed => "completed",
        TransferStatus::Failed => "failed",
        TransferStatus::Cancelled => "cancelled",
    }
}

fn transfer_status_from_db(raw: &str) -> rusqlite::Result<TransferStatus> {
    match raw {
        "pending" => Ok(TransferStatus::Pending),
        "active" => Ok(TransferStatus::Active),
        "completed" => Ok(TransferStatus::Completed),
        "failed" => Ok(TransferStatus::Failed),
        "cancelled" => Ok(TransferStatus::Cancelled),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn parse_uuid_arg(raw: &str, field: &str) -> Result<Uuid> {
    Uuid::parse_str(raw).map_err(|_| CoreError::Validation(format!("invalid {field}: {raw}")))
}

fn sqlite_persistence(operation: &str, error: rusqlite::Error) -> CoreError {
    CoreError::Persistence(format!("{operation}: {error}"))
}

fn not_found_error(kind: &str, id: &str) -> CoreError {
    CoreError::Persistence(format!("{kind} not found: {id}"))
}

fn bool_to_db(value: bool) -> i64 {
    if value {
        1
    } else {
        0
    }
}

fn db_to_bool(value: i64) -> bool {
    value != 0
}

fn u64_to_db_i64(value: u64, field: &str) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| CoreError::Validation(format!("{field} exceeds sqlite integer range")))
}

fn option_u64_to_db_i64(value: Option<u64>, field: &str) -> Result<Option<i64>> {
    value.map(|value| u64_to_db_i64(value, field)).transpose()
}

fn db_i64_to_u32(value: i64, column: usize) -> rusqlite::Result<u32> {
    u32::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(column, Type::Integer, Box::new(error))
    })
}

fn db_i64_to_u64(value: i64, column: usize) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(column, Type::Integer, Box::new(error))
    })
}

fn option_db_i64_to_u64(value: Option<i64>, column: usize) -> rusqlite::Result<Option<u64>> {
    value.map(|value| db_i64_to_u64(value, column)).transpose()
}
