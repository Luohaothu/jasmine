CREATE TABLE sender_keys_v4 (
    group_id TEXT NOT NULL,
    sender_device_id TEXT NOT NULL,
    epoch INTEGER NOT NULL DEFAULT 0,
    key_id TEXT NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000',
    key_data BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(group_id, sender_device_id, epoch)
);

INSERT INTO sender_keys_v4 (group_id, sender_device_id, epoch, key_id, key_data, created_at)
SELECT
    group_id,
    sender_device_id,
    epoch,
    '00000000-0000-0000-0000-000000000000',
    key_data,
    created_at
FROM sender_keys;

DROP TABLE sender_keys;
ALTER TABLE sender_keys_v4 RENAME TO sender_keys;

CREATE INDEX idx_sender_keys_latest ON sender_keys(group_id, sender_device_id, epoch DESC, created_at DESC);

PRAGMA user_version = 4;
