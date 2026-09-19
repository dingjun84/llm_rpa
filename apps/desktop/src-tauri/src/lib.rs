//! 桌面应用的命令层。
//!
//! 界面通过 IPC 驱动 `automation-core` 的编排器：
//!
//! ```text
//! 界面 ──invoke──> start_task ──> 工作流线程 ──> WorkflowRunner
//!   ^                                                  │
//!   └────────── task://updated 事件（状态实时推送）──────┘
//!   ^
//!   └── task://confirmation-requested ──> 确认框 ──> confirm_task
//! ```
//!
//! 消息正文只存在于内存与界面预览中，**不落库**；审计表只保存长度与哈希。

pub mod confirmation;
pub mod icon_library;
pub mod runtime;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use automation_core::{
    CancelToken, EvidenceRecorder, Failure, Point, ProgressSink, Rect, Screenshot, SendTask,
    StateChange, TaskId, TaskState, TextBox,
};
use serde::{Deserialize, Serialize};
use storage::{EvidenceStore, RedactedImage, SqliteAuditStore, SqliteSendLedger};
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use uuid::Uuid;
use vision::RedactionPlan;

use crate::confirmation::UiConfirmation;
use crate::icon_library::IconEntry;
use crate::runtime::{RuntimeConfig, RuntimeMode, WindowGeometry};

pub const EVENT_TASK_UPDATED: &str = "task://updated";
const EVIDENCE_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const MAX_MESSAGE_CHARS: usize = 2000;

fn to_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|delta| delta.as_millis() as u64)
        .unwrap_or(0)
}

// ── 传输对象 ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct StartTaskRequest {
    pub external_contact_name: String,
    pub text: String,
    #[serde(default)]
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub from: TaskState,
    pub to: TaskState,
    pub at_ms: u64,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceView {
    pub label: String,
    pub byte_len: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskView {
    pub id: String,
    pub external_contact_name: String,
    /// 仅供界面预览，来自内存，不落库。
    pub text: String,
    pub text_length: usize,
    pub created_by: String,
    pub state: TaskState,
    pub state_label: String,
    pub detail: Option<String>,
    pub failure: Option<Failure>,
    pub evidence: Vec<String>,
    pub history: Vec<HistoryEntry>,
    pub awaiting_confirmation: bool,
    pub evidence_artifacts: Vec<EvidenceView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeInfo {
    pub config: RuntimeConfig,
    pub data_dir: String,
    /// 图标库目录。界面上要显示它——模板是**文件**，用户有权知道它们存在哪。
    pub icons_dir: String,
    /// 图标模板的边长下限 / 上限（像素）。
    ///
    /// 由 `vision` 的常量下发，**不在前端再写一份**：界面上「框得太小 / 太大」的
    /// 即时提示必须和保存时真正会拒绝的那条线是同一条，否则会出现
    /// 「界面说没问题、点保存却被拒」这种最让人不知所措的组合。
    pub template_min_side: u32,
    pub template_max_side: u32,
    pub audit_entry_count: u64,
    pub is_windows: bool,
    /// 演练模式的显式提示，避免被误当成真实发送。
    pub notice: String,
}

/// 标定预览图的最大宽度。
///
/// 只为"看清比例"服务，所以不需要原始分辨率；缩到这个宽度再编码，
/// 能把 4K 窗口的 IPC 载荷从几十 MB 压到几百 KB。
const PREVIEW_MAX_WIDTH: u32 = 1280;

/// 目标窗口的一次只读预览，供界面做区域标定。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowPreview {
    /// 窗口在屏幕上的边界（像素）。
    pub window: Rect,
    /// 预览图的像素尺寸。窗口过宽时会等比缩小，因此**不一定等于**窗口尺寸；
    /// 要窗口的真实尺寸请用 [`Self::window`]。
    pub width: u32,
    pub height: u32,
    /// `data:image/png;base64,...`，可直接塞进 `<img src>`。
    pub image: String,
    /// 截图指纹，便于和审计记录里的证据引用对照。
    pub fingerprint: String,
}

/// 光标下的窗口信息，供界面「指认目标窗口」使用。
///
/// 只读快照：不点击、不聚焦、不产生任何输入。选中的是**特征**而不是 HWND ——
/// 窗口句柄不跨进程重启稳定，所以界面把这里的 `class_name` / `exe_path`
/// 写进配置，之后由平台层按特征重新定位。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PickedWindow {
    /// 窗口类名。可以直接填进 `RuntimeConfig::window_class`。
    pub class_name: String,
    /// 窗口标题。目前只用于展示与人工核对，**不参与**匹配。
    pub title: String,
    /// 窗口所属进程的可执行文件路径。读不到时为 `None`（例如权限受限的系统进程）。
    pub exe_path: Option<String>,
    /// 窗口在屏幕上的边界（像素）。
    pub window: Rect,
    /// 这个窗口是不是本程序自己。用来拦住「把界面本身指认成目标」这种误操作。
    pub is_self: bool,
}

#[derive(Debug, Clone)]
pub struct TaskRecord {
    pub task: SendTask,
    pub state: TaskState,
    pub detail: Option<String>,
    pub failure: Option<Failure>,
    pub evidence: Vec<String>,
    pub history: Vec<StateChange>,
    pub evidence_artifacts: Vec<EvidenceView>,
}

impl TaskRecord {
    fn to_view(&self, awaiting_confirmation: bool) -> TaskView {
        TaskView {
            id: self.task.id.to_string(),
            external_contact_name: self.task.external_contact_name.clone(),
            text: self.task.text.clone(),
            text_length: self.task.text.chars().count(),
            created_by: self.task.created_by.clone(),
            state: self.state,
            state_label: self.state.describe().to_string(),
            detail: self.detail.clone(),
            failure: self.failure.clone(),
            evidence: self.evidence.clone(),
            history: self
                .history
                .iter()
                .map(|change| HistoryEntry {
                    from: change.from,
                    to: change.to,
                    at_ms: to_ms(change.at),
                    detail: change.detail.clone(),
                })
                .collect(),
            awaiting_confirmation,
            evidence_artifacts: self.evidence_artifacts.clone(),
        }
    }
}

// ── 状态变更推送 ────────────────────────────────────────────────────────

struct TaskProgress<R: Runtime> {
    app: AppHandle<R>,
    tasks: Arc<Mutex<HashMap<TaskId, TaskRecord>>>,
    confirmation: Arc<UiConfirmation<R>>,
    /// 这次任务的后台活动日志。
    log_path: std::path::PathBuf,
}

/// 把一行后台活动追加到任务日志。
///
/// **为什么需要它**：界面只显示"当前状态 + 失败原因"，而排查「点了开始什么都没动」
/// 时真正要看的是**每一步的先后顺序**——走到哪一步了、在哪一步停的、停之前的细节是什么。
///
/// 为什么是文件而不是控制台：双击启动的 GUI 进程**没有控制台**，
/// 代码里那些 `eprintln!` 全都看不见。落文件是唯一可靠的观测点。
///
/// 刻意保持极简：纯追加、每行带毫秒时间戳、**写完立刻 flush**——
/// 这样进程被强杀或崩溃时，已经写下的内容仍然在盘上。
fn append_task_log(path: &std::path::Path, line: &str) {
    use std::io::Write;
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let _ = writeln!(file, "[{ms}] {line}");
    let _ = file.flush();
}

impl<R: Runtime> ProgressSink for TaskProgress<R> {
    fn state_changed(&self, change: &StateChange) {
        {
            let mut line = format!("{} -> {}", change.from.as_str(), change.to.as_str());
            if let Some(detail) = change.detail.as_deref() {
                line.push_str("  | ");
                line.push_str(detail);
            }
            if let Some(code) = change.failure_code.as_deref() {
                line.push_str("  | 失败码=");
                line.push_str(code);
            }
            append_task_log(&self.log_path, &line);
        }
        let awaiting = self.confirmation.is_pending(change.task_id);
        let view = {
            let mut tasks = match self.tasks.lock() {
                Ok(tasks) => tasks,
                Err(_) => return,
            };
            let Some(record) = tasks.get_mut(&change.task_id) else {
                return;
            };
            record.state = change.to;
            record.detail = change.detail.clone();
            // 失败收敛时，代码与原因随终态一起落地，
            // 界面不必等后台线程收尾就能显示完整失败信息。
            if let Some(code) = change.failure_code.as_ref() {
                record.failure = Some(Failure {
                    code: code.clone(),
                    reason: change.detail.clone().unwrap_or_default(),
                });
            }
            record.history.push(change.clone());
            record.to_view(awaiting)
        };
        let _ = self.app.emit(EVENT_TASK_UPDATED, view);
    }
}

