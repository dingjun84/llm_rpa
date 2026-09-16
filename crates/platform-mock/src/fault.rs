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
    /// 按调用顺序注入的截图故障；用尽后恢复正常。
    pub capture: std::collections::VecDeque<Fault>,
    pub click: Option<Fault>,
    pub paste: Option<Fault>,
    pub send: Option<Fault>,
}

impl MockFaults {
    pub fn with_capture(mut self, faults: impl IntoIterator<Item = Fault>) -> Self {
        self.capture = faults.into_iter().collect();
        self
    }
}
