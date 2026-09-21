//! MVP 主路径与失败路径的端到端测试。
//!
//! 全部基于 `platform-mock` 的替身端口运行，不接触真实桌面、不接触网络。
//! 这些用例对应 `docs/architecture.md` §9 验收条件中可在软件层验证的部分。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use automation_core::{
    AuditEntry, CalibratedWindow, CancelToken, DiagnosticRecorder, IconLocator, IconTemplate,
    MemoryAudit, MemorySendLedger, Observation, ProgressSink, Rect, RunOutcome, RunnerConfig,
    RunnerPorts, ScreenMetrics, SendTask, StateChange, TaskId, TaskState, Workflow, WorkflowRunner,
    DEFAULT_NAV_STRIP,
};
use platform_mock::{
    tb, ConfirmationOutcome, Fault, MockDesktop, MockHumanConfirmation, MockIconLocator, MockOcr,
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

fn list_config() -> RunnerConfig {
    RunnerConfig {
        platform_label: "test".into(),
        retry_backoff: Duration::ZERO,
        workflow: Workflow::ScrollListContact,
        // 列表扫描式**总是**先切到聊天历史；核心层测这条路时也要带上模板，
        // 否则会在导航那一步就转人工，而现象看起来像列表查找坏了。
        nav_icon_templates: vec![nav_template()],
        nav_target_label: "聊天历史".into(),
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
    confirmation: Arc<MockHumanConfirmation>,
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
}

impl RecordingDiagnostics {
    fn seen(&self) -> Vec<(String, Rect, usize)> {
        self.seen.lock().unwrap().clone()
    }
}

impl DiagnosticRecorder for RecordingDiagnostics {
    fn observe(&self, _task_id: TaskId, observation: &Observation<'_>) {
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
            MockHumanConfirmation::default(),
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
        confirmation: MockHumanConfirmation,
        config: RunnerConfig,
    ) -> Self {
        Self::build_with_script(desktop, confirmation, config, scenario.script())
    }

    /// 用自定义 OCR 脚本装配。
    ///
    /// 预置场景的脚本是固定四段（候选区 → 标题 → 正文前 → 正文后），
    /// 但「滚动查找联系人」会对候选区识别**多次**，段数不固定，必须自己排。
    fn build_with_script(
        desktop: MockDesktop,
        confirmation: MockHumanConfirmation,
        config: RunnerConfig,
        script: Vec<ScriptedCall>,
    ) -> Self {
        Self::build_with_matcher(
            desktop,
            confirmation,
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
        confirmation: MockHumanConfirmation,
        config: RunnerConfig,
        script: Vec<ScriptedCall>,
        matcher: Arc<dyn automation_core::ContactMatcher>,
    ) -> Self {
        Self::build_full(desktop, confirmation, config, script, matcher, Arc::new(MockIconLocator::new()))
    }

    /// 最完整的一层：端口逐个指定。
    ///
    /// 图标定位端口默认给 `MockIconLocator::new()`（正中命中），
    /// 需要测"图标找不到"这类路径时才换掉它。
    fn build_full(
        desktop: MockDesktop,
        confirmation: MockHumanConfirmation,
        config: RunnerConfig,
        script: Vec<ScriptedCall>,
        matcher: Arc<dyn automation_core::ContactMatcher>,
        icons: Arc<dyn IconLocator>,
    ) -> Self {
        let desktop = Arc::new(desktop);
        let ocr = Arc::new(MockOcr::new(script));
        let confirmation = Arc::new(confirmation);
        let audit = Arc::new(MemoryAudit::new());
        let ledger = Arc::new(MemorySendLedger::new());
        let diagnostics = Arc::new(RecordingDiagnostics::default());

        let runner = WorkflowRunner::new(
            RunnerPorts {
                platform: desktop.clone(),
                ocr: ocr.clone(),
                matcher,
                icons,
                confirmation: confirmation.clone(),
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
            confirmation,
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

#[test]
fn happy_path_reaches_completed() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let fixture = Fixture::new(&scenario);
    let task = fixture.task();

    let outcome = fixture.run(&task);

    assert_eq!(outcome.state, TaskState::Completed, "失败原因：{:?}", outcome.failure);
    assert!(outcome.succeeded());
    assert_eq!(fixture.desktop.send_count(), 1);
    assert_eq!(fixture.desktop.pasted_texts(), vec![MESSAGE.to_string()]);
    assert_eq!(fixture.confirmation.call_count(), 1);
}

#[test]
fn happy_path_visits_every_documented_state_in_order() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let fixture = Fixture::new(&scenario);
    let task = fixture.task();

    fixture.run(&task);

    assert_eq!(
        fixture.progress.states(),
        vec![
            TaskState::LaunchingClient,
            TaskState::WaitingForClient,
            TaskState::NavigatingToView,
            TaskState::SearchingContact,
            TaskState::VerifyingCandidate,
            TaskState::VerifyingChatHeader,
            TaskState::PreparingMessage,
            TaskState::AwaitingHumanConfirmation,
            TaskState::Sending,
            TaskState::VerifyingDelivery,
            TaskState::Completed,
        ]
    );
}

#[test]
fn ocr_only_receives_calibrated_sub_regions_never_the_full_window() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let fixture = Fixture::new(&scenario);
    fixture.run(&fixture.task());

    let calls = fixture.ocr.calls.lock().unwrap().clone();
    assert_eq!(calls, vec![(358, 634), (922, 72), (922, 518), (922, 518)]);
    assert!(!calls.contains(&(1280, 720)), "不得扫描整屏");
}

#[test]
fn click_lands_on_the_matched_contact_centre_in_screen_coordinates() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let fixture = Fixture::new(&scenario);
    fixture.run(&fixture.task());

    let clicks = fixture.desktop.clicks.lock().unwrap().clone();
    // 三次受守卫的点击：导航图标 → 联系人 → 消息输入框。
    assert_eq!(clicks.len(), 3, "应先点导航，再点联系人，再聚焦消息输入框");

    // 期望值由**配置算出来**，不写死屏幕坐标。
    //
    // 脚本里那个候选文字框的图像坐标是 (8, 20, 200, 28)，中心 (108, 34)。
    // 核心层要把它加上**候选区原点的屏幕偏移**——这一步正是本用例要钉的东西：
    // 少加或多加一次原点偏移，点击就会落到别的联系人身上。
    // 而候选区默认值本身是会变的（左边界从 0.0 挪到 0.14 就是一次，为了让开
    // 头像列），写死的数字必然过期；区域默认值由 `runtime.rs` 里的用例单独钉住。
    let panel = RunnerConfig::default().contact_panel.resolve(DEFAULT_WINDOW);
    assert_eq!(clicks[1].x, panel.x + 108, "联系人点击横坐标应含候选区原点偏移");
    assert_eq!(clicks[1].y, panel.y + 34, "联系人点击纵坐标应含候选区原点偏移");
    // 输入框区默认标定 [0.28,0.82,0.72,0.18]，窗口 1280x720
    // → 区域 (358,590,922,130)，中心 (819,655)
    assert_eq!(clicks[2].x, 819);
    assert_eq!(clicks[2].y, 655);
}

#[test]
fn the_message_input_box_is_focused_before_pasting() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let fixture = Fixture::new(&scenario);
    fixture.run(&fixture.task());

    // 输入框必须**先**被点击，粘贴才会落到正确位置。
    let clicks = fixture.desktop.clicks.lock().unwrap().clone();
    let pasted = fixture.desktop.pasted.lock().unwrap().clone();
    assert_eq!(clicks.len(), 3, "导航 + 联系人 + 输入框");
    assert_eq!(pasted.len(), 1, "只应粘贴一次");
    assert!(fixture.desktop.send_count() >= 1);
}

#[test]
fn evidence_records_screenshot_fingerprints() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let fixture = Fixture::new(&scenario);
    let outcome = fixture.run(&fixture.task());

    assert!(outcome.evidence.iter().any(|e| e.starts_with("contact_panel#")));
    assert!(outcome.evidence.iter().any(|e| e.starts_with("chat_header#")));
    assert!(outcome.evidence.iter().any(|e| e.starts_with("chat_before#")));
    assert!(outcome.evidence.iter().any(|e| e.starts_with("chat_after#")));
}

// ── 联系人核验失败 ──────────────────────────────────────────────────────

#[test]
fn duplicate_contact_names_stop_the_task_before_any_input() {
    let scenario = MockScenario::duplicate_contact(CONTACT, MESSAGE);
    let fixture = Fixture::new(&scenario);

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    assert_eq!(outcome.failure.as_ref().unwrap().code, "AMBIGUOUS_VISION");
    assert_eq!(fixture.desktop.send_count(), 0);
    // 列表式总会先点一次导航图标；但不得再点联系人或输入框。
    assert_eq!(
        fixture.desktop.clicks.lock().unwrap().len(),
        1,
        "歧义时只允许导航那一次点击，不得点联系人/输入框"
    );
}

/// **严格匹配器**会拒绝近似名。
///
/// 用例名里的「strict matcher」是刻意的：它装配的是 `MockContactMatcher`
/// （内层 `StrictContactMatcher`），而**生产默认走的是放宽层**
/// （`RunnerConfig`/`RuntimeConfig` 的 `relaxed_name_match` 默认为 true）。
/// 放宽层下同一个近似名**会被接受** —— 那是已知代价，
/// 由 `the_relaxed_matcher_accepts_a_near_name_and_that_is_the_known_cost` 钉住。
/// 不写清楚这一点，很容易把这条用例误读成「近似名永远不会被匹配」。
#[test]
fn a_near_name_is_rejected_by_the_strict_matcher() {
    let scenario = MockScenario::near_name_only(CONTACT, MESSAGE);
    let fixture = Fixture::new(&scenario);

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    assert_eq!(fixture.desktop.send_count(), 0);
}

/// 放宽层下，近似名**会被当成目标发出去** —— 把代价钉成可执行的事实。
///
/// **为什么必须有一条这样的用例**：`docs/architecture.md` §6.4/§6.6 要求姓名逐字精确匹配，
/// 放宽层是 2026-09-17 操作者为了先跑通链路而明确要求的临时措施。
/// 它把「找不到人」换成了「**可能找错人**」——而后者更危险：
/// 找不到人只是转人工，找错人是把消息发给了别人。
///
/// 这条用例不是"验收通过"，而是**把风险写在能被 CI 看见的地方**：
/// 一旦有人以为放宽层是安全的，它会立刻提醒代价是什么。
/// 关闭方式：`relaxed_name_match = false`（界面上的「宽松姓名匹配」）。
#[test]
fn the_relaxed_matcher_accepts_a_near_name_and_that_is_the_known_cost() {
    let scenario = MockScenario::near_name_only(CONTACT, MESSAGE);
    let fixture = Fixture::build_with_matcher(
        MockDesktop::new(),
        MockHumanConfirmation::default(),
        RunnerConfig { retry_backoff: Duration::ZERO, ..list_config() },
        scenario.script(),
        Arc::new(automation_core::ContainsNameMatcher::default()),
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(
        outcome.state,
        TaskState::Completed,
        "放宽层会接受「{CONTACT}丰」——这就是它的代价，不是意外。失败原因：{:?}",
        outcome.failure
    );
    assert_eq!(
        fixture.desktop.send_count(),
        1,
        "消息被发给了这个并非目标的近似名 —— 这正是放宽层最危险的地方"
    );
}

#[test]
fn low_confidence_contact_is_rejected() {
    let scenario = MockScenario::low_confidence_contact(CONTACT, MESSAGE);
    let fixture = Fixture::new(&scenario);

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    assert_eq!(fixture.desktop.send_count(), 0);
}

#[test]
fn chat_header_mismatch_blocks_the_send() {
    let scenario = MockScenario::header_mismatch(CONTACT, MESSAGE);
    let fixture = Fixture::new(&scenario);

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    assert_eq!(fixture.desktop.send_count(), 0);
    assert!(fixture.confirmation.calls.lock().unwrap().is_empty(), "未通过核验不得请求确认");
}

#[test]
fn login_prompt_stops_the_task() {
    let scenario = MockScenario::login_prompt(CONTACT, MESSAGE);
    let fixture = Fixture::new(&scenario);

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    assert_eq!(fixture.desktop.send_count(), 0);
}

// ── 人工确认 ────────────────────────────────────────────────────────────

#[test]
fn rejected_confirmation_blocks_the_send() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let fixture = Fixture::build(
        &scenario,
        MockDesktop::new(),
        MockHumanConfirmation::new(ConfirmationOutcome::Reject("内容不对".into())),
        RunnerConfig { retry_backoff: Duration::ZERO, ..list_config() },
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    assert_eq!(fixture.desktop.send_count(), 0);
}

#[test]
fn expired_confirmation_blocks_the_send() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let fixture = Fixture::build(
        &scenario,
        MockDesktop::new(),
        MockHumanConfirmation::new(ConfirmationOutcome::Expired),
        RunnerConfig { retry_backoff: Duration::ZERO, ..list_config() },
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    assert_eq!(fixture.desktop.send_count(), 0);
}

#[test]
fn confirmation_is_requested_with_the_configured_ttl() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let fixture = Fixture::build(
        &scenario,
        MockDesktop::new(),
        MockHumanConfirmation::default(),
        RunnerConfig {
            confirmation_ttl: Duration::from_secs(45),
            retry_backoff: Duration::ZERO,
            ..list_config()
        },
    );

    fixture.run(&fixture.task());

    assert_eq!(fixture.confirmation.last_ttl(), Some(Duration::from_secs(45)));
}

// ── 窗口与标定守卫 ──────────────────────────────────────────────────────

#[test]
fn window_replaced_before_the_click_is_detected() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let desktop = MockDesktop::new();
    // 第一次 focus 用于标定，第二次 focus（点击前守卫）返回另一个窗口。
    desktop.script_windows([
        platform_mock::DEFAULT_WINDOW,
        automation_core::Rect { x: 300, y: 200, width: 900, height: 600 },
    ]);
    let fixture = Fixture::build(
        &scenario,
        desktop,
        MockHumanConfirmation::default(),
        RunnerConfig { retry_backoff: Duration::ZERO, ..list_config() },
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    assert_eq!(outcome.failure.as_ref().unwrap().code, "SCREEN_CHANGED");
    assert_eq!(fixture.desktop.send_count(), 0);
}

#[test]
fn display_scale_change_is_detected_before_input() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let desktop = MockDesktop::new();
    desktop.script_metrics([
        platform_mock::DEFAULT_METRICS,
        automation_core::ScreenMetrics { width: 1920, height: 1080, scale_factor: 1.5 },
    ]);
    let fixture = Fixture::build(
        &scenario,
        desktop,
        MockHumanConfirmation::default(),
        RunnerConfig { retry_backoff: Duration::ZERO, ..list_config() },
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    assert_eq!(outcome.failure.as_ref().unwrap().code, "SCREEN_CHANGED");
    assert_eq!(fixture.desktop.send_count(), 0);
}

// ── 送达核验 ────────────────────────────────────────────────────────────

#[test]
fn missing_message_in_chat_body_is_not_reported_as_delivered() {
    let scenario = MockScenario::delivery_missing(CONTACT, MESSAGE);
    let fixture = Fixture::new(&scenario);

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    assert_ne!(outcome.state, TaskState::Completed);
}

#[test]
fn unchanged_screen_after_send_is_not_reported_as_delivered() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let desktop = MockDesktop::new();
    desktop.freeze_fingerprints();
    let fixture = Fixture::build(
        &scenario,
        desktop,
        MockHumanConfirmation::default(),
        RunnerConfig { retry_backoff: Duration::ZERO, ..list_config() },
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
}

// ── 幂等与取消 ──────────────────────────────────────────────────────────

#[test]
fn the_same_task_cannot_be_sent_twice() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let mut script = scenario.script();
    script.extend(scenario.script());
    let desktop = Arc::new(MockDesktop::new());
    let audit = Arc::new(MemoryAudit::new());
    let ledger = Arc::new(MemorySendLedger::new());
    let runner = WorkflowRunner::new(
        RunnerPorts {
            platform: desktop.clone(),
            ocr: Arc::new(MockOcr::new(script)),
            matcher: Arc::new(platform_mock::MockContactMatcher::new()),
            icons: Arc::new(MockIconLocator::new()),
            confirmation: Arc::new(MockHumanConfirmation::default()),
        },
        RunnerConfig { retry_backoff: Duration::ZERO, ..list_config() },
    )
    .with_audit(audit)
    .with_ledger(ledger);

    let task = SendTask {
        id: uuid::Uuid::new_v4(),
        external_contact_name: CONTACT.into(),
        text: MESSAGE.into(),
        created_by: "测试操作者".into(),
    };

    let first = runner.run(&task, &automation_core::NoopProgress, &CancelToken::new());
    let second = runner.run(&task, &automation_core::NoopProgress, &CancelToken::new());

    assert_eq!(first.state, TaskState::Completed);
    assert_eq!(second.state, TaskState::NeedsHumanReview);
    assert_eq!(desktop.send_count(), 1, "重复执行不得再次发送");
}

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
fn transient_capture_failure_is_retried_and_recovers() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let desktop = MockDesktop::new();
    desktop.inject(|faults| faults.capture.push_back(Fault::platform("瞬时截图失败")));
    let fixture = Fixture::build(
        &scenario,
        desktop,
        MockHumanConfirmation::default(),
        RunnerConfig {
            max_attempts: 3,
            retry_backoff: Duration::ZERO,
            ..list_config()
        },
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(
        outcome.state,
        TaskState::Completed,
        "失败原因：{:?}",
        outcome.failure
    );
    assert_eq!(fixture.ocr.call_count(), 4, "失败的尝试不应触发识别");
}

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
        MockHumanConfirmation::default(),
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

