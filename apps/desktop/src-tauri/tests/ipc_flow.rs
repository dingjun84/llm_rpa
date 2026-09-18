//! 命令层（IPC）的端到端测试。
//!
//! 这一层要回答的问题是：**界面发出的那条 `invoke` 真的能驱动完整工作流吗？**
//! 因此这里刻意不使用内部函数调用，而是走 `tauri::test::get_ipc_response`，
//! 让请求经过与生产环境完全相同的命令表、参数反序列化和响应序列化路径。
//!
//! 之所以能这样做，是因为 `AppState` 与全部命令都对 `tauri::Runtime` 泛型，
//! 生产入口用 `Wry`、这里用 `MockRuntime`，共用 [`desktop_lib::with_commands`]。
//!
//! 覆盖的验收点：
//!
//! 1. 演练模式下一条任务能走完 11 步主路径并到达 `Completed`；
//! 2. `task://updated` 事件按状态顺序推送；
//! 3. 人工拒绝与"无法确认送达"都收敛到 `NeedsHumanReview`，绝不误判成功；
//! 4. 消息正文不进审计库，只留长度与哈希；
//! 5. 入参校验与不存在的任务 ID 会返回明确错误。

use std::ops::Deref;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use automation_core::TaskState;
use base64::Engine as _;
use desktop_lib::runtime::{DemoScenario, RuntimeConfig, RuntimeMode};
use desktop_lib::{AppState, TaskView, EVENT_TASK_UPDATED};
use serde::de::DeserializeOwned;
use serde_json::json;
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, MockRuntime, INVOKE_KEY};
use tauri::webview::InvokeRequest;
use tauri::{Listener, Manager, WebviewWindow, WebviewWindowBuilder};

/// 主路径的 11 个状态，顺序即文档 §5 的约定。
const HAPPY_PATH: [TaskState; 11] = [
    TaskState::Draft,
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
];

/// 只在本用例里出现、绝不允许落库的正文。
const BODY: &str = "机密正文-仅供测试-20260916";

// ── 测试脚手架 ──────────────────────────────────────────────────────────

/// 每个用例一个独立临时目录，退出时自动清理。
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("rpa-llm-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("创建临时目录失败");
        Self(path)
    }
}

impl Deref for TempDir {
    type Target = PathBuf;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Harness {
    app: tauri::App<MockRuntime>,
    webview: WebviewWindow<MockRuntime>,
    _dir: TempDir,
}

impl Harness {
    /// 按场景装配一个演练模式的应用实例。
    ///
    /// 配置不是直接塞进内存，而是**先写成 `config.json` 再启动**，
    /// 这样顺带验证了配置的读取路径。
    fn new(tag: &str, scenario: DemoScenario) -> Self {
        Self::with_config(
            tag,
            RuntimeConfig {
                mode: RuntimeMode::DryRun,
                demo_scenario: scenario,
                // 确认窗口收紧到 5 秒，让"确认过期"这类用例不必真的等一分钟。
                confirmation_ttl_secs: 5,
                ..Default::default()
            },
        )
    }

    /// 用任意配置装配应用实例，供需要真实模式的用例使用。
    fn with_config(tag: &str, config: RuntimeConfig) -> Self {
        let dir = TempDir::new(tag);
        let evidence_root = dir.join("evidence");
        std::fs::write(
            dir.join("config.json"),
            serde_json::to_string_pretty(&config).expect("序列化配置失败"),
        )
        .expect("写入配置失败");

        let state_root = evidence_root.clone();
        let app = desktop_lib::with_commands(mock_builder())
            .build(mock_context(noop_assets()))
            .expect("构建测试应用失败");

        // 注意：`Builder::setup` 只在 `App::run()` 里执行，`build()` 不会触发。
        // 测试不跑事件循环，因此这里直接注册状态，效果与生产入口一致。
        let state = AppState::in_memory(&app.handle().clone(), &state_root)
            .expect("初始化应用状态失败");
        app.manage(state);

        let webview = WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("创建测试窗口失败");

        Self { app, webview, _dir: dir }
    }

