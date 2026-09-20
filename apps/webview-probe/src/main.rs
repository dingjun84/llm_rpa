//! WebView 渲染探针。
//!
//! 目的只有一个：**把"界面出不来"这件事从主应用里隔离出来**，判断病因在
//! 环境（WebView2 / 窗口 / 图形栈）还是在主应用自己的前端栈（Vite + React）。
//!
//! 为此刻意做了三件事：
//!
//! 1. **零前端工具链**：没有 npm、没有 Vite、没有 React，前端就是
//!    `ui/index.html` 一个静态文件（`frontendDist` 直接指向目录）。
//!    不需要跑任何构建步骤，`cargo build` 出来的 exe 就能直接双击运行。
//! 2. **窗口在代码里建，不在配置里建**：这样 `additionalBrowserArgs`
//!    可以靠环境变量在运行时切换，不用改配置重编就能做 A/B 对比。
//! 3. **全过程写日志文件**：窗口建不出来时界面上什么都看不到，
//!    所以从 `main` 第一行起就把每一步写进 exe 旁边的 `webview-probe.log`。
//!    那份日志是唯一能在"全黑/全白窗口"情况下拿到的证据。
//!
//! 界面本身只有两个按钮（确认 / 取消），另加一块只读的诊断面板。

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use serde::Serialize;
use tauri::{WebviewUrl, WebviewWindowBuilder};

/// 探针窗口的标签。
const WINDOW_LABEL: &str = "main";

/// 主应用 `apps/desktop/src-tauri/tauri.conf.json` 里当前用的 `additionalBrowserArgs`。
///
/// 这里**故意写成字面量**而不是去读主应用的配置：探针要在"主应用配置本身可能就是
/// 病因"的前提下工作，而我们要的正是 A/B 对比——直接读那份配置反而没法做对照。
/// 代价是主应用那份改了这里要跟着改。用 `PROBE_BROWSER_ARGS=app` 取这一份。
const APP_BROWSER_ARGS: &str =
    "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --disable-gpu --remote-debugging-port=9222";

/// 进程启动时刻，用来给日志打相对时间戳（不引 chrono）。
static START: OnceLock<Instant> = OnceLock::new();

/// 日志文件路径。写进 exe 所在目录，跑完可以直接从仓库里读。
static LOG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

// ── 日志 ────────────────────────────────────────────────────────────────

/// 挑一个日志落点。
///
/// 优先放在可执行文件旁边：那是**我知道去哪读**的位置（`target/debug/`）。
/// 万一目录只读（比如把 exe 拷到了别处），退到临时目录，保证一定有日志。
fn resolve_log_path() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("webview-probe.log");
            let writable = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&candidate)
                .is_ok();
            if writable {
                return candidate;
            }
        }
    }
    std::env::temp_dir().join("webview-probe.log")
}

/// 写一行日志：同时进文件与 stderr。
///
/// 每次都重新开文件并立刻 `flush`，是为了让**进程被杀/崩溃时已写的内容仍然在盘上**。
/// 探针最怕的就是"跑完什么都没留下"，性能在这里完全不重要。
fn log(line: &str) {
    let elapsed = START.get().map(|start| start.elapsed().as_millis()).unwrap_or(0);
    let stamp = format!("[{elapsed:>7} ms] {line}");
    eprintln!("{stamp}");
    if let Ok(guard) = LOG_PATH.lock() {
        if let Some(path) = guard.as_ref() {
            if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(file, "{stamp}");
                let _ = file.flush();
            }
        }
    }
}

fn current_log_path() -> String {
    LOG_PATH
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
        .map(|path| path.display().to_string())
        .unwrap_or_default()
}

// ── 环境探测 ────────────────────────────────────────────────────────────

