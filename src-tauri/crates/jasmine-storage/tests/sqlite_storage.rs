use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jasmine_core::{
    ChatId, CoreError, DeviceId, Message, MessageStatus, PeerInfo, StorageEngine, TransferRecord,
    TransferStatus, UserId,
};
use jasmine_storage::{ChatRecord, ChatType, SqliteStorage};
use rusqlite::{params, Connection};
use tempfile::TempDir;
use tokio::task::JoinSet;
use uuid::Uuid;

const V1_MIGRATION_SQL: &str = include_str!("../migrations/V1__initial.sql");

#[test]
fn migration_creates_database_and_records_v2_schema() {
    let (_temp_dir, db_path) = temp_db_path("create");

    let _storage = SqliteStorage::open(&db_path).expect("storage opens");

    assert!(db_path.exists(), "database file should be created");

    let conn = Connection::open(&db_path).expect("open sqlite db");
    assert_tables_exist(
        &conn,
        &[
            "_migrations",
            "messages",
            "chats",
            "chat_members",
            "peers",
            "transfers",
        ],
    );

    let versions = applied_versions(&conn);
    assert_eq!(versions, vec![1, 2]);
    assert_eq!(user_version(&conn), 2);
    assert_columns_exist(
        &conn,
        "messages",
        &[
            "edit_version",
            "edited_at",
            "is_deleted",
            "deleted_at",
            "reply_to_id",
            "reply_to_preview",
        ],
    );
    assert_columns_exist(
        &conn,
        "transfers",
        &["thumbnail_path", "folder_id", "folder_relative_path"],
    );
}

#[test]
fn migration_upgrades_v0_database_to_v2() {
    let (_temp_dir, db_path) = temp_db_path("upgrade");
    Connection::open(&db_path)
        .expect("create empty v0 db")
        .execute_batch("PRAGMA user_version = 0;")
        .expect("mark as v0");

    let _storage = SqliteStorage::open(&db_path).expect("storage opens");

    let conn = Connection::open(&db_path).expect("open sqlite db");
    assert_tables_exist(
        &conn,
        &[
            "_migrations",
            "messages",
            "chats",
            "chat_members",
            "peers",
            "transfers",
        ],
    );
    assert_eq!(applied_versions(&conn), vec![1, 2]);
    assert_eq!(user_version(&conn), 2);
}

