//! 脱敏证据的持久化。
//!
//! 依据 `docs/architecture.md` §7：失败截图必须局部裁切并脱敏，
//! 且设置可配置的自动清理期限。
//!
//! 本模块**只接受**已经裁切与脱敏的图像（[`RedactedImage`]）。
//! 裁切与打码由视觉层完成，存储层负责落盘、计算哈希与到期清理。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use automation_core::TaskId;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

use crate::db;
use crate::store::StorageError;
use crate::time::{from_unix_ms, now_unix_ms, to_unix_ms};

/// 已经过局部裁切与脱敏处理的图像。
///
/// 构造它即代表调用方已完成脱敏；存储层不会再做任何图像处理。
#[derive(Debug, Clone)]
pub struct RedactedImage {
    pub label: String,
    pub png_bytes: Vec<u8>,
}

impl RedactedImage {
    pub fn new(label: impl Into<String>, png_bytes: Vec<u8>) -> Self {
        Self { label: label.into(), png_bytes }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceRecord {
    pub id: i64,
    pub task_id: TaskId,
    pub label: String,
    pub relative_path: String,
    pub byte_len: u64,
    pub sha256: String,
    pub created_at: SystemTime,
    pub expires_at: SystemTime,
}

pub struct EvidenceStore {
    conn: Mutex<Connection>,
    root: PathBuf,
    retention: Duration,
}

impl EvidenceStore {
    pub fn open(
        db_path: impl AsRef<Path>,
        root: impl AsRef<Path>,
        retention: Duration,
    ) -> Result<Self, StorageError> {
        std::fs::create_dir_all(root.as_ref()).map_err(StorageError::Io)?;
        Ok(Self {
            conn: Mutex::new(db::open(db_path)?),
            root: root.as_ref().to_path_buf(),
            retention,
        })
    }

    pub fn in_memory(
        root: impl AsRef<Path>,
        retention: Duration,
    ) -> Result<Self, StorageError> {
        std::fs::create_dir_all(root.as_ref()).map_err(StorageError::Io)?;
        Ok(Self {
            conn: Mutex::new(db::open_in_memory()?),
            root: root.as_ref().to_path_buf(),
            retention,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 保存一份脱敏证据，返回落库记录。
    pub fn save(
        &self,
        task_id: TaskId,
        image: &RedactedImage,
    ) -> Result<EvidenceRecord, StorageError> {
        let created_at = SystemTime::now();
        let expires_at = created_at + self.retention;

        let dir = self.root.join(task_id.to_string());
        std::fs::create_dir_all(&dir).map_err(StorageError::Io)?;
        let file_name = format!("{}-{}.png", sanitize(&image.label), now_unix_ms());
        let path = dir.join(&file_name);
        std::fs::write(&path, &image.png_bytes).map_err(StorageError::Io)?;

        let relative_path = format!("{task_id}/{file_name}");
        let sha256 = hex_lower(&Sha256::digest(&image.png_bytes));

        let conn = self.conn.lock().map_err(|_| StorageError::Poisoned)?;
        conn.execute(
            "INSERT INTO evidence_artifacts
                (task_id, label, relative_path, byte_len, sha256, created_at_ms, expires_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                task_id.to_string(),
                image.label,
                relative_path,
                image.png_bytes.len() as i64,
                sha256,
                to_unix_ms(created_at),
                to_unix_ms(expires_at),
            ],
        )?;
        let id = conn.last_insert_rowid();

        Ok(EvidenceRecord {
            id,
            task_id,
            label: image.label.clone(),
            relative_path,
            byte_len: image.png_bytes.len() as u64,
            sha256,
            created_at,
            expires_at,
        })
    }

    pub fn list_for(&self, task_id: TaskId) -> Result<Vec<EvidenceRecord>, StorageError> {
        let conn = self.conn.lock().map_err(|_| StorageError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT id, task_id, label, relative_path, byte_len, sha256,
                    created_at_ms, expires_at_ms
             FROM evidence_artifacts
             WHERE task_id = ?1
             ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![task_id.to_string()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
            ))
        })?;

        let mut records = Vec::new();
        for row in rows {
            let (id, task_id, label, relative_path, byte_len, sha256, created, expires) = row?;
            records.push(EvidenceRecord {
                id,
                task_id: task_id
                    .parse()
                    .map_err(|_| StorageError::InvalidTaskId(task_id.clone()))?,
                label,
                relative_path,
                byte_len: byte_len.max(0) as u64,
                sha256,
                created_at: from_unix_ms(created),
                expires_at: from_unix_ms(expires),
            });
        }
        Ok(records)
    }

    pub fn read(&self, record: &EvidenceRecord) -> Result<Vec<u8>, StorageError> {
        std::fs::read(self.root.join(&record.relative_path)).map_err(StorageError::Io)
    }

    /// 删除已到期的证据文件与记录，返回删除条数。
    pub fn purge_expired(&self) -> Result<usize, StorageError> {
        let now = now_unix_ms();
        let conn = self.conn.lock().map_err(|_| StorageError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT id, relative_path FROM evidence_artifacts WHERE expires_at_ms <= ?1",
        )?;
        let expired: Vec<(i64, String)> = stmt
            .query_map(params![now], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        drop(stmt);

        for (_, relative_path) in &expired {
            // 文件可能已被人工删除，这里不视为错误。
            let _ = std::fs::remove_file(self.root.join(relative_path));
        }
        let mut deleted = 0usize;
        for (id, _) in &expired {
            deleted += conn.execute("DELETE FROM evidence_artifacts WHERE id = ?1", params![id])?;
        }
        Ok(deleted)
    }

    pub fn count(&self) -> Result<u64, StorageError> {
        let conn = self.conn.lock().map_err(|_| StorageError::Poisoned)?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM evidence_artifacts",
            [],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u64)
    }
}

fn sanitize(label: &str) -> String {
    let cleaned: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if cleaned.is_empty() {
        "evidence".to_string()
    } else {
        cleaned
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}