#[test]
fn audit_records_every_transition_and_never_stores_the_message_body() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let fixture = Fixture::new(&scenario);
    fixture.run(&fixture.task());

    let entries = fixture.entries();
    assert!(entries.len() >= 10, "每一步都应有审计记录");

    let rendered = format!("{entries:?}");
    assert!(!rendered.contains(MESSAGE), "审计记录不得包含消息正文");

    let sending = entries
        .iter()
        .find(|e| e.to == TaskState::Sending)
        .expect("应记录 Sending 状态");
    let digest = sending.message.as_ref().expect("Sending 应带消息摘要");
    assert_eq!(digest.char_count, MESSAGE.chars().count());
    assert_eq!(digest.sha256.len(), 64);
    assert!(sending.confirmation_at.is_some(), "Sending 应记录确认时间");
    assert_eq!(sending.actor, "测试操作者");
    assert_eq!(sending.platform, "test");
}

#[test]
fn audit_records_a_stable_failure_code_on_failure() {
    let scenario = MockScenario::duplicate_contact(CONTACT, MESSAGE);
    let fixture = Fixture::new(&scenario);
    fixture.run(&fixture.task());

    let last = fixture.entries().last().cloned().expect("应有审计记录");
    assert_eq!(last.to, TaskState::NeedsHumanReview);
    assert_eq!(last.failure_code.as_deref(), Some("AMBIGUOUS_VISION"));
    assert!(last.failure_reason.is_some());
}

#[test]
fn audit_never_contains_the_clipboard_payload_of_a_failed_send() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let desktop = MockDesktop::new();
    desktop.inject(|faults| faults.send = Some(Fault::platform("发送快捷键失败")));
    let fixture = Fixture::build(
        &scenario,
        desktop,
        MockHumanConfirmation::default(),
        RunnerConfig { retry_backoff: Duration::ZERO, ..list_config() },
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::Failed);
    let rendered = format!("{:?}", fixture.entries());
    assert!(!rendered.contains(MESSAGE));
}

// ── 滚动查找联系人 ──────────────────────────────────────────────────────

/// 目标不在第一屏，滚动两次后才出现 —— 应当能自己找到并继续走完。
#[test]
fn scrolling_finds_a_contact_that_is_not_on_the_first_screen() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    // 候选区识别会被调用多次：前两屏都没有目标，第三屏才出现。
    let script = vec![
        ScriptedCall::boxes(vec![tb("别的联系人甲", 20, 0.99)]),
        ScriptedCall::boxes(vec![tb("别的联系人乙", 20, 0.99)]),
        ScriptedCall::boxes(vec![tb(CONTACT, 20, 0.99)]),
        ScriptedCall::Ok(scenario.header.clone()),
        ScriptedCall::Ok(scenario.body_before.clone()),
        ScriptedCall::Ok(scenario.body_after.clone()),
    ];
    let fixture = Fixture::build_with_script(
        MockDesktop::new(),
        MockHumanConfirmation::default(),
        RunnerConfig { retry_backoff: Duration::ZERO, ..list_config() },
        script,
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::Completed, "失败原因：{:?}", outcome.failure);
    assert_eq!(fixture.desktop.scroll_count(), 2, "应当在两次滚动后找到目标");
    assert_eq!(fixture.desktop.scrolled_down_notches(), 6, "默认每次 3 格、滚 2 次");
    // 查找联系人只会向下滚。方向反了会越滚越远，所以把符号也钉住。
    assert!(
        fixture.desktop.scrolls.lock().unwrap().iter().all(|(_, n)| *n > 0),
        "查找联系人只应向下滚动"
    );
}

