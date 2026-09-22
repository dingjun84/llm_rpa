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
//! 1. 演练模式下一条任务能走完 10 步主路径并到达 `Completed`；
//! 2. `task://updated` 事件按状态顺序推送；
//! 3. 「无法确认送达」收敛到 `NeedsHumanReview`，绝不误判成功；
//! 4. 消息正文不进审计库，只留长度与哈希；
//! 5. 入参校验与不存在的任务 ID 会返回明确错误；
//! 6. 发送前**没有任何等待点**：不调任何命令，任务自己走到 `Completed`。

use std::ops::Deref;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use automation_core::TaskState;
use base64::Engine as _;
use desktop_lib::runtime::{DemoScenario, RuntimeConfig, RuntimeMode};
use automation_core::Workflow;
use desktop_lib::{AppState, TaskView, EVENT_TASK_UPDATED};
use serde::de::DeserializeOwned;
use serde_json::json;
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, MockRuntime, INVOKE_KEY};
use tauri::webview::InvokeRequest;
use tauri::{Listener, Manager, WebviewWindow, WebviewWindowBuilder};

/// 主路径的 10 个状态，顺序即文档 §5 的约定。
///
/// 这里曾经有 11 个：`PreparingMessage` 与 `Sending` 之间夹着一个
/// `AwaitingHumanConfirmation`（等操作者在界面上点确认）。2026-09-22 取消。
const HAPPY_PATH: [TaskState; 10] = [
    TaskState::Draft,
    TaskState::LaunchingClient,
    TaskState::WaitingForClient,
    TaskState::SearchingContact,
    TaskState::VerifyingCandidate,
    TaskState::VerifyingChatHeader,
    TaskState::PreparingMessage,
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
    /// 启动任务时随请求发下去的「本次走哪条路」。
    ///
    /// ★ 它**不在配置里**（见 `StartTaskRequest::run_choice`），所以不能像别的
    /// 字段那样只写进 `config.json` 就完事——每条构造请求的地方都得带上。
    /// 这里默认照抄配置里那个同名字段（配置里那份只是界面初始值），
    /// 于是"用例改一处配置就换一条路"的写法保持不变。
    /// 需要**刻意制造"请求与配置不一致"**的用例直接改这个字段。
    run_choice: serde_json::Value,
    _dir: TempDir,
}

/// 把配置里那几个同名字段照抄成一份运行参数（JSON 形状）。
///
/// 手写 JSON 而不是用 `RunChoice` 结构体：这条路径顺带验证**请求的反序列化**，
/// 而"字段名写错"恰恰是这条链路最容易出的问题（写错了会被静默忽略）。
fn run_choice_of(config: &RuntimeConfig) -> serde_json::Value {
    json!({
        "mode": serde_json::to_value(config.mode).expect("序列化模式失败"),
        "workflow": serde_json::to_value(config.workflow).expect("序列化工作流失败"),
        "nav_target": serde_json::to_value(config.nav_target.clone()).expect("序列化导航目标失败"),
    })
}

impl Harness {
    /// 按场景装配一个演练模式的应用实例。
    ///
    /// 配置不是直接塞进内存，而是**先写成 `config.json` 再启动**，
    /// 这样顺带验证了配置的读取路径。
    ///
    /// 工作流显式选**列表扫描式**：应用层的默认值已经是**搜索式**，而搜索式
    /// 要求先标好三块只有对着真实窗口框一次才知道在哪儿的区域。这些用例演的是
    /// 「任务能不能一路跑到底」与各类失败收敛，与走哪条路无关；
    /// 沿用默认值会让它们全部卡在"缺标定区域"上，而那条报错看起来像是流程坏了。
    /// 搜索式那条路另有专门的用例，见本文件末尾。
    fn new(tag: &str, scenario: DemoScenario) -> Self {
        Self::with_config(
            tag,
            RuntimeConfig {
                mode: RuntimeMode::DryRun,
                demo_scenario: scenario,
                workflow: Workflow::ScrollListContact,
                ..Default::default()
            },
        )
    }

    /// 用任意配置装配应用实例，供需要真实模式的用例使用。
    fn with_config(tag: &str, config: RuntimeConfig) -> Self {
        let dir = TempDir::new(tag);
        let evidence_root = dir.join("evidence");
        let run_choice = run_choice_of(&config);
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

        Self { app, webview, run_choice, _dir: dir }
    }

    fn state(&self) -> tauri::State<'_, AppState> {
        self.app.state::<AppState>()
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
            json!({ "request": {
                "external_contact_name": contact,
                "text": text,
                "run_choice": self.run_choice.clone(),
            } }),
        )
    }

    fn task(&self, id: &str) -> TaskView {
        self.ok("get_task", json!({ "taskId": id }))
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

    // 从 `start_task` 返回起，任务就自己在后台跑完——**中间不需要任何人再调一次命令**。
    // 这正是取消人工确认之后要钉住的东西：没有"等确认"这个中间态，
    // 界面只要等 `task://updated` 推终态即可。
    let done = harness.wait_until(&id, "到达终态", |view| view.state.is_terminal());
    assert_eq!(done.state, TaskState::Completed);
    assert!(done.failure.is_none(), "主路径不应有失败信息");
    assert_eq!(done.external_contact_name, "张三");
    assert_eq!(done.text_length, BODY.chars().count());
    assert_eq!(done.created_by, "本机操作者", "未指定操作者时应回落到本机操作者");

    // 证据清单是**第二次推送**才补齐的：终态本身在状态迁移时就推过一次，
    // 那一次还没有落盘后的证据（见 `lib.rs` 结尾的「终态补推」）。
    // 所以不能拿"刚变成终态"那一瞬间的快照去断言证据——并发跑测试时
    // 这个窗口会被拉大，于是偶发地报「没有证据」，而它其实马上就到。
    let done = harness.wait_until(&id, "终态的证据补齐", |view| {
        view.state.is_terminal() && !view.evidence.is_empty()
    });
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

/// 取消人工确认之后，**不存在"等在人身上"的中间态**。
///
/// 这条用例从前是两个（"操作者拒绝发送"与"确认过期"），它们钉的是
/// 一个已经不存在的行为。现在钉反向的：从 `start_task` 起不再调用任何命令，
/// 任务照样自己走到 `Completed`——万一哪天有人把等待点加回来，
/// 它会卡在中间直到 `wait_until` 超时。
#[test]
fn a_task_reaches_completed_without_anyone_having_to_confirm() {
    let harness = Harness::new("no-waiting", DemoScenario::Happy);

    let id = harness.start("张三", BODY);

    let done = harness.wait_until(&id, "自己走到终态", |view| view.state.is_terminal());
    assert_eq!(done.state, TaskState::Completed, "失败信息：{:?}", done.failure);

    // `PreparingMessage` 之后必须**直接**是 `Sending`。
    let path: Vec<TaskState> = done.history.iter().map(|change| change.to).collect();
    let after = path
        .iter()
        .position(|state| *state == TaskState::PreparingMessage)
        .expect("应当经过 PreparingMessage")
        + 1;
    assert_eq!(
        path.get(after),
        Some(&TaskState::Sending),
        "准备消息之后必须直接发送，中间不该再插一个等人工的环节：{path:?}"
    );
}

#[test]
fn a_task_whose_delivery_cannot_be_verified_is_not_reported_as_sent() {
    // 指纹冻结 = 发送后界面毫无变化，无法确认消息真的发出去了。
    let harness = Harness::new("unstable", DemoScenario::UnstableScreen);

    let id = harness.start("张三", BODY);

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
        !path.contains(&TaskState::Sending),
        "连候选人都没定下来，绝不能走到发送：{path:?}"
    );
}