// ── 失败证据：裁切 + 遮盖已识别文字 ─────────────────────────────────────

struct RedactingRecorder {
    store: Arc<EvidenceStore>,
}

impl EvidenceRecorder for RedactingRecorder {
    fn record(&self, task_id: TaskId, label: &str, frame: &Screenshot, text_boxes: &[TextBox]) {
        // 只保留这一帧本身（捕获时就已经是局部区域），
        // 并把所有识别到的文字框涂掉，避免把姓名与消息正文留存下来。
        let plan = RedactionPlan::keep_only(automation_core::Rect {
            x: 0,
            y: 0,
            width: frame.width as i32,
            height: frame.height as i32,
        })
        .with_mask(text_boxes.iter().map(|item| item.bounds));

        let Ok(png) = vision::redact(frame, &plan) else {
            return;
        };
        let _ = self.store.save(task_id, &RedactedImage::new(label, png));
    }
}

// ── 应用状态 ────────────────────────────────────────────────────────────

pub struct AppState<R: Runtime> {
    tasks: Arc<Mutex<HashMap<TaskId, TaskRecord>>>,
    order: Arc<Mutex<Vec<TaskId>>>,
    cancels: Arc<Mutex<HashMap<TaskId, CancelToken>>>,
    confirmation: Arc<UiConfirmation<R>>,
    audit: Arc<SqliteAuditStore>,
    ledger: Arc<SqliteSendLedger>,
    evidence: Arc<EvidenceStore>,
    config: Arc<Mutex<RuntimeConfig>>,
    config_path: std::path::PathBuf,
    data_dir: std::path::PathBuf,
}

impl<R: Runtime> AppState<R> {
    pub fn new(app: &AppHandle<R>) -> Result<Self, String> {
        let data_dir = app
            .path()
            .app_data_dir()
            .map_err(|err| format!("无法定位应用数据目录：{err}"))?;
        std::fs::create_dir_all(&data_dir)
            .map_err(|err| format!("无法创建应用数据目录：{err}"))?;

        let db_path = data_dir.join("audit.sqlite");
        let audit = SqliteAuditStore::open(&db_path).map_err(|err| err.to_string())?;
        let ledger = SqliteSendLedger::open(&db_path).map_err(|err| err.to_string())?;
        let evidence = EvidenceStore::open(&db_path, data_dir.join("evidence"), EVIDENCE_RETENTION)
            .map_err(|err| err.to_string())?;

        Ok(Self::assemble(
            app,
            data_dir,
            Arc::new(audit),
            Arc::new(ledger),
            Arc::new(evidence),
        ))
    }

    /// 测试用：数据库全部走内存，只有脱敏证据落在一个临时目录里。
    ///
    /// 之所以不直接复用 [`AppState::new`]，是因为它依赖 `app_data_dir()`，
    /// 而测试运行时（`MockRuntime`）没有真实的应用数据目录。
    pub fn in_memory(
        app: &AppHandle<R>,
        evidence_root: impl Into<std::path::PathBuf>,
    ) -> Result<Self, String> {
        let evidence_root = evidence_root.into();
        let data_dir = evidence_root
            .parent()
            .map(|parent| parent.to_path_buf())
            .unwrap_or_else(|| evidence_root.clone());

        let audit = SqliteAuditStore::in_memory().map_err(|err| err.to_string())?;
        let ledger = SqliteSendLedger::in_memory().map_err(|err| err.to_string())?;
        let evidence = EvidenceStore::in_memory(&evidence_root, EVIDENCE_RETENTION)
            .map_err(|err| err.to_string())?;

        Ok(Self::assemble(
            app,
            data_dir,
            Arc::new(audit),
            Arc::new(ledger),
            Arc::new(evidence),
        ))
    }

    fn assemble(
        app: &AppHandle<R>,
        data_dir: std::path::PathBuf,
        audit: Arc<SqliteAuditStore>,
        ledger: Arc<SqliteSendLedger>,
        evidence: Arc<EvidenceStore>,
    ) -> Self {
        let config_path = data_dir.join("config.json");
        let config = std::fs::read_to_string(&config_path)
            .ok()
            .and_then(|text| serde_json::from_str::<RuntimeConfig>(&text).ok())
            .unwrap_or_default();

        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            order: Arc::new(Mutex::new(Vec::new())),
            cancels: Arc::new(Mutex::new(HashMap::new())),
            confirmation: Arc::new(UiConfirmation::new(app.clone())),
            audit,
            ledger,
            evidence,
            config: Arc::new(Mutex::new(config)),
            config_path,
            data_dir,
        }
    }

    fn record(&self, task_id: TaskId) -> Option<TaskRecord> {
        self.tasks.lock().ok()?.get(&task_id).cloned()
    }

    /// 供集成测试核对审计内容。
    ///
    /// 生产代码不应绕过命令层直接读库，因此这里只是把句柄借出去，
    /// 不提供任何写入路径。
    #[doc(hidden)]
    pub fn audit_store(&self) -> Arc<SqliteAuditStore> {
        self.audit.clone()
    }

    /// 供集成测试核对已落盘的脱敏证据。
    #[doc(hidden)]
    pub fn evidence_store(&self) -> Arc<EvidenceStore> {
        self.evidence.clone()
    }
}

// ── 命令 ────────────────────────────────────────────────────────────────
//
// 关于每个命令上那个 `_app: AppHandle<R>` 参数：
//
// `tauri` 的 `impl<'r, T, R> CommandArg<'de, R> for State<'r, T>` 里，`R` 与 `T`
// 之间**没有任何约束关系**。因此如果一个命令的运行时信息只出现在
// `State<'_, AppState<R>>` 内部，`R` 就成了无法推断的自由变量，
// `generate_handler!` 会直接报 `E0283: cannot infer type`。
//
// 只有 `AppHandle<R>` / `Window<R>` / `Webview<R>` 这类实现里
// `R` 出现在 trait 的 `Self` 类型上，才能把 `R` 钉死。
//
// 该参数不会增加前端调用负担：`CommandArg for AppHandle<R>` 只从消息里取
// 应用句柄，完全不读请求体。