/// 滚动落点必须是「上下居中、左右偏右一点」，而不是候选区的正中心。
///
/// 为什么把这一点钉住：候选区的左边界 0.14 已经落在姓名那一列上，
/// 于是**正中心恰好压在姓名与消息预览的交界处**（实测姓名文字从屏幕 x≈140 起）。
/// 落点一旦飘回正中心，滚轮就可能落到不滚动列表的地方——现象是「滚了半天没反应」，
/// 那是现场最难查的一类失败。
#[test]
fn scrolling_happens_at_the_configured_anchor_not_the_panel_center() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let script = vec![
        ScriptedCall::boxes(vec![tb("别的联系人甲", 20, 0.99)]),
        ScriptedCall::boxes(vec![tb(CONTACT, 20, 0.99)]),
        ScriptedCall::Ok(scenario.header.clone()),
        ScriptedCall::Ok(scenario.body_before.clone()),
        ScriptedCall::Ok(scenario.body_after.clone()),
    ];
    let config = RunnerConfig { retry_backoff: Duration::ZERO, ..list_config() };
    // 期望值由配置**算出来**，不写死坐标：区域标定或落点比例一改，这里跟着走。
    let panel = config.contact_panel.resolve(DEFAULT_WINDOW);
    let expected = config.scroll_anchor.resolve(panel);

    let fixture = Fixture::build_with_script(
        MockDesktop::new(),
        MockHumanConfirmation::default(),
        config,
        script,
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::Completed, "失败原因：{:?}", outcome.failure);
    let points: Vec<_> = fixture
        .desktop
        .scrolls
        .lock()
        .unwrap()
        .iter()
        .map(|(point, _)| *point)
        .collect();
    assert!(!points.is_empty(), "应当至少滚动一次");
    assert!(
        points.iter().all(|point| *point == expected),
        "滚动落点应全部是 {expected:?}，实际 {points:?}"
    );

    let center = panel.center();
    assert_ne!(expected, center, "落点不该是区域正中心");
    assert!(expected.x > center.x, "横向应当偏右（避开姓名与预览的交界处）");
    assert!(
        (expected.y - center.y).abs() <= 1,
        "纵向应当是居中：落点 y={}，中心 y={}",
        expected.y,
        center.y
    );
}

/// 每轮识别到的文字必须进过程证据。
///
/// **为什么钉住它**：只记 `contact_panel#<指纹>` 的话，「找不到联系人」永远分不清
/// 两种原因——① OCR 把名字读错了（要调识别）；② 名字根本不在这屏（要改范围）。
/// 这两件事的处置完全相反，而现场只能靠猜。
#[test]
fn every_sweep_step_records_what_the_ocr_actually_read() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let script = vec![
        ScriptedCall::boxes(vec![tb("别的联系人甲", 20, 0.99)]),
        ScriptedCall::boxes(vec![tb(CONTACT, 20, 0.99)]),
        ScriptedCall::Ok(scenario.header.clone()),
        ScriptedCall::Ok(scenario.body_before.clone()),
        ScriptedCall::Ok(scenario.body_after.clone()),
    ];
    let fixture = Fixture::build_with_script(
        MockDesktop::new(),
        MockHumanConfirmation::default(),
        RunnerConfig { retry_backoff: Duration::ZERO, ..list_config() },
        script,
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::Completed, "失败原因：{:?}", outcome.failure);
    assert!(
        outcome.evidence.iter().any(|line| line.contains("别的联系人甲")),
        "过程证据里应当记下 OCR 实际读到的文字，实际：{:#?}",
        outcome.evidence
    );
    assert!(
        outcome.evidence.iter().any(|line| line.contains(CONTACT)),
        "读到目标的那一帧同样要记下来，实际：{:#?}",
        outcome.evidence
    );
}

/// 每一步「看了哪块区域、读到了什么」都必须交到诊断记录器手里。
///
/// **为什么钉住它**：诊断图（`data/tasks/<id>/steps/*.png`）是失败现场唯一的复盘手段，
/// 而它完全依赖 runner 主动上报。漏报不会让任何测试变红，只会让盘上的图
/// **少几步**——而少了哪几步恰恰是看不出来的，人只会以为"任务就是这么跑的"。
///
/// 这里同时钉住区域**不是整窗**：诊断图要回答的是"区域标定偏了没有"，
/// 画一块整窗等于把这个问题抹掉。
#[test]
fn every_read_step_reaches_the_diagnostic_recorder() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let fixture = Fixture::new(&scenario);

    let outcome = fixture.run(&fixture.task());
    assert_eq!(outcome.state, TaskState::Completed, "失败原因：{:?}", outcome.failure);

    let seen = fixture.diagnostics.seen();
    assert!(!seen.is_empty(), "runner 一次都没把观察交给诊断记录器");
    assert!(
        seen.iter().any(|(_, _, boxes)| *boxes > 0),
        "没有任何一步带上 OCR 文字块——那样渲染出来的图上会一个字都没有，实际：{seen:#?}"
    );
    for (label, region, _) in &seen {
        assert!(!region.is_degenerate(), "步骤「{label}」报上来的区域是空的：{region:?}");
        assert!(
            region.width < DEFAULT_WINDOW.width as i32
                || region.height < DEFAULT_WINDOW.height as i32,
            "步骤「{label}」报上来的是整窗 {region:?}——诊断图的价值就在于显示「只看了这一块」"
        );
    }
}

/// 关掉开关后，过程证据里**不该**出现识别到的文字，只留指纹。
#[test]
fn recording_what_was_read_can_be_switched_off() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let script = vec![
        ScriptedCall::boxes(vec![tb("别的联系人甲", 20, 0.99)]),
        ScriptedCall::boxes(vec![tb(CONTACT, 20, 0.99)]),
        ScriptedCall::Ok(scenario.header.clone()),
        ScriptedCall::Ok(scenario.body_before.clone()),
        ScriptedCall::Ok(scenario.body_after.clone()),
    ];
    let fixture = Fixture::build_with_script(
        MockDesktop::new(),
        MockHumanConfirmation::default(),
        RunnerConfig {
            retry_backoff: Duration::ZERO,
            log_ocr_candidates: false,
            ..list_config()
        },
        script,
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::Completed, "失败原因：{:?}", outcome.failure);
    assert!(
        outcome.evidence.iter().all(|line| !line.contains("别的联系人甲")),
        "关掉开关后不该记录识别到的文字，实际：{:#?}",
        outcome.evidence
    );
    assert!(
        outcome.evidence.iter().any(|line| line.starts_with("contact_panel#")),
        "指纹本身要照常记，关掉的只是「读到了什么」这一项，实际：{:#?}",
        outcome.evidence
    );
}

/// 日志里必须能查到「鼠标会被放到哪儿」和「点了哪儿」。
///
/// **为什么钉住它**：操作者反馈「鼠标没有移动到滚动区域」时，原来日志里只有
/// "读到几块"，**根本无从对照**——只能靠猜。落点与点击坐标是回答这个问题的
/// 唯一依据，而它们纯粹是诊断输出，最容易被后续重构顺手删掉。
#[test]
fn the_log_records_where_the_cursor_will_go() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let script = vec![
        ScriptedCall::boxes(vec![tb("别的联系人甲", 20, 0.99)]),
        ScriptedCall::boxes(vec![tb(CONTACT, 20, 0.99)]),
        ScriptedCall::Ok(scenario.header.clone()),
        ScriptedCall::Ok(scenario.body_before.clone()),
        ScriptedCall::Ok(scenario.body_after.clone()),
    ];
    let config = RunnerConfig { retry_backoff: Duration::ZERO, ..list_config() };
    // 期望坐标由配置**算出来**，不写死像素：区域标定或落点比例一改，这里跟着走。
    let panel = config.contact_panel.resolve(DEFAULT_WINDOW);
    let anchor = config.scroll_anchor.resolve(panel);

    let fixture = Fixture::build_with_script(
        MockDesktop::new(),
        MockHumanConfirmation::default(),
        config,
        script,
    );

    let outcome = fixture.run(&fixture.task());
    assert_eq!(outcome.state, TaskState::Completed, "失败原因：{:?}", outcome.failure);

    assert!(
        outcome.evidence.iter().any(|line| {
            line.starts_with("滚动落点")
                && line.contains(&format!("({}, {})", anchor.x, anchor.y))
        }),
        "日志里要能查到滚动落点 {anchor:?}，实际：{:#?}",
        outcome.evidence
    );
    assert!(
        outcome.evidence.iter().any(|line| line.starts_with("点击联系人")),
        "日志里要能查到点击坐标，实际：{:#?}",
        outcome.evidence
    );
    assert!(
        outcome.evidence.iter().any(|line| line.starts_with("聚焦输入框")),
        "日志里要能查到输入框坐标，实际：{:#?}",
        outcome.evidence
    );
}

