//! 界面驱动的人工确认。
//!
//! `automation-core` 的 [`HumanConfirmation`] 端口在这里被真正实现为
//! "阻塞等待操作者在界面上点击"：工作流线程会停在
//! `AwaitingHumanConfirmation`，直到确认、拒绝或超时。

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use automation_core::{AutomationError, HumanConfirmation, SendTask, TaskId};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};

pub const EVENT_CONFIRMATION_REQUESTED: &str = "task://confirmation-requested";

/// 发给界面的确认请求。
#[derive(Debug, Clone, Serialize)]
pub struct ConfirmationRequest {
    pub task_id: String,
    pub external_contact_name: String,
    pub text: String,
    pub expires_in_ms: u64,
}

#[derive(Debug, Default)]
struct SlotState {
    decided: Option<Result<(), String>>,
}

type Slot = Arc<(Mutex<SlotState>, Condvar)>;

pub struct UiConfirmation<R: Runtime> {
    app: AppHandle<R>,
    slots: Mutex<HashMap<TaskId, Slot>>,
}

impl<R: Runtime> UiConfirmation<R> {
    pub fn new(app: AppHandle<R>) -> Self {
        Self { app, slots: Mutex::new(HashMap::new()) }
    }

    /// 由 `confirm_task` 命令调用：记录操作者的决定并唤醒工作流线程。
    pub fn decide(
        &self,
        task_id: TaskId,
        approved: bool,
        reason: Option<String>,
    ) -> Result<(), String> {
        let slot = self
            .slots
            .lock()
            .map_err(|_| "确认状态锁已中毒".to_string())?
            .get(&task_id)
            .cloned()
            .ok_or_else(|| "该任务当前不在等待确认".to_string())?;

        let (lock, cvar) = &*slot;
        let mut guard = lock.lock().map_err(|_| "确认状态锁已中毒".to_string())?;
        if guard.decided.is_some() {
            return Ok(());
        }
        guard.decided = Some(if approved {
            Ok(())
        } else {
            Err(reason.unwrap_or_else(|| "操作者拒绝发送".to_string()))
        });
        drop(guard);
        cvar.notify_all();
        Ok(())
    }

    pub fn is_pending(&self, task_id: TaskId) -> bool {
        self.slots
            .lock()
            .map(|slots| slots.contains_key(&task_id))
            .unwrap_or(false)
    }
}

impl<R: Runtime> HumanConfirmation for UiConfirmation<R> {
    fn confirm_send(&self, task: &SendTask, expires_in: Duration) -> Result<(), AutomationError> {
        let slot: Slot = Arc::new((Mutex::new(SlotState::default()), Condvar::new()));
        {
            let mut slots = self
                .slots
                .lock()
                .map_err(|_| AutomationError::Platform("确认状态锁已中毒".into()))?;
            slots.insert(task.id, slot.clone());
        }

        // 通知界面弹出确认框。界面必须同时展示目标名称与消息预览。
        let _ = self.app.emit(
            EVENT_CONFIRMATION_REQUESTED,
            ConfirmationRequest {
                task_id: task.id.to_string(),
                external_contact_name: task.external_contact_name.clone(),
                text: task.text.clone(),
                expires_in_ms: expires_in.as_millis() as u64,
            },
        );

        let (lock, cvar) = &*slot;
        let guard = lock
            .lock()
            .map_err(|_| AutomationError::Platform("确认状态锁已中毒".into()))?;
        let (guard, _timeout) = cvar
            .wait_timeout_while(guard, expires_in, |state| state.decided.is_none())
            .map_err(|_| AutomationError::Platform("确认等待被中断".into()))?;
        let decision = guard.decided.clone();
        drop(guard);

        if let Ok(mut slots) = self.slots.lock() {
            slots.remove(&task.id);
        }

        match decision {
            Some(Ok(())) => Ok(()),
            Some(Err(reason)) => {
                Err(AutomationError::NeedsHumanReview(format!("操作者拒绝发送：{reason}")))
            }
            None => Err(AutomationError::NeedsHumanReview(
                "人工确认已过期，未获得有效确认".into(),
            )),
        }
    }
}