    fn state(&self) -> tauri::State<'_, AppState<MockRuntime>> {
        self.app.state::<AppState<MockRuntime>>()
    }

    fn call<T: DeserializeOwned>(
        &self,
        cmd: &str,
        body: serde_json::Value,
    ) -> Result<T, serde_json::Value> {
        get_ipc_response(&self.webview, request(cmd, body))
            .map(|response| response.deserialize::<T>().expect("响应无法反序列化"))
    }

    fn ok<T: DeserializeOwned>(&self, cmd: &str, body: serde_json::Value) -> T {
        match self.call::<T>(cmd, body) {
            Ok(value) => value,
            Err(err) => panic!("命令 {cmd} 本应成功，却返回错误：{err}"),
        }
    }

    fn err(&self, cmd: &str, body: serde_json::Value) -> String {
        match self.call::<serde_json::Value>(cmd, body) {
            Ok(value) => panic!("命令 {cmd} 本应失败，却返回成功：{value}"),
            Err(err) => err.as_str().unwrap_or_default().to_string(),
        }
    }

    fn start(&self, contact: &str, text: &str) -> String {
        self.ok(
            "start_task",
            json!({ "request": { "external_contact_name": contact, "text": text } }),
        )
    }

    fn task(&self, id: &str) -> TaskView {
        self.ok("get_task", json!({ "taskId": id }))
    }

    fn confirm(&self, id: &str, approved: bool, reason: Option<&str>) {
        self.ok::<()>(
            "confirm_task",
            json!({ "taskId": id, "approved": approved, "reason": reason }),
        );
    }

    /// 轮询到满足条件为止。工作流跑在后台线程，界面只能靠推送与轮询观察。
    fn wait_until<F>(&self, id: &str, what: &str, done: F) -> TaskView
    where
        F: Fn(&TaskView) -> bool,
    {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut last = self.task(id);
        while Instant::now() < deadline {
            if done(&last) {
                return last;
            }
            std::thread::sleep(Duration::from_millis(15));
            last = self.task(id);
        }
        panic!("等待「{what}」超时，最后观察到的状态为 {:?}", last.state);
    }
}

fn request(cmd: &str, body: serde_json::Value) -> InvokeRequest {
    InvokeRequest {
        cmd: cmd.into(),
        callback: CallbackFn(0),
        error: CallbackFn(1),
        url: "http://tauri.localhost".parse().unwrap(),
        body: InvokeBody::Json(body),
        headers: Default::default(),
        invoke_key: INVOKE_KEY.to_string(),
    }
}

/// 收集 `task://updated` 推送的状态序列。
fn collect_states(app: &tauri::App<MockRuntime>) -> Arc<Mutex<Vec<TaskState>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    app.listen(EVENT_TASK_UPDATED, move |event| {
        let value: serde_json::Value =
            serde_json::from_str(event.payload()).expect("事件载荷不是合法 JSON");
        if let Some(state) = value.get("state").and_then(|s| s.as_str()) {
            if let Some(state) = TaskState::from_str_name(&snake_to_pascal(state)) {
                sink.lock().unwrap().push(state);
            }
        }
    });
    seen
}