/// OCR 把姓名读脏时，放宽匹配必须把链路跑通 —— 而且只放宽「怎么算命中」。
///
/// **为什么钉住它**：实测日志里 OCR 读到的姓名行是 `0 李四`（那个 `0` 是头像列的
/// 未读红点被 `Windows.Media.Ocr` **按行并进**了姓名块），而严格匹配要求逐字相等，
/// 于是「李四明明就在列表里」却永远匹配不上。这条用例复现同一形状：
/// 姓名行被污染成 `0 {CONTACT}`，同时群预览行 `{CONTACT}：…` 也包含目标名
/// （用户点名的待优化项），放宽层取**最短**的那个 ⇒ 落到姓名行上。
///
/// 用例里跑**两组对照**：同一条 OCR 脚本，放宽层成功、严格层转人工。
/// 少了严格那一组，放宽层以后被误改成「什么都匹配」时这条用例仍然是绿的。
#[test]
fn a_dirty_name_still_finds_the_contact_under_relaxed_matching() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    // 姓名行被前导噪声污染；群预览行是「发送者：消息内容」形状，同样包含目标名。
    let polluted = format!("0 {CONTACT}");
    let group_preview = format!("{CONTACT}：可以了，登录进去了");
    let script = vec![
        // 第 1 帧：目标还不在视野里，必须先滚一下。
        ScriptedCall::boxes(vec![tb("别的联系人甲", 20, 0.99)]),
        // 第 2 帧：两条候选都包含目标名，最短的那条才是姓名行。
        ScriptedCall::boxes(vec![tb(&polluted, 20, 0.99), tb(&group_preview, 90, 0.99)]),
        ScriptedCall::Ok(scenario.header.clone()),
        ScriptedCall::Ok(scenario.body_before.clone()),
        ScriptedCall::Ok(scenario.body_after.clone()),
    ];
    let config = RunnerConfig { retry_backoff: Duration::ZERO, ..list_config() };
    // 期望坐标由配置**算出来**，不写死像素：区域标定一改，这里跟着走。
    let panel = config.contact_panel.resolve(DEFAULT_WINDOW);
    // 姓名行那块的图像坐标：x/width 是替身 `tb()` 的固定形状，y 是本脚本给的。
    let name_box = automation_core::Rect { x: 8, y: 20, width: 200, height: 28 };
    let expected = name_box.center();

    let relaxed = Fixture::build_with_matcher(
        MockDesktop::new(),
        MockHumanConfirmation::default(),
        config.clone(),
        script.clone(),
        Arc::new(automation_core::ContainsNameMatcher::default()),
    );
    let outcome = relaxed.run(&relaxed.task());

    assert_eq!(outcome.state, TaskState::Completed, "失败原因：{:?}", outcome.failure);
    assert!(
        outcome.evidence.iter().any(|line| line.contains(&polluted)),
        "证据里要留下 OCR 读到的脏名字，否则现场无从判断放宽层有没有生效：{:#?}",
        outcome.evidence
    );

    // 点击必须落在**姓名行**（被污染的那条）上，而不是更长的群预览行。
    let clicks = relaxed.desktop.clicks.lock().unwrap().clone();
    assert_eq!(clicks.len(), 3, "导航 + 联系人 + 输入框");
    assert_eq!(
        (clicks[1].x, clicks[1].y),
        (panel.x + expected.x, panel.y + expected.y),
        "应点中姓名行，而不是更长的群预览行（`{group_preview}`）"
    );

    // 对照组：同一条脚本换成严格匹配 —— 必须转人工，证明上面那条真的是放宽层的功劳。
    let strict = Fixture::build_with_matcher(
        MockDesktop::new(),
        MockHumanConfirmation::default(),
        config,
        script,
        Arc::new(automation_core::StrictContactMatcher::default()),
    );
    let strict_outcome = strict.run(&strict.task());
    assert_eq!(
        strict_outcome.state,
        TaskState::NeedsHumanReview,
        "严格匹配下 `{polluted}` 不该命中，否则这条用例没有测到放宽层"
    );
    assert_eq!(strict.desktop.send_count(), 0, "严格匹配失败时绝不能发送");
}

/// 每轮向下扫都严格卡在上限上，两轮扫完仍未找到 —— 必须转人工，绝不能猜一个近似名。
#[test]
fn scrolling_gives_up_at_the_attempt_limit_and_asks_for_a_human() {
    // 脚本里永远只有别人，目标从不出现；脚本耗尽后 MockOcr 返回空列表。
    let fixture = Fixture::build_with_script(
        MockDesktop::new(),
        MockHumanConfirmation::default(),
        RunnerConfig {
            retry_backoff: Duration::ZERO,
            max_scroll_attempts: 3,
            ..list_config()
        },
        vec![ScriptedCall::boxes(vec![tb("别的联系人", 20, 0.99)])],
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    assert_eq!(fixture.desktop.send_count(), 0, "绝不能发送");

    // 每轮向下扫的次数必须**恰好**卡在 `max_scroll_attempts` 上：
    // 少一次是提前放弃，多一次就是把上限当摆设。
    // 向上滚的次数不计入——那是"回顶"和"确认到底"的开销，不是查找本身。
    let downward = fixture
        .desktop
        .scrolls
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, notches)| *notches > 0)
        .count();
    assert_eq!(
        downward,
        (3 * RunnerConfig::default().max_search_sweeps) as usize,
        "两轮扫描，每轮向下滚动都应恰好用满上限 3 次"
    );

    let reason = outcome.failure.expect("应有失败原因").reason;
    assert!(reason.contains("未找到"), "失败原因应说明查找失败：{reason}");
}

/// 列表只有一屏、上下都滚不动 —— 提前收工，不该把滚动上限耗完。
///
/// 注意这**不是**"客户端卡死"：系统判定它在响应，只是没有可滚动的内容。
/// 只看"画面没动"是不够的，判据见 `runner::ensure_not_frozen`。
#[test]
fn a_list_that_cannot_scroll_stops_early_instead_of_burning_the_limit() {
    let desktop = MockDesktop::new();
    // 底部偏移为 0：列表只有一屏，往下往上都滚不动。
    desktop.script_scroll_bottom(0);
    let fixture = Fixture::build_with_script(
        desktop,
        MockHumanConfirmation::default(),
        RunnerConfig {
            retry_backoff: Duration::ZERO,
            max_scroll_attempts: 50,
            ..list_config()
        },
        vec![ScriptedCall::boxes(vec![tb("别的联系人", 20, 0.99)])],
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    assert!(
        fixture.desktop.scroll_count() < 10,
        "画面第一次没变化就该收工，不该把 50 次上限用完（实际 {} 次）",
        fixture.desktop.scroll_count()
    );
    let reason = outcome.failure.expect("应有失败原因").reason;
    assert!(reason.contains("未找到"), "应说明是找不到，而不是卡死：{reason}");
    assert!(
        !reason.contains("卡死"),
        "列表滚不动不等于客户端卡死，不能误报：{reason}"
    );
}

/// 上下都滚不动、且系统判定窗口未响应 —— 判定客户端卡死，转人工。
#[test]
fn a_frozen_client_is_reported_as_frozen_instead_of_scrolled_to_the_bottom() {
    let desktop = MockDesktop::new();
    desktop.script_scroll_bottom(0);
    desktop.set_responsive(false);
    let fixture = Fixture::build_with_script(
        desktop,
        MockHumanConfirmation::default(),
        RunnerConfig { retry_backoff: Duration::ZERO, ..list_config() },
        vec![ScriptedCall::boxes(vec![tb("别的联系人", 20, 0.99)])],
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    assert_eq!(fixture.desktop.send_count(), 0, "卡死时绝不能发送");
    let reason = outcome.failure.expect("应有失败原因").reason;
    assert!(reason.contains("卡死"), "应判定为客户端卡死：{reason}");
}

/// 扫描期间新消息把目标顶到列表最上面 —— 第二轮从顶部重扫时能找到。
///
/// 这正是"下拉查找会错过"的那个场景：第一轮从头扫到底时目标并不在屏幕上，
/// 等它出现时那一屏已经被翻过去了。只扫一轮必然漏掉。
#[test]
fn a_contact_pushed_to_the_top_by_a_new_message_is_found_on_the_second_sweep() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let desktop = MockDesktop::new();
    // 列表正好三屏：偏移 0 到 9。
    desktop.script_scroll_bottom(9);

    let others = || ScriptedCall::boxes(vec![tb("别的联系人", 20, 0.99)]);
    let script = vec![
        // 第一轮：从头扫到底，目标一直不在屏幕上。
        others(),
        others(),
        others(),
        others(),
        others(),
        // 第二轮回到顶部 —— 扫描期间到达的新消息已经把它顶到了最上面。
        ScriptedCall::boxes(vec![tb(CONTACT, 20, 0.99)]),
        ScriptedCall::Ok(scenario.header.clone()),
        ScriptedCall::Ok(scenario.body_before.clone()),
        ScriptedCall::Ok(scenario.body_after.clone()),
    ];
    let fixture = Fixture::build_with_script(
        desktop,
        MockHumanConfirmation::default(),
        RunnerConfig { retry_backoff: Duration::ZERO, ..list_config() },
        script,
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::Completed, "失败原因：{:?}", outcome.failure);
    assert!(
        outcome.evidence.iter().any(|entry| entry == "contact_sweep#1"),
        "第一轮应当确实扫完了（证据里应留下轮次标记）：{:?}",
        outcome.evidence
    );
    // 目标是在**顶部**被找到的：找到时视图必须停在顶部。
    assert_eq!(fixture.desktop.view_offset(), 0, "找到目标时视图应停在列表顶部");
}

/// 只扫一轮（`max_search_sweeps = 1`）就会漏掉"被顶上去"的目标 —— 把差别钉住。
///
/// 这不是为了证明单轮没用，而是为了说明默认值为什么是 2：
/// 少了第二轮，"目标被新消息顶到最上面"就必然漏。
#[test]
fn a_single_sweep_misses_a_contact_that_jumped_to_the_top() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let desktop = MockDesktop::new();
    desktop.script_scroll_bottom(9);

    let others = || ScriptedCall::boxes(vec![tb("别的联系人", 20, 0.99)]);
    let script = vec![
        others(),
        others(),
        others(),
        others(),
        others(),
        // 第二轮才会读到的那个"被顶上去"的目标。单轮配置下永远读不到这一条。
        ScriptedCall::boxes(vec![tb(CONTACT, 20, 0.99)]),
        ScriptedCall::Ok(scenario.header.clone()),
        ScriptedCall::Ok(scenario.body_before.clone()),
        ScriptedCall::Ok(scenario.body_after.clone()),
    ];
    let fixture = Fixture::build_with_script(
        desktop,
        MockHumanConfirmation::default(),
        RunnerConfig {
            retry_backoff: Duration::ZERO,
            max_search_sweeps: 1,
            ..list_config()
        },
        script,
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    assert_eq!(fixture.desktop.send_count(), 0);
    let reason = outcome.failure.expect("应有失败原因").reason;
    assert!(reason.contains("未找到"), "应说明是找不到：{reason}");
}

/// 出现同名联系人时**不滚动重试** —— 滚动改变不了「名字是否唯一」。
#[test]
fn an_ambiguous_name_fails_immediately_without_scrolling() {
    let scenario = MockScenario::duplicate_contact(CONTACT, MESSAGE);
    let fixture = Fixture::new(&scenario);

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    assert_eq!(fixture.desktop.scroll_count(), 0, "歧义滚动也解决不了，不该白滚一轮");
    assert_eq!(outcome.failure.expect("应有失败原因").code, "AMBIGUOUS_VISION");
}

/// 滚动之后必须重新截图识别，不能复用滚动前的结果去点击。
#[test]
fn every_scroll_is_followed_by_a_fresh_recognition() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let script = vec![
        ScriptedCall::boxes(vec![tb("甲", 20, 0.99)]),
        ScriptedCall::boxes(vec![tb(CONTACT, 20, 0.99)]),
        ScriptedCall::Ok(scenario.header.clone()),
        ScriptedCall::Ok(scenario.body_before.clone()),
        ScriptedCall::Ok(scenario.body_after.clone()),
    ];
    let fixture = Fixture::build_with_script(
        MockDesktop::new(),
        MockHumanConfirmation::default(),
        RunnerConfig { retry_backoff: Duration::ZERO, ..list_config() },
        script,
    );

    fixture.run(&fixture.task());

    // 候选区识别 2 次（滚前 + 滚后）+ 标题 1 + 正文前 1 + 正文后 1 = 5
    assert_eq!(fixture.ocr.call_count(), 5, "滚动之后必须重新识别");
}

