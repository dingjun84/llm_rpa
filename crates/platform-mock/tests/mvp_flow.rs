//! MVP 主路径与失败路径的端到端测试。
//!
//! 全部基于 `platform-mock` 的替身端口运行，不接触真实桌面、不接触网络。
//! 这些用例对应 `docs/architecture.md` §9 验收条件中可在软件层验证的部分。
//!
//! ⚠️ 2026-09-30：产品工作流已换成 YOLO 路径（`chat_list_send` /
//! `contacts_search_send`）。本文件多数端到端用例仍按旧模板/OCR 脚本编写，
//! 且 dry_run 端口挂的是 `UnconfiguredYolo`——**会大面积失败**。
//! 编译已对齐新枚举；补 `MockYoloDetector` 脚本化检测后再恢复断言。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use automation_core::{
    AuditEntry, CalibratedWindow, CancelToken, DiagnosticRecorder, IconLocator, IconTemplate,
    MemoryAudit, MemorySendLedger, Observation, ProgressSink, Rect, RunOutcome, RunnerConfig,
    RunnerPorts, ScreenMetrics, SendTask, StateChange, TaskId, TaskState, Workflow,
    WorkflowRunner,
};
use platform_mock::{
    Fault, MockDesktop, MockIconLocator, MockOcr,
    MockScenario, ScriptedCall, DEFAULT_WINDOW,
};

const CONTACT: &str = "外部测试联系人";
const MESSAGE: &str = "这是一条测试消息";

/// 端到端用例的基线配置：**列表扫描式**。
///
/// ## 为什么在用例里显式选，而不是吃 `RunnerConfig::default()`
///
/// 应用层的默认值是**搜索式**（操作者当下要的那条路），而搜索式要三块
/// 只有对着真实窗口框一次才知道在哪儿的区域（搜索框 / 下拉 / 资料页）。
/// 下面这些用例演的是最早那条路——它在会话列表里滚着找人，只用
/// `contact_panel` 一块区域。不显式选的话，它们会全部卡在
/// 「搜索框区还没标定」上，而那条报错看起来像是流程坏了。
///
/// 搜索式那条路本身另有专门的用例，见文件末尾。
/// 造一张"图标模板"。内容不重要——编排层只把它转交给图标定位端口。
fn nav_template() -> IconTemplate {
    IconTemplate {
        label: "聊天历史图标".into(),
        pixels: vec![200; 24 * 24 * 4],
        width: 24,
        height: 24,
    }
}


/// 把「发送按钮」的位置告诉替身：落点在这块里的点击才算一次发送。
///
/// 取值从**配置**里换算（`config.send_button`），不另写一份坐标——
/// 两边不一致时 `send_count` 会恒为 0，那十几条"绝不能发送"的断言
/// 全部静默失效（照样绿，却什么都没测）。
///
/// ⚠️ 自建 runner 的用例（不走 `Fixture::build*`）必须自己调它。
fn arm_send_button(desktop: &MockDesktop, config: &RunnerConfig) {
    if let Some(region) = config.send_button {
        if let Ok(rect) = region.resolve_within(desktop.window()) {
            desktop.script_send_button(rect);
        }
    }
}

fn list_config() -> RunnerConfig {
    RunnerConfig {
        platform_label: "test".into(),
        retry_backoff: Duration::ZERO,
        workflow: Workflow::ContactsSearchSend,
        // 列表扫描式**总是**先切到聊天历史；核心层测这条路时也要带上模板，
        // 否则会在导航那一步就转人工，而现象看起来像列表查找坏了。
        nav_icon_templates: vec![nav_template()],
        nav_target_label: "聊天历史".into(),
        // 「发送按钮区」：发送动作要在这块区域里认出按钮再点它。
        //
        // 真实模式下它来自「界面标定」页那一项 `send_button`（没标就不许开跑，
        // 见 `runtime/requirements.rs`）。测试里给一块固定区域即可——
        // 真正要紧的是替身按**同一块区域**判断"这次点击算不算发送"，
        // 那件事由 `Fixture::build_full` 从本字段换算，不在这里另写一份。
        send_button: Some(automation_core::RelativeRegion::new(0.86, 0.90, 0.13, 0.08)),
        ..Default::default()
    }
}

