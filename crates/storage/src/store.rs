//! 审计写入、查询、清理与幂等台账。

use std::path::Path;
use std::sync::Mutex;
use std::time::SystemTime;

use automation_core::{AuditEntry, AuditSink, AutomationError, SendLedger, TaskId, TaskState};
use rusqlite::{params, Connection};
use thiserror::Error;

use crate::db;
use crate::time::{from_unix_ms, now_unix_ms, to_unix_ms};

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("数据库操作失败：{0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("证据引用序列化失败：{0}")]
    Serde(#[from] serde_json::Error),
    #[error("审计记录中的任务 ID 不是合法 UUID：{0}")]
    InvalidTaskId(String),
    #[error("审计记录中出现未知状态标识：{0}")]
    UnknownState(String),
    #[error("文件操作失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("数据库互斥锁已中毒")]
    Poisoned,
}

impl From<StorageError> for AutomationError {
    fn from(err: StorageError) -> Self {
        AutomationError::Platform(format!("审计存储失败：{err}"))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, StorageError> {
    mutex.lock().map_err(|_| StorageError::Poisoned)
}

/// SQLite 审计接收器。
pub struct SqliteAuditStore {
    conn: Mutex<Connection>,
}

impl SqliteAuditStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        Ok(Self { conn: Mutex::new(db::open(path)?) })
    }

    pub fn in_memory() -> Result<Self, StorageError> {
        Ok(Self { conn: Mutex::new(db::open_in_memory()?) })
    }

    pub fn from_connection(conn: Connection) -> Result<Self, StorageError> {
        db::migrate(&conn)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn count(&self) -> Result<u64, StorageError> {
        let conn = lock(&self.conn)?;
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM audit_entries", [], |row| row.get(0))?;
        Ok(count.max(0) as u64)
    }

    /// 按任务 ID 取回审计轨迹，按时间升序。
    pub fn entries_for(&self, task_id: TaskId) -> Result<Vec<AuditEntry>, StorageError> {
        let conn = lock(&self.conn)?;
        let mut stmt = conn.prepare(
            "SELECT task_id, actor, platform, from_state, to_state, at_unix_ms,
                    failure_code, failure_reason, evidence_json,
                    message_char_count, message_sha256, message_recorded_at_ms
             FROM audit_entries
             WHERE task_id = ?1
             ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![task_id.to_string()], |row| {
            let evidence_json: String = row.get(8)?;
            let char_count: Option<i64> = row.get(9)?;
            let sha256: Option<String> = row.get(10)?;
            let recorded_at: Option<i64> = row.get(11)?;
            Ok(RawEntry {
                task_id: row.get(0)?,
                actor: row.get(1)?,
                platform: row.get(2)?,
                from_state: row.get(3)?,
                to_state: row.get(4)?,
                at_unix_ms: row.get(5)?,
                failure_code: row.get(6)?,
                failure_reason: row.get(7)?,
                evidence_json,
                message_char_count: char_count,
                message_sha256: sha256,
                message_recorded_at_ms: recorded_at,
            })
        })?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?.into_entry()?);
        }
        Ok(entries)
    }

    /// 删除早于给定时刻的审计记录，返回删除行数。
    pub fn purge_older_than(&self, cutoff: SystemTime) -> Result<usize, StorageError> {
        let conn = lock(&self.conn)?;
        let deleted = conn.execute(
            "DELETE FROM audit_entries WHERE at_unix_ms < ?1",
            params![to_unix_ms(cutoff)],
        )?;
        Ok(deleted)
    }

    /// 按保留天数清理，返回删除行数。
    pub fn purge_expired(&self, retention_days: i64) -> Result<usize, StorageError> {
        let days = retention_days.max(0);
        let cutoff_ms = now_unix_ms() - days * 24 * 60 * 60 * 1000;
        let conn = lock(&self.conn)?;
        let deleted = conn.execute(
            "DELETE FROM audit_entries WHERE at_unix_ms < ?1",
            params![cutoff_ms],
        )?;
        Ok(deleted)
    }
}