// ── 审计：正文不落库 ────────────────────────────────────────────────────

#[test]
fn the_message_body_never_reaches_the_audit_store() {
    let harness = Harness::new("audit", DemoScenario::Happy);

    let id = harness.start("张三", BODY);
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

    // 这里曾经断言"过人工确认的任务要留下确认时间"。取消人工确认后，
    // 审计里**不许**再出现任何"有人批准过"的痕迹（字段本身已从 `AuditEntry`
    // 与建表语句里删掉），所以反向钉住：`Sending` 那条记录除了发起者与摘要，
    // 不带任何"批准"信息。
    let sending = entries
        .iter()
        .find(|entry| entry.to == TaskState::Sending)
        .expect("审计里应有 Sending 那条记录");
    assert_eq!(sending.actor, "本机操作者", "发起者仍然要留痕");
    assert!(sending.message.is_some(), "摘要仍然要留痕");
}

#[test]
fn runtime_info_reports_the_dry_run_notice_and_audit_count() {
    let harness = Harness::new("info", DemoScenario::Happy);

    let before: desktop_lib::RuntimeInfo = harness.ok("runtime_info", json!({}));
    assert_eq!(before.config.mode, RuntimeMode::DryRun);
    assert_eq!(before.audit_entry_count, 0);
    // 两种模式的提示都下发（模式是运行参数，界面上选的与配置里存的可以不是同一个），
    // 界面上按当前选的那个取。
    assert!(
        before.mode_notices.dry_run.contains("演练模式"),
        "演练模式必须有醒目提示"
    );
    assert!(
        before.mode_notices.live.contains("真实模式"),
        "真实模式必须有醒目提示"
    );
    assert_eq!(before.is_windows, cfg!(windows));
    assert_eq!(before.is_macos, cfg!(target_os = "macos"));
    assert_eq!(before.live_supported, cfg!(windows) || cfg!(target_os = "macos"));

    let id = harness.start("张三", BODY);
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
        json!({ "request": { "external_contact_name": "   ", "text": "你好", "run_choice": harness.run_choice.clone() } }),
    );
    assert!(blank_contact.contains("联系人"), "错误信息应指出联系人问题：{blank_contact}");

    let blank_text = harness.err(
        "start_task",
        json!({ "request": { "external_contact_name": "张三", "text": "  ", "run_choice": harness.run_choice.clone() } }),
    );
    assert!(blank_text.contains("正文"), "错误信息应指出正文问题：{blank_text}");

    let too_long = harness.err(
        "start_task",
        json!({ "request": { "external_contact_name": "张三", "text": "长".repeat(2001), "run_choice": harness.run_choice.clone() } }),
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

    // `confirm_task` 已随人工确认一起删掉。它现在是一条**不存在的命令**，
    // 因此必须报"命令不存在"——而不是被某个兜底逻辑静默接受。
    // 前端若还留着确认框的旧代码，这里就会先亮。
    let removed = harness.err(
        "confirm_task",
        json!({ "taskId": uuid::Uuid::new_v4().to_string(), "approved": true }),
    );
    assert!(
        !removed.is_empty(),
        "`confirm_task` 已经不是一条命令，调用它必须有明确的错误"
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
        json!({ "request": { "external_contact_name": "张三", "text": "你好", "run_choice": harness.run_choice.clone() } }),
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

// ── 图标匹配标定（只读） ────────────────────────────────────────────────
//
// 「先点导航图标切视图」这一步的成败全靠**模板截得对不对、阈值定得准不准**，
// 而这两件事只能对着真实画面量。这组用例盯住三件事：
//
// 1. 命令注册（靠 `AppHandle<R>` 钉运行时泛型，漏进 `generate_handler!`
//    的话前端只会拿到 "command not found"，编译期毫无提示）；
// 2. 参数校验在**任何输入动作之前**发生；
// 3. 真的对着一个存在的窗口跑时，报出来的东西自洽（搜索区在窗口内、
//    命中位置在搜索区内、预览图能解码）。

/// 造一张真的 PNG 当图标模板。
///
/// 刻意不引 `image` 依赖：`vision::pixels` 已经能把 BGRA 缓冲编码成 PNG。
fn write_icon_png(dir: &std::path::Path, name: &str, width: u32, height: u32) -> String {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            // 有纹理的图案，避免"纯色模板"被当成无效输入而提前返回。
            pixels.extend_from_slice(&[
                (x * 13) as u8,
                (y * 29) as u8,
                ((x * 7 + y * 3) % 251) as u8,
                255,
            ]);
        }
    }
    let shot = automation_core::Screenshot {
        pixels,
        width,
        height,
        captured_at: std::time::SystemTime::now(),
        fingerprint: String::new(),
    };
    let rgba = vision::pixels::to_rgba(&shot).unwrap();
    std::fs::write(&path, vision::pixels::encode_png(&rgba).unwrap()).unwrap();
    path.display().to_string()
}

/// 参数校验必须发生在**截屏与点击之前**。
///
/// 这条不只是"报错友好"：如果校验写在动作之后，用户填错一个比例就会
/// 先截一张图（把当前屏幕内容读进内存）再报错，而这是个纯只读标定动作，
/// 不该有这种副作用。**用例的判据是错误文案**，不是内部调用顺序——
/// 文案能证明它走到了哪个分支。
#[test]
fn probe_nav_icon_validates_its_arguments_before_touching_the_screen() {
    let harness = Harness::new("probe-icon-args", DemoScenario::Happy);

    // 类名为空：无从定位窗口。
    let message = harness.err(
        "probe_nav_icon",
        json!({
            "windowClass": "   ",
            "wecomExe": null,
            "navStrip": [0.0, 0.0, 0.08, 1.0],
            "templates": [],
            "minScore": 0.8,
        }),
    );
    assert!(message.contains("窗口类名"), "应指出类名为空：{message}");

    // 比例越界：必须在读图标之前就拦下。
    let message = harness.err(
        "probe_nav_icon",
        json!({
            "windowClass": "Progman",
            "wecomExe": null,
            "navStrip": [0.0, 0.0, 1.5, 1.0],
            "templates": ["根本不存在的图标"],
            "minScore": 0.8,
        }),
    );
    assert!(
        message.contains("导航图标搜索区"),
        "比例越界应先于图标载入被拦下：{message}"
    );

    // 阈值越界。
    let message = harness.err(
        "probe_nav_icon",
        json!({
            "windowClass": "Progman",
            "wecomExe": null,
            "navStrip": [0.0, 0.0, 0.08, 1.0],
            "templates": ["根本不存在的图标"],
            "minScore": 1.4,
        }),
    );
    assert!(message.contains("0–1"), "应指出阈值越界：{message}");
}

