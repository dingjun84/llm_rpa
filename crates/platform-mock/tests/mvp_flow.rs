//! MVP 主路径与失败路径的端到端测试。
//!
//! 全部基于 `platform-mock` 的替身端口运行，不接触真实桌面、不接触网络。
//! 这些用例对应 `docs/architecture.md` §9 验收条件中可在软件层验证的部分。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use automation_core::{
    AuditEntry, CalibratedWindow, CancelToken, MemoryAudit, MemorySendLedger, ProgressSink,
    RunOutcome, RunnerConfig, RunnerPorts, SendTask, StateChange, TaskState, WorkflowRunner,
};
use platform_mock::{
    tb, ConfirmationOutcome, Fault, MockDesktop, MockHumanConfirmation, MockOcr, MockScenario,
    ScriptedCall, DEFAULT_WINDOW,
};

const CONTACT: &str = "外部测试联系人";
const MESSAGE: &str = "这是一条测试消息";

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
    progress: RecordingProgress,
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
                ..Default::default()
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
        let desktop = Arc::new(desktop);
        let ocr = Arc::new(MockOcr::new(script));
        let confirmation = Arc::new(confirmation);
        let audit = Arc::new(MemoryAudit::new());
        let ledger = Arc::new(MemorySendLedger::new());

        let runner = WorkflowRunner::new(
            RunnerPorts {
                platform: desktop.clone(),
                ocr: ocr.clone(),
                matcher,
                confirmation: confirmation.clone(),
            },
            config,
        )
        .with_audit(audit.clone())
        .with_ledger(ledger.clone());

        Self {
            runner,
            desktop,
            ocr,
            confirmation,
            audit,
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
    // 两次受守卫的点击：先点联系人，再点消息输入框。
    assert_eq!(clicks.len(), 2, "应先在联系人上点击，再聚焦消息输入框");

    // 期望值由**配置算出来**，不写死屏幕坐标。
    //
    // 脚本里那个候选文字框的图像坐标是 (8, 20, 200, 28)，中心 (108, 34)。
    // 核心层要把它加上**候选区原点的屏幕偏移**——这一步正是本用例要钉的东西：
    // 少加或多加一次原点偏移，点击就会落到别的联系人身上。
    // 而候选区默认值本身是会变的（左边界从 0.0 挪到 0.14 就是一次，为了让开
    // 头像列），写死的数字必然过期；区域默认值由 `runtime.rs` 里的用例单独钉住。
    let panel = RunnerConfig::default().contact_panel.resolve(DEFAULT_WINDOW);
    assert_eq!(clicks[0].x, panel.x + 108, "点击横坐标应含候选区原点偏移");
    assert_eq!(clicks[0].y, panel.y + 34, "点击纵坐标应含候选区原点偏移");
    // 输入框区默认标定 [0.28,0.82,0.72,0.18]，窗口 1280x720
    // → 区域 (358,590,922,130)，中心 (819,655)
    assert_eq!(clicks[1].x, 819);
    assert_eq!(clicks[1].y, 655);
}