#[tauri::command]
fn start_task<R: Runtime>(
    state: State<'_, AppState<R>>,
    app: AppHandle<R>,
    request: StartTaskRequest,
) -> Result<String, String> {
    let contact = request.external_contact_name.trim().to_string();
    if contact.is_empty() {
        return Err("外部联系人名称不能为空".into());
    }
    let text = request.text.trim().to_string();
    if text.is_empty() {
        return Err("消息正文不能为空".into());
    }
    if text.chars().count() > MAX_MESSAGE_CHARS {
        return Err(format!("消息正文超过 {MAX_MESSAGE_CHARS} 字上限"));
    }

    let task = SendTask {
        id: Uuid::new_v4(),
        external_contact_name: contact,
        text,
        created_by: request
            .created_by
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "本机操作者".to_string()),
    };
    let task_id = task.id;

    let config = state
        .config
        .lock()
        .map_err(|_| "配置锁已中毒".to_string())?
        .clone();

    // **先装配、再登记**。装配会因为配置问题失败（例如真实模式还没记录标定尺寸），
    // 那种情况下不该在任务列表里留下一个永远不会推进的草稿任务——
    // 列表里凭空多出一条"草稿"，会让人以为任务已经提交了。
    let runner = runtime::build_runner(
        &config,
        &task,
        state.audit.clone(),
        state.ledger.clone(),
        state.confirmation.clone(),
    )?
    .with_evidence_recorder(Arc::new(RedactingRecorder { store: state.evidence.clone() }));

    {
        let mut tasks = state.tasks.lock().map_err(|_| "任务表锁已中毒".to_string())?;
        tasks.insert(
            task_id,
            TaskRecord {
                task: task.clone(),
                state: TaskState::Draft,
                detail: None,
                failure: None,
                evidence: Vec::new(),
                history: Vec::new(),
                evidence_artifacts: Vec::new(),
            },
        );
    }
    state
        .order
        .lock()
        .map_err(|_| "任务顺序锁已中毒".to_string())?
        .insert(0, task_id);

    let cancel = CancelToken::new();
    state
        .cancels
        .lock()
        .map_err(|_| "取消表锁已中毒".to_string())?
        .insert(task_id, cancel.clone());

    // 每次任务单独一个日志文件，文件名用 ID 前 8 位。
    // 开头先落一份**配置快照**——排查时第一个要问的就是"当时到底按哪份配置跑的"，
    // 而配置随时可能被改，事后再读 config.json 未必是当时那份。
    let log_path = state
        .data_dir
        .join(format!("task-{}.log", &task_id.to_string()[..8]));
    {
        let config = state
            .config
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        append_task_log(&log_path, "=== 任务开始 ===");
        append_task_log(&log_path, &format!("任务 ID    : {task_id}"));
        append_task_log(
            &log_path,
            &format!("目标联系人 : {}", task.external_contact_name),
        );
        append_task_log(&log_path, &format!("运行模式   : {:?}", config.mode));
        append_task_log(&log_path, &format!("窗口类名   : {}", config.window_class));
        append_task_log(&log_path, &format!("目标程序   : {:?}", config.wecom_exe));
        append_task_log(&log_path, &format!("OCR 程序   : {:?}", config.ocr_command));
        append_task_log(
            &log_path,
            &format!("标定尺寸   : {:?}", config.calibrated_window),
        );
        append_task_log(
            &log_path,
            &format!(
                "滚动/扫描  : 每次 {} 格，最多 {} 次，最多扫 {} 轮，停稳等待 ≤{}ms",
                config.scroll_notches_per_step,
                config.max_scroll_attempts,
                config.max_search_sweeps,
                config.scroll_settle_ms
            ),
        );
        append_task_log(
            &log_path,
            &format!(
                "只填不发   : {}    卡死检测: {}    记录识别结果: {}",
                config.stop_before_send, config.liveness_check, config.log_ocr_candidates
            ),
        );
        // 「先点导航图标切视图」会改变"在哪一屏找联系人"，
        // 出问题时第一件要确认的就是"当时到底开没开、用的哪张模板"。
        if config.navigate_before_search {
            let names = config
                .nav_icon_templates
                .iter()
                .map(|path| path.trim())
                .filter(|path| !path.is_empty())
                .collect::<Vec<_>>()
                .join(" | ");
            append_task_log(
                &log_path,
                &format!(
                    "切换视图   : 开    模板 {} 张 [{}]    搜索区 {:?}    最低分 {:.2}",
                    config.nav_icon_templates.iter().filter(|p| !p.trim().is_empty()).count(),
                    names,
                    config.nav_strip,
                    config.nav_icon_min_score
                ),
            );
        }
        // 放宽匹配是**临时措施**，但它会改变"点到谁"这个结果，
        // 所以必须在每次任务的配置快照里留痕：事后复盘时不用去翻当时改没改配置。
        if config.relaxed_name_match {
            append_task_log(
                &log_path,
                "⚠️ 姓名匹配 : 已放宽为「包含即可」（临时措施，非架构要求的逐字精确匹配）",
            );
            if !config.stop_before_send {
                append_task_log(
                    &log_path,
                    "⚠️ 注意     : 放宽匹配 + 允许真实发送同时打开 —— 存在「找错人」的风险",
                );
            }
        }
        append_task_log(&log_path, "=== 状态轨迹 ===");
    }

    let progress = TaskProgress {
        app: app.clone(),
        tasks: state.tasks.clone(),
        confirmation: state.confirmation.clone(),
        log_path: log_path.clone(),
    };
    let tasks = state.tasks.clone();
    let cancels = state.cancels.clone();
    let evidence = state.evidence.clone();

    // 工作流是同步阻塞的（还要等人工确认），因此放到独立线程，不阻塞界面。
    std::thread::spawn(move || {
        let outcome = runner.run(&task, &progress, &cancel);

        append_task_log(&log_path, "=== 结束 ===");
        append_task_log(&log_path, &format!("终态     : {}", outcome.state.as_str()));
        if let Some(failure) = outcome.failure.as_ref() {
            append_task_log(&log_path, &format!("失败码   : {}", failure.code));
            append_task_log(&log_path, &format!("失败原因 : {}", failure.reason));
        }
        if !outcome.evidence.is_empty() {
            append_task_log(&log_path, "过程证据 :");
            for item in &outcome.evidence {
                append_task_log(&log_path, &format!("  - {item}"));
            }
        }
        append_task_log(&log_path, &format!("日志文件 : {}", log_path.display()));

        let artifacts = evidence
            .list_for(task_id)
            .map(|records| {
                records
                    .into_iter()
                    .map(|record| EvidenceView {
                        label: record.label,
                        byte_len: record.byte_len,
                        sha256: record.sha256,
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let view = if let Ok(mut tasks) = tasks.lock() {
            tasks.get_mut(&task_id).map(|record| {
                record.state = outcome.state;
                record.failure = outcome.failure.clone();
                record.evidence = outcome.evidence.clone();
                record.evidence_artifacts = artifacts;
                record.to_view(false)
            })
        } else {
            None
        };
        if let Ok(mut cancels) = cancels.lock() {
            cancels.remove(&task_id);
        }
        // 终态补推：状态本身在 `state_changed` 里已经推过一次，
        // 这一次是为了把落盘后的证据清单与最终失败信息补齐。
        // 因此同一个终态可能出现两次，界面按幂等处理即可。
        if let Some(view) = view {
            let _ = app.emit(EVENT_TASK_UPDATED, view);
        }
    });

    Ok(task_id.to_string())
}

#[tauri::command]
fn list_tasks<R: Runtime>(
    state: State<'_, AppState<R>>,
    _app: AppHandle<R>,
) -> Result<Vec<TaskView>, String> {
    let order = state.order.lock().map_err(|_| "任务顺序锁已中毒".to_string())?.clone();
    let tasks = state.tasks.lock().map_err(|_| "任务表锁已中毒".to_string())?;
    Ok(order
        .iter()
        .filter_map(|id| tasks.get(id))
        .map(|record| record.to_view(state.confirmation.is_pending(record.task.id)))
        .collect())
}

#[tauri::command]
fn get_task<R: Runtime>(
    state: State<'_, AppState<R>>,
    _app: AppHandle<R>,
    task_id: String,
) -> Result<TaskView, String> {
    let id: Uuid = task_id.parse().map_err(|_| "任务 ID 非法".to_string())?;
    let record = state.record(id).ok_or_else(|| "任务不存在".to_string())?;
    Ok(record.to_view(state.confirmation.is_pending(id)))
}

#[tauri::command]
fn confirm_task<R: Runtime>(
    state: State<'_, AppState<R>>,
    _app: AppHandle<R>,
    task_id: String,
    approved: bool,
    reason: Option<String>,
) -> Result<(), String> {
    let id: Uuid = task_id.parse().map_err(|_| "任务 ID 非法".to_string())?;
    state.confirmation.decide(id, approved, reason)
}

#[tauri::command]
fn cancel_task<R: Runtime>(
    state: State<'_, AppState<R>>,
    _app: AppHandle<R>,
    task_id: String,
) -> Result<(), String> {
    let id: Uuid = task_id.parse().map_err(|_| "任务 ID 非法".to_string())?;
    let cancels = state.cancels.lock().map_err(|_| "取消表锁已中毒".to_string())?;
    match cancels.get(&id) {
        Some(token) => {
            token.cancel();
            Ok(())
        }
        None => Err("该任务当前不可取消".into()),
    }
}

#[tauri::command]
fn runtime_info<R: Runtime>(
    state: State<'_, AppState<R>>,
    _app: AppHandle<R>,
) -> Result<RuntimeInfo, String> {
    let config = state.config.lock().map_err(|_| "配置锁已中毒".to_string())?.clone();
    let notice = match config.mode {
        RuntimeMode::DryRun => {
            "当前为演练模式：全部使用替身端口，不会启动企业微信、不会产生任何真实输入。".to_string()
        }
        RuntimeMode::Live => {
            "当前为真实模式：会操作本机企业微信窗口。发送期间请勿切换窗口或操作鼠标键盘。"
                .to_string()
        }
    };
    Ok(RuntimeInfo {
        config,
        data_dir: state.data_dir.display().to_string(),
        icons_dir: icon_library::dir(&state.data_dir).display().to_string(),
        template_min_side: vision::MIN_TEMPLATE_SIDE,
        template_max_side: vision::MAX_TEMPLATE_SIDE,
        audit_entry_count: state.audit.count().unwrap_or(0),
        is_windows: cfg!(windows),
        notice,
    })
}

#[tauri::command]
fn set_runtime_config<R: Runtime>(
    state: State<'_, AppState<R>>,
    _app: AppHandle<R>,
    config: RuntimeConfig,
) -> Result<(), String> {
    let text = serde_json::to_string_pretty(&config).map_err(|err| err.to_string())?;
    std::fs::write(&state.config_path, text).map_err(|err| format!("保存配置失败：{err}"))?;
    *state.config.lock().map_err(|_| "配置锁已中毒".to_string())? = config;
    Ok(())
}

/// 按「窗口类名 +（可选）可执行文件路径」构造桌面适配器。
///
/// 标定类的命令（预览、量图标、截图标模板、试点击）**全都走它**，
/// 这样定位规则与任务执行时是同一套。为什么要强调这一点：Qt 系程序
/// （微信 4.x 就是）所有顶层窗口共用同一个类名，只按类名定位可能选中**登录窗**，
/// 那样标定就全白做了——而标定结果看起来完全正常。
#[cfg(windows)]
fn desktop_for(window_class: &str, wecom_exe: Option<&str>) -> platform_windows::WindowsDesktop {
    use platform_windows::{WindowsDesktop, WindowsDesktopConfig};

    WindowsDesktop::new(WindowsDesktopConfig {
        wecom_exe: wecom_exe
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from),
        window_matcher: platform_windows::WindowMatcher::ClassName(window_class.to_string()),
        ..WindowsDesktopConfig::default()
    })
}

/// 把一帧窗口画面压成界面能直接塞进 `<img src>` 的 `data:` URL，并返回它的像素尺寸。
///
/// 先缩到 [`PREVIEW_MAX_WIDTH`] 再编码：4K 窗口的原始 PNG + base64 能到几十 MB，
/// 塞进 webview 会明显卡顿；而标定只看比例，缩放不影响结果。
///
/// ⚠️ 缩过的图**只能用来给人看**。要拿来做模板必须回原始分辨率重截一次——
/// 这里是 Triangle 滤波的重采样，裁出来的图案边缘会带上插值出来的杂色，
/// 拿去匹配真实画面自然对不准，而且分数不会低到让人起疑。
#[cfg(windows)]
fn preview_data_url(shot: &Screenshot) -> Result<(String, u32, u32), String> {
    use base64::Engine;

    let rgba = vision::pixels::to_rgba(shot).map_err(|err| err.to_string())?;
    let scaled = vision::pixels::downscale_to_max_width(&rgba, PREVIEW_MAX_WIDTH);
    let png = vision::pixels::encode_png(&scaled).map_err(|err| err.to_string())?;
    Ok((
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&png)
        ),
        scaled.width(),
        scaled.height(),
    ))
}