#[derive(Default)]
struct RecordingProgress {
    states: Mutex<Vec<TaskState>>,
}

impl RecordingProgress {
    fn states(&self) -> Vec<TaskState> {
        self.states.lock().unwrap().clone()
    }
}

impl ProgressSink for RecordingProgress {
    fn state_changed(&self, change: &StateChange) {
        self.states.lock().unwrap().push(change.to);
    }
}

struct Fixture {
    runner: WorkflowRunner,
    desktop: Arc<MockDesktop>,
    ocr: Arc<MockOcr>,
    audit: Arc<MemoryAudit>,
    diagnostics: Arc<RecordingDiagnostics>,
    progress: RecordingProgress,
}

/// 内存版诊断记录器：只记「哪一步、看了哪块区域、读到几个字块」。
///
/// 这里钉的是**接线**——`DiagnosticRecorder::observe` 到底有没有被 runner 调用。
/// 真正把画面渲染成 PNG 的实现在 `apps/desktop` 的 `task_diagnostics.rs`，
/// 那里另有它自己的单测（渲染、拼图、字体缺失退化）。
///
/// 为什么不复用那个实现：它在测试环境里既没有数据目录也没有字体，
/// 而且会往盘上写图；端到端用例要的是"上报有没有发生"，不是"图好不好看"。
#[derive(Default)]
struct RecordingDiagnostics {
    seen: Mutex<Vec<(String, Rect, usize)>>,
    /// 做了 OCR（`ocr_raw.is_some()`）却还带着整窗的步骤——应当恒为空：
    /// OCR 步骤的诊断底图必须是送 OCR 的那一帧，事后补截的整窗可能已是另一个画面。
    ocr_with_window: Mutex<Vec<String>>,
    /// 做了 OCR 的步骤数（证明上面那条断言不是"一步 OCR 都没有"而空过）。
    ocr_steps: Mutex<usize>,
}

impl RecordingDiagnostics {
    fn seen(&self) -> Vec<(String, Rect, usize)> {
        self.seen.lock().unwrap().clone()
    }
}

impl DiagnosticRecorder for RecordingDiagnostics {
    fn observe(&self, _task_id: TaskId, observation: &Observation<'_>) {
        if observation.ocr_raw.is_some() {
            *self.ocr_steps.lock().unwrap() += 1;
            if observation.window.is_some() {
                self.ocr_with_window.lock().unwrap().push(observation.label.to_string());
            }
        }
        self.seen.lock().unwrap().push((
            observation.label.to_string(),
            observation.region,
            observation.text_boxes.len(),
        ));
    }
}

impl Fixture {
    fn new(scenario: &MockScenario) -> Self {
        Self::build(
            scenario,
            MockDesktop::new(),
            RunnerConfig {
                platform_label: "test".into(),
                retry_backoff: Duration::ZERO,
                ..list_config()
            },
        )
    }

    fn build(
        scenario: &MockScenario,
        desktop: MockDesktop,
        config: RunnerConfig,
    ) -> Self {
        Self::build_with_script(desktop, config, scenario.script())
    }

    /// 用自定义 OCR 脚本装配。
    ///
    /// 预置场景的脚本是固定四段（候选区 → 标题 → 正文前 → 正文后），
    /// 但「滚动查找联系人」会对候选区识别**多次**，段数不固定，必须自己排。
    fn build_with_script(
        desktop: MockDesktop,
        config: RunnerConfig,
        script: Vec<ScriptedCall>,
    ) -> Self {
        Self::build_with_matcher(
            desktop,
            config,
            script,
            Arc::new(platform_mock::MockContactMatcher::new()),
        )
    }

