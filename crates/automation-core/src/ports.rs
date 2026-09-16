//! 平台、视觉与确认端口。
//!
//! 业务流程只依赖这些端口，不得直接调用 Win32、OCR SDK 或鼠标键盘库。
//!
//! ## 坐标约定
//!
//! - [`Rect`] / [`Point`] 在**屏幕坐标系**下表示绝对像素位置；
//! - [`DesktopPlatform::capture`] 接收屏幕坐标系的区域，调用方不得传入窗口外的区域；
//! - [`TextBox::bounds`] 位于**截图图像坐标系**（原点为该截图的左上角），
//!   由核心层通过 `区域原点 + 图像坐标` 换算为屏幕坐标，OCR 实现无需感知屏幕偏移。

use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub type TaskId = Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    /// 区域中心点，用于把 OCR 文本框转换为可点击坐标。
    pub fn center(&self) -> Point {
        Point { x: self.x + self.width / 2, y: self.y + self.height / 2 }
    }

    /// 把截图图像坐标系下的矩形换算为屏幕坐标。
    pub fn to_screen(&self, region_origin: Point) -> Rect {
        Rect {
            x: region_origin.x + self.x,
            y: region_origin.y + self.y,
            width: self.width,
            height: self.height,
        }
    }

    pub fn is_degenerate(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }
}

#[derive(Debug, Clone)]
pub struct Screenshot {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub captured_at: SystemTime,
    /// 用于把证据与具体一次截图绑定；不参与任何相等性判断。
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextBox {
    pub text: String,
    pub bounds: Rect,
    pub confidence: f32,
}

/// 当前显示器的分辨率与缩放比例，用于点击前的标定一致性校验。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScreenMetrics {
    pub width: u32,
    pub height: u32,
    pub scale_factor: f32,
}

#[derive(Debug, Error)]
pub enum AutomationError {
    #[error("企业微信窗口不可用或不是前台窗口")]
    ClientNotReady,
    #[error("屏幕状态在操作前发生变化")]
    ScreenChanged,
    #[error("本地视觉识别结果不确定：{0}")]
    AmbiguousVision(String),
    #[error("需要人工处理：{0}")]
    NeedsHumanReview(String),
    #[error("平台操作失败：{0}")]
    Platform(String),
    #[error("任务已被取消")]
    Cancelled,
    #[error("步骤超时：{0}")]
    Timeout(String),
}

impl AutomationError {
    /// 审计用的稳定失败代码，不随提示文案变化。
    pub fn code(&self) -> &'static str {
        match self {
            Self::ClientNotReady => "CLIENT_NOT_READY",
            Self::ScreenChanged => "SCREEN_CHANGED",
            Self::AmbiguousVision(_) => "AMBIGUOUS_VISION",
            Self::NeedsHumanReview(_) => "NEEDS_HUMAN_REVIEW",
            Self::Platform(_) => "PLATFORM_ERROR",
            Self::Cancelled => "CANCELLED",
            Self::Timeout(_) => "TIMEOUT",
        }
    }

    /// 该错误是否应转入人工处理，而不是判定为失败。
    ///
    /// 依据 `docs/architecture.md` §5：超时、失焦、窗口被替换、
    /// OCR 结果冲突、风控或登录界面出现时转 `NeedsHumanReview`。
    pub fn requires_human_review(&self) -> bool {
        match self {
            Self::ClientNotReady
            | Self::ScreenChanged
            | Self::AmbiguousVision(_)
            | Self::NeedsHumanReview(_)
            | Self::Timeout(_) => true,
            Self::Platform(_) | Self::Cancelled => false,
        }
    }

    /// 是否属于可重试的瞬时错误。
    ///
    /// 只有平台层 I/O 与超时可重试；识别结果不确定、窗口状态异常、
    /// 需要人工判断的情况一律不重试，避免"越重试越错"。
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Platform(_) | Self::Timeout(_))
    }
}

/// 真实实现只可对用户可见、已解锁的交互式桌面执行操作。
pub trait DesktopPlatform: Send + Sync {
    /// 启动已由用户配置且经验证的企业微信可执行文件；不得猜测路径或提权启动。
    fn launch_wecom(&self) -> Result<(), AutomationError>;

    /// 将已验证的企业微信窗口置于前台，并返回它在屏幕上的边界。
    fn focus_wecom(&self) -> Result<Rect, AutomationError>;

    /// 读取当前显示器分辨率与缩放比例，供点击前的标定校验使用。
    fn screen_metrics(&self) -> Result<ScreenMetrics, AutomationError>;

    /// 捕获指定屏幕区域。调用方不得传入企业微信窗口外的区域。
    fn capture(&self, region: Rect) -> Result<Screenshot, AutomationError>;

    /// 点击前验证当前前台窗口与预期窗口一致；不满足则拒绝输入。
    fn guarded_click(&self, target: Point, expected_window: Rect) -> Result<(), AutomationError>;

    /// 将文本写入剪贴板并粘贴到当前已聚焦控件；完成后清除临时剪贴板内容。
    fn paste_text(&self, text: &str, expected_window: Rect) -> Result<(), AutomationError>;

    /// 发送由配置限定的快捷键；不支持任意按键序列。
    fn send_message_shortcut(&self, expected_window: Rect) -> Result<(), AutomationError>;
}

/// OCR 实现必须仅使用本地模型和本机内存中的图像。
pub trait LocalOcr: Send + Sync {
    fn recognize(&self, image: &Screenshot) -> Result<Vec<TextBox>, AutomationError>;
}

pub trait ContactMatcher: Send + Sync {
    fn find_unique_exact_match(
        &self,
        expected_name: &str,
        candidates: &[TextBox],
        min_confidence: f32,
    ) -> Result<TextBox, AutomationError>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendTask {
    pub id: TaskId,
    pub external_contact_name: String,
    pub text: String,
    pub created_by: String,
}

pub trait HumanConfirmation: Send + Sync {
    fn confirm_send(&self, task: &SendTask, expires_in: std::time::Duration)
        -> Result<(), AutomationError>;
}

/// 失败证据记录端口。
///
/// 实现方**必须**先做局部裁切与脱敏（至少遮盖已识别出的文字区域），
/// 只允许把脱敏后的画面落盘；不得保存整屏原图、消息正文或剪贴板内容。
pub trait EvidenceRecorder: Send + Sync {
    fn record(&self, task_id: TaskId, label: &str, frame: &Screenshot, text_boxes: &[TextBox]);
}