#[test]
fn migration_upgrades_existing_v1_database_to_v2_and_preserves_data() {
    let (_temp_dir, db_path) = temp_db_path("upgrade-v1-to-v2");
    let message_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let sender_id = Uuid::new_v4();
    let transfer_id = Uuid::new_v4();
    let peer_id = Uuid::new_v4();

    let conn = Connection::open(&db_path).expect("create v1 db");
    conn.execute_batch(V1_MIGRATION_SQL)
        .expect("apply v1 schema manually");
    conn.execute(
        "INSERT INTO messages (id, chat_id, sender_id, content, timestamp, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            message_id.to_string(),
            chat_id.to_string(),
            sender_id.to_string(),
            "legacy message",
            1_234_i64,
            "sent",
            1_234_i64
        ],
    )
    .expect("insert legacy message");
    conn.execute(
        "INSERT INTO transfers (
            id, peer_id, filename, size, direction, status, sha256, created_at, completed_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            transfer_id.to_string(),
            peer_id.to_string(),
            "legacy.bin",
            512_i64,
            "send",
            "pending",
            Option::<String>::None,
            2_000_i64,
            Option::<i64>::None
        ],
    )
    .expect("insert legacy transfer");
    conn.execute(
        "INSERT INTO transfer_context (id, chat_id, local_path) VALUES (?1, ?2, ?3)",
        params![
            transfer_id.to_string(),
            Option::<String>::None,
            "/tmp/legacy.bin"
        ],
    )
    .expect("insert legacy transfer context");
    conn.execute_batch("PRAGMA user_version = 1;")
        .expect("mark db as v1");
    drop(conn);

    let storage = SqliteStorage::open(&db_path).expect("storage opens and migrates");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build runtime");
    let loaded = runtime
        .block_on(async { storage.get_message(&message_id.to_string()).await })
        .expect("load migrated message")
        .expect("migrated message exists");

    assert_eq!(loaded.id, message_id);
    assert_eq!(loaded.chat_id, ChatId(chat_id));
    assert_eq!(loaded.sender_id, UserId(sender_id));
    assert_eq!(loaded.content, "legacy message");
    assert_eq!(loaded.status, MessageStatus::Sent);
    assert_eq!(loaded.edit_version, 0);
    assert_eq!(loaded.edited_at, None);
    assert!(!loaded.is_deleted);
    assert_eq!(loaded.deleted_at, None);
    assert_eq!(loaded.reply_to_id, None);
    assert_eq!(loaded.reply_to_preview, None);

    let conn = Connection::open(&db_path).expect("open migrated db");
    assert_eq!(applied_versions(&conn), vec![1, 2]);
    assert_eq!(user_version(&conn), 2);

    let transfer_row = conn
        .query_row(
            "SELECT filename, thumbnail_path, folder_id, folder_relative_path
             FROM transfers WHERE id = ?1",
            params![transfer_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .expect("query migrated transfer row");
    assert_eq!(transfer_row.0, "legacy.bin");
    assert_eq!(transfer_row.1, None);
    assert_eq!(transfer_row.2, None);
    assert_eq!(transfer_row.3, None);
}

#[test]
fn migration_rejects_corrupted_database() {
    let (_temp_dir, db_path) = temp_db_path("corrupted");
    fs::write(&db_path, b"not-a-sqlite-database").expect("write corrupt bytes");

    let result = SqliteStorage::open(&db_path);

    assert!(result.is_err(), "corrupted db should be rejected");
}

#[test]
fn wal_mode_is_enabled_for_new_connections() {
    let (_temp_dir, db_path) = temp_db_path("wal");

    let _storage = SqliteStorage::open(&db_path).expect("storage opens");

    let conn = Connection::open(&db_path).expect("open sqlite db");
    let journal_mode: String = conn
        .query_row("PRAGMA journal_mode;", [], |row| row.get(0))
        .expect("read journal mode");

    assert_eq!(journal_mode, "wal");
}

#[tokio::test]
async fn message_crud_persists_and_orders_descending() {
    let (_temp_dir, db_path) = temp_db_path("message-order");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let chat_id = chat_id();

    let older = message(chat_id.clone(), 1_000, MessageStatus::Sent, "older");
    let newer = message(chat_id.clone(), 2_000, MessageStatus::Delivered, "newer");

    storage.save_message(&older).await.expect("save older");
    storage.save_message(&newer).await.expect("save newer");

    let messages = storage
        .get_messages(&chat_id, 10, 0)
        .await
        .expect("query messages");

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].id, newer.id);
    assert_eq!(messages[1].id, older.id);
}

#[tokio::test]
async fn message_crud_applies_limit_and_offset() {
    let (_temp_dir, db_path) = temp_db_path("message-pagination");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let chat_id = chat_id();

    let first = message(chat_id.clone(), 1_000, MessageStatus::Sent, "first");
    let second = message(chat_id.clone(), 2_000, MessageStatus::Sent, "second");
    let third = message(chat_id.clone(), 3_000, MessageStatus::Sent, "third");

    storage.save_message(&first).await.expect("save first");
    storage.save_message(&second).await.expect("save second");
    storage.save_message(&third).await.expect("save third");

    let page = storage
        .get_messages(&chat_id, 1, 1)
        .await
        .expect("query page");

    assert_eq!(page.len(), 1);
    assert_eq!(page[0].id, second.id);
}

#[tokio::test]
async fn message_crud_filters_by_chat() {
    let (_temp_dir, db_path) = temp_db_path("message-chat-filter");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let primary_chat = chat_id();
    let other_chat = chat_id();

    storage
        .save_message(&message(
            primary_chat.clone(),
            1_000,
            MessageStatus::Sent,
            "hello",
        ))
        .await
        .expect("save primary chat message");
    storage
        .save_message(&message(
            other_chat.clone(),
            1_500,
            MessageStatus::Sent,
            "ignored",
        ))
        .await
        .expect("save other chat message");

    let messages = storage
        .get_messages(&primary_chat, 10, 0)
        .await
        .expect("query primary chat messages");

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].chat_id, primary_chat);
    assert_eq!(messages[0].content, "hello");
}

