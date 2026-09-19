//! 运行模式、配置与运行器装配。
//!
//! 两种模式：
//!
//! - **演练模式**（`DryRun`）：全部使用 `platform-mock` 的替身端口，
//!   不接触真实桌面、不启动企业微信、不产生任何输入事件。用于演示与自检。
//! - **真实模式**（`Live`）：使用 `platform-windows` 与本地 OCR 进程。
//!   所有敏感参数都必须由使用者在界面上显式配置。

use std::sync::Arc;
use std::time::Duration;

use automation_core::{
    AuditSink, CalibratedWindow, ContainsNameMatcher, HumanConfirmation, IconTemplate, LocalOcr,
    RelativePoint, RelativeRegion, RunnerConfig, RunnerPorts, SendLedger, SendTask,
    StrictContactMatcher, WorkflowRunner, DEFAULT_MIN_CONFIDENCE, DEFAULT_NAV_ICON_MIN_SCORE,
    DEFAULT_NAV_STRIP, DEFAULT_REGIONS,
};
use platform_mock::{
    MockContactMatcher, MockDesktop, MockHumanConfirmation, MockIconLocator, MockOcr, MockScenario,
};
use serde::{Deserialize, Serialize};

/// 单步超时相对 OCR 超时的余量（秒）。
///
/// 一个步骤里除了 OCR 还有截屏、图像编码、进程启动这些开销，
/// 单步超时必须比 OCR 超时宽松这么多，否则调大 `ocr_timeout_ms` 是白调的。
const STEP_TIMEOUT_OCR_MARGIN_SECS: u64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMode {
    DryRun,
    Live,
}

/// 演练模式下要复现的场景，用于在界面上演示各类失败路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DemoScenario {
    Happy,
    DuplicateContact,
    NearName,
    LowConfidence,
    HeaderMismatch,
    LoginPrompt,
    DeliveryMissing,
    UnstableScreen,
}

/// 相对窗口的标定区域（比例，0.0–1.0）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionConfig {
    pub contact_panel: [f32; 4],
    pub chat_header: [f32; 4],
    pub chat_body: [f32; 4],
    pub composer: [f32; 4],
}

impl Default for RegionConfig {
    fn default() -> Self {
        // 从核心层的常量取值，**不在这里再写一份字面量**。
        //
        // 这两份标定值此前是各写一份的，而它们不一致时**不报任何错**：
        // 界面按一份画标定框、任务按另一份裁图，操作者只会看到「框画在这儿，
        // 识别却像在看别的地方」。让它们由同一个常量派生，这类不同步就不可能发生。
        let [panel, header, body, composer] = DEFAULT_REGIONS;
        let flatten = |region: RelativeRegion| [region.x, region.y, region.width, region.height];
        Self {
            contact_panel: flatten(panel),
            chat_header: flatten(header),
            chat_body: flatten(body),
            composer: flatten(composer),
        }
    }
}

impl RegionConfig {
    fn to_runner_regions(&self) -> (RelativeRegion, RelativeRegion, RelativeRegion, RelativeRegion) {
        let build = |values: [f32; 4]| RelativeRegion::new(values[0], values[1], values[2], values[3]);
        (
            build(self.contact_panel),
            build(self.chat_header),
            build(self.chat_body),
            build(self.composer),
        )
    }
}

/// 滚动落点：鼠标落到联系人列表内的哪个位置（相对该区域的比例）。
///
/// 默认 `(0.62, 0.5)` = **上下居中、左右偏右一点**（2026-09-17 操作者指定）。
/// 偏右而不是取正中心：`DEFAULT_CONTACT_PANEL` 的左边界已经落在姓名那一列上，
/// 取正中心会压住姓名与消息预览的交界处。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScrollAnchorConfig {
    pub x: f32,
    pub y: f32,
}

impl Default for ScrollAnchorConfig {
    fn default() -> Self {
        Self { x: 0.62, y: 0.5 }
    }
}

impl ScrollAnchorConfig {
    /// 两个分量是否都是合法比例（0.0–1.0）。
    pub fn is_valid(&self) -> bool {
        let in_range = |v: f32| (0.0..=1.0).contains(&v);
        in_range(self.x) && in_range(self.y)
    }
}

/// 标定时记录的窗口几何（屏幕坐标 + 显示器缩放）。
///
/// 位置只用来在界面上显示"记的是哪一个窗口"，**不参与校验**——窗口挪一下很正常。
/// 真正参与校验的是尺寸与缩放，理由见 [`automation_core::CalibratedWindow`]。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub scale_factor: f32,
}

