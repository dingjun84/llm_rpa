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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::audit::{AuditEntry, AuditSink, MemorySendLedger, MessageDigest, NoopAudit, SendLedger};
use crate::ports::{
    AutomationError, ContactMatcher, DesktopPlatform, EvidenceRecorder, HumanConfirmation,
    IconLocator, IconTemplate, LocalOcr, Point, Rect, ScreenMetrics, Screenshot, SendTask, TaskId,
    TextBox,
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
    /// 任务只在**标定时的那个尺寸**下工作，尺寸对不上就转人工，绝不按错的尺寸去点。
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
            scroll_anchor: RelativePoint::new(0.62, 0.5),
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

    /// 用模板匹配找到左侧导航图标，点它一下，把视图切到"能查到联系人"的那个页面。
    ///
    /// ## 这一步和"点击联系人"是同一类动作
    ///
    /// 它**不是只读的**：点下去之后界面会重绘。所以和点击联系人一样，
    /// 动作前要确认客户端还活着、前台窗口与标定一致；动作后要确认画面真的变了。
    ///
    /// ## 为什么"点完必须看到画面变化"
    ///
    /// 因为"点到了图标"和"点击生效了"是两件事。图标可能被别的窗口挡住、
    /// 客户端可能正好卡了一下、坐标可能因为某种原因落在图标边缘的空白上——
    /// 这些情况下 `guarded_click` 会正常返回（鼠标确实点下去了），
    /// 而视图**根本没切**。不校验的话，后面整条查找流程都作用在一个
    /// 没切换成功的界面上，失败原因会表现为"找不到联系人"——那是错的方向。
    fn navigate_to_view(&mut self) -> Result<(), AutomationError> {
        let (strip, min_score, panel) = {
            let cfg = self.cfg();
            (
                self.resolve(cfg.nav_strip, "导航图标搜索区")?,
                cfg.nav_icon_min_score,
                self.resolve(cfg.contact_panel, "联系人候选区")?,
            )
        };

        // 用**联系人候选区**当"视图变了没有"的参照物：切换成功的话，
        // 这一块的内容必然整体换掉。用它而不是整窗，是因为整窗里有闪烁的光标、
        // 未读红点之类会自己变的东西，"变了"就不再是"切换成功了"的证据。
        let before = self.capture_frame(panel, "切换视图前")?.fingerprint;

        let frame = self.capture_frame(strip, "导航图标搜索区")?;
        self.evidence.push(format!(
            "nav_strip#{}   搜索区 屏幕 ({}, {}) {}x{}",
            frame.fingerprint, strip.x, strip.y, strip.width, strip.height
        ));

        // 只借用 `self.runner`（它是一个共享引用），不碰 `self` 的可变部分——
        // 否则下面 `self.evidence.push` 会和这次调用打架。
        let runner = self.runner;
        let found = runner
            .ports
            .icons
            .locate(&frame, &runner.config().nav_icon_templates, min_score)?;

        let hit_screen = found.bounds.to_screen(Point { x: strip.x, y: strip.y });
        let target = hit_screen.center();
        // 记下"点的是哪儿、分数多少"。图标匹配不像文字识别那样有天然的可读结果，
        // 这一行是事后唯一能回答"它到底认成了什么"的地方。
        self.evidence.push(format!(
            "导航图标 : 模板「{}」分数 {:.3}   命中框 屏幕 ({}, {}) {}x{}   点击 屏幕 ({}, {})",
            found.template_label,
            found.score,
            hit_screen.x,
            hit_screen.y,
            hit_screen.width,
            hit_screen.height,
            target.x,
            target.y
        ));

        self.ensure_not_frozen("已取消切换视图")?;
        let expected_window = self.ensure_calibrated()?;
        self.runner
            .ports
            .platform
            .guarded_click(target, expected_window)?;
        self.check_deadline("点击导航图标")?;

        // 视图切换是重绘，同样要等停稳再比指纹——否则会截到动画中间帧，
        // 与"切换前"偶然相同，于是把一次成功的切换误判成"没生效"。
        self.wait_for_settle(panel)?;
        let after = self.capture_frame(panel, "切换视图后")?.fingerprint;
        if after == before {
            // ── 为什么这里只记警告，**不**转人工 ──────────────────────────
            //
            // "点下去画面没变"有两种成因，而它们在画面上**无法区分**：
            //
            // 1. 界面本来就已经停在这个视图上（上一次运行点完就留在这里了）
            //    ⇒ 点击无效是**正确**行为，一切正常；
            // 2. 客户端卡死 / 图标被别的窗口挡住 / 匹配到了一个不响应点击的位置
            //    ⇒ 点击真的没生效。
            //
            // 如果在这里直接转人工，第 1 种情况就会变成"第二次跑必然失败"——
            // 一个正常操作被判成故障，而且报错文案（"点击可能没有生效"）
            // 会把人引向排查客户端，方向完全错了。
            //
            // 那为什么不等一等再判、或者重试一次？因为"点击没生效"不是**时机**问题，
            // 重试与等待都解决不了（见 `REFERENCE.md` §15 的同型教训）。
            //
            // 于是把判定交给**下一步**：`locate_contact` 是纯只读的，
            // 视图不对它就在候选区里找不到目标，照样转人工。判据落在能真正
            // 决断的地方，而不是在这里猜。这一行警告的作用是——真出问题时，
            // 日志里已经写明了"视图可能根本没切过去"，不必再从"找不到联系人"倒推。
            self.evidence.push(format!(
                "⚠️ 点击导航图标后联系人候选区画面未变化（模板「{}」分数 {:.3}，点击 屏幕 ({}, {})）。\
                 若界面本来就停在这个视图上，这属于正常；否则说明这次点击没有生效，\
                 后面若报「找不到联系人」，先从这里查。",
                found.template_label, found.score, target.x, target.y
            ));
        }
        Ok(())
    }

    /// 在联系人候选区里找到目标。
    ///
    /// ## 为什么是"回顶 + 多轮扫描"
    ///
    /// 列表按**最近有消息**排序：任何一条新消息都会把对应的会话顶到最上面。
    /// 如果从上一次遗留的滚动位置一路向下找，目标可能**已经在身后**了——
    /// 这正是"下拉查找会错过"的成因。所以每一轮都先回到列表顶部（一个确定的起点），
    /// 一轮扫不到就回顶再扫一轮，把"扫描期间被新消息顶上去"这件事兜住。
    ///
    /// ## 三重设限（每一层都不能省）
    ///
    /// - 单轮的滚动次数上限（`max_scroll_attempts`）；
    /// - 完整扫描的轮数上限（`max_search_sweeps`）；
    /// - 取消令牌每一步都检查。
    ///
    /// **歧义不滚动重试**：滚动改变的是"现在能看到谁"，不改变"这个名字是否唯一"。
    /// 出现多个逐字匹配时滚下去只会把同一个歧义重复 N 次，所以立刻失败。
    fn locate_contact(&mut self, panel: Rect) -> Result<TextBox, AutomationError> {
        let sweeps = self.cfg().max_search_sweeps.max(1);
        // 把**鼠标会被放到哪儿**写进日志。
        //
        // 为什么值得单独记一行：`scroll_anchor` 是相对比例，而候选区一改宽度，
        // 同一个比例就落到完全不同的像素上（实测：区域从 0.28 改到 0.46，
        // 落点从 x=178 跳到 x=273）。操作者看到"鼠标没动到列表上"时，
        // 日志里原来只有"读到几块"，**根本无从对照**——只能靠猜。
        // 有了这一行，"鼠标到底去了哪里"就变成可核对的事实。
        //
        // 而且这一行现在是**可信的落点**，不只是"打算放到哪"：平台层的 `scroll`
        // 在真正发滚轮之前会用 `GetCursorPos` 核对光标确实停在这里（对不上就报错
        // 退出，不会继续滚）。所以任务能走到下一行，就说明光标真的到过这个坐标。
        let anchor = self.scroll_anchor(panel)?;
        self.evidence.push(format!(
            "滚动落点 : 屏幕 ({}, {})   候选区 : 屏幕 ({}, {}) {}x{}",
            anchor.x, anchor.y, panel.x, panel.y, panel.width, panel.height
        ));
        for sweep in 1..=sweeps {
            // 第二轮开始之前先回顶。第一轮刻意**不**回顶：从当前位置往下扫是最省的，
            // 而"目标在当前位置上方"这种情况由第二轮（从顶部重扫）兜住——
            // 两轮合起来的覆盖范围就是整个列表，不需要额外那一次回顶。
            //
            // 所以 `max_search_sweeps` 配成 1 就等于退回"只往下扫一遍"的老行为。
            if sweep > 1 {
                self.scroll_to_top(panel)?;
            }
            if let Some(matched) = self.sweep_contact_list(panel)? {
                return Ok(matched);
            }
            self.evidence.push(format!("contact_sweep#{sweep}"));
        }
        Err(AutomationError::NeedsHumanReview(format!(
            "已把联系人列表从头到尾完整扫描 {sweeps} 轮，仍未找到「{}」。\
             请确认该联系人确实存在，且列表没有被搜索框或筛选条件限制。",
            self.task.external_contact_name.trim()
        )))
    }

    /// 把配置里的滚动落点换算到屏幕坐标。
    ///
    /// 配置不合法就**报错**，而不是夹到边界上凑合：夹边界会让「比例写错了」
    /// 表现为「滚了半天没反应」，那是本项目最难查的一类现象。
    fn scroll_anchor(&self, panel: Rect) -> Result<Point, AutomationError> {
        let anchor = self.cfg().scroll_anchor;
        anchor.validate().map_err(|err| {
            AutomationError::NeedsHumanReview(format!("滚动落点配置不合法：{err}"))
        })?;
        Ok(anchor.resolve(panel))
    }

    /// 把联系人列表滚回顶部，为一次确定性的扫描定好起点。
    ///
    /// 判据是"向上滚之后画面不再变化"，而**不是**某个固定的滚动次数：
    /// 列表长度随联系人数量变化，写死次数换台机器、换个账号就不对了。
    ///
    /// 这里只截屏算指纹、**不做 OCR**——判断"画面动没动"不需要读字，
    /// 而 OCR 在真实模式下是一次几百毫秒的独立进程调用。
    fn scroll_to_top(&mut self, panel: Rect) -> Result<(), AutomationError> {
        let mut previous: Option<String> = None;
        // 上滚次数比下扫上限**多一次**，不是随手加的：
        // 最后一次是"空滚"——只有再滚一下、看到画面不再变化，才能确认已经到顶。
        // 恰好用满下扫上限的那种情况（列表正好那么长），少这一次就会误报失败。
        let limit = self.cfg().max_scroll_attempts.max(1) + 1;
        // 落点只算一次：配置错了要在**滚动之前**就失败，而不是滚了几轮才发现。
        let anchor = self.scroll_anchor(panel)?;
        for _ in 0..=limit {
            self.check_cancel()?;
            let fingerprint = self.capture_frame(panel, "联系人列表")?.fingerprint;
            if previous.as_deref() == Some(fingerprint.as_str()) {
                // 向上滚了一格画面没动 ⇒ 已经在顶部（或者这个列表根本滚不动）。
                return Ok(());
            }
            previous = Some(fingerprint);

            let expected_window = self.ensure_calibrated()?;
            self.runner.ports.platform.scroll(
                anchor,
                -self.cfg().scroll_notches_per_step,
                expected_window,
            )?;
            // 等停稳再进入下一轮：截到缓动动画的中间帧，指纹会跟"真的到顶了"
            // 长得一样，于是把"还在动"误判成"到顶了"，回顶这一步就白做了。
            self.wait_for_settle(panel)?;
            self.step_started = Instant::now();
        }
        Err(AutomationError::NeedsHumanReview(format!(
            "向上滚动 {limit} 次仍未回到联系人列表顶部，拒绝继续猜测扫描起点。"
        )))
    }

    /// 从当前位置向下扫描一遍联系人列表。
    ///
    /// 返回 `Ok(None)` 表示"这一遍扫到底了、没找到"——**不是失败**，
    /// 调用方可以回顶再扫一轮。
    fn sweep_contact_list(&mut self, panel: Rect) -> Result<Option<TextBox>, AutomationError> {
        let mut previous_fingerprint: Option<String> = None;
        let mut scrolled: u32 = 0;
        let step = self.cfg().scroll_notches_per_step;
        // 同 `scroll_to_top`：落点先算一次，配置错了立刻失败。
        let anchor = self.scroll_anchor(panel)?;

        loop {
            self.check_cancel()?;
            let (shot, candidates) = self.capture_and_recognize(panel, "联系人识别")?;
            self.evidence.push(format!("contact_panel#{}", shot.fingerprint));
            // 把这一帧**读成了什么**记下来。只记指纹的话，「找不到联系人」永远
            // 分不清两种原因：① OCR 读错了（要调放大倍数或把窗口调大）；
            // ② 名字根本不在这屏（要换列表或重做区域标定）。
            // 这两件事的处置完全相反，而原来在现场只能靠猜。
            let log_candidates = self.cfg().log_ocr_candidates;
            if log_candidates {
                self.evidence.push(format!(
                    "  读到 {} 块：{}",
                    candidates.len(),
                    describe_candidates(&candidates)
                ));
            }

            match self.runner.ports.matcher.find_unique_exact_match(
                &self.task.external_contact_name,
                &candidates,
                self.cfg().min_confidence,
            ) {
                Ok(matched) => return Ok(Some(matched)),
                // 歧义：滚动解决不了，立刻停下。
                Err(err @ AutomationError::AmbiguousVision(_)) => return Err(err),
                Err(_) => {
                    if scrolled >= self.cfg().max_scroll_attempts {
                        return Ok(None);
                    }
                    if previous_fingerprint.as_deref() == Some(shot.fingerprint.as_str()) {
                        // 向下滚了一格，画面没动。两种可能，必须区分开：
                        //   1) 已经到底了 —— 正常的收工条件；
                        //   2) 客户端卡死了 —— 必须立刻转人工，绝不能继续点。
                        // 唯一的区分办法是**往反方向滚一下**，看画面动不动。
                        if self.view_moves_when_scrolling(panel, -step)? {
                            return Ok(None);
                        }
                        self.ensure_not_frozen(
                            "向下滚动与向上滚动都无法改变联系人列表画面",
                        )?;
                        // 系统说它还活着 ⇒ 这个列表本来就没得滚（只有一屏），
                        // 按"这一遍扫到底了"处理，不要误报卡死。
                        return Ok(None);
                    }
                    previous_fingerprint = Some(shot.fingerprint.clone());

                    let expected_window = self.ensure_calibrated()?;
                    self.runner
                        .ports
                        .platform
                        .scroll(anchor, step, expected_window)?;
                    scrolled += 1;
                    // 等画面停稳再进入下一轮截图。不等的话，下一轮截到的是
                    // 缓动动画的中间帧：文字糊、行错位，OCR 读出来是乱的；
                    // 而且"上一帧和这一帧一样 ⇒ 到底了"这个判断也会被带偏。
                    self.wait_for_settle(panel)?;

                    // 滚动会让列表内容整体移动，上一轮的文字框位置全部作废，
                    // 下一轮必须重新截图识别——绝不能拿滚动前的结果去点击。
                    //
                    // 也正因为如此，每轮都把单步计时重置：滚动 N 次是 N 次独立尝试，
                    // 不重置的话滚动 20 次必然撑爆一次 `step_timeout`，
                    // 变成"搜到一半被判超时"。
                    self.step_started = Instant::now();
                }
            }
        }
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

    /// 滚一下，看画面有没有变化。只回答"这个方向的滚动还有没有效果"，不做 OCR。
    fn view_moves_when_scrolling(
        &mut self,
        panel: Rect,
        notches: i32,
    ) -> Result<bool, AutomationError> {
        let anchor = self.scroll_anchor(panel)?;
        let before = self.capture_frame(panel, "联系人列表")?.fingerprint;
        let expected_window = self.ensure_calibrated()?;
        self.runner.ports.platform.scroll(anchor, notches, expected_window)?;
        // **必须先等停稳再截"滚动之后"这一帧**：截早了，重绘还没发生，
        // `before` 与 `after` 相同 ⇒ 这一句会回答"滚不动"，而它的结论正是
        // "到底了没有"。判错的代价是整段列表被跳过，且完全不报错。
        self.wait_for_settle(panel)?;
        self.step_started = Instant::now();
        let after = self.capture_frame(panel, "联系人列表")?.fingerprint;
        Ok(before != after)
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

    /// 校验当前窗口尺寸与标定记录一致。
    ///
    /// 这是"按标定尺寸工作"这条约定的落地点：标定是相对窗口的比例，
    /// 而真实界面不是等比缩放的，尺寸变了四个区域就会整体偏移。
    /// 与其按偏了的区域去点击，不如停在原地让人把窗口恢复回去。
    fn verify_calibrated_size(
        &self,
        window: Rect,
        metrics: ScreenMetrics,
    ) -> Result<(), AutomationError> {
        let Some(expected) = self.cfg().calibrated_window else {
            return Ok(());
        };
        let size_ok = (window.width - expected.width).abs() <= WINDOW_SIZE_TOLERANCE_PX
            && (window.height - expected.height).abs() <= WINDOW_SIZE_TOLERANCE_PX;
        let scale_ok =
            (metrics.scale_factor - expected.scale_factor).abs() <= SCALE_FACTOR_TOLERANCE;
        if size_ok && scale_ok {
            return Ok(());
        }
        Err(AutomationError::NeedsHumanReview(format!(
            "窗口尺寸与标定记录不一致：记录 {}×{}（缩放 {:.2}），当前 {}×{}（缩放 {:.2}）。\
             区域标定是相对窗口的比例，但真实界面不是等比缩放的，尺寸变了就会点偏。\
             请把窗口恢复到标定时的尺寸，或重新点「记录窗口尺寸」并保存配置。",
            expected.width, expected.height, expected.scale_factor,
            window.width, window.height, metrics.scale_factor
        )))
    }

    fn execute(&mut self) -> Result<(), AutomationError> {
        self.check_cancel()?;

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
        self.verify_calibrated_size(window, metrics)?;
        self.window = Some(window);
        self.metrics = Some(metrics);
        self.advance(
            TaskState::WaitingForClient,
            Some(format!(
                "窗口 {}x{} @({},{})，缩放 {}",
                window.width, window.height, window.x, window.y, metrics.scale_factor
            )),
        )?;

        // ── 切换视图（可选）────────────────────────────────────────
        //
        // 图标上没有文字，OCR 读不到它，所以"先切到联系人视图"这一步只能靠
        // 模板匹配。它必须在查找之前——查找假定"现在看的就是目标视图"。
        if self.cfg().navigate_before_search {
            self.advance(TaskState::NavigatingToView, None)?;
            self.navigate_to_view()?;
        }

        // ── 查找联系人 ──────────────────────────────────────────────
        self.advance(TaskState::SearchingContact, None)?;
        let panel = self.resolve(self.cfg().contact_panel, "联系人候选区")?;
        // 列表一屏放不下时向下滚动继续找，找不到就转人工，绝不猜。
        let matched = self.locate_contact(panel)?;

        // ── 核验候选人 ──────────────────────────────────────────────
        self.advance(
            TaskState::VerifyingCandidate,
            Some(format!("候选文字：{}", matched.text.trim())),
        )?;
        // 判据必须问**匹配器**，不能在这里自己写「文字是否等于目标名」。
        // 这里曾经写的是 `matched.text.trim() != task.external_contact_name.trim()`：
        // 于是「联系人匹配器」换成放宽策略之后，任务照样在这个复检处转人工，
        // 而失败文案（"不完全一致"）看起来像是 OCR 认不准 —— 排查会一路往
        // 识别精度上找，永远找不到「两道关卡各写了一套判据」这个真原因。
        if !self.runner.ports.matcher.accepts(&self.task.external_contact_name, &matched) {
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

        // ── 点击候选人并核验聊天页标题 ──────────────────────────────
        // 先记下点击**之前**对话区的画面：一次生效的点击应该让它变化。
        // 这条指纹用来区分"点错了人"和"点击根本没生效"——两者的处置完全不同。
        let body = self.resolve(self.cfg().chat_body, "聊天正文区")?;
        let body_before_click = self.capture_frame(body, "点击前聊天区")?.fingerprint;

        // 点击之前先确认客户端还活着：往一个卡死的窗口里点击，什么都查不出来，
        // 只会让后面每一步都建立在"没生效的动作"上。
        self.ensure_not_frozen("已取消点击联系人")?;
        let expected_window = self.ensure_calibrated()?;
        let contact_screen_rect = matched.bounds.to_screen(Point { x: panel.x, y: panel.y });
        // 记下"点了哪儿"。这是"准备给联系人发消息"的第一步，也是整个流程里
        // 唯一一个真的把光标放到某个联系人身上的动作——出问题时必须能核对坐标。
        let target = contact_screen_rect.center();
        self.evidence.push(format!(
            "点击联系人 : 屏幕 ({}, {})   命中文字「{}」框 ({}, {}) {}x{}   置信度 {:.2}",
            target.x,
            target.y,
            matched.text.trim(),
            contact_screen_rect.x,
            contact_screen_rect.y,
            contact_screen_rect.width,
            contact_screen_rect.height,
            matched.confidence
        ));
        self.runner
            .ports
            .platform
            .guarded_click(target, expected_window)?;
        self.check_deadline("点击联系人")?;
        let chat_reacted = self.capture_frame(body, "点击后聊天区")?.fingerprint
            != body_before_click;

        self.advance(TaskState::VerifyingChatHeader, None)?;
        let header = self.resolve(self.cfg().chat_header, "聊天标题区")?;
        let (header_shot, header_boxes) = self.capture_and_recognize(header, "聊天标题识别")?;
        self.evidence.push(format!("chat_header#{}", header_shot.fingerprint));
        let header_result = self.runner.ports.matcher.find_unique_exact_match(
            &self.task.external_contact_name,
            &header_boxes,
            self.cfg().min_confidence,
        );
        // 同「核验候选人」：判据一律问匹配器。这里原来写死了逐字相等，
        // 于是放宽匹配能选中联系人，却在标题核验处被判「不一致」——
        // 明明点对了人，任务还是转人工。
        let header_matched = header_result
            .as_ref()
            .map(|found| self.runner.ports.matcher.accepts(&self.task.external_contact_name, found))
            .unwrap_or(false);
        if !header_matched {
            if !chat_reacted {
                // 点完之后对话区一个像素都没变 ⇒ 这次点击很可能根本没落到界面上。
                // 常见原因：客户端卡死、窗口被别的窗口或弹窗挡住、点到了列表空白处。
                // 此时报"标题不符"会把人引到错的方向，所以单独说清楚。
                return Err(AutomationError::NeedsHumanReview(format!(
                    "点击联系人「{}」之后，对话区画面没有任何变化——这次点击可能没有生效。\
                     请检查客户端是否卡死、是否被其它窗口遮挡，然后重试。",
                    self.task.external_contact_name.trim()
                )));
            }
            let found = header_result?;
            return Err(AutomationError::AmbiguousVision(format!(
                "聊天页标题「{}」与目标「{}」不一致",
                found.text.trim(),
                self.task.external_contact_name.trim()
            )));
        }

        // ── 准备消息：聚焦输入框并记录发送前聊天区基线 ──────────────
        self.advance(TaskState::PreparingMessage, None)?;
        // 选中联系人之后，焦点仍在会话列表（甚至搜索框）上，**不在消息输入框**里。
        // 不先点进输入框的话，后面那次粘贴会落到错误的位置——最坏情况是粘进
        // 搜索框，把搜索结果本身改掉。所以这里必须先做一次受守卫的点击。
        let expected_window = self.ensure_calibrated()?;
        let composer = self.resolve(self.cfg().composer, "消息输入框区")?;
        // 同样记下坐标：这一步点错地方，后面那次粘贴就会落到别的控件里
        // （最坏是落到搜索框，把搜索结果本身改掉），而现象只是"字没进去"。
        self.evidence.push(format!(
            "聚焦输入框 : 屏幕 ({}, {})   输入框区 : 屏幕 ({}, {}) {}x{}",
            composer.center().x,
            composer.center().y,
            composer.x,
            composer.y,
            composer.width,
            composer.height
        ));
        self.runner
            .ports
            .platform
            .guarded_click(composer.center(), expected_window)?;
        self.check_deadline("聚焦消息输入框")?;

        let (before_shot, _) = self.capture_and_recognize(body, "发送前聊天区识别")?;
        self.baseline_fingerprint = Some(before_shot.fingerprint.clone());
        self.evidence.push(format!("chat_before#{}", before_shot.fingerprint));

        // ── 「只填不发」：正文入框后就地结束 ────────────────────────
        //
        // 这条分支必须在人工确认**之前**，而且不能复用下面的发送路径：
        // 它存在的全部意义就是"绝不发送"，所以这里既不申请发送台账，
        // 也不写消息摘要——审计里不该留下任何"像发过了"的痕迹。
        if self.cfg().stop_before_send {
            let expected_window = self.ensure_calibrated()?;
            self.ensure_not_frozen("已取消填入消息正文")?;
            self.runner
                .ports
                .platform
                .paste_text(&self.task.text, expected_window)?;
            self.check_deadline("填入消息正文")?;
            self.advance(
                TaskState::Prepared,
                Some(format!(
                    "已把 {} 个字符填入输入框，按配置未发送",
                    self.task.text.chars().count()
                )),
            )?;
            return Ok(());
        }

        // ── 人工确认 ────────────────────────────────────────────────
        self.advance(TaskState::AwaitingHumanConfirmation, None)?;
        self.runner
            .ports
            .confirmation
            .confirm_send(self.task, self.cfg().confirmation_ttl)?;
        self.confirmation_at = Some(SystemTime::now());

        // ── 发送 ────────────────────────────────────────────────────
        self.check_cancel()?;
        self.runner.ledger.claim(self.task.id)?;
        // 摘要先于状态转换写入，确保 Sending 的审计记录已带上摘要。
        self.message_digest = Some(MessageDigest::of(&self.task.text));
        self.advance(TaskState::Sending, None)?;
        let expected_window = self.ensure_calibrated()?;
        // 发送前最后一次确认客户端还活着。往一个卡死的窗口里粘贴 + 回车，
        // 结果是"看起来发出去了，其实什么都没发生"——比失败更糟。
        self.ensure_not_frozen("已取消发送")?;
        self.runner
            .ports
            .platform
            .paste_text(&self.task.text, expected_window)?;
        self.runner
            .ports
            .platform
            .send_message_shortcut(expected_window)?;
        self.check_deadline("发送消息")?;

        // ── 核验送达 ────────────────────────────────────────────────
        self.advance(TaskState::VerifyingDelivery, None)?;
        let (after_shot, after_boxes) = self.capture_and_recognize(body, "送达核验识别")?;
        self.evidence.push(format!("chat_after#{}", after_shot.fingerprint));
        if Some(&after_shot.fingerprint) == self.baseline_fingerprint.as_ref() {
            // 画面没变有两种成因：消息真的没出现，或者客户端已经卡死。
            // 系统判定读得出来就带上这句；读不出来（Err）就不加——
            // 这只是诊断提示，不该因为诊断本身失败而改变结论。
            let frozen_hint = match self.runner.ports.platform.is_responsive() {
                Ok(false) => "（且系统判定客户端未响应，疑似卡死）",
                _ => "",
            };
            return Err(AutomationError::NeedsHumanReview(format!(
                "发送后聊天区截图未发生变化{frozen_hint}，无法确认消息已出现"
            )));
        }
        let wanted = self.task.text.trim();
        if wanted.is_empty() {
            return Err(AutomationError::NeedsHumanReview("消息正文为空".into()));
        }
        let appeared = after_boxes.iter().any(|b| {
            b.confidence >= self.cfg().min_confidence && b.text.contains(wanted)
        });
        if !appeared {
            return Err(AutomationError::NeedsHumanReview(
                "聊天区未识别到本条消息，拒绝判定为已送达".into(),
            ));
        }

        self.advance(TaskState::Completed, None)?;
        Ok(())
    }
}
