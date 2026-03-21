ALTER TABLE messages ADD COLUMN edit_version INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN edited_at INTEGER NULL;
ALTER TABLE messages ADD COLUMN is_deleted INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN deleted_at INTEGER NULL;
ALTER TABLE messages ADD COLUMN reply_to_id TEXT NULL;
ALTER TABLE messages ADD COLUMN reply_to_preview TEXT NULL;

ALTER TABLE transfers ADD COLUMN thumbnail_path TEXT NULL;
ALTER TABLE transfers ADD COLUMN folder_id TEXT NULL;
ALTER TABLE transfers ADD COLUMN folder_relative_path TEXT NULL;

PRAGMA user_version = 2;