/// 缺字段时回落到 [`Default`]。
///
/// 两个用处：
/// 1. 手写标定文件（例如 `target/live-wechat.json`）只需要写关心的几项；
/// 2. 将来再加字段时，旧配置不会因为少一个键就整个反序列化失败。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeConfig {
    pub mode: RuntimeMode,
    pub demo_scenario: DemoScenario,
    /// 企业微信可执行文件路径；留空表示拒绝启动。
    pub wecom_exe: Option<String>,
    /// 可执行文件期望的 SHA-256；留空表示跳过校验。
    pub wecom_exe_sha256: Option<String>,
    pub window_class: String,
    /// 本地 OCR 程序路径；留空表示未配置。
    pub ocr_command: Option<String>,
    pub ocr_args: Vec<String>,
    pub ocr_timeout_ms: u64,
    /// 单步超时的**下限**（秒）。
    ///
    /// 实际生效值还会与「OCR 超时 + 5 秒余量」取较大者——见
    /// [`RuntimeConfig::effective_step_timeout`]。这样两个超时不可能互相矛盾。
    pub step_timeout_secs: u64,
    pub confirmation_ttl_secs: u64,
    pub min_confidence: f32,
    pub regions: RegionConfig,
    /// 「只填不发」：把正文填进输入框后就结束，绝不发送。
    ///
    /// 用来验证"定位联系人 + 输入"这条链路是否准确，而不产生任何对外影响。
    pub stop_before_send: bool,
    /// 查找联系人时最多向下滚动多少次。
    pub max_scroll_attempts: u32,
    /// 每次向下滚动的格数。
    pub scroll_notches_per_step: i32,
    /// 滚动时鼠标落在联系人列表内的哪个位置（相对该区域的比例）。
    pub scroll_anchor: ScrollAnchorConfig,
    /// 标定时记录的窗口几何。
    ///
    /// 客户端由操作者手动启动并登录，任务只在**这个尺寸**下工作：
    /// 尺寸对不上就转人工，绝不按错的尺寸去点。
    /// 真实模式下这一项必须有值（[`build_runner`] 会拦），演练模式下用不上。
    pub calibrated_window: Option<WindowGeometry>,
    /// 联系人列表最多**完整**扫描几轮（每轮 = 从列表顶部向下扫到底）。
    ///
    /// 列表按"最近有消息"排序，扫描期间到达的新消息会把目标顶到最上面，
    /// 而那一屏早被翻过去了。多扫一轮就是专门兜这种情况的。
    pub max_search_sweeps: u32,
    /// 是否在"该动的画面没动"时判定客户端卡死并转人工。
    pub liveness_check: bool,
    /// 滚动之后**等画面停稳**再截图的上限（毫秒）。`0` = 不等。
    ///
    /// 客户端列表滚动是带缓动的动画，滚完立刻截图会截到中间帧（文字糊、行错位），
    /// 而且会让"滚了没动 ⇒ 到底了"的判断把"还没画完"当成"到底了"。
    /// 这是**上限**不是等待时长：连续两帧一样就立刻继续，正常只多花一帧。
    pub scroll_settle_ms: u64,
    /// 是否把每轮 OCR 实际读到的文字写进任务日志与界面。
    ///
    /// 排查"找不到联系人"时，只有指纹是看不出问题的：分不清是 OCR 读错了，
    /// 还是名字根本不在这屏。姓名本来就已经在日志里（开头的"目标联系人"一行），
    /// 所以这不算新增数据类别；且**不进审计库**。
    pub log_ocr_candidates: bool,
    /// ⚠️ **临时放宽姓名匹配**：精确匹配不到时，退化为「候选文本**包含**目标名」。
    ///
    /// 2026-09-17 操作者要求「先不用精确匹配，只要识别到包含的字符就行，先测试通过」，
    /// 目的是先让整条链路跑通，不让 OCR 的个别错字卡住后面的验证。
    ///
    /// **它违反 `docs/architecture.md` §6.4/§6.6 的「逐字精确匹配」**：
    /// 会把「找不到人」变成「可能找错人」。已知的误判风险、以及后续该怎么收紧，
    /// 记在 `docs/todo.md`。**有真实发送需求时必须关掉它。**
    ///
    /// 只影响**真实模式**：演练模式的匹配器由场景脚本驱动（`MockContactMatcher`），
    /// 开着它也不会改变那几条失败路径的演示行为。
    pub relaxed_name_match: bool,
    /// 是否在查找联系人之前，先用**模板匹配**找到左侧导航图标并点它一下，
    /// 把视图切到"能查到联系人"的那个页面。
    ///
    /// ## 为什么需要它
    ///
    /// 图标上没有文字，OCR 读不到它。靠文字找入口的流程因此完全无法表达
    /// "先切到联系人视图"这件事——而界面停在哪个视图，决定了后面那一次
    /// 联系人列表识别到底在看什么。模板匹配补的正是这一段。
    ///
    /// 打开之后状态轨迹里会多出一个 `NavigatingToView`；关掉就是原来的行为。
    ///
    /// **打开时必须配模板**（`nav_icon_templates` 非空），否则装配期直接拒绝：
    /// 宁可"任务还没登记就报错"，也不要留一条跑到一半才发现没模板的失败记录。
    /// 演练模式同样要求配模板——两个模式的校验规则必须一致，
    /// 否则"演练通过、真实报错"这种事迟早会发生。
    pub navigate_before_search: bool,
    /// 导航图标模板的 **PNG 路径**，可以多张（每行一个）。
    ///
    /// 为什么要多张：同一个图标在**选中 / 未选中**两种状态下长得不一样。
    /// 只留一张，就会出现"上一次运行点完停在这个页面上，这一次再也匹配不上"。
    ///
    /// 模板要**自己截**（用 `screen_probe template`，或系统的截图工具），
    /// 不要指望程序自动裁一个：程序猜出来的模板会把"点错了地方"变成一次
    /// 看起来完全正常的运行——匹配分数照样很高，因为它匹配的是它自己刚裁的那块。
    pub nav_icon_templates: Vec<String>,
    /// 图标模板匹配的最低分数（0–1），低于它转人工。
    ///
    /// 界面上那个「测试图标匹配」按钮就是用来量这个值的：它会报出当前画面上
    /// 的最高分。**不要照抄默认值**——阈值定低了会点错图标，定高了会频繁转人工。
    pub nav_icon_min_score: f32,
    /// 导航图标搜索区（相对窗口比例 `[x, y, w, h]`）。
    pub nav_strip: [f32; 4],
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            mode: RuntimeMode::DryRun,
            demo_scenario: DemoScenario::Happy,
            wecom_exe: None,
            wecom_exe_sha256: None,
            window_class: "WeWorkWindow".to_string(),
            ocr_command: None,
            ocr_args: Vec::new(),
            ocr_timeout_ms: 10_000,
            // 比 OCR 超时（10 秒）宽 2 倍，给慢机器留余量；
            // 真正的下限由 effective_step_timeout 推导，不靠这个值兜底。
            step_timeout_secs: 20,
            confirmation_ttl_secs: 60,
            min_confidence: DEFAULT_MIN_CONFIDENCE,
            regions: RegionConfig::default(),
            // 默认**不开**只填不发：默认行为应当是完整流程，且发送本身还有人工确认兜底。
            // 这个开关是给"验证定位与输入"用的，需要操作者显式打开。
            stop_before_send: false,
            max_scroll_attempts: 20,
            scroll_notches_per_step: 3,
            scroll_anchor: ScrollAnchorConfig::default(),
            // 默认没有标定尺寸：真实模式必须先在界面上点「记录窗口尺寸」。
            calibrated_window: None,
            // 两轮：一轮从当前位置扫到底，一轮回顶重扫。
            max_search_sweeps: 2,
            liveness_check: true,
            // 上限 600ms：画面一稳就继续，所以正常开销是"多截一帧"，
            // 只有动画真的还在跑时才会真的等这么久。
            scroll_settle_ms: 600,
            log_ocr_candidates: true,
            // 默认**开**：这是操作者当下要的"先把链路跑通"。
            // 关掉它就回到架构要求的逐字精确匹配。
            relaxed_name_match: true,
            // 默认**关**：这一步要先有操作者自己截的图标模板，
            // 默认打开等于让每个还没准备模板的人都撞上一次装配错误。
            navigate_before_search: false,
            nav_icon_templates: Vec::new(),
            nav_icon_min_score: DEFAULT_NAV_ICON_MIN_SCORE,
            nav_strip: flatten(DEFAULT_NAV_STRIP),
        }
    }
}

