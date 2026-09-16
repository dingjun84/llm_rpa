//! MVP 主路径与失败路径的端到端测试。
//!
//! 全部基于 `platform-mock` 的替身端口运行，不接触真实桌面、不接触网络。
//! 这些用例对应 `docs/architecture.md` §9 验收条件中可在软件层验证的部分。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use automation_core::{
    AuditEntry, CancelToken, MemoryAudit, MemorySendLedger, ProgressSink, RunOutcome, RunnerConfig,
    RunnerPorts, SendTask, StateChange, TaskState, WorkflowRunner,
};
use platform_mock::{
    ConfirmationOutcome, Fault, MockDesktop, MockHumanConfirmation, MockOcr, MockScenario,
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
        let desktop = Arc::new(desktop);
        let ocr = Arc::new(MockOcr::new(scenario.script()));
        let confirmation = Arc::new(confirmation);
        let audit = Arc::new(MemoryAudit::new());
        let ledger = Arc::new(MemorySendLedger::new());

        let runner = WorkflowRunner::new(
            RunnerPorts {
                platform: desktop.clone(),
                ocr: ocr.clone(),
                matcher: Arc::new(platform_mock::MockContactMatcher::new()),
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
    // 候选框图像坐标 (8,20,200,28) → 中心 (108,34)；候选区原点 (0,86)
    assert_eq!(clicks[0].x, 108);
    assert_eq!(clicks[0].y, 120);
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

#[test]
fn near_name_never_matches_exactly() {
    let scenario = MockScenario::near_name_only(CONTACT, MESSAGE);
    let fixture = Fixture::new(&scenario);

    let outcome = fixture.run(&fixture.task());

    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    assert_eq!(fixture.desktop.send_count(), 0);
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

    assert_eq!(outcome.state, TaskState::Completed);
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