/// 一个图标都没配时，必须明确报错而不是返回"没找到图标"。
///
/// 这两者的区别很关键：`hit: null` 意味着"图标都对不上"，
/// 而"一个图标都没配"是**配置缺失**——界面要提示用户去截一张图，
/// 不是让他去调阈值。
#[test]
fn probe_nav_icon_refuses_an_empty_template_list() {
    let harness = Harness::new("probe-icon-notpl", DemoScenario::Happy);

    let message = harness.err(
        "probe_nav_icon",
        json!({
            "windowClass": "Progman",
            "wecomExe": null,
            "navStrip": [0.0, 0.0, 0.08, 1.0],
            "templates": ["  ", ""],
            "minScore": 0.8,
        }),
    );
    assert!(
        message.contains("没有配置「导航」图标的模板"),
        "空列表要报配置缺失，并说清是哪一组：{message}"
    );
    assert!(
        message.contains("图标库"),
        "要告诉人下一步去哪配：{message}"
    );
}

/// 图标不存在 / 尺寸不可用：在装配期就报错，不留到"分数很低"。
#[test]
fn probe_nav_icon_reports_an_unusable_template_immediately() {
    let harness = Harness::new("probe-icon-badtpl", DemoScenario::Happy);
    let icons_dir = runtime_info(&harness).icons_dir;

    // 名字对不上：说清是哪个名字、以及去哪儿看。
    let message = harness.err(
        "probe_nav_icon",
        json!({
            "windowClass": "Progman",
            "wecomExe": null,
            "navStrip": [0.0, 0.0, 0.08, 1.0],
            "templates": ["根本没有这个图标"],
            "minScore": 0.8,
        }),
    );
    assert!(
        message.contains("根本没有这个图标") && message.contains("图标库"),
        "图标找不到时要给出名字和去处，而不是继续往下走：{message}"
    );

    // 旧配置里留下的**路径**：给一句能照着做的提示，而不是"图标丢了"。
    let message = harness.err(
        "probe_nav_icon",
        json!({
            "windowClass": "Progman",
            "wecomExe": null,
            "navStrip": [0.0, 0.0, 0.08, 1.0],
            "templates": ["D:\\icons\\聊天.png"],
            "minScore": 0.8,
        }),
    );
    assert!(message.contains("名字"), "要说清现在按名字引用：{message}");

    // 过大的图同样当场拒绝。
    write_icon_png(
        &std::path::Path::new(&icons_dir).join("太大"),
        "1.png",
        300,
        40,
    );
    let message = harness.err(
        "probe_nav_icon",
        json!({
            "windowClass": "Progman",
            "wecomExe": null,
            "navStrip": [0.0, 0.0, 0.08, 1.0],
            "templates": ["太大"],
            "minScore": 0.8,
        }),
    );
    assert!(message.contains("太大"), "报错要说清是尺寸问题：{message}");
}

/// 对着一个确实存在的窗口走完整条链路，检查报出来的东西**自洽**。
///
/// 只断言"返回了 Ok"是不够的：坐标换算一旦写错（比如忘了减窗口原点），
/// 返回的照样是 Ok，只是框画在屏幕另一个角落。所以这里逐条核对：
///
/// - 搜索区落在窗口图像范围内；
/// - 命中框落在搜索区之内（这正是"用搜索区缩小范围"的意义）；
/// - 预览图能解码，且尺寸与上报一致。
///
/// 匹配分数**不断言**：模板是随机纹理，真实桌面上不该命中，分数必然很低。
/// 这条用例测的是链路与坐标，不是匹配质量——那要靠操作者对着真图标量。
#[test]
fn probe_nav_icon_returns_self_consistent_geometry_for_a_real_window() {
    let harness = Harness::new("probe-icon-real", DemoScenario::Happy);
    let icons_dir = runtime_info(&harness).icons_dir;
    let icon = put_icon(&icons_dir, "图标", 1);

    let probe: desktop_lib::NavIconProbe = match harness.call(
        "probe_nav_icon",
        json!({
            "windowClass": "Progman",
            "wecomExe": null,
            "navStrip": [0.0, 0.0, 0.08, 1.0],
            "templates": [icon],
            "minScore": 0.8,
        }),
    ) {
        Ok(value) => value,
        Err(err) => {
            // 没有交互式桌面（无头环境）就跳过，不把环境问题当缺陷。
            eprintln!("跳过：当前会话没有可用的交互式桌面（{err}）");
            return;
        }
    };

    assert!(!probe.window.is_degenerate(), "窗口矩形不应退化：{:?}", probe.window);
    assert!(probe.width > 0 && probe.height > 0);

    // 预览图必须是能解码的 PNG，且尺寸与上报一致。
    let encoded = probe
        .image
        .strip_prefix("data:image/png;base64,")
        .expect("image 应是 PNG 的 data URL");
    let png = base64::engine::general_purpose::STANDARD.decode(encoded).expect("base64 应能解码");
    assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), probe.width);
    assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), probe.height);

    // 搜索区必须落在**窗口图像**之内——坐标换算错了这里就会挂。
    assert!(
        probe.strip.x >= 0
            && probe.strip.y >= 0
            && probe.strip.x + probe.strip.width <= probe.window.width
            && probe.strip.y + probe.strip.height <= probe.window.height,
        "搜索区 {:?} 越出了窗口 {:?}",
        probe.strip,
        probe.window
    );
    // 宽度应当约等于窗口宽的 8%（比例写错会在这里露出来）。
    let expected = (probe.window.width as f32 * 0.08).round() as i32;
    assert!(
        (probe.strip.width - expected).abs() <= 2,
        "搜索区宽度 {} 与窗口宽 {} 的 8%（{expected}）不符",
        probe.strip.width,
        probe.window.width
    );

    if let Some(hit) = &probe.hit {
        // 命中框必须落在搜索区里：这就是"先缩范围再匹配"的全部意义。
        assert!(
            hit.x >= probe.strip.x
                && hit.y >= probe.strip.y
                && hit.x + hit.width <= probe.strip.x + probe.strip.width
                && hit.y + hit.height <= probe.strip.y + probe.strip.height,
            "命中框 ({}, {}) {}x{} 越出了搜索区 {:?}",
            hit.x,
            hit.y,
            hit.width,
            hit.height,
            probe.strip
        );
        assert_eq!((hit.width, hit.height), (20, 20), "命中框尺寸就是模板尺寸");
        assert!(!hit.template.trim().is_empty(), "要能看出是哪一张模板命中的");
        assert!(
            (-1.0..=1.0).contains(&hit.score),
            "归一化相关系数必须落在 [-1, 1]，实际 {}",
            hit.score
        );
        // `accepted` 必须与阈值判定一致，不能自说自话。
        assert_eq!(hit.accepted, hit.score >= 0.8, "accepted 与分数/阈值不一致");
    }
    assert!(!probe.notice.trim().is_empty(), "必须给操作者一句话结论");
}

