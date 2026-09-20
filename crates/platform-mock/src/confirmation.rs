//! 人工确认端口替身。

use std::sync::Mutex;
use std::time::Duration;

use automation_core::{AutomationError, HumanConfirmation, SendTask, TaskId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmationOutcome {
    /// 操作者在有效期内确认。
    Approve,
    /// 操作者明确拒绝。
    Reject(String),
    /// 确认超时，未在有效期内完成。
    Expired,
}

#[derive(Debug)]
pub struct MockHumanConfirmation {
    outcome: Mutex<ConfirmationOutcome>,
    /// 每次请求确认时记录的 (任务 ID, 有效期)。
    pub calls: Mutex<Vec<(TaskId, Duration)>>,
}

impl Default for MockHumanConfirmation {
    fn default() -> Self {
        Self::new(ConfirmationOutcome::Approve)
    }
}

impl MockHumanConfirmation {
    pub fn new(outcome: ConfirmationOutcome) -> Self {
        Self { outcome: Mutex::new(outcome), calls: Mutex::new(Vec::new()) }
    }

    pub fn set_outcome(&self, outcome: ConfirmationOutcome) {
        *self.outcome.lock().unwrap() = outcome;
    }

    pub fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    pub fn last_ttl(&self) -> Option<Duration> {
        self.calls.lock().unwrap().last().map(|(_, ttl)| *ttl)
    }
}

impl HumanConfirmation for MockHumanConfirmation {
    fn confirm_send(
        &self,
        task: &SendTask,
        expires_in: Duration,
    ) -> Result<(), AutomationError> {
        self.calls.lock().unwrap().push((task.id, expires_in));
        match self.outcome.lock().unwrap().clone() {
            ConfirmationOutcome::Approve => Ok(()),
            ConfirmationOutcome::Reject(reason) => {
                Err(AutomationError::NeedsHumanReview(format!("操作者拒绝发送：{reason}")))
            }
            ConfirmationOutcome::Expired => Err(AutomationError::NeedsHumanReview(
                "人工确认已过期，未获得有效确认".into(),
            )),
        }
    }
}