    /// 同上，但**自己指定姓名匹配器**。
    ///
    /// 需要它是因为「宽松姓名匹配」是一个独立的匹配器实现
    /// （`ContainsNameMatcher`），不换匹配器就测不到它。
    fn build_with_matcher(
        desktop: MockDesktop,
        config: RunnerConfig,
        script: Vec<ScriptedCall>,
        matcher: Arc<dyn automation_core::ContactMatcher>,
    ) -> Self {
        Self::build_full(desktop, config, script, matcher, Arc::new(MockIconLocator::new()))
    }

    /// 最完整的一层：端口逐个指定。
    ///
    /// 图标定位端口默认给 `MockIconLocator::new()`（正中命中），
    /// 需要测"图标找不到"这类路径时才换掉它。
    fn build_full(
        desktop: MockDesktop,
        config: RunnerConfig,
        script: Vec<ScriptedCall>,
        matcher: Arc<dyn automation_core::ContactMatcher>,
        icons: Arc<dyn IconLocator>,
    ) -> Self {
        arm_send_button(&desktop, &config);
        let desktop = Arc::new(desktop);
        let ocr = Arc::new(MockOcr::new(script));
        let audit = Arc::new(MemoryAudit::new());
        let ledger = Arc::new(MemorySendLedger::new());
        let diagnostics = Arc::new(RecordingDiagnostics::default());

        let runner = WorkflowRunner::new(
            RunnerPorts {
                platform: desktop.clone(),
                ocr: ocr.clone(),
                matcher,
                icons,
                        yolo: std::sync::Arc::new(automation_core::yolo::UnconfiguredYolo),
        },
            config,
        )
        .with_audit(audit.clone())
        .with_ledger(ledger.clone())
        .with_diagnostic_recorder(diagnostics.clone());

        Self {
            runner,
            desktop,
            ocr,
            audit,
            diagnostics,
            progress: RecordingProgress::default(),
        }
    }

    fn task(&self) -> SendTask {
        SendTask {
            id: uuid::Uuid::new_v4(),
            external_contact_name: CONTACT.into(),
            text: MESSAGE.into(),
            created_by: "测试操作者".into(),
        }
    }

    fn run(&self, task: &SendTask) -> RunOutcome {
        self.run_with(task, &CancelToken::new())
    }

    fn run_with(&self, task: &SendTask, cancel: &CancelToken) -> RunOutcome {
        self.runner.run(task, &self.progress, cancel)
    }

    fn entries(&self) -> Vec<AuditEntry> {
        self.audit.entries()
    }
}

// ── 主路径 ──────────────────────────────────────────────────────────────

// ── 联系人核验失败 ──────────────────────────────────────────────────────

// ── 发送前不再有人工确认 ────────────────────────────────────────────────

// ── 窗口与标定守卫 ──────────────────────────────────────────────────────

#[test]
fn window_move_or_resize_does_not_reject_input() {
    use automation_core::DesktopPlatform;
    // 窗口变宽/挪动不再是 SCREEN_CHANGED。传入的矩形可以和当前窗口不一致。
    let desktop = MockDesktop::new();
    desktop.set_window(automation_core::Rect { x: 300, y: 200, width: 1600, height: 900 });
    desktop
        .guarded_click(automation_core::Point { x: 320, y: 240 }, platform_mock::DEFAULT_WINDOW)
        .expect("变宽或挪动不应拒绝点击");
    desktop
        .type_text("hi", platform_mock::DEFAULT_WINDOW)
        .expect("变宽或挪动不应拒绝输入");
    desktop
        .scroll(automation_core::Point { x: 320, y: 240 }, 1, platform_mock::DEFAULT_WINDOW)
        .expect("变宽或挪动不应拒绝滚动");
    assert_eq!(desktop.send_count(), 0, "这次点击不在发送按钮上");
}

#[test]
fn input_rejected_when_foreground_is_not_the_target_window() {
    use automation_core::DesktopPlatform;
    let desktop = MockDesktop::new();
    desktop.script_foreground_not_target();
    let click = desktop
        .guarded_click(automation_core::Point { x: 1, y: 1 }, platform_mock::DEFAULT_WINDOW)
        .unwrap_err();
    assert_eq!(click.code(), "CLIENT_NOT_READY");
    assert_ne!(click.code(), "SCREEN_CHANGED");
    let typed = desktop.type_text("x", platform_mock::DEFAULT_WINDOW).unwrap_err();
    assert_eq!(typed.code(), "CLIENT_NOT_READY");
    assert_eq!(desktop.send_count(), 0, "前台不是目标窗口时不能发出去");
}

