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
    /// 在**联系人资料页**上核验姓名。
    ///
    /// 搜索式查找的落点不是聊天页，而是资料页——它是"选中这个人"与
    /// "进入与他的聊天"之间的一个独立界面。单独设一个状态，是因为后面所有动作
    /// （滚到资料页底部、找「发消息」入口、点它）都建立在
    /// "资料页上显示的就是要找的那个人"之上；混在 `VerifyingCandidate` 里的话，
    /// 出问题时看不出到底是"选错了人"还是"资料页根本没打开"。
    VerifyingProfile,
    /// 在资料页里找到进入聊天的入口并点击它。
    ///
    /// 和 [`TaskState::NavigatingToView`] 同类：它是一个**会改变界面内容**的动作
    /// （资料页 → 聊天页），点完之后所有的截图与识别都必须建立在**新画面**上。
    OpeningChatFromProfile,
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
    /// 终态：只完成了"找到导航图标并点击它"，没有打开任何会话、也没有发消息。
    ///
    /// 用于**单独验证导航那一步**（找联系人图标 / 聊天历史图标）。
    /// 它和 [`TaskState::Prepared`] 一样是**正常结束**，不是失败：
    /// 流程按配置在该停的地方停下了，没有发生任何意外。
    Navigated,
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
    pub const ALL: [TaskState; 19] = [
        Self::Draft,
        Self::LaunchingClient,
        Self::WaitingForClient,
        Self::NavigatingToView,
        Self::SearchingContact,
        Self::VerifyingCandidate,
        Self::VerifyingProfile,
        Self::OpeningChatFromProfile,
        Self::VerifyingChatHeader,
        Self::PreparingMessage,
        Self::AwaitingHumanConfirmation,
        Self::Sending,
        Self::VerifyingDelivery,
        Self::Completed,
        Self::Prepared,
        Self::Navigated,
        Self::NeedsHumanReview,
        Self::Failed,
        Self::Cancelled,
    ];

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Prepared
                | Self::Navigated
                | Self::NeedsHumanReview
                | Self::Failed
                | Self::Cancelled
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
            Self::VerifyingProfile => "VerifyingProfile",
            Self::OpeningChatFromProfile => "OpeningChatFromProfile",
            Self::VerifyingChatHeader => "VerifyingChatHeader",
            Self::PreparingMessage => "PreparingMessage",
            Self::AwaitingHumanConfirmation => "AwaitingHumanConfirmation",
            Self::Sending => "Sending",
            Self::VerifyingDelivery => "VerifyingDelivery",
            Self::Completed => "Completed",
            Self::Prepared => "Prepared",
            Self::Navigated => "Navigated",
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
            "VerifyingProfile" => Self::VerifyingProfile,
            "OpeningChatFromProfile" => Self::OpeningChatFromProfile,
            "VerifyingChatHeader" => Self::VerifyingChatHeader,
            "PreparingMessage" => Self::PreparingMessage,
            "AwaitingHumanConfirmation" => Self::AwaitingHumanConfirmation,
            "Sending" => Self::Sending,
            "VerifyingDelivery" => Self::VerifyingDelivery,
            "Completed" => Self::Completed,
            "Prepared" => Self::Prepared,
            "Navigated" => Self::Navigated,
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
            Self::VerifyingProfile => "正在核验联系人资料",
            Self::OpeningChatFromProfile => "正在从资料页打开聊天",
            Self::VerifyingChatHeader => "正在核验聊天页标题",
            Self::PreparingMessage => "正在准备消息",
            Self::AwaitingHumanConfirmation => "等待人工确认",
            Self::Sending => "正在发送",
            Self::VerifyingDelivery => "正在核验送达",
            Self::Completed => "已完成",
            Self::Prepared => "已填入正文，未发送",
            Self::Navigated => "已切换视图，未打开会话",
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
                // 「只做导航」这一条路：点完图标就结束，不查找任何人。
                | (TaskState::NavigatingToView, TaskState::Navigated)
                | (TaskState::SearchingContact, TaskState::VerifyingCandidate)
                // 两条查找路径在这里分叉，两条边都必须允许：
                //   - 列表扫描式：候选之后直接核验聊天标题（选中即打开会话）；
                //   - 搜索框式：候选之后先落在**资料页**，再从那儿的入口进聊天。
                // 只留一条会把另一种查找方式变成一次非法转换。
                | (TaskState::VerifyingCandidate, TaskState::VerifyingChatHeader)
                | (TaskState::VerifyingCandidate, TaskState::VerifyingProfile)
                | (TaskState::VerifyingProfile, TaskState::OpeningChatFromProfile)
                // 搜索式那一次点击**未必**落在资料页：目标已经有会话时，
                // 客户端会直接打开那份聊天记录（见 `runner/search.rs` 的
                // `open_chat_from_dropdown`），于是从核验资料页直接跳到核验标题。
                // 少这条边，这种落点会在半路报"非法转换"，而现象看起来
                // 像是流程坏了，看不出是"界面本来就长这样"。
                | (TaskState::VerifyingProfile, TaskState::VerifyingChatHeader)
                | (TaskState::OpeningChatFromProfile, TaskState::VerifyingChatHeader)
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
mod tests;
