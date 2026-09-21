//! 运行模式、配置与运行器装配。
//!
//! 两种模式：
//!
//! - **演练模式**（`DryRun`）：全部使用 `platform-mock` 的替身端口，
//!   不接触真实桌面、不启动企业微信、不产生任何输入事件。用于演示与自检。
//! - **真实模式**（`Live`）：使用 `platform-windows` / `platform-macos` 与本地 OCR 进程。
//!   所有敏感参数都必须由使用者在界面上显式配置。

use std::sync::Arc;
use std::time::Duration;

use automation_core::{
    AuditSink, ContainsNameMatcher, HumanConfirmation, IconTemplate, LocalOcr, RelativePoint,
    RelativeRegion, RunnerConfig, RunnerPorts, SendLedger, SendTask, StrictContactMatcher, Workflow,
    WorkflowRunner, DEFAULT_ICON_PRIOR_SCORE_TOLERANCE,
    DEFAULT_MIN_CONFIDENCE, DEFAULT_NAV_ICON_MIN_SCORE, DEFAULT_NAV_STRIP,
    DEFAULT_PROFILE_CHAT_ENTRY_TEXT, DEFAULT_PROFILE_SCROLL_ANCHOR, DEFAULT_REGIONS,
    DEFAULT_SCROLL_ANCHOR, DEFAULT_SEARCH_CONTACT_GROUP_LABEL,
};
use platform_mock::{
    MockContactMatcher, MockDesktop, MockHumanConfirmation, MockIconLocator, MockOcr, MockScenario,
};
use serde::{Deserialize, Serialize};

use crate::calibration;

mod mode;
mod requirements;
mod calibrations;

// ★ 再导出：这几块搬进了 `mode.rs` / `requirements.rs`，但
// `crate::runtime::X` 这条路径必须照旧成立——`lib.rs` 与 `runtime/tests.rs`
// 都按它引用（`use crate::runtime::{RunChoice, RuntimeMode, …}`）。
// 少这几行，症状是一堆 `E0432: unresolved import`，看着像"文件没编进去"。
pub use mode::{DemoScenario, ModeNotices, RuntimeMode};
pub use calibrations::CalibrationSnapshot;
pub use requirements::{
    workflow_inputs, workflow_requirements, MarkRequirement, WorkflowInputs, WorkflowRequirement,
};

// 它只在 `runtime.rs` 内部用（装配期按工作流拒绝任务），所以不对外再导出。
use requirements::missing_marks;

