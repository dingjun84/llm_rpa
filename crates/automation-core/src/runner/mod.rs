//! 任务编排：用注入的端口驱动 [`TaskMachine`] 走完整条 MVP 流程。
//!
//! 设计约束（见 `docs/architecture.md` §2、§5）：
//!
//! - **先验证、后操作**：每次可能影响收件人的操作前都要拿到可观测证据；
//! - **默认安全失败**：不确定即失败，绝不根据模糊 OCR 结果发送；
//! - **状态转换必须经过 [`TaskMachine`]**，不允许绕过转换校验。
//!
//! ## 关于超时
//!
//! 端口是同步的，因此超时是**协作式**的：每个步骤返回后校验是否超期。
//! 阻塞在端口内部的调用无法被抢占，平台层需要为自己的阻塞操作设置内部超时。

mod list;
mod message;
mod navigate;
mod search;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::audit::{AuditEntry, AuditSink, MemorySendLedger, MessageDigest, NoopAudit, SendLedger};
use crate::ports::{
    AutomationError, ContactMatcher, DesktopPlatform, EvidenceRecorder, HumanConfirmation,
    IconLocator, IconPrior, IconQuery, IconTemplate, LocalOcr, Point, Rect, ScreenMetrics,
    Screenshot, SendTask, TaskId, TextBox,
};
use crate::regions::{RelativePoint, RelativeRegion};
use crate::state::{TaskMachine, TaskState};

/// 默认的最低 OCR 置信度。
pub const DEFAULT_MIN_CONFIDENCE: f32 = 0.85;

/// 取消令牌。可在任意线程置位，编排器在每个步骤边界检查。
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    fn check(&self) -> Result<(), AutomationError> {
        if self.is_cancelled() {
            return Err(AutomationError::Cancelled);
        }
        Ok(())
    }
}

/// 一次状态变更。
#[derive(Debug, Clone)]
pub struct StateChange {
    pub task_id: TaskId,
    pub from: TaskState,
    pub to: TaskState,
    pub at: SystemTime,
    pub detail: Option<String>,
    /// 仅在该次转换是失败收敛时给出，与 `detail` 一起构成完整的失败信息。
    ///
    /// 之所以要和状态一起下发，是为了让界面在收到终态的那一刻就能同时
    /// 拿到失败代码与原因，而不是先看到"需要人工处理"、过一会儿才补上理由。
    pub failure_code: Option<String>,
}

pub trait ProgressSink: Send + Sync {
    fn state_changed(&self, change: &StateChange);
}

#[derive(Debug, Default)]
pub struct NoopProgress;

impl ProgressSink for NoopProgress {
    fn state_changed(&self, _change: &StateChange) {}
}

/// 标定时记录的窗口几何，只记**尺寸与缩放**，不记位置。
///
/// 为什么只比尺寸、不比位置：区域标定是相对窗口的比例，窗口挪到哪儿都不影响换算；
/// 而尺寸一变，同一个比例就落到了不同的实际像素上——真实界面**不是等比缩放的**
/// （会话列表宽度、输入框高度都是固定像素），于是四个区域会整体偏移。
///
/// 位置故意不记：操作者把窗口挪一下是再正常不过的事，为此拒绝执行只会让人困惑。
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CalibratedWindow {
    pub width: i32,
    pub height: i32,
    pub scale_factor: f32,
}

/// 窗口尺寸比较的容差（像素）。
///
/// 这不是"允许操作者随便改窗口大小"，而是给 DPI 取整留的余量：
/// 同一套吸附布局在不同缩放比下可能报出差 1 像素的矩形。
/// 1 像素对应的区域偏移远小于任何一次 OCR 定位误差，不会让标定失准。
const WINDOW_SIZE_TOLERANCE_PX: i32 = 1;

/// 显示器缩放比例比较的容差。
const SCALE_FACTOR_TOLERANCE: f32 = 0.01;

/// 联系人候选区的出厂默认值（相对窗口比例）。
///
/// ## 为什么左边界是 0.14 而不是 0.0
///
/// 微信 4.x 的会话列表左侧还有两条**不属于列表内容**的东西：导航图标栏
/// （约 0..57px）和头像列（约 78..129px，头像右上角还有未读红点）。
/// 左边界取 0.0 会把它们一起圈进来，而 `Windows.Media.Ocr` 会按**行**合并文字框——
/// 头像/红点与联系人姓名在同一行高度上，于是被并成一个块。
///
/// 2026-09-17 实测（960x734 的窗口，用 `winocr --upscale 2` 逐边界对比）：
///
/// | 左边界 | 读到的东西 |
/// |---|---|
/// | `0.00`（旧默认） | `《明月（美、加、欧洲）清库存` ← 红点被并进名字 |
/// | `0.115` | 仍有噪声块 `鳶`（屏幕 x 114–129） |
/// | `0.125` | 名字全部干净 |
/// | `0.14`（现值） | 名字全部干净 |
///
/// 名字文字实测起点是屏幕 x≈140，噪声最远到 x≈129 ⇒ 取两者中点 134px ≈ 0.14。
///
/// **代价**：这个结论是在「窗口宽 960」这个前提下量出来的，而真实 UI 不是等比缩放的
/// （导航栏与头像列都是固定像素宽，不随窗口变宽）。所以窗口尺寸一变，这个比例就要重标——
/// 好在 `calibrated_window` 已经把「一次运行只在一个尺寸下工作」钉死了，
/// 换了尺寸本来就必须重新标定。换窗口尺寸时**四个区域都要重标**，别只改这一个。
pub const DEFAULT_CONTACT_PANEL: RelativeRegion = RelativeRegion::new(0.14, 0.12, 0.28, 0.88);

/// 聊天页标题区出厂默认值。
pub const DEFAULT_CHAT_HEADER: RelativeRegion = RelativeRegion::new(0.28, 0.0, 0.72, 0.10);

/// 聊天正文区出厂默认值。
pub const DEFAULT_CHAT_BODY: RelativeRegion = RelativeRegion::new(0.28, 0.10, 0.72, 0.72);

/// 消息输入框区出厂默认值。
pub const DEFAULT_COMPOSER: RelativeRegion = RelativeRegion::new(0.28, 0.82, 0.72, 0.18);

/// 四个区域的出厂默认值，顺序固定为
/// `[联系人候选区, 聊天页标题区, 聊天正文区, 消息输入框区]`。
///
/// ⚠️ **Rust 侧的唯一权威来源**。`apps/desktop` 的 `RuntimeConfig::default()` 与
/// `screen_probe` 的兜底值都从这里取，不再各写一份字面量——此前三处各写一份，
/// 「改一处忘两处」是迟早的事（这类不同步不会报错，只会让界面显示的和实际跑的
/// 是两套标定值）。
///
/// 前端 `RegionCalibration.tsx` 里仍有一份**必须手工同步**的副本：
/// TypeScript 读不到 Rust 常量。改这四个值时务必一起改。
pub const DEFAULT_REGIONS: [RelativeRegion; 4] = [
    DEFAULT_CONTACT_PANEL,
    DEFAULT_CHAT_HEADER,
    DEFAULT_CHAT_BODY,
    DEFAULT_COMPOSER,
];