/// 把 [`RelativeRegion`] 摊平成配置里用的 `[x, y, w, h]`。
fn flatten(region: RelativeRegion) -> [f32; 4] {
    [region.x, region.y, region.width, region.height]
}

impl RuntimeConfig {
    pub fn to_runner_config(&self) -> RunnerConfig {
        let (contact_panel, chat_header, chat_body, composer) = self.regions.to_runner_regions();
        RunnerConfig {
            platform_label: match self.mode {
                RuntimeMode::DryRun => "dry-run".to_string(),
                RuntimeMode::Live => std::env::consts::OS.to_string(),
            },
            min_confidence: self.min_confidence.clamp(0.0, 1.0),
            confirmation_ttl: Duration::from_secs(self.confirmation_ttl_secs.max(5)),
            step_timeout: self.effective_step_timeout(),
            max_attempts: 3,
            retry_backoff: Duration::from_millis(150),
            contact_panel,
            chat_header,
            chat_body,
            composer,
            stop_before_send: self.stop_before_send,
            // 上限兜底到 1：允许配 0 就等于"完全不滚动"，那是另一个功能（禁用滚动查找），
            // 不该由"次数"这个旋钮顺手表达。
            max_scroll_attempts: self.max_scroll_attempts.max(1),
            scroll_notches_per_step: self.scroll_notches_per_step.clamp(1, 20),
            // 落点比例原样透传，**不在这里夹到 0–1**：夹边界会把「配置写错了」
            // 变成「滚了半天没反应」，而核心层会直接报错，那才是能查的失败。
            scroll_anchor: RelativePoint::new(self.scroll_anchor.x, self.scroll_anchor.y),
            calibrated_window: match self.mode {
                // 演练模式跑的是替身窗口，没有"真实窗口尺寸"这回事。
                // 拿配置里的尺寸去比，只会把每个演练场景都变成失败。
                RuntimeMode::DryRun => None,
                RuntimeMode::Live => self.calibrated_window.map(|geometry| CalibratedWindow {
                    width: geometry.width,
                    height: geometry.height,
                    scale_factor: geometry.scale_factor,
                }),
            },
            max_search_sweeps: self.max_search_sweeps.clamp(1, 20),
            liveness_check: self.liveness_check,
            // 刻意**不设上限**：这是"最多等多久"的上限值，操作者愿意等多久是他的选择。
            // 真正的自适应来自 runner 里的"连续两帧一致就继续"，不是靠这个数。
            scroll_settle_timeout: Duration::from_millis(self.scroll_settle_ms),
            log_ocr_candidates: self.log_ocr_candidates,
            navigate_before_search: self.navigate_before_search,
            // 模板**不在这里填**：载入 PNG 会失败，而本方法没有 `Result`。
            // 由 `build_runner` 在装配期调用 `load_nav_icon_templates` 填进去，
            // 失败就在任务登记之前报出来。这里留空是刻意的，不是漏了。
            nav_icon_templates: Vec::new(),
            nav_icon_min_score: self.nav_icon_min_score.clamp(0.0, 1.0),
            nav_strip: RelativeRegion::new(
                self.nav_strip[0],
                self.nav_strip[1],
                self.nav_strip[2],
                self.nav_strip[3],
            ),
        }
    }

