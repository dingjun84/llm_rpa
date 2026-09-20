//! 故障注入：让测试可以脚本化地制造端口失败。

use automation_core::AutomationError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    ClientNotReady,
    ScreenChanged,
    AmbiguousVision(String),
    NeedsHumanReview(String),
    Platform(String),
    Timeout(String),
}

impl Fault {
    pub fn ambiguous(text: impl Into<String>) -> Self {
        Self::AmbiguousVision(text.into())
    }

    pub fn needs_human(text: impl Into<String>) -> Self {
        Self::NeedsHumanReview(text.into())
    }

    pub fn platform(text: impl Into<String>) -> Self {
        Self::Platform(text.into())
    }

    pub fn timeout(text: impl Into<String>) -> Self {
        Self::Timeout(text.into())
    }

    pub fn into_error(self) -> AutomationError {
        match self {
            Self::ClientNotReady => AutomationError::ClientNotReady,
            Self::ScreenChanged => AutomationError::ScreenChanged,
            Self::AmbiguousVision(text) => AutomationError::AmbiguousVision(text),
            Self::NeedsHumanReview(text) => AutomationError::NeedsHumanReview(text),
            Self::Platform(text) => AutomationError::Platform(text),
            Self::Timeout(text) => AutomationError::Timeout(text),
        }
    }
}

/// 各端口按次注入的故障计划。
#[derive(Debug, Default)]
pub struct MockFaults {
    pub launch: Option<Fault>,
    pub focus: Option<Fault>,
    pub metrics: Option<Fault>,
    /// 调整窗口尺寸。单独一格：它和 `focus` 是两件事，
    /// "定位到了但调不动"与"根本定位不到"要能分开测。
    pub resize: Option<Fault>,
    /// 按调用顺序注入的截图故障；用尽后恢复正常。
    pub capture: std::collections::VecDeque<Fault>,
    pub click: Option<Fault>,
    pub paste: Option<Fault>,
    /// 逐字输入。与 `paste` 分开：两者是两条不同的输入路径，
    /// 合成一个的话，"粘贴失败了"与"逐字输入失败了"就分不出来。
    pub type_text: Option<Fault>,
    /// 清空输入框。与 `type_text` 分开：清空失败时输入仍然可能成功，
    /// 而后果是"新词接在旧词后面"——合成一格就测不出这个区别了。
    pub clear: Option<Fault>,
    pub send: Option<Fault>,
}

impl MockFaults {
    pub fn with_capture(mut self, faults: impl IntoIterator<Item = Fault>) -> Self {
        self.capture = faults.into_iter().collect();
        self
    }
}
