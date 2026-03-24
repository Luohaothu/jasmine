use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jasmine_core::{
    ChatId, CoreError, DeviceId, Message, MessageStatus, OgMetadata, PeerInfo, StorageEngine,
    TransferRecord, TransferStatus, UserId,
};
use jasmine_storage::{CachedOgMetadata, ChatRecord, ChatType, SqliteStorage};
use rusqlite::{params, Connection, OptionalExtension};
use tempfile::TempDir;
use tokio::task::JoinSet;
use uuid::Uuid;

const V1_MIGRATION_SQL: &str = include_str!("../migrations/V1__initial.sql");
const V2_MIGRATION_SQL: &str = include_str!("../migrations/V2__edit_delete_richtext.sql");
const V3_MIGRATION_SQL: &str = include_str!("../migrations/V3__crypto_and_resume.sql");
const V4_MIGRATION_SQL: &str = include_str!("../migrations/V4__sender_key_epochs.sql");
const V5_MIGRATION_SQL: &str = include_str!("../migrations/V5__folder_file_status.sql");

#[test]
fn migration_creates_database_and_records_v6_schema() {
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
            "transfer_context",
            "peer_keys",
            "sender_keys",
            "folder_file_status",
            "og_cache",
        ],
    );

    let versions = applied_versions(&conn);
    assert_eq!(versions, vec![1, 2, 3, 4, 5, 6]);
    assert_eq!(user_version(&conn), 6);
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
            "vector_clock",
        ],
    );
    assert_columns_exist(
        &conn,
        "og_cache",
        &[
            "url",
            "title",
            "description",
            "image_url",
            "site_name",
            "fetched_at",
            "ttl_seconds",
        ],
    );
    assert_index_exists(&conn, "idx_messages_reply_to_id", "messages");
    assert_columns_exist(
        &conn,
        "transfers",
        &[
            "thumbnail_path",
            "folder_id",
            "folder_relative_path",
            "bytes_transferred",
            "resume_token",
        ],
    );
}

#[test]
fn migration_upgrades_v0_database_to_v6() {
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
            "transfer_context",
            "peer_keys",
            "sender_keys",
            "folder_file_status",
            "og_cache",
        ],
    );
    assert_eq!(applied_versions(&conn), vec![1, 2, 3, 4, 5, 6]);
    assert_eq!(user_version(&conn), 6);
}