/// 截一张目标窗口的预览图，供界面做区域标定。
///
/// 这是**纯只读**操作：只截屏，不点击、不粘贴、不发送，也不会把窗口带到前台。
/// 捕获范围严格限制在已定位窗口的边界内，因此不会扫描整屏。
///
/// 为什么不做成"先聚焦再截图"：区域标定只需要看清窗口长什么样，
/// 而抢焦点在 Windows 前台锁定策略下经常被拒绝（调用方自身不在前台时），
/// 会让这个功能时灵时不灵。
///
/// **两个刻意的设计**：
///
/// 1. **不按运行模式设限。** 标定属于"配置"，不属于"执行"：它只读地看一眼目标窗口，
///    不产生任何输入，跟这次任务跑演练还是跑真实无关。实际使用顺序也往往是
///    "先把窗口和四个区域标定好，再决定用哪种模式跑"，卡在模式上只会让人没法准备。
/// 2. **窗口类名与目标程序路径由调用方传入，不读已保存的配置。** 界面上显示的是**草稿**，
///    用户改了类名但还没点「保存配置」时，配置里仍是旧值。如果这里读配置，
///    就会出现"界面上明明写着新类名，截图却报找不到窗口"这种自相矛盾的报错。
///    传进来的就是用户此刻看到的值，两边不可能不一致。
///
/// 传 `wecom_exe` 是为了让定位规则和任务执行时**完全一致**：Qt 系程序所有顶层窗口
/// 共用同一个类名，只按类名截到的可能是登录窗——那样标定就白做了。
#[tauri::command]
fn preview_target_window<R: Runtime>(
    _app: AppHandle<R>,
    window_class: String,
    wecom_exe: Option<String>,
) -> Result<WindowPreview, String> {
    let class = window_class.trim().to_string();

    if class.is_empty() {
        return Err("窗口类名为空，无法定位目标窗口。先点「指认窗口」或手工填写类名。".into());
    }

    #[cfg(windows)]
    {
        let desktop = desktop_for(&class, wecom_exe.as_deref());

        let (window, shot) = desktop
            .preview()
            .map_err(|err| format!("未能截取目标窗口（类名「{class}」）：{err}"))?;

        let (image, width, height) = preview_data_url(&shot)?;

        Ok(WindowPreview {
            window,
            width,
            height,
            image,
            fingerprint: shot.fingerprint,
        })
    }
    #[cfg(not(windows))]
    {
        let _ = (class, wecom_exe);
        Err("区域标定目前只支持 Windows".to_string())
    }
}

/// 读出**光标下**的窗口，供界面「指认目标窗口」。
///
/// 为什么是"悬停"而不是"点击选中"：点击会在目标程序里产生副作用——
/// 在会话列表上点一下就把会话打开了，在输入框上点一下就把光标挪走了。
/// 悬停读取则完全没有副作用，而且不必知道目标程序内部结构。
///
/// 界面在倒计时期间反复调用它做实时回显，所以这里必须足够轻：
/// 只读光标、窗口矩形、类名、标题、所属进程路径，不截屏、不聚焦。
#[tauri::command]
fn pick_target_window<R: Runtime>(_app: AppHandle<R>) -> Result<PickedWindow, String> {
    #[cfg(windows)]
    {
        use platform_windows::winapi;

        let (x, y) = winapi::cursor_position()?;
        let hwnd = winapi::window_from_point(x, y)
            .ok_or_else(|| format!("光标位置 ({x}, {y}) 下没有窗口"))?;

        let window = winapi::window_rect(hwnd)?;
        let exe_path = winapi::window_process_path(hwnd).ok();

        // 用「可执行文件路径」而不是进程 ID 判自己：PID 每次启动都变，
        // 而当前进程的路径随时能拿到，比较也不依赖任何 Win32 调用。
        let is_self = match (&exe_path, std::env::current_exe()) {
            (Some(picked), Ok(own)) => picked
                .to_string_lossy()
                .eq_ignore_ascii_case(&own.to_string_lossy()),
            _ => false,
        };

        Ok(PickedWindow {
            class_name: winapi::window_class_name(hwnd),
            title: winapi::window_title(hwnd),
            exe_path: exe_path.map(|path| path.display().to_string()),
            window,
            is_self,
        })
    }
    #[cfg(not(windows))]
    {
        Err("指认窗口目前只支持 Windows".to_string())
    }
}