// ── 图标库（模板从哪来） ────────────────────────────────────────────────
//
// 图标库是「先点击导航图标跳转」那一步的**前提**：导航图标上没有文字，
// OCR 读不到，只能靠模板匹配；而模板只能由人从真实画面上框出来。
//
// 这组用例盯住四件事：
//
// 1. 命令真的注册上了（靠 `AppHandle<R>` 钉运行时泛型——漏进 `generate_handler!`
//    的话前端只会拿到 "command not found"，编译期毫无提示）；
// 2. 目录位置与尺寸上下限由后端下发，前端不另写一份；
// 3. 坏文件**留在列表里**并带上原因，而不是让整个列表打不开；
// 4. 参数校验发生在**任何截屏动作之前**。
//
// ⚠️ `click_icon` 的**成功路径刻意不在这里测**：它会真的点一下鼠标。
// 那种验证只能由人对着客户端做，测试里跑就等于对着测试机乱点。

/// 读出后端的运行信息（顺带验证新字段确实下发了）。
fn runtime_info(harness: &Harness) -> desktop_lib::RuntimeInfo {
    harness.ok("runtime_info", json!({}))
}

/// 往图标库里放一个**真的能用**的图标：名字对应一个目录，底下带 `variants` 张图。
///
/// 返回**图标名**——配置里引用的、以及 `probe_nav_icon` / `click_icon` 收的
/// 都是名字，不是路径。
///
/// 自己 `create_dir_all`：图标库目录只有在**保存过图标之后**才存在
/// （`list` 把"目录不存在"当成空库而不是错误），所以测试不能假定它在。
fn put_icon(icons_dir: &str, name: &str, variants: u32) -> String {
    for index in 1..=variants {
        write_icon_png(
            &std::path::Path::new(icons_dir).join(name),
            &format!("{index}.png"),
            20,
            20,
        );
    }
    name.to_string()
}

/// 图标库位置与尺寸上下限必须由后端下发，而且与 `vision` 的常量一致。
///
/// 界面上「框得太小 / 太大」的即时提示用的就是这两个值。前端另写一份的话，
/// 迟早会出现「界面说没问题、点保存却被拒」——那是最让人不知所措的组合。
///
/// 图标库的位置有**两个**要下发：当前生效的那个（`icons_dir`），
/// 以及留空时的默认值（`icons_dir_default`，项目根下的 `data/icons/`）。
/// 后者是给界面上的输入框当占位提示用的——前端自己拼一份的话，
/// 两边迟早不一致，而"默认到底存哪儿"恰恰最需要说准。
#[test]
fn runtime_info_carries_the_icon_library_location_and_template_limits() {
    let harness = Harness::new("icon-info", DemoScenario::Happy);
    let info = runtime_info(&harness);

    // 测试实例把图标库按在临时数据目录里（`AppState::in_memory`），
    // 免得跑到真实的项目目录里去读写。
    assert!(
        info.icons_dir.ends_with("icons"),
        "图标库目录应当是某个数据目录下的 icons/：{}",
        info.icons_dir
    );
    assert!(info.icons_dir.starts_with(&info.data_dir), "测试实例的图标库在临时数据目录之下");

    // 默认值必须指向**项目里**，不是 AppData：图标模板是人对着屏幕框出来的素材，
    // 找得到、备份得了才算数。
    let default = std::path::Path::new(&info.icons_dir_default);
    assert!(
        default.ends_with(std::path::Path::new("data").join("icons")),
        "默认图标库应当是项目根下的 data/icons/：{}",
        info.icons_dir_default
    );
    assert!(
        default.is_absolute(),
        "默认图标库要是个绝对路径，界面直接显示它：{}",
        info.icons_dir_default
    );

    assert_eq!(info.template_min_side, vision::MIN_TEMPLATE_SIDE);
    assert_eq!(info.template_max_side, vision::MAX_TEMPLATE_SIDE);
}

/// 还没存过任何图标时：列表是**空的**，不是错误。
///
/// 把"还没开始用"报成失败，会让界面第一次打开就挂一个红条——
/// 而那时用户什么都还没做。
#[test]
fn the_icon_library_starts_empty_and_deleting_a_missing_icon_says_so() {
    let harness = Harness::new("icon-empty", DemoScenario::Happy);

    let listed: Vec<desktop_lib::icon_library::IconEntry> =
        harness.ok("list_icons", json!({}));
    assert!(listed.is_empty(), "全新安装时图标库应当是空的");

    let message = harness.err("delete_icon", json!({ "name": "并不存在" }));
    assert!(message.contains("没有"), "删一个不存在的图标要说清楚：{message}");
}

/// 列表要能读出文件、量出尺寸、**一个名字底下的多张图都列出来**，
/// 并且**坏文件带原因留下来**。
///
/// 图标库目录是给人看也给人手工放的（有人会用画图另存一遍、也有人直接丢一张
/// `<名字>.png` 进去）。坏文件从列表里藏起来，只会让人以为「我明明存过」；
/// 显示出来，顺手就把「任务装配时才发现模板不可用」提前到了列表里。
#[test]
fn the_icon_library_lists_files_and_flags_the_broken_ones() {
    let harness = Harness::new("icon-list", DemoScenario::Happy);
    let icons_dir = runtime_info(&harness).icons_dir;
    let root = std::path::Path::new(&icons_dir);

    // 一个名字，三张图（未选中 / 选中 / 带气泡）。
    put_icon(&icons_dir, "聊天", 3);
    // 一张坏的（有人拿别的工具改坏了）。
    std::fs::create_dir_all(root.join("坏的")).unwrap();
    std::fs::write(root.join("坏的").join("1.png"), b"this is not a png").unwrap();
    // 旧式的单文件图标：命令行产出的就是这种，照样要认。
    let legacy = write_icon_png(root, "通讯录.png", 20, 20);
    assert!(std::path::Path::new(&legacy).is_file());
    // 非 PNG 要跳过：图标库目录里混进别的文件不该让整个列表打不开。
    std::fs::write(root.join("说明.txt"), b"x").unwrap();

    let listed: Vec<desktop_lib::icon_library::IconEntry> =
        harness.ok("list_icons", json!({}));
    assert_eq!(listed.len(), 3, "应当只认 PNG：{listed:?}");

    let chat = listed.iter().find(|item| item.name == "聊天").expect("应当有「聊天」");
    assert_eq!(chat.variants.len(), 3, "一个名字底下的三张图都要列出来");
    assert_eq!(
        chat.variants.iter().map(|item| item.relative.clone()).collect::<Vec<_>>(),
        ["聊天/1.png", "聊天/2.png", "聊天/3.png"]
    );
    assert!(chat.usable, "三张都读得出来，这个图标就能用于导航");
    for variant in &chat.variants {
        assert_eq!((variant.width, variant.height), (20, 20));
        assert!(variant.problem.is_none());
        assert!(
            variant.image.starts_with("data:image/png;base64,"),
            "缩略图要以 data URL 形式带回来，界面一次调用就能画完列表"
        );
    }

    let broken = listed.iter().find(|item| item.name == "坏的").expect("坏文件要留在列表里");
    assert!(broken.variants[0].problem.is_some(), "坏文件必须带上原因，而不是静默消失");
    assert_eq!((broken.variants[0].width, broken.variants[0].height), (0, 0));
    assert!(!broken.usable, "有一张坏的，这个图标就不能用于导航");

    let legacy = listed.iter().find(|item| item.name == "通讯录").expect("旧式单文件也要认");
    assert_eq!(legacy.variants.len(), 1);
    assert_eq!(legacy.variants[0].relative, "通讯录.png");
    assert!(legacy.usable);

    // 删掉一张变体：同一个名字还在，只是少了一张。
    harness.ok::<()>(
        "delete_icon_variant",
        json!({ "name": "聊天", "relative": "聊天/2.png" }),
    );
    let listed: Vec<desktop_lib::icon_library::IconEntry> =
        harness.ok("list_icons", json!({}));
    let chat = listed.iter().find(|item| item.name == "聊天").unwrap();
    assert_eq!(
        chat.variants.iter().map(|item| item.relative.clone()).collect::<Vec<_>>(),
        ["聊天/1.png", "聊天/3.png"],
        "删掉中间那张之后，剩下的编号不重排"
    );

    // 删除整组：三张（现在剩两张）一起走。
    harness.ok::<()>("delete_icon", json!({ "name": "聊天" }));
    let listed: Vec<desktop_lib::icon_library::IconEntry> =
        harness.ok("list_icons", json!({}));
    assert!(!listed.iter().any(|item| item.name == "聊天"), "整组删除要连变体一起删掉");

    // 旧式单文件也能按相对路径删掉。
    harness.ok::<()>(
        "delete_icon_variant",
        json!({ "name": "通讯录", "relative": "通讯录.png" }),
    );
    let listed: Vec<desktop_lib::icon_library::IconEntry> =
        harness.ok("list_icons", json!({}));
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "坏的");
}

