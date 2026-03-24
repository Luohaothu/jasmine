ALTER TABLE messages ADD COLUMN vector_clock TEXT NULL;

CREATE TABLE og_cache (
    url TEXT PRIMARY KEY,
    title TEXT,
    description TEXT,
    image_url TEXT,
    site_name TEXT,
    fetched_at INTEGER NOT NULL,
    ttl_seconds INTEGER NOT NULL DEFAULT 86400
);

CREATE INDEX idx_messages_reply_to_id ON messages(reply_to_id);

PRAGMA user_version = 6;