// ── 手动动作：启动客户端 / 记录窗口尺寸 ─────────────────────────────────

/// 由操作者**手动**启动客户端。
///
/// 刻意不做成任务流程的一步：客户端由操作者自己启动并登录，任务只负责接管
/// 已经就绪的窗口。自动启动会带来三件麻烦事——重复启动弹出登录窗、
/// 登录窗与主窗同类名（窗口定位会选错）、以及"还没登录完就被当成就绪"。
///
/// 参数取**草稿**而不是已保存配置（理由同 `preview_target_window`）：
/// 用户刚填好路径还没点保存时，按草稿启动才是他此刻看到的那份配置。
///
/// 路径存在性与 SHA-256 校验沿用平台层那一套：对不上就拒绝启动。
#[tauri::command]
fn launch_client<R: Runtime>(
    _app: AppHandle<R>,
    wecom_exe: String,
    wecom_exe_sha256: Option<String>,
) -> Result<(), String> {
    let path = wecom_exe.trim();
    if path.is_empty() {
        return Err("还没有配置目标程序的可执行文件路径。先点「指认窗口」或手工填写。".into());
    }

    #[cfg(windows)]
    {
        use automation_core::DesktopPlatform;
        use platform_windows::{WindowsDesktop, WindowsDesktopConfig};

        let desktop = WindowsDesktop::new(WindowsDesktopConfig {
            wecom_exe: Some(std::path::PathBuf::from(path)),
            wecom_exe_sha256: wecom_exe_sha256.filter(|value| !value.trim().is_empty()),
            ..WindowsDesktopConfig::default()
        });
        desktop.launch_wecom().map_err(|err| err.to_string())
    }
    #[cfg(not(windows))]
    {
        let _ = (path, wecom_exe_sha256);
        Err("启动客户端目前只支持 Windows".to_string())
    }
}

/// 记录目标窗口当前的尺寸与显示器缩放，供「按标定尺寸工作」使用。
///
/// **纯只读**：只读窗口矩形与显示器指标，不截屏、不点击、不聚焦。
///
/// 窗口类名与目标程序路径都由调用方传草稿值，而且必须传——记录用的定位规则要和
/// 任务执行时**完全一致**，否则"记下来的"和"任务看到的"可能不是同一个窗口。
#[tauri::command]
fn record_window_geometry<R: Runtime>(
    _app: AppHandle<R>,
    window_class: String,
    wecom_exe: Option<String>,
) -> Result<WindowGeometry, String> {
    let class = window_class.trim().to_string();
    if class.is_empty() {
        return Err("窗口类名为空，无法定位目标窗口。先点「指认窗口」或手工填写类名。".into());
    }

    #[cfg(windows)]
    {
        use platform_windows::{WindowsDesktop, WindowsDesktopConfig};

        let desktop = WindowsDesktop::new(WindowsDesktopConfig {
            wecom_exe: wecom_exe
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(std::path::PathBuf::from),
            window_matcher: platform_windows::WindowMatcher::ClassName(class.clone()),
            ..WindowsDesktopConfig::default()
        });

        let (window, metrics) = desktop
            .measure()
            .map_err(|err| format!("未能读取目标窗口尺寸（类名「{class}」）：{err}"))?;

        Ok(WindowGeometry {
            x: window.x,
            y: window.y,
            width: window.width,
            height: window.height,
            scale_factor: metrics.scale_factor,
        })
    }
    #[cfg(not(windows))]
    {
        let _ = (class, wecom_exe);
        Err("记录窗口尺寸目前只支持 Windows".to_string())
    }
}

/// 「测试图标匹配」的结果。
///
/// 刻意**不是**"成功 / 失败"两态：这是一个标定工具，最有用的是**分数本身**。
/// 分数 0.62 低于阈值 0.80 时，操作者需要知道的是"差多少"，
/// 而不是一句"匹配失败"——前者能决定是去重截模板、还是去调阈值。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NavIconProbe {
    /// 目标窗口在屏幕上的边界（像素）。
    pub window: Rect,
    /// 预览图的像素尺寸（可能被等比缩小，见 [`WindowPreview::width`]）。
    pub width: u32,
    pub height: u32,
    /// `data:image/png;base64,...`，可直接塞进 `<img src>`。
    pub image: String,
    /// 搜索区，**窗口内相对坐标**。界面按 `width / window.width` 缩放后画框。
    pub strip: Rect,
    /// 最佳命中。`None` 表示所有模板都放不进搜索区。
    pub hit: Option<NavIconHit>,
    /// 面向操作者的一句话结论（含分数与是否过阈值）。
    pub notice: String,
}

/// 一次图标命中的位置与分数（坐标均为**窗口内相对坐标**）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NavIconHit {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// 归一化互相关系数，1.0 表示完全一致。
    pub score: f32,
    /// 命中的是哪一张模板（文件名）。
    pub template: String,
    /// 是否达到了最低分数。**没过阈值也要把位置报出来**——
    /// 操作者需要看到"它认为像的地方在哪儿"，那才是判断模板对不对的依据。
    pub accepted: bool,
}

/// 在当前画面上试一次图标模板匹配，把**分数和位置**报出来。
///
/// 这是给「先点击导航图标跳转」做标定用的：模板截得对不对、阈值该定多少，
/// 都只能靠对着真实画面量一次。纯只读——只截屏，不点击、不聚焦、不产生任何输入。
///
/// 参数一律取**草稿**（理由同 `preview_target_window`）：用户刚填好路径还没点保存时，
/// 按草稿量出来的结果才是他此刻看到的那份配置。
#[tauri::command]
fn probe_nav_icon<R: Runtime>(
    _app: AppHandle<R>,
    window_class: String,
    wecom_exe: Option<String>,
    nav_strip: [f32; 4],
    templates: Vec<String>,
    min_score: f32,
) -> Result<NavIconProbe, String> {
    let class = window_class.trim().to_string();
    if class.is_empty() {
        return Err("窗口类名为空，无法定位目标窗口。先点「指认窗口」或手工填写类名。".into());
    }

    let region = automation_core::RelativeRegion::new(
        nav_strip[0],
        nav_strip[1],
        nav_strip[2],
        nav_strip[3],
    );
    region
        .validate()
        .map_err(|err| format!("导航图标搜索区的比例不合法（{nav_strip:?}）：{err}"))?;
    if !(0.0..=1.0).contains(&min_score) {
        return Err(format!("最低分数必须在 0–1 之间（当前 {min_score}）"));
    }

    // 模板载入与任务装配用的是**同一个函数**：这个按钮报出来的分数必须和任务里
    // 真正会用的那张图一致，否则标定就白做了。
    let templates = runtime::load_nav_icon_templates(&templates)?;

    #[cfg(windows)]
    {
        let desktop = desktop_for(&class, wecom_exe.as_deref());

        let (window, shot) = desktop
            .preview()
            .map_err(|err| format!("未能截取目标窗口（类名「{class}」）：{err}"))?;

        // 搜索区在**窗口图像**里的位置：窗口矩形是屏幕坐标，截图是窗口自己的画面，
        // 所以要把屏幕原点减掉。
        let strip_screen = region
            .resolve_within(window)
            .map_err(|err| format!("导航图标搜索区越出了窗口：{err}"))?;
        let strip_in_image = Rect {
            x: strip_screen.x - window.x,
            y: strip_screen.y - window.y,
            width: strip_screen.width,
            height: strip_screen.height,
        };
        let strip_shot =
            vision::crop(&shot, strip_in_image).map_err(|err| format!("裁出搜索区失败：{err}"))?;

        let mut best: Option<(f32, Rect, String)> = None;
        for template in &templates {
            // 这里用 `match_template` 而不是 `TemplateLocator::locate`：
            // 后者会把"低于阈值"变成一个错误，而这个按钮要的正是**分数**本身。
            match vision::match_template(&strip_shot, template) {
                Ok(Some((bounds, score))) => {
                    if best.as_ref().map(|(current, _, _)| score > *current).unwrap_or(true) {
                        best = Some((score, bounds, template.label.clone()));
                    }
                }
                Ok(None) => {}
                Err(err) => return Err(format!("模板「{}」无法匹配：{err}", template.label)),
            }
        }

        let hit = best.map(|(score, bounds, template)| NavIconHit {
            // 换算到窗口内相对坐标，界面才能画在整窗预览图上。
            x: strip_in_image.x + bounds.x,
            y: strip_in_image.y + bounds.y,
            width: bounds.width,
            height: bounds.height,
            score,
            template,
            accepted: score >= min_score,
        });

        let notice = match &hit {
            Some(found) if found.accepted => format!(
                "命中：模板「{}」分数 {:.3}（阈值 {:.3}），位置 窗口内 ({}, {}) {}x{}。",
                found.template, found.score, min_score, found.x, found.y, found.width, found.height
            ),
            Some(found) => format!(
                "分数不足：模板「{}」最高 {:.3}，低于阈值 {:.3}。\
                 位置 窗口内 ({}, {}) 是它认为最像的地方——对着预览图看看那里是不是图标；\
                 如果不是，说明模板截错了或者搜索区没盖住图标。",
                found.template, found.score, min_score, found.x, found.y
            ),
            None => "所有模板都放不进搜索区：模板比搜索区还大，先把搜索区调宽或把模板截小一点。"
                .to_string(),
        };

        let (image, width, height) = preview_data_url(&shot)?;

        Ok(NavIconProbe {
            window,
            width,
            height,
            image,
            strip: Rect {
                x: strip_in_image.x,
                y: strip_in_image.y,
                width: strip_in_image.width,
                height: strip_in_image.height,
            },
            hit,
            notice,
        })
    }
    #[cfg(not(windows))]
    {
        let _ = (class, region, min_score, templates);
        Err("图标匹配测试目前只支持 Windows".to_string())
    }
}

