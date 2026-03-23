CREATE TABLE IF NOT EXISTS folder_file_status (
    folder_id TEXT NOT NULL,
    file_path TEXT NOT NULL,
    status TEXT NOT NULL,
    bytes_transferred INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(folder_id, file_path)
);

PRAGMA user_version = 5;