    /// 单步超时的**有效值**：取配置值与「OCR 超时 + 余量」中的较大者。
    ///
    /// 这两个超时是嵌套关系：一个步骤里就包含一次 `capture + 本地 OCR`。
    /// 如果单步超时比 OCR 超时还小，那么把 `ocr_timeout_ms` 调大**完全没有效果**——
    /// 外层的单步超时会先到点，任务报 `Timeout`，而使用者以为是 OCR 的问题。
    ///
    /// 所以这里不写死，而是**推导**出来：配置值只是下限，
    /// 真正的下限由 OCR 超时决定，两者不可能再互相矛盾。
    ///
    /// 余量 5 秒覆盖截屏、图像编码、进程启动这些 OCR 之外的开销。
    fn effective_step_timeout(&self) -> Duration {
        let configured = Duration::from_secs(self.step_timeout_secs.max(1));
        let ocr_floor = Duration::from_millis(self.ocr_timeout_ms.max(500))
            + Duration::from_secs(STEP_TIMEOUT_OCR_MARGIN_SECS);
        configured.max(ocr_floor)
    }
}

/// 演练模式：按所选场景装配替身端口。
fn dry_run_ports(
    config: &RuntimeConfig,
    task: &SendTask,
) -> RunnerPorts {
    let contact = task.external_contact_name.as_str();
    let message = task.text.as_str();

    let scenario = match config.demo_scenario {
        DemoScenario::Happy => MockScenario::happy(contact, message),
        DemoScenario::DuplicateContact => MockScenario::duplicate_contact(contact, message),
        DemoScenario::NearName => MockScenario::near_name_only(contact, message),
        DemoScenario::LowConfidence => MockScenario::low_confidence_contact(contact, message),
        DemoScenario::HeaderMismatch => MockScenario::header_mismatch(contact, message),
        DemoScenario::LoginPrompt => MockScenario::login_prompt(contact, message),
        DemoScenario::DeliveryMissing => MockScenario::delivery_missing(contact, message),
        // 指纹冻结 = 发送后界面毫无变化，用于演示"无法确认送达"。
        DemoScenario::UnstableScreen => MockScenario::happy(contact, message),
    };

    let desktop = MockDesktop::new();
    if config.demo_scenario == DemoScenario::UnstableScreen {
        desktop.freeze_fingerprints();
    }

    RunnerPorts {
        platform: Arc::new(desktop),
        ocr: Arc::new(MockOcr::new(scenario.script())),
        matcher: Arc::new(MockContactMatcher::new()),
        // 演练模式的图标定位：正中命中。真去读模板反而会让演示依赖一张真图片。
        icons: Arc::new(MockIconLocator::new()),
        confirmation: Arc::new(MockHumanConfirmation::default()),
    }
}

/// 按配置选姓名匹配器。
///
/// 抽成独立函数，是为了让「这个开关真的选到了放宽层」可以被用例钉住——
/// 直接写在 `live_ports` 里就只能靠读代码确认，而选错匹配器**不会报任何错**，
/// 只会在现场表现为「它怎么点到别人身上去了」。
fn build_matcher(relaxed: bool) -> Arc<dyn automation_core::ContactMatcher> {
    if relaxed {
        Arc::new(ContainsNameMatcher::default())
    } else {
        Arc::new(StrictContactMatcher::default())
    }
}

/// 真实模式：装配 Windows 平台与本地 OCR。
fn live_ports(config: &RuntimeConfig) -> Result<RunnerPorts, String> {
    #[cfg(windows)]
    {
        use platform_windows::{WindowsDesktop, WindowsDesktopConfig};
        use vision::{ExternalOcr, UnconfiguredOcr};

        let desktop = WindowsDesktop::new(WindowsDesktopConfig {
            wecom_exe: config.wecom_exe.as_ref().map(std::path::PathBuf::from),
            wecom_exe_sha256: config.wecom_exe_sha256.clone(),
            window_matcher: platform_windows::WindowMatcher::ClassName(config.window_class.clone()),
            // 其余取默认值：粘贴后清空剪贴板、600ms 剪贴板读取等待、4M 像素捕获上限。
            ..WindowsDesktopConfig::default()
        });

        let ocr: Arc<dyn LocalOcr> = match config.ocr_command.as_ref() {
            Some(command) if !command.trim().is_empty() => Arc::new(
                ExternalOcr::new(command.trim())
                    .with_args(config.ocr_args.clone())
                    .with_timeout(Duration::from_millis(config.ocr_timeout_ms.max(500))),
            ),
            _ => Arc::new(UnconfiguredOcr),
        };

        Ok(RunnerPorts {
            platform: Arc::new(desktop),
            ocr,
            // 真实模式的姓名匹配器在这里选型。
            // 放宽层是**临时**的，理由与风险见 `ContainsNameMatcher` 与 `docs/todo.md`。
            matcher: build_matcher(config.relaxed_name_match),
            // 纯 Rust 的模板匹配。为什么不挂 OpenCV 见 `vision::template` 的模块文档
            // 与 `docs/todo.md` T9——端口在这里，换实现不用动调用方。
            icons: Arc::new(vision::TemplateLocator),
            confirmation: Arc::new(MockHumanConfirmation::default()),
        })
    }
    #[cfg(not(windows))]
    {
        let _ = config;
        Err("真实模式目前只支持 Windows".to_string())
    }
}

