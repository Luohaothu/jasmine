use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jasmine_core::{
    ChatId, DeviceId, Message, MessageStatus, PeerInfo, StorageEngine, TransferRecord,
    TransferStatus, UserId,
};
use jasmine_storage::{ChatRecord, ChatType, SqliteStorage};
use rusqlite::{params, Connection};
use tempfile::TempDir;
use tokio::task::JoinSet;
use uuid::Uuid;

#[test]
fn migration_creates_database_and_records_v1() {
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
    assert_eq!(versions, vec![1]);
}

#[test]
fn migration_upgrades_v0_database_to_v1() {
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
    assert_eq!(applied_versions(&conn), vec![1]);
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