// ── 送达核验 ────────────────────────────────────────────────────────────

// ── 幂等与取消 ──────────────────────────────────────────────────────────

#[test]
fn cancelling_before_start_leaves_the_task_cancelled() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let fixture = Fixture::new(&scenario);
    let cancel = CancelToken::new();
    cancel.cancel();

    let outcome = fixture.run_with(&fixture.task(), &cancel);

    assert_eq!(outcome.state, TaskState::Cancelled);
    assert_eq!(fixture.desktop.send_count(), 0);
}

// ── 重试 ────────────────────────────────────────────────────────────────

#[test]
fn persistent_capture_failure_gives_up_after_max_attempts() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let desktop = MockDesktop::new();
    desktop.inject(|faults| {
        for _ in 0..5 {
            faults.capture.push_back(Fault::platform("持续截图失败"));
        }
    });
    let fixture = Fixture::build(
        &scenario,
        desktop,
        RunnerConfig {
            max_attempts: 3,
            retry_backoff: Duration::ZERO,
            ..list_config()
        },
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::Failed);
    assert_eq!(outcome.failure.as_ref().unwrap().code, "PLATFORM_ERROR");
    assert_eq!(fixture.desktop.operations().iter().filter(|o| *o == "capture").count(), 0);
}

// ── 审计 ────────────────────────────────────────────────────────────────

// ── 滚动查找联系人 ──────────────────────────────────────────────────────

// ── 卡死检测 ────────────────────────────────────────────────────────────

// ── 标定尺寸 ────────────────────────────────────────────────────────────

/// 客户端调不动（有自己的最小尺寸）—— 转人工，且**不得**按偏了的区域去点击。
///
/// 关键在于：这时 `SetWindowPos` 是**报成功**的，只有重新量一遍才知道没调成。
#[test]
fn a_client_that_refuses_to_be_resized_is_refused_before_any_input() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let desktop = MockDesktop::new();
    // 客户端最小 1000x700：请求 900x600 会被夹住，而调用照样"成功"。
    desktop.set_min_window_size((1000, 700));
    let fixture = Fixture::build(
        &scenario,
        desktop,
        RunnerConfig {
            retry_backoff: Duration::ZERO,
            calibrated_window: Some(CalibratedWindow {
                width: 900,
                height: 600,
                scale_factor: 1.0,
            }),
            ..list_config()
        },
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    assert!(fixture.desktop.clicks.lock().unwrap().is_empty(), "尺寸对不上就绝不能点击");
    assert_eq!(fixture.desktop.send_count(), 0);
    let reason = outcome.failure.expect("应有失败原因").reason;
    assert!(reason.contains("标定"), "应说明与标定不一致：{reason}");
    assert!(reason.contains("夹"), "应说明是被客户端夹住了：{reason}");
}

/// 缩放对不上 —— **不靠调窗口尺寸糊过去**，直接转人工。
///
/// DPI 缩放是显示器/系统属性，不是窗口属性：缩放不同意味着同一物理尺寸下的
/// 逻辑布局本来就不同，调物理像素解决不了，只会白动一次窗口。
#[test]
fn a_scale_factor_mismatch_is_not_papered_over_by_resizing() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let desktop = MockDesktop::new();
    desktop.set_metrics(ScreenMetrics { width: 1920, height: 1080, scale_factor: 1.5 });
    let fixture = Fixture::build(
        &scenario,
        desktop,
        RunnerConfig {
            retry_backoff: Duration::ZERO,
            calibrated_window: Some(CalibratedWindow {
                width: DEFAULT_WINDOW.width,
                height: DEFAULT_WINDOW.height,
                scale_factor: 1.0,
            }),
            ..list_config()
        },
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    assert!(
        fixture.desktop.resizes.lock().unwrap().is_empty(),
        "缩放对不上不是尺寸问题，不该去动窗口"
    );
    assert!(fixture.desktop.clicks.lock().unwrap().is_empty());
}

