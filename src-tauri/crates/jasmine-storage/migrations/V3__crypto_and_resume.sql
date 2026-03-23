CREATE TABLE IF NOT EXISTS peer_keys (
    device_id TEXT PRIMARY KEY,
    public_key BLOB NOT NULL,
    fingerprint TEXT NOT NULL,
    verified INTEGER NOT NULL DEFAULT 0,
    first_seen INTEGER NOT NULL,
    last_seen INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sender_keys (
    group_id TEXT NOT NULL,
    sender_device_id TEXT NOT NULL,
    key_data BLOB NOT NULL,
    epoch INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(group_id, sender_device_id)
);

ALTER TABLE transfers ADD COLUMN bytes_transferred INTEGER NOT NULL DEFAULT 0;
ALTER TABLE transfers ADD COLUMN resume_token TEXT NULL;

PRAGMA user_version = 3;