/// 单步超时相对 OCR 超时的余量（秒）。
///
/// 一个步骤里除了 OCR 还有截屏、图像编码、进程启动这些开销，
/// 单步超时必须比 OCR 超时宽松这么多，否则调大 `ocr_timeout_ms` 是白调的。
const STEP_TIMEOUT_OCR_MARGIN_SECS: u64 = 5;

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
        // 从核心层的常量派生，**不在这里再写一份字面量**：界面按一份显示、
        // 任务按另一份滚动时，现象只是「滚了半天没反应」，看不出是两边不同步。
        Self { x: DEFAULT_SCROLL_ANCHOR.x, y: DEFAULT_SCROLL_ANCHOR.y }
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
    /// 尺寸对不上会先自动把窗口调回这个尺寸（见
    /// `automation_core::runner` 的 `ensure_calibrated_size`），
    /// 客户端的最小尺寸不允许时才会转人工——绝不按错的尺寸去点。
    /// 真实模式下这一项必须有值（[`build_runner`] 会拦），演练模式下用不上。
    pub calibrated_window: Option<WindowGeometry>,
    /// 按显示器缩放保存的多份界面标定（窗口尺寸 + 区域 + 导航搜索区 + 滚动落点）。
    ///
    /// 真实模式装配时按**当前**缩放挑一份；没有匹配的就拒绝开跑。
    /// 顶层的 `calibrated_window` / `regions` / `area_marks` / `nav_strip` / `scroll_anchor`
    /// 是「界面标定」页正在编辑的工作副本；保存时会 upsert 进这里。
    /// 旧配置只有顶层字段时，加载/保存时会自动迁成一份。
    #[serde(default)]
    pub calibrations: Vec<CalibrationSnapshot>,
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
    /// 参与匹配的图标：**图标名**，可以多个。
    ///
    /// ## 为什么存的是名字，不是路径
    ///
    /// 一个名字底下可以有**任意多张图**（`data/icons/聊天/1.png`、`2.png`…）：
    /// 同一个图标在选中 / 未选中 / 带气泡提醒 / 气泡里数字不一样时长得都不一样，
    /// 而它们指的是**同一个**图标。
    ///
    /// 配置引用名字，载入时才展开成"这个名字下的全部图"。这样以后补一张变体
    /// 不用回配置页重勾一次——每补一张都要重勾，迟早会漏，而漏掉的那张**不报任何错**，
    /// 只表现为「这个状态下匹配不上」。
    ///
    /// ## 为什么必须有多张
    ///
    /// 只留一张（比如只留未选中态）会出现：上一次运行点完，界面**停在这个视图上**，
    /// 图标随之变成选中态；这一次跑的时候画面上是选中态，于是再也匹配不上。
    ///
    /// 模板要**自己截**（界面上「图标库」页，或 `screen_probe template`），
    /// 不要指望程序自动裁一个：程序猜出来的模板会把"点错了地方"变成一次
    /// 看起来完全正常的运行——匹配分数照样很高，因为它匹配的是它自己刚裁的那块。
    pub nav_icon_templates: Vec<String>,
    /// 「用于对话历史导航」勾上的图标名（常见「聊天」）。
    ///
    /// 列表扫描式在扫会话列表之前，要先点它把视图切到聊天历史页。
    /// 与 [`Self::nav_icon_templates`]（通讯录 / 联系人）是两套，不要混。
    #[serde(default)]
    pub chat_history_nav_templates: Vec<String>,
    /// 图标库目录。**留空 = 用默认**（项目根下的 `data/icons/`）。
    ///
    /// 默认放在项目里而不是 AppData：AppData 底下那层目录名是包标识符，
    /// 没人记得住，找一次要翻半天；而图标模板是人对着屏幕一张一张框出来的素材。
    ///
    /// 写相对路径时按**项目根**解析（例如 `data/icons`），换盘符/换机器都不会失效。
    pub icons_dir: Option<String>,
    /// 图标模板匹配的最低分数（0–1），低于它转人工。
    ///
    /// 界面上那个「测试图标匹配」按钮就是用来量这个值的：它会报出当前画面上
    /// 的最高分。**不要照抄默认值**——阈值定低了会点错图标，定高了会频繁转人工。
    pub nav_icon_min_score: f32,
    /// 导航图标搜索区（相对窗口比例 `[x, y, w, h]`）。
    pub nav_strip: [f32; 4],
    /// 界面打开时**默认选中**的那条路。见 [`automation_core::Workflow`]。
    ///
    /// ★★ **真正跑的那条路不在这里。** 它是本次任务的运行参数 [`RunChoice`]，
    /// 由 `start_task` 从任务请求里取，**既不读这个字段、也不写回配置**。
    /// 这个字段只负责给界面一个初始值（"上次是这样"），改它不影响任何任务。
    ///
    /// 之所以拆开：放在配置里就必然出现两处真相——界面改的是草稿、
    /// 命令层读的是已保存的那份，于是「界面上选了 A、跑的是 B」。
    ///
    /// ## 为什么要显式选，而不是自动判断
    ///
    /// 「找联系人」有两条完全不同的路：在顶部搜索框里打字、从联想下拉里挑人；
    /// 或者在会话列表里往下滚、用 OCR 一行行认名字。两者看的是**不同的界面**，
    /// 需要的标定区域也不同（搜索式要 `main_search` / `search_dropdown` /
    /// `contact_profile`，列表式只用 `list_area`）。
    ///
    /// 而它们的失败现象一模一样：**「找不到联系人」**。自动判断一旦选错，
    /// 现场就分不清是"搜索没生效"还是"列表里真的没有这个人"——
    /// 这两种情况的处置方向完全相反。所以由操作者显式指定。
    pub workflow: Workflow,
    /// 「只做导航」时要点哪一个图标（只有 [`Workflow::NavigateOnly`] 读它）。
    ///
    /// 值是**图标库里的名字**——`data/icons/` 下的一级目录名，与
    /// [`RunChoice::nav_target`] 同一个值域。
    ///
    /// 与 [`Self::workflow`] 同理：这只是界面上的初始值，
    /// **本次任务要点哪个**由 [`RunChoice::nav_target`] 决定。
    ///
    /// 把"找图标 → 点它"单独拎出来跑一遍，是为了在图标匹配不准时**一眼看出来**：
    /// 混在完整流程里的话，点错图标的症状会表现为"找不到联系人"，
    /// 排查方向会一路偏向 OCR。
    pub nav_target: String,
    /// 位置先验的分数容差，`0` = **关掉**先验。
    ///
    /// 导航栏是一列纵向排列、彼此长得很像的图标，逐张模板取最高分时偶尔会出现
    /// "旁边那个图标分数略高一点"。而这件事有先验可用：**越靠近导航区中心的
    /// 命中越可信**。容差决定"分数差多少以内才允许用位置来取舍"——
    /// 定大了等于用位置替代了识别，定小了先验基本不生效。
    pub icon_prior_score_tolerance: f32,
    /// 逐字输入时**字符之间**的间隔（毫秒）。`0` = 不留间隔。
    ///
    /// 为什么要有间隔：输入框带联想（搜索框尤其明显），一次性灌进去的字符
    /// 可能被联想逻辑吞掉或重排。逐字输入 + 间隔是"像人一样打字"的最小代价。
    ///
    /// 为什么是**配置项**而不是写死：不同机器上客户端处理输入的速度差很多，
    /// 写死一个数必然在某些机器上偏快（丢字）、在另一些机器上白等。
    pub typing_interval_ms: u64,
    /// 资料页里"进入聊天"那个入口上的文字（默认「发消息」）。
    ///
    /// 做成配置而不是写死：这是**靶标相关的文字**，换一个客户端版本就可能不一样
    /// （本机靶标是微信 4.x，`window_class` 默认值也是为它改过的）。
    /// 写死的话，症状是"资料页滚到底了却找不到入口"——看不出是文字对不上。
    pub profile_chat_entry_text: String,
    /// 搜索下拉里可作为「联系人」的分组标题（默认「联系人 / 最常使用」）。
    ///
    /// 可写多项（`/`、`、`、空白分隔）。Mac 微信上人有时只出现在「最常使用」底下。
    /// 标题取错的话，会把「聊天记录里提到这个名字」当成联系人。
    pub search_contact_group_label: String,
    /// 界面标定出来的**新增区域**（键 = `calibration::ITEMS` 里的 `key`）。
    ///
    /// ## 为什么是 map，不是又一组具名字段
    ///
    /// 标定项会随流程完善而增长，而每加一项都要人对着屏幕重新框一次。
    /// 每加一项都去动 Rust 结构体、TypeScript 类型、界面元数据表三处，
    /// 漏一处**不会报错**——只表现为界面上少一个入口，或者多一个永远存不进去的框。
    /// 存成 map 之后，清单只有 `calibration.rs` 一处，界面把它当数据渲染。
    ///
    /// ## 为什么没有默认值
    ///
    /// [`Self::regions`] 里那四个区域是有默认值的（编排层必须拿到它们），
    /// 而这里的区域对应的步骤还没有编排代码。**没标就是没有**：
    /// 给一个猜出来的默认值，症状会是「任务照常跑完，只是点到了别的地方」。
    ///
    /// ## 为什么存 `AreaMark` 而不是裸的 `[f32; 4]`
    ///
    /// 比例本身跨窗口尺寸可用，但界面元素（左侧图标栏、头像列）是**固定像素宽**的，
    /// 换了窗口尺寸就得重标（见 `docs/todo.md` T2）。所以每一项都要能回答
    /// "这一份比例是在多大的窗口上量的"——只存四个浮点数是答不出来的。
    pub area_marks: calibration::AreaMarks,
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
            calibrations: Vec::new(),
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
            chat_history_nav_templates: Vec::new(),
            // 留空 = 项目根下的 `data/icons/`，由 `icon_library::resolve_dir` 定夺。
            icons_dir: None,
            nav_icon_min_score: DEFAULT_NAV_ICON_MIN_SCORE,
            nav_strip: flatten(DEFAULT_NAV_STRIP),
            // 默认走**搜索式**：它不依赖"列表里滚得到人"，是操作者当下要的那条路。
            // 列表扫描式仍然完整保留（`ScrollListContact`），改这一项即可切回去。
            workflow: Workflow::SearchContact,
            // 空串 = 还没选过。界面上会显示成「（还没选）」，选完才生效。
            nav_target: String::new(),
            icon_prior_score_tolerance: DEFAULT_ICON_PRIOR_SCORE_TOLERANCE,
            // 30ms 的依据：比人手打字快，又给客户端的联想逻辑留出处理时间。
            // 这是**间隔**不是超时，累加起来也就每字符 30ms，不影响总时长。
            typing_interval_ms: 30,
            profile_chat_entry_text: DEFAULT_PROFILE_CHAT_ENTRY_TEXT.to_string(),
            search_contact_group_label: DEFAULT_SEARCH_CONTACT_GROUP_LABEL.to_string(),
            // 空表 = 一个新增区域都还没标。这不是"缺失"，是如实反映现状：
            // 界面会把它们显示成「未标定」，而不是画一个猜出来的框。
            area_marks: calibration::AreaMarks::new(),
        }
    }
}