#[tokio::test]
async fn message_status_update_transitions_sent_delivered_failed() {
    let (_temp_dir, db_path) = temp_db_path("message-status");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let chat_id = chat_id();
    let message = message(chat_id.clone(), 1_000, MessageStatus::Sent, "status");

    storage.save_message(&message).await.expect("save message");
    storage
        .update_message_status(&message.id, MessageStatus::Delivered)
        .await
        .expect("mark delivered");

    let delivered = storage
        .get_messages(&chat_id, 10, 0)
        .await
        .expect("query delivered message");
    assert_eq!(delivered[0].status, MessageStatus::Delivered);

    storage
        .update_message_status(&message.id, MessageStatus::Failed)
        .await
        .expect("mark failed");

    let failed = storage
        .get_messages(&chat_id, 10, 0)
        .await
        .expect("query failed message");
    assert_eq!(failed[0].status, MessageStatus::Failed);
}

#[tokio::test]
async fn message_v2_fields_round_trip_through_storage() {
    let (_temp_dir, db_path) = temp_db_path("message-v2-roundtrip");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let chat_id = chat_id();
    let mut message = message(
        chat_id.clone(),
        3_000,
        MessageStatus::Delivered,
        "reply body",
    );

    message.edit_version = 4;
    message.edited_at = Some(4_000);
    message.is_deleted = true;
    message.deleted_at = Some(4_500);
    message.reply_to_id = Some(Uuid::new_v4().to_string());
    message.reply_to_preview = Some("quoted preview".to_string());

    storage
        .save_message(&message)
        .await
        .expect("save v2 message");

    let loaded = storage
        .get_message(&message.id.to_string())
        .await
        .expect("get message")
        .expect("message exists");
    assert_eq!(loaded, message);

    let messages = storage
        .get_messages(&chat_id, 10, 0)
        .await
        .expect("list messages");
    assert_eq!(messages, vec![message]);
}

#[tokio::test]
async fn storage_v2_methods_update_messages_and_thumbnail_paths() {
    let (temp_dir, db_path) = temp_db_path("storage-v2-methods");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let chat_id = chat_id();
    let mut message = message(chat_id, 1_000, MessageStatus::Sent, "original");
    let reply_to_id = Uuid::new_v4().to_string();

    message.reply_to_id = Some(reply_to_id.clone());
    message.reply_to_preview = Some("reply snapshot".to_string());
    storage.save_message(&message).await.expect("save message");

    let loaded = storage
        .get_message(&message.id.to_string())
        .await
        .expect("get message")
        .expect("message exists");
    assert_eq!(loaded.edit_version, 0);
    assert_eq!(loaded.edited_at, None);
    assert!(!loaded.is_deleted);
    assert_eq!(loaded.deleted_at, None);
    assert_eq!(loaded.reply_to_id.as_deref(), Some(reply_to_id.as_str()));
    assert_eq!(loaded.reply_to_preview.as_deref(), Some("reply snapshot"));

    storage
        .update_message_content(&message.id.to_string(), "updated", 1, 5_000)
        .await
        .expect("update message content");
    let updated = storage
        .get_message(&message.id.to_string())
        .await
        .expect("reload updated message")
        .expect("updated message exists");
    assert_eq!(updated.content, "updated");
    assert_eq!(updated.edit_version, 1);
    assert_eq!(updated.edited_at, Some(5_000));
    assert!(!updated.is_deleted);
    assert_eq!(updated.deleted_at, None);

    storage
        .mark_message_deleted(&message.id.to_string(), 6_000)
        .await
        .expect("mark message deleted");
    let deleted = storage
        .get_message(&message.id.to_string())
        .await
        .expect("reload deleted message")
        .expect("deleted message exists");
    assert_eq!(deleted.content, "updated");
    assert!(deleted.is_deleted);
    assert_eq!(deleted.deleted_at, Some(6_000));

    let payload_path = write_transfer_file(temp_dir.path(), "thumb.png", 32);
    let transfer = transfer_record(&payload_path, TransferStatus::Pending);
    storage
        .save_transfer(&transfer)
        .await
        .expect("save transfer");
    assert_eq!(
        storage
            .get_thumbnail_path(&transfer.id.to_string())
            .await
            .expect("read empty thumbnail"),
        None
    );

    storage
        .save_thumbnail_path(&transfer.id.to_string(), "/tmp/thumb.webp")
        .await
        .expect("save thumbnail path");
    assert_eq!(
        storage
            .get_thumbnail_path(&transfer.id.to_string())
            .await
            .expect("read thumbnail path"),
        Some("/tmp/thumb.webp".to_string())
    );

    let stored_transfer = storage
        .get_transfers(10, 0)
        .await
        .expect("load transfers")
        .into_iter()
        .find(|candidate| candidate.id == transfer.id)
        .expect("transfer exists");
    assert_eq!(
        stored_transfer.thumbnail_path.as_deref(),
        Some("/tmp/thumb.webp")
    );

    let missing_id = Uuid::new_v4().to_string();
    let error = storage
        .update_message_content(&missing_id, "missing", 1, 7_000)
        .await
        .expect_err("missing message should error");
    assert!(
        matches!(error, CoreError::Persistence(message) if message.contains("message not found"))
    );
}

