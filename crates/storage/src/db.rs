//! 数据库连接与建表。
//!
//! 注意：`audit_entries` 表中**没有**任何可以存放消息正文、剪贴板原文
//! 或聊天记录的列。这不是靠调用方自觉，而是结构上就无法写入。

use std::path::Path;

use rusqlite::Connection;

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS audit_entries (
    id                       INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id                  TEXT    NOT NULL,
    actor                    TEXT    NOT NULL,
    platform                 TEXT    NOT NULL,
    from_state               TEXT    NOT NULL,
    to_state                 TEXT    NOT NULL,
    at_unix_ms               INTEGER NOT NULL,
    confirmation_at_unix_ms  INTEGER,
    failure_code             TEXT,
    failure_reason           TEXT,
    evidence_json            TEXT    NOT NULL DEFAULT '[]',
    message_char_count       INTEGER,
    message_sha256           TEXT,
    message_recorded_at_ms   INTEGER
);

CREATE INDEX IF NOT EXISTS idx_audit_task_id ON audit_entries (task_id);
CREATE INDEX IF NOT EXISTS idx_audit_at      ON audit_entries (at_unix_ms);

CREATE TABLE IF NOT EXISTS send_ledger (
    task_id           TEXT    PRIMARY KEY,
    claimed_at_unix_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS evidence_artifacts (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id          TEXT    NOT NULL,
    label            TEXT    NOT NULL,
    relative_path    TEXT    NOT NULL,
    byte_len         INTEGER NOT NULL,
    sha256           TEXT    NOT NULL,
    created_at_ms    INTEGER NOT NULL,
    expires_at_ms    INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_evidence_task    ON evidence_artifacts (task_id);
CREATE INDEX IF NOT EXISTS idx_evidence_expires ON evidence_artifacts (expires_at_ms);
"#;

/// 打开磁盘数据库并完成建表。
pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    configure(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

/// 打开内存数据库并完成建表，供测试使用。
pub fn open_in_memory() -> rusqlite::Result<Connection> {
    let conn = Connection::open_in_memory()?;
    configure(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

/// 多个连接可能同时写入同一个文件，给一个宽松的忙等待窗口。
fn configure(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(std::time::Duration::from_secs(5))
}

pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA)
}
