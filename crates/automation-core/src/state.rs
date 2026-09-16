use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Draft,
    LaunchingClient,
    WaitingForClient,
    SearchingContact,
    VerifyingCandidate,
    VerifyingChatHeader,
    PreparingMessage,
    AwaitingHumanConfirmation,
    Sending,
    VerifyingDelivery,
    Completed,
    NeedsHumanReview,
    Failed,
    Cancelled,
}

impl TaskState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::NeedsHumanReview | Self::Failed | Self::Cancelled)
    }

    /// 供界面与审计记录使用的稳定标识，不随文案调整而变化。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "Draft",
            Self::LaunchingClient => "LaunchingClient",
            Self::WaitingForClient => "WaitingForClient",
            Self::SearchingContact => "SearchingContact",
            Self::VerifyingCandidate => "VerifyingCandidate",
            Self::VerifyingChatHeader => "VerifyingChatHeader",
            Self::PreparingMessage => "PreparingMessage",
            Self::AwaitingHumanConfirmation => "AwaitingHumanConfirmation",
            Self::Sending => "Sending",
            Self::VerifyingDelivery => "VerifyingDelivery",
            Self::Completed => "Completed",
            Self::NeedsHumanReview => "NeedsHumanReview",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
        }
    }

    /// 从 [`TaskState::as_str`] 产出的标识还原状态。
    pub fn from_str_name(name: &str) -> Option<Self> {
        Some(match name {
            "Draft" => Self::Draft,
            "LaunchingClient" => Self::LaunchingClient,
            "WaitingForClient" => Self::WaitingForClient,
            "SearchingContact" => Self::SearchingContact,
            "VerifyingCandidate" => Self::VerifyingCandidate,
            "VerifyingChatHeader" => Self::VerifyingChatHeader,
            "PreparingMessage" => Self::PreparingMessage,
            "AwaitingHumanConfirmation" => Self::AwaitingHumanConfirmation,
            "Sending" => Self::Sending,
            "VerifyingDelivery" => Self::VerifyingDelivery,
            "Completed" => Self::Completed,
            "NeedsHumanReview" => Self::NeedsHumanReview,
            "Failed" => Self::Failed,
            "Cancelled" => Self::Cancelled,
            _ => return None,
        })
    }

    /// 面向操作者的中文说明，用于任务历史与失败原因展示。
    pub fn describe(self) -> &'static str {
        match self {
            Self::Draft => "草稿",
            Self::LaunchingClient => "正在启动企业微信",
            Self::WaitingForClient => "等待企业微信就绪",
            Self::SearchingContact => "正在查找联系人",
            Self::VerifyingCandidate => "正在核验联系人",
            Self::VerifyingChatHeader => "正在核验聊天页标题",
            Self::PreparingMessage => "正在准备消息",
            Self::AwaitingHumanConfirmation => "等待人工确认",
            Self::Sending => "正在发送",
            Self::VerifyingDelivery => "正在核验送达",
            Self::Completed => "已完成",
            Self::NeedsHumanReview => "需要人工处理",
            Self::Failed => "已失败",
            Self::Cancelled => "已取消",
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum StateError {
    #[error("不能从终态 {from:?} 转换")]
    TerminalState { from: TaskState },
    #[error("不允许的状态转换：{from:?} → {to:?}")]
    InvalidTransition { from: TaskState, to: TaskState },
}

#[derive(Debug)]
pub struct TaskMachine {
    state: TaskState,
}

impl Default for TaskMachine {
    fn default() -> Self {
        Self { state: TaskState::Draft }
    }
}

impl TaskMachine {
    pub fn state(&self) -> TaskState { self.state }

    pub fn transition(&mut self, to: TaskState) -> Result<(), StateError> {
        let from = self.state;
        if from.is_terminal() {
            return Err(StateError::TerminalState { from });
        }
        if matches!(to, TaskState::NeedsHumanReview | TaskState::Failed | TaskState::Cancelled) {
            self.state = to;
            return Ok(());
        }
        let allowed = matches!(
            (from, to),
            (TaskState::Draft, TaskState::LaunchingClient)
                | (TaskState::LaunchingClient, TaskState::WaitingForClient)
                | (TaskState::WaitingForClient, TaskState::SearchingContact)
                | (TaskState::SearchingContact, TaskState::VerifyingCandidate)
                | (TaskState::VerifyingCandidate, TaskState::VerifyingChatHeader)
                | (TaskState::VerifyingChatHeader, TaskState::PreparingMessage)
                | (TaskState::PreparingMessage, TaskState::AwaitingHumanConfirmation)
                | (TaskState::AwaitingHumanConfirmation, TaskState::Sending)
                | (TaskState::Sending, TaskState::VerifyingDelivery)
                | (TaskState::VerifyingDelivery, TaskState::Completed)
        );
        if !allowed {
            return Err(StateError::InvalidTransition { from, to });
        }
        self.state = to;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_full_verified_path_can_complete() {
        let mut task = TaskMachine::default();
        for state in [
            TaskState::LaunchingClient,
            TaskState::WaitingForClient,
            TaskState::SearchingContact,
            TaskState::VerifyingCandidate,
            TaskState::VerifyingChatHeader,
            TaskState::PreparingMessage,
            TaskState::AwaitingHumanConfirmation,
            TaskState::Sending,
            TaskState::VerifyingDelivery,
            TaskState::Completed,
        ] {
            task.transition(state).unwrap();
        }
        assert_eq!(task.state(), TaskState::Completed);
    }

    #[test]
    fn sending_cannot_skip_human_confirmation() {
        let mut task = TaskMachine::default();
        assert_eq!(
            task.transition(TaskState::Sending),
            Err(StateError::InvalidTransition { from: TaskState::Draft, to: TaskState::Sending })
        );
    }
}
