//! 真实模式（Live）实机验证：以**微信**为靶标，
//! 验证「滚动下拉找到指定人名 → 在输入框填入文字 → 不发送」这条链路。
//!
//! 这是 `#[ignore]` 的手动用例：**它会在真实桌面上产生真实的鼠标与键盘输入**。
//! 它**绝不发送**——配置里强制 `stop_before_send = true`，
//! 并且确认端口写成"一旦被调用就 panic"，任何"其实走到了发送"的回归都会当场炸出来。
//!
//! ```text
//! cargo build -p winocr
//! # 先手写标定文件（见下），再：
//! RPA_LIVE_CONTACT=某个联系人 cargo test -p desktop --test live_wechat -- --ignored --nocapture
//! ```
//!
//! ## 前置条件：目标窗口必须**已经在前台**
//!
//! `SetForegroundWindow` 受 Windows 前台锁定策略限制：调用方进程自己不在前台时，
//! 系统会直接拒绝置前。这是**宿主限制，不是产品缺陷**——真实使用时操作者点界面，
//! 程序本来就在前台。但在自动化宿主里（测试进程是 shell 的子进程），
//! 置前会失败并报 `ClientNotReady`。
//!
//! 所以跑之前请**手动点一下微信窗口**，让它保持在前台。
//! 窗口已经是前台时，`focus_wecom` 会跳过置前那一步，直接通过。
//!
//! ## 标定文件
//!
//! 四个区域（联系人区 / 聊天标题 / 聊天正文 / 输入框）是**按真机界面调的**，
//! 换机器、换微信版本、改缩放都会变。文件默认放在
//! `target/live-wechat.json`（`target/` 不入库），可用 `RPA_LIVE_CONFIG` 覆盖。
//!
//! 因为 `RuntimeConfig` 带 `#[serde(default)]`，只写关心的几项就行。
//! **四个区域的比例必须按真机界面实测重标**（下面的数值只是占位示例）。
//!
//! `window_class` 则**不必再猜**——本机微信 4.x 已实测（2026-09-17）：
//!
//! | 项 | 值 |
//! |---|---|
//! | 窗口类名 | `Qt51514QWindowIcon` |
//! | 窗口标题 | `casper`（微信 4.x 的内部代号，**不是**「微信」） |
//! | 所属 exe | `C:\Program Files\Tencent\Weixin\Weixin.exe` |
//!
//! **注意标题不可信、exe 路径才可信**：`screen_probe list` 里那个标题为
//! `casper` 的窗口就是微信主窗口，别因为它不叫「微信」就以为窗口没出现。
//! 换机器/换微信版本请重新用 `screen_probe pick` 实测（把鼠标停在微信窗口上即可，
//! 它会连 exe 路径一起读出来）。
//!
//! ```json
//! {
//!   "window_class": "Qt51514QWindowIcon",
//!   "regions": {
//!     "contact_panel": [0.14, 0.12, 0.28, 0.88],
//!     "chat_header":   [0.28, 0.00, 0.72, 0.10],
//!     "chat_body":     [0.28, 0.10, 0.72, 0.72],
//!     "composer":      [0.28, 0.82, 0.72, 0.18]
//!   }
//! }
//! ```
//!
//! ⚠️ `contact_panel` 的**左边界不能取 0**：会话列表左侧的导航图标栏与头像列
//! （含未读红点）会被 OCR 按行并进姓名里，把「丁俊」读成「0 丁俊」——
//! 而姓名是逐字精确匹配，多一个字就永远找不到人。实测数据见
//! `automation_core::DEFAULT_CONTACT_PANEL`。
//!
//!
//! 标定流程：把窗口点到前台 → `screen_probe annotate casper out.png` 截图并画上
//! 四个区域与 10% 网格 → 照着图量出四个区域相对窗口的比例（网格让这件事可以用
//! 百分比描述）→ 也可以用界面上的「指认窗口」+「截图并标注」按钮做同一件事。
//!
//! ## 这一步验证了什么、没验证什么
//!
//! 已验证（跑通后）：平台层与视觉层的真实实现能串起来跑完整条工作流，
//! 包括**真实的滚轮滚动**（这是 `platform-windows` 里最新、也最没跑过的一段）。
//!
//! 未验证：界面上的**人工确认**交互（由 `tests/ipc_flow.rs` 覆盖），
//! 以及真实发送与送达核验（本用例刻意不发送）。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use automation_core::{
    AuditSink, AutomationError, CancelToken, DesktopPlatform, HumanConfirmation, LocalOcr,
    ProgressSink, Rect, RelativeRegion, SendLedger, SendTask, StateChange, TaskState, TextBox,
};
use desktop_lib::runtime::{RuntimeConfig, RuntimeMode, WindowGeometry};
use platform_windows::{WindowsDesktop, WindowsDesktopConfig};
use storage::{SqliteAuditStore, SqliteSendLedger};
use uuid::Uuid;
use vision::ExternalOcr;

