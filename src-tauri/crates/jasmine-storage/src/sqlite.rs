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

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: include_str!("../migrations/V1__initial.sql"),
}];

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
}

impl StorageEngine for SqliteStorage {
    async fn save_message(&self, message: &Message) -> Result<()> {
        let message = message.clone();

        self.with_connection("sqlite save message failed", move |conn| {
            conn.execute(
                "INSERT INTO messages (id, chat_id, sender_id, content, timestamp, status, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET
                     chat_id = excluded.chat_id,
                     sender_id = excluded.sender_id,
                     content = excluded.content,
                     timestamp = excluded.timestamp,
                     status = excluded.status,
                     created_at = excluded.created_at",
                params![
                    message.id.to_string(),
                    message.chat_id.0.to_string(),
                    message.sender_id.0.to_string(),
                    message.content,
                    message.timestamp_ms,
                    message_status_to_db(&message.status),
                    message.timestamp_ms
                ],
            )?;
            Ok(())
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
            let mut statement = conn.prepare(
                "SELECT id, chat_id, sender_id, content, timestamp, status
                 FROM messages
                 WHERE chat_id = ?1
                 ORDER BY timestamp DESC, created_at DESC, id DESC
                 LIMIT ?2 OFFSET ?3",
            )?;
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
                    id, peer_id, filename, size, direction, status, sha256, created_at, completed_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8)
                 ON CONFLICT(id) DO UPDATE SET
                     peer_id = excluded.peer_id,
                     filename = excluded.filename,
                     size = excluded.size,
                     direction = excluded.direction,
                     status = excluded.status,
                     sha256 = excluded.sha256,
                     created_at = excluded.created_at,
                     completed_at = excluded.completed_at",
                params![
                    transfer.id.to_string(),
                    transfer.peer_id.0.to_string(),
                    transfer.file_name,
                    size,
                    TRANSFER_DIRECTION_SEND,
                    transfer_status_to_db(&transfer.status),
                    created_at,
                    completed_at
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
                    t.status
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

                Ok(TransferRecord {
                    id: parse_uuid(&transfer_id)?,
                    peer_id: DeviceId(parse_uuid(&peer_id)?),
                    chat_id: chat_id.as_deref().map(parse_uuid).transpose()?.map(ChatId),
                    file_name: file_name.clone(),
                    local_path: local_path
                        .map(PathBuf::from)
                        .unwrap_or_else(|| PathBuf::from(file_name)),
                    status: transfer_status_from_db(&status)?,
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

    for migration in MIGRATIONS {
        if applied_versions.contains(&migration.version) {
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
    }

    Ok(())
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

    Ok(Message {
        id: parse_uuid(&message_id)?,
        chat_id: ChatId(parse_uuid(&chat_id)?),
        sender_id: UserId(parse_uuid(&sender_id)?),
        content: row.get(3)?,
        timestamp_ms: row.get(4)?,
        status: message_status_from_db(&status)?,
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