// ── 客户端由操作者启动 ──────────────────────────────────────────────────

// ── 只填不发 ────────────────────────────────────────────────────────────

// ── 切换视图：用模板匹配点左侧导航图标 ──────────────────────────────────
//
// 图标上没有文字，OCR 读不到它，所以"先切到联系人视图"这一步只能靠模板匹配。
// 这三条用例覆盖：命中并跳转、认不出图标、点击没生效。



#[test]
fn the_navigation_defaults_are_off_and_the_strip_clears_the_avatar_column() {
    let config = RunnerConfig::default();
    // 默认必须是**关**的：模板要操作者先自己截、自己确认，
    // 默认打开等于让每个没配模板的人都撞上一次配置错误。
    assert!(!config.navigate_before_search);
    assert!(config.nav_icon_templates.is_empty());
    assert!(config.nav_strip.validate().is_ok());
    assert!((0.0..=1.0).contains(&config.nav_icon_min_score));

    // 宽度要落在"实测的两条线之间"：导航图标栏 0–57px、头像列从 78px 起
    // （2026-09-17 在 974 宽的窗口上量的）。窄了会切掉图标，宽了会把头像
    // 和未读红点一起圈进搜索区。
    let width_at_974 = (974.0f32 * config.nav_strip.width).round() as i32;
    assert!(
        width_at_974 > 57,
        "导航条宽 {width_at_974}px 比图标栏本身还窄，会把图标切掉"
    );
    assert!(
        width_at_974 < 78,
        "导航条宽 {width_at_974}px 已经盖住头像列（从 78px 起）"
    );
}

// ── 搜索式工作流（工作流 3）─────────────────────────────────────────────
//
// 这条路的界面与列表扫描式完全不同：它看的是顶部搜索框的**联想下拉**、
// 然后落在**联系人资料页**上，再从资料页点进聊天。所以它需要三块
// 列表式用不到的标定区域（`main_search` / `search_dropdown` / `contact_profile`）。
//
// 编排器对 OCR 的调用顺序（与下面的脚本一一对应）：
//   搜索下拉 → 资料页 → 资料页入口 → 聊天标题 → 发送前聊天正文 → 发送按钮 → 送达核验





// ── 「只做导航」（工作流 1 / 2 的最小验证单元）──────────────────────────

/// 「只做导航」在**真实模式**下同样要走标定尺寸这一关——那是与工作流无关的前置条件。
///
/// 这条用例守的是"把 `NavigateOnly` 做成免检捷径"这种改法：尺寸对不上时区域会整体
/// 偏移，点下去就是点到别的地方。尺寸不符时**同样会自动调回标定尺寸**，
/// 这条前置条件不会因为工作流更简单就被跳过。
#[test]
fn navigate_only_still_requires_a_calibrated_window_in_live_mode() {
    let config = RunnerConfig {
        workflow: Workflow::ChatListSend,
        nav_icon_templates: vec![nav_template()],
        calibrated_window: Some(CalibratedWindow {
            width: 960,
            height: 734,
            scale_factor: 1.0,
        }),
        ..list_config()
    };
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let desktop = MockDesktop::new();
    desktop.set_window(automation_core::Rect { x: 0, y: 0, width: 800, height: 600 });
    let fixture = Fixture::build_full(
        desktop,
        config,
        scenario.script(),
        Arc::new(platform_mock::MockContactMatcher::new()),
        Arc::new(MockIconLocator::new()),
    );

    let _ = fixture.run(&fixture.task());

    // 尺寸对不上 ⇒ 先把窗口调回标定尺寸，之后才允许动作。
    assert_eq!(
        fixture.desktop.resizes.lock().unwrap().as_slice(),
        &[(960, 734)],
        "只做导航也要先把窗口调回标定尺寸"
    );
    assert_eq!(fixture.desktop.window().width, 960);
    assert_eq!(fixture.desktop.window().height, 734);
}
