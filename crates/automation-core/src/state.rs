use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Draft,
    /// 接入客户端：定位已由操作者启动并登录的窗口，并校验它和标定一致。
    ///
    /// 名字里的 "Launching" 是历史遗留——早期版本会自己拉起客户端，现在不会了。
    /// **字符串标识刻意保持不变**（[`TaskState::as_str`]）：审计记录已经按
    /// `"LaunchingClient"` 落库，改名会让历史记录读不回来。
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
    /// 终态：已定位联系人并把正文填进输入框，但**没有发送**。
    ///
    /// 用于「只填不发」模式——操作者想确认定位与输入是否准确，而不希望真的发出消息。
    /// 它与 `Completed` 一样是正常结束，不是失败：没有发生任何意外，
    /// 只是流程按配置在发送前停下了。
    Prepared,
    NeedsHumanReview,
    Failed,
    Cancelled,
}

impl TaskState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Prepared | Self::NeedsHumanReview | Self::Failed | Self::Cancelled
        )
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
            Self::Prepared => "Prepared",
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
            "Prepared" => Self::Prepared,
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
            Self::LaunchingClient => "正在接入客户端",
            Self::WaitingForClient => "客户端已就绪",
            Self::SearchingContact => "正在查找联系人",
            Self::VerifyingCandidate => "正在核验联系人",
            Self::VerifyingChatHeader => "正在核验聊天页标题",
            Self::PreparingMessage => "正在准备消息",
            Self::AwaitingHumanConfirmation => "等待人工确认",
            Self::Sending => "正在发送",
            Self::VerifyingDelivery => "正在核验送达",
            Self::Completed => "已完成",
            Self::Prepared => "已填入正文，未发送",
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
        // 注意：`Prepared` 和 `Completed` 一样，**不**属于上面那组"任意状态都能去"的
        // 终态。它们各自表示某一步正常走完，只能从固定的前驱到达；
        // 否则 `Draft → Prepared` 这种毫无意义的跳转也会被放行。
        let allowed = matches!(
            (from, to),
            (TaskState::Draft, TaskState::LaunchingClient)
                | (TaskState::LaunchingClient, TaskState::WaitingForClient)
                | (TaskState::WaitingForClient, TaskState::SearchingContact)
                | (TaskState::SearchingContact, TaskState::VerifyingCandidate)
                | (TaskState::VerifyingCandidate, TaskState::VerifyingChatHeader)
                | (TaskState::VerifyingChatHeader, TaskState::PreparingMessage)
                | (TaskState::PreparingMessage, TaskState::AwaitingHumanConfirmation)
                | (TaskState::PreparingMessage, TaskState::Prepared)
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

    #[test]
    fn prepared_is_a_terminal_state() {
        assert!(TaskState::Prepared.is_terminal());
        assert_eq!(TaskState::Prepared.as_str(), "Prepared");
        assert_eq!(TaskState::from_str_name("Prepared"), Some(TaskState::Prepared));
    }

    /// `Prepared` 只能从 `PreparingMessage` 到达。
    ///
    /// 它和 `Completed` 一样表示"某一步正常走完"，**不是**那种任意状态都能跳过去的
    /// 失败终态。如果哪天有人把它并进 `NeedsHumanReview | Failed | Cancelled` 那一组，
    /// `Draft → Prepared` 这种毫无意义的跳转就会被放行。
    #[test]
    fn prepared_cannot_be_reached_from_an_unrelated_state() {
        let mut task = TaskMachine::default();
        assert_eq!(
            task.transition(TaskState::Prepared),
            Err(StateError::InvalidTransition { from: TaskState::Draft, to: TaskState::Prepared })
        );
    }

    #[test]
    fn preparing_message_may_end_at_prepared() {
        let mut task = TaskMachine::default();
        for state in [
            TaskState::LaunchingClient,
            TaskState::WaitingForClient,
            TaskState::SearchingContact,
            TaskState::VerifyingCandidate,
            TaskState::VerifyingChatHeader,
            TaskState::PreparingMessage,
        ] {
            task.transition(state).unwrap();
        }
        task.transition(TaskState::Prepared).unwrap();
        assert_eq!(task.state(), TaskState::Prepared);

        // 已经是终态，再想转去 Sending 必须被拒绝。
        assert_eq!(
            task.transition(TaskState::Sending),
            Err(StateError::TerminalState { from: TaskState::Prepared })
        );
    }

    /// 从 `Prepared` 出发再也发不出消息——这是「只填不发」的安全底线。
    #[test]
    fn prepared_can_never_be_followed_by_sending() {
        let mut task = TaskMachine::default();
        for state in [
            TaskState::LaunchingClient,
            TaskState::WaitingForClient,
            TaskState::SearchingContact,
            TaskState::VerifyingCandidate,
            TaskState::VerifyingChatHeader,
            TaskState::PreparingMessage,
        ] {
            task.transition(state).unwrap();
        }
        task.transition(TaskState::Prepared).unwrap();

        // 从 Prepared 出发，无论想去哪一步都不行——它是终态。
        for next in [
            TaskState::Sending,
            TaskState::VerifyingDelivery,
            TaskState::Completed,
            TaskState::AwaitingHumanConfirmation,
        ] {
            assert_eq!(
                task.transition(next),
                Err(StateError::TerminalState { from: TaskState::Prepared }),
                "Prepared 之后不该还能转到 {next:?}"
            );
        }
        assert_eq!(task.state(), TaskState::Prepared);
    }
}