/// 图标模板匹配的默认最低分数。
///
/// ⚠️ 这是**推断值，尚未在真实图标上量过**。给出 0.80 的依据是归一化互相关的
/// 量级：同一台机器、同一个 DPI、同一个图标状态下的正确命中通常在 0.95 以上；
/// 而把另一个图标拿来比，分数一般落在 0.6 以下。0.80 落在这段空隙里。
///
/// 真实阈值要用界面的「测试图标匹配」按当前靶标量出来，不要照抄这个数——
/// 阈值定低了会点错图标，定高了会频繁转人工，两种代价都不小。
pub const DEFAULT_NAV_ICON_MIN_SCORE: f32 = 0.80;

/// 导航图标搜索区的出厂默认值（相对窗口比例）：**最左侧那条竖带、整高**。
///
/// 宽度的依据来自 2026-09-17 的实测（见 [`DEFAULT_CONTACT_PANEL`] 的表格）：
/// 微信 4.x 的左侧导航图标栏是 0–57px（固定像素宽），头像列从 78px 起。
/// 取 0.075 在 974 宽的窗口上是 73px——**让开了头像列，又给图标留了余量**。
///
/// 高度取满：图标在竖带里的纵向位置随版本变化，猜一个高度范围省下的那点时间
/// 换不来"猜错了就找不到图标"的风险。真要提速，把这一项调小即可——
/// 匹配开销与搜索面积成正比，高度减半就快一倍。
pub const DEFAULT_NAV_STRIP: RelativeRegion = RelativeRegion::new(0.0, 0.0, 0.075, 1.0);

/// 位置先验的默认分数容差。
///
/// 取 0.05 的依据：同一图标在选中 / 未选中 / 带气泡几种状态之间，
/// 实测分数差通常在这个量级以内；而"旁边那个图标"与目标的分数差要大得多。
/// 容差定大了会把低分命中抬上来（等于用位置替代了识别），
/// 定小了先验基本不生效——两种偏差的方向相反，取一个中间值并留在这里可调。
pub const DEFAULT_ICON_PRIOR_SCORE_TOLERANCE: f32 = 0.05;

/// 资料页里"进入聊天"入口上的默认文字。
///
/// 微信 4.x 的联系人资料页上，进入会话的按钮写着「发消息」。
/// 换成别的靶标（企业微信）时改这一项即可，不用改代码。
pub const DEFAULT_PROFILE_CHAT_ENTRY_TEXT: &str = "发消息";

/// 搜索下拉列表里"联系人"那一组的标题文字。
///
/// 下拉是**分组**的（联系人 / 聊天记录 / 群聊…），而只有"联系人"这一组
/// 下面才是人。这个标题是靶标相关的文字，所以做成配置而不是写死——
/// 写死的话，换一个客户端就会表现为"下拉里找不到联系人"。
pub const DEFAULT_SEARCH_CONTACT_GROUP_LABEL: &str = "联系人";

/// 会话列表滚动时的默认落点：**上下居中、左右偏右一点**。
///
/// 偏右而不是取正中心（`Rect::center`）：[`DEFAULT_CONTACT_PANEL`] 的左边界
/// 0.14 已经落在姓名文字那一列上，于是区域中心会压住姓名与消息预览的交界处。
/// 往右一点稳稳落在列表内容里，同时也离右边框的滚动条还有余量。
///
/// ⚠️ 这是相对**当前** `contact_panel` 的比例，改区域宽度就要重新想这个值。
///
/// 定义成常量而不是在 `Default` 里写字面量：桌面层的配置结构（`ScrollAnchorConfig`）
/// 也要拿它当默认值，各写一份的话，两边不一致时**不报任何错**——
/// 只表现为「界面上显示 0.62、任务用的是另一个数」。
pub const DEFAULT_SCROLL_ANCHOR: RelativePoint = RelativePoint::new(0.62, 0.5);

/// 资料页滚动时的默认落点：**几何正中**。
///
/// 与 [`DEFAULT_SCROLL_ANCHOR`]（会话列表那个 `(0.62, 0.5)`）不同，这里不需要
/// 往右偏：资料页是一整块可滚动内容，不存在"左边是头像列、右边是名字"
/// 这种要避开的列，正中一定落在内容上。
///
/// 定义成常量而不是在 `Default` 里写字面量：桌面层要拿它填 `RunnerConfig`
/// （那里的结构体字面量必须写全每一项），各写一份的话，
/// 「界面显示一个落点、任务用另一个」这种事就迟早会发生。
pub const DEFAULT_PROFILE_SCROLL_ANCHOR: RelativePoint = RelativePoint::new(0.5, 0.5);

/// 本次任务跑到哪一步。
///
/// ## 为什么要把它显式说出来
///
/// "找联系人"这件事有**两条完全不同的路**：一条是在顶部搜索框里打字、
/// 从联想下拉里挑人；另一条是在会话列表里往下滚、用 OCR 一行行认名字。
/// 两者适用的界面不同（前者要求搜索框能用，后者要求列表里滚得到人），
/// 而它们的失败现象都是"找不到联系人"——不把路分开，现场就分不清
/// 到底是搜索没生效还是列表里真的没有这个人。
///
/// 第三条路 `NavigateOnly` 是**只做导航**：它不查找任何人，
/// 用来单独验证"找图标 → 点它"这一步（找联系人图标 / 找聊天历史图标）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Workflow {
    /// 只做「找到导航图标并点击」：点完停在 [`TaskState::Navigated`]。
    NavigateOnly,
    /// 用顶部搜索框查找联系人，打开与他的聊天，把正文填进输入框后停下。
    ///
    /// 完整链路：点导航图标切视图 → 点搜索框 → 逐字输入姓名 →
    /// 在下拉列表的「联系人」分组里找到他并点击 → 核验资料页 →
    /// 资料页滚到底 → 点「发消息」→ 核验聊天标题 → 聚焦输入框 →
    /// 逐字输入正文 → 停在 [`TaskState::Prepared`]。
    SearchContact,
    /// 在**会话列表**里滚动扫描查找联系人，然后打开聊天、准备消息。
    ///
    /// 这是本项目最早实现的那条路，保留它是因为它和搜索式互补：
    /// 列表扫描不依赖搜索框能不能触发联想，而搜索式不依赖列表里滚得到人。
    ScrollListContact,
}

impl Workflow {
    /// 全部工作流，**顺序即界面上的顺序**。
    ///
    /// 与 [`TaskState::ALL`] 同一个道理：给界面用的枚举要有一个稳定的清单，
    /// 让界面遍历它而不是自己再列一遍——两边各列一份，加了新变体时
    /// 界面那一份不会报错，只会**少一个选项**，而少掉的那个没人会发现。
    pub const ALL: [Workflow; 3] = [
        Workflow::SearchContact,
        Workflow::ScrollListContact,
        Workflow::NavigateOnly,
    ];

    /// 面向操作者的名字，用于界面下拉与失败信息。
    ///
    /// 必须能**区分**三条路：它们的失败现象都是"找不到联系人"，
    /// 而处置方向完全不同（搜索没生效 / 列表里真没有 / 图标点错了）。
    /// 名字里带上区分点，比只写"查找联系人"有用得多。
    pub fn describe(self) -> &'static str {
        match self {
            Self::NavigateOnly => "只做导航（找到图标并点击）",
            Self::SearchContact => "搜索式查找联系人",
            Self::ScrollListContact => "列表扫描式查找联系人",
        }
    }
}