/// 载入导航图标模板。
///
/// **必须在任务登记之前调用**：模板文件不存在、尺寸不像图标，都要在
/// "任务还没进列表"的时候就报出来。否则列表里会留下一条注定失败的记录，
/// 看起来像是真的跑过——而它连第一步都没走完。
///
/// 校验规则对两个模式**完全一致**。演练模式本来可以放宽（替身不读图片），
/// 但那样就会出现"演练一路通过、切到真实模式立刻报错"，而报错的那一刻
/// 任务已经登记了。宁可在演练模式也要求配一张真图片。
pub(crate) fn load_nav_icon_templates(paths: &[String]) -> Result<Vec<IconTemplate>, String> {
    let paths: Vec<&str> = paths
        .iter()
        .map(|path| path.trim())
        .filter(|path| !path.is_empty())
        .collect();
    if paths.is_empty() {
        return Err(
            "「先点击导航图标跳转」已经打开，但没有配置任何图标模板。\
             请先用 screen_probe 的 template 子命令从目标窗口截一张图标 PNG，\
             再把路径填进「导航图标模板」。"
                .to_string(),
        );
    }

    let mut templates = Vec::with_capacity(paths.len());
    for path in paths {
        let path = std::path::PathBuf::from(path);
        // label 取文件名：失败信息里要能一眼看出是**哪一张**模板出的问题。
        // 用完整路径会让一行日志变得很长，而文件名在同一个目录下是唯一的。
        let label = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let template =
            vision::load_icon_template(&path, label).map_err(|err| format!("图标模板不可用：{err}"))?;
        templates.push(template);
    }
    Ok(templates)
}

/// 组装一个可运行的 [`WorkflowRunner`]。
///
/// `confirmation` 由调用方注入：真实界面走 [`crate::confirmation::UiConfirmation`]，
/// 因此"人工确认"不是被跳过的环节，而是真的会阻塞等待操作者。
pub fn build_runner(
    config: &RuntimeConfig,
    task: &SendTask,
    audit: Arc<dyn AuditSink>,
    ledger: Arc<dyn SendLedger>,
    confirmation: Arc<dyn HumanConfirmation>,
) -> Result<WorkflowRunner, String> {
    // 真实模式下没有标定尺寸就拒绝开跑。客户端由操作者手动启动，程序没法从窗口外面
    // 分辨"这是不是我标定过的那个窗口、是不是那个尺寸"，只能靠这条记录。
    // 少了它，"按标定尺寸工作"就只是一句口号：尺寸变了区域会整体偏移，而点击
    // 落偏的后果是点到别的地方——宁可停在原地让人把窗口恢复回去。
    if config.mode == RuntimeMode::Live && config.calibrated_window.is_none() {
        return Err(
            "真实模式必须先点「记录窗口尺寸」并保存配置：\
             任务只在标定时的窗口尺寸下运行，否则四个区域会整体偏移。"
                .to_string(),
        );
    }

    // 滚动落点也得是个合法比例。核心层同样会拦（而且是在**第一次滚动之前**就拦），
    // 这里再拦一道的理由跟上面那条一样：在核心层报错时任务已经登记进列表了，
    // 会留下一条注定失败的记录，让人以为任务真的跑过。
    let anchor = config.scroll_anchor;
    if !anchor.is_valid() {
        return Err(format!(
            "滚动落点必须在 0–1 之间（当前 {:.2} / {:.2}）：\
             它是鼠标停在联系人候选区内的位置比例，超出范围会落到区域外面，\
             滚轮就滚不动那个列表了。",
            anchor.x, anchor.y
        ));
    }

    let mut ports = match config.mode {
        RuntimeMode::DryRun => dry_run_ports(config, task),
        RuntimeMode::Live => live_ports(config)?,
    };
    // 确认端口始终来自界面，保证真实模式下也必须人工确认。
    ports.confirmation = confirmation;

    let mut runner_config = config.to_runner_config();

    // 「先点导航图标切视图」这一组的校验与模板载入。
    //
    // 和上面两条同一个道理：**全部放在装配期**。区域比例非法、模板文件读不出来、
    // 该配模板却没配——这些都会让任务注定失败，而装配失败**不会在任务列表里
    // 留下记录**，装配成功才会登记。所以能提前判的一律提前判。
    if config.navigate_before_search {
        let strip = runner_config.nav_strip;
        if let Err(err) = strip.validate() {
            return Err(format!(
                "导航图标搜索区的比例不合法（{:.3}, {:.3}, {:.3}, {:.3}）：{err}。\
                 它是相对窗口的比例，四项都要落在 0–1 之间且不能越出窗口。",
                strip.x, strip.y, strip.width, strip.height
            ));
        }
        if !(0.0..=1.0).contains(&config.nav_icon_min_score) {
            return Err(format!(
                "图标匹配最低分数必须在 0–1 之间（当前 {}）：\
                 它是归一化互相关系数，1.0 表示完全一致。",
                config.nav_icon_min_score
            ));
        }
        runner_config.nav_icon_templates = load_nav_icon_templates(&config.nav_icon_templates)?;
    }

    Ok(WorkflowRunner::new(ports, runner_config)
        .with_audit(audit)
        .with_ledger(ledger))
}

#[cfg(test)]
mod tests {
    use super::*;
    use automation_core::{MemoryAudit, MemorySendLedger};

