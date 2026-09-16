//! 存储层测试：审计往返、正文不落库、幂等台账持久化、证据与清理。

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use automation_core::{
    AuditEntry, AuditSink, MessageDigest, SendLedger, TaskId, TaskState,
};
use rusqlite::Connection;
use rusqlite::types::ValueRef;
use storage::{EvidenceStore, RedactedImage, SqliteAuditStore, SqliteSendLedger};

const BODY: &str = "这是一条绝不应该落库的消息正文";

fn temp_path(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("rpa_llm_{}_{}.sqlite", name, uuid::Uuid::new_v4()));
    path
}

fn entry(task_id: TaskId, from: TaskState, to: TaskState, at: SystemTime) -> AuditEntry {
    AuditEntry {
        task_id,
        actor: "测试操作者".into(),
        platform: "windows".into(),
        from,
        to,
        at,
        confirmation_at: Some(at),
        failure_code: None,
        failure_reason: None,
        evidence: vec!["contact_panel#mock-0".into()],
        message: Some(MessageDigest::of(BODY)),
    }
}

#[test]
fn audit_entries_round_trip_through_sqlite() {
    let store = SqliteAuditStore::in_memory().unwrap();
    let task_id = uuid::Uuid::new_v4();
    let now = SystemTime::now();

    store.record(&entry(task_id, TaskState::Sending, TaskState::Completed, now)).unwrap();

    let entries = store.entries_for(task_id).unwrap();
    assert_eq!(entries.len(), 1);
    let first = &entries[0];
    assert_eq!(first.task_id, task_id);
    assert_eq!(first.actor, "测试操作者");
    assert_eq!(first.platform, "windows");
    assert_eq!(first.from, TaskState::Sending);
    assert_eq!(first.to, TaskState::Completed);
    assert_eq!(first.evidence, vec!["contact_panel#mock-0".to_string()]);
    assert!(first.confirmation_at.is_some());
    assert_eq!(first.message.as_ref().unwrap().char_count, BODY.chars().count());
    assert_eq!(first.message.as_ref().unwrap().sha256.len(), 64);
}

#[test]
fn the_message_body_is_never_written_to_the_database() {
    let path = temp_path("no_body");
    let task_id = uuid::Uuid::new_v4();
    {
        let store = SqliteAuditStore::open(&path).unwrap();
        store
            .record(&entry(task_id, TaskState::Sending, TaskState::Completed, SystemTime::now()))
            .unwrap();
    }

    let conn = Connection::open(&path).unwrap();
    let mut stmt = conn.prepare("SELECT * FROM audit_entries").unwrap();
    let columns = stmt.column_count();
    let mut rows = stmt.query([]).unwrap();
    while let Some(row) = rows.next().unwrap() {
        for index in 0..columns {
            let value = row.get_ref(index).unwrap();
            let text = match value {
                ValueRef::Text(bytes) => String::from_utf8_lossy(bytes).to_string(),
                ValueRef::Integer(number) => number.to_string(),
                ValueRef::Real(number) => number.to_string(),
                _ => String::new(),
            };
            assert!(!text.contains(BODY), "第 {index} 列出现了消息正文：{text}");
        }
    }

    // 文件本身也不应包含正文。
    let raw = std::fs::read(&path).unwrap();
    let needle = BODY.as_bytes();
    assert!(
        !raw.windows(needle.len()).any(|w| w == needle),
        "数据库文件中出现了消息正文"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn send_ledger_survives_a_reopen() {
    let path = temp_path("ledger");
    let task_id = uuid::Uuid::new_v4();

    {
        let ledger = SqliteSendLedger::open(&path).unwrap();
        assert!(ledger.claim(task_id).is_ok());
        assert!(ledger.is_claimed(task_id));
    }

    let reopened = SqliteSendLedger::open(&path).unwrap();
    assert!(reopened.is_claimed(task_id), "重启后仍应记得已发送过");
    assert!(reopened.claim(task_id).is_err(), "不得重复占用");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn purge_expired_removes_only_old_entries() {
    let store = SqliteAuditStore::in_memory().unwrap();
    let old_task = uuid::Uuid::new_v4();
    let new_task = uuid::Uuid::new_v4();

    let long_ago = SystemTime::now() - Duration::from_secs(60 * 24 * 60 * 60);
    store.record(&entry(old_task, TaskState::Draft, TaskState::Failed, long_ago)).unwrap();
    store
        .record(&entry(new_task, TaskState::Draft, TaskState::Completed, SystemTime::now()))
        .unwrap();
    assert_eq!(store.count().unwrap(), 2);

    let deleted = store.purge_expired(30).unwrap();
    assert_eq!(deleted, 1);
    assert!(store.entries_for(old_task).unwrap().is_empty());
    assert_eq!(store.entries_for(new_task).unwrap().len(), 1);
}

#[test]
fn evidence_is_stored_redacted_and_purged_after_expiry() {
    let root = std::env::temp_dir().join(format!("rpa_llm_evidence_{}", uuid::Uuid::new_v4()));
    let store = EvidenceStore::in_memory(&root, Duration::ZERO).unwrap();
    let task_id = uuid::Uuid::new_v4();

    let image = RedactedImage::new("failed_chat_header", vec![0x89, 0x50, 0x4e, 0x47]);
    let record = store.save(task_id, &image).unwrap();

    assert_eq!(record.byte_len, 4);
    assert_eq!(record.sha256.len(), 64);
    assert!(store.root().join(&record.relative_path).exists());
    assert_eq!(store.read(&record).unwrap(), image.png_bytes);
    assert_eq!(store.list_for(task_id).unwrap().len(), 1);

    // 保留期为零，立即到期。
    let deleted = store.purge_expired().unwrap();
    assert_eq!(deleted, 1);
    assert_eq!(store.count().unwrap(), 0);
    assert!(!store.root().join(&record.relative_path).exists(), "文件也应被删除");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn evidence_labels_cannot_escape_the_storage_root() {
    let root = std::env::temp_dir().join(format!("rpa_llm_evidence_{}", uuid::Uuid::new_v4()));
    let store = EvidenceStore::in_memory(&root, Duration::from_secs(3600)).unwrap();

    let record = store
        .save(uuid::Uuid::new_v4(), &RedactedImage::new("../../etc/passwd", vec![1, 2, 3]))
        .unwrap();

    assert!(!record.relative_path.contains(".."), "标签不得逃逸出存储根目录");
    let resolved = store.root().join(&record.relative_path);
    assert!(resolved.starts_with(store.root()));

    let _ = std::fs::remove_dir_all(&root);
}