/// [`Workflow::NavigateOnly`] 要点的是哪一个导航图标。
///
/// 两个目标各有一组模板：它们在界面上长得不一样，模板不能通用。
/// 分组而不是塞进一个列表里，是因为"用错了哪一组"不会报错——
/// 只会拿聊天历史的模板去匹配联系人图标，然后转人工。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NavTarget {
    /// 联系人（通讯录）视图。
    Contact,
    /// 聊天历史（会话列表）视图。
    History,
}

impl NavTarget {
    /// 面向操作者的名字，用于失败信息与证据。
    pub fn describe(self) -> &'static str {
        match self {
            Self::Contact => "联系人",
            Self::History => "聊天历史",
        }
    }
}

/// 运行参数。
#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// 审计记录里标注的平台名称。
    pub platform_label: String,
    pub min_confidence: f32,
    /// 人工确认的有效期，过期后任务转入人工处理。
    pub confirmation_ttl: Duration,
    /// 单步超时（协作式，见模块文档）。
    pub step_timeout: Duration,
    /// 单个可重试步骤的最大尝试次数（含首次）。
    pub max_attempts: u32,
    pub retry_backoff: Duration,
    /// 联系人候选区（相对窗口）。
    pub contact_panel: RelativeRegion,
    /// 聊天页标题区（相对窗口）。
    pub chat_header: RelativeRegion,
    /// 聊天正文区（相对窗口），用于送达核验。
    pub chat_body: RelativeRegion,
    /// 消息输入框区（相对窗口）。
    pub composer: RelativeRegion,
    /// 「只填不发」：把正文填进输入框后停在 [`TaskState::Prepared`]，绝不发送。
    ///
    /// 用于验证"定位 + 输入"这条链路是否准确，而不产生任何对外影响。
    /// 打开后流程**不会**进入 `Sending`，因此也不会写发送台账、不会做送达核验。
    pub stop_before_send: bool,
    /// 查找联系人时最多向下滚动多少次。
    ///
    /// 必须有界：滚动是唯一一个"反复动作直到成功"的环节，
    /// 没有上限就会变成对着一个永远找不到的名字一直滚下去。
    pub max_scroll_attempts: u32,
    /// 每次向下滚动的格数。
    pub scroll_notches_per_step: i32,
    /// 滚动时鼠标落在联系人列表内的位置（相对该区域的比例）。
    ///
    /// 默认 `(0.62, 0.5)` = **上下居中、左右偏右一点**（2026-09-17 操作者指定）。
    /// 偏右而不是取正中心（`Rect::center`）：`DEFAULT_CONTACT_PANEL` 的左边界
    /// 0.14 已经落在姓名文字那一列上，于是区域中心会压住姓名与消息预览的交界处。
    /// 往右一点稳稳落在列表内容里，同时也离右边框的滚动条还有余量
    /// （滚轮落到滚动条上是另一回事）。
    ///
    /// **注意**：这是相对**当前** `contact_panel` 的比例，改区域宽度就要重新想这个值——
    /// 同一组比例落在不同宽度的区域上，像素位置差得很远（实测：区域宽度从 0.28
    /// 改到 0.46，落点从 x=178 跳到 x=273）。日志里那行「滚动落点」就是为此存在的。
    pub scroll_anchor: RelativePoint,
    /// 标定时记录的窗口尺寸与显示器缩放。
    ///
    /// `None` = 不校验（演练模式，或者还没记录过）。
    /// 真实模式下由命令层保证它一定被填上：客户端由操作者手动启动并登录，
    /// 任务只在**标定时的那个尺寸**下工作——尺寸对不上会先自动把窗口调回标定尺寸，
    /// 调不动（客户端有最小尺寸限制）才转人工，绝不按错的尺寸去点。
    /// 见 [`Run::ensure_calibrated_size`]。
    pub calibrated_window: Option<CalibratedWindow>,
    /// 联系人列表最多**完整**扫描几轮（每轮 = 从列表顶部一路向下扫到底）。
    ///
    /// 为什么需要多轮：列表按"最近有消息"排序，扫描过程中到达的新消息会把目标
    /// 顶到列表最上面，而那一屏早就被翻过去了——只扫一轮就必然漏掉。
    /// 一轮扫完回顶再扫一轮，就是专门兜这种情况的。代价是每多一轮多一遍滚动。
    pub max_search_sweeps: u32,
    /// 是否做"该动的画面没动 ⇒ 判定客户端卡死"的检测。
    ///
    /// 关掉它等于允许程序继续往一个可能已经卡死的客户端里粘贴和回车——
    /// 保留这个开关只是为了在现场排查误判时能临时绕过，默认必须开着。
    pub liveness_check: bool,
    /// 滚动之后**等画面停稳**再截图，最多等这么久。`0` = 不等。
    ///
    /// **为什么需要**：`platform.scroll` 把滚轮事件发出去就返回了，而客户端的
    /// 列表滚动**是带缓动的动画**。紧接着截屏很可能截到动画中间那一帧——
    /// 文字是糊的、行是错位的，OCR 读出来自然也是乱的。操作者 2026-09-17
    /// 看过一次运行后说「画太快了」，指的就是这里。
    ///
    /// 还有一个更隐蔽的后果：`view_moves_when_scrolling` 用「滚动前后指纹是否相同」
    /// 判断「这个方向到底还有没有效果」。截得太早、重绘还没发生 ⇒ 两帧相同 ⇒
    /// **把「还没画完」误判成「已经到底」**，于是整轮扫描提前收工、白白漏掉半屏。
    ///
    /// 为什么是「轮询到画面稳定」而不是 `sleep(一个固定值)`：缓动时长随机器、
    /// 负载、列表长度变，写死一个数必然在某些机器上偏短、在另一些机器上白等。
    /// 这里是**有界轮询**——连续两帧指纹相同就立刻继续，最多等到本值，
    /// 等不到也照常往下走。这是「等结算」，不是「重试到成功」。
    pub scroll_settle_timeout: Duration,
    /// 是否把每轮 OCR **实际读到的文字**写进过程证据（任务日志 + 界面）。默认开着。
    ///
    /// **为什么需要**：原来只记 `contact_panel#<指纹>`，它只能证明「看过这一帧」，
    /// 证明不了「看成了什么」。于是「找不到联系人」永远分不清两种原因——
    /// ① OCR 把名字读错了（识别问题，要调放大倍数或把窗口调大）；
    /// ② 名字确实不在这一屏（范围问题，要换列表或重做区域标定）。
    /// 两者的处置方式完全相反，而现场只能靠猜。
    ///
    /// **隐私**：联系人姓名本来就已经在日志里了（开头的「目标联系人」一行），
    /// 这里没有引入新类别的数据；而且只写任务日志与界面，**不进审计库**。
    /// 不想要就把这一项关掉。
    pub log_ocr_candidates: bool,
    /// 是否在查找联系人之前，先用模板匹配点一下左侧导航图标把视图切过去。
    ///
    /// ## 为什么需要这一步
    ///
    /// 靠 OCR 认字找入口有个结构性弱点：**图标上没有文字**。左侧导航栏那排图标
    /// 在 OCR 眼里是空白的，于是"先切到联系人视图"这件事没法用文字表达。
    /// 模板匹配补的正是这一段——图标是固定的像素图案，比一比就知道它在哪。
    ///
    /// 打开之后，`WaitingForClient` 后面会多出一个 `NavigatingToView` 状态。
    /// 关掉就是原来的行为（直接进 `SearchingContact`）。
    ///
    /// ## 打开时必须配模板
    ///
    /// 为 `true` 而 `nav_icon_templates` 为空，是**配置错误**：
    /// 装配层（`apps/desktop` 的 `build_runner`）会在任务登记之前就拒绝，
    /// 不让它变成一条"跑到一半才发现没模板"的失败记录。
    pub navigate_before_search: bool,
    /// 导航图标模板。**可以多张**——同一个图标在选中 / 未选中两种状态下长得不一样。
    ///
    /// 只留一张模板，就会出现"上一次运行点完停在这个页面上，这一次再也匹配不上"。
    /// 多张模板是这里唯一诚实的解法，而不是把阈值调低到"两个状态都能过"。
    pub nav_icon_templates: Vec<IconTemplate>,
    /// 图标模板匹配的最低分数，低于它转人工。见 [`DEFAULT_NAV_ICON_MIN_SCORE`]。
    pub nav_icon_min_score: f32,
    /// 在窗口的哪个区域里找导航图标。见 [`DEFAULT_NAV_STRIP`]。
    pub nav_strip: RelativeRegion,
    /// 本次任务跑到哪一步。见 [`Workflow`]。
    pub workflow: Workflow,
    /// [`Workflow::NavigateOnly`] 要点的是哪一个图标。
    pub nav_target: NavTarget,
    /// **聊天历史**视图那个导航图标的模板。
    ///
    /// 与 [`Self::nav_icon_templates`]（联系人图标）分开：两个图标长得不一样，
    /// 混成一个列表会让"用错了哪一组"退化成一次分数不高的匹配，
    /// 而不是一个能一眼看出来的配置错误。
    pub history_icon_templates: Vec<IconTemplate>,
    /// 位置先验的分数容差，`0` = 关掉先验。见 [`IconPrior`]。
    ///
    /// 导航栏是一列纵向排列、彼此长得很像的图标，逐张模板取最高分时偶尔会出现
    /// "旁边那个图标分数略高一点"。而这件事有先验可用：**越靠近导航区中心的
    /// 命中越可信**。容差决定"分数差多少以内才允许用位置来取舍"。
    pub icon_prior_score_tolerance: f32,
    /// 导航区（左侧那一列图标）的标定区域。
    ///
    /// 只用来**算位置先验的中心**，不用于裁图——真正的搜索范围是 [`Self::nav_strip`]。
    /// `None` 时退回 `nav_strip` 的中心：它更宽，但中心仍在图标那一列上。
    pub nav_bar: Option<RelativeRegion>,
    /// 主界面顶部的搜索框（相对窗口）。搜索式查找的第一步就点它。
    pub main_search: Option<RelativeRegion>,
    /// 搜索框下面弹出的下拉列表（相对窗口）。
    pub search_dropdown: Option<RelativeRegion>,
    /// 联系人资料面板（相对窗口）。
    pub contact_profile: Option<RelativeRegion>,
    /// 资料页里"进入聊天"那个入口上的文字。见 [`DEFAULT_PROFILE_CHAT_ENTRY_TEXT`]。
    pub profile_chat_entry_text: String,
    /// 搜索下拉列表里"联系人"那一组的标题文字。
    /// 见 [`DEFAULT_SEARCH_CONTACT_GROUP_LABEL`]。
    pub search_contact_group_label: String,
    /// 资料页滚动时的落点（相对资料区）。默认正中。
    ///
    /// 与联系人列表的 `scroll_anchor` 不同：资料页是一整块可滚动内容，
    /// 不存在"左边是头像列、右边是名字"这种要避开的列，取几何中心即可。
    pub profile_scroll_anchor: RelativePoint,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            platform_label: std::env::consts::OS.to_string(),
            min_confidence: DEFAULT_MIN_CONFIDENCE,
            confirmation_ttl: Duration::from_secs(60),
            step_timeout: Duration::from_secs(10),
            max_attempts: 3,
            retry_backoff: Duration::from_millis(200),
            contact_panel: DEFAULT_CONTACT_PANEL,
            chat_header: DEFAULT_CHAT_HEADER,
            chat_body: DEFAULT_CHAT_BODY,
            composer: DEFAULT_COMPOSER,
            stop_before_send: false,
            max_scroll_attempts: 20,
            scroll_notches_per_step: 3,
            scroll_anchor: DEFAULT_SCROLL_ANCHOR,
            calibrated_window: None,
            // 两轮：一轮从当前位置扫到底，一轮回顶重扫。第二轮专门兜
            // "扫描期间新消息把目标顶到列表最上面"这种情况。
            max_search_sweeps: 2,
            liveness_check: true,
            // 600ms 的依据：微信 4.x 列表的缓动动画实测在几百毫秒内结束，
            // 而这是**上限**不是等待时长——画面一稳就立刻继续，正常只多花一帧。
            scroll_settle_timeout: Duration::from_millis(600),
            log_ocr_candidates: true,
            // 默认**关**：这一步要先用模板匹配认图标，而模板必须由操作者自己截、
            // 自己确认。默认打开等于让每个没配模板的人都撞上一次配置错误。
            navigate_before_search: false,
            nav_icon_templates: Vec::new(),
            nav_icon_min_score: DEFAULT_NAV_ICON_MIN_SCORE,
            nav_strip: DEFAULT_NAV_STRIP,
            // 默认走**搜索式**：它是操作者当下要的那条路，也是不依赖
            // "列表里滚得到人"的那条路。列表扫描式仍然可用，改这一项即可。
            workflow: Workflow::SearchContact,
            nav_target: NavTarget::Contact,
            history_icon_templates: Vec::new(),
            icon_prior_score_tolerance: DEFAULT_ICON_PRIOR_SCORE_TOLERANCE,
            nav_bar: None,
            // 下面这些新增区域**没有默认值**：它们对应的界面元素在哪儿，
            // 只有对着真实窗口框一次才知道。给一个猜出来的默认值，
            // 症状会是"任务照常跑完，只是点到了别的地方"。
            main_search: None,
            search_dropdown: None,
            contact_profile: None,
            profile_chat_entry_text: DEFAULT_PROFILE_CHAT_ENTRY_TEXT.to_string(),
            search_contact_group_label: DEFAULT_SEARCH_CONTACT_GROUP_LABEL.to_string(),
            // 资料页是一整块可滚动内容，几何中心一定落在内容上。
            profile_scroll_anchor: DEFAULT_PROFILE_SCROLL_ANCHOR,
        }
    }
}