/// 把 [`RelativeRegion`] 摊平成配置里用的 `[x, y, w, h]`。
fn flatten(region: RelativeRegion) -> [f32; 4] {
    [region.x, region.y, region.width, region.height]
}

/// 从标定结果里取一个**新增区域**的比例。
///
/// 键就是标定清单里的 `key`（`calibration::ITEMS`），所以这里不需要一张映射表——
/// 多一张表就多一处会跟清单不同步的地方。
///
/// `None` = 还没标过。**不兜底、不猜**：给一个默认值的话，症状会是
/// "任务照常跑完，只是点到了别的地方"，那是本项目最难查的一类现象。
/// 缺哪些区域会在装配期被拦下（见 [`build_runner`]）。
/// 可见性是 `pub(super)`：`requirements.rs` 也要按它判断"这块标了没有"，
/// 但它不该出现在 `crate::runtime::` 的公开面上（外面只认 `WorkflowRequirement`）。
pub(super) fn mark_region(config: &RuntimeConfig, key: &str) -> Option<RelativeRegion> {
    config.area_marks.get(key).map(|mark| {
        let [x, y, width, height] = mark.rect;
        RelativeRegion::new(x, y, width, height)
    })
}

impl RuntimeConfig {
    pub fn to_runner_config(&self) -> RunnerConfig {
        let (contact_panel, chat_header, chat_body, composer) = self.regions.to_runner_regions();
        RunnerConfig {
            platform_label: self.mode.platform_label(),
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
            // 这里读的是**配置里的默认模式**。真正开跑时 `build_runner` 会用
            // 请求带来的模式重算一遍——判断只有 `RuntimeMode::calibrated_window` 一处。
            calibrated_window: self.mode.calibrated_window(self.calibrated_window),
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
            workflow: self.workflow,
            // 同上：装配期会用**本次请求**选的那个图标名覆盖它，并在那里把
            // `nav_icon_templates` 一起填上（两者必须同时改，见核心层字段的文档）。
            nav_target_label: self.nav_target.clone(),
            icon_prior_score_tolerance: self.icon_prior_score_tolerance,
            // 这四个区域来自「界面标定」页。**没标就是 `None`**，
            // 由装配期（真实/演练都算）按所选工作流的要求拦下。
            nav_bar: mark_region(self, "nav_bar"),
            main_search: mark_region(self, "main_search"),
            search_dropdown: mark_region(self, "search_dropdown"),
            contact_profile: mark_region(self, "contact_profile"),
            profile_chat_entry_text: self.profile_chat_entry_text.clone(),
            search_contact_group_label: self.search_contact_group_label.clone(),
            // 资料页是一整块可滚动内容，正中一定落在内容上，所以**不给配置项**：
            // 会话列表那个落点之所以可调，是因为要避开头像列与姓名列，
            // 而这里没有需要避开的东西。多一个旋钮就多一处会被设错的地方。
            profile_scroll_anchor: DEFAULT_PROFILE_SCROLL_ANCHOR,
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

/// 真实模式：装配当前操作系统的平台适配层与本地 OCR。
fn live_ports(config: &RuntimeConfig) -> Result<RunnerPorts, String> {
    use vision::{ExternalOcr, UnconfiguredOcr};

    let ocr: Arc<dyn LocalOcr> = match config.ocr_command.as_ref() {
        Some(command) if !command.trim().is_empty() => Arc::new(
            ExternalOcr::new(command.trim())
                .with_args(config.ocr_args.clone())
                .with_timeout(Duration::from_millis(config.ocr_timeout_ms.max(500))),
        ),
        _ => Arc::new(UnconfiguredOcr),
    };

    #[cfg(windows)]
    let platform: Arc<dyn automation_core::DesktopPlatform> = {
        use platform_windows::{WindowsDesktop, WindowsDesktopConfig};
        Arc::new(WindowsDesktop::new(WindowsDesktopConfig {
            wecom_exe: config.wecom_exe.as_ref().map(std::path::PathBuf::from),
            wecom_exe_sha256: config.wecom_exe_sha256.clone(),
            window_matcher: platform_windows::WindowMatcher::ClassName(config.window_class.clone()),
            // 逐字输入的字符间隔：搜索框是联想式的，连珠炮式地灌进去会让联想
            // 请求互相打断，而下拉列表只按第一个字符的结果定格——
            // 现象是"搜出来的东西不对"，不会让人想到是**输入太快**。
            typing_interval: Duration::from_millis(config.typing_interval_ms),
            // 其余取默认值：粘贴后清空剪贴板、600ms 剪贴板读取等待、4M 像素捕获上限。
            ..WindowsDesktopConfig::default()
        }))
    };

    #[cfg(target_os = "macos")]
    let platform: Arc<dyn automation_core::DesktopPlatform> = {
        use platform_macos::{MacOSDesktop, MacOSDesktopConfig};
        Arc::new(MacOSDesktop::new(MacOSDesktopConfig {
            wecom_exe: config.wecom_exe.as_ref().map(std::path::PathBuf::from),
            wecom_exe_sha256: config.wecom_exe_sha256.clone(),
            // macOS 上 window_class 表示「所有者名」（应用显示名），见 platform-macos 文档。
            window_matcher: platform_macos::WindowMatcher::ClassName(config.window_class.clone()),
            typing_interval: Duration::from_millis(config.typing_interval_ms),
            ..MacOSDesktopConfig::default()
        }))
    };

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (config, ocr);
        return Err("真实模式目前只支持 Windows 与 macOS".to_string());
    }

    #[cfg(any(windows, target_os = "macos"))]
    {
        Ok(RunnerPorts {
            platform,
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
}

/// 载入一组导航图标模板：把配置里的**图标名**展开成它底下的**全部图**，再逐张读进来。
///
/// `what` 是这组模板对应的目标名——「只做导航」传的是**图标目录名**，
/// 切视图那条路传的是「联系人」。**必须传**：图标库里通常有四五个图标，
/// 报错时不说清是哪一个，人只会去改错的那一个。
///
/// **必须在任务登记之前调用**：图标不存在、尺寸不像图标，都要在
/// "任务还没进列表"的时候就报出来。否则列表里会留下一条注定失败的记录，
/// 看起来像是真的跑过——而它连第一步都没走完。
///
/// 校验规则对两个模式**完全一致**。演练模式本来可以放宽（替身不读图片），
/// 但那样就会出现"演练一路通过、切到真实模式立刻报错"，而报错的那一刻
/// 任务已经登记了。宁可在演练模式也要求配一张真图片。

/// 解析「用于某某导航」勾选的图标名；为空时按常见目录名回退。
fn resolve_nav_icon_names(
    icons_dir: &std::path::Path,
    configured: &[String],
    fallbacks: &[&str],
    what: &str,
    checkbox_label: &str,
) -> Result<Vec<String>, String> {
    let mut names: Vec<String> = configured
        .iter()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .collect();
    if names.is_empty() {
        for candidate in fallbacks {
            if !crate::icon_library::variants_of(icons_dir, candidate).is_empty() {
                names.push((*candidate).to_string());
                break;
            }
        }
    }
    if names.is_empty() {
        let available = crate::icon_library::list(icons_dir)
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.name)
            .collect::<Vec<_>>();
        let available = if available.is_empty() {
            "（图标库是空的）".to_string()
        } else {
            available.join("、")
        };
        return Err(format!(
            "要先点「{what}」导航图标，但还没指定用哪一个。\n\
· 「只做导航」能点，是因为任务页另选了图标——那是另一套。\n\
· 请到「图标库」把对应图标勾上「{checkbox_label}」，再保存配置。\n\
· 当前库里有：{available}\n\
· 图标库目录：{}",
            icons_dir.display()
        ));
    }
    Ok(names)
}

pub(crate) fn load_nav_icon_templates(
    icons_dir: &std::path::Path,
    names: &[String],
    what: &str,
) -> Result<Vec<IconTemplate>, String> {
    let paths = crate::icon_library::resolve_selection(icons_dir, names)?;
    if paths.is_empty() {
        return Err(format!(
            "没有配置「{what}」图标的模板，无法定位它。\
             到「图标库」页对着那个图标截一张图、框出来存下来，\
             再把它勾进这一组（当前图标库目录：{}）。",
            icons_dir.display()
        ));
    }

    let mut templates = Vec::with_capacity(paths.len());
    for path in &paths {
        // label 取**相对图标库目录**的路径（`聊天/2.png`）：失败信息里要能一眼看出
        // 是哪个图标的哪一张出的问题。用完整路径会让一行日志变得很长。
        let label = path
            .strip_prefix(icons_dir)
            .unwrap_or(path)
            .display()
            .to_string()
            .replace('\\', "/");
        let template = vision::load_icon_template(path, label)
            .map_err(|err| format!("图标模板不可用：{err}"))?;
        templates.push(template);
    }
    Ok(templates)
}


/// 本次任务走哪一条路 —— **运行参数，不进配置文件**。
///
/// ★★ 为什么不放进 [`RuntimeConfig`]：配置回答的是"这台机器怎么配"，
/// 而这条路回答的是"这一次要做什么"。放进配置就必然出现**两处真相**——
/// 界面改的是草稿、`start_task` 读的是已保存的那份，于是表现为
/// 「界面上选了列表扫描式、跑的还是搜索式」。
/// 2026-09-19 实测：连跑三条任务，三条 `task-*.log` 里记的全是 `SearchContact`。
///
/// 做成运行参数之后：**界面选的与真正跑的是同一个值**，而且不写进 `config.json`——
/// 换一次任务就选一次，不留痕，也不需要为了跑一次先去点「保存配置」。
// 注意**没有 `Copy`**：`nav_target` 是图标库目录名（`String`），不再是枚举。
// 加上 `Copy` 会得到一个指向别处内存的浅拷贝——那是本项目最不想看到的一类错。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunChoice {
    /// 这一次跑**演练还是真实**。
    ///
    /// 它和 [`RuntimeConfig::mode`] 是同一个枚举，但回答的是不同的问题：
    /// 配置里那个是"这台机器默认怎么跑"，这里是"这一次怎么跑"。
    ///
    /// ## 为什么模式也必须由请求带
    ///
    /// 它踩的是**同一个坑**：界面上的下拉改的是草稿、`start_task` 读的是已保存的
    /// `state.config`，于是"界面上切成真实模式、实际按演练跑"（或者反过来——
    /// **那个方向更危险**：以为在演练、其实在真实客户端上操作）。
    /// 工作流那条路 2026-09-19 已经改成运行参数，模式当时**刻意留着没动**
    /// （影响面更大），现在一并改掉。
    ///
    /// ## 它不只是"换一组端口"
    ///
    /// 模式还决定三件事，全部在装配期落地：
    /// - 用哪一组端口（替身 / 真实）；
    /// - **要不要带标定窗口**（演练模式没有"真实窗口尺寸"这回事）；
    /// - 核心层审计里的"平台"字段（`dry-run` 还是 `windows`）。
    ///
    /// 所以它是 `RunChoice` 的字段，而不是 `build_runner` 的第四个参数：
    /// 这三个判断都只该有一处。
    pub mode: RuntimeMode,
    /// 走哪一条路。见 [`automation_core::Workflow`]。
    ///
    /// ## 为什么要显式选，而不是自动判断
    ///
    /// 「找联系人」有两条完全不同的路：在顶部搜索框里打字、从联想下拉里挑人；
    /// 或者在会话列表里往下滚、用 OCR 一行行认名字。两者看的是**不同的界面**，
    /// 需要的标定区域也不同（搜索式要 `main_search` / `search_dropdown` /
    /// `contact_profile`，列表式只用 `list_area`）。
    ///
    /// 而它们的失败现象一模一样：**「找不到联系人」**。自动判断一旦选错，
    /// 现场就分不清是"搜索没生效"还是"列表里真的没有这个人"——
    /// 这两种情况的处置方向完全相反。所以由操作者显式指定。
    pub workflow: Workflow,
    /// 「只做导航」时要点哪一个图标——**本次任务**要点的那个
    /// （只有 [`Workflow::NavigateOnly`] 读它）。
    ///
    /// ## 为什么是**图标库里的名字**，不是一个写死的枚举
    ///
    /// 它要回答的问题是"点哪个图标"，而这个问题的答案**只在图标库里**：
    /// `data/icons/` 下的一级目录名（发现 / 收藏夹 / 聊天历史 / 通讯录…）。
    ///
    /// 早先这里是一个只有两个变体的枚举（`Contact` / `History`），于是界面上
    /// 只能显示「联系人图标」「聊天历史图标」两个抽象名字，而操作者手里的图标
    /// 有四五个——**选不出来，也对不上**。名字换成图标库里的目录名之后，
    /// 界面上列的就是他实际存的那几个，选哪个就点哪个。
    ///
    /// 一个目录下的**全部图**一起参与匹配、取最高分：它们本来就是同一个图标的
    /// 不同状态（选中 / 未选中 / 带气泡），不该被当成不同图标比高低。
    ///
    /// 空串 = 还没选。装配期会直接拒绝，而不是猜一个默认图标——
    /// 猜错的表现是"点了一个别的图标，任务照常往下跑"。
    pub nav_target: String,
}

impl Default for RunChoice {
    fn default() -> Self {
        Self {
            // 默认走**演练**：它是安全的那一侧。这份默认值只在测试与
            // "请求里没带"（不可能，字段必填）时用到。
            mode: RuntimeMode::DryRun,
            workflow: Workflow::SearchContact,
            nav_target: String::new(),
        }
    }
}

/// 组装一个可运行的 [`WorkflowRunner`]。
///
/// `icons_dir` 是**图标库目录**（由调用方按 [`RuntimeConfig::icons_dir`] 解析好）。
/// 之所以从外面传进来而不是在这里算：算它需要"配置没写时的兜底目录"，
/// 那是应用状态才知道的事（见 `AppState::icons_dir`）。
///
/// `choice` 是**本次任务**要跑的那条路（运行参数，来自任务请求，见 [`RunChoice`]）。
/// 它决定跑演练还是真实、用哪几块标定区域、要不要点导航图标；**不写回配置**。
/// 注意它带来的模式**会覆盖** `config.mode`——配置里那个只是"这台机器的默认值"。
///
/// `confirmation` 由调用方注入：真实界面走 [`crate::confirmation::UiConfirmation`]，
/// 因此"人工确认"不是被跳过的环节，而是真的会阻塞等待操作者。
pub fn build_runner(
    config: &RuntimeConfig,
    choice: &RunChoice,
    task: &SendTask,
    icons_dir: &std::path::Path,
    audit: Arc<dyn AuditSink>,
    ledger: Arc<dyn SendLedger>,
    confirmation: Arc<dyn HumanConfirmation>,
) -> Result<WorkflowRunner, String> {
    // ★★ 第一件事：把**本次请求带来的模式**并进配置副本。
    //
    // 放在所有校验之前，是因为下面每一处"模式相关"的判断，问的都是
    // "**这一次**跑的是演练还是真实"：
    // - 真实模式没有标定尺寸就拒绝开跑；
    // - 挑替身端口还是真实端口；
    // - 审计里记 `dry-run` 还是当前系统；
    // - 要不要把标定窗口交给核心层（演练模式没有"真实窗口尺寸"这回事）。
    //
    // 这四处**只该有一个判据**。所以不在这里逐条 `if`，而是把模式本身换掉，
    // 让它们照旧读 `config.mode` —— 之后再加第五处也不会漏。
    //
    // ⚠️ 只改**内存里的副本**：不写回 `state.config`、不落盘。这正是
    // 「界面上选的那个模式，不点保存也生效」的落点。
    let mut config = config.clone();
    config.mode = choice.mode;

    // 真实模式下没有标定尺寸就拒绝开跑。客户端由操作者手动启动，程序没法从窗口外面
    // 分辨"这是不是我标定过的那个窗口、是不是那个尺寸"，只能靠这条记录。
    // 少了它，"按标定尺寸工作"就只是一句口号：尺寸变了区域会整体偏移，而点击
    // 落偏的后果是点到别的地方——宁可停在原地让人把窗口恢复回去。
    if config.mode == RuntimeMode::Live {
        config.ensure_calibrations_migrated();
        if config.calibrations.is_empty() {
            return Err(
                "真实模式必须先在「界面标定」页记录至少一份窗口标定并保存：                 先点「记录窗口尺寸」，再框区域。任务按当前显示器缩放挑选对应那份标定；                 缩放对不上会直接拒绝，绝不用错缩放的区域去点。"
                    .to_string(),
            );
        }
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

    // ── 所选工作流必须的标定区域 ────────────────────────────────
    //
    // 和上面两条同一个道理：**放在装配期**。装配失败**不会在任务列表里
    // 留下记录**，装配成功才会登记。所以能提前判的一律提前判。
    let missing = missing_marks(&config, choice);
    if !missing.is_empty() {
        return Err(format!(
            "「{}」还缺 {} 块没标定的区域：{}。\
             请到「界面标定」页按提示切到对应界面、截图、把这几块框出来，\
             保存配置后再跑——否则会在走到那一步时转人工，\
             而那时任务已经登记进列表了。",
            choice.workflow.describe(),
            missing.len(),
            missing.join("、")
        ));
    }

    // ── 靶标文字不能是空的 ──────────────────────────────────────
    //
    // 空串在"包含"判断里**匹配一切**：空的分组标题会让下拉里的第一行
    // 被当成「联系人」组的标题，于是后面整段判据全部错位——而任务照样跑完。
    // 这属于"配置写错了"而不是"界面上没有"，所以在装配期拦。
    for (value, label, key) in [
        (
            &config.profile_chat_entry_text,
            "资料页进入聊天的入口文字",
            "profile_chat_entry_text",
        ),
        (
            &config.search_contact_group_label,
            "搜索下拉里联系人分组的标题",
            "search_contact_group_label",
        ),
    ] {
        if value.trim().is_empty() {
            return Err(format!(
                "「{label}」（{key}）不能留空：空文字在「包含」判断里会匹配到任何一行，\
                 结果不是「找不到」而是找错。填上客户端上实际显示的那几个字。"
            ));
        }
    }

    let mut ports = match config.mode {
        RuntimeMode::DryRun => dry_run_ports(&config, task),
        RuntimeMode::Live => live_ports(&config)?,
    };
    // 确认端口始终来自界面，保证真实模式下也必须人工确认。
    ports.confirmation = confirmation;

    // 真实模式：按**当前**显示器缩放挑一份标定，覆盖工作副本后再交给核心层。
    // 演练模式没有真窗口，继续用配置里正在编辑的那一份（或默认值）。
    if config.mode == RuntimeMode::Live {
        // 装配期还没 focus，必须用「定位目标窗 → 所在屏缩放」，与「记录窗口尺寸」同源。
        let (_rect, metrics) = ports
            .platform
            .measure_target_window()
            .map_err(|err| format!("读取目标窗口所在显示器缩放失败：{err}"))?;
        let snap = config.pick_calibration(metrics.scale_factor)?.clone();
        config.apply_snapshot(&snap);
    }

    let mut runner_config = config.to_runner_config();

    // 运行参数覆盖「配置里的默认值」。
    //
    // `to_runner_config` 是纯粹的"配置 → 核心层结构"映射，它照抄的是配置里
    // 那个**默认值**字段；本次真正要跑的是请求带来的这一份。**只改内存里的副本**，
    // 不回写 `state.config`、也不落盘——这正是「选工作流不要保存」的落点。
    //
    // ⚠️ 模式不在这里覆盖：它在本函数**开头**就换掉了 `config.mode`，
    // 所以 `to_runner_config()` 出来的 `platform_label` / `calibrated_window`
    // 已经是本次的那一份。再覆盖一遍等于把判据写成两处。
    runner_config.workflow = choice.workflow;

    // ── 导航图标：这一次要点哪一个 ──────────────────────────────
    //
    // 「只做导航」那条路**导航就是任务本身**。
    // 搜索式 → 通讯录/联系人；列表扫描式 → 聊天/对话历史；只做导航 → 任务页所选。
    let navigate_only = choice.workflow == Workflow::NavigateOnly;
    let search_contact = choice.workflow == Workflow::SearchContact;
    let scroll_list = choice.workflow == Workflow::ScrollListContact;
    // 搜索式 → 通讯录图标；列表扫描式 → 聊天历史图标；只做导航 → 任务页所选。
    let need_nav = navigate_only || search_contact || scroll_list || config.navigate_before_search;

    if need_nav {
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
                 它是归一化互相相关系数，1.0 表示完全一致。",
                config.nav_icon_min_score
            ));
        }
        if config.icon_prior_score_tolerance < 0.0 {
            return Err(format!(
                "位置先验的分数容差不能是负数（当前 {}）：\
                 它是「最高分往下多少以内才允许用位置取舍」的幅度，\
                 0 表示关掉先验。",
                config.icon_prior_score_tolerance
            ));
        }

        if navigate_only {
            let name = choice.nav_target.trim();
            if name.is_empty() {
                return Err(
                    "「只做导航」要指定点哪一个图标，而现在还没选。\
                     到「图标库」页把那个图标截下来存好，再回到「任务」页的\
                     「要点哪一个图标」里选它。"
                        .to_string(),
                );
            }
            runner_config.nav_target_label = name.to_string();
            runner_config.nav_icon_templates =
                load_nav_icon_templates(icons_dir, &[name.to_string()], name)?;
        } else if scroll_list {
            // 列表扫描式：先切到「聊天 / 对话历史」页，再扫会话列表。
            let names = resolve_nav_icon_names(
                icons_dir,
                &config.chat_history_nav_templates,
                &["聊天", "微信"],
                "对话历史",
                "用于对话历史导航",
            )?;
            runner_config.nav_target_label = names[0].clone();
            runner_config.nav_icon_templates =
                load_nav_icon_templates(icons_dir, &names, &names[0])?;
        } else {
            // 搜索式，或旧开关 navigate_before_search：切「通讯录 / 联系人」。
            let names = resolve_nav_icon_names(
                icons_dir,
                &config.nav_icon_templates,
                &["通讯录", "联系人"],
                "通讯录/联系人",
                "用于联系人导航",
            )?;
            runner_config.nav_target_label = names[0].clone();
            runner_config.nav_icon_templates =
                load_nav_icon_templates(icons_dir, &names, &names[0])?;
        }
    }

    Ok(WorkflowRunner::new(ports, runner_config)
        .with_audit(audit)
        .with_ledger(ledger))
}

#[cfg(test)]
mod tests;