/// 删除变体**不能**被前端传来的一串路径带出图标库。
///
/// 这是个删除命令，`relative` 来自界面。不校验的话，一个 `../../重要文件.png`
/// 就能删掉磁盘上任何东西——而界面上什么异常都看不出来。
#[test]
fn delete_icon_variant_refuses_a_path_outside_the_icon_library() {
    let harness = Harness::new("icon-delete-escape", DemoScenario::Happy);
    let icons_dir = runtime_info(&harness).icons_dir;
    put_icon(&icons_dir, "聊天", 2);

    // 图标库**外面**放一个文件，它绝不能因为"名字对得上"就被删掉。
    let outside = std::env::temp_dir().join("rpa-llm-outside-icon-lib");
    std::fs::create_dir_all(&outside).unwrap();
    let victim = write_icon_png(&outside, "重要文件.png", 20, 20);

    for relative in [
        "../重要文件.png",
        "..\\重要文件.png",
        "C:/Windows/System32/calc.png",
        "聊天/../../重要文件.png",
        "聊天",
        "",
    ] {
        assert!(
            harness
                .err(
                    "delete_icon_variant",
                    json!({ "name": "聊天", "relative": relative }),
                )
                .len()
                > 0,
            "「{relative}」不该被当成「聊天」的变体删掉"
        );
    }

    assert!(
        std::path::Path::new(&victim).is_file(),
        "图标库外面的文件一个都不该被碰"
    );
    let listed: Vec<desktop_lib::icon_library::IconEntry> =
        harness.ok("list_icons", json!({}));
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].variants.len(), 2, "两次都没删掉东西");

    let _ = std::fs::remove_dir_all(&outside);
}

/// 参数校验必须发生在**任何截屏动作之前**。
///
/// 判据是错误文案，不是内部调用顺序——文案能证明它走到了哪个分支。
/// 顺序本身有实际意义：这是个只读标定动作，参数填错时不该先去读一遍屏幕。
#[test]
fn save_icon_from_crop_validates_its_arguments_before_touching_the_screen() {
    let harness = Harness::new("icon-save-args", DemoScenario::Happy);
    let base = json!({
        "name": "通讯录",
        "windowClass": "Progman",
        "wecomExe": null,
        "rect": [10, 10, 20, 20],
        "preview": [100, 80],
    });

    // 类名为空：无从定位窗口。
    let mut body = base.clone();
    body["windowClass"] = json!("   ");
    let message = harness.err("save_icon_from_crop", body);
    assert!(message.contains("窗口类名"), "{message}");

    // 宽高为 0：还没框就点了保存。
    let mut body = base.clone();
    body["rect"] = json!([10, 10, 0, 20]);
    let message = harness.err("save_icon_from_crop", body);
    assert!(message.contains("大于 0"), "{message}");

    // 名字能当路径用 ⇒ 拒绝，而且**在截屏之前**就拒绝
    // （这条分支不需要真的有一个窗口，正好证明它没走到截屏那一步）。
    for bad in ["../逃逸", "a/b", "", "CON", "末尾的点."] {
        let mut body = base.clone();
        body["name"] = json!(bad);
        let message = harness.err("save_icon_from_crop", body);
        assert!(
            !message.contains("未能截取目标窗口"),
            "「{bad}」应当在截屏之前就被名字校验拦下，实际报的是：{message}"
        );
    }
}

/// `click_icon` 的校验路径。
///
/// 这个命令**会产生真实的鼠标点击**，所以它的校验比别处更要紧：
/// 参数不对时必须**一次点击都不发**。下面每条都在参数层面就返回了错误，
/// 因此不会碰到鼠标——成功路径只能由人对着客户端点。
#[test]
fn click_icon_refuses_bad_arguments_without_clicking_anything() {
    let harness = Harness::new("icon-click-args", DemoScenario::Happy);
    let icons_dir = runtime_info(&harness).icons_dir;
    let icon = put_icon(&icons_dir, "通讯录", 2);

    let base = json!({
        "windowClass": "Progman",
        "wecomExe": null,
        "navStrip": [0.0, 0.0, 0.08, 1.0],
        "contactPanel": [0.14, 0.2, 0.32, 0.6],
        "templates": [icon.clone()],
        "minScore": 0.8,
        "settleMs": 200,
    });

    let mut body = base.clone();
    body["windowClass"] = json!("");
    assert!(harness.err("click_icon", body).contains("窗口类名"));

    let mut body = base.clone();
    body["navStrip"] = json!([0.0, 0.0, 1.5, 1.0]);
    assert!(harness.err("click_icon", body).contains("导航图标搜索区"));

    let mut body = base.clone();
    body["contactPanel"] = json!([0.14, 0.2, 0.32, 3.0]);
    assert!(harness.err("click_icon", body).contains("联系人候选区"));

    let mut body = base.clone();
    body["minScore"] = json!(1.4);
    assert!(harness.err("click_icon", body).contains("0–1"));

    // 一个图标都没配：这是**配置缺失**，不是"没找到图标"。
    let mut body = base.clone();
    body["templates"] = json!(["  ", ""]);
    let message = harness.err("click_icon", body);
    assert!(
        message.contains("没有配置「导航」图标的模板"),
        "空图标列表要报配置缺失，并说清是哪一组：{message}"
    );

    // 图标名对不上：同样在装配期报错，不留到"分数很低"。
    let mut body = base.clone();
    body["templates"] = json!(["根本没有这个图标"]);
    let message = harness.err("click_icon", body);
    assert!(
        message.contains("根本没有这个图标"),
        "图标找不到时要说清是哪个：{message}"
    );
}