/// 「等画面停稳」的轮询间隔 = `scroll_settle_timeout` / 这个数（再取下限）。
///
/// 取 8 的依据：一个超时窗口内最多采样 8 次，足够分辨「还在动」和「停下来了」；
/// 再密也只是重复截同一帧，截图本身就有开销。
const SETTLE_POLL_DIVISOR: u32 = 8;

/// 轮询间隔的下限：比这更密没有意义，一次截图本身就要几十毫秒。
const MIN_SETTLE_POLL: Duration = Duration::from_millis(20);

/// 由「等停稳」的超时推出**轮询间隔**。
///
/// 单独做成公开函数，是因为「等界面动画停下来」这件事不止编排器要做——
/// 界面上的「定位并点击」标定按钮同样要（点完得等重绘画完，才能回答"画面变了没有"）。
/// 两处各写一遍比例的话，改了一处另一处就悄悄不一致了：症状是标定按钮报"没变化"，
/// 而真实任务里同样的点击判定为"变化了"。
pub fn settle_poll_interval(timeout: Duration) -> Duration {
    (timeout / SETTLE_POLL_DIVISOR).max(MIN_SETTLE_POLL)
}

/// 一帧最多记多少个文字块、总共多少个字符（见 [`describe_candidates`]）。
///
/// 取值依据：联系人列表一屏最多也就 20 来行、每行名字 10~20 字，
/// 24 块 / 600 字符足够覆盖一整屏。再多出来的多半是乱码碎块。
const OCR_LOG_MAX_BLOCKS: usize = 24;
const OCR_LOG_MAX_CHARS: usize = 600;