impl SqliteAuditStore {
    fn insert(&self, entry: &AuditEntry) -> Result<(), StorageError> {
        let evidence_json = serde_json::to_string(&entry.evidence)?;
        let conn = lock(&self.conn)?;
        conn.execute(
            "INSERT INTO audit_entries (
                task_id, actor, platform, from_state, to_state, at_unix_ms,
                failure_code, failure_reason, evidence_json,
                message_char_count, message_sha256, message_recorded_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                entry.task_id.to_string(),
                entry.actor,
                entry.platform,
                entry.from.as_str(),
                entry.to.as_str(),
                to_unix_ms(entry.at),
                entry.failure_code,
                entry.failure_reason,
                evidence_json,
                entry.message.as_ref().map(|m| m.char_count as i64),
                entry.message.as_ref().map(|m| m.sha256.clone()),
                entry.message.as_ref().map(|m| to_unix_ms(m.recorded_at)),
            ],
        )?;
        Ok(())
    }
}

impl AuditSink for SqliteAuditStore {
    fn record(&self, entry: &AuditEntry) -> Result<(), AutomationError> {
        self.insert(entry).map_err(AutomationError::from)
    }
}

/// 落库的持久幂等台账，保证跨进程重启也不会重复发送。
pub struct SqliteSendLedger {
    conn: Mutex<Connection>,
}

impl SqliteSendLedger {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        Ok(Self { conn: Mutex::new(db::open(path)?) })
    }

    pub fn in_memory() -> Result<Self, StorageError> {
        Ok(Self { conn: Mutex::new(db::open_in_memory()?) })
    }

    pub fn from_connection(conn: Connection) -> Result<Self, StorageError> {
        db::migrate(&conn)?;
        Ok(Self { conn: Mutex::new(conn) })
    }
}

impl SqliteSendLedger {
    fn try_claim(&self, task_id: TaskId) -> Result<bool, StorageError> {
        let conn = lock(&self.conn)?;
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO send_ledger (task_id, claimed_at_unix_ms) VALUES (?1, ?2)",
            params![task_id.to_string(), now_unix_ms()],
        )?;
        Ok(inserted > 0)
    }

    fn query_claimed(&self, task_id: TaskId) -> Result<bool, StorageError> {
        let conn = lock(&self.conn)?;
        Ok(conn
            .query_row(
                "SELECT 1 FROM send_ledger WHERE task_id = ?1",
                params![task_id.to_string()],
                |_| Ok(()),
            )
            .is_ok())
    }
}

impl SendLedger for SqliteSendLedger {
    fn claim(&self, task_id: TaskId) -> Result<(), AutomationError> {
        let inserted = self.try_claim(task_id).map_err(AutomationError::from)?;
        if !inserted {
            return Err(AutomationError::NeedsHumanReview(format!(
                "任务 {task_id} 已进入过发送流程，拒绝重复发送"
            )));
        }
        Ok(())
    }

    fn is_claimed(&self, task_id: TaskId) -> bool {
        self.query_claimed(task_id).unwrap_or(false)
    }
}

struct RawEntry {
    task_id: String,
    actor: String,
    platform: String,
    from_state: String,
    to_state: String,
    at_unix_ms: i64,
    failure_code: Option<String>,
    failure_reason: Option<String>,
    evidence_json: String,
    message_char_count: Option<i64>,
    message_sha256: Option<String>,
    message_recorded_at_ms: Option<i64>,
}

impl RawEntry {
    fn into_entry(self) -> Result<AuditEntry, StorageError> {
        let task_id = self
            .task_id
            .parse()
            .map_err(|_| StorageError::InvalidTaskId(self.task_id.clone()))?;
        let parse_state = |name: &str| {
            TaskState::from_str_name(name)
                .ok_or_else(|| StorageError::UnknownState(name.to_string()))
        };
        let message = match (self.message_char_count, self.message_sha256) {
            (Some(count), Some(sha256)) => Some(automation_core::MessageDigest {
                char_count: count.max(0) as usize,
                sha256,
                recorded_at: from_unix_ms(self.message_recorded_at_ms.unwrap_or(0)),
            }),
            _ => None,
        };
        Ok(AuditEntry {
            task_id,
            actor: self.actor,
            platform: self.platform,
            from: parse_state(&self.from_state)?,
            to: parse_state(&self.to_state)?,
            at: from_unix_ms(self.at_unix_ms),
            failure_code: self.failure_code,
            failure_reason: self.failure_reason,
            evidence: serde_json::from_str(&self.evidence_json)?,
            message,
        })
    }
}