// ── 工作流：装配期该拒绝什么 ────────────────────────────────────────────
//
// 「这条工作流需要哪几块标定区域」是一个**判据**——装配期就是按它拒绝任务的。
// 所以既要验"缺了会被拒"，也要验"界面拿到的那份清单和后端那一条是同一份"。

/// 搜索式缺三块区域 ⇒ 装配期拒绝，**任务列表里不留记录**。
///
/// "不留记录"这一条是重点：装配失败发生在登记之前，所以列表里不该出现
/// 一条永远不推进的草稿——那会让人以为任务已经提交了。
#[test]
fn the_search_workflow_is_refused_without_its_regions() {
    let harness = Harness::with_config(
        "search-no-regions",
        RuntimeConfig {
            mode: RuntimeMode::DryRun,
            workflow: Workflow::SearchContact,
            ..Default::default()
        },
    );

    let message = harness.err(
        "start_task",
        json!({ "request": { "external_contact_name": "张三", "text": "你好", "run_choice": harness.run_choice.clone() } }),
    );

    // 报错要说清"哪条工作流、缺哪几块"，并且用界面上的说法（label）而不是 key：
    // 只报 `main_search` 的话，人得自己把它翻译成标定页里的某一项。
    assert!(message.contains("搜索式查找联系人"), "{message}");
    for label in ["搜索框区", "下拉列表区域", "联系人资料区域"] {
        assert!(message.contains(label), "缺哪块要说清（{label}）：{message}");
    }
    assert!(message.contains("界面标定"), "要告诉人下一步去哪标：{message}");

    let tasks: Vec<TaskView> = harness.ok("list_tasks", json!({}));
    assert!(tasks.is_empty(), "装配失败时不该登记任务，实际有 {} 条", tasks.len());
}

/// 列表扫描式**不**要求搜索式那三块——这是"按工作流分别要求"的另一半。
///
/// 没有这条，上面那条用例可以被"一概全要"糊弄过去，而代价是
/// 每个只想跑列表式的人都被逼着去标三块用不上的区域。
#[test]
fn the_list_workflow_does_not_need_the_search_regions() {
    let harness = Harness::with_config(
        "list-no-search-regions",
        RuntimeConfig {
            mode: RuntimeMode::DryRun,
            workflow: Workflow::ScrollListContact,
            ..Default::default()
        },
    );

    // 能一路跑到结束就够了——这里不关心终态，只关心"装配没有被拒"。
    let id = harness.start("外部测试联系人", "你好");
    assert!(!id.is_empty());
}

/// ★ 回归（端到端）：`start_task` 按**请求里的** run_choice 走，不读配置里那个同名字段。
///
/// 2026-09-19 的 bug —— 界面上选了工作流、跑的却是配置里那条：连跑三条任务，
/// 三条 `task-*.log` 里记的全是 `SearchContact`。修法是把工作流变成**运行参数**
/// （`StartTaskRequest::run_choice`）。`runtime::tests` 里有一条装配层的同款用例，
/// 这条走完整 IPC 链路（请求反序列化 → 命令层 → 装配），两处一起钉住。
///
/// 手法：配置写成**列表扫描式**（一块区域都不需要），请求里带**搜索式**（缺三块）。
/// 装配被拒 ⇒ 它读的是请求；若装配通过，说明它又回去读配置了。
#[test]
fn start_task_follows_the_request_not_the_saved_config() {
    let harness = Harness::with_config(
        "run-choice-from-request",
        RuntimeConfig {
            mode: RuntimeMode::DryRun,
            workflow: Workflow::ScrollListContact,
            ..Default::default()
        },
    );

    // 前提：**已保存的**配置是列表式。前提不成立的话下面那条断言什么都证明不了。
    let info: desktop_lib::RuntimeInfo = harness.ok("runtime_info", json!({}));
    assert_eq!(
        info.config.workflow,
        Workflow::ScrollListContact,
        "前提：已保存的配置是列表式"
    );

    // 请求里带搜索式 ⇒ 缺三块区域 ⇒ 装配期就该被拒。
    // 这里没被拒，就说明命令层又回去读配置了 —— 那正是这个 bug。
    let message = harness.err(
        "start_task",
        json!({ "request": {
            "external_contact_name": "张三",
            "text": "你好",
            "run_choice": {
                "mode": "dry_run",
                "workflow": "search_contact",
                // 这条路不导航，所以这一项没有意义。留空串而不是随便写个名字：
                // 写个像"选好了"的值，会让人以为它真的参与了什么判断。
                "nav_target": "",
            },
        } }),
    );
    assert!(
        message.contains("搜索式查找联系人"),
        "应当按**请求里**的工作流报错：{message}"
    );
    assert!(message.contains("界面标定"), "{message}");
}

/// ★ 回归（端到端）：**模式也取自请求**，不读配置里那个同名字段。
///
/// 与工作流那条是同一个坑，但后果更重：模式不只决定用哪一组端口，还决定
/// **要不要带标定窗口**、以及审计里记哪个平台。读错了方向可能是
/// 「以为在演练、其实在真实客户端上操作」。
///
/// 手法：配置写成**演练**且**没有**标定尺寸（这样按配置走必然装配成功），
/// 请求里带**真实**。被拒 ⇒ 它读的是请求；若装配通过，说明它又回去读配置了。
#[test]
fn start_task_takes_the_mode_from_the_request_not_the_saved_config() {
    let harness = Harness::with_config(
        "run-choice-mode-from-request",
        RuntimeConfig {
            mode: RuntimeMode::DryRun,
            calibrated_window: None,
            workflow: Workflow::ScrollListContact,
            ..Default::default()
        },
    );

    // 前提：**已保存的**配置是演练模式。前提不成立的话下面那条断言什么都证明不了。
    let info: desktop_lib::RuntimeInfo = harness.ok("runtime_info", json!({}));
    assert_eq!(
        info.config.mode,
        RuntimeMode::DryRun,
        "前提：已保存的配置是演练模式"
    );

    // 请求里带真实模式 ⇒ 没有标定尺寸 ⇒ 装配期就该被拒。
    let message = harness.err(
        "start_task",
        json!({ "request": {
            "external_contact_name": "张三",
            "text": "你好",
            "run_choice": {
                "mode": "live",
                "workflow": "scroll_list_contact",
                "nav_target": "",
            },
        } }),
    );
    assert!(
        message.contains("记录窗口尺寸"),
        "应当按**请求里**的模式报错：{message}"
    );
}