    /// 造一张真的 PNG 当模板。
    ///
    /// 刻意**不引 `image` 这个依赖**：`vision::pixels` 已经能把 BGRA 缓冲编码成
    /// PNG，而构造 BGRA 缓冲只需要一个 `Vec<u8>`。少一个依赖就少一处版本漂移
    /// （`RgbaImage` 在两个 crate 里是两个不同的类型，版本不一致时很难看出原因）。
    fn write_template_png(name: &str, width: u32, height: u32) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("rpa-llm-nav-template");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[(x * 7) as u8, (y * 11) as u8, ((x + y) * 5) as u8, 255]);
            }
        }
        let shot = automation_core::Screenshot {
            pixels,
            width,
            height,
            captured_at: std::time::SystemTime::now(),
            fingerprint: String::new(),
        };
        let rgba = vision::pixels::to_rgba(&shot).unwrap();
        std::fs::write(&path, vision::pixels::encode_png(&rgba).unwrap()).unwrap();
        path
    }

    fn sample_task() -> SendTask {
        SendTask {
            id: uuid::Uuid::new_v4(),
            external_contact_name: "外部测试联系人".into(),
            text: "测试正文".into(),
            created_by: "测试操作者".into(),
        }
    }

    /// 走一遍真实的装配路径，只关心它成不成功、以及装配出来的运行器里有什么。
    fn assemble(config: &RuntimeConfig) -> Result<WorkflowRunner, String> {
        build_runner(
            config,
            &sample_task(),
            Arc::new(MemoryAudit::new()),
            Arc::new(MemorySendLedger::new()),
            Arc::new(MockHumanConfirmation::default()),
        )
    }

    /// 单步超时不能小于 OCR 超时，否则把 OCR 超时调大是白调的。
    ///
    /// 这两个超时是嵌套关系：一个步骤里就包含一次 `capture + 本地 OCR`。
    /// 外层先到点的话，任务报的是 `Timeout`，使用者会误以为是 OCR 的问题。
    #[test]
    fn step_timeout_never_undercuts_the_ocr_timeout() {
        let config = RuntimeConfig {
            ocr_timeout_ms: 60_000,
            // 故意配一个比 OCR 超时小得多的下限。
            step_timeout_secs: 5,
            ..RuntimeConfig::default()
        };
        let effective = config.to_runner_config().step_timeout;
        assert!(
            effective >= Duration::from_secs(65),
            "单步超时应至少是 OCR 超时加余量，实际是 {effective:?}"
        );
    }

    /// 反过来：单步下限配得很大时，不能被 OCR 超时压下去。
    #[test]
    fn a_large_step_floor_is_respected() {
        let config = RuntimeConfig {
            ocr_timeout_ms: 1_000,
            step_timeout_secs: 120,
            ..RuntimeConfig::default()
        };
        assert_eq!(
            config.to_runner_config().step_timeout,
            Duration::from_secs(120)
        );
    }

    /// 默认值本身也要自洽：默认的单步超时必须容得下默认的 OCR 超时。
    #[test]
    fn the_defaults_are_self_consistent() {
        let config = RuntimeConfig::default();
        let effective = config.to_runner_config().step_timeout;
        assert!(
            effective > Duration::from_millis(config.ocr_timeout_ms),
            "默认单步超时 {effective:?} 容不下默认 OCR 超时 {}ms",
            config.ocr_timeout_ms
        );
    }

    /// 界面标定用的默认值必须与核心层的出厂值逐字段一致。
    ///
    /// `RegionConfig` 用 `[f32; 4]`、核心层用 `RelativeRegion`，类型不同，
    /// 以前是各写一份字面量——两边不一致时**不报任何错**，只是界面按一份画框、
    /// 任务按另一份裁图，现场表现为「框明明画对了，却识别不到」。
    /// 现在后者由前者派生，这条用例把它钉死。
    #[test]
    fn the_region_defaults_match_the_core_constants() {
        let regions = RegionConfig::default();
        let [panel, header, body, composer] = DEFAULT_REGIONS;
        assert_eq!(regions.contact_panel, [panel.x, panel.y, panel.width, panel.height]);
        assert_eq!(regions.chat_header, [header.x, header.y, header.width, header.height]);
        assert_eq!(regions.chat_body, [body.x, body.y, body.width, body.height]);
        assert_eq!(regions.composer, [composer.x, composer.y, composer.width, composer.height]);

        // 顺带钉住「区域经过校验」：比例写错要到任务跑起来才发现就太晚了。
        let runner = RuntimeConfig::default().to_runner_config();
        for (label, region) in [
            ("联系人候选区", runner.contact_panel),
            ("聊天页标题区", runner.chat_header),
            ("聊天正文区", runner.chat_body),
            ("消息输入框区", runner.composer),
        ] {
            assert!(region.validate().is_ok(), "{label} 的默认比例不合法：{region:?}");
        }
    }

    /// `contact_panel` 的左边界**不能是 0**，否则联系人姓名永远匹配不上。
    ///
    /// 会话列表左侧还有导航图标栏和头像列（头像右上角带未读红点），
    /// 它们与姓名在同一行高度上，会被 OCR **并进同一个文字块**。
    /// 实测（960x734）：左边界取 0 时读到 `《明月（美、加、欧洲）清库存`、
    /// `0 丁俊`，取 0.14 时读到干净的 `明月（美、加、欧洲）清库存`、`丁俊`。
    /// 而姓名匹配是逐字精确的（`docs/architecture.md` §6.4/§6.6），
    /// 多一个前导字符就永远找不到人。
    ///
    /// 这条用例守的是「有人为了多看到点头像信息，顺手把左边界改回 0」。
    #[test]
    fn the_contact_panel_must_not_swallow_the_avatar_column() {
        let regions = RegionConfig::default();
        assert!(
            regions.contact_panel[0] > 0.0,
            "联系人候选区的左边界必须让开左侧图标栏与头像列，实际是 {}",
            regions.contact_panel[0]
        );
        assert!(
            regions.contact_panel[0] + regions.contact_panel[2] <= 1.0,
            "联系人候选区右边界越出了窗口：{regions:?}"
        );
    }

    /// 滚动落点默认是「上下居中、左右偏右一点」，并原样透传到核心层。
    #[test]
    fn scroll_anchor_defaults_to_slightly_right_of_center() {
        let config = RuntimeConfig::default();
        assert_eq!(config.scroll_anchor, ScrollAnchorConfig { x: 0.62, y: 0.5 });
        let runner = config.to_runner_config();
        assert_eq!(runner.scroll_anchor, RelativePoint::new(0.62, 0.5));
        assert!(runner.scroll_anchor.validate().is_ok());
    }

    /// 落点比例**不在这里夹到 0–1**，非法值必须原样透传、由核心层报错。
    ///
    /// 夹边界会把「配置写错了」变成「滚了半天没反应」——
    /// 后者是现场最难查的一类现象，所以宁可让任务直接失败并说清原因。
    #[test]
    fn an_out_of_range_scroll_anchor_is_passed_through_not_clamped() {
        let config = RuntimeConfig {
            scroll_anchor: ScrollAnchorConfig { x: 1.5, y: -0.2 },
            ..RuntimeConfig::default()
        };
        assert!(!config.scroll_anchor.is_valid());
        let runner = config.to_runner_config();
        assert_eq!(runner.scroll_anchor, RelativePoint::new(1.5, -0.2));
        assert!(runner.scroll_anchor.validate().is_err());
    }

    /// 「滚动停稳等待」的默认值必须是个**有限的上限**，而不是 0（0 = 不等，
    /// 等于把缓动动画的中间帧直接喂给 OCR），也不是一个大到让每滚一步都要
    /// 等上几秒的数——它是上限，正常开销只是多截一帧。
    #[test]
    fn scroll_settle_defaults_to_a_small_bounded_wait() {
        let config = RuntimeConfig::default();
        assert_eq!(config.scroll_settle_ms, 600);
        let runner = config.to_runner_config();
        assert_eq!(runner.scroll_settle_timeout, Duration::from_millis(600));
        assert!(!runner.scroll_settle_timeout.is_zero());
        assert!(runner.scroll_settle_timeout <= Duration::from_secs(2));
    }

    /// 毫秒值原样换算成 `Duration`，**不夹到某个区间**：
    /// 操作者填 0 就是明确要求"不等"，不能替他改主意。
    #[test]
    fn scroll_settle_ms_is_converted_verbatim() {
        let config = RuntimeConfig { scroll_settle_ms: 0, ..RuntimeConfig::default() };
        assert!(config.to_runner_config().scroll_settle_timeout.is_zero());

        let config = RuntimeConfig { scroll_settle_ms: 1500, ..RuntimeConfig::default() };
        assert_eq!(
            config.to_runner_config().scroll_settle_timeout,
            Duration::from_millis(1500)
        );
    }

    /// 默认要**记录识别结果**：排查"找不到联系人"时，没有这一项就分不清
    /// 是 OCR 读错了还是名字根本不在这屏，两者的处置完全相反。
    #[test]
    fn reading_back_the_ocr_text_is_on_by_default() {
        let config = RuntimeConfig::default();
        assert!(config.log_ocr_candidates);
        assert!(config.to_runner_config().log_ocr_candidates);

        let config = RuntimeConfig { log_ocr_candidates: false, ..RuntimeConfig::default() };
        assert!(!config.to_runner_config().log_ocr_candidates);
    }

    /// 「放宽姓名匹配」这个开关必须**真的**换掉匹配器，两个方向都要验。
    ///
    /// 为什么值得单独钉：选错匹配器**不报任何错**，只在现场表现为
    /// 「它怎么点到别人身上去了」或者「明明在列表里却说找不到」。
    /// 这里用实测过的那条数据（OCR 把头像红点并进了姓名行，读出 `0 丁俊`）。
    #[test]
    fn the_relaxed_switch_actually_selects_the_lenient_matcher() {
        use automation_core::{Rect, TextBox};

        let noisy = vec![TextBox {
            text: "0 丁俊".into(),
            bounds: Rect { x: 4, y: 29, width: 38, height: 19 },
            confidence: 1.0,
        }];

        let lenient = build_matcher(true);
        let found = lenient
            .find_unique_exact_match("丁俊", &noisy, DEFAULT_MIN_CONFIDENCE)
            .expect("放宽模式下应当认出「0 丁俊」里的「丁俊」");
        assert_eq!(found.text, "0 丁俊");

        let strict = build_matcher(false);
        assert!(
            strict.find_unique_exact_match("丁俊", &noisy, DEFAULT_MIN_CONFIDENCE).is_err(),
            "关掉开关就必须回到架构要求的逐字精确匹配"
        );
    }

    /// 默认**开**着放宽匹配——这是操作者当下要的「先把链路跑通」。
    ///
    /// 顺带把「它只是临时措施」这件事钉在用例里：将来收紧默认值时，
    /// 这条用例会失败，逼着改的人回来读一遍 `ContainsNameMatcher` 的文档。
    #[test]
    fn relaxed_name_matching_is_on_by_default_for_now() {
        assert!(RuntimeConfig::default().relaxed_name_match);
    }

    /// 导航图标这一组的默认值必须与核心层的常量逐字段一致，并且**默认关**。
    ///
    /// 默认关的理由要钉住：这一步需要操作者自己截一张图标模板，
    /// 默认打开等于让每个还没准备模板的人都在点「开始任务」时撞上一次装配错误。
    #[test]
    fn navigation_defaults_come_from_the_core_constants_and_are_off() {
        let config = RuntimeConfig::default();
        assert!(!config.navigate_before_search);
        assert!(config.nav_icon_templates.is_empty());
        assert_eq!(config.nav_icon_min_score, DEFAULT_NAV_ICON_MIN_SCORE);
        assert_eq!(config.nav_strip, flatten(DEFAULT_NAV_STRIP));

        let runner = config.to_runner_config();
        assert_eq!(runner.nav_strip, DEFAULT_NAV_STRIP);
        assert_eq!(runner.nav_icon_min_score, DEFAULT_NAV_ICON_MIN_SCORE);
        assert!(runner.nav_icon_templates.is_empty(), "模板由装配期载入，不在这个转换里");
        assert!(!runner.navigate_before_search);
    }

    /// 打开开关却没配模板 ⇒ **装配期**就拒绝，而不是留一条跑到一半才失败的记录。
    #[test]
    fn turning_on_navigation_without_a_template_is_refused() {
        let config = RuntimeConfig {
            navigate_before_search: true,
            ..RuntimeConfig::default()
        };
        let err = assemble(&config).err().expect("没有模板时必须拒绝装配");
        assert!(err.contains("没有配置任何图标模板"), "{err}");

        // 只填空白也算没配——不然会变成"文件名叫空字符串"这种更难查的错。
        let config = RuntimeConfig {
            navigate_before_search: true,
            nav_icon_templates: vec!["   ".into(), String::new()],
            ..RuntimeConfig::default()
        };
        assert!(assemble(&config).is_err());
    }

    /// 模板在装配期载入，并且真的被带进了运行器。
    #[test]
    fn templates_are_loaded_at_assembly_time() {
        let path = write_template_png("nav-icon.png", 20, 18);
        let config = RuntimeConfig {
            navigate_before_search: true,
            nav_icon_templates: vec![path.display().to_string(), "   ".into()],
            ..RuntimeConfig::default()
        };

        let runner = assemble(&config).expect("应当装配成功");
        let templates = &runner.config().nav_icon_templates;
        assert_eq!(templates.len(), 1, "空白行要忽略，不然会变成一个空路径");
        assert_eq!(
            templates[0].label, "nav-icon.png",
            "label 取文件名：失败信息里要能一眼看出是哪一张模板出的问题"
        );
        assert_eq!((templates[0].width, templates[0].height), (20, 18));
    }

    /// 模板本身不可用时也要在装配期报错。
    ///
    /// 关键在"什么时候报"：留到运行期的话，症状是"匹配分数很低"，
    /// 而人只会去怀疑阈值，不会想到"这张图根本不是图标"。
    #[test]
    fn an_unusable_template_is_refused_at_assembly_time() {
        let too_big = write_template_png("too-big.png", 300, 40);
        let config = RuntimeConfig {
            navigate_before_search: true,
            nav_icon_templates: vec![too_big.display().to_string()],
            ..RuntimeConfig::default()
        };
        let err = assemble(&config).err().expect("过大的模板必须被拒绝");
        assert!(err.contains("太大"), "报错要说清是尺寸问题：{err}");

        let missing = std::env::temp_dir().join("rpa-llm-nav-template/definitely-missing.png");
        let config = RuntimeConfig {
            navigate_before_search: true,
            nav_icon_templates: vec![missing.display().to_string()],
            ..RuntimeConfig::default()
        };
        assert!(assemble(&config).is_err(), "文件不存在也必须被拒绝");
    }

    /// 搜索区比例非法 / 阈值越界，同样在装配期拦下。
    #[test]
    fn an_illegal_nav_strip_or_threshold_is_refused() {
        let path = write_template_png("nav-icon.png", 20, 18);

        let config = RuntimeConfig {
            navigate_before_search: true,
            nav_icon_templates: vec![path.display().to_string()],
            nav_strip: [0.0, 0.0, 1.5, 1.0],
            ..RuntimeConfig::default()
        };
        let err = assemble(&config).err().expect("比例越界必须被拒绝");
        assert!(err.contains("导航图标搜索区"), "{err}");

        let config = RuntimeConfig {
            navigate_before_search: true,
            nav_icon_templates: vec![path.display().to_string()],
            nav_icon_min_score: 1.4,
            ..RuntimeConfig::default()
        };
        let err = assemble(&config).err().expect("阈值越界必须被拒绝");
        assert!(err.contains("0–1"), "{err}");
    }

    /// 关着的时候不该去读模板：路径写错了也不该拦住任务。
    #[test]
    fn templates_are_not_touched_while_navigation_is_off() {
        let config = RuntimeConfig {
            navigate_before_search: false,
            nav_icon_templates: vec!["Z:/definitely/missing.png".into()],
            nav_strip: [0.0, 0.0, 9.0, 9.0],
            ..RuntimeConfig::default()
        };
        let runner = assemble(&config).expect("关着的时候这些配置项都不该被读");
        assert!(runner.config().nav_icon_templates.is_empty());
    }
}
