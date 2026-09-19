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

/// 一张用于**模板匹配**的小图。
///
/// 像素表示与 [`Screenshot`] 完全一致：BGRA、自上而下、32 位。
///
/// ## 为什么模板必须由人给
///
/// 模板决定了"程序会去点哪儿"。如果让程序自己"顺手从画面上裁一块"当模板，
/// 那么裁错位置、裁到了空白，都会变成一次**静默的、看起来一切正常的**运行——
/// 匹配分数照样很高（因为它匹配的是它自己刚裁的那块），点击照样发出去，
/// 只是点到了别的地方。所以模板只能来自操作者确认过的那张图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconTemplate {
    /// 人类可读名称（一般就是文件名），只用于日志与失败信息。
    pub label: String,
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// 一次模板匹配的命中结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IconMatch {
    /// 命中位置，位于**传入截图的图像坐标系**（与 [`TextBox::bounds`] 同一套语义），
    /// 由调用方加上区域原点换算成屏幕坐标。
    pub bounds: Rect,
    /// 归一化相关系数，`-1.0 ~ 1.0`。1.0 = 完全一致。
    pub score: f32,
    /// 命中的是第几个模板（对应传入的 `templates` 下标）。
    pub template_index: usize,
    pub template_label: String,
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
    ///
    /// **不属于任务流程**：客户端由操作者自己启动并登录，编排器不再调用它。
    /// 保留这个端口是为了界面上的「启动客户端」按钮——那是一个显式的、人点的动作，
    /// 与"任务跑到一半自己去拉一个程序起来"是两回事。
    fn launch_wecom(&self) -> Result<(), AutomationError>;

    /// 将已验证的企业微信窗口置于前台，并返回它在屏幕上的边界。
    fn focus_wecom(&self) -> Result<Rect, AutomationError>;

    /// 系统是否认为目标窗口**正在响应**（界面线程有没有在取消息）。
    ///
    /// 用途是"动作之前先确认客户端没卡死"：往一个卡死的窗口里点击、粘贴、回车，
    /// 什么都不会发生，而调用方从返回值上完全看不出区别——最坏的结果是
    /// "以为发出去了，其实一个字都没进去"。
    ///
    /// 约定：`Ok(true)` = 正在响应，`Ok(false)` = 未响应，`Err` = 无法判定
    /// （例如还没定位到窗口）。实现方不得为了让调用方"继续跑"而吞掉定位失败。
    fn is_responsive(&self) -> Result<bool, AutomationError>;

    /// 读取当前显示器分辨率与缩放比例，供点击前的标定校验使用。
    fn screen_metrics(&self) -> Result<ScreenMetrics, AutomationError>;

    /// 捕获指定屏幕区域。调用方不得传入企业微信窗口外的区域。
    fn capture(&self, region: Rect) -> Result<Screenshot, AutomationError>;

    /// 点击前验证当前前台窗口与预期窗口一致；不满足则拒绝输入。
    fn guarded_click(&self, target: Point, expected_window: Rect) -> Result<(), AutomationError>;

    /// 在 `at` 处滚动鼠标滚轮，用于在联系人列表里向下翻找。
    ///
    /// `notches > 0` 表示**向下滚动内容**（看列表里更靠后的项），`< 0` 表示向上。
    /// 实现方需要先把光标移到 `at`（滚轮事件只送给光标下的窗口），
    /// 并和点击一样在动作前验证前台窗口与标定一致。
    ///
    /// 滚动不会误触收件人，但会改变界面内容，因此它**不是**只读操作：
    /// 调用方必须在滚动后重新截图识别，不能复用滚动前的结果。
    fn scroll(
        &self,
        at: Point,
        notches: i32,
        expected_window: Rect,
    ) -> Result<(), AutomationError>;

    /// 将文本写入剪贴板并粘贴到当前已聚焦控件；完成后清除临时剪贴板内容。
    fn paste_text(&self, text: &str, expected_window: Rect) -> Result<(), AutomationError>;

    /// 发送由配置限定的快捷键；不支持任意按键序列。
    fn send_message_shortcut(&self, expected_window: Rect) -> Result<(), AutomationError>;
}

/// OCR 实现必须仅使用本地模型和本机内存中的图像。
pub trait LocalOcr: Send + Sync {
    fn recognize(&self, image: &Screenshot) -> Result<Vec<TextBox>, AutomationError>;
}

/// 图标定位端口：在一帧**局部截图**里用模板匹配找出一个小图的位置。
///
/// 与 [`LocalOcr`] 的分工很清楚——OCR 回答"这一片文字写的是什么"，
/// 本端口回答"这个图标在哪儿"。两者都是本地的、都只吃局部截图、都不联网。
///
/// ## 为什么要有它
///
/// 靠 OCR 认字来找入口有个结构性弱点：图标**根本没有文字**。
/// 左侧导航栏那排图标在 OCR 眼里是空白的，于是"先切到通讯录再找联系人"
/// 这件事无从表达。模板匹配补的正是这一段：图标是固定的像素图案，
/// 拿它跟画面比一比就知道在哪。
///
/// ## 约定
///
/// - `templates` 为空 ⇒ 实现方**必须报错**，不得当成"没找到"静默通过；
/// - 最高分低于 `min_score` ⇒ 返回 [`AutomationError::AmbiguousVision`]
///   （识别不确定 ⇒ 转人工），**不得**返回一个"分数不高但先用了"的结果。
///   本项目不接受"凑合着点"：点错图标的代价是后面整条流程都作用在错误的界面上；
/// - 返回的 `bounds` 是图像坐标系，由调用方换算成屏幕坐标。
///
/// ## 为什么是"一组模板取最高分"
///
/// 同一个图标在**选中 / 未选中**两种状态下长得不一样（选中态通常有高亮底色）。
/// 只留一张模板，就会出现"上一次运行点完停在这个页面上，这一次就再也匹配不上"。
/// 多张模板是这里唯一诚实的解法——而不是把阈值调低到"两个状态都能过"。
pub trait IconLocator: Send + Sync {
    fn locate(
        &self,
        frame: &Screenshot,
        templates: &[IconTemplate],
        min_score: f32,
    ) -> Result<IconMatch, AutomationError>;
}

pub trait ContactMatcher: Send + Sync {
    fn find_unique_exact_match(
        &self,
        expected_name: &str,
        candidates: &[TextBox],
        min_confidence: f32,
    ) -> Result<TextBox, AutomationError>;

    /// 单个候选是否**被本策略接受**为目标联系人。
    ///
    /// **为什么它必须由匹配器回答**：编排层在选定候选之后还要复检两次
    /// （`execute` 的「核验候选人」与「核验聊天页标题」）。那两处如果自己写一套
    /// 「文字是否等于目标名」，任何放宽/收紧都会被它们**静默挡回去**——
    /// 现象是「匹配器明明放宽了，任务照样转人工」，而且失败文案看起来像是
    /// 视觉识别不确定，排查时会一路往 OCR 上找，永远找不到。
    ///
    /// 判据只有一处权威：本方法。`find_unique_exact_match` 负责「在候选集里挑一个」，
    /// 本方法负责「这一个行不行」，两者必须对同一个名字给出同样的答案。
    fn accepts(&self, expected_name: &str, candidate: &TextBox) -> bool;
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