/// 把一帧识别到的文字拼成一行，供事后回答「OCR 到底读成了什么」。
///
/// 为什么要截断：一帧乱码可能识别出上百个碎块，全写进日志会把真正有用的那几行
/// 淹掉。**截断会显式标出来**（写一个 `…`），不是悄悄丢掉——否则
/// 「本来只读到 24 块」和「读到 200 块、这里只显示 24 块」看起来一模一样，
/// 而这两种情况的含义完全不同。
fn describe_candidates(candidates: &[TextBox]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut used = 0usize;
    for item in candidates.iter().take(OCR_LOG_MAX_BLOCKS) {
        let text = item.text.trim();
        if text.is_empty() {
            continue;
        }
        // 识别结果里可能带换行，压成字面量 `\n`，保住日志「一行一条」的格式。
        let text = text.replace('\n', "\\n");
        let len = text.chars().count();
        if used + len > OCR_LOG_MAX_CHARS {
            parts.push("…".to_string());
            break;
        }
        used += len;
        parts.push(text);
    }
    if candidates.len() > OCR_LOG_MAX_BLOCKS {
        parts.push(format!(
            "（共 {} 块，只列前 {}）",
            candidates.len(),
            OCR_LOG_MAX_BLOCKS
        ));
    }
    parts.join(" / ")
}


/// 注入的端口集合。
pub struct RunnerPorts {
    pub platform: Arc<dyn DesktopPlatform>,
    pub ocr: Arc<dyn LocalOcr>,
    pub matcher: Arc<dyn ContactMatcher>,
    /// 图标定位（模板匹配）。只有 `navigate_before_search` 打开时才会被调用，
    /// 但**端口本身必须始终在场**：让它变成 `Option` 的话，"忘了装配"就会
    /// 在运行期变成一个 `unwrap` 或一次静默跳过，而不是装配期的报错。
    pub icons: Arc<dyn IconLocator>,
    pub confirmation: Arc<dyn HumanConfirmation>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Failure {
    pub code: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub task_id: TaskId,
    pub state: TaskState,
    pub failure: Option<Failure>,
    /// 证据引用（截图指纹等），不含图像与正文。
    pub evidence: Vec<String>,
}

impl RunOutcome {
    /// 是否**真的把消息发出去了**并完成了送达核验。
    ///
    /// 注意 [`TaskState::Prepared`] 不算成功发送——它是"按配置在发送前停下"，
    /// 属于正常结束，但没有发出任何东西。要判断这一点用
    /// [`RunOutcome::stopped_before_send`]。
    pub fn succeeded(&self) -> bool {
        self.state == TaskState::Completed
    }

    /// 是否按「只填不发」配置正常结束：正文已填入输入框，未发送。
    pub fn stopped_before_send(&self) -> bool {
        self.state == TaskState::Prepared
    }
}

pub struct WorkflowRunner {
    ports: RunnerPorts,
    audit: Arc<dyn AuditSink>,
    ledger: Arc<dyn SendLedger>,
    evidence: Option<Arc<dyn EvidenceRecorder>>,
    config: RunnerConfig,
}

impl WorkflowRunner {
    pub fn new(ports: RunnerPorts, config: RunnerConfig) -> Self {
        Self {
            ports,
            audit: Arc::new(NoopAudit),
            ledger: Arc::new(MemorySendLedger::new()),
            evidence: None,
            config,
        }
    }

    pub fn with_audit(mut self, audit: Arc<dyn AuditSink>) -> Self {
        self.audit = audit;
        self
    }

    pub fn with_ledger(mut self, ledger: Arc<dyn SendLedger>) -> Self {
        self.ledger = ledger;
        self
    }

    /// 注入失败证据记录器。未注入时不保存任何画面。
    pub fn with_evidence_recorder(mut self, recorder: Arc<dyn EvidenceRecorder>) -> Self {
        self.evidence = Some(recorder);
        self
    }

    pub fn config(&self) -> &RunnerConfig {
        &self.config
    }

    /// 同步执行一条发送任务。不会 panic，所有失败都收敛为终态。
    pub fn run(
        &self,
        task: &SendTask,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> RunOutcome {
        let mut run = Run::new(self, task, progress, cancel);
        if let Err(err) = run.execute() {
            run.settle(&err);
        }
        run.into_outcome()
    }
}

struct Run<'a> {
    runner: &'a WorkflowRunner,
    task: &'a SendTask,
    progress: &'a dyn ProgressSink,
    cancel: &'a CancelToken,
    machine: TaskMachine,
    step_started: Instant,
    window: Option<Rect>,
    metrics: Option<ScreenMetrics>,
    baseline_fingerprint: Option<String>,
    confirmation_at: Option<SystemTime>,
    message_digest: Option<MessageDigest>,
    /// 最近一次成功捕获的画面与其识别结果，仅在失败时用于生成脱敏证据。
    last_frame: Option<(Screenshot, Vec<TextBox>)>,
    /// 最近一次"会改变界面"的点击之后，画面到底变没变。
    ///
    /// 它回答的是一个**只能靠对比两帧**才知道的问题：点击是发出去了，
    /// 但它生效了吗？核验聊天标题失败时，这个标志决定失败信息指向
    /// "点错了人"还是"这次点击根本没落到界面上"——两者的处置完全相反。
    last_click_reacted: Option<bool>,
    evidence: Vec<String>,
    failure: Option<Failure>,
}

impl<'a> Run<'a> {
    fn new(
        runner: &'a WorkflowRunner,
        task: &'a SendTask,
        progress: &'a dyn ProgressSink,
        cancel: &'a CancelToken,
    ) -> Self {
        Self {
            runner,
            task,
            progress,
            cancel,
            machine: TaskMachine::default(),
            step_started: Instant::now(),
            window: None,
            metrics: None,
            baseline_fingerprint: None,
            confirmation_at: None,
            message_digest: None,
            last_frame: None,
            last_click_reacted: None,
            evidence: Vec::new(),
            failure: None,
        }
    }

    fn cfg(&self) -> &RunnerConfig {
        &self.runner.config
    }

    fn into_outcome(self) -> RunOutcome {
        RunOutcome {
            task_id: self.task.id,
            state: self.machine.state(),
            failure: self.failure,
            evidence: self.evidence,
        }
    }

    /// 记录一次状态转换：先过状态机校验，再通知进度，再写审计。
    fn advance(&mut self, to: TaskState, detail: Option<String>) -> Result<(), AutomationError> {
        let from = self.machine.state();
        if from.is_terminal() {
            return Ok(());
        }
        self.machine
            .transition(to)
            .map_err(|err| AutomationError::Platform(format!("状态机拒绝转换：{err}")))?;

        let at = SystemTime::now();
        self.progress.state_changed(&StateChange {
            task_id: self.task.id,
            from,
            to,
            at,
            detail: detail.clone(),
            failure_code: None,
        });

        self.runner.audit.record(&AuditEntry {
            task_id: self.task.id,
            actor: self.task.created_by.clone(),
            platform: self.cfg().platform_label.clone(),
            from,
            to,
            at,
            confirmation_at: self.confirmation_at,
            failure_code: None,
            failure_reason: detail,
            evidence: self.evidence.clone(),
            message: self.message_digest.clone(),
        })?;

        self.step_started = Instant::now();
        Ok(())
    }