/// 默认联系人：微信自带的「文件传输助手」——不用真的打扰任何人。
const DEFAULT_CONTACT: &str = "文件传输助手";

/// 确认端口：**一旦被调用就 panic**。
///
/// 本用例配置了「只填不发」，工作流必须在人工确认**之前**结束。
/// 如果它走到了这里，说明"只填不发"分支失效了——这是缺陷，不是可以放行的噪声，
/// 所以这里不放行、直接炸。
struct NeverReached;

impl HumanConfirmation for NeverReached {
    fn confirm_send(&self, _task: &SendTask, _expires_in: Duration) -> Result<(), AutomationError> {
        panic!("配置了「只填不发」，绝不该走到人工确认——「只填不发」分支失效了");
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

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn winocr_path() -> PathBuf {
    workspace_root().join("target").join("debug").join("winocr.exe")
}

fn config_path() -> PathBuf {
    match std::env::var("RPA_LIVE_CONFIG") {
        Ok(value) if !value.trim().is_empty() => PathBuf::from(value.trim()),
        _ => workspace_root().join("target").join("live-wechat.json"),
    }
}

fn desktop_for(config: &RuntimeConfig) -> WindowsDesktop {
    WindowsDesktop::new(WindowsDesktopConfig {
        wecom_exe: config.wecom_exe.as_ref().map(PathBuf::from),
        wecom_exe_sha256: config.wecom_exe_sha256.clone(),
        window_matcher: platform_windows::WindowMatcher::ClassName(config.window_class.clone()),
        ..WindowsDesktopConfig::default()
    })
}

/// 截取某个区域并做一次**真实**的本地 OCR。
///
/// 真实模式一旦失败，第一件要知道的事就是「OCR 到底看到了什么」，
/// 所以把识别结果打出来，比对着截图猜快得多。
fn recognize(
    desktop: &WindowsDesktop,
    ocr: &ExternalOcr,
    region: [f32; 4],
    window: Rect,
    label: &str,
) -> Vec<TextBox> {
    let rect = RelativeRegion::new(region[0], region[1], region[2], region[3]).resolve(window);
    let shot = desktop.capture(rect).expect("截屏失败");
    let boxes = ocr.recognize(&shot).expect("本地 OCR 调用失败");

    println!(
        "  {label} {}x{} @({},{})，指纹 {}，识别到 {} 个文字框",
        rect.width,
        rect.height,
        rect.x,
        rect.y,
        &shot.fingerprint[..16.min(shot.fingerprint.len())],
        boxes.len()
    );
    for b in &boxes {
        println!(
            "      ({:4},{:4}) {:3}x{:<3} 置信 {:.2}  「{}」",
            b.bounds.x, b.bounds.y, b.bounds.width, b.bounds.height, b.confidence, b.text
        );
    }
    boxes
}

/// 生成 6 位随机数字标记。
///
/// 用来在截图里**独立**确认"输入框里有这段文字"以及"聊天正文区里没有这段文字"。
/// 刻意只用数字：本地 OCR 对数字的识别比随机字母串稳得多。
fn unique_marker() -> String {
    Uuid::new_v4()
        .as_bytes()
        .iter()
        .take(6)
        .map(|byte| char::from(b'0' + (byte % 10)))
        .collect()
}

#[test]
#[ignore = "会在真实桌面上产生真实输入，需要人工先把微信窗口点到前台"]
fn live_wechat_finds_contact_by_scrolling_and_types_without_sending() {
    let ocr_path = winocr_path();
    assert!(
        ocr_path.is_file(),
        "缺少本地 OCR 工具 {}，请先执行：cargo build -p winocr",
        ocr_path.display()
    );

    // ── 配置 ────────────────────────────────────────────────────────
    let config_file = config_path();
    let mut config: RuntimeConfig = match std::fs::read_to_string(&config_file) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|err| {
            panic!("标定文件 {} 解析失败：{err}", config_file.display())
        }),
        Err(err) => panic!(
            "读不到标定文件 {}（{err}）。请先按文件头注释写一份，\
             或用 RPA_LIVE_CONFIG 指定路径。",
            config_file.display()
        ),
    };