/// 本机装了哪些 WebView2 运行时版本。
///
/// 走文件系统而不是注册表：`reg.exe` 在某些受限环境下会被安全策略拦掉，
/// 而目录列表到处都能读。目录名本身就是版本号。
fn webview2_versions() -> Vec<String> {
    // 这两个是 WebView2 的固定安装位置，不是"可调参数"，
    // 不随分辨率/机器配置变化，所以直接写死。
    const ROOTS: [&str; 2] = [
        r"C:\Program Files (x86)\Microsoft\EdgeWebView\Application",
        r"C:\Program Files\Microsoft\EdgeWebView\Application",
    ];

    let mut found: Vec<String> = Vec::new();
    for root in ROOTS {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            // 只看形如 `153.0.4234.32` 的目录，跳过 `SetupMetrics` 之类。
            let looks_like_version = name
                .chars()
                .next()
                .map(|first| first.is_ascii_digit())
                .unwrap_or(false);
            if looks_like_version {
                found.push(name);
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

#[derive(Debug, Clone, Serialize)]
pub struct EnvInfo {
    pub exe: String,
    pub cwd: String,
    pub log_path: String,
    /// 实际传给 WebView2 的附加参数；`None` 表示一个都没传（最朴素的启动方式）。
    pub browser_args: Option<String>,
    pub tauri_version: String,
    pub os_version: String,
    pub webview2_versions: Vec<String>,
    /// WebView2 允许通过环境变量指定运行时目录，一并记下来便于排查。
    pub webview2_env_override: Option<String>,
}

#[tauri::command]
fn env_info() -> EnvInfo {
    EnvInfo {
        exe: std::env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        cwd: std::env::current_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        log_path: current_log_path(),
        browser_args: resolve_browser_args(),
        tauri_version: tauri::VERSION.to_string(),
        os_version: os_version(),
        webview2_versions: webview2_versions(),
        webview2_env_override: std::env::var("WEBVIEW2_BROWSER_EXECUTABLE_FOLDER").ok(),
    }
}

/// 读 Windows 版本号。
///
/// 不引 `windows` crate（探针只依赖 tauri 与 serde，依赖越少越不容易编不过），
/// 直接读 `cmd /c ver`。这条命令不依赖注册表，也不会被安全策略拦。
fn os_version() -> String {
    let output = std::process::Command::new("cmd").args(["/c", "ver"]).output();
    match output {
        Ok(output) => String::from_utf8_lossy(&output.stdout).trim().to_string(),
        Err(err) => format!("<读不到：{err}>"),
    }
}

/// 解析 `PROBE_BROWSER_ARGS`。
///
/// - 不设 / 空串 ⇒ 不传任何附加参数（最朴素，先看这一档）
/// - `app`      ⇒ 用主应用当前那份（见 [`APP_BROWSER_ARGS`]）
/// - 其他值     ⇒ 原样作为附加参数
fn resolve_browser_args() -> Option<String> {
    let raw = std::env::var("PROBE_BROWSER_ARGS").unwrap_or_default();
    match raw.trim() {
        "" => None,
        "app" | "APP" => Some(APP_BROWSER_ARGS.to_string()),
        other => Some(other.to_string()),
    }
}

// ── 前端回调 ────────────────────────────────────────────────────────────

/// 前端把关键事件回传到这里，落进日志。
///
/// 这是判断"WebView2 到底走到哪一步"的主证据链：
/// `js_boot` 出现 ⇒ HTML 与 JS 都跑起来了；只有 `page_load` 没有 `js_boot`
/// ⇒ 页面加载了但脚本没执行；两个都没有 ⇒ WebView2 根本没起来。
#[tauri::command]
fn report(event: String, detail: Option<String>) -> Result<String, String> {
    let detail = detail.unwrap_or_default();
    if detail.is_empty() {
        log(&format!("前端 :: {event}"));
    } else {
        log(&format!("前端 :: {event} :: {detail}"));
    }
    Ok(current_log_path())
}

// ── 入口 ────────────────────────────────────────────────────────────────

fn main() {
    let _ = START.set(Instant::now());

    let path = resolve_log_path();
    if let Ok(mut guard) = LOG_PATH.lock() {
        *guard = Some(path.clone());
    }

    log("================ webview-probe 启动 ================");
    log(&format!("日志文件    : {}", path.display()));
    log(&format!(
        "可执行文件  : {}",
        std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|e| format!("<读不到：{e}>"))
    ));
    log(&format!("tauri       : {}", tauri::VERSION));
    log(&format!("系统        : {}", os_version()));
    log(&format!("WebView2    : {:?}", webview2_versions()));

    let browser_args = resolve_browser_args();
    match browser_args.as_ref() {
        Some(args) => log(&format!("附加参数    : {args}")),
        None => log("附加参数    : <无，使用 WebView2 默认>"),
    }

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![report, env_info])
        .setup(move |app| {
            log("setup 回调进入");

            let mut builder =
                WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::App("index.html".into()))
                    .title("WebView 探针")
                    .inner_size(760.0, 620.0)
                    .min_inner_size(520.0, 420.0);

            if let Some(args) = browser_args.as_ref() {
                builder = builder.additional_browser_args(args);
            }

            // 页面导航事件：这是 WebView2 侧唯一能拿到的"它动了"的信号。
            builder = builder.on_page_load(|_window, payload| {
                log(&format!(
                    "页面导航    : {:?} {}",
                    payload.event(),
                    payload.url()
                ));
            });

            match builder.build() {
                Ok(window) => log(&format!("窗口创建成功: label={}", window.label())),
                Err(err) => {
                    // 窗口都建不出来时界面上没有任何东西可以显示，
                    // 所以这里只能靠日志——那正是它存在的意义。
                    log(&format!("!! 窗口创建失败: {err}"));
                    return Err(err.into());
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("探针启动失败");

    log("事件循环已退出（窗口被关闭）");
}