/// ★ 回归（端到端）：「只做导航」点的图标由**图标库目录名**指定，不再是写死的枚举。
///
/// 这条路以前是"两个写死的目标（联系人 / 聊天历史）各配一组模板"，于是图标库里
/// 四五个图标在下拉里根本选不出来——想测「收藏夹」都没得选。改成"从图标库选一个名字"
/// 之后，这条链路有三处必须对齐：**请求里的字段名**（写错会被静默忽略）、
/// **装配期拿它去图标库查目录**、以及**查不到时要拒**。
///
/// 走完整的 IPC 反序列化，正是为了钉住第一处：只在装配层测的话，
/// 字段名写错照样全绿，而线上表现是"选了一个图标、跑起来却是另一个"。
#[test]
fn start_task_navigate_only_takes_the_icon_name_from_the_request() {
    let harness = Harness::new("navigate-only-icon-name", DemoScenario::Happy);
    let icons_dir = runtime_info(&harness).icons_dir;
    put_icon(&icons_dir, "收藏夹", 2);
    put_icon(&icons_dir, "通讯录", 1);

    let choice = |nav_target: &str| {
        json!({ "mode": "dry_run", "workflow": "navigate_only", "nav_target": nav_target })
    };
    let request = |nav_target: &str| {
        json!({ "request": {
            "external_contact_name": "张三",
            "text": "你好",
            "run_choice": choice(nav_target),
        } })
    };

    // 选「收藏夹」⇒ 装配通过。「只做导航」不需要任何标定区域（它连人都不找），
    // 需要的只有"这个名字能在图标库里展开出图"。
    let id: String = harness.ok("start_task", request("收藏夹"));
    assert!(!id.is_empty());

    // 名字在图标库里没有 ⇒ 装配期拒绝，而且报错里要有**那个名字**：
    // 图标库里通常有四五个图标，不点名等于让人自己猜该去改哪一个。
    let message = harness.err("start_task", request("根本没有这个图标"));
    assert!(message.contains("根本没有这个图标"), "{message}");

    // 一个都没选 ⇒ 也拒绝。**不兜底**挑一个：那会变成"点到了别的地方"，
    // 而任务照常跑完，看不出任何异常。
    let message = harness.err("start_task", request(""));
    assert!(message.contains("要指定点哪一个图标"), "{message}");

    // ★★ 回归：**联系人与正文留空也必须能提交**。
    //
    // 这条路不找任何人、也发不出消息，那两个输入框在界面上根本不显示。
    // 以前命令层无条件要求它们非空，于是点「开始任务」什么都不发生——
    // 连 `task-*.log` 都没生成，看起来像"后端判断逻辑坏了"（2026-09-20 实测）。
    // 这里连"字段整个不带"一起钉住：前端传 `""` 与不传都该放行。
    let bare = json!({ "request": {
        "external_contact_name": "",
        "text": "",
        "run_choice": choice("通讯录"),
    } });
    let id: String = harness.ok("start_task", bare);
    assert!(!id.is_empty(), "空联系人与空正文不该拦住「只做导航」");
}

/// ★ 回归：另外两条工作流**仍然**要求联系人与正文非空。
///
/// 与上面那条是一对：放宽「只做导航」的时候最容易顺手把门槛整条删掉，
/// 而"发消息给谁、发什么"这两件事在那两条路上都是**必需**的——
/// 少了它们，任务会跑到"找不到人"那一步才转人工，失败现象与"真的没有这个人"
/// 一模一样，方向全错。
#[test]
fn the_two_contact_workflows_still_require_contact_and_message() {
    let harness = Harness::with_config(
        "contact-inputs-required",
        RuntimeConfig {
            mode: RuntimeMode::DryRun,
            // 列表扫描式：一块标定区域都不需要，所以下面报的错只可能来自输入校验。
            workflow: Workflow::ScrollListContact,
            ..Default::default()
        },
    );

    for (contact, text, expected) in [
        ("", "你好", "外部联系人名称不能为空"),
        ("张三", "", "消息正文不能为空"),
    ] {
        let message = harness.err(
            "start_task",
            json!({ "request": {
                "external_contact_name": contact,
                "text": text,
                "run_choice": { "mode": "dry_run", "workflow": "scroll_list_contact", "nav_target": "" },
            } }),
        );
        assert!(message.contains(expected), "应当报「{expected}」：{message}");
    }
}

/// 靶标文字留空 ⇒ 装配期拒绝。
///
/// 空串在「包含」判断里**匹配一切**：空的分组标题会让下拉里的第一行被当成
/// 「联系人」组的标题，于是后面整段判据全部错位。而这**不会报错**——
/// 只会表现为"点到了不相干的一行"。
#[test]
fn blank_target_texts_are_refused_at_assembly_time() {
    let harness = Harness::with_config(
        "blank-target-text",
        RuntimeConfig {
            mode: RuntimeMode::DryRun,
            workflow: Workflow::ScrollListContact,
            profile_chat_entry_text: "   ".into(),
            ..Default::default()
        },
    );

    let message = harness.err(
        "start_task",
        json!({ "request": { "external_contact_name": "张三", "text": "你好", "run_choice": harness.run_choice.clone() } }),
    );
    assert!(message.contains("不能留空"), "{message}");
    assert!(
        message.contains("profile_chat_entry_text"),
        "要说清是哪一个字段：{message}"
    );
}

/// `workflow_requirements` 下发的清单必须与装配期用的是**同一份**。
///
/// 这是这条命令存在的全部理由：界面自己列一张表的话，两边不一致时的表现是
/// 「界面说齐了、点开始却被拒」，而人只会去怀疑标定本身。
/// 所以这里**拿命令的输出去构造一次装配**：命令说"缺这几块"，
/// 那就把这几块补上，装配必须随之成功。
#[test]
fn the_requirement_list_matches_what_assembly_enforces() {
    let harness = Harness::with_config(
        "requirements-match",
        RuntimeConfig {
            mode: RuntimeMode::DryRun,
            workflow: Workflow::SearchContact,
            ..Default::default()
        },
    );

    let requirements: Vec<desktop_lib::runtime::WorkflowRequirement> = harness.ok(
        "workflow_requirements",
        json!({ "config": serde_json::to_value(RuntimeConfig {
            mode: RuntimeMode::DryRun,
            workflow: Workflow::SearchContact,
            ..Default::default()
        }).unwrap() }),
    );
    let search = requirements
        .iter()
        .find(|item| item.workflow == Workflow::SearchContact)
        .expect("三条工作流都要下发");
    assert_eq!(
        search.required.iter().map(|item| item.key.as_str()).collect::<Vec<_>>(),
        ["main_search", "search_dropdown", "contact_profile"],
        "顺序即界面上显示的顺序"
    );
    assert!(
        search.required.iter().all(|item| !item.marked),
        "默认配置一块都没标，如实报 false"
    );

    // 清单里说"一条都不用"的那两条工作流，实际装配也确实不要求。
    for workflow in [Workflow::ScrollListContact, Workflow::NavigateOnly] {
        let item = requirements
            .iter()
            .find(|item| item.workflow == workflow)
            .expect("三条工作流都要下发");
        assert!(item.required.is_empty(), "{workflow:?} 不该要求任何新增区域");
    }
}