    // 硬性安全约束：本用例**绝不发送**。即使标定文件里写了 false 也强制打开，
    // 免得"改个配置就把消息真发出去了"。
    config.mode = RuntimeMode::Live;
    config.stop_before_send = true;
    // 客户端由操作者手动启动，任务不再自己拉起程序（见 `runner::execute`）。
    // `wecom_exe` 现在的用途是**校验窗口归属**：填了它，定位就会要求窗口必须属于
    // 该程序。本用例要验的是滚动查找与输入这条链路，所以留空，不去和标定文件里
    // 可能写着的目标程序较劲。
    config.wecom_exe = None;
    config.ocr_command = Some(ocr_path.to_string_lossy().into_owned());
    config.wecom_exe_sha256 = None;

    let contact = std::env::var("RPA_LIVE_CONTACT").unwrap_or_else(|_| DEFAULT_CONTACT.to_string());
    let marker = unique_marker();
    let message = std::env::var("RPA_LIVE_MESSAGE")
        .unwrap_or_else(|_| format!("rpa_llm 只填不发验证 {marker}"));

    println!("标定文件：{}", config_file.display());
    println!("窗口类名：{}", config.window_class);
    println!("联系人：{contact}");
    println!("正文（{} 字符）：{message}", message.chars().count());
    println!("标记：{marker}（用来独立核对「填进去了」与「没发出去」）\n");

    // ── 预检：窗口必须已经在前台 ────────────────────────────────────
    let desktop = desktop_for(&config);
    let window = match desktop.focus_wecom() {
        Ok(rect) => rect,
        Err(err) => panic!(
            "无法定位/置前窗口「{}」：{err}\n\
             → 请**手动点一下微信窗口**让它保持在前台，再重跑本用例。\n\
             本进程不是前台进程时，Windows 会拒绝 SetForegroundWindow，\n\
             这是宿主限制，不是产品缺陷。",
            config.window_class
        ),
    };
    println!(
        "已定位目标窗口：{}x{} @({},{})\n",
        window.width, window.height, window.x, window.y
    );

    // 真实模式要求「标定尺寸」必须有值：任务只在标定过的尺寸下工作
    // （见 `runtime::build_runner`）。真机用例要验的是滚动查找与输入这条链路，
    // 不是尺寸校验本身，所以就地量一次当前窗口当作标定值。
    if config.calibrated_window.is_none() {
        match desktop.measure() {
            Ok((rect, metrics)) => {
                println!(
                    "标定尺寸（就地量取）：{}x{} @({},{})，缩放 {}\n",
                    rect.width, rect.height, rect.x, rect.y, metrics.scale_factor
                );
                config.calibrated_window = Some(WindowGeometry {
                    x: rect.x,
                    y: rect.y,
                    width: rect.width,
                    height: rect.height,
                    scale_factor: metrics.scale_factor,
                });
            }
            Err(err) => println!("（量取窗口尺寸失败：{err}；装配会被拒绝，请先修好再跑）\n"),
        }
    }