// ── 卡死检测 ────────────────────────────────────────────────────────────

/// 动作前发现客户端卡死 —— 必须停下，绝不能往一个卡死的窗口里点击/粘贴/回车。
///
/// 往卡死的窗口里输入，结果是"看起来发出去了，其实什么都没发生"，比失败更糟。
#[test]
fn a_frozen_client_stops_the_task_before_any_input() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let desktop = MockDesktop::new();
    desktop.set_responsive(false);
    let fixture = Fixture::build(
        &scenario,
        desktop,
        MockHumanConfirmation::default(),
        RunnerConfig { retry_backoff: Duration::ZERO, ..list_config() },
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    assert_eq!(fixture.desktop.send_count(), 0, "卡死时绝不能发送");
    assert!(fixture.desktop.clicks.lock().unwrap().is_empty(), "卡死时绝不能点击");
    assert!(fixture.desktop.pasted_texts().is_empty(), "卡死时绝不能粘贴");
    let reason = outcome.failure.expect("应有失败原因").reason;
    assert!(reason.contains("卡死"), "应说明客户端卡死：{reason}");
}

/// 点击之后对话区一个像素都没变 —— 报"点击没生效"，而不是"标题不符"。
///
/// 这两种情况的处置完全不同：前者要去查窗口是不是被挡住、客户端是不是卡死；
/// 后者要去查是不是选错了联系人。报错了会把人引到反方向。
#[test]
fn a_click_that_changes_nothing_is_reported_as_ineffective_not_as_a_wrong_title() {
    let scenario = MockScenario::header_mismatch(CONTACT, MESSAGE);
    let desktop = MockDesktop::new();
    desktop.script_clicks_without_effect();
    let fixture = Fixture::build(
        &scenario,
        desktop,
        MockHumanConfirmation::default(),
        RunnerConfig { retry_backoff: Duration::ZERO, ..list_config() },
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    assert_eq!(fixture.desktop.send_count(), 0);
    let reason = outcome.failure.expect("应有失败原因").reason;
    assert!(reason.contains("没有生效"), "应指出这次点击没生效：{reason}");
}

/// 关掉卡死检测后不再拦截。保留这个开关只是为了现场排查误判时能临时绕过。
#[test]
fn the_liveness_check_can_be_turned_off() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let desktop = MockDesktop::new();
    desktop.set_responsive(false);
    let fixture = Fixture::build(
        &scenario,
        desktop,
        MockHumanConfirmation::default(),
        RunnerConfig {
            retry_backoff: Duration::ZERO,
            liveness_check: false,
            ..list_config()
        },
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::Completed, "失败原因：{:?}", outcome.failure);
}

// ── 标定尺寸 ────────────────────────────────────────────────────────────

/// 窗口被拖成了别的尺寸 —— **先自动调回标定尺寸**，而不是停下让人手工拖。
///
/// 尺寸是程序完全能确定的量（标定记录里就写着目标值），调它是确定性的、可逆的，
/// 不该推给操作者做。
#[test]
fn a_window_that_is_not_the_calibrated_size_is_resized_back_before_any_input() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let desktop = MockDesktop::new();
    // 客户端被拖成了 900x600，而标定记录是 1280x720（= `DEFAULT_WINDOW`）。
    desktop.set_window(Rect { x: 0, y: 0, width: 900, height: 600 });
    let fixture = Fixture::build(
        &scenario,
        desktop,
        MockHumanConfirmation::default(),
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

    assert_eq!(
        fixture.desktop.resizes.lock().unwrap().as_slice(),
        &[(DEFAULT_WINDOW.width, DEFAULT_WINDOW.height)],
        "应请求把窗口调回标定尺寸"
    );
    assert_eq!(fixture.desktop.window().width, DEFAULT_WINDOW.width);
    assert_eq!(fixture.desktop.window().height, DEFAULT_WINDOW.height);
    assert_eq!(
        outcome.state,
        TaskState::Completed,
        "调成之后应照常跑完：{:?}",
        outcome.failure
    );
}

/// 尺寸本来就对 —— **不许去动窗口**。
///
/// 这是上一条的反面：自动调整不能变成"每次运行都先把窗口设一遍"，
/// 那会在操作者没要求的时候改变他的桌面。
#[test]
fn a_window_matching_the_calibrated_size_is_accepted_untouched() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let fixture = Fixture::build(
        &scenario,
        MockDesktop::new(),
        MockHumanConfirmation::default(),
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

    assert_eq!(outcome.state, TaskState::Completed, "失败原因：{:?}", outcome.failure);
    assert!(
        fixture.desktop.resizes.lock().unwrap().is_empty(),
        "尺寸本来就对，不该去动窗口"
    );
}

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
        MockHumanConfirmation::default(),
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
        MockHumanConfirmation::default(),
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

/// 客户端由操作者自己启动并登录：任务流程里**不得**出现任何"拉起程序"的动作。
#[test]
fn the_task_never_launches_the_client_itself() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let fixture = Fixture::new(&scenario);

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::Completed, "失败原因：{:?}", outcome.failure);
    assert!(
        !fixture.desktop.operations().iter().any(|op| op == "launch"),
        "任务不该自己启动客户端，实际动作序列：{:?}",
        fixture.desktop.operations()
    );
}

// ── 只填不发 ────────────────────────────────────────────────────────────

/// 打开「只填不发」：正文进输入框，但绝不发送。
#[test]
fn stop_before_send_fills_the_box_and_never_sends() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let fixture = Fixture::build(
        &scenario,
        MockDesktop::new(),
        MockHumanConfirmation::default(),
        RunnerConfig {
            retry_backoff: Duration::ZERO,
            stop_before_send: true,
            ..list_config()
        },
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::Prepared, "失败原因：{:?}", outcome.failure);
    assert!(outcome.stopped_before_send());
    assert!(!outcome.succeeded(), "只填不发不算发送成功");
    // 逐字输入，不是粘贴：搜索框必须逐字敲才会触发联想，而消息正文与它
    // 共用同一条输入路径——共用才不会出现"某条输入路径从没被验证过"。
    assert_eq!(
        fixture.desktop.typed_texts(),
        vec![MESSAGE.to_string()],
        "正文应当已经填进输入框"
    );
    assert!(
        fixture.desktop.pasted_texts().is_empty(),
        "这条路已经不走粘贴了：{:?}",
        fixture.desktop.pasted_texts()
    );
    assert_eq!(fixture.desktop.send_count(), 0, "绝不能按发送");
    assert_eq!(
        fixture.confirmation.call_count(),
        0,
        "既然不会发送，就不该再走人工确认"
    );
}

/// 「只填不发」不能只是「跳过发送键」——它必须压根不进入 Sending 状态，
/// 也不该在审计里留下消息摘要（那会让人误以为发过了）。
#[test]
fn stop_before_send_never_reaches_the_sending_state() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let fixture = Fixture::build(
        &scenario,
        MockDesktop::new(),
        MockHumanConfirmation::default(),
        RunnerConfig {
            retry_backoff: Duration::ZERO,
            stop_before_send: true,
            ..list_config()
        },
    );
    let task = fixture.task();

    fixture.run(&task);

    let states = fixture.progress.states();
    assert!(!states.contains(&TaskState::Sending), "不该经过 Sending，实际路径：{states:?}");
    assert!(
        !states.contains(&TaskState::AwaitingHumanConfirmation),
        "不该要求确认，实际路径：{states:?}"
    );
    assert_eq!(states.last(), Some(&TaskState::Prepared));
    assert!(
        fixture.entries().iter().all(|entry| entry.message.is_none()),
        "没发送就不该写消息摘要"
    );
}

/// 关掉开关时行为必须和以前完全一致：仍然会发送。
#[test]
fn sending_still_happens_when_stop_before_send_is_off() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let fixture = Fixture::build(
        &scenario,
        MockDesktop::new(),
        MockHumanConfirmation::default(),
        RunnerConfig {
            retry_backoff: Duration::ZERO,
            stop_before_send: false,
            ..list_config()
        },
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::Completed);
    assert_eq!(fixture.desktop.send_count(), 1);
}

/// 「只填不发」+ 滚动查找：两个新功能叠在一起也要能走通。
#[test]
fn stop_before_send_works_together_with_scrolling() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let script = vec![
        ScriptedCall::boxes(vec![tb("甲", 20, 0.99)]),
        ScriptedCall::boxes(vec![tb(CONTACT, 20, 0.99)]),
        ScriptedCall::Ok(scenario.header.clone()),
        ScriptedCall::Ok(scenario.body_before.clone()),
    ];
    let fixture = Fixture::build_with_script(
        MockDesktop::new(),
        MockHumanConfirmation::default(),
        RunnerConfig {
            retry_backoff: Duration::ZERO,
            stop_before_send: true,
            ..list_config()
        },
        script,
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::Prepared, "失败原因：{:?}", outcome.failure);
    assert_eq!(fixture.desktop.scroll_count(), 1);
    assert_eq!(fixture.desktop.typed_texts(), vec![MESSAGE.to_string()]);
    assert_eq!(fixture.desktop.send_count(), 0);
}

// ── 切换视图：用模板匹配点左侧导航图标 ──────────────────────────────────
//
// 图标上没有文字，OCR 读不到它，所以"先切到联系人视图"这一步只能靠模板匹配。
// 这三条用例覆盖：命中并跳转、认不出图标、点击没生效。

fn navigation_config() -> RunnerConfig {
    // 列表扫描式本身就会导航；这里不再叠 `navigate_before_search`。
    list_config()
}

fn navigation_fixture(desktop: MockDesktop, icons: Arc<dyn IconLocator>) -> Fixture {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    Fixture::build_full(
        desktop,
        MockHumanConfirmation::default(),
        navigation_config(),
        scenario.script(),
        Arc::new(platform_mock::MockContactMatcher::new()),
        icons,
    )
}

