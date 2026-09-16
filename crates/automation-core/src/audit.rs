//! 审计记录与幂等保护。
//!
//! 依据 `docs/architecture.md` §7：审计记录只保存执行任务必需的数据，
//! 消息正文默认不长期保存，只存摘要（长度、哈希、时间）；
//! 不得记录剪贴板原文、账号令牌或完整聊天历史。

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ports::{AutomationError, TaskId};
use crate::state::TaskState;

/// 消息摘要：只保留长度、哈希与时间，绝不落正文。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageDigest {
    pub char_count: usize,
    pub sha256: String,
    pub recorded_at: SystemTime,
}

impl MessageDigest {
    pub fn of(text: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(text.as_bytes());
        Self {
            char_count: text.chars().count(),
            sha256: hex_lower(&hasher.finalize()),
            recorded_at: SystemTime::now(),
        }
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

/// 一条状态转换的审计记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub task_id: TaskId,
    pub actor: String,
    pub platform: String,
    pub from: TaskState,
    pub to: TaskState,
    pub at: SystemTime,
    /// 操作者确认时间；仅在经过人工确认的任务上出现。
    pub confirmation_at: Option<SystemTime>,
    pub failure_code: Option<String>,
    pub failure_reason: Option<String>,
    /// 证据引用（例如截图指纹）。不得包含图像数据或消息正文。
    pub evidence: Vec<String>,
    /// 消息摘要。绝不包含正文。
    pub message: Option<MessageDigest>,
}

pub trait AuditSink: Send + Sync {
    fn record(&self, entry: &AuditEntry) -> Result<(), AutomationError>;
}

/// 不记录任何内容，仅用于不需要审计的场景（如纯状态机单测）。
#[derive(Debug, Default)]
pub struct NoopAudit;

impl AuditSink for NoopAudit {
    fn record(&self, _entry: &AuditEntry) -> Result<(), AutomationError> {
        Ok(())
    }
}

/// 内存审计，供测试与本地预览使用。
#[derive(Debug, Default)]
pub struct MemoryAudit {
    entries: Mutex<Vec<AuditEntry>>,
}

impl MemoryAudit {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn entries(&self) -> Vec<AuditEntry> {
        self.entries.lock().unwrap().clone()
    }

    pub fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl AuditSink for MemoryAudit {
    fn record(&self, entry: &AuditEntry) -> Result<(), AutomationError> {
        self.entries.lock().unwrap().push(entry.clone());
        Ok(())
    }
}

/// 幂等保护：同一任务只允许进入一次发送流程。
///
/// 占用后不提供释放接口：发送一旦开始，宁可让任务停在人工处理，
/// 也不允许重试造成重复发送。
pub trait SendLedger: Send + Sync {
    /// 首次占用返回 `Ok`；重复占用返回需要人工处理的错误。
    fn claim(&self, task_id: TaskId) -> Result<(), AutomationError>;

    fn is_claimed(&self, task_id: TaskId) -> bool;
}

#[derive(Debug, Default)]
pub struct MemorySendLedger {
    claimed: Mutex<BTreeSet<TaskId>>,
}

impl MemorySendLedger {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SendLedger for MemorySendLedger {
    fn claim(&self, task_id: TaskId) -> Result<(), AutomationError> {
        let mut claimed = self.claimed.lock().unwrap();
        if !claimed.insert(task_id) {
            return Err(AutomationError::NeedsHumanReview(format!(
                "任务 {task_id} 已进入过发送流程，拒绝重复发送"
            )));
        }
        Ok(())
    }

    fn is_claimed(&self, task_id: TaskId) -> bool {
        self.claimed.lock().unwrap().contains(&task_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_keeps_only_length_and_hash() {
        let digest = MessageDigest::of("你好，明天见");
        assert_eq!(digest.char_count, 6);
        assert_eq!(digest.sha256.len(), 64);
    }

    #[test]
    fn digest_is_stable_for_same_text() {
        assert_eq!(MessageDigest::of("abc").sha256, MessageDigest::of("abc").sha256);
    }

    #[test]
    fn digest_differs_for_different_text() {
        assert_ne!(MessageDigest::of("abc").sha256, MessageDigest::of("abd").sha256);
    }

    #[test]
    fn ledger_rejects_second_claim() {
        let ledger = MemorySendLedger::new();
        let id = uuid::Uuid::new_v4();
        assert!(ledger.claim(id).is_ok());
        assert!(ledger.claim(id).is_err());
        assert!(ledger.is_claimed(id));
    }

    #[test]
    fn ledger_allows_distinct_tasks() {
        let ledger = MemorySendLedger::new();
        assert!(ledger.claim(uuid::Uuid::new_v4()).is_ok());
        assert!(ledger.claim(uuid::Uuid::new_v4()).is_ok());
    }
}