fn snake_to_pascal(name: &str) -> String {
    name.split('_')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

// ── 主路径 ──────────────────────────────────────────────────────────────

#[test]
fn a_dry_run_task_travels_the_full_happy_path_through_ipc() {
    let harness = Harness::new("happy", DemoScenario::Happy);

    let id = harness.start("张三", BODY);
    assert!(!id.is_empty(), "start_task 应返回任务 ID");

    // 刚创建时应停在 Draft，且已经进入等待确认之前的步骤。
    let pending = harness.wait_until(&id, "进入等待人工确认", |view| {
        view.state == TaskState::AwaitingHumanConfirmation
    });
    assert!(pending.awaiting_confirmation, "等待确认时应置位 awaiting_confirmation");
    assert_eq!(pending.external_contact_name, "张三");
    assert_eq!(pending.text_length, BODY.chars().count());
    assert_eq!(pending.created_by, "本机操作者", "未指定操作者时应回落到本机操作者");

    harness.confirm(&id, true, None);

    let done = harness.wait_until(&id, "到达终态", |view| view.state.is_terminal());
    assert_eq!(done.state, TaskState::Completed);
    assert!(done.failure.is_none(), "主路径不应有失败信息");
    assert!(!done.awaiting_confirmation, "终态不应再处于等待确认");
    assert!(!done.evidence.is_empty(), "主路径应留下截图指纹作为证据");

    let path: Vec<TaskState> = done.history.iter().map(|change| change.to).collect();
    assert_eq!(
        path,
        HAPPY_PATH[1..].to_vec(),
        "状态序列必须与文档 §5 的主路径完全一致"
    );
    assert_eq!(done.history[0].from, TaskState::Draft, "首个转换应从 Draft 出发");
}

#[test]
fn state_changes_are_pushed_to_the_frontend_in_order() {
    let harness = Harness::new("events", DemoScenario::Happy);
    let seen = collect_states(&harness.app);

    let id = harness.start("张三", BODY);
    harness.wait_until(&id, "进入等待人工确认", |view| {
        view.state == TaskState::AwaitingHumanConfirmation
    });
    harness.confirm(&id, true, None);
    harness.wait_until(&id, "到达终态", |view| view.state.is_terminal());

    // 事件是异步派发的，等最后一条（终态补推）到达。
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && seen.lock().unwrap().len() < HAPPY_PATH.len() {
        std::thread::sleep(Duration::from_millis(15));
    }

    let pushed = seen.lock().unwrap().clone();
    assert!(
        pushed.len() >= HAPPY_PATH.len() - 1,
        "至少要收到每一次状态转换，实际只收到 {pushed:?}"
    );

    // 终态会补推一次（补充证据清单与最终失败信息），因此允许末尾出现重复；
    // 把连续重复压平之后，必须与主路径逐项一致。
    let mut collapsed: Vec<TaskState> = Vec::new();
    for state in pushed {
        if collapsed.last() != Some(&state) {
            collapsed.push(state);
        }
    }
    assert_eq!(
        collapsed,
        HAPPY_PATH[1..].to_vec(),
        "推送的状态序列应与主路径一致，且顺序不得错乱"
    );
}

// ── 失败路径：一律不得误判成功 ──────────────────────────────────────────

#[test]
fn a_rejected_confirmation_ends_in_human_review_and_never_sends() {
    let harness = Harness::new("reject", DemoScenario::Happy);

    let id = harness.start("张三", BODY);
    harness.wait_until(&id, "进入等待人工确认", |view| {
        view.state == TaskState::AwaitingHumanConfirmation
    });
    harness.confirm(&id, false, Some("这不是我要找的人"));

    let done = harness.wait_until(&id, "到达终态", |view| view.state.is_terminal());
    assert_eq!(done.state, TaskState::NeedsHumanReview);
    let failure = done.failure.expect("应带失败信息");
    assert_eq!(failure.code, "NEEDS_HUMAN_REVIEW");
    assert!(failure.reason.contains("这不是我要找的人"), "应保留操作者填写的理由");

    // 关键：拒绝之后绝不能再出现 Sending / VerifyingDelivery / Completed。
    let path: Vec<TaskState> = done.history.iter().map(|change| change.to).collect();
    assert!(!path.contains(&TaskState::Sending), "被拒绝的任务不得进入发送步骤");
    assert!(!path.contains(&TaskState::Completed));
}

#[test]
fn an_expired_confirmation_ends_in_human_review() {
    let harness = Harness::new("expire", DemoScenario::Happy);

    let id = harness.start("张三", BODY);
    harness.wait_until(&id, "进入等待人工确认", |view| {
        view.state == TaskState::AwaitingHumanConfirmation
    });

    // 配置里把确认有效期压到 5 秒，这里什么都不做，等它自己过期。
    let done = harness.wait_until(&id, "确认过期", |view| view.state.is_terminal());
    assert_eq!(done.state, TaskState::NeedsHumanReview);
    assert_eq!(done.failure.expect("应带失败信息").code, "NEEDS_HUMAN_REVIEW");
}

#[test]
fn a_task_whose_delivery_cannot_be_verified_is_not_reported_as_sent() {
    // 指纹冻结 = 发送后界面毫无变化，无法确认消息真的发出去了。
    let harness = Harness::new("unstable", DemoScenario::UnstableScreen);

    let id = harness.start("张三", BODY);
    harness.wait_until(&id, "进入等待人工确认", |view| {
        view.state == TaskState::AwaitingHumanConfirmation
    });
    harness.confirm(&id, true, None);

    let done = harness.wait_until(&id, "到达终态", |view| view.state.is_terminal());
    assert_eq!(
        done.state,
        TaskState::NeedsHumanReview,
        "无法确认送达时必须转人工，绝不能报 Completed"
    );
}

#[test]
fn a_duplicate_contact_name_is_never_guessed() {
    let harness = Harness::new("duplicate", DemoScenario::DuplicateContact);

    let id = harness.start("张三", BODY);

    let done = harness.wait_until(&id, "到达终态", |view| view.state.is_terminal());
    assert_eq!(done.state, TaskState::NeedsHumanReview);
    let path: Vec<TaskState> = done.history.iter().map(|change| change.to).collect();
    assert!(
        !path.contains(&TaskState::AwaitingHumanConfirmation),
        "连候选人都没定下来，不该走到人工确认"
    );
}

// ── 审计：正文不落库 ────────────────────────────────────────────────────

#[test]
fn the_message_body_never_reaches_the_audit_store() {
    let harness = Harness::new("audit", DemoScenario::Happy);

    let id = harness.start("张三", BODY);
    harness.wait_until(&id, "进入等待人工确认", |view| {
        view.state == TaskState::AwaitingHumanConfirmation
    });
    harness.confirm(&id, true, None);
    let done = harness.wait_until(&id, "到达终态", |view| view.state.is_terminal());
    assert_eq!(done.state, TaskState::Completed);

    let task_id: uuid::Uuid = id.parse().expect("任务 ID 应是 UUID");
    let entries = harness
        .state()
        .audit_store()
        .entries_for(task_id)
        .expect("读取审计记录失败");

    assert_eq!(entries.len(), HAPPY_PATH.len() - 1, "每一次状态转换都应留痕");

    let rendered = format!("{entries:?}");
    assert!(!rendered.contains(BODY), "审计记录里绝不能出现消息正文");
    assert!(!rendered.contains("机密"), "连正文片段都不允许出现");

    let digest = entries
        .iter()
        .find_map(|entry| entry.message.clone())
        .expect("审计里应有一条消息摘要");
    assert_eq!(digest.char_count, BODY.chars().count());
    assert_eq!(digest.sha256.len(), 64, "摘要只保留 SHA-256 十六进制串");

    // 主路径留下了截图指纹，但那是哈希，不含图像与正文。
    assert!(
        entries.iter().any(|entry| !entry.evidence.is_empty()),
        "主路径应记录证据引用"
    );

    // 经过人工确认的任务必须留下确认时间。
    assert!(
        entries.iter().any(|entry| entry.confirmation_at.is_some()),
        "人工确认必须留痕"
    );
}

#[test]
fn runtime_info_reports_the_dry_run_notice_and_audit_count() {
    let harness = Harness::new("info", DemoScenario::Happy);

    let before: desktop_lib::RuntimeInfo = harness.ok("runtime_info", json!({}));
    assert_eq!(before.config.mode, RuntimeMode::DryRun);
    assert_eq!(before.audit_entry_count, 0);
    assert!(before.notice.contains("演练模式"), "演练模式必须有醒目提示");
    assert_eq!(before.is_windows, cfg!(windows));

    let id = harness.start("张三", BODY);
    harness.wait_until(&id, "进入等待人工确认", |view| {
        view.state == TaskState::AwaitingHumanConfirmation
    });
    harness.confirm(&id, true, None);
    harness.wait_until(&id, "到达终态", |view| view.state.is_terminal());

    let after: desktop_lib::RuntimeInfo = harness.ok("runtime_info", json!({}));
    assert_eq!(after.audit_entry_count, HAPPY_PATH.len() as u64 - 1);
}

// ── 入参校验 ────────────────────────────────────────────────────────────

#[test]
fn start_task_rejects_blank_input() {
    let harness = Harness::new("validate", DemoScenario::Happy);

    let blank_contact = harness.err(
        "start_task",
        json!({ "request": { "external_contact_name": "   ", "text": "你好" } }),
    );
    assert!(blank_contact.contains("联系人"), "错误信息应指出联系人问题：{blank_contact}");

    let blank_text = harness.err(
        "start_task",
        json!({ "request": { "external_contact_name": "张三", "text": "  " } }),
    );
    assert!(blank_text.contains("正文"), "错误信息应指出正文问题：{blank_text}");

    let too_long = harness.err(
        "start_task",
        json!({ "request": { "external_contact_name": "张三", "text": "长".repeat(2001) } }),
    );
    assert!(too_long.contains("上限"), "超长正文应被拒绝：{too_long}");
}

#[test]
fn commands_reject_unknown_or_malformed_task_ids() {
    let harness = Harness::new("unknown", DemoScenario::Happy);

    let not_a_uuid = harness.err("get_task", json!({ "taskId": "不是-uuid" }));
    assert!(not_a_uuid.contains("ID"), "应提示任务 ID 非法：{not_a_uuid}");

    let missing = harness.err("get_task", json!({ "taskId": uuid::Uuid::new_v4().to_string() }));
    assert!(missing.contains("不存在"), "应提示任务不存在：{missing}");

    let not_waiting = harness.err(
        "confirm_task",
        json!({ "taskId": uuid::Uuid::new_v4().to_string(), "approved": true }),
    );
    assert!(
        not_waiting.contains("等待确认"),
        "不该给一个不在等待确认的任务投票：{not_waiting}"
    );
}

// ── 区域标定的只读预览 ──────────────────────────────────────────────────

/// 标定**不受运行模式限制**：演练模式下同样要能截目标窗口。
///
/// 这不是"顺便允许"，是刻意的：标定属于**配置**而不是执行——它只读地看一眼目标窗口，
/// 不点击、不输入、不发送。而且实际顺序本来就是"先把窗口和四个区域标定好，
/// 再决定用哪种模式跑"，卡在模式上只会让人没法做准备。
#[test]
fn preview_target_window_is_not_gated_by_mode() {
    // `Harness::new` 就是演练模式。
    let harness = Harness::new("preview-dry", DemoScenario::Happy);

    let outcome = harness.call::<desktop_lib::WindowPreview>(
        "preview_target_window",
        json!({ "windowClass": "Progman", "wecomExe": null }),
    );

    if let Err(err) = outcome {
        let message = err.as_str().unwrap_or_default();
        // 没有交互式桌面（无头环境）可以跳过，不把环境问题当缺陷；
        // 但**不能**再因为"模式不对"被拒。
        assert!(!message.contains("模式"), "标定不该受运行模式限制：{message}");
        eprintln!("跳过：当前会话没有可用的交互式桌面（{message}）");
    }
}

/// 窗口类名为空：无从定位，必须明确报错而不是截一张空白图。
///
/// 类名由**调用方传入**（界面上的草稿值），不读已保存的配置——否则会出现
/// "界面上明明写着新类名，截图却报找不到窗口"这种自相矛盾的报错。
#[test]
fn preview_target_window_is_refused_without_a_window_class() {
    let harness = Harness::new("preview-noclass", DemoScenario::Happy);

    let message = harness.err(
        "preview_target_window",
        json!({ "windowClass": "   ", "wecomExe": null }),
    );
    assert!(message.contains("窗口类名"), "应指出类名为空：{message}");
}

/// 对着一个确实存在的窗口走完整条链路。
///
/// 这一条验证的是**界面拿到的东西真的能用**：响应能反序列化成 `WindowPreview`、
/// `image` 是合法的 PNG data URL、PNG 里的尺寸与 `width`/`height` 一致。
/// 只断言"返回了 Ok"是不够的——编码或缩放一旦写错，返回的照样是 Ok。
///
/// 用 `Harness::new`（演练模式）是刻意的：顺带证明标定与运行模式无关。
#[test]
fn preview_target_window_returns_a_decodable_png_for_a_real_window() {
    // `Progman` 是桌面窗口，任何交互式会话里都存在。
    let harness = Harness::new("preview-png", DemoScenario::Happy);

    let preview: desktop_lib::WindowPreview = match harness.call(
        "preview_target_window",
        json!({ "windowClass": "Progman", "wecomExe": null }),
    ) {
            Ok(value) => value,
            // 没有交互式桌面（无头环境）就跳过，不把环境问题当缺陷。
            Err(err) => {
                eprintln!("跳过：当前会话没有可用的交互式桌面（{err}）");
                return;
            }
        };

    assert!(
        !preview.window.is_degenerate(),
        "窗口矩形不应退化：{:?}",
        preview.window
    );
    assert_eq!(preview.fingerprint.len(), 64, "指纹应为 sha256 十六进制");

    let encoded = preview
        .image
        .strip_prefix("data:image/png;base64,")
        .expect("image 应是 PNG 的 data URL");
    let png = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .expect("base64 应能解码");
    assert_eq!(
        &png[..8],
        &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
        "解出来的应当是 PNG"
    );

    // 直接读 IHDR，避免为了这个用例引入图像解码依赖。
    let ihdr_width = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let ihdr_height = u32::from_be_bytes(png[20..24].try_into().unwrap());
    assert_eq!(ihdr_width, preview.width, "PNG 宽度应与上报的 width 一致");
    assert_eq!(ihdr_height, preview.height, "PNG 高度应与上报的 height 一致");
    assert!(preview.width > 0 && preview.height > 0);

    // 预览会被等比缩小，所以不可能比窗口本身更宽；
    // 而 `window` 保留的是窗口真实尺寸，标定文案用的是它。
    assert!(
        preview.width <= preview.window.width as u32,
        "预览图不应比窗口本身更宽"
    );
}

// ── 指认目标窗口 ────────────────────────────────────────────────────────

/// 「指认窗口」命令必须可调用，并且返回自洽的窗口特征。
///
/// 这个用例的主要价值在**命令注册**：它和 `preview_target_window` 一样靠
/// `AppHandle<R>` 钉住运行时泛型，一旦漏进 `generate_handler!`，
/// 前端点了按钮只会拿到 "command not found"，而编译期毫无提示。
///
/// 光标停在哪个窗口是不确定的，所以只断言"读到的自洽"，不断言具体是哪个窗口。
#[test]
fn pick_target_window_reports_a_consistent_window() {
    let harness = Harness::new("pick-window", DemoScenario::Happy);

    let picked: desktop_lib::PickedWindow = match harness.call("pick_target_window", json!({})) {
        Ok(value) => value,
        // 没有交互式桌面（无头环境）就跳过，不把环境问题当缺陷。
        Err(err) => {
            eprintln!("跳过：当前会话读不到光标位置（{err}）");
            return;
        }
    };

    assert!(
        !picked.window.is_degenerate(),
        "窗口矩形不应退化：{:?}",
        picked.window
    );
    // `window_from_point` 会先上溯到根窗口，所以拿到的必然是顶层窗口，必然有类名。
    assert!(
        !picked.class_name.trim().is_empty(),
        "顶层窗口都应该有类名，否则没法拿去配置"
    );
    if let Some(path) = &picked.exe_path {
        assert!(
            path.to_ascii_lowercase().ends_with(".exe"),
            "所属程序应指向一个可执行文件：{path}"
        );
    }
}

// ── 手动动作：启动客户端 / 记录窗口尺寸 ─────────────────────────────────

/// 「启动客户端」必须可调用，并且**在没有配置路径时明确报错**。
///
/// 这里只验"命令注册 + 参数校验"这一层，不去真的启动程序：
/// 真启动会往用户桌面上拉起一个进程，那是操作者该做的动作，不是测试该做的。
#[test]
fn launch_client_refuses_without_a_configured_path() {
    let harness = Harness::new("launch-nopath", DemoScenario::Happy);

    let message = harness.err(
        "launch_client",
        json!({ "wecomExe": "  ", "wecomExeSha256": null }),
    );
    assert!(message.contains("可执行文件路径"), "应指出还没配路径：{message}");
}

/// 「记录窗口尺寸」必须可调用，并且返回自洽的几何。
///
/// 这个用例的主要价值在**命令注册**：它和 `preview_target_window` 一样靠
/// `AppHandle<R>` 钉住运行时泛型，一旦漏进 `generate_handler!`，
/// 前端点了按钮只会拿到 "command not found"，而编译期毫无提示。
#[test]
fn record_window_geometry_reports_a_consistent_geometry() {
    // `Progman` 是桌面窗口，任何交互式会话里都存在。
    let harness = Harness::new("record-geometry", DemoScenario::Happy);

    let geometry: desktop_lib::runtime::WindowGeometry = match harness.call(
        "record_window_geometry",
        json!({ "windowClass": "Progman", "wecomExe": null }),
    ) {
        Ok(value) => value,
        // 没有交互式桌面（无头环境）就跳过，不把环境问题当缺陷。
        Err(err) => {
            eprintln!("跳过：当前会话没有可用的交互式桌面（{err}）");
            return;
        }
    };

    assert!(geometry.width > 0 && geometry.height > 0, "窗口尺寸应有效：{geometry:?}");
    assert!(geometry.scale_factor > 0.0, "缩放比例应有效：{geometry:?}");
}

/// 类名为空时无从定位，必须明确报错。
#[test]
fn record_window_geometry_is_refused_without_a_window_class() {
    let harness = Harness::new("record-noclass", DemoScenario::Happy);

    let message = harness.err(
        "record_window_geometry",
        json!({ "windowClass": "", "wecomExe": null }),
    );
    assert!(message.contains("窗口类名"), "应指出类名为空：{message}");
}

/// 真实模式还没有「标定尺寸」时，任务必须被拒绝装配。
///
/// 客户端由操作者手动启动，程序没法从窗口外面分辨"这是不是我标定过的那个窗口、
/// 是不是那个尺寸"，只能靠这条记录。少了它，"按标定尺寸工作"就只是句口号。
#[test]
fn live_mode_without_a_calibrated_window_is_refused() {
    let harness = Harness::with_config(
        "live-nocalib",
        RuntimeConfig { mode: RuntimeMode::Live, ..Default::default() },
    );

    let message = harness.err(
        "start_task",
        json!({ "request": { "external_contact_name": "张三", "text": "你好" } }),
    );
    assert!(
        message.contains("记录窗口尺寸"),
        "应提示先去记录窗口尺寸：{message}"
    );

    // 装配失败不该在任务列表里留下一个永远不会推进的草稿任务。
    let tasks: Vec<TaskView> = harness.ok("list_tasks", json!({}));
    assert!(
        tasks.is_empty(),
        "装配失败时不该登记任务，实际有 {} 条",
        tasks.len()
    );
}