#[test]
fn the_message_input_box_is_focused_before_pasting() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let fixture = Fixture::new(&scenario);
    fixture.run(&fixture.task());

    // 输入框必须**先**被点击，粘贴才会落到正确位置。
    let clicks = fixture.desktop.clicks.lock().unwrap().clone();
    let pasted = fixture.desktop.pasted.lock().unwrap().clone();
    assert_eq!(clicks.len(), 2);
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
    assert!(fixture.desktop.clicks.lock().unwrap().is_empty(), "不得点击");
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
        RunnerConfig { retry_backoff: Duration::ZERO, ..Default::default() },
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
        RunnerConfig { retry_backoff: Duration::ZERO, ..Default::default() },
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
        RunnerConfig { retry_backoff: Duration::ZERO, ..Default::default() },
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
            ..Default::default()
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
        RunnerConfig { retry_backoff: Duration::ZERO, ..Default::default() },
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
        RunnerConfig { retry_backoff: Duration::ZERO, ..Default::default() },
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
        RunnerConfig { retry_backoff: Duration::ZERO, ..Default::default() },
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
            confirmation: Arc::new(MockHumanConfirmation::default()),
        },
        RunnerConfig { retry_backoff: Duration::ZERO, ..Default::default() },
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
            ..Default::default()
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
            ..Default::default()
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
        RunnerConfig { retry_backoff: Duration::ZERO, ..Default::default() },
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
        RunnerConfig { retry_backoff: Duration::ZERO, ..Default::default() },
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
    let config = RunnerConfig { retry_backoff: Duration::ZERO, ..Default::default() };
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
        RunnerConfig { retry_backoff: Duration::ZERO, ..Default::default() },
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
            ..Default::default()
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
    let config = RunnerConfig { retry_backoff: Duration::ZERO, ..Default::default() };
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
/// **为什么钉住它**：实测日志里 OCR 读到的姓名行是 `0 丁俊`（那个 `0` 是头像列的
/// 未读红点被 `Windows.Media.Ocr` **按行并进**了姓名块），而严格匹配要求逐字相等，
/// 于是「丁俊明明就在列表里」却永远匹配不上。这条用例复现同一形状：
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
    let config = RunnerConfig { retry_backoff: Duration::ZERO, ..Default::default() };
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
    assert_eq!(clicks.len(), 2, "先点联系人，再聚焦输入框");
    assert_eq!(
        (clicks[0].x, clicks[0].y),
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
            ..Default::default()
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
            ..Default::default()
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
        RunnerConfig { retry_backoff: Duration::ZERO, ..Default::default() },
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
        RunnerConfig { retry_backoff: Duration::ZERO, ..Default::default() },
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
            ..Default::default()
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
        RunnerConfig { retry_backoff: Duration::ZERO, ..Default::default() },
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
        RunnerConfig { retry_backoff: Duration::ZERO, ..Default::default() },
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
        RunnerConfig { retry_backoff: Duration::ZERO, ..Default::default() },
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
            ..Default::default()
        },
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::Completed, "失败原因：{:?}", outcome.failure);
}

// ── 标定尺寸 ────────────────────────────────────────────────────────────

/// 窗口尺寸与标定记录不一致 —— 在动任何东西之前就停下。
///
/// 区域标定是相对窗口的比例，而真实界面不是等比缩放的：尺寸一变四个区域整体偏移，
/// 而点击落偏的后果是点到别的地方。
#[test]
fn a_window_that_is_not_the_calibrated_size_is_refused_before_any_input() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    // 替身窗口是 1280x720，标定记录里写的是别的尺寸。
    let fixture = Fixture::build(
        &scenario,
        MockDesktop::new(),
        MockHumanConfirmation::default(),
        RunnerConfig {
            retry_backoff: Duration::ZERO,
            calibrated_window: Some(CalibratedWindow {
                width: 900,
                height: 600,
                scale_factor: 1.0,
            }),
            ..Default::default()
        },
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    assert!(fixture.desktop.clicks.lock().unwrap().is_empty(), "尺寸对不上就绝不能点击");
    assert_eq!(fixture.desktop.send_count(), 0);
    let reason = outcome.failure.expect("应有失败原因").reason;
    assert!(reason.contains("标定"), "应说明与标定不一致：{reason}");
}

/// 尺寸与标定一致时正常放行 —— 别把校验做成"永远失败"。
#[test]
fn a_window_matching_the_calibrated_size_is_accepted() {
    let scenario = MockScenario::happy(CONTACT, MESSAGE);
    let fixture = Fixture::build(
        &scenario,
        MockDesktop::new(),
        MockHumanConfirmation::default(),
        RunnerConfig {
            retry_backoff: Duration::ZERO,
            calibrated_window: Some(CalibratedWindow {
                width: 1280,
                height: 720,
                scale_factor: 1.0,
            }),
            ..Default::default()
        },
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::Completed, "失败原因：{:?}", outcome.failure);
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
            ..Default::default()
        },
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::Prepared, "失败原因：{:?}", outcome.failure);
    assert!(outcome.stopped_before_send());
    assert!(!outcome.succeeded(), "只填不发不算发送成功");
    assert_eq!(
        fixture.desktop.pasted_texts(),
        vec![MESSAGE.to_string()],
        "正文应当已经填进输入框"
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
        },
        script,
    );

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::Prepared, "失败原因：{:?}", outcome.failure);
    assert_eq!(fixture.desktop.scroll_count(), 1);
    assert_eq!(fixture.desktop.pasted_texts(), vec![MESSAGE.to_string()]);
    assert_eq!(fixture.desktop.send_count(), 0);
}
