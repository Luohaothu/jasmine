use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jasmine_core::{
    ChatId, CoreError, DeviceId, Message, MessageStatus, PeerInfo, Result, StorageEngine,
    TransferRecord, TransferStatus, UserId,
};
use r2d2::{ManageConnection, Pool};
use rusqlite::types::Type;
use rusqlite::{params, Connection, OptionalExtension, Row};
use tokio::task;
use uuid::Uuid;

const POOL_SIZE: u32 = 4;
const BUSY_TIMEOUT_MS: u64 = 5_000;
const TRANSFER_DIRECTION_SEND: &str = "send";
const MESSAGE_SELECT_COLUMNS: &str =
    "id, chat_id, sender_id, content, timestamp, status, edit_version, edited_at, is_deleted, deleted_at, reply_to_id, reply_to_preview";

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: include_str!("../migrations/V1__initial.sql"),
    },
    Migration {
        version: 2,
        sql: include_str!("../migrations/V2__edit_delete_richtext.sql"),
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

#[derive(Clone)]
pub struct SqliteStorage {
    db_path: PathBuf,
    pool: Pool<SqliteConnectionManager>,
}

impl SqliteStorage {
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
}

impl StorageEngine for SqliteStorage {
    async fn save_message(&self, message: &Message) -> Result<()> {
        let message = message.clone();
        let edited_at = option_u64_to_db_i64(message.edited_at, "edited_at")?;
        let deleted_at = option_u64_to_db_i64(message.deleted_at, "deleted_at")?;
        let is_deleted = bool_to_db(message.is_deleted);

        self.with_connection("sqlite save message failed", move |conn| {
            conn.execute(
                "INSERT INTO messages (
                    id, chat_id, sender_id, content, timestamp, status, created_at,
                    edit_version, edited_at, is_deleted, deleted_at, reply_to_id, reply_to_preview
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT(id) DO UPDATE SET
                     chat_id = excluded.chat_id,
                     sender_id = excluded.sender_id,
                     content = excluded.content,
                     timestamp = excluded.timestamp,
                     status = excluded.status,
                     created_at = excluded.created_at,
                     edit_version = MAX(messages.edit_version, excluded.edit_version),
                     edited_at = COALESCE(excluded.edited_at, messages.edited_at),
                     is_deleted = MAX(messages.is_deleted, excluded.is_deleted),
                     deleted_at = COALESCE(excluded.deleted_at, messages.deleted_at),
                     reply_to_id = COALESCE(excluded.reply_to_id, messages.reply_to_id),
                     reply_to_preview = COALESCE(excluded.reply_to_preview, messages.reply_to_preview)",
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
                    message.reply_to_preview
                ],
            )?;
            Ok(())
        })
        .await
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
                 ORDER BY timestamp DESC, created_at DESC, id DESC
                 LIMIT ?2 OFFSET ?3"
            ))?;
            let rows = statement.query_map(
                params![chat_id.0.to_string(), limit as i64, offset as i64],
                map_message_row,
            )?;

            let mut messages = Vec::new();
            for row in rows {
                messages.push(row?);
            }

            Ok(messages)
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

        self.with_connection("sqlite save transfer failed", move |conn| {
            let created_at = now_ms();
            let completed_at =
                matches!(transfer.status, TransferStatus::Completed).then_some(created_at);

            let transaction = conn.transaction()?;
            transaction.execute(
                "INSERT INTO transfers (
                    id, peer_id, filename, size, direction, status, sha256, created_at, completed_at,
                    thumbnail_path, folder_id, folder_relative_path
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8, ?9, ?10, ?11)
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
                     folder_relative_path = COALESCE(excluded.folder_relative_path, transfers.folder_relative_path)",
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
                    transfer.folder_relative_path
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
                    t.folder_relative_path
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

fn parse_uuid(raw: &str) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(raw)
        .map_err(|error| rusqlite::Error::FromSqlConversionFailure(0, Type::Text, Box::new(error)))
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