#[test]
fn the_navigation_step_switches_the_view_before_searching() {
    let icons = Arc::new(MockIconLocator::new());
    let fixture = navigation_fixture(MockDesktop::new(), icons.clone());
    let task = fixture.task();

    let outcome = fixture.run(&task);

    assert_eq!(outcome.state, TaskState::Completed, "失败原因：{:?}", outcome.failure);
    assert_eq!(icons.call_count(), 1, "切换视图这一步应当恰好找一次图标");

    // 状态序列里必须出现 NavigatingToView，而且**在**查找之前。
    let states = fixture.progress.states();
    let navigating = states
        .iter()
        .position(|state| *state == TaskState::NavigatingToView)
        .expect("应当经过「切换视图」");
    let searching = states
        .iter()
        .position(|state| *state == TaskState::SearchingContact)
        .expect("应当经过「查找联系人」");
    assert!(navigating < searching, "切换视图必须发生在查找之前：{states:?}");

    // 搜索区必须**只有**导航条那么大，而不是整个窗口——
    // 传整窗的话，"图标在哪"这件事就没有任何约束了。
    let strip = DEFAULT_NAV_STRIP.resolve(DEFAULT_WINDOW);
    assert_eq!(
        icons.regions.lock().unwrap().as_slice(),
        &[(strip.width as u32, strip.height as u32)]
    );

    // 点击必须落在搜索区之内（命中位置 → 换算到屏幕 → 取中心）。
    // 第一次点击是导航图标，第二次才是联系人——顺序本身也是约定。
    let clicks = fixture.desktop.clicks.lock().unwrap().clone();
    let nav_click = clicks.first().expect("应当点过一次导航图标");
    assert!(
        (strip.x..strip.x + strip.width).contains(&nav_click.x)
            && (strip.y..strip.y + strip.height).contains(&nav_click.y),
        "导航点击 ({}, {}) 落在搜索区 {strip:?} 之外",
        nav_click.x,
        nav_click.y
    );
    assert_eq!(fixture.desktop.send_count(), 1);
}

#[test]
fn an_icon_that_cannot_be_found_stops_before_any_search() {
    let fixture = navigation_fixture(MockDesktop::new(), Arc::new(MockIconLocator::never()));
    let task = fixture.task();

    let outcome = fixture.run(&task);

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    assert_eq!(
        outcome.failure.as_ref().map(|failure| failure.code.as_str()),
        Some("AMBIGUOUS_VISION"),
        "认不出图标属于「识别不确定」，不是平台故障"
    );
    // 认不出图标就**不该**去点任何东西，也不该开始找联系人——
    // 否则后面每一步都建立在一个"没切换成功"的界面上。
    assert!(
        fixture.desktop.clicks.lock().unwrap().is_empty(),
        "认不出图标时不该点任何地方"
    );
    assert_eq!(fixture.desktop.send_count(), 0);
    assert!(
        !fixture.progress.states().contains(&TaskState::SearchingContact),
        "这一步失败就不该再往下走"
    );
}

/// 「点下去画面没变」**不**在这一步失败，但必须在证据里留痕。
///
/// ## 为什么不是转人工
///
/// 这个现象有两种成因，而在画面上**分不出来**：
///
/// 1. 界面本来就已经停在这个视图上（上一次运行点完就留在这里了）
///    ⇒ 点击无效是**正确**行为；
/// 2. 客户端卡死 / 图标被挡住 / 匹配到了不响应点击的位置
///    ⇒ 点击真的没生效。
///
/// 在这一步直接转人工，第 1 种就会变成"第二次跑必然失败"——
/// 报错文案还会把人引向排查客户端，方向完全错了。
/// 所以判定交给下一步：`locate_contact` 是只读的，视图不对就在候选区里找不到目标。
#[test]
fn a_navigation_click_that_changes_nothing_is_recorded_and_left_to_the_next_step() {
    let desktop = MockDesktop::new();
    desktop.script_clicks_without_effect();
    let fixture = navigation_fixture(desktop, Arc::new(MockIconLocator::new()));
    let task = fixture.task();

    let outcome = fixture.run(&task);

    // 本例的候选区（`MockScenario::happy`）里**有**目标联系人，
    // 对应"其实已经停在这个视图上"那种情况 ⇒ 应当正常跑完。
    assert_eq!(outcome.state, TaskState::Completed, "失败原因：{:?}", outcome.failure);
    assert_eq!(fixture.desktop.send_count(), 1);

    // 留痕是这一条用例的重点：真出问题时，日志要能直接回答
    // "是不是视图根本没切过去"，而不是从"找不到联系人"倒推。
    let warned = outcome.evidence.iter().any(|line| line.contains("未变化"));
    assert!(
        warned,
        "画面没变必须在证据里留痕，实际证据：{:#?}",
        outcome.evidence
    );
}

/// 视图没切过去时，由**下一步**兜住：候选区里找不到目标 ⇒ 转人工。
///
/// 这条与上一条合起来才是完整的故事：本步骤不武断失败，但也没有放弃判定——
/// 只是把判定挪到了能真正决断的地方。
#[test]
fn a_view_that_never_switched_is_caught_when_the_contact_is_not_found() {
    let desktop = MockDesktop::new();
    desktop.script_clicks_without_effect();
    // 候选区里没有目标联系人（相当于界面停在了别的视图上）。
    let scenario = MockScenario::login_prompt(CONTACT, MESSAGE);
    let fixture = Fixture::build_full(
        desktop,
        MockHumanConfirmation::default(),
        navigation_config(),
        scenario.script(),
        Arc::new(platform_mock::MockContactMatcher::new()),
        Arc::new(MockIconLocator::new()),
    );
    let task = fixture.task();

    let outcome = fixture.run(&task);

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    assert_eq!(fixture.desktop.send_count(), 0);
    // 证据里应当同时有"画面没变"的警告——它是这次失败的第一现场。
    assert!(
        outcome.evidence.iter().any(|line| line.contains("未变化")),
        "证据里应当留有「视图可能没切过去」的线索：{:#?}",
        outcome.evidence
    );
}

#[test]
fn the_list_workflow_always_navigates_to_chat_history() {
    let icons = Arc::new(MockIconLocator::new());
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let fixture = Fixture::build_full(
        MockDesktop::new(),
        MockHumanConfirmation::default(),
        list_config(),
        scenario.script(),
        Arc::new(platform_mock::MockContactMatcher::new()),
        icons.clone(),
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::Completed, "失败原因：{:?}", outcome.failure);
    // 列表扫描式总是先切到聊天历史——不受 navigate_before_search 控制。
    assert_eq!(icons.call_count(), 1);
    assert!(fixture.progress.states().contains(&TaskState::NavigatingToView));
}

#[test]
fn search_workflow_always_navigates_to_contacts_first() {
    let icons = Arc::new(MockIconLocator::new());
    let mut cfg = search_config();
    cfg.nav_icon_templates = vec![nav_template()];
    cfg.navigate_before_search = false; // 开关关着也不影响：搜索式一律先切联系人
    let desktop = MockDesktop::new();
    desktop.script_scroll_bottom(6);
    let fixture = Fixture::build_full(
        desktop,
        MockHumanConfirmation::default(),
        cfg,
        search_script(CONTACT),
        Arc::new(platform_mock::MockContactMatcher::new()),
        icons.clone(),
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::Prepared, "失败原因：{:?}", outcome.failure);
    assert_eq!(icons.call_count(), 1);
    assert!(fixture.progress.states().contains(&TaskState::NavigatingToView));
}

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
//   搜索下拉 → 资料页 → 资料页入口 → 聊天标题 → 发送前聊天正文

/// 搜索式要用的 OCR 脚本。
///
/// 段数比预置场景多一段（资料页与它的入口要分别识别一次），
/// 所以不能复用 `MockScenario::script()`，得自己排。
fn search_script(contact: &str) -> Vec<ScriptedCall> {
    vec![
        // 1. 联想下拉：先一行「联系人」分组标题，标题**下面**才是人。
        ScriptedCall::Ok(vec![tb("联系人", 10, 0.99), tb(contact, 40, 0.99)]),
        // 2. 资料页上这个人自己的名字——用来核对"点的是不是他"。
        ScriptedCall::Ok(vec![tb(contact, 20, 0.99)]),
        // 3. 资料页滚到底之后找「发消息」入口。
        ScriptedCall::Ok(vec![tb("发消息", 300, 0.99)]),
        // 4. 聊天页标题。
        ScriptedCall::Ok(vec![tb(contact, 16, 0.99)]),
        // 5. 发送前的聊天正文（聚焦输入框之后截的那一帧）。
        ScriptedCall::Ok(vec![tb("上一条历史消息", 40, 0.99)]),
    ]
}

/// 搜索式的基线配置：三块区域都标好了；导航模板从 `list_config` 带上（搜索式一律先切联系人）。
fn search_config() -> RunnerConfig {
    let region = |x: f32, y: f32, w: f32, h: f32| {
        automation_core::RelativeRegion::new(x, y, w, h)
    };
    RunnerConfig {
        platform_label: "test".into(),
        retry_backoff: Duration::ZERO,
        workflow: Workflow::SearchContact,
        main_search: Some(region(0.10, 0.02, 0.60, 0.05)),
        search_dropdown: Some(region(0.10, 0.07, 0.60, 0.50)),
        contact_profile: Some(region(0.72, 0.05, 0.27, 0.90)),
        ..list_config()
    }
}

fn search_fixture(script: Vec<ScriptedCall>) -> Fixture {
    let desktop = MockDesktop::new();
    // 资料页只滚几格就到底。
    //
    // 替身默认的"底"是 1000 格，而编排层最多滚 20 次、每次 3 格 —— 也就是说
    // 按默认值它永远滚不到底，会以「向下滚动 21 次仍未让资料页画面稳定下来」
    // 转人工。那是**替身的设置问题**，不是流程的问题：真实资料页几十格就到头了。
    desktop.script_scroll_bottom(6);
    Fixture::build_with_script(
        desktop,
        MockHumanConfirmation::default(),
        search_config(),
        script,
    )
}

/// 搜索式主路径：一路走到 `Prepared`，而且**绝不发送**。
///
/// 这是操作者明确要求的那条验收线：
/// 「发送动作先不要点，mock 上。全部测试通过了，我再加这个发送逻辑。」
/// 所以这里不仅断言终态，还要断言**没有**任何发送痕迹——
/// 发送次数为 0、没有申请过人工确认、审计里没有 Sending。
#[test]
fn the_search_workflow_reaches_prepared_and_stops_before_sending() {
    let fixture = search_fixture(search_script(CONTACT));
    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::Prepared, "失败原因：{:?}", outcome.failure);
    assert!(outcome.stopped_before_send(), "Prepared 就是这条路的正常终态");
    assert!(!outcome.succeeded(), "Prepared 是「填好了但没发」，不是完成");

    // ── 没有发送痕迹 ────────────────────────────────────────────
    assert_eq!(fixture.desktop.send_count(), 0, "搜索式工作流不该发送");
    assert_eq!(
        fixture.confirmation.call_count(),
        0,
        "没打算发送，就不该去打扰操作者做人工确认"
    );
    assert!(
        !fixture.progress.states().contains(&TaskState::Sending),
        "状态轨迹里不该出现 Sending：{:?}",
        fixture.progress.states()
    );
    assert!(
        fixture.entries().iter().all(|entry| entry.to != TaskState::Sending),
        "审计里也不该出现 Sending"
    );

    // ── 两次输入都走的是**逐字输入**，不是粘贴 ──────────────────
    //
    // 搜索框必须逐字敲才会触发联想；用粘贴的话下拉根本不弹，
    // 而现象看起来像"搜不到人"。
    assert_eq!(
        fixture.desktop.typed_texts(),
        vec![CONTACT.to_string(), MESSAGE.to_string()],
        "先逐字输入搜索词，再逐字输入正文"
    );
    assert!(
        fixture.desktop.pasted_texts().is_empty(),
        "搜索式全程不该用粘贴：{:?}",
        fixture.desktop.pasted_texts()
    );

    // ── 状态轨迹必须经过资料页那两步 ────────────────────────────
    let states = fixture.progress.states();
    for expected in [
        TaskState::SearchingContact,
        TaskState::VerifyingCandidate,
        TaskState::VerifyingProfile,
        TaskState::OpeningChatFromProfile,
        TaskState::VerifyingChatHeader,
        TaskState::Prepared,
    ] {
        assert!(states.contains(&expected), "状态轨迹里缺 {expected:?}：{states:?}");
    }
}