// ── 图标库 ──────────────────────────────────────────────────────────────
//
// 图标库里存的是「从真实画面上框出来的小图标」。它服务的是导航图标那一步：
// 图标上没有文字，OCR 读不到，只能靠模板匹配。
//
// 四个命令分三类：
//
// - **库的管理**（`list_icons` / `delete_icon`）：不碰屏幕，纯文件操作；
// - **取模板**（`save_icon_from_crop`）：**只读屏幕**，把界面上框出来的那块裁下来存好；
// - **验证**（`click_icon`）：**会真的点一下**。这是唯一会产生输入事件的命令，
//   也是唯一一个必须由人明确点下去的——它不在任务流程里，任务走的是编排器。

#[tauri::command]
fn list_icons<R: Runtime>(
    _app: AppHandle<R>,
    state: State<'_, AppState<R>>,
) -> Result<Vec<IconEntry>, String> {
    icon_library::list(&state.data_dir)
}

#[tauri::command]
fn delete_icon<R: Runtime>(
    _app: AppHandle<R>,
    state: State<'_, AppState<R>>,
    name: String,
) -> Result<(), String> {
    icon_library::delete(&state.data_dir, &name)
}

/// 把预览图上框出来的一块换算成**窗口图像坐标系**里的框。
///
/// 单独抽出来是为了能直接测：这段换算写错的表现是「模板截到了旁边的空白」，
/// 而那种错误在界面上**看不出来**——框明明画在图标上，存下来的却是别处。
fn window_rect_from_preview(
    rect: [i32; 4],
    preview: [u32; 2],
    shot_width: u32,
    shot_height: u32,
) -> Result<Rect, String> {
    if preview[0] == 0 || shot_width == 0 {
        return Err("预览图尺寸无效，请重新点「截取窗口画面」。".to_string());
    }

    // 预览图是**等比**缩放的，所以两个轴用同一个比例。
    let scale = shot_width as f32 / preview[0] as f32;
    let map = |value: i32| (value as f32 * scale).round() as i32;

    // 两条边分别换算再相减，而不是把宽高直接乘：
    // 后者会因为两次取整而与「左边界的实际位置」差一个像素，
    // 而图标只有二十几个像素，一个像素的偏移会实打实地吃掉一条边。
    let left = map(rect[0]);
    let top = map(rect[1]);
    // `saturating_add`：这个函数接收的是前端传来的原始数字，越界输入必须能安全拒绝，
    // 而不是在 debug 构建下直接溢出 panic。
    let right = map(rect[0].saturating_add(rect[2]));
    let bottom = map(rect[1].saturating_add(rect[3]));

    let mut width = right - left;
    let mut height = bottom - top;
    if width <= 0 || height <= 0 {
        return Err("框太小了：拖出来的框换算到窗口坐标后不足 1 像素。".to_string());
    }

    // 越界只**平移**，不缩尺寸：缩尺寸会悄悄改掉模板的大小，
    // 而模板大小必须与图标严格一致，否则匹配分数会掉下来且看不出原因。
    if width > shot_width as i32 || height > shot_height as i32 {
        return Err(format!(
            "框比窗口画面还大（{width}×{height}，窗口 {shot_width}×{shot_height}），\
             请重新框一次。"
        ));
    }
    let mut x = left.max(0);
    let mut y = top.max(0);
    if x + width > shot_width as i32 {
        x = shot_width as i32 - width;
    }
    if y + height > shot_height as i32 {
        y = shot_height as i32 - height;
    }
    width = width.min(shot_width as i32);
    height = height.min(shot_height as i32);

    Ok(Rect { x, y, width, height })
}

/// 把界面上框出来的一块存成图标模板。
///
/// `rect` 是**预览图坐标系**里的框（`[x, y, w, h]`），`preview` 是那张预览图的像素尺寸。
/// 两个都要传：光有框没法换算回窗口坐标。
///
/// ## 为什么重新截一张，而不是直接从预览图上裁
///
/// 预览图是缩过的（[`PREVIEW_MAX_WIDTH`] + Triangle 滤波）。从它上面裁出来的模板
/// 边缘会带上插值出来的杂色，拿去匹配**原始分辨率**的真实画面自然对不准——
/// 而且分数不会低到让人起疑，只会表现为「图标明明在，就是匹配不上」。
/// 所以这里按框重新截一张原始分辨率的窗口画面再裁。
///
/// 这是**只读**操作：只截屏，不点击、不聚焦、不产生任何输入。
#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn save_icon_from_crop<R: Runtime>(
    _app: AppHandle<R>,
    state: State<'_, AppState<R>>,
    name: String,
    window_class: String,
    wecom_exe: Option<String>,
    rect: [i32; 4],
    preview: [u32; 2],
) -> Result<IconEntry, String> {
    let class = window_class.trim().to_string();
    if class.is_empty() {
        return Err("窗口类名为空，无法定位目标窗口。先点「指认窗口」或手工填写类名。".into());
    }
    if rect[2] <= 0 || rect[3] <= 0 {
        return Err("框选的宽高必须大于 0——在画面上按住左键拖出一个框，再点保存。".into());
    }
    // 名字先校验一遍。`icon_library::save` 里还会再校验一次（那是**判据所在的地方**），
    // 这里重复一次是为了把拒绝的时机提前到"碰屏幕之前"：
    // 名字不合法就不该去截一张图，用户改了名字再点就是了。
    icon_library::validate_name(&name)?;

    #[cfg(windows)]
    {
        let desktop = desktop_for(&class, wecom_exe.as_deref());
        let (_window, shot) = desktop
            .preview()
            .map_err(|err| format!("未能截取目标窗口（类名「{class}」）：{err}"))?;

        // ── 先确认「现在这一帧」和「刚才那张预览」是同一个尺寸 ────────────
        //
        // 框是在预览图上量的，换算回窗口坐标靠的是两者的比例。窗口在「预览」与
        // 「保存」之间改了尺寸的话，这个比例就变了——换算出来的框会落到别处，
        // 而截出来的模板看起来**挺正常**，只是它不是那个图标。
        // 这种错没有任何症状，直到任务里点错地方，所以宁可直接拒绝。
        //
        // 用**同一个缩放函数**算期望尺寸，不在这里另写一份比例公式：
        // 两处各算一遍，迟早会有一天只改了一处。
        let rgba = vision::pixels::to_rgba(&shot).map_err(|err| err.to_string())?;
        let scaled = vision::pixels::downscale_to_max_width(&rgba, PREVIEW_MAX_WIDTH);
        if (scaled.width(), scaled.height()) != (preview[0], preview[1]) {
            return Err(format!(
                "窗口尺寸在预览之后变了：预览时 {}×{}，现在是 {}×{}。\
                 框选的坐标是按当时那张图量的，直接换算会落到别的地方——\
                 请重新点「截取窗口画面」，在**当前**这张图上重新框一次。",
                preview[0],
                preview[1],
                scaled.width(),
                scaled.height()
            ));
        }

        let rect = window_rect_from_preview(rect, preview, shot.width, shot.height)?;
        icon_library::save(&state.data_dir, &name, &shot, rect)
    }
    #[cfg(not(windows))]
    {
        let _ = (class, wecom_exe, rect, preview, name, state);
        Err("图标模板目前只能在 Windows 上截取".to_string())
    }
}