    fn check_cancel(&self) -> Result<(), AutomationError> {
        self.cancel.check()
    }

    fn check_deadline(&self, step: &str) -> Result<(), AutomationError> {
        if self.step_started.elapsed() > self.cfg().step_timeout {
            return Err(AutomationError::Timeout(format!(
                "{step} 超过单步上限 {:?}",
                self.cfg().step_timeout
            )));
        }
        Ok(())
    }

    /// 把失败收敛为终态，并写入带失败代码的审计记录。
    fn settle(&mut self, err: &AutomationError) {
        let target = match err {
            AutomationError::Cancelled => TaskState::Cancelled,
            other if other.requires_human_review() => TaskState::NeedsHumanReview,
            _ => TaskState::Failed,
        };
        let code = err.code().to_string();
        let reason = err.to_string();
        self.failure = Some(Failure { code: code.clone(), reason: reason.clone() });

        // 失败时才落证据，且由记录器负责裁切与脱敏。
        if let (Some(recorder), Some((frame, boxes))) =
            (self.runner.evidence.as_ref(), self.last_frame.as_ref())
        {
            recorder.record(self.task.id, "failure", frame, boxes);
        }

        let from = self.machine.state();
        if from.is_terminal() {
            return;
        }
        if self.machine.transition(target).is_err() {
            return;
        }
        let at = SystemTime::now();
        self.progress.state_changed(&StateChange {
            task_id: self.task.id,
            from,
            to: target,
            at,
            detail: Some(reason.clone()),
            failure_code: Some(code.clone()),
        });
        // 结算阶段的审计失败不再改变任务终态，只记录在失败原因中。
        let _ = self.runner.audit.record(&AuditEntry {
            task_id: self.task.id,
            actor: self.task.created_by.clone(),
            platform: self.cfg().platform_label.clone(),
            from,
            to: target,
            at,
            confirmation_at: self.confirmation_at,
            failure_code: Some(code),
            failure_reason: Some(reason),
            evidence: self.evidence.clone(),
            message: self.message_digest.clone(),
        });
    }

    fn resolve(&self, region: RelativeRegion, label: &str) -> Result<Rect, AutomationError> {
        let window = self.window.ok_or(AutomationError::ClientNotReady)?;
        region.resolve_within(window).map_err(|err| {
            AutomationError::NeedsHumanReview(format!("{label} 区域标定无效：{err}"))
        })
    }



    /// 点击前守卫：重新确认前台窗口与显示器指标仍与标定一致。
    fn ensure_calibrated(&self) -> Result<Rect, AutomationError> {
        let expected = self.window.ok_or(AutomationError::ClientNotReady)?;
        let current = self.runner.ports.platform.focus_wecom()?;
        if current != expected {
            return Err(AutomationError::ScreenChanged);
        }
        let metrics = self.runner.ports.platform.screen_metrics()?;
        if Some(metrics) != self.metrics {
            return Err(AutomationError::ScreenChanged);
        }
        Ok(expected)
    }







    /// 等到联系人列表画面**连续两帧一致**，最多等 `scroll_settle_timeout`。
    ///
    /// `Ok(())` 只表示「等够了」——稳定了、或者超时了，两种情况都不算失败。
    /// 超时不算失败是刻意的：慢机器上宁可多花点时间，也不该因为「动画还没停」
    /// 就把一次本来正常的扫描判死。
    ///
    /// 轮询间隔由超时**推出来**（`timeout / SETTLE_POLL_DIVISOR`，下限
    /// [`MIN_SETTLE_POLL`]），不另外写死一个数——操作者把超时调大时采样点
    /// 跟着变密，不需要再同步改第二个参数。
    fn wait_for_settle(&mut self, panel: Rect) -> Result<(), AutomationError> {
        let timeout = self.cfg().scroll_settle_timeout;
        if timeout.is_zero() {
            return Ok(());
        }
        let interval = settle_poll_interval(timeout);
        let deadline = Instant::now() + timeout;
        let mut previous = self.capture_frame(panel, "联系人列表")?.fingerprint;
        while Instant::now() < deadline {
            self.check_cancel()?;
            std::thread::sleep(interval);
            let current = self.capture_frame(panel, "联系人列表")?.fingerprint;
            if current == previous {
                return Ok(());
            }
            previous = current;
        }
        Ok(())
    }