    // ── 预检：先看看联系人区到底识别到了什么 ────────────────────────
    let ocr = ExternalOcr::new(&ocr_path).with_timeout(Duration::from_secs(10));
    println!("预检（联系人区首屏）：");
    let first_screen = recognize(&desktop, &ocr, config.regions.contact_panel, window, "联系人区");
    if first_screen.iter().any(|b| b.text.contains(&contact)) {
        println!("\n  注意：「{contact}」已在首屏，本次不会真正触发滚动。");
        println!("  想验证滚动，请换一个需要下拉才能看到的名字。\n");
    } else {
        println!("\n  「{contact}」不在首屏——正好，工作流应当自己向下滚动去找。\n");
    }

    // ── 装配并执行真实工作流 ────────────────────────────────────────
    let task = SendTask {
        id: Uuid::new_v4(),
        external_contact_name: contact.clone(),
        text: message.clone(),
        created_by: "live-wechat".to_string(),
    };

    let audit = Arc::new(SqliteAuditStore::in_memory().expect("创建审计库失败"));
    let ledger = Arc::new(SqliteSendLedger::in_memory().expect("创建发送台账失败"));

    let runner = desktop_lib::runtime::build_runner(
        &config,
        &task,
        audit.clone() as Arc<dyn AuditSink>,
        ledger.clone() as Arc<dyn SendLedger>,
        Arc::new(NeverReached),
    )
    .expect("装配真实模式运行器失败");

    println!("开始执行真实模式工作流（只填不发）：");
    let outcome = runner.run(&task, &PrintProgress, &CancelToken::new());

    let recognitions = outcome
        .evidence
        .iter()
        .filter(|entry| entry.starts_with("contact_panel#"))
        .count();
    println!("\n终态：{:?}", outcome.state);
    if let Some(failure) = &outcome.failure {
        println!("失败代码：{}", failure.code);
        println!("失败原因：{}", failure.reason);
    }
    println!("联系人区识别轮数：{recognitions}（1 轮 = 首屏命中；>1 轮 = 滚动过）");
    println!("证据引用：{:?}", outcome.evidence);

    // ── 独立复核 ────────────────────────────────────────────────────
    println!("\n复核 1（输入框区）：");
    let composer = recognize(&desktop, &ocr, config.regions.composer, window, "输入框区");
    println!("\n复核 2（聊天正文区）：");
    let body = recognize(&desktop, &ocr, config.regions.chat_body, window, "聊天正文区");

    assert_eq!(
        outcome.state,
        TaskState::Prepared,
        "工作流没有走到「已填入正文，未发送」，失败信息：{:?}",
        outcome.failure
    );
    assert!(outcome.stopped_before_send(), "终态应当是 Prepared");
    assert!(
        !outcome.succeeded(),
        "「只填不发」不能被算作发送成功"
    );

    assert!(
        composer.iter().any(|b| b.text.contains(&marker)),
        "输入框区里没找到标记 {marker}——文字没真的填进去。识别结果：{:?}",
        composer.iter().map(|b| b.text.as_str()).collect::<Vec<_>>()
    );

    assert!(
        !body.iter().any(|b| b.text.contains(&marker)),
        "聊天正文区出现了标记 {marker}——消息被发出去了！这是严重缺陷。识别结果：{:?}",
        body.iter().map(|b| b.text.as_str()).collect::<Vec<_>>()
    );