/// 「定位并点击」的结果。
///
/// 刻意把**分数**和**画面变了没有**都带回来：这个按钮的用途就是回答
/// 「这张模板到底能不能用」，而「能用」拆开是两件事——找得到（分数够）、
/// 点得动（画面变了）。只说一句"成功"没法告诉人下一步该调什么。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IconClickResult {
    pub window: Rect,
    pub width: u32,
    pub height: u32,
    /// 点击之后的整窗预览，供界面画框。
    pub image: String,
    pub strip: Rect,
    pub hit: NavIconHit,
    /// 实际点击的**屏幕**坐标。
    pub clicked: Point,
    /// 点击后联系人候选区的画面有没有变化。
    pub changed: bool,
    pub notice: String,
}

/// 定位一个图标并**真的点它一下**，把「找得到吗、点得动吗」一次答清楚。
///
/// ## 它和任务里的 `NavigatingToView` 是什么关系
///
/// 同一个动作，但**判据不同**：任务里点击后画面没变只记警告、继续往下走
/// （理由见 `docs/todo.md` T11——"界面本来就停在这个视图上"是最常见的场景，
/// 硬失败会让第二次跑必然失败）。而这里是一个人在按按钮，他要知道的就是
/// "这一下到底有没有生效"，所以如实回报 `changed`，不做任何收敛。
///
/// ## 为什么它会产生输入事件、却仍然不做模式设限
///
/// 它是**人点的一个按钮**，不是任务流程的一步——和「启动客户端」同类。
/// 运行模式决定的是"任务怎么执行"，不是"人能不能动手"。按钮上必须写明它会真的点。
///
/// `settle_ms` 复用界面上「滚动停稳等待」那个值：等待的物理现象是同一个
/// （界面动画还没画完），没有理由再立第二个旋钮。
#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn click_icon<R: Runtime>(
    _app: AppHandle<R>,
    window_class: String,
    wecom_exe: Option<String>,
    nav_strip: [f32; 4],
    contact_panel: [f32; 4],
    templates: Vec<String>,
    min_score: f32,
    settle_ms: u64,
) -> Result<IconClickResult, String> {
    let class = window_class.trim().to_string();
    if class.is_empty() {
        return Err("窗口类名为空，无法定位目标窗口。先点「指认窗口」或手工填写类名。".into());
    }

    let strip_region = automation_core::RelativeRegion::new(
        nav_strip[0],
        nav_strip[1],
        nav_strip[2],
        nav_strip[3],
    );
    strip_region
        .validate()
        .map_err(|err| format!("导航图标搜索区的比例不合法（{nav_strip:?}）：{err}"))?;

    let panel_region = automation_core::RelativeRegion::new(
        contact_panel[0],
        contact_panel[1],
        contact_panel[2],
        contact_panel[3],
    );
    panel_region
        .validate()
        .map_err(|err| format!("联系人候选区的比例不合法（{contact_panel:?}）：{err}"))?;

    if !(0.0..=1.0).contains(&min_score) {
        return Err(format!("最低分数必须在 0–1 之间（当前 {min_score}）"));
    }

    // 模板载入与任务装配用的是**同一个函数**：这里量到的分数必须就是任务里会用的那个。
    let templates = runtime::load_nav_icon_templates(&templates)?;

    #[cfg(windows)]
    {
        use automation_core::{DesktopPlatform, IconLocator};

        let desktop = desktop_for(&class, wecom_exe.as_deref());

        // 先接管窗口：点击必须落在客户端上，而 `guarded_click` 还会核对前台窗口。
        let window = desktop.focus_wecom().map_err(|err| {
            format!(
                "把目标窗口带到前台失败：{err}。\
                 先手动点一下客户端窗口（有时前台锁定会挡掉程序发起的置前），再点这个按钮。"
            )
        })?;

        // 卡死的窗口点下去不会有任何反应，而返回值上完全看不出来。
        // 这里先问一句系统，比事后从"画面没变"去猜要直接得多。
        match desktop.is_responsive() {
            Ok(true) => {}
            Ok(false) => {
                return Err(
                    "系统判定目标窗口**没有响应**（界面线程没在取消息）。\
                     往卡死的窗口里点击不会有任何效果，先看看客户端是不是卡住了。"
                        .to_string(),
                )
            }
            Err(err) => {
                return Err(format!("无法判断目标窗口是否响应：{err}。为安全起见不执行点击。"))
            }
        }

        let (_win, shot) = desktop
            .preview()
            .map_err(|err| format!("未能截取目标窗口（类名「{class}」）：{err}"))?;

        let strip_screen = strip_region
            .resolve_within(window)
            .map_err(|err| format!("导航图标搜索区越出了窗口：{err}"))?;
        let strip_in_image = Rect {
            x: strip_screen.x - window.x,
            y: strip_screen.y - window.y,
            width: strip_screen.width,
            height: strip_screen.height,
        };
        let strip_shot =
            vision::crop(&shot, strip_in_image).map_err(|err| format!("裁出搜索区失败：{err}"))?;

        // 走 `TemplateLocator`（也就是任务里那个实现），**阈值在这里是硬的**：
        // 分数不够就是错误。这个按钮问的是"能不能用"，"分数不够但先点了"
        // 正好是最不能接受的结果——它会真的点到一个不确定的地方。
        //
        // 失败时把"什么都没点"说在最前面：操作者按的是个会真的产生点击的按钮，
        // 他必须先知道这一下有没有落下去。
        let found = vision::TemplateLocator
            .locate(&strip_shot, &templates, min_score)
            .map_err(|err| format!("这一次**没有执行任何点击**——先得能确定图标在哪，才谈得上点它：{err}"))?;

        let hit_screen = found.bounds.to_screen(Point {
            x: strip_screen.x,
            y: strip_screen.y,
        });
        let target = hit_screen.center();

        let panel_screen = panel_region
            .resolve_within(window)
            .map_err(|err| format!("联系人候选区越出了窗口：{err}"))?;
        let capture_panel = || {
            desktop
                .capture(panel_screen)
                .map(|frame| frame.fingerprint)
                .map_err(|err| format!("截取联系人候选区失败：{err}"))
        };

        let before = capture_panel()?;
        desktop
            .guarded_click(target, window)
            .map_err(|err| format!("点击失败：{err}"))?;

        // 等画面停稳再比：截早了会截到重绘的中间帧，与点击前偶然相同，
        // 于是把一次成功的切换报成"没变化"。轮询间隔由超时推出来，
        // 用的是编排器里同一个函数（同一个物理现象，不该有两套比例）。
        let timeout = Duration::from_millis(settle_ms);
        let mut after = capture_panel()?;
        if !timeout.is_zero() {
            let interval = automation_core::settle_poll_interval(timeout);
            let deadline = std::time::Instant::now() + timeout;
            while std::time::Instant::now() < deadline {
                std::thread::sleep(interval);
                let current = capture_panel()?;
                if current == after {
                    break;
                }
                after = current;
            }
        }
        let changed = after != before;

        let hit = NavIconHit {
            x: strip_in_image.x + found.bounds.x,
            y: strip_in_image.y + found.bounds.y,
            width: found.bounds.width,
            height: found.bounds.height,
            score: found.score,
            template: found.template_label.clone(),
            // 能走到这里就说明分数过了阈值——阈值不够时上面那行 locate 已经返回错误了。
            accepted: true,
        };

        let notice = if changed {
            format!(
                "已点击：模板「{}」分数 {:.3}，点击屏幕 ({}, {})。\
                 点击后联系人候选区的画面变了——这一步确实生效了。",
                hit.template, hit.score, target.x, target.y
            )
        } else {
            format!(
                "已点击：模板「{}」分数 {:.3}，点击屏幕 ({}, {})。\
                 但点击后联系人候选区的画面**没有变化**。两种可能：\
                 ①界面本来就已经停在这个视图上（正常，上次点完就留在这儿了）；\
                 ②这次点击没有生效（客户端卡住、图标被别的窗口挡住、\
                 或者框到的那块位置根本不响应点击）。\
                 想区分它们：先手动把界面切到别的视图，再点一次这个按钮——\
                 这次要是变了，就说明模板和坐标都是好的。",
                hit.template, hit.score, target.x, target.y
            )
        };

        let (image, width, height) = preview_data_url(&shot)?;

        Ok(IconClickResult {
            window,
            width,
            height,
            image,
            strip: Rect {
                x: strip_in_image.x,
                y: strip_in_image.y,
                width: strip_in_image.width,
                height: strip_in_image.height,
            },
            hit,
            clicked: target,
            changed,
            notice,
        })
    }
    #[cfg(not(windows))]
    {
        let _ = (class, wecom_exe, min_score, templates, settle_ms);
        Err("图标点击测试目前只支持 Windows".to_string())
    }
}

