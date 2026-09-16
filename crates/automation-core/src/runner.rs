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
    LocalOcr, Point, Rect, ScreenMetrics, Screenshot, SendTask, TaskId, TextBox,
};
use crate::regions::RelativeRegion;
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
            contact_panel: RelativeRegion::new(0.0, 0.12, 0.28, 0.88),
            chat_header: RelativeRegion::new(0.28, 0.0, 0.72, 0.10),
            chat_body: RelativeRegion::new(0.28, 0.10, 0.72, 0.72),
            composer: RelativeRegion::new(0.28, 0.82, 0.72, 0.18),
        }
    }
}

/// 注入的端口集合。
pub struct RunnerPorts {
    pub platform: Arc<dyn DesktopPlatform>,
    pub ocr: Arc<dyn LocalOcr>,
    pub matcher: Arc<dyn ContactMatcher>,
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
    pub fn succeeded(&self) -> bool {
        self.state == TaskState::Completed
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

    /// 截图 + 识别，仅对可重试的瞬时错误重试。
    fn capture_and_recognize(
        &mut self,
        region: Rect,
        step: &str,
    ) -> Result<(Screenshot, Vec<TextBox>), AutomationError> {
        let attempts = self.cfg().max_attempts.max(1);
        let platform = &self.runner.ports.platform;
        let ocr = &self.runner.ports.ocr;
        let mut last_err: Option<AutomationError> = None;
        for attempt in 1..=attempts {
            self.check_cancel()?;
            let outcome = platform.capture(region).and_then(|shot| {
                let boxes = ocr.recognize(&shot)?;
                Ok((shot, boxes))
            });
            match outcome {
                Ok(value) => {
                    self.last_frame = Some(value.clone());
                    return Ok(value);
                }
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
        let err = last_err.unwrap_or_else(|| {
            AutomationError::Platform(format!("{step} 失败且没有返回错误信息"))
        });
        Err(err)
    }

    fn execute(&mut self) -> Result<(), AutomationError> {
        self.check_cancel()?;

        // ── 启动客户端 ──────────────────────────────────────────────
        self.advance(TaskState::LaunchingClient, None)?;
        self.runner.ports.platform.launch_wecom()?;
        self.check_deadline("启动企业微信")?;

        // ── 等待客户端就绪并完成标定 ────────────────────────────────
        let window = self.runner.ports.platform.focus_wecom()?;
        if window.is_degenerate() {
            return Err(AutomationError::ClientNotReady);
        }
        let metrics = self.runner.ports.platform.screen_metrics()?;
        self.window = Some(window);
        self.metrics = Some(metrics);
        self.advance(
            TaskState::WaitingForClient,
            Some(format!(
                "窗口 {}x{} @({},{})，缩放 {}",
                window.width, window.height, window.x, window.y, metrics.scale_factor
            )),
        )?;

        // ── 查找联系人 ──────────────────────────────────────────────
        self.advance(TaskState::SearchingContact, None)?;
        let panel = self.resolve(self.cfg().contact_panel, "联系人候选区")?;
        let (panel_shot, candidates) = self.capture_and_recognize(panel, "联系人识别")?;
        self.evidence.push(format!("contact_panel#{}", panel_shot.fingerprint));
        let matched = self.runner.ports.matcher.find_unique_exact_match(
            &self.task.external_contact_name,
            &candidates,
            self.cfg().min_confidence,
        )?;

        // ── 核验候选人 ──────────────────────────────────────────────
        self.advance(
            TaskState::VerifyingCandidate,
            Some(format!("候选文字：{}", matched.text.trim())),
        )?;
        if matched.text.trim() != self.task.external_contact_name.trim() {
            return Err(AutomationError::AmbiguousVision(format!(
                "候选人文字「{}」与目标「{}」不完全一致",
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
        let expected_window = self.ensure_calibrated()?;
        let contact_screen_rect = matched.bounds.to_screen(Point { x: panel.x, y: panel.y });
        self.runner
            .ports
            .platform
            .guarded_click(contact_screen_rect.center(), expected_window)?;
        self.check_deadline("点击联系人")?;

        self.advance(TaskState::VerifyingChatHeader, None)?;
        let header = self.resolve(self.cfg().chat_header, "聊天标题区")?;
        let (header_shot, header_boxes) = self.capture_and_recognize(header, "聊天标题识别")?;
        self.evidence.push(format!("chat_header#{}", header_shot.fingerprint));
        let header_match = self.runner.ports.matcher.find_unique_exact_match(
            &self.task.external_contact_name,
            &header_boxes,
            self.cfg().min_confidence,
        )?;
        if header_match.text.trim() != self.task.external_contact_name.trim() {
            return Err(AutomationError::AmbiguousVision(format!(
                "聊天页标题「{}」与目标「{}」不一致",
                header_match.text.trim(),
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
        self.runner
            .ports
            .platform
            .guarded_click(composer.center(), expected_window)?;
        self.check_deadline("聚焦消息输入框")?;

        let body = self.resolve(self.cfg().chat_body, "聊天正文区")?;
        let (before_shot, _) = self.capture_and_recognize(body, "发送前聊天区识别")?;
        self.baseline_fingerprint = Some(before_shot.fingerprint.clone());
        self.evidence.push(format!("chat_before#{}", before_shot.fingerprint));

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
            return Err(AutomationError::NeedsHumanReview(
                "发送后聊天区截图未发生变化，无法确认消息已出现".into(),
            ));
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
