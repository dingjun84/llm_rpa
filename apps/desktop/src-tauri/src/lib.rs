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
pub mod runtime;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use automation_core::{
    CancelToken, EvidenceRecorder, Failure, ProgressSink, Rect, Screenshot, SendTask, StateChange,
    TaskId, TaskState, TextBox,
};
use serde::{Deserialize, Serialize};
use storage::{EvidenceStore, RedactedImage, SqliteAuditStore, SqliteSendLedger};
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use uuid::Uuid;
use vision::RedactionPlan;

use crate::confirmation::UiConfirmation;
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
        use base64::Engine;
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

        let (window, shot) = desktop
            .preview()
            .map_err(|err| format!("未能截取目标窗口（类名「{class}」）：{err}"))?;

        // 先缩到界面够用的宽度再编码。4K 窗口的原始 PNG + base64 能到几十 MB，
        // 塞进 webview 会明显卡顿；而标定只看比例，缩放不影响结果。
        let rgba = vision::pixels::to_rgba(&shot).map_err(|err| err.to_string())?;
        let scaled = vision::pixels::downscale_to_max_width(&rgba, PREVIEW_MAX_WIDTH);
        let png = vision::pixels::encode_png(&scaled).map_err(|err| err.to_string())?;
        let image = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&png)
        );

        Ok(WindowPreview {
            window,
            width: scaled.width(),
            height: scaled.height(),
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
        record_window_geometry
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