#[tokio::test]
async fn message_upsert_preserves_newer_edit_delete_and_reply_snapshot() {
    let (_temp_dir, db_path) = temp_db_path("message-upsert-snapshot");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let chat_id = chat_id();
    let reply_to_id = Uuid::new_v4().to_string();
    let newer_reply_to_id = Uuid::new_v4().to_string();

    let mut newer = message(
        chat_id.clone(),
        2_000,
        MessageStatus::Delivered,
        "newer body",
    );
    newer.edit_version = 3;
    newer.edited_at = Some(5_000);
    newer.is_deleted = true;
    newer.deleted_at = Some(6_000);
    newer.reply_to_id = Some(newer_reply_to_id.clone());
    newer.reply_to_preview = Some("newer preview".to_string());

    let mut older = newer.clone();
    older.content = "stale body".to_string();
    older.edit_version = 1;
    older.edited_at = Some(4_000);
    older.is_deleted = false;
    older.deleted_at = None;
    older.reply_to_id = Some(reply_to_id);
    older.reply_to_preview = Some("stale preview".to_string());

    storage
        .save_message(&newer)
        .await
        .expect("save newer snapshot");
    storage
        .save_message(&older)
        .await
        .expect("save stale snapshot");

    let loaded = storage
        .get_message(&newer.id.to_string())
        .await
        .expect("load message")
        .expect("message exists");

    assert_eq!(loaded.content, "newer body");
    assert_eq!(loaded.edit_version, 3);
    assert_eq!(loaded.edited_at, Some(5_000));
    assert!(loaded.is_deleted);
    assert_eq!(loaded.deleted_at, Some(6_000));
    assert_eq!(loaded.reply_to_id, Some(newer_reply_to_id));
    assert_eq!(loaded.reply_to_preview.as_deref(), Some("newer preview"));
}

#[tokio::test]
async fn message_upsert_preserves_latest_delete_timestamp() {
    let (_temp_dir, db_path) = temp_db_path("message-upsert-delete-timestamp");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let chat_id = chat_id();

    let mut latest_delete = message(chat_id, 2_000, MessageStatus::Delivered, "body");
    latest_delete.is_deleted = true;
    latest_delete.deleted_at = Some(6_000);

    let mut stale_delete = latest_delete.clone();
    stale_delete.deleted_at = Some(5_000);

    storage
        .save_message(&latest_delete)
        .await
        .expect("save latest delete snapshot");
    storage
        .save_message(&stale_delete)
        .await
        .expect("save stale delete snapshot");

    let loaded = storage
        .get_message(&latest_delete.id.to_string())
        .await
        .expect("load message")
        .expect("message exists");

    assert!(loaded.is_deleted);
    assert_eq!(loaded.deleted_at, Some(6_000));
}

