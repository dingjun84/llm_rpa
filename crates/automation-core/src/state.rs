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
    /// 用模板匹配找到左侧导航图标并点击它，把界面切到"能查到联系人"的那个视图。
    ///
    /// 这一步是**可选**的（`RunnerConfig.navigate_before_search`）：
    /// 关掉时状态机直接走 `WaitingForClient → SearchingContact`，两条边都允许。
    ///
    /// 之所以值得单独一个状态，是因为它是一个**会改变界面内容的动作**：
    /// 它点下去之后画面会重绘，后面所有的截图与识别都必须建立在"新画面"上。
    /// 藏在"查找联系人"里的话，出问题时看不出"是切换没生效"还是"列表里没有这个人"。
    NavigatingToView,
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
    /// 全部状态的清单，供「遍历式」用例使用。
    ///
    /// ## 为什么要有这张表
    ///
    /// 标识（[`TaskState::as_str`]）、反查（[`TaskState::from_str_name`]）、
    /// 说明（[`TaskState::describe`]）是三个**互相独立**的 `match`。
    /// 新增一个状态时，只改其中一两个是很自然的疏忽——而后果是审计库里
    /// 的历史记录读不回来（漏了 `from_str_name`）或界面上一片空白（漏了 `describe`）。
    ///
    /// 手写「断言某个状态能往返」的用例堵不住这个洞：它只测作者当时想到的那一个。
    /// 遍历这张表才能一次覆盖全部状态。
    ///
    /// ## 维护约定
    ///
    /// **新增状态时必须同时加进这张表。** 这一点编译器不会替我们盯着
    /// （数组长度是字面量），所以下面 `all_variants_round_trip` 那条用例
    /// 是唯一的兜底——表里没有的状态，它就测不到。
    pub const ALL: [TaskState; 16] = [
        Self::Draft,
        Self::LaunchingClient,
        Self::WaitingForClient,
        Self::NavigatingToView,
        Self::SearchingContact,
        Self::VerifyingCandidate,
        Self::VerifyingChatHeader,
        Self::PreparingMessage,
        Self::AwaitingHumanConfirmation,
        Self::Sending,
        Self::VerifyingDelivery,
        Self::Completed,
        Self::Prepared,
        Self::NeedsHumanReview,
        Self::Failed,
        Self::Cancelled,
    ];

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
            Self::NavigatingToView => "NavigatingToView",
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
            "NavigatingToView" => Self::NavigatingToView,
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
            Self::NavigatingToView => "正在切换视图",
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
                // 「切换视图」是可选的：开了它就走上面那条边，关了就直接进查找。
                // 两条边都必须允许——只留一条会把"关掉这个功能"变成一次非法转换。
                | (TaskState::WaitingForClient, TaskState::NavigatingToView)
                | (TaskState::WaitingForClient, TaskState::SearchingContact)
                | (TaskState::NavigatingToView, TaskState::SearchingContact)
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

    /// 「先点导航图标切视图」这一步是**可选**的，两条边都必须放行。
    ///
    /// 只留其中一条都会坏：删掉直连边 ⇒ 关掉这个功能之后每次任务都在这里报非法转换；
    /// 删掉经 `NavigatingToView` 的边 ⇒ 打开这个功能就再也跑不起来。
    /// 这两种失败都发生在任务刚开始的时候，看起来都像"程序坏了"。
    #[test]
    fn navigating_to_view_is_an_optional_step() {
        let mut direct = TaskMachine::default();
        for state in [TaskState::LaunchingClient, TaskState::WaitingForClient] {
            direct.transition(state).unwrap();
        }
        direct.transition(TaskState::SearchingContact).unwrap();
        assert_eq!(direct.state(), TaskState::SearchingContact);

        let mut via_icon = TaskMachine::default();
        for state in [
            TaskState::LaunchingClient,
            TaskState::WaitingForClient,
            TaskState::NavigatingToView,
            TaskState::SearchingContact,
        ] {
            via_icon.transition(state).unwrap();
        }
        assert_eq!(via_icon.state(), TaskState::SearchingContact);
    }

    /// 每个状态都必须能从自己的标识**读回来**，并且有非空的说明文案。
    ///
    /// 遍历 [`TaskState::ALL`] 而不是逐个手写：漏了哪个状态，这条用例就会点名报出来。
    /// （实测过它的价值：`NavigatingToView` 曾经漏在 `from_str_name` 之外，
    /// 症状是审计库里这一条记录的 `state` 列还原不出来。）
    #[test]
    fn all_variants_round_trip_through_their_identifier() {
        // 先钉住表本身没被漏改：长度与去重后的数量必须一致。
        assert_eq!(
            TaskState::ALL.len(),
            16,
            "状态数量变了——请同时更新 ALL 的长度与内容"
        );
        let mut names: Vec<&str> = TaskState::ALL.iter().map(|s| s.as_str()).collect();
        names.sort_unstable();
        let unique = names.len();
        names.dedup();
        assert_eq!(names.len(), unique, "两个状态用了同一个标识：{names:?}");

        for state in TaskState::ALL {
            let name = state.as_str();
            assert_eq!(
                TaskState::from_str_name(name),
                Some(state),
                "{name} 读不回来 —— `from_str_name` 里漏了这个分支"
            );
            assert!(
                !state.describe().is_empty(),
                "{name} 没有说明文案 —— `describe` 里漏了这个分支"
            );
        }

        // 未知标识必须返回 None，不能悄悄落到某个默认状态上。
        assert_eq!(TaskState::from_str_name("NoSuchState"), None);
        assert_eq!(TaskState::from_str_name(""), None);
    }

    /// 终态集合必须与 `is_terminal` 一致，且**恰好**是那五个。
    ///
    /// 写成遍历而不是 `assert!(X.is_terminal())`：后者只能证明"多算了一个"，
    /// 证明不了"少算了一个"——而少算一个的后果是任务已经结束了、
    /// 界面却还在转圈等下一步。
    #[test]
    fn exactly_five_states_are_terminal() {
        let terminal: Vec<&str> = TaskState::ALL
            .iter()
            .filter(|state| state.is_terminal())
            .map(|state| state.as_str())
            .collect();
        assert_eq!(
            terminal,
            vec!["Completed", "Prepared", "NeedsHumanReview", "Failed", "Cancelled"],
            "终态集合变了——先想清楚「界面该不该停止等待」，再改这条断言"
        );
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