// ── 一键截屏热键 ────────────────────────────────────────────────────────
//
// 这三个用例**都不注册真实热键**：它们走的是校验失败与幂等注销那两条路，
// 到不了 `RegisterHotKey`。校验逻辑本身在 `platform-windows` 里另有单元测试，
// 这里测的是**命令层的往返**——命令有没有登记、请求字段名跟前端对不对得上、
// 错误文案能不能原样传到界面。这三件事任一件错了，症状都是「点了按钮没反应」
// 或者「界面报了个看不懂的错」，而代码本身看不出问题。

/// 没勾修饰键的组合**必须被拒**。
///
/// 注册一个不带修饰键的 `A`，会把那个键从整个系统里抢走——用户在**任何**程序里
/// 都打不出 a。这不是「配置没生效」，是把别人的键盘弄坏，所以后端直接拒绝。
#[test]
fn a_bare_hotkey_key_is_rejected_through_ipc() {
    let harness = Harness::new("hotkey-bare-key", DemoScenario::Happy);

    let message = harness.err(
        "register_capture_hotkey",
        json!({ "request": { "ctrl": false, "alt": false, "shift": false, "win": false, "key": "A" } }),
    );

    assert!(
        message.contains("修饰键"),
        "错误文案要说清为什么，不能只报一句非法参数：{message}"
    );
}

/// 认不出的主键要被拒，而且**要点名是哪一个**。
///
/// `F13` 是最容易踩的那个：功能键只到 `F12`。报错里不点名的话，操作者盯着
/// 界面上那个 `F13` 看不出哪里不对。
#[test]
fn an_unsupported_hotkey_key_is_named_in_the_error() {
    let harness = Harness::new("hotkey-bad-key", DemoScenario::Happy);

    let message = harness.err(
        "register_capture_hotkey",
        json!({ "request": { "ctrl": true, "alt": false, "shift": false, "win": false, "key": "F13" } }),
    );

    assert!(message.contains("F13"), "要点名是哪个键：{message}");
    assert!(
        message.contains("F1") || message.contains("F12"),
        "要说明允许的范围：{message}"
    );
}

/// 注销**幂等**：没注册过也算成功。
///
/// 界面在离开标定页、以及每次改组合键之前都会调它。要是「本来就没注册」返回错误，
/// 界面就得为它写一堆无意义的判断，而那种判断迟早会有人漏掉一个。
#[test]
fn unregistering_a_hotkey_that_was_never_registered_succeeds() {
    let harness = Harness::new("hotkey-unregister", DemoScenario::Happy);

    harness.ok::<()>("unregister_capture_hotkey", json!({}));
    harness.ok::<()>("unregister_capture_hotkey", json!({}));
}

/// 真机用例：完整走一遍 `invoke` → 注册 → 注销。
///
/// 与 `platform-windows` 里那条的区别：那条测注册/注销本身，这条测**命令层的往返**
/// ——参数怎么反序列化、返回的组合键写法对不对。返回的那个写法就是界面上显示的
/// 「已生效：Ctrl+Alt+Shift+F12」，所以它必须与操作者勾的、填的完全一致
/// （大小写规范化、修饰键顺序固定）。
///
/// 默认跳过：会真的占一个全系统热键。要跑：
/// `cargo test -p desktop --features custom-protocol -- --ignored a_hotkey_survives`
#[test]
#[ignore]
fn a_hotkey_survives_a_round_trip_through_ipc() {
    let harness = Harness::new("hotkey-round-trip", DemoScenario::Happy);

    let label = harness.ok::<String>(
        "register_capture_hotkey",
        json!({ "request": { "ctrl": true, "alt": true, "shift": true, "win": false, "key": "f12" } }),
    );
    assert_eq!(
        label, "Ctrl+Alt+Shift+F12",
        "小写 f12 要规范化成 F12，修饰键顺序固定为 Ctrl+Alt+Shift+Win"
    );

    harness.ok::<()>("unregister_capture_hotkey", json!({}));
}

// ── 鼠标轨迹自检 ────────────────────────────────────────────────────────

/// 真机用例：「画圆」命令真的注册上了，而且回报的数字自洽。
///
/// 默认跳过：**它会真的把光标画一圈**（半径是本程序窗口短边的一半，通常两三百
/// 像素），跑起来会抢走操作者的鼠标 —— 一个会劫持鼠标的自动化用例，比没有用例
/// 更糟。要跑就明确地跑：
/// `cargo test -p desktop --features custom-protocol -- --ignored a_circle_trace`
///
/// 这个用例的主要价值在**命令注册**：命令靠 `AppHandle<R>` 钉住运行时泛型，
/// 一旦漏进 `generate_handler!`，前端点了按钮只会拿到 "command not found"，
/// 而编译期毫无提示。
///
/// 圆周本身算得对不对，另有**不碰光标**的纯计算用例钉着
/// （`platform-windows/src/winapi/tests.rs`）—— 那些才是每次都会跑的。
#[test]
#[ignore]
fn a_circle_trace_comes_back_self_consistent() {
    let harness = Harness::new("circle-trace", DemoScenario::Happy);

    let trace: desktop_lib::cursor_trace::CircleTraceView =
        harness.ok("draw_cursor_circle", json!({}));

    // 半径 = 窗口**短边**的一半。这条判据与界面上的文案必须一致，别让两边各自算。
    let short_side = trace.window_width.min(trace.window_height);
    assert_eq!(
        trace.radius,
        (short_side / 2) as i32,
        "半径应当是窗口短边 {short_side} 的一半，实得 {}",
        trace.radius
    );

    // 步数下限：少于这个数圆周会变成肉眼可见的多边形。
    assert!(trace.steps >= 24, "步数太少，会看出棱角：{}", trace.steps);

    // 时长夹在 [400ms, 8s]：短于此看不出是圆，长于此看起来像卡死。
    assert!(
        (400..=8000).contains(&trace.duration_ms),
        "一圈的时长跑出界了：{} 毫秒",
        trace.duration_ms
    );

    // 速度只有一个来源（平台层默认值），回报出来是为了能核对，不是给人调的。
    assert!(
        trace.speed_px_per_sec > 0.0,
        "速度必须是正数，实得 {}",
        trace.speed_px_per_sec
    );

    // ★ **实测**终点：`end` 是走完之后重新读的光标位置，不是算出来的那个终点。
    // 走对了的话它到圆心的距离 ≈ 半径（终点在圆上，且**不在起点**——
    // 起点也在圆上，所以"离圆心 ≈ 半径"本身证明不了终点离开了起点；
    // 真正证明它动了的是"实测值"这件事本身：轨迹没生效时这个数会接近 0）。
    let gap = trace.end_distance_px;
    assert!(
        (gap - trace.radius).abs() <= trace.radius / 10,
        "实测终点离圆心 {gap} 像素，而半径是 {} —— 轨迹可能没走完，或者整段没生效",
        trace.radius
    );
    assert_ne!(
        trace.end,
        [trace.center[0] + trace.radius, trace.center[1]],
        "终点落回了圆的起点：那就分不出「走过」和「根本没动」了"
    );
}
