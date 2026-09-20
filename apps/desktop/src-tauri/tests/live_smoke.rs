//! 真实模式（Live）实机验证。
//!
//! 在没有安装企业微信的机器上，用「记事本」当替代靶标，
//! 把整条真实链路跑一遍：
//!
//! ```text
//! 真实窗口定位 → 真实 GDI 截屏 → 真实本地 OCR 进程 → 真实逐字精确匹配
//!   → 真实受保护点击 → 真实剪贴板粘贴 → 真实回车 → 真实送达核验
//! ```
//!
//! 这是 `#[ignore]` 的手动用例：**它会在真实桌面上产生真实的鼠标与键盘输入**，
//! 并且会启动、随后关闭一个记事本进程，所以默认不参与 `cargo test`。
//!
//! ```text
//! cargo build -p winocr
//! cargo test -p desktop --test live_smoke -- --ignored --nocapture
//! ```
//!
//! ## 为什么用记事本
//!
//! 在记事本里按回车只是插入一个换行，**不会真的把消息发出去**，
//! 因此整条链路（含"发送"与"送达核验"）可以在**零副作用**的前提下验证。
//! 换成 WorkBuddy 之类的窗口就不行了：那里的回车会把消息真的发出去。
//!
//! ## 这一步到底验证了什么、没验证什么
//!
//! 已验证：平台层与视觉层的**真实实现**（`platform-windows` + `winocr` +
//! `StrictContactMatcher`）能串起来跑完整条工作流。
//!
//! 未验证：企业微信本身的界面标定（四个区域的相对坐标是按真机界面调的），
//! 以及界面上的人工确认交互——后者由 `tests/ipc_flow.rs` 覆盖。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use automation_core::{
    AuditSink, AutomationError, CancelToken, DesktopPlatform, HumanConfirmation, LocalOcr,
    ProgressSink, Rect, RelativeRegion, SendLedger, SendTask, StateChange, TaskState,
};
use desktop_lib::runtime::{RegionConfig, RunChoice, RuntimeConfig, RuntimeMode, WindowGeometry};
use platform_windows::{WindowsDesktop, WindowsDesktopConfig};
use storage::{SqliteAuditStore, SqliteSendLedger};
use uuid::Uuid;
use vision::ExternalOcr;

/// 用例把"要跑哪条路"写在配置上，装配时照抄成运行参数。
///
/// 生产路径上这一步由 `start_task` 从任务请求里取（见 `RunChoice` 的文档）。
/// 装配函数**不再自己去读配置**——配置里那几个字段只是界面上的初始默认值，
/// 照抄一份到运行参数里是为了让这些用例保持"改一个地方就换一条路"的写法。
fn run_choice_from(config: &RuntimeConfig) -> RunChoice {
    RunChoice {
        mode: config.mode,
        workflow: config.workflow,
        nav_target: config.nav_target.clone(),
    }
}

/// 替代靶标：记事本的窗口类名。
const TARGET_CLASS: &str = "Notepad";
/// 目标联系人。会被写进记事本正文，等着被 OCR 读出来。
const CONTACT: &str = "张三";
/// 要"发送"的消息。
///
/// 刻意保持较短：送达核验要求**某一个文字框**的文本包含整条消息，
/// 消息一旦在界面上折行，单行文字框就装不下它了。
const MESSAGE: &str = "rpa_llm 真实模式联调：本消息由本地截屏 + 本地 OCR + 受保护输入产生。";

/// 替代靶标没有"联系人列表 / 聊天标题 / 聊天正文"这些分栏，
/// 因此把四个区域都指向记事本的正文区。
///
/// 这里要验证的是「能否唯一识别并核验姓名、能否确认消息出现」，
/// 而不是界面分栏本身——分栏是留给真机标定的。
///
/// 取值是按记事本实测调的：
/// - 上边界 0.12：记事本的标题栏 + 菜单/标签栏约占窗口高度的 12%，
///   正文第一行紧贴其下。取 0.20 会把第一行（也就是姓名）切在外面。
/// - 下边界 0.80：再往下就压到任务栏了，会把任务栏上的时间一起识别进来。
const TEXT_AREA: [f32; 4] = [0.02, 0.12, 0.96, 0.68];

