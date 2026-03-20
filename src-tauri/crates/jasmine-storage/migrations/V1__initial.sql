CREATE TABLE messages (
    id TEXT PRIMARY KEY,
    chat_id TEXT NOT NULL,
    sender_id TEXT NOT NULL,
    content TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    status TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE chats (
    id TEXT PRIMARY KEY,
    type TEXT NOT NULL CHECK(type IN ('direct', 'group')),
    name TEXT,
    created_at INTEGER NOT NULL
);

CREATE TABLE chat_members (
    chat_id TEXT NOT NULL,
    member_id TEXT NOT NULL,
    joined_at INTEGER NOT NULL,
    PRIMARY KEY(chat_id, member_id)
);

CREATE TABLE peers (
    device_id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    avatar_hash TEXT,
    last_seen INTEGER,
    ws_port INTEGER
);

CREATE TABLE transfers (
    id TEXT PRIMARY KEY,
    peer_id TEXT NOT NULL,
    filename TEXT NOT NULL,
    size INTEGER NOT NULL,
    direction TEXT NOT NULL CHECK(direction IN ('send', 'receive')),
    status TEXT NOT NULL CHECK(status IN ('pending', 'active', 'completed', 'failed', 'cancelled')),
    sha256 TEXT,
    created_at INTEGER NOT NULL,
    completed_at INTEGER
);

CREATE TABLE transfer_context (
    id TEXT PRIMARY KEY,
    chat_id TEXT,
    local_path TEXT NOT NULL
);

CREATE INDEX idx_messages_chat_id_timestamp ON messages(chat_id, timestamp DESC);
CREATE INDEX idx_chat_members_chat_id ON chat_members(chat_id);
CREATE INDEX idx_transfers_created_at ON transfers(created_at DESC);
