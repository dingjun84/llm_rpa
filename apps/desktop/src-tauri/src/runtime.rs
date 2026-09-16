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
    AuditSink, HumanConfirmation, LocalOcr, RelativeRegion, RunnerConfig, RunnerPorts, SendLedger,
    SendTask, StrictContactMatcher, WorkflowRunner, DEFAULT_MIN_CONFIDENCE,
};
use platform_mock::{MockContactMatcher, MockDesktop, MockHumanConfirmation, MockOcr, MockScenario};
use serde::{Deserialize, Serialize};

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
        Self {
            contact_panel: [0.00, 0.12, 0.28, 0.88],
            chat_header: [0.28, 0.00, 0.72, 0.10],
            chat_body: [0.28, 0.10, 0.72, 0.72],
            composer: [0.28, 0.82, 0.72, 0.18],
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub confirmation_ttl_secs: u64,
    pub min_confidence: f32,
    pub regions: RegionConfig,
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
            confirmation_ttl_secs: 60,
            min_confidence: DEFAULT_MIN_CONFIDENCE,
            regions: RegionConfig::default(),
        }
    }
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
            step_timeout: Duration::from_secs(10),
            max_attempts: 3,
            retry_backoff: Duration::from_millis(150),
            contact_panel,
            chat_header,
            chat_body,
            composer,
        }
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
        confirmation: Arc::new(MockHumanConfirmation::default()),
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
            matcher: Arc::new(StrictContactMatcher::default()),
            confirmation: Arc::new(MockHumanConfirmation::default()),
        })
    }
    #[cfg(not(windows))]
    {
        let _ = config;
        Err("真实模式目前只支持 Windows".to_string())
    }
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
    let mut ports = match config.mode {
        RuntimeMode::DryRun => dry_run_ports(config, task),
        RuntimeMode::Live => live_ports(config)?,
    };
    // 确认端口始终来自界面，保证真实模式下也必须人工确认。
    ports.confirmation = confirmation;

    Ok(WorkflowRunner::new(ports, config.to_runner_config())
        .with_audit(audit)
        .with_ledger(ledger))
}