/// 自动放行的确认端口。
///
/// 真实界面里这一步是 `UiConfirmation`，会阻塞等操作者点确认。
/// 本用例只验证**平台层与视觉层的真实链路**，人工确认环节已由
/// `tests/ipc_flow.rs` 的界面用例覆盖，因此这里替身放行。
struct AutoApprove;

impl HumanConfirmation for AutoApprove {
    fn confirm_send(&self, _task: &SendTask, _expires_in: Duration) -> Result<(), AutomationError> {
        Ok(())
    }
}

/// 把状态迁移打到标准输出，便于人工核对每一步是否按预期推进。
struct PrintProgress;

impl ProgressSink for PrintProgress {
    fn state_changed(&self, change: &StateChange) {
        let detail = change.detail.as_deref().unwrap_or("");
        let failure = change
            .failure_code
            .as_deref()
            .map(|code| format!("  ← 失败代码 {code}"))
            .unwrap_or_default();
        println!("  [状态] {:?} → {:?}   {detail}{failure}", change.from, change.to);
    }
}

/// 用例无论正常结束还是 panic，都要收掉记事本。
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// 图标库目录。
///
/// 这些用例**不开**「先点击导航图标跳转」，用不到图标库；但装配函数要求传它。
/// 按生产代码的默认位置给（项目根下的 `data/icons/`）——写死一个假路径的话，
/// 哪天顺手把开关打开，报错信息会指向一个根本不存在的地方。
fn icons_dir() -> PathBuf {
    desktop_lib::icon_library::default_dir().expect("这个 crate 就在 workspace 里")
}

fn winocr_path() -> PathBuf {
    workspace_root().join("target").join("debug").join("winocr.exe")
}

fn system_exe(name: &str) -> PathBuf {
    PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()))
        .join("System32")
        .join(name)
}

/// 写一个带 BOM 的 UTF-8 临时文件，确保记事本一定按 UTF-8 打开（否则中文会乱码）。
fn write_scratch_file() -> PathBuf {
    let path = std::env::temp_dir().join("rpa_live_scratch.txt");
    let mut content = String::from("\u{FEFF}");
    // 首行单独留给 BOM：免得它黏在姓名上影响 OCR。
    content.push_str("\r\n");
    content.push_str(CONTACT);
    content.push_str("\r\n");
    // 大量空行，让"输入框"（composer 区域的中心）落在空白处，
    // 粘贴进去的消息就能独占一行。
    for _ in 0..40 {
        content.push_str("\r\n");
    }
    std::fs::write(&path, content).expect("写入临时文件失败");
    path
}

fn live_config(ocr: &Path) -> RuntimeConfig {
    RuntimeConfig {
        mode: RuntimeMode::Live,
        window_class: TARGET_CLASS.to_string(),
        // 客户端由操作者手动启动，任务不再自己拉起程序（见 `runner::execute`）。
        // 这里刻意留空：`wecom_exe` 现在的用途是**校验窗口归属**——
        // 一旦填了它，定位就会要求窗口必须属于该程序，而本用例的靶标是记事本。
        wecom_exe: None,
        ocr_command: Some(ocr.to_string_lossy().into_owned()),
        min_confidence: 0.85,
        confirmation_ttl_secs: 30,
        regions: RegionConfig {
            contact_panel: TEXT_AREA,
            chat_header: TEXT_AREA,
            chat_body: TEXT_AREA,
            composer: TEXT_AREA,
        },
        ..RuntimeConfig::default()
    }
}

/// 把当前窗口尺寸填进配置的「标定尺寸」。
///
/// 真实模式要求这一项必须有值（见 `runtime::build_runner`）：任务只在标定过的
/// 尺寸下工作。真机用例要验的是定位/OCR/输入这条链路，不是尺寸校验本身，
/// 所以就地量一次当前窗口当作标定值。
fn with_measured_geometry(mut config: RuntimeConfig) -> RuntimeConfig {
    if config.calibrated_window.is_none() {
        match probe_desktop().measure() {
            Ok((rect, metrics)) => {
                config.calibrated_window = Some(WindowGeometry {
                    x: rect.x,
                    y: rect.y,
                    width: rect.width,
                    height: rect.height,
                    scale_factor: metrics.scale_factor,
                });
            }
            Err(err) => println!("（量取窗口尺寸失败：{err}；标定尺寸仍为空，装配会被拒绝）"),
        }
    }
    config
}

