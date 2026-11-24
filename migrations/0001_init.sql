-- CloudNest initial schema
CREATE TABLE IF NOT EXISTS users (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT NOT NULL,
    role          TEXT NOT NULL DEFAULT 'user' CHECK (role IN ('admin', 'user')),
    disabled      INTEGER NOT NULL DEFAULT 0,
    quota_bytes   INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    token      TEXT PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    csrf       TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    last_seen  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sessions_user ON sessions(user_id);
CREATE INDEX IF NOT EXISTS idx_sessions_expires ON sessions(expires_at);

CREATE TABLE IF NOT EXISTS files (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    owner_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    parent_id    INTEGER REFERENCES files(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    is_dir       INTEGER NOT NULL DEFAULT 0,
    size         INTEGER NOT NULL DEFAULT 0,
    mime         TEXT,
    storage_key  TEXT,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    deleted_at   INTEGER,
    trash_group  TEXT,
    orig_parent_id INTEGER,
    orig_name    TEXT
);
CREATE INDEX IF NOT EXISTS idx_files_owner_parent ON files(owner_id, parent_id);
CREATE INDEX IF NOT EXISTS idx_files_owner_deleted ON files(owner_id, deleted_at);
CREATE INDEX IF NOT EXISTS idx_files_trash_group ON files(trash_group);
CREATE UNIQUE INDEX IF NOT EXISTS idx_files_active_name
    ON files(owner_id, parent_id, name) WHERE deleted_at IS NULL;

CREATE TABLE IF NOT EXISTS shares (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    token          TEXT NOT NULL UNIQUE,
    owner_id       INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    file_id        INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    password_hash  TEXT,
    expires_at     INTEGER,
    max_downloads  INTEGER,
    download_count INTEGER NOT NULL DEFAULT 0,
    created_at     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_shares_owner ON shares(owner_id);
CREATE INDEX IF NOT EXISTS idx_shares_file ON shares(file_id);

CREATE TABLE IF NOT EXISTS uploads (
    token      TEXT PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    parent_id  INTEGER REFERENCES files(id) ON DELETE CASCADE,
    dir_path   TEXT NOT NULL DEFAULT '',
    filename   TEXT NOT NULL,
    total_size INTEGER NOT NULL,
    received   INTEGER NOT NULL DEFAULT 0,
    temp_path  TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