#[tokio::test]
async fn peer_upsert_updates_existing_row() {
    let (_temp_dir, db_path) = temp_db_path("peer-upsert");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let device_id = device_id();

    storage
        .save_peer(&PeerInfo {
            device_id: device_id.clone(),
            user_id: Some(user_id()),
            display_name: "Alpha".to_string(),
            ws_port: Some(9000),
            addresses: vec!["127.0.0.1".to_string()],
        })
        .await
        .expect("save peer");
    storage
        .save_peer(&PeerInfo {
            device_id: device_id.clone(),
            user_id: None,
            display_name: "Bravo".to_string(),
            ws_port: Some(9001),
            addresses: vec![],
        })
        .await
        .expect("update peer");

    let peers = storage.get_peers().await.expect("query peers");

    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].device_id, device_id);
    assert_eq!(peers[0].display_name, "Bravo");
    assert_eq!(peers[0].ws_port, Some(9001));
}

#[tokio::test]
async fn chat_crud_saves_and_loads_direct_chat() {
    let (_temp_dir, db_path) = temp_db_path("chat-direct");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let chat = ChatRecord {
        id: chat_id(),
        chat_type: ChatType::Direct,
        name: Some("Alice and Bob".to_string()),
        created_at_ms: 12_345,
    };

    storage.save_chat(&chat).await.expect("save chat");

    let loaded = storage
        .get_chat(&chat.id)
        .await
        .expect("query chat")
        .expect("chat exists");

    assert_eq!(loaded, chat);
}

#[tokio::test]
async fn chat_crud_stores_and_reads_group_members() {
    let (_temp_dir, db_path) = temp_db_path("chat-members");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let chat = ChatRecord {
        id: chat_id(),
        chat_type: ChatType::Group,
        name: Some("Study Group".to_string()),
        created_at_ms: 55_555,
    };
    let members = vec![user_id(), user_id(), user_id()];

    storage.save_chat(&chat).await.expect("save chat");
    storage
        .replace_chat_members(&chat.id, &members, 55_556)
        .await
        .expect("save chat members");

    let loaded = storage
        .get_chat_members(&chat.id)
        .await
        .expect("query chat members");

    assert_eq!(loaded, members);
}

#[tokio::test]
async fn transfer_crud_persists_and_reads_back() {
    let (temp_dir, db_path) = temp_db_path("transfer-save");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let payload_path = write_transfer_file(temp_dir.path(), "report.pdf", 1_024);
    let transfer = transfer_record(&payload_path, TransferStatus::Pending);

    storage
        .save_transfer(&transfer)
        .await
        .expect("save transfer");

    let transfers = storage.get_transfers(10, 0).await.expect("query transfers");

    assert_eq!(transfers, vec![transfer.clone()]);

    let conn = Connection::open(&db_path).expect("open sqlite db");
    let row = conn
        .query_row(
            "SELECT filename, size, direction, completed_at FROM transfers WHERE id = ?1",
            params![transfer.id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            },
        )
        .expect("query transfer row");

    assert_eq!(row.0, transfer.file_name);
    assert_eq!(row.1, 1_024);
    assert_eq!(row.2, "send");
    assert_eq!(row.3, None);
}

#[tokio::test]
async fn transfer_status_update_sets_completed_at() {
    let (temp_dir, db_path) = temp_db_path("transfer-status");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let payload_path = write_transfer_file(temp_dir.path(), "archive.zip", 2_048);
    let transfer = transfer_record(&payload_path, TransferStatus::Pending);

    storage
        .save_transfer(&transfer)
        .await
        .expect("save transfer");
    storage
        .update_transfer_status(&transfer.id, TransferStatus::Active)
        .await
        .expect("mark in progress");
    storage
        .update_transfer_status(&transfer.id, TransferStatus::Completed)
        .await
        .expect("mark completed");

    let transfers = storage.get_transfers(10, 0).await.expect("query transfers");
    assert_eq!(transfers[0].status, TransferStatus::Completed);

    let conn = Connection::open(&db_path).expect("open sqlite db");
    let row = conn
        .query_row(
            "SELECT status, completed_at FROM transfers WHERE id = ?1",
            params![transfer.id.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?)),
        )
        .expect("query transfer row");

    assert_eq!(row.0, "completed");
    assert!(row.1.is_some(), "completed_at should be set");
}