/// 点导航 → 点搜索框 → 点下拉里那一行 → 点资料页入口 → 点输入框：**五次**点击。
///
/// 为什么要数点击：少一次就少一个动作，而少的那一次**不一定报错**——
/// 比如漏了点输入框，正文会敲进当时有焦点的控件里（最坏是搜索框，
/// 把搜索结果本身改掉），现象只是"字没进去"。数一遍能把这类漏步钉住。
#[test]
fn the_search_workflow_clicks_every_control_it_needs() {
    let fixture = search_fixture(search_script(CONTACT));
    let outcome = fixture.run(&fixture.task());
    assert_eq!(outcome.state, TaskState::Prepared, "失败原因：{:?}", outcome.failure);

    let clicks = fixture.desktop.clicks.lock().unwrap().clone();
    assert_eq!(
        clicks.len(),
        5,
        "点击序列：联系人导航 / 搜索框 / 下拉行 / 资料页入口 / 输入框"
    );

    let window = fixture.desktop.window();
    let inside = |point: automation_core::Point, region: automation_core::RelativeRegion| {
        let rect = region.resolve_within(window).expect("区域应当能换算");
        point.x >= rect.x
            && point.x <= rect.x + rect.width
            && point.y >= rect.y
            && point.y <= rect.y + rect.height
    };
    let config = search_config();
    // 第一次是导航图标（落在 nav_strip / 窗口左侧一带），不强制区域断言。
    assert!(inside(clicks[1], config.main_search.unwrap()), "第二次点击应当在搜索框里");
    assert!(
        inside(clicks[2], config.search_dropdown.unwrap()),
        "第三次点击应当在联想下拉里（点的是那一行文字）"
    );
    assert!(
        inside(clicks[3], config.contact_profile.unwrap()),
        "第四次点击应当在资料页里（点的是「发消息」入口）"
    );
    assert!(
        inside(clicks[4], config.composer),
        "第五次点击应当落在消息输入框里"
    );
}

/// 下拉里的行按**分组标题下方**取，标题**上方**的行不算。
///
/// 这一条防的是"聊天记录冒充联系人"：下拉里的聊天记录行常常长成
/// 「和 张三 的聊天」，它也含目标名。不按分组切的话，会点进一条聊天记录。
#[test]
fn a_row_above_the_contact_group_is_not_a_candidate() {
    let mut script = search_script(CONTACT);
    // 把第一段换成：聊天记录行在上、「联系人」分组标题在下、目标行在标题下方。
    script[0] = ScriptedCall::Ok(vec![
        tb("聊天记录", 5, 0.99),
        tb(&format!("和{CONTACT}的聊天"), 30, 0.99),
        tb("联系人", 200, 0.99),
        tb(CONTACT, 240, 0.99),
    ]);
    let fixture = search_fixture(script);

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::Prepared, "失败原因：{:?}", outcome.failure);
    let clicks = fixture.desktop.clicks.lock().unwrap().clone();
    // 第二次点击的 y 必须在标题**下方**（标题底边 = 200 + 28）。
    let dropdown = search_config().search_dropdown.unwrap();
    let rect = dropdown.resolve_within(fixture.desktop.window()).unwrap();
    // clicks[0]=导航, [1]=搜索框, [2]=下拉行
    let clicked_y = clicks[2].y - rect.y;
    assert!(
        clicked_y >= 228,
        "点到「联系人」标题上方去了（相对下拉区 y={clicked_y}）：那是聊天记录行"
    );
}

/// 名字一样长的两行并列时，**照样取最上面那一行**——"同名就转人工"那道闸门
/// 已按操作者 2026-09-21 的要求撤掉：客户端把最匹配的排在最前面，认它。
#[test]
fn a_tie_between_equally_long_rows_follows_the_topmost_one_instead_of_stopping() {
    let mut script = search_script(CONTACT);
    script[0] = ScriptedCall::Ok(vec![
        tb("联系人", 10, 0.99),
        tb(&format!("小{CONTACT}"), 40, 0.99),
        tb(&format!("大{CONTACT}"), 80, 0.99),
    ]);
    let fixture = search_fixture_relaxed(script);

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::Prepared, "失败原因：{:?}", outcome.failure);
    let clicks = fixture.desktop.clicks.lock().unwrap().clone();
    let dropdown = search_config().search_dropdown.unwrap();
    let rect = dropdown.resolve_within(fixture.desktop.window()).unwrap();
    let clicked_y = clicks[2].y - rect.y;
    assert!(
        (40..70).contains(&clicked_y),
        "应当点最上面的那一行（相对 y={clicked_y}），不是下面那行"
    );
    assert_eq!(fixture.desktop.send_count(), 0);
}

/// 同 `search_fixture`，但装配**放宽层**（`ContainsNameMatcher`）。
///
/// 生产跑的就是这一层（`runtime.rs` 里 `relaxed_name_match` 默认 true），
/// 而替身默认装配的是严格匹配器。严格层下 `Jerry-张三同学` 这种行会被
/// `verify_candidate` 判成"不被当前的姓名匹配策略接受"，流程在**挑完人之后**
/// 就停了——那样就测不出"下拉里究竟挑了哪一行"这件事，
/// 而这正是下面两条用例要钉的。
fn search_fixture_relaxed(script: Vec<ScriptedCall>) -> Fixture {
    let desktop = MockDesktop::new();
    desktop.script_scroll_bottom(6);
    Fixture::build_with_matcher(
        desktop,
        MockHumanConfirmation::default(),
        search_config(),
        script,
        Arc::new(automation_core::ContainsNameMatcher::default()),
    )
}

/// 下拉里根本没有「联系人」这一组 ⇒ 转人工，并说清读到的是什么。
#[test]
fn a_dropdown_without_the_contact_group_is_a_human_review() {
    let mut script = search_script(CONTACT);
    script[0] = ScriptedCall::Ok(vec![
        tb("聊天记录", 10, 0.99),
        tb(&format!("和{CONTACT}的聊天"), 40, 0.99),
    ]);
    let fixture = search_fixture(script);

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    let reason = outcome.failure.as_ref().map(|f| f.reason.clone()).unwrap_or_default();
    assert!(reason.contains("联系人"), "要说清缺的是哪一组：{reason}");
}

/// 人只出现在「最常使用」底下时也能找到——Mac 微信上常见。
#[test]
fn a_contact_only_under_frequently_used_is_picked() {
    let mut script = search_script(CONTACT);
    script[0] = ScriptedCall::Ok(vec![
        tb("联系人", 10, 0.99),
        tb("最常使用", 80, 0.99),
        tb(CONTACT, 110, 0.99),
        tb("聊天记录", 200, 0.99),
        tb(&format!("和{CONTACT}的聊天"), 230, 0.99),
    ]);
    let fixture = search_fixture(script);

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::Prepared, "失败原因：{:?}", outcome.failure);
    let clicks = fixture.desktop.clicks.lock().unwrap().clone();
    let dropdown = search_config().search_dropdown.unwrap();
    let rect = dropdown.resolve_within(fixture.desktop.window()).unwrap();
    let clicked_y = clicks[2].y - rect.y;
    assert!(
        (110..200).contains(&clicked_y),
        "应当点「最常使用」下的那一行（相对 y={clicked_y}），不是聊天记录行"
    );
}

/// 下拉里有多行含关键词时，取**最上面**的那一行——哪怕本人那一行在它下面。
///
/// 复现自 2026-09-21 Mac 微信实测（`/Users/admin/data/task-a537432a.log`）：
/// 搜「李小明」时「联系人」分组下方同时有 `李小明` 与 `Jerry-李小明同学`，
/// 原来的"多个就转人工"让整条流程停在搜索这一步（鼠标一下都没动）。
///
/// 判据是操作者定的「**排最上面的优先**」（不是"取最短"）：客户端自己会把
/// 最匹配的那一行放在最前面。所以这条用例刻意把更长的 `Jerry-…同学` 放在
/// 本人上方——取最短会点下面那行，取最上面才会点上面那行，
/// 两种实现只有在这里能被区分开。
#[test]
fn the_topmost_matching_row_wins_when_several_rows_contain_the_name() {
    let mut script = search_script(CONTACT);
    script[0] = ScriptedCall::Ok(vec![
        tb("联系人", 10, 0.99),
        tb(&format!("Jerry-{CONTACT}同学"), 40, 0.99),
        tb(CONTACT, 70, 0.99),
        tb("群聊", 130, 0.99),
        tb(&format!("和{CONTACT}的聊天"), 160, 0.99),
    ]);
    let fixture = search_fixture_relaxed(script);

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::Prepared, "失败原因：{:?}", outcome.failure);
    let clicks = fixture.desktop.clicks.lock().unwrap().clone();
    let dropdown = search_config().search_dropdown.unwrap();
    let rect = dropdown.resolve_within(fixture.desktop.window()).unwrap();
    let clicked_y = clicks[2].y - rect.y;
    assert!(
        (40..70).contains(&clicked_y),
        "应当点最上面的那一行（相对 y={clicked_y}），而不是下面那行"
    );
}

/// 「聊天记录」下的同名行不能冒充联系人——即使上面的联系人分组是空的。
#[test]
fn a_chat_history_row_is_not_picked_when_contact_sections_exist() {
    let mut script = search_script(CONTACT);
    script[0] = ScriptedCall::Ok(vec![
        tb("联系人", 10, 0.99),
        tb("最常使用", 50, 0.99),
        tb("聊天记录", 100, 0.99),
        tb(&format!("和{CONTACT}的聊天"), 130, 0.99),
    ]);
    let fixture = search_fixture(script);

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    let reason = outcome.failure.as_ref().map(|f| f.reason.clone()).unwrap_or_default();
    assert!(
        reason.contains("联系人") || reason.contains("最常使用"),
        "失败信息要点名试过的分组：{reason}"
    );
    assert_eq!(
        fixture.desktop.clicks.lock().unwrap().len(),
        2,
        "导航 + 搜索框之后就该停下（还没点下拉行）"
    );
}

