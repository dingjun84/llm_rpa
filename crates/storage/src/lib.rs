//! 本地 SQLite 持久化：审计记录、幂等台账与脱敏证据。
//!
//! 数据边界（见 `docs/architecture.md` §7）：
//!
//! - 只保存执行任务必需的数据；
//! - 消息正文**永不落库**，只保存长度、哈希与时间；
//! - 不保存剪贴板原文、账号令牌或完整聊天历史；
//! - 失败截图必须先由视觉层局部裁切并脱敏，再交给 [`EvidenceStore`]。

pub mod db;
pub mod evidence;
pub mod store;
pub mod time;

pub use db::{open, open_in_memory};
pub use evidence::{EvidenceRecord, EvidenceStore, RedactedImage};
pub use store::{SqliteAuditStore, SqliteSendLedger, StorageError};

/// 默认审计保留期限。
pub const DEFAULT_RETENTION_DAYS: i64 = 30;