#[tokio::test]
async fn concurrent_reads_complete_with_pooling_and_wal() {
    let (_temp_dir, db_path) = temp_db_path("concurrent");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let chat_id = chat_id();

    for index in 0..5 {
        let message = message(
            chat_id.clone(),
            1_000 + i64::from(index),
            MessageStatus::Sent,
            &format!("message-{index}"),
        );
        storage.save_message(&message).await.expect("save message");
    }

    let mut join_set = JoinSet::new();
    for _ in 0..10 {
        let storage = storage.clone();
        let chat_id = chat_id.clone();
        join_set.spawn(async move { storage.get_messages(&chat_id, 10, 0).await });
    }

    tokio::time::timeout(Duration::from_secs(1), async {
        while let Some(result) = join_set.join_next().await {
            let messages = result.expect("task joins").expect("query succeeds");
            assert_eq!(messages.len(), 5);
        }
    })
    .await
    .expect("concurrent reads finish within timeout");

    let conn = Connection::open(&db_path).expect("open sqlite db");
    let journal_mode: String = conn
        .query_row("PRAGMA journal_mode;", [], |row| row.get(0))
        .expect("read journal mode");
    assert_eq!(journal_mode, "wal");
}

fn temp_db_path(name: &str) -> (TempDir, PathBuf) {
    let temp_dir = TempDir::new().expect("create temp dir");
    let db_path = temp_dir.path().join(format!("{name}.sqlite3"));
    (temp_dir, db_path)
}

fn chat_id() -> ChatId {
    ChatId(Uuid::new_v4())
}

fn user_id() -> UserId {
    UserId(Uuid::new_v4())
}

fn device_id() -> DeviceId {
    DeviceId(Uuid::new_v4())
}

fn message(chat_id: ChatId, timestamp_ms: i64, status: MessageStatus, content: &str) -> Message {
    Message {
        id: Uuid::new_v4(),
        chat_id,
        sender_id: user_id(),
        content: content.to_string(),
        timestamp_ms,
        status,
        edit_version: 0,
        edited_at: None,
        is_deleted: false,
        deleted_at: None,
        reply_to_id: None,
        reply_to_preview: None,
    }
}

fn transfer_record(path: &Path, status: TransferStatus) -> TransferRecord {
    TransferRecord {
        id: Uuid::new_v4(),
        peer_id: device_id(),
        chat_id: Some(chat_id()),
        file_name: path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("valid utf-8 filename")
            .to_string(),
        local_path: path.to_path_buf(),
        status,
        thumbnail_path: None,
        folder_id: None,
        folder_relative_path: None,
    }
}

fn write_transfer_file(dir: &Path, name: &str, size: usize) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, vec![0x5A; size]).expect("write transfer payload");
    path
}

fn applied_versions(conn: &Connection) -> Vec<i64> {
    let mut statement = conn
        .prepare("SELECT version FROM _migrations ORDER BY version")
        .expect("prepare migration query");
    statement
        .query_map([], |row| row.get::<_, i64>(0))
        .expect("query migrations")
        .map(|row| row.expect("read migration row"))
        .collect()
}

fn user_version(conn: &Connection) -> i64 {
    conn.query_row("PRAGMA user_version;", [], |row| row.get(0))
        .expect("read user_version")
}

fn assert_columns_exist(conn: &Connection, table: &str, expected_columns: &[&str]) {
    let mut statement = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .expect("prepare table info query");
    let column_names = statement
        .query_map([], |row| row.get::<_, String>(1))
        .expect("query table info")
        .map(|row| row.expect("read column name"))
        .collect::<Vec<_>>();

    for expected in expected_columns {
        assert!(
            column_names.iter().any(|column| column == expected),
            "expected column {expected} in table {table}, got {column_names:?}"
        );
    }
}

fn assert_tables_exist(conn: &Connection, tables: &[&str]) {
    for table in tables {
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![table],
                |row| row.get(0),
            )
            .expect("query sqlite_master");
        assert_eq!(exists, 1, "expected table {table} to exist");
    }
}