    println!("\n✓ 已填入正文（标记 {marker} 出现在输入框区）");
    println!("✓ 未发送（标记 {marker} 没有出现在聊天正文区）");
    println!("→ 请人工看一眼微信窗口确认：文字在输入框里，且没有发出去。");
}

/// 负面用例：联系人不存在的时，必须**拒绝发送**并留下可读原因。
///
/// 对应验收条件 5（"识别失败时不会发送，并会留下可读失败原因"）。
/// 这里会真的滚到上限/滚到底，因此也顺带验证了滚动的两个边界。
#[test]
#[ignore = "会在真实桌面上产生真实输入，需要人工先把微信窗口点到前台"]
fn live_wechat_refuses_when_the_contact_cannot_be_found() {
    let ocr_path = winocr_path();
    assert!(ocr_path.is_file(), "请先执行：cargo build -p winocr");

    let config_file = config_path();
    let mut config: RuntimeConfig = serde_json::from_str(
        &std::fs::read_to_string(&config_file)
            .unwrap_or_else(|err| panic!("读不到标定文件 {}：{err}", config_file.display())),
    )
    .unwrap_or_else(|err| panic!("标定文件 {} 解析失败：{err}", config_file.display()));

    config.mode = RuntimeMode::Live;
    config.stop_before_send = true;
    // 同正例：客户端由操作者手动启动，`wecom_exe` 现在只用于校验窗口归属，留空。
    config.wecom_exe = None;
    config.ocr_command = Some(ocr_path.to_string_lossy().into_owned());
    config.wecom_exe_sha256 = None;

    let desktop = desktop_for(&config);
    let _ = desktop.focus_wecom().unwrap_or_else(|err| {
        panic!("无法定位/置前窗口「{}」：{err} → 请先手动点一下微信窗口", config.window_class)
    });

    // 真实模式要求「标定尺寸」必须有值，就地量一次。
    if config.calibrated_window.is_none() {
        if let Ok((rect, metrics)) = desktop.measure() {
            config.calibrated_window = Some(WindowGeometry {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
                scale_factor: metrics.scale_factor,
            });
        }
    }

    // 这个名字不可能存在，必须走"找不到 → 转人工"。
    let task = SendTask {
        id: Uuid::new_v4(),
        external_contact_name: "这个名字一定不存在ZZZ".to_string(),
        text: "rpa_llm 负面用例".to_string(),
        created_by: "live-wechat".to_string(),
    };

    let audit = Arc::new(SqliteAuditStore::in_memory().expect("创建审计库失败"));
    let ledger = Arc::new(SqliteSendLedger::in_memory().expect("创建发送台账失败"));

    let runner = desktop_lib::runtime::build_runner(
        &config,
        &task,
        audit.clone() as Arc<dyn AuditSink>,
        ledger.clone() as Arc<dyn SendLedger>,
        Arc::new(NeverReached),
    )
    .expect("装配真实模式运行器失败");

    println!("开始执行负面用例（应当转人工）：");    let outcome = runner.run(&task, &PrintProgress, &CancelToken::new());

    let recognitions = outcome
        .evidence
        .iter()
        .filter(|entry| entry.starts_with("contact_panel#"))
        .count();
    println!("\n终态：{:?}，联系人区识别轮数：{recognitions}", outcome.state);

    assert_eq!(
        outcome.state,
        TaskState::NeedsHumanReview,
        "找不到联系人的时候必须转人工，绝不能猜一个近似名"
    );
    assert!(!outcome.succeeded(), "绝不能算作发送成功");
    assert!(
        recognitions > 1,
        "应当滚动查找过（识别轮数 {recognitions}），而不是只看了一眼首屏"
    );
    let reason = outcome.failure.expect("应当留下可读失败原因").reason;
    println!("失败原因：{reason}");
    assert!(reason.contains("滚动") || reason.contains("未找到"), "失败原因应说明查找失败：{reason}");
}