/// 点完搜索框之后**先清空、再输入**。
///
/// 客户端的搜索框保留上一次的输入：不清空的话，这一次的关键词会接在上一次的
/// 后面（「张三」→「张三李四」），而它是联想式的——会拿这个混合词去查，
/// 结果是一片与目标无关的内容。现象是"搜出来的东西不对"，
/// 不会让人想到是**上一次的词还在**。
///
/// 这条用例断言的是**顺序**而不是"清空调用过没有"：清空晚于输入就等于没清。
#[test]
fn the_search_workflow_clears_the_box_before_typing() {
    let fixture = search_fixture(search_script(CONTACT));
    let outcome = fixture.run(&fixture.task());
    assert_eq!(outcome.state, TaskState::Prepared, "失败原因：{:?}", outcome.failure);

    let operations = fixture.desktop.operations();
    // 只看**输入动作**：编排层每次动作前都会重新确认前台窗口（序列里的 `focus`），
    // 截图也夹在中间（`capture`）。把它们算进来，断言就变成了"数序列位置"，
    // 而那些都不是这一步要守的东西。
    let inputs: Vec<&str> = operations
        .iter()
        .map(String::as_str)
        .filter(|op| matches!(*op, "click" | "clear" | "type" | "paste"))
        .collect();
    // 开头多一次导航点击；之后必须是 点搜索框 → 清空 → 输入。
    let head: Vec<&str> = inputs.iter().take(4).copied().collect();
    assert_eq!(
        head,
        ["click", "click", "clear", "type"],
        "导航之后：点搜索框 → 清空 → 输入。完整操作序列：{operations:?}"
    );
    // 第一次逐字输入必须是关键词（后面还有一次是消息正文——搜索式也走逐字输入）。
    assert_eq!(
        fixture.desktop.typed_texts().first().map(String::as_str),
        Some(CONTACT),
        "搜索框里逐字敲的应当是关键词本身，而不是「旧词 + 关键词」"
    );
}

/// 下拉里找不到人时，失败信息要**把搜索框里的内容一并报出来**。
///
/// 这一条守的是「输入根本没落进搜索框」那种情况：那时下拉里是一片与关键词
/// 无关的内容（甚至是刚点开搜索框时的默认列表），而过程证据里照样写着
/// "已在搜索框逐字输入 N 个字符"——看起来像"确实没有这个人"，
/// 于是排查方向会一路偏到关键词和客户端上。实测踩过一次：三次运行里有一次
/// 输入没落进去，日志里看不出任何异常。
///
/// 把搜索框里的原文报出来，两种情况一眼就能分开。
#[test]
fn a_failed_search_reports_what_is_actually_in_the_search_box() {
    let fixture = search_fixture(vec![
        // 1. 联想下拉：有「联系人」标题，但标题下面没有目标。
        ScriptedCall::Ok(vec![tb("联系人", 10, 0.99), tb("另一个联系人", 40, 0.99)]),
        // 2. 失败之后补的那一次核对：搜索框里读到的是上一次留下的词。
        ScriptedCall::Ok(vec![tb("上一次的词", 5, 0.99)]),
    ]);

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    let reason = outcome.failure.as_ref().map(|f| f.reason.clone()).unwrap_or_default();
    assert!(reason.contains("核对"), "要说清核对过搜索框：{reason}");
    assert!(
        reason.contains("上一次的词"),
        "要把搜索框里读到的东西原样报出来：{reason}"
    );
}

/// 资料页里找不到「发消息」入口 ⇒ 转人工，并指向那两个可能的原因。
#[test]
fn a_profile_without_the_chat_entry_is_a_human_review() {
    let mut script = search_script(CONTACT);
    // 资料页滚到底之后读到的是别的东西，没有「发消息」。
    script[2] = ScriptedCall::Ok(vec![tb("朋友圈", 300, 0.99)]);
    let fixture = search_fixture(script);

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    let reason = outcome.failure.as_ref().map(|f| f.reason.clone()).unwrap_or_default();
    assert!(reason.contains("发消息"), "要说清找的是哪几个字：{reason}");
    assert!(reason.contains("界面标定"), "要指出可能是区域标偏了：{reason}");
    assert_eq!(fixture.desktop.send_count(), 0);
}

/// 资料页上读到的是别人 ⇒ 转人工（"可能点错了人"）。
///
/// 这一步存在的全部意义就是**重名**：下拉里那一行只是"文字包含关键词"，
/// 资料页上才是这个人自己的名字，两处对上了才说明点对了人。
#[test]
fn a_profile_showing_someone_else_is_refused() {
    let mut script = search_script(CONTACT);
    script[1] = ScriptedCall::Ok(vec![tb("另一个联系人", 20, 0.99)]);
    let fixture = search_fixture(script);

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    let reason = outcome.failure.as_ref().map(|f| f.reason.clone()).unwrap_or_default();
    assert!(reason.contains(CONTACT), "要把目标名报出来：{reason}");
    // 判据归匹配器，**不在这里另写一份**（那正是 `verify_profile` 文档里
    // 反复强调的那件事）。"到底读到了什么"走证据这条通道——
    // 它在界面上也能看到，而且不掺进判据里。
    let evidence: Vec<String> = fixture
        .entries()
        .iter()
        .flat_map(|entry| entry.evidence.clone())
        .collect();
    assert!(
        evidence.iter().any(|line| line.contains("另一个联系人")),
        "证据里要能看到资料页实际读到的文字：{evidence:?}"
    );
    assert_eq!(fixture.desktop.send_count(), 0);
}

/// 搜索式少了必填区域 ⇒ 在**第一次点击之前**就转人工，而不是点空。
///
/// 没标的区域在编排层是 `None`，`resolve_extra` 会如实报错并说清去哪儿标。
/// 这里刻意**不**在测试里补一个猜出来的区域：那正是要防的事。
#[test]
fn a_search_workflow_without_its_regions_stops_before_clicking() {
    let config = RunnerConfig { search_dropdown: None, ..search_config() };
    let fixture = Fixture::build_with_script(
        MockDesktop::new(),
        MockHumanConfirmation::default(),
        config,
        search_script(CONTACT),
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    assert_eq!(
        fixture.desktop.clicks.lock().unwrap().len(),
        2,
        "导航与搜索框还是点得到的，缺的是下拉区——点完搜索框就该停"
    );
    let reason = outcome.failure.as_ref().map(|f| f.reason.clone()).unwrap_or_default();
    assert!(reason.contains("搜索下拉列表"), "要说清缺的是哪一块：{reason}");
    assert!(reason.contains("界面标定"), "要告诉人下一步去哪标：{reason}");
}

/// 点下拉那一行之后**画面一个像素都没变** ⇒ 报"点击可能没生效"，而不是
/// 报"资料页上没有这个人"。
///
/// 这两种原因处置完全不同：前者要去查客户端是不是卡死/被挡住，后者要去查
/// 是不是点错了人。把前者说成后者，会让人一路往"名字识别错了"上找。
#[test]
fn a_click_that_changes_nothing_is_reported_as_such() {
    let desktop = MockDesktop::new();
    desktop.script_scroll_bottom(6);
    // 所有点击都不改变画面。
    desktop.script_clicks_without_effect();
    let mut script = search_script(CONTACT);
    // 资料页上读到的不是目标本人——否则 `verify_profile` 会先判定通过，
    // 根本走不到"点击没生效"那条分支。
    script[1] = ScriptedCall::Ok(vec![tb("另一个联系人", 20, 0.99)]);
    let fixture = Fixture::build_with_script(
        desktop,
        MockHumanConfirmation::default(),
        search_config(),
        script,
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    let reason = outcome.failure.as_ref().map(|f| f.reason.clone()).unwrap_or_default();
    assert!(reason.contains("没有生效"), "要说清是点击没生效：{reason}");
    assert!(
        !reason.contains("逐字匹配"),
        "不该把它说成「资料页上没找到人」——那是另一个方向：{reason}"
    );
    assert_eq!(fixture.desktop.send_count(), 0);
}

// ── 「只做导航」（工作流 1 / 2 的最小验证单元）──────────────────────────
/// 只做导航：找到图标、点它、停在 `Navigated`，**不找任何人**。
///
/// 这条路的价值是把"图标匹配得准不准"从整条链路里单独拎出来验证——
/// 混在完整流程里时，点错图标的症状会表现为"找不到联系人"，
/// 排查方向会一路偏向 OCR。
#[test]
fn navigate_only_finds_the_icon_and_stops_at_navigated() {
    let icons = Arc::new(MockIconLocator::new());
    let config = RunnerConfig {
        workflow: Workflow::NavigateOnly,
        nav_target_label: "通讯录".to_string(),
        nav_icon_templates: vec![nav_template()],
        ..list_config()
    };
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let fixture = Fixture::build_full(
        MockDesktop::new(),
        MockHumanConfirmation::default(),
        config,
        scenario.script(),
        Arc::new(platform_mock::MockContactMatcher::new()),
        icons.clone(),
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::Navigated, "失败原因：{:?}", outcome.failure);
    // `succeeded()` 只认 `Completed`（真的发出去了）。`Navigated` 是这条路的
    // 正常终态，但它**没有完成一次发送**——两者不能混。
    assert!(!outcome.succeeded(), "只做导航没有发送任何消息");
    assert_eq!(icons.call_count(), 1, "只找一次图标");

    // 关键：**一次 OCR 都没有**。它不找人，所以不该去识别任何文字。
    assert_eq!(fixture.ocr.call_count(), 0, "只做导航不该做任何文字识别");
    assert_eq!(fixture.desktop.clicks.lock().unwrap().len(), 1, "只点图标那一下");

    let states = fixture.progress.states();
    assert!(states.contains(&TaskState::NavigatingToView), "{states:?}");
    assert!(
        !states.contains(&TaskState::SearchingContact),
        "只做导航不该进入查找：{states:?}"
    );
}

/// 「只做导航」在**真实模式**下同样要走标定尺寸这一关——那是与工作流无关的前置条件。
///
/// 这条用例守的是"把 `NavigateOnly` 做成免检捷径"这种改法：尺寸对不上时区域会整体
/// 偏移，点下去就是点到别的地方。尺寸不符时**同样会自动调回标定尺寸**，
/// 这条前置条件不会因为工作流更简单就被跳过。
#[test]
fn navigate_only_still_requires_a_calibrated_window_in_live_mode() {
    let config = RunnerConfig {
        workflow: Workflow::NavigateOnly,
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
        MockHumanConfirmation::default(),
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
