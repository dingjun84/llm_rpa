//! 任务输入与失败证据端口（从 `ports` 拆出，避开 500 行上限）。

use serde::{Deserialize, Serialize};

use crate::ports::{Screenshot, TaskId, TextBox};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendTask {
    pub id: TaskId,
    pub external_contact_name: String,
    pub text: String,
    pub created_by: String,
}

// 这里曾经有一个 `HumanConfirmation` 端口（`confirm_send`）：发送前阻塞等操作者
// 在界面上点一次"确认"。2026-09-22 操作者要求去掉——**没勾「只填不发」就是同意发**，
// 中间不再插一道等人工的闸门（理由与替代的安全边界见 `runner/message.rs`）。

/// 失败证据记录端口。
///
/// 实现方**必须**先做局部裁切与脱敏（至少遮盖已识别出的文字区域），
/// 只允许把脱敏后的画面落盘；不得保存整屏原图、消息正文或剪贴板内容。
pub trait EvidenceRecorder: Send + Sync {
    fn record(&self, task_id: TaskId, label: &str, frame: &Screenshot, text_boxes: &[TextBox]);
}