    /// 对可重试的瞬时错误重试，最多 `max_attempts` 次。
    ///
    /// 只有平台层 I/O 与超时会重试；识别结果不确定、窗口状态异常一律不重试
    /// （见 [`AutomationError::is_retryable`]），避免"越重试越错"。
    fn with_retry<T>(
        &self,
        step: &str,
        mut action: impl FnMut() -> Result<T, AutomationError>,
    ) -> Result<T, AutomationError> {
        let attempts = self.cfg().max_attempts.max(1);
        let mut last_err: Option<AutomationError> = None;
        for attempt in 1..=attempts {
            self.check_cancel()?;
            match action() {
                Ok(value) => return Ok(value),
                Err(err) => {
                    let retryable = err.is_retryable() && attempt < attempts;
                    last_err = Some(err);
                    if !retryable {
                        break;
                    }
                    if !self.cfg().retry_backoff.is_zero() {
                        std::thread::sleep(self.cfg().retry_backoff);
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| {
            AutomationError::Platform(format!("{step} 失败且没有返回错误信息"))
        }))
    }

    /// 只截一帧，不做 OCR。
    ///
    /// 为什么不复用 [`Self::capture_and_recognize`]：那个是给"要读文字"的场景用的，
    /// 会走一次 OCR（真实模式下是几百毫秒的独立进程调用），而"画面动没动"只要指纹。
    /// 更关键的是 OCR 端口是**有状态**的——测试替身按调用顺序产出结果——
    /// 多发一次 OCR 会把脚本顺序打乱，让断言失去意义。
    ///
    /// 刻意**不**更新 `last_frame`：失败证据记录器靠 OCR 文字框做遮盖，
    /// 这里没有文字框，把这一帧当证据会在盘上留下未脱敏的画面。
    fn capture_frame(&self, region: Rect, label: &str) -> Result<Screenshot, AutomationError> {
        self.with_retry(label, || self.runner.ports.platform.capture(region))
    }

    /// 截图 + 识别，仅对可重试的瞬时错误重试。
    fn capture_and_recognize(
        &mut self,
        region: Rect,
        step: &str,
    ) -> Result<(Screenshot, Vec<TextBox>), AutomationError> {
        // 截屏与识别放在**同一次尝试**里重试：两者任一失败都算这一次尝试失败。
        // 分开重试会让总尝试次数翻倍，也会让"截到了但识别炸了"这种情况
        // 白白多截几次图。
        let value = self.with_retry(step, || {
            let shot = self.runner.ports.platform.capture(region)?;
            let boxes = self.runner.ports.ocr.recognize(&shot)?;
            Ok((shot, boxes))
        })?;
        self.last_frame = Some(value.clone());
        Ok(value)
    }

    /// 卡死守卫：系统必须认为客户端窗口**正在响应**。
    ///
    /// 判据分两层，缺一不可：
    ///
    /// - **画面层**：该让画面动的动作没让它动；
    /// - **系统层**：`IsHungAppWindow` 认为窗口所属的 GUI 线程没在取消息。
    ///
    /// 只有画面层不够——一个装得下一屏的联系人列表本来就上下都滚不动，
    /// 只看画面会把"列表太短"误报成"客户端卡死"。
    /// 只看系统层也不够——消息循环活着但界面没刷新时，系统仍会认为它在响应。
    ///
    /// `what` 描述"因此被取消的动作"，用来告诉操作者程序停在了哪一步。
    fn ensure_not_frozen(&self, what: &str) -> Result<(), AutomationError> {
        if !self.cfg().liveness_check {
            return Ok(());
        }
        if self.runner.ports.platform.is_responsive()? {
            return Ok(());
        }
        Err(AutomationError::NeedsHumanReview(format!(
            "客户端疑似卡死：{what}，且系统判定该窗口未响应消息。\
             请确认程序没有卡住（必要时重启客户端并重新登录）后重试。"
        )))
    }

    /// 当前窗口尺寸与缩放是否与标定记录一致。
    ///
    /// **只比尺寸与缩放，不比位置**：标定记的是相对窗口的比例，窗口挪到哪儿
    /// 都不影响换算；而尺寸一变，同一个比例就落到不同的实际像素上——
    /// 真实界面不是等比缩放的（会话列表宽度、输入框高度都是固定像素）。
    fn size_matches(window: Rect, metrics: ScreenMetrics, expected: &CalibratedWindow) -> bool {
        (window.width - expected.width).abs() <= WINDOW_SIZE_TOLERANCE_PX
            && (window.height - expected.height).abs() <= WINDOW_SIZE_TOLERANCE_PX
            && (metrics.scale_factor - expected.scale_factor).abs() <= SCALE_FACTOR_TOLERANCE
    }

    /// 把窗口弄回标定时的尺寸；实在弄不回去才转人工。返回本次运行的窗口基准。
    ///
    /// 这是"按标定尺寸工作"这条约定的落地点。
    ///
    /// ## 为什么先自动调，而不是像以前那样直接停下
    ///
    /// 窗口尺寸是程序**完全能确定**的一件事——标定记录里就写着目标值。
    /// 把它调回去是确定性的、可逆的，不涉及任何对业务内容的猜测，
    /// 所以不属于"不确定即失败"要拦的那一类。停下来让操作者手工拖窗口，
    /// 只是把一件机器能做的事推给人做。
    ///
    /// ## 为什么调完还要再量一遍
    ///
    /// 客户端有自己的最小尺寸限制：请求值可能被应用自己夹住，
    /// `SetWindowPos` 会报成功而窗口并没有变成你要的尺寸。
    /// 所以判据是**量出来的**尺寸，不是请求值——量不中就转人工，
    /// 绝不"按偏了的区域"往下走。
    ///
    /// ## 缩放不一致时不尝试调整
    ///
    /// DPI 缩放是**显示器/系统**属性，不是窗口属性。缩放不同意味着同一物理尺寸下
    /// 的逻辑布局本来就不同，调物理像素解决不了——调完照样偏，只是白动一次窗口。
    fn ensure_calibrated_size(
        &mut self,
        window: Rect,
        metrics: ScreenMetrics,
    ) -> Result<Rect, AutomationError> {
        let Some(expected) = self.cfg().calibrated_window else {
            return Ok(window);
        };
        if Self::size_matches(window, metrics, &expected) {
            return Ok(window);
        }

        let scale_ok =
            (metrics.scale_factor - expected.scale_factor).abs() <= SCALE_FACTOR_TOLERANCE;
        if !scale_ok {
            return Err(AutomationError::NeedsHumanReview(format!(
                "显示器缩放与标定记录不一致：记录 {:.2}，当前 {:.2}。\
                 缩放不同意味着同一物理尺寸下的界面布局本来就不同，\
                 调整窗口尺寸解决不了——请把窗口移回标定时那块显示器，\
                 或重新点「记录窗口尺寸」并保存配置。",
                expected.scale_factor, metrics.scale_factor
            )));
        }

        // 尺寸不符、缩放一致 ⇒ 把窗口调回标定尺寸。成不成看**量出来的**结果。
        let adjusted = self
            .runner
            .ports
            .platform
            .resize_wecom(expected.width, expected.height)?;
        if Self::size_matches(adjusted, metrics, &expected) {
            self.evidence.push(format!(
                "自动调整窗口尺寸 : {}x{} → {}x{}（标定值）",
                window.width, window.height, adjusted.width, adjusted.height
            ));
            return Ok(adjusted);
        }

        Err(AutomationError::NeedsHumanReview(format!(
            "窗口尺寸与标定记录不一致，且自动调整没有生效：\
             标定 {}×{}，调整前 {}×{}，调整后 {}×{}。\
             客户端通常会限制最小窗口尺寸，标定值比它小时就会被夹住。\
             请把窗口手工调到 {}×{}，或重新点「记录窗口尺寸」并保存配置。",
            expected.width,
            expected.height,
            window.width,
            window.height,
            adjusted.width,
            adjusted.height,
            expected.width,
            expected.height
        )))
    }

    /// 接入客户端：接管操作者已经启动并登录的那个窗口，并把它记成本次运行的基准。
    ///
    /// 抽成独立方法是因为它和"跑到哪一步"无关——三条工作流都要先过这一关。
    fn enter_client(&mut self) -> Result<(), AutomationError> {
        // ── 接入客户端 ──────────────────────────────────────────────
        //
        // 客户端由操作者**自己启动并登录**，这里只负责接管已经就绪的窗口。
        // 界面上另有一个显式的「启动客户端」按钮，但那是人点的，不是流程的一步。
        //
        // 为什么不替操作者拉起程序：
        //
        // 1. 重复启动会弹出登录窗，而 Qt 系程序（微信 4.x 就是）**所有顶层窗口
        //    共用同一个类名**，窗口定位很容易选到那个登录窗；
        // 2. 登录本身是人的事——扫码、验证码、风控确认，程序插不上手；
        // 3. "刚启动还没登录完"和"已经就绪"从窗口外面看不出区别，
        //    与其猜一个等待时间，不如要求人先把这件事做完。
        self.advance(TaskState::LaunchingClient, None)?;
        let window = match self.runner.ports.platform.focus_wecom() {
            Ok(window) => window,
            // 平台层现在会给出**精确到哪一步**的说明（找不到窗口 / 置前被拒 /
            // 置前后前台仍未改变 / 矩形退化），原样透传——操作者要靠它决定下一步做什么。
            Err(AutomationError::NeedsHumanReview(reason)) => {
                return Err(AutomationError::NeedsHumanReview(reason));
            }
            // `ClientNotReady` 只是平台层没给出细节时的兜底。
            Err(AutomationError::ClientNotReady) => {
                return Err(AutomationError::NeedsHumanReview(
                    "未能接管目标窗口：找不到可见的目标窗口，或它无法被带到前台。\
                     请先手动启动并登录客户端，确认它的主窗口可见、未被最小化，然后重试。"
                        .into(),
                ));
            }
            Err(other) => return Err(other),
        };
        if window.is_degenerate() {
            return Err(AutomationError::NeedsHumanReview(
                "目标窗口尺寸为 0，不能作为标定基准。请先让客户端主窗口正常显示。".into(),
            ));
        }
        let metrics = self.runner.ports.platform.screen_metrics()?;
        // 先校验尺寸、再把它当成本次运行的基准：基准错了，后面每一次换算都错。
        // 尺寸不符时这里会**先尝试自动调回去**，调成了就用调完的尺寸当基准。
        let original = window;
        let window = self.ensure_calibrated_size(original, metrics)?;
        self.window = Some(window);
        self.metrics = Some(metrics);
        // 调整过就明说：窗口被程序动过，操作者有权知道动了多少。
        let adjusted_note = if window.width != original.width || window.height != original.height {
            format!(
                "（已自动从 {}x{} 调整到标定尺寸）",
                original.width, original.height
            )
        } else {
            String::new()
        };
        self.advance(
            TaskState::WaitingForClient,
            Some(format!(
                "窗口 {}x{} @({},{})，缩放 {}{}",
                window.width, window.height, window.x, window.y, metrics.scale_factor, adjusted_note
            )),
        )?;
        Ok(())
    }

    fn execute(&mut self) -> Result<(), AutomationError> {
        self.check_cancel()?;
        self.enter_client()?;

        let workflow = self.cfg().workflow;

        // ── 只做导航 ────────────────────────────────────────────────
        //
        // 这条路**不查找任何人**：找到指定图标、点它、结束。
        // 它的价值是把"图标匹配得准不准"从整条链路里单独拎出来验证——
        // 混在完整流程里时，点错图标的症状会表现为"找不到联系人"，
        // 而排查方向会一路偏向 OCR。
        if workflow == Workflow::NavigateOnly {
            let nav_target = self.cfg().nav_target;
            self.advance(
                TaskState::NavigatingToView,
                Some(format!("目标：{}图标", nav_target.describe())),
            )?;
            self.navigate_to_view(nav_target)?;
            self.advance(
                TaskState::Navigated,
                Some(format!("已找到并点击「{}」图标", nav_target.describe())),
            )?;
            return Ok(());
        }

        // ── 切换视图（可选）────────────────────────────────────────
        //
        // 图标上没有文字，OCR 读不到它，所以"先切到联系人视图"这一步只能靠
        // 模板匹配。它必须在查找之前——查找假定"现在看的就是目标视图"。
        if self.cfg().navigate_before_search {
            self.advance(TaskState::NavigatingToView, None)?;
            self.navigate_to_view(NavTarget::Contact)?;
        }

        // ── 查找联系人 ──────────────────────────────────────────────
        //
        // 两条路在状态机上同为 `SearchingContact`，但**看的是完全不同的界面**：
        // 搜索式看顶部的联想下拉，列表扫描式看左侧的会话列表。
        // 正因为界面不同，它们的失败原因也完全不同，所以走之前必须分开。
        self.advance(TaskState::SearchingContact, None)?;
        let matched = match workflow {
            Workflow::SearchContact => self.search_contact_by_keyword()?,
            _ => {
                let panel = self.resolve(self.cfg().contact_panel, "联系人候选区")?;
                // 列表一屏放不下时向下滚动继续找，找不到就转人工，绝不猜。
                self.locate_contact(panel)?
            }
        };

        // ── 核验候选人 ──────────────────────────────────────────────
        self.advance(
            TaskState::VerifyingCandidate,
            Some(format!("候选文字：{}", matched.text.trim())),
        )?;
        self.verify_candidate(&matched)?;

        // ── 打开与他的聊天 ──────────────────────────────────────────
        //
        // 两条路的落点不同，所以这一步必须分开：
        //   - 搜索式：点下拉里那一行之后落在**资料页**，还要从那儿点「发消息」；
        //   - 列表扫描式：点一下候选人就直接进了聊天页。
        match workflow {
            Workflow::SearchContact => {
                self.click_dropdown_row(&matched)?;
                self.verify_profile()?;
                self.open_chat_from_profile()?;
            }
            _ => self.open_chat_from_list(&matched)?,
        }

        // ── 核验聊天页标题 ──────────────────────────────────────────
        self.advance(TaskState::VerifyingChatHeader, None)?;
        self.verify_chat_header()?;

        // ── 准备消息 ────────────────────────────────────────────────
        self.advance(TaskState::PreparingMessage, None)?;
        self.prepare_message(workflow)
    }







    /// 核验候选人：文字与置信度两道都要过。
    ///
    /// **判据一律问匹配器**，不能在这里自己写「文字是否等于目标名」。
    /// 这里曾经写的是 `matched.text.trim() != task.external_contact_name.trim()`：
    /// 于是「联系人匹配器」换成放宽策略之后，任务照样在这个复检处转人工，
    /// 而失败文案（"不完全一致"）看起来像是 OCR 认不准 —— 排查会一路往
    /// 识别精度上找，永远找不到「两道关卡各写了一套判据」这个真原因。
    fn verify_candidate(&self, matched: &TextBox) -> Result<(), AutomationError> {
        if !self.runner.ports.matcher.accepts(&self.task.external_contact_name, matched) {
            return Err(AutomationError::AmbiguousVision(format!(
                "候选人文字「{}」不被当前的姓名匹配策略接受（目标「{}」）",
                matched.text.trim(),
                self.task.external_contact_name.trim()
            )));
        }
        if matched.confidence < self.cfg().min_confidence {
            return Err(AutomationError::AmbiguousVision(format!(
                "候选人置信度 {:.2} 低于阈值 {:.2}",
                matched.confidence,
                self.cfg().min_confidence
            )));
        }
        Ok(())
    }


    /// 核验聊天页标题：标题上写的人必须与目标一致。
    ///
    /// 失败时要分清两件事——"点错了人"和"那次点击根本没生效"。
    /// 后者用 `last_click_reacted` 判断：上一步点完画面一个像素都没变。
    fn verify_chat_header(&mut self) -> Result<(), AutomationError> {
        let header = self.resolve(self.cfg().chat_header, "聊天标题区")?;
        let (header_shot, header_boxes) = self.capture_and_recognize(header, "聊天标题识别")?;
        self.evidence.push(format!("chat_header#{}", header_shot.fingerprint));
        let header_result = self.runner.ports.matcher.find_unique_exact_match(
            &self.task.external_contact_name,
            &header_boxes,
            self.cfg().min_confidence,
        );
        // 判据一律问匹配器。这里原来写死了逐字相等，于是放宽匹配能选中联系人、
        // 却在标题核验处被判「不一致」——明明点对了人，任务还是转人工。
        let header_matched = header_result
            .as_ref()
            .map(|found| self.runner.ports.matcher.accepts(&self.task.external_contact_name, found))
            .unwrap_or(false);
        if header_matched {
            return Ok(());
        }
        if self.last_click_reacted == Some(false) {
            // 上一步点完之后画面一个像素都没变 ⇒ 那次点击很可能根本没落到界面上。
            // 常见原因：客户端卡死、窗口被别的窗口或弹窗挡住、点到了空白处。
            // 此时报"标题不符"会把人引到错的方向，所以单独说清楚。
            return Err(AutomationError::NeedsHumanReview(format!(
                "为「{}」打开聊天的那个点击之后，画面没有任何变化——这次点击可能没有生效。\
                 请检查客户端是否卡死、是否被其它窗口遮挡，然后重试。",
                self.task.external_contact_name.trim()
            )));
        }
        let found = header_result?;
        Err(AutomationError::AmbiguousVision(format!(
            "聊天页标题「{}」与目标「{}」不一致",
            found.text.trim(),
            self.task.external_contact_name.trim()
        )))
    }

}