#[test]
fn migration_upgrades_existing_v2_database_to_v6_and_preserves_data() {
    let (_temp_dir, db_path) = temp_db_path("upgrade-v2-to-v3");
    let message_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let sender_id = Uuid::new_v4();
    let transfer_id = Uuid::new_v4();
    let peer_id = Uuid::new_v4();

    let conn = Connection::open(&db_path).expect("create v1 db");
    conn.execute_batch(V1_MIGRATION_SQL)
        .expect("apply v1 schema manually");
    conn.execute_batch(V2_MIGRATION_SQL)
        .expect("apply v2 schema manually");
    conn.execute(
        "INSERT INTO messages (
            id, chat_id, sender_id, content, timestamp, status, created_at,
            edit_version, edited_at, is_deleted, deleted_at, reply_to_id, reply_to_preview
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            message_id.to_string(),
            chat_id.to_string(),
            sender_id.to_string(),
            "legacy message",
            1_234_i64,
            "sent",
            1_234_i64,
            2_i64,
            1_500_i64,
            0_i64,
            Option::<i64>::None,
            Some(Uuid::new_v4().to_string()),
            Some("legacy preview".to_string())
        ],
    )
    .expect("insert legacy message");
    conn.execute(
        "INSERT INTO transfers (
            id, peer_id, filename, size, direction, status, sha256, created_at, completed_at,
            thumbnail_path, folder_id, folder_relative_path
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            transfer_id.to_string(),
            peer_id.to_string(),
            "legacy.bin",
            512_i64,
            "send",
            "pending",
            Option::<String>::None,
            2_000_i64,
            Option::<i64>::None,
            Some("/tmp/thumb.webp".to_string()),
            Some("folder-1".to_string()),
            Some("docs/legacy.bin".to_string())
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
    conn.execute_batch("PRAGMA user_version = 2;")
        .expect("mark db as v2");
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
    assert_eq!(loaded.edit_version, 2);
    assert_eq!(loaded.edited_at, Some(1_500));
    assert!(!loaded.is_deleted);
    assert_eq!(loaded.deleted_at, None);
    assert!(loaded.reply_to_id.is_some());
    assert_eq!(loaded.reply_to_preview.as_deref(), Some("legacy preview"));

    let conn = Connection::open(&db_path).expect("open migrated db");
    assert_eq!(applied_versions(&conn), vec![1, 2, 3, 4, 5, 6]);
    assert_eq!(user_version(&conn), 6);
    assert_columns_exist(&conn, "messages", &["vector_clock"]);

    let transfer_row = conn
        .query_row(
            "SELECT filename, thumbnail_path, folder_id, folder_relative_path,
                    bytes_transferred, resume_token
             FROM transfers WHERE id = ?1",
            params![transfer_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .expect("query migrated transfer row");
    assert_eq!(transfer_row.0, "legacy.bin");
    assert_eq!(transfer_row.1.as_deref(), Some("/tmp/thumb.webp"));
    assert_eq!(transfer_row.2.as_deref(), Some("folder-1"));
    assert_eq!(transfer_row.3.as_deref(), Some("docs/legacy.bin"));
    assert_eq!(transfer_row.4, 0);
    assert_eq!(transfer_row.5, None);
}

#[test]
fn migration_rejects_corrupted_database() {
    let (_temp_dir, db_path) = temp_db_path("corrupted");
    fs::write(&db_path, b"not-a-sqlite-database").expect("write corrupt bytes");

    let result = SqliteStorage::open(&db_path);

    assert!(result.is_err(), "corrupted db should be rejected");
}

#[tokio::test]
async fn migration_adds_reply_to_id_index_in_v6_upgrade() {
    let (_temp_dir, db_path) = temp_db_path("reply-to-index-v6");

    let conn = Connection::open(&db_path).expect("create v5 db");
    conn.execute_batch(V1_MIGRATION_SQL)
        .expect("apply v1 schema manually");
    conn.execute_batch(V2_MIGRATION_SQL)
        .expect("apply v2 schema manually");
    conn.execute_batch(V3_MIGRATION_SQL)
        .expect("apply v3 schema manually");
    conn.execute_batch(V4_MIGRATION_SQL)
        .expect("apply v4 schema manually");
    conn.execute_batch(V5_MIGRATION_SQL)
        .expect("apply v5 schema manually");
    conn.execute_batch("PRAGMA user_version = 5;")
        .expect("set v5 user version");
    drop(conn);

    let _storage = SqliteStorage::open(&db_path).expect("storage opens and upgrades to v6");

    let conn = Connection::open(&db_path).expect("open migrated db");
    assert_eq!(applied_versions(&conn), vec![1, 2, 3, 4, 5, 6]);
    assert_index_exists(&conn, "idx_messages_reply_to_id", "messages");
}

#[tokio::test]
async fn og_cache_and_vector_clock_crud_and_nullability() {
    let (_temp_dir, db_path) = temp_db_path("og-cache-v6");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let chat_id = chat_id();
    let message = message(chat_id, 1_000, MessageStatus::Sent, "hello");

    storage
        .save_message(&message)
        .await
        .expect("save message for cache CRUD");

    let message_id = message.id.to_string();
    let conn = Connection::open(&db_path).expect("open db");
    let cache_url = "https://example.org/og-cache-test";

    conn.execute(
        "UPDATE messages SET vector_clock = ?1 WHERE id = ?2",
        params!["1-2-3", message_id],
    )
    .expect("set vector clock");

    conn.execute(
        "INSERT INTO og_cache (url, title, description, image_url, site_name, fetched_at, ttl_seconds)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            cache_url,
            "title one",
            "description one",
            "https://example.org/image.png",
            "site",
            1_700_000_000_000_i64,
            86_400_i64,
        ],
    )
    .expect("insert cache row");

    let vector_clock: Option<String> = conn
        .query_row(
            "SELECT vector_clock FROM messages WHERE id = ?1",
            params![message.id.to_string()],
            |row| row.get(0),
        )
        .expect("read vector clock");
    let (title, description, image_url, site_name, fetched_at, ttl_seconds): (
        String,
        String,
        String,
        String,
        i64,
        i64,
    ) = conn
        .query_row(
            "SELECT title, description, image_url, site_name, fetched_at, ttl_seconds FROM og_cache WHERE url = ?1",
            params![cache_url],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .expect("read cache row");
    assert_eq!(vector_clock.as_deref(), Some("1-2-3"));
    assert_eq!(title, "title one");
    assert_eq!(description, "description one");
    assert_eq!(image_url, "https://example.org/image.png");
    assert_eq!(site_name, "site");
    assert_eq!(fetched_at, 1_700_000_000_000);
    assert_eq!(ttl_seconds, 86_400);

    conn.execute(
        "UPDATE messages SET vector_clock = ?1 WHERE id = ?2",
        params!["4-5-6", message_id],
    )
    .expect("update vector clock");

    conn.execute(
        "UPDATE og_cache
         SET title = ?2, description = ?3, image_url = ?4, site_name = ?5, fetched_at = ?6, ttl_seconds = ?7
         WHERE url = ?1",
        params![
            cache_url,
            "title two",
            "description two",
            "https://example.org/image2.png",
            "site 2",
            1_700_000_000_600_i64,
            43_200_i64,
        ],
    )
    .expect("update cache fields");

    let vector_clock: Option<String> = conn
        .query_row(
            "SELECT vector_clock FROM messages WHERE id = ?1",
            params![message_id],
            |row| row.get(0),
        )
        .expect("read updated vector clock");
    let (title, description, image_url, site_name, fetched_at, ttl_seconds): (
        String,
        String,
        String,
        String,
        i64,
        i64,
    ) = conn
        .query_row(
            "SELECT title, description, image_url, site_name, fetched_at, ttl_seconds FROM og_cache WHERE url = ?1",
            params![cache_url],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .expect("read updated cache row");
    assert_eq!(vector_clock.as_deref(), Some("4-5-6"));
    assert_eq!(title, "title two");
    assert_eq!(description, "description two");
    assert_eq!(image_url, "https://example.org/image2.png");
    assert_eq!(site_name, "site 2");
    assert_eq!(fetched_at, 1_700_000_000_600);
    assert_eq!(ttl_seconds, 43_200);

    conn.execute(
        "UPDATE messages SET vector_clock = ?1 WHERE id = ?2",
        params![Option::<String>::None, message_id],
    )
    .expect("nullify vector clock");

    conn.execute("DELETE FROM og_cache WHERE url = ?1", params![cache_url])
        .expect("delete cache row");

    let vector_clock: Option<String> = conn
        .query_row(
            "SELECT vector_clock FROM messages WHERE id = ?1",
            params![message_id],
            |row| row.get(0),
        )
        .expect("read nullified vector clock");
    assert!(vector_clock.is_none());

    let deleted: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM og_cache WHERE url = ?1",
            params![cache_url],
            |row| row.get(0),
        )
        .optional()
        .expect("check deleted cache row");
    assert!(deleted.is_none());
}

#[tokio::test]
async fn og_cache_helpers_round_trip_metadata_with_ttl() {
    let (_temp_dir, db_path) = temp_db_path("og-cache-helpers-roundtrip");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let metadata = OgMetadata {
        url: "https://example.org/preview".to_string(),
        title: Some("Preview title".to_string()),
        description: Some("Preview description".to_string()),
        image_url: Some("https://example.org/preview.png".to_string()),
        site_name: Some("Example".to_string()),
    };

    storage
        .save_og_metadata(&metadata, 86_400)
        .await
        .expect("save og metadata through helper");

    let cached = storage
        .get_cached_og_metadata(&metadata.url)
        .await
        .expect("load cached og metadata");
    assert_eq!(
        cached,
        Some(CachedOgMetadata::new(
            metadata.clone(),
            cached.as_ref().expect("cached row exists").fetched_at_ms,
            86_400,
        ))
    );

    let conn = Connection::open(&db_path).expect("open sqlite db");
    let (stored_url, fetched_at, ttl_seconds): (String, i64, i64) = conn
        .query_row(
            "SELECT url, fetched_at, ttl_seconds FROM og_cache WHERE url = ?1",
            params![metadata.url],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("query cached og row");
    assert_eq!(stored_url, "https://example.org/preview");
    assert!(fetched_at > 0);
    assert_eq!(ttl_seconds, 86_400);
}

#[tokio::test]
async fn og_cache_helpers_return_expired_rows_without_filtering_ttl() {
    let (_temp_dir, db_path) = temp_db_path("og-cache-helpers-expired");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let conn = Connection::open(&db_path).expect("open sqlite db");
    let expired_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("current time")
        .as_millis() as i64
        - 86_401_000;

    conn.execute(
        "INSERT INTO og_cache (url, title, description, image_url, site_name, fetched_at, ttl_seconds)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            "https://example.org/expired",
            "Expired title",
            Option::<String>::None,
            Option::<String>::None,
            Option::<String>::None,
            expired_at,
            86_400_i64,
        ],
    )
    .expect("insert expired og cache row");

    let cached = storage
        .get_cached_og_metadata("https://example.org/expired")
        .await
        .expect("query expired og cache row");

    assert_eq!(
        cached,
        Some(CachedOgMetadata::new(
            OgMetadata {
                url: "https://example.org/expired".to_string(),
                title: Some("Expired title".to_string()),
                description: None,
                image_url: None,
                site_name: None,
            },
            expired_at,
            86_400,
        ))
    );
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
async fn message_vector_clock_roundtrip_persists_json_and_preserves_legacy_reads() {
    let (_temp_dir, db_path) = temp_db_path("message-vector-clock-roundtrip");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let chat_id = chat_id();
    let message = message(chat_id, 1_000, MessageStatus::Sent, "clocked");
    let clock = vector_clock(&[("alice", 2), ("bob", 1)]);

    storage
        .save_message_with_vector_clock(&message, Some(&clock))
        .await
        .expect("save message with vector clock");

    let loaded = storage
        .get_message(&message.id.to_string())
        .await
        .expect("load message")
        .expect("message exists");
    assert_eq!(loaded, message);

    let stored_clock = storage
        .get_message_vector_clock(&message.id.to_string())
        .await
        .expect("load vector clock");
    assert_eq!(stored_clock, Some(clock.clone()));

    let conn = Connection::open(&db_path).expect("open sqlite db");
    let stored_json: Option<String> = conn
        .query_row(
            "SELECT vector_clock FROM messages WHERE id = ?1",
            params![message.id.to_string()],
            |row| row.get(0),
        )
        .expect("query vector clock json");
    assert_eq!(stored_json.as_deref(), Some(r#"{"alice":2,"bob":1}"#));

    storage
        .save_message(&message)
        .await
        .expect("legacy save should preserve clock");

    let preserved_clock = storage
        .get_message_vector_clock(&message.id.to_string())
        .await
        .expect("reload vector clock");
    assert_eq!(preserved_clock, Some(clock));
}

#[tokio::test]
async fn message_vector_clock_ordering_prefers_causal_order_over_timestamp() {
    let (_temp_dir, db_path) = temp_db_path("message-vector-clock-ordering");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let chat_id = chat_id();

    let first = message(
        chat_id.clone(),
        8_000,
        MessageStatus::Sent,
        "first by timestamp",
    );
    let second = message(
        chat_id.clone(),
        1_000,
        MessageStatus::Sent,
        "second by clock",
    );
    let first_clock = vector_clock(&[("alice", 1)]);
    let second_clock = vector_clock(&[("alice", 2)]);

    storage
        .save_message_with_vector_clock(&first, Some(&first_clock))
        .await
        .expect("save first clocked message");
    storage
        .save_message_with_vector_clock(&second, Some(&second_clock))
        .await
        .expect("save second clocked message");

    let messages = storage
        .get_messages(&chat_id, 10, 0)
        .await
        .expect("query messages");

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].id, second.id);
    assert_eq!(messages[1].id, first.id);
}

#[tokio::test]
async fn message_vector_clock_ordering_uses_timestamp_fallback_for_mixed_rows() {
    let (_temp_dir, db_path) = temp_db_path("message-vector-clock-mixed-ordering");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let chat_id = chat_id();

    let legacy = message(chat_id.clone(), 3_000, MessageStatus::Sent, "legacy");
    let clocked_first = message(chat_id.clone(), 2_000, MessageStatus::Sent, "clocked first");
    let clocked_second = message(
        chat_id.clone(),
        1_000,
        MessageStatus::Sent,
        "clocked second",
    );
    let first_clock = vector_clock(&[("alice", 1)]);
    let second_clock = vector_clock(&[("alice", 2)]);

    storage
        .save_message(&legacy)
        .await
        .expect("save legacy message");
    storage
        .save_message_with_vector_clock(&clocked_first, Some(&first_clock))
        .await
        .expect("save first clocked message");
    storage
        .save_message_with_vector_clock(&clocked_second, Some(&second_clock))
        .await
        .expect("save second clocked message");

    let messages = storage
        .get_messages(&chat_id, 10, 0)
        .await
        .expect("query mixed messages");

    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].id, legacy.id);
    assert_eq!(messages[1].id, clocked_second.id);
    assert_eq!(messages[2].id, clocked_first.id);
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
async fn reply_count_returns_zero_for_messages_without_replies() {
    let (_temp_dir, db_path) = temp_db_path("reply-count-zero");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let chat_id = chat_id();
    let parent = message(chat_id, 1_000, MessageStatus::Sent, "parent");

    storage.save_message(&parent).await.expect("save parent");

    let reply_count = storage
        .get_reply_count(&parent.id.to_string())
        .await
        .expect("query reply count");

    assert_eq!(reply_count, 0);
}

#[tokio::test]
async fn reply_count_batch_returns_counts_for_multiple_parent_messages() {
    let (_temp_dir, db_path) = temp_db_path("reply-count-batch");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let chat_id = chat_id();
    let parent_one = message(chat_id.clone(), 1_000, MessageStatus::Sent, "parent one");
    let parent_two = message(chat_id.clone(), 2_000, MessageStatus::Sent, "parent two");
    let missing_parent = Uuid::new_v4().to_string();

    storage
        .save_message(&parent_one)
        .await
        .expect("save first parent");
    storage
        .save_message(&parent_two)
        .await
        .expect("save second parent");

    for index in 0..3 {
        let mut reply = message(
            chat_id.clone(),
            3_000 + i64::from(index),
            MessageStatus::Delivered,
            &format!("reply-one-{index}"),
        );
        reply.reply_to_id = Some(parent_one.id.to_string());
        storage
            .save_message(&reply)
            .await
            .expect("save first parent reply");
    }

    let mut reply = message(chat_id, 4_000, MessageStatus::Delivered, "reply-two");
    reply.reply_to_id = Some(parent_two.id.to_string());
    storage
        .save_message(&reply)
        .await
        .expect("save second parent reply");

    let single_count = storage
        .get_reply_count(&parent_one.id.to_string())
        .await
        .expect("query single reply count");
    assert_eq!(single_count, 3);

    let reply_counts = storage
        .get_reply_counts(&[
            parent_one.id.to_string(),
            parent_two.id.to_string(),
            missing_parent.clone(),
        ])
        .await
        .expect("query reply counts");

    assert_eq!(reply_counts.get(&parent_one.id.to_string()), Some(&3));
    assert_eq!(reply_counts.get(&parent_two.id.to_string()), Some(&1));
    assert_eq!(reply_counts.get(&missing_parent), Some(&0));
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
async fn peer_key_crud_saves_gets_and_verifies_rows() {
    let (_temp_dir, db_path) = temp_db_path("peer-key-crud");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let device_id = Uuid::new_v4().to_string();

    storage
        .save_peer_key(&device_id, &[1, 2, 3, 4], "AA11 BB22")
        .await
        .expect("save peer key");

    let saved = storage
        .get_peer_key(&device_id)
        .await
        .expect("get peer key")
        .expect("peer key exists");
    assert_eq!(saved.device_id, device_id);
    assert_eq!(saved.public_key, vec![1, 2, 3, 4]);
    assert_eq!(saved.fingerprint, "AA11 BB22");
    assert!(!saved.verified);
    assert!(saved.first_seen > 0);
    assert_eq!(saved.first_seen, saved.last_seen);

    storage
        .set_peer_verified(&saved.device_id, true)
        .await
        .expect("verify peer key");

    let verified = storage
        .get_peer_key(&saved.device_id)
        .await
        .expect("reload peer key")
        .expect("verified peer key exists");
    assert!(verified.verified);
    assert_eq!(verified.public_key, vec![1, 2, 3, 4]);
    assert_eq!(verified.fingerprint, "AA11 BB22");
}

#[tokio::test]
async fn peer_key_get_missing_returns_none() {
    let (_temp_dir, db_path) = temp_db_path("peer-key-missing");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");

    let missing = storage
        .get_peer_key("missing-device")
        .await
        .expect("lookup missing peer key");

    assert!(missing.is_none());
}

#[tokio::test]
async fn peer_key_list_returns_all_keys_in_last_seen_order() {
    let (_temp_dir, db_path) = temp_db_path("peer-key-list");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let first = Uuid::new_v4().to_string();
    let second = Uuid::new_v4().to_string();
    let third = Uuid::new_v4().to_string();

    storage
        .save_peer_key(&first, &[1, 2, 3], "FP-1")
        .await
        .expect("save first peer key");
    tokio::time::sleep(Duration::from_millis(5)).await;
    storage
        .save_peer_key(&second, &[4, 5, 6], "FP-2")
        .await
        .expect("save second peer key");
    tokio::time::sleep(Duration::from_millis(5)).await;
    storage
        .save_peer_key(&third, &[7, 8, 9], "FP-3")
        .await
        .expect("save third peer key");

    tokio::time::sleep(Duration::from_millis(5)).await;
    storage
        .update_peer_key_last_seen(&second)
        .await
        .expect("refresh second peer key");
    tokio::time::sleep(Duration::from_millis(5)).await;
    storage
        .update_peer_key_last_seen(&third)
        .await
        .expect("refresh third peer key");

    let keys = storage
        .get_all_peer_keys()
        .await
        .expect("list all peer keys");

    assert_eq!(keys.len(), 3);
    assert_eq!(keys[0].device_id, third);
    assert_eq!(keys[1].device_id, second);
    assert_eq!(keys[2].device_id, first);
}

#[tokio::test]
async fn peer_key_last_seen_updates_without_mutating_first_seen() {
    let (_temp_dir, db_path) = temp_db_path("peer-key-last-seen");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let device_id = Uuid::new_v4().to_string();

    storage
        .save_peer_key(&device_id, &[1, 2, 3, 4], "AA11 BB22")
        .await
        .expect("save peer key");
    let original = storage
        .get_peer_key(&device_id)
        .await
        .expect("load original peer key")
        .expect("peer key exists");

    tokio::time::sleep(Duration::from_millis(5)).await;
    storage
        .update_peer_key_last_seen(&device_id)
        .await
        .expect("update peer key last seen");

    let updated = storage
        .get_peer_key(&device_id)
        .await
        .expect("load updated peer key")
        .expect("updated peer key exists");

    assert_eq!(updated.device_id, device_id);
    assert_eq!(updated.public_key, vec![1, 2, 3, 4]);
    assert_eq!(updated.fingerprint, "AA11 BB22");
    assert_eq!(updated.verified, original.verified);
    assert_eq!(updated.first_seen, original.first_seen);
    assert!(updated.last_seen >= original.last_seen);
}

#[tokio::test]
async fn peer_key_save_updates_existing_row_and_last_seen() {
    let (_temp_dir, db_path) = temp_db_path("peer-key-update");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let device_id = Uuid::new_v4().to_string();

    storage
        .save_peer_key(&device_id, &[1, 2, 3], "OLD FP")
        .await
        .expect("save original peer key");
    let original = storage
        .get_peer_key(&device_id)
        .await
        .expect("load original peer key")
        .expect("original peer key exists");

    tokio::time::sleep(Duration::from_millis(2)).await;

    storage
        .save_peer_key(&device_id, &[9, 8, 7], "NEW FP")
        .await
        .expect("update peer key");
    let updated = storage
        .get_peer_key(&device_id)
        .await
        .expect("load updated peer key")
        .expect("updated peer key exists");

    assert_eq!(updated.device_id, device_id);
    assert_eq!(updated.public_key, vec![9, 8, 7]);
    assert_eq!(updated.fingerprint, "NEW FP");
    assert_eq!(updated.first_seen, original.first_seen);
    assert!(updated.last_seen >= original.last_seen);
}

#[tokio::test]
async fn sender_key_save_latest_get_by_epoch_and_delete_group() {
    let (_temp_dir, db_path) = temp_db_path("sender-key-epochs");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let original_key_id = Uuid::new_v4();
    let rotated_key_id = Uuid::new_v4();
    let original_created_at = 1_700_000_000_000_u64;
    let rotated_created_at = original_created_at + 500;

    storage
        .save_sender_key(
            "group-1",
            "device-a",
            &original_key_id,
            &[4, 5, 6],
            0,
            original_created_at,
        )
        .await
        .expect("save sender key");

    let original = storage
        .get_sender_key("group-1", "device-a")
        .await
        .expect("get sender key")
        .expect("sender key exists");
    assert_eq!(original.group_id, "group-1");
    assert_eq!(original.sender_device_id, "device-a");
    assert_eq!(original.key_id, original_key_id);
    assert_eq!(original.key_data, vec![4, 5, 6]);
    assert_eq!(original.epoch, 0);
    assert_eq!(original.created_at, original_created_at);

    storage
        .save_sender_key(
            "group-1",
            "device-a",
            &rotated_key_id,
            &[7, 8, 9, 10],
            3,
            rotated_created_at,
        )
        .await
        .expect("rotate sender key");

    let rotated = storage
        .get_sender_key("group-1", "device-a")
        .await
        .expect("reload sender key")
        .expect("rotated sender key exists");
    assert_eq!(rotated.group_id, "group-1");
    assert_eq!(rotated.sender_device_id, "device-a");
    assert_eq!(rotated.key_id, rotated_key_id);
    assert_eq!(rotated.key_data, vec![7, 8, 9, 10]);
    assert_eq!(rotated.epoch, 3);
    assert_eq!(rotated.created_at, rotated_created_at);

    let epoch_zero = storage
        .get_sender_key_by_epoch("group-1", "device-a", 0)
        .await
        .expect("get sender key by epoch 0")
        .expect("epoch 0 sender key exists");
    assert_eq!(epoch_zero.key_id, original_key_id);
    assert_eq!(epoch_zero.key_data, vec![4, 5, 6]);
    assert_eq!(epoch_zero.epoch, 0);

    let epoch_three = storage
        .get_sender_key_by_epoch("group-1", "device-a", 3)
        .await
        .expect("get sender key by epoch 3")
        .expect("epoch 3 sender key exists");
    assert_eq!(epoch_three.key_id, rotated_key_id);
    assert_eq!(epoch_three.key_data, vec![7, 8, 9, 10]);
    assert_eq!(epoch_three.epoch, 3);

    storage
        .save_sender_key(
            "group-1",
            "device-b",
            &Uuid::new_v4(),
            &[11, 12, 13],
            1,
            rotated_created_at + 100,
        )
        .await
        .expect("save sender key for second sender");

    storage
        .delete_sender_keys_for_group("group-1")
        .await
        .expect("delete sender keys for group");

    assert!(storage
        .get_sender_key("group-1", "device-a")
        .await
        .expect("reload sender key after delete")
        .is_none());
    assert!(storage
        .get_sender_key_by_epoch("group-1", "device-a", 0)
        .await
        .expect("reload sender key by epoch after delete")
        .is_none());
}

#[tokio::test]
async fn bytes_transferred_update_and_resume_info_round_trip() {
    let (temp_dir, db_path) = temp_db_path("bytes-transferred");
    let storage = SqliteStorage::open(&db_path).expect("storage opens");
    let payload_path = write_transfer_file(temp_dir.path(), "resume.bin", 2_048);
    let mut transfer = transfer_record(&payload_path, TransferStatus::Pending);
    transfer.resume_token = Some("resume-token-1".to_string());

    storage
        .save_transfer(&transfer)
        .await
        .expect("save transfer with resume token");

    let initial = storage
        .get_transfer_resume_info(&transfer.id.to_string())
        .await
        .expect("query initial resume info")
        .expect("resume info exists");
    assert_eq!(initial.bytes_transferred, 0);
    assert_eq!(initial.resume_token.as_deref(), Some("resume-token-1"));

    storage
        .update_bytes_transferred(&transfer.id.to_string(), 512)
        .await
        .expect("update bytes transferred");

    let updated = storage
        .get_transfer_resume_info(&transfer.id.to_string())
        .await
        .expect("query updated resume info")
        .expect("updated resume info exists");
    assert_eq!(updated.bytes_transferred, 512);
    assert_eq!(updated.resume_token.as_deref(), Some("resume-token-1"));

    let stored_transfer = storage
        .get_transfers(10, 0)
        .await
        .expect("load transfers")
        .into_iter()
        .find(|candidate| candidate.id == transfer.id)
        .expect("stored transfer exists");
    assert_eq!(stored_transfer.bytes_transferred, 512);
    assert_eq!(
        stored_transfer.resume_token.as_deref(),
        Some("resume-token-1")
    );

    let conn = Connection::open(&db_path).expect("open sqlite db");
    let row = conn
        .query_row(
            "SELECT bytes_transferred, resume_token FROM transfers WHERE id = ?1",
            params![transfer.id.to_string()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .expect("query transfer resume row");
    assert_eq!(row.0, 512);
    assert_eq!(row.1.as_deref(), Some("resume-token-1"));
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

fn vector_clock(entries: &[(&str, u64)]) -> HashMap<String, u64> {
    entries
        .iter()
        .map(|(device_id, counter)| ((*device_id).to_string(), *counter))
        .collect()
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
        bytes_transferred: 0,
        resume_token: None,
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

fn assert_index_exists(conn: &Connection, index_name: &str, table_name: &str) {
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1 AND tbl_name = ?2",
            params![index_name, table_name],
            |row| row.get(0),
        )
        .expect("query sqlite_master for index");
    assert_eq!(
        exists, 1,
        "expected index {index_name} to exist on {table_name}"
    );
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