// ── 入口 ────────────────────────────────────────────────────────────────
/// 把命令表装到 builder 上。
///
/// 这里必须让 `generate_handler!` **直接出现在 `invoke_handler(...)` 的实参位置**：
/// 宏展开出的闭包参数不带类型标注，只有借助该形参上的
/// `F: Fn(Invoke<R>) -> bool + Send + Sync + 'static` 约束，运行时泛型 `R`
/// 才能被推断出来。一旦改成 `let h = generate_handler![...];` 或包一层
/// `-> impl Fn(...)`，都会因为闭包签名无法推断而报 `E0282`。
///
/// 抽成泛型函数的好处是：生产入口用 `Wry`、测试入口用 `MockRuntime`，
/// 共用同一份命令表，避免"测试跑的命令和线上不是同一批"。
pub fn with_commands<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder.invoke_handler(tauri::generate_handler![
        start_task,
        list_tasks,
        get_task,
        confirm_task,
        cancel_task,
        runtime_info,
        set_runtime_config,
        preview_target_window,
        pick_target_window,
        launch_client,
        record_window_geometry,
        probe_nav_icon,
        list_icons,
        save_icon_from_crop,
        delete_icon,
        click_icon
    ])
}

pub fn run() {
    with_commands(tauri::Builder::default())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle().clone();
            match AppState::new(&handle) {
                Ok(state) => {
                    app.manage(state);
                }
                Err(err) => {
                    eprintln!("[desktop] 初始化应用状态失败：{err}");
                    return Err(err.into());
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("启动桌面应用失败");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 没有缩放时（窗口宽度本来就没超过预览上限）框应当**原样**通过。
    #[test]
    fn a_frame_without_scaling_maps_one_to_one() {
        let rect = window_rect_from_preview([100, 50, 26, 26], [974, 734], 974, 734).unwrap();
        assert_eq!(
            (rect.x, rect.y, rect.width, rect.height),
            (100, 50, 26, 26)
        );
    }

    /// 窗口比预览上限宽时，框要按比例放大回原始分辨率。
    ///
    /// 这一条盯的是"模板大小必须与真实图标一致"：换算错了，截出来的模板
    /// 会是图标的一部分或者连着一圈背景，而匹配分数只是"偏低一点"。
    #[test]
    fn a_scaled_preview_maps_the_box_back_up() {
        // 2560x1440 的窗口被缩到 1280x720 做预览 ⇒ 比例 2。
        let rect = window_rect_from_preview([100, 50, 26, 26], [1280, 720], 2560, 1440).unwrap();
        assert_eq!((rect.x, rect.y), (200, 100));
        assert_eq!((rect.width, rect.height), (52, 52));
    }

    /// 两条边分别取整：宽高不能"用宽度乘比例"算出来，否则会差一个像素。
    ///
    /// 用一个必然踩到取整边界的比例（1.5，框宽 1）：
    ///
    /// - 分别取整：左边界 `round(1×1.5)=2`、右边界 `round(2×1.5)=3` ⇒ 宽 1（正确，
    ///   它盖住的正是画面里 `[2, 3)` 这一个像素）；
    /// - 直接乘：`round(1×1.5)=2` ⇒ 宽 2，多吃了旁边一列像素。
    ///
    /// 图标只有二十几个像素，多吃一列就是整条边都带着隔壁的背景。
    #[test]
    fn both_edges_are_rounded_independently() {
        let rect = window_rect_from_preview([1, 1, 1, 1], [100, 60], 150, 90).unwrap();
        assert_eq!(rect.x, 2);
        assert_eq!(
            rect.width, 1,
            "宽度必须由两条边相减得出，不能直接用宽度乘比例"
        );
        assert_eq!(rect.height, 1);
    }

    /// 越界的框只**平移**进画面，不缩尺寸。
    ///
    /// 缩尺寸会把模板改小，而模板大小必须与图标严格一致：改小之后分数会掉下来，
    /// 且没有任何提示告诉人"是坐标换算把它改小了"。
    #[test]
    fn an_out_of_bounds_box_is_shifted_not_shrunk() {
        // 比例 2，框右边界超出预览宽度 ⇒ 换算后越过画面右边缘。
        let rect = window_rect_from_preview([1270, 0, 20, 20], [1280, 720], 2560, 1440).unwrap();
        assert_eq!(rect.width, 40, "尺寸必须保持，不能被缩");
        assert_eq!(rect.x + rect.width, 2560, "整体平移到贴住右边缘");

        // 负数左边界：夹到 0。比例是 2，所以 40 个预览像素对应 80 个窗口像素——
        // 尺寸按比例走，夹的只是位置。
        let rect = window_rect_from_preview([-30, -30, 40, 40], [1280, 720], 2560, 1440).unwrap();
        assert_eq!((rect.x, rect.y), (0, 0));
        assert_eq!((rect.width, rect.height), (80, 80));
    }

    /// 换算后不足一个像素、或者比画面还大的框，一律明确报错。
    #[test]
    fn a_degenerate_or_oversized_box_is_refused() {
        assert!(window_rect_from_preview([0, 0, 0, 0], [1280, 720], 2560, 1440).is_err());
        assert!(window_rect_from_preview([0, 0, 3000, 20], [1280, 720], 2560, 1440).is_err());
        // 预览尺寸为 0：说明界面传了个没截过图的空值。
        assert!(window_rect_from_preview([0, 0, 10, 10], [0, 0], 2560, 1440).is_err());
    }

    /// 极端输入不能 panic（前端传的是原始 JSON 数字，什么都有可能）。
    #[test]
    fn absurd_numbers_are_rejected_without_panicking() {
        assert!(window_rect_from_preview(
            [i32::MAX, i32::MAX, i32::MAX, i32::MAX],
            [1280, 720],
            2560,
            1440
        )
        .is_err());
        assert!(window_rect_from_preview([i32::MIN, 0, 10, 10], [1280, 720], 2560, 1440).is_err());
    }
}