fn probe_desktop() -> WindowsDesktop {
    WindowsDesktop::new(WindowsDesktopConfig {
        window_matcher: platform_windows::WindowMatcher::ClassName(TARGET_CLASS.to_string()),
        ..WindowsDesktopConfig::default()
    })
}

/// 等目标窗口出现（同时会把它带到前台）。
fn wait_for_window(deadline: Duration) -> Rect {
    let desktop = probe_desktop();
    let start = Instant::now();
    loop {
        if let Ok(rect) = desktop.focus_wecom() {
            return rect;
        }
        assert!(start.elapsed() < deadline, "等待目标窗口超时：{TARGET_CLASS}");
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// 截取目标区域并做一次**真实**的本地 OCR。
///
/// 真实模式一旦失败，第一件要知道的事就是「OCR 到底看到了什么」，
/// 所以把识别结果打出来，比对着截图猜快得多。
fn recognize_target_area(ocr: &ExternalOcr, verbose: bool) -> Vec<automation_core::TextBox> {
    let desktop = probe_desktop();
    let rect = desktop.focus_wecom().expect("定位目标窗口失败");
    let region =
        RelativeRegion::new(TEXT_AREA[0], TEXT_AREA[1], TEXT_AREA[2], TEXT_AREA[3]).resolve(rect);
    let shot = desktop.capture(region).expect("截屏失败");
    let boxes = ocr.recognize(&shot).expect("本地 OCR 调用失败");

    if verbose {
        println!(
            "  区域 {}x{} @({},{})，截图指纹 {}，识别到 {} 个文字框",
            region.width,
            region.height,
            region.x,
            region.y,
            &shot.fingerprint[..16.min(shot.fingerprint.len())],
            boxes.len()
        );
        for b in &boxes {
            println!(
                "      ({:4},{:4}) {:3}x{:<3} 置信 {:.2}  「{}」",
                b.bounds.x, b.bounds.y, b.bounds.width, b.bounds.height, b.confidence, b.text
            );
        }
    }
    boxes
}

/// 轮询到目标区域里出现指定文字为止。
///
/// 窗口**一存在**就能被定位，但内容往往还没渲染完；这时候截屏会拿到一片空白。
/// 真实模式里这属于「界面还没准备好」，所以这里等它，而不是立刻判失败。
fn wait_for_text(ocr: &ExternalOcr, expected: &str, label: &str) -> Vec<automation_core::TextBox> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let boxes = recognize_target_area(ocr, false);
        if boxes.iter().any(|b| b.text == expected) {
            println!("  {label}：已识别到「{expected}」");
            return boxes;
        }
        if Instant::now() >= deadline {
            println!("  {label}：等待「{expected}」超时，最后一次识别结果如下");
            recognize_target_area(ocr, true);
            panic!("目标区域里始终没有识别到「{expected}」");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[test]
#[ignore = "会在真实桌面上产生真实输入，需要人工准备目标窗口"]
fn live_run_against_a_stand_in_window() {
    let ocr_path = winocr_path();
    assert!(
        ocr_path.is_file(),
        "缺少本地 OCR 工具 {}，请先执行：cargo build -p winocr",
        ocr_path.display()
    );

    let scratch = write_scratch_file();
    println!("临时文件：{}", scratch.display());

    let notepad = std::process::Command::new(system_exe("notepad.exe"))
        .arg(&scratch)
        .spawn()
        .expect("启动记事本失败");
    let _guard = ChildGuard(notepad);

    let config = with_measured_geometry(live_config(&ocr_path));
    let rect = wait_for_window(Duration::from_secs(20));
    println!(
        "已定位目标窗口：{}x{} @({},{})\n",
        rect.width, rect.height, rect.x, rect.y
    );

    // ── 预检：先确认 OCR 真的能在目标区域读到联系人 ──────────────────
    let ocr = ExternalOcr::new(&ocr_path).with_timeout(Duration::from_secs(10));
    println!("预检（发送前）：");
    wait_for_text(&ocr, CONTACT, "识别联系人");

    // ── 装配并执行真实工作流 ────────────────────────────────────────
    let task = SendTask {
        id: Uuid::new_v4(),
        external_contact_name: CONTACT.to_string(),
        text: MESSAGE.to_string(),
        created_by: "live-smoke".to_string(),
    };

    let audit = Arc::new(SqliteAuditStore::in_memory().expect("创建审计库失败"));
    let ledger = Arc::new(SqliteSendLedger::in_memory().expect("创建发送台账失败"));

    let runner = desktop_lib::runtime::build_runner(
        &config,
        &run_choice_from(&config),
        &task,
        &icons_dir(),
        audit.clone() as Arc<dyn AuditSink>,
        ledger.clone() as Arc<dyn SendLedger>,
        Arc::new(AutoApprove),
    )
    .expect("装配真实模式运行器失败");

    println!("\n开始执行真实模式工作流：");
    let outcome = runner.run(&task, &PrintProgress, &CancelToken::new());

    println!("\n终态：{:?}", outcome.state);
    if let Some(failure) = &outcome.failure {
        println!("失败代码：{}", failure.code);
        println!("失败原因：{}", failure.reason);
    }
    println!("证据引用：{:?}", outcome.evidence);

    // 无论成败都把现场打出来，方便定位。
    println!("\n复核（发送后）：");
    recognize_target_area(&ocr, true);

    assert_eq!(
        outcome.state,
        TaskState::Completed,
        "真实模式没有走通，失败信息：{:?}",
        outcome.failure
    );

    // 送达核验是核心：消息必须真的出现在目标区域里。
    assert!(
        !outcome.evidence.is_empty(),
        "应当留下截图指纹作为证据引用"
    );
}

/// 负面用例：目标区域里没有这个联系人时，必须**拒绝发送**并留下可读原因。
///
/// 对应验收条件 5（"识别失败时不会发送，并会留下可读失败原因"）。
#[test]
#[ignore = "会在真实桌面上产生真实输入，需要人工准备目标窗口"]
fn live_run_refuses_to_send_when_the_contact_is_absent() {
    let ocr_path = winocr_path();
    assert!(ocr_path.is_file(), "请先执行：cargo build -p winocr");

    let scratch = write_scratch_file();
    let notepad = std::process::Command::new(system_exe("notepad.exe"))
        .arg(&scratch)
        .spawn()
        .expect("启动记事本失败");
    let _guard = ChildGuard(notepad);

    let config = live_config(&ocr_path);
    wait_for_window(Duration::from_secs(20));

    let task = SendTask {
        id: Uuid::new_v4(),
        // 这个名字在记事本里不存在，必须被拒绝。
        external_contact_name: "李四".to_string(),
        text: MESSAGE.to_string(),
        created_by: "live-smoke".to_string(),
    };

    let audit = Arc::new(SqliteAuditStore::in_memory().expect("创建审计库失败"));
    let ledger = Arc::new(SqliteSendLedger::in_memory().expect("创建发送台账失败"));

    let runner = desktop_lib::runtime::build_runner(
        &config,
        &run_choice_from(&config),
        &task,
        &icons_dir(),
        audit.clone() as Arc<dyn AuditSink>,
        ledger.clone() as Arc<dyn SendLedger>,
        Arc::new(AutoApprove),
    )
    .expect("装配真实模式运行器失败");

    let task_id = task.id;
    println!("\n开始执行（期望被拒绝）：");
    let outcome = runner.run(&task, &PrintProgress, &CancelToken::new());

    println!("\n终态：{:?}", outcome.state);
    if let Some(failure) = &outcome.failure {
        println!("失败代码：{}", failure.code);
        println!("失败原因：{}", failure.reason);
    }

    assert_ne!(outcome.state, TaskState::Completed, "找不到联系人时绝不能判定成功");
    assert_eq!(outcome.state, TaskState::NeedsHumanReview);
    let failure = outcome.failure.expect("必须留下失败原因");
    assert!(
        !failure.reason.trim().is_empty(),
        "失败原因必须可读，不能为空"
    );
    // 一个字都不该被粘进去：任务不该进入发送台账。
    assert!(
        !ledger.is_claimed(task_id),
        "拒绝发送时不应在发送台账里留下已占用记录"
    );
}

// ── 鼠标滚轮 ────────────────────────────────────────────────────────────

/// 实机验证：滚轮事件真的送到了目标窗口，并且内容真的动了。
///
/// 这是「滚动查找联系人」里唯一无法用替身验证的一环——替身只会记录
/// "调用过 scroll"，证明不了 Win32 那一层真的把内容滚了。
/// 用记事本当靶标：滚一下文本区，画面指纹必须变化。
///
/// ```text
/// cargo test -p desktop --test live_smoke -- --ignored --nocapture live_scroll
/// ```
#[test]
#[ignore = "会在真实桌面上产生真实输入，需要人工准备目标窗口"]
fn live_scroll_actually_moves_a_real_window() {
    // 内容要足够长，否则一屏放得下，怎么滚都不动。
    let path = std::env::temp_dir().join("rpa_live_scroll.txt");
    let mut content = String::from("\u{FEFF}");
    for index in 0..200 {
        content.push_str(&format!("第 {index} 行：滚动验证用的填充内容\r\n"));
    }
    std::fs::write(&path, content).expect("写入临时文件失败");

    let notepad = std::process::Command::new(system_exe("notepad.exe"))
        .arg(&path)
        .spawn()
        .expect("启动记事本失败");
    let _guard = ChildGuard(notepad);

    let rect = wait_for_window(Duration::from_secs(20));
    println!("已定位目标窗口：{}x{} @({},{})", rect.width, rect.height, rect.x, rect.y);

    let desktop = probe_desktop();
    // `capture` / `scroll` 都要求"已经定位过窗口"（它们共用同一套前台守卫），
    // 所以这里必须先在这**同一个实例**上做一次定位。
    let located = desktop.focus_wecom().expect("定位目标窗口失败");
    assert_eq!(located, rect, "两次定位拿到的窗口边界应当一致");

    let area = RelativeRegion::new(TEXT_AREA[0], TEXT_AREA[1], TEXT_AREA[2], TEXT_AREA[3])
        .resolve(rect);
    let center = area.center();

    // 窗口刚出现时内容可能还没画完，先等它稳定下来：
    // 连续两次截屏指纹一致，才认为可以开始比对了。
    let before = wait_for_stable_frame(&desktop, area);
    println!("滚动前指纹：{}", &before.fingerprint[..16]);

    desktop
        .scroll(center, 5, rect)
        .expect("滚动失败（窗口不在前台？）");

    // 滚动是异步生效的，给它一点时间再截。
    std::thread::sleep(Duration::from_millis(400));
    let after = desktop.capture(area).expect("滚动后截屏失败");
    println!("滚动后指纹：{}", &after.fingerprint[..16]);

    assert_ne!(
        before.fingerprint, after.fingerprint,
        "滚动 5 格后画面没有任何变化：滚轮事件没有真正到达目标窗口"
    );

    // 再往回滚，应当又变一次。这一步能抓出"方向搞反了"这类错误：
    // 如果正负号写错，向下滚会变成向上滚，而向上滚在顶端是**没有效果**的，
    // 于是这次就不会有变化。
    desktop
        .scroll(center, -5, rect)
        .expect("反向滚动失败（窗口不在前台？）");
    std::thread::sleep(Duration::from_millis(400));
    let back = desktop.capture(area).expect("反向滚动后截屏失败");
    println!("回滚后指纹：{}", &back.fingerprint[..16]);

    assert_ne!(
        back.fingerprint, after.fingerprint,
        "往回滚之后画面没有变化：滚动方向可能反了（顶端再往上滚是没效果的）"
    );

    println!("\n滚动验证通过：内容确实随滚轮移动了。");
}

/// 反复截屏直到连续两帧指纹一致，返回稳定下来的那一帧。
///
/// 窗口刚出现时内容还在渲染，直接拿第一帧去比对会把"还没画完"误判成
/// "滚动没生效"。
fn wait_for_stable_frame(
    desktop: &WindowsDesktop,
    area: Rect,
) -> automation_core::Screenshot {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut previous = desktop.capture(area).expect("截屏失败");
    loop {
        std::thread::sleep(Duration::from_millis(300));
        let current = desktop.capture(area).expect("截屏失败");
        if current.fingerprint == previous.fingerprint {
            return current;
        }
        assert!(
            Instant::now() < deadline,
            "目标区域画面一直不稳定，无法进行滚动比对"
        );
        previous = current;
    }
}
