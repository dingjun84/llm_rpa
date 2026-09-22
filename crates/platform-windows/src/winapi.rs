//! Win32 调用的最小封装。
//!
//! 所有 `unsafe` 都集中在本模块（含 `cursor` 子模块），上层只看到 `Result`。
//! 这里不做任何策略判断，策略在 [`crate::desktop`] 中。

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use automation_core::Rect;
use sha2::{Digest, Sha256};
use windows::core::{BOOL, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, GlobalFree, HANDLE, HWND, LPARAM, MAX_PATH, POINT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    GetDeviceCaps, GetMonitorInfoW, MonitorFromRect, ReleaseDC, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ, LOGPIXELSX, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, SRCCOPY,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
// `PrintWindow` 虽然在 user32 里，但 windows-rs 把它归到了 `Storage::Xps` 下。
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetOpenClipboardWindow, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
};
use windows::Win32::System::Threading::{
    CreateProcessW, OpenProcess, QueryFullProcessImageNameW, PROCESS_INFORMATION,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, STARTUPINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetAncestor, GetClassNameW, GetCursorPos, GetForegroundWindow,
    GetSystemMetrics, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsHungAppWindow, IsIconic, IsWindowVisible,
    SetForegroundWindow, SetWindowPos, ShowWindow, WindowFromPoint, GA_ROOT, PW_RENDERFULLCONTENT,
    SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SW_RESTORE,
};

/// `CF_UNICODETEXT`：Win32 中稳定的剪贴板格式常量。
const CF_UNICODETEXT_FORMAT: u32 = 13;

/// 窗口类名的最大长度。
///
/// Win32 规定类名**不超过 256 个字符**（`MAX_CLASS_NAME`），所以这个缓冲区
/// 不可能截断。这里写死是**照规范来**，不是为了省事——不需要做成可配置的。
const MAX_CLASS_NAME_CHARS: usize = 256;

/// 读取进程可执行文件路径的缓冲区长度。
///
/// `MAX_PATH` 是 260，但带 `\\?\` 前缀的长路径可以远超它，所以按 4 倍开。
/// 注意这个值**不是正确性边界**：`QueryFullProcessImageNameW` 在缓冲区不够时
/// 会直接失败并报错，不会静默截断，所以"够用就行"。
const PROCESS_PATH_BUFFER_CHARS: usize = MAX_PATH as usize * 4;

pub type WinResult<T> = Result<T, String>;

/// 捕获到的一帧画面（BGRA，自上而下）。
pub struct CapturedFrame {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub fingerprint: String,
}

// ── 窗口 ────────────────────────────────────────────────────────────────

struct TitleSearch {
    prefix: Vec<u16>,
    found: Option<HWND>,
}

unsafe extern "system" fn enum_title_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = unsafe { &mut *(lparam.0 as *mut TitleSearch) };
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return BOOL(1);
    }
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return BOOL(1);
    }
    let mut buffer = vec![0u16; len as usize + 1];
    let copied = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    if copied <= 0 {
        return BOOL(1);
    }
    if buffer[..copied as usize].starts_with(&ctx.prefix) {
        ctx.found = Some(hwnd);
        return BOOL(0);
    }
    BOOL(1)
}

/// 按窗口类名查找**可见**的顶层窗口。
///
/// 为什么不用 `FindWindowW(class, null)`：它**不区分可见性**，而 Qt 系程序
/// （微信 4.x 就是）**所有顶层窗口共用同一个类名**——主窗口、登录窗、设置窗，
/// 以及各种隐藏的辅助窗口，类名全是 `Qt51514QWindowIcon`。
/// `FindWindowW` 只按 Z 序返回第一个命中的，那个很可能是隐藏窗口，
/// 于是"定位成功"却拿到一个退化矩形（0×0）或根本不是用户想要的那个窗口。
///
/// 改成 `EnumWindows` + `IsWindowVisible` + 类名比对，与按标题查找
/// （[`find_window_by_title_prefix`]）的严格程度**完全一致**。`EnumWindows`
/// 按 Z 序枚举（最上层在前），所以拿到的是**最靠上的那个可见匹配**。
///
/// `IsWindowVisible` 对**最小化**窗口返回真（它查的是 `WS_VISIBLE` 样式位，
/// 与是否最小化、是否被遮挡无关），所以这个过滤不会误杀正常可用的窗口。
///
/// 注意这里**仍不校验窗口属于哪个进程**——需要那一层请用
/// [`find_window_by_class_in_process`]。
pub fn find_window_by_class(class_name: &str) -> WinResult<HWND> {
    let mut ctx = ClassSearch { class: class_name.to_string(), found: None };
    unsafe {
        let _ = EnumWindows(
            Some(enum_class_proc),
            LPARAM(&mut ctx as *mut ClassSearch as isize),
        );
    }
    ctx.found.ok_or_else(|| format!("未找到类名为「{class_name}」的可见窗口"))
}

/// 按窗口类名查找**属于指定程序**的可见顶层窗口。
///
/// 为什么需要这一层：Qt 系程序（微信 4.x 就是）所有顶层窗口共用同一个类名，
/// 主窗口、登录窗、设置窗、各种隐藏辅助窗口全是 `Qt51514QWindowIcon`。
/// 只按类名匹配，拿到的是"Z 序最靠上的那个"，很可能不是你要的那个。
/// 加上"属于哪个 exe"这一条，才能把"目标窗口"这件事说清楚。
///
/// 同样按 Z 序枚举，返回**最靠上的**可见匹配。
pub fn find_window_by_class_in_process(class_name: &str, exe: &Path) -> WinResult<HWND> {
    let mut ctx = OwnedClassSearch {
        class: class_name.to_string(),
        exe: exe.to_path_buf(),
        found: None,
    };
    unsafe {
        let _ = EnumWindows(
            Some(enum_class_in_process_proc),
            LPARAM(&mut ctx as *mut OwnedClassSearch as isize),
        );
    }
    ctx.found.ok_or_else(|| {
        format!(
            "未找到属于「{}」且类名为「{class_name}」的可见窗口",
            exe.display()
        )
    })
}

struct OwnedClassSearch {
    class: String,
    exe: PathBuf,
    found: Option<HWND>,
}

unsafe extern "system" fn enum_class_in_process_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = unsafe { &mut *(lparam.0 as *mut OwnedClassSearch) };
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return BOOL(1);
    }
    if window_class_name(hwnd) != ctx.class {
        return BOOL(1);
    }
    if !process_matches(hwnd, &ctx.exe) {
        return BOOL(1);
    }
    ctx.found = Some(hwnd);
    BOOL(0)
}

/// 该窗口所属进程的可执行文件是不是 `exe`。
///
/// 比较用规范化后的路径、忽略大小写（Windows 路径不区分大小写）。
/// `canonicalize` 失败（例如权限受限读不到）就退回原样比较——
/// 宁可比较得松一点，也不要因为拿不到规范路径就断定"不是同一个程序"。
///
/// 读不到路径时返回 `false`：调用方是拿它当**准入条件**用的，
/// "读不出来"不该被当成"就是它"。
pub fn process_matches(hwnd: HWND, exe: &Path) -> bool {
    let Ok(actual) = window_process_path(hwnd) else {
        return false;
    };
    let normalize = |path: &Path| {
        std::fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .into_owned()
    };
    normalize(&actual).eq_ignore_ascii_case(&normalize(exe))
}

/// 系统是否认为该窗口**未响应**。
///
/// `IsHungAppWindow` 由窗口管理器判定：窗口所属线程超过 5 秒没有从消息队列取消息，
/// 系统就把它标记为未响应。它是**纯只读**查询——不发送消息、不改变焦点、
/// 不等待对方线程，所以可以安全地放在每次点击之前。
///
/// 它只回答"界面线程还在不在转"，回答不了"界面有没有刷新"。
/// 后者要靠画面变化来判断，两者是互补的，缺一不可。
pub fn is_hung_window(hwnd: HWND) -> bool {
    unsafe { IsHungAppWindow(hwnd).as_bool() }
}

struct ClassSearch {
    class: String,
    found: Option<HWND>,
}

unsafe extern "system" fn enum_class_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = unsafe { &mut *(lparam.0 as *mut ClassSearch) };
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return BOOL(1);
    }
    if window_class_name(hwnd) == ctx.class {
        ctx.found = Some(hwnd);
        return BOOL(0);
    }
    BOOL(1)
}

pub fn find_window_by_title_prefix(prefix: &str) -> WinResult<HWND> {
    let mut ctx = TitleSearch { prefix: prefix.encode_utf16().collect(), found: None };
    unsafe {
        let _ = EnumWindows(
            Some(enum_title_proc),
            LPARAM(&mut ctx as *mut TitleSearch as isize),
        );
    }
    ctx.found.ok_or_else(|| format!("未找到标题以「{prefix}」开头的可见窗口"))
}

/// 一个可见顶层窗口的快照，用于诊断与"定位企业微信"这一步的排查。
#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub hwnd: isize,
    pub title: String,
    pub class_name: String,
    pub rect: Rect,
    pub is_foreground: bool,
    pub is_minimized: bool,
}

struct WindowScan {
    foreground: HWND,
    found: Vec<WindowInfo>,
}

unsafe extern "system" fn enum_scan_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = unsafe { &mut *(lparam.0 as *mut WindowScan) };
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return BOOL(1);
    }
    // 只收集有标题的窗口：无标题的通常是托盘、输入法等辅助窗口。
    let title = window_title(hwnd);
    if title.is_empty() {
        return BOOL(1);
    }
    let Ok(rect) = window_rect(hwnd) else {
        return BOOL(1);
    };
    ctx.found.push(WindowInfo {
        hwnd: hwnd.0 as isize,
        title,
        class_name: window_class_name(hwnd),
        rect,
        is_foreground: same_window(hwnd, ctx.foreground),
        is_minimized: is_minimized(hwnd),
    });
    BOOL(1)
}

/// 列出当前所有可见且有标题的顶层窗口，按窗口左上角排序。
///
/// 只读操作，不改变任何窗口状态。
pub fn list_visible_windows() -> Vec<WindowInfo> {
    let mut ctx = WindowScan { foreground: foreground_window(), found: Vec::new() };
    unsafe {
        let _ = EnumWindows(
            Some(enum_scan_proc),
            LPARAM(&mut ctx as *mut WindowScan as isize),
        );
    }
    ctx.found.sort_by_key(|info| (info.rect.y, info.rect.x));
    ctx.found
}

pub fn window_class_name(hwnd: HWND) -> String {
    let mut buffer = vec![0u16; MAX_CLASS_NAME_CHARS];
    let copied = unsafe { GetClassNameW(hwnd, &mut buffer) };
    if copied <= 0 {
        return String::new();
    }
    OsString::from_wide(&buffer[..copied as usize]).to_string_lossy().into_owned()
}

pub fn window_title(hwnd: HWND) -> String {
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return String::new();
    }
    let mut buffer = vec![0u16; len as usize + 1];
    let copied = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    if copied <= 0 {
        return String::new();
    }
    OsString::from_wide(&buffer[..copied as usize]).to_string_lossy().into_owned()
}

/// 光标当前的屏幕坐标。
///
/// 只读，不改变任何状态。
pub fn cursor_position() -> WinResult<(i32, i32)> {
    let mut point = POINT::default();
    unsafe { GetCursorPos(&mut point) }.map_err(|err| format!("读取光标位置失败：{err}"))?;
    Ok((point.x, point.y))
}

/// 屏幕坐标下那个**顶层**窗口。
///
/// `WindowFromPoint` 返回的可能是子控件（比如输入框本身），所以再上溯到根窗口 ——
/// 调用方要的是「操作者指着哪个窗口」，不是「哪个控件」。
///
/// 该点落在桌面本体上时返回 `None`。
///
/// 只读，不改变焦点、不产生任何输入。这是「让操作者用鼠标指认目标窗口」
/// 这个方案的地基：不需要知道窗口类名，也不需要把窗口置前。
pub fn window_from_point(x: i32, y: i32) -> Option<HWND> {
    let hwnd = unsafe { WindowFromPoint(POINT { x, y }) };
    if hwnd.0.is_null() {
        return None;
    }
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    Some(if root.0.is_null() { hwnd } else { root })
}

/// 窗口所属进程的可执行文件路径。
///
/// 这是**窗口身份校验**的关键一环：窗口类名可以被别的程序复用，
/// 但「这个窗口属于哪个 exe」是进程级事实，靠它才能回答
/// 「找到的这个窗口到底是不是目标程序」。
///
/// 用 `PROCESS_QUERY_LIMITED_INFORMATION` 而不是 `PROCESS_QUERY_INFORMATION`：
/// 前者对权限的要求低得多，不需要提权，读到的信息已经够用。
pub fn window_process_path(hwnd: HWND) -> WinResult<PathBuf> {
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return Err("无法读取窗口所属进程 ID".to_string());
    }

    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .map_err(|err| format!("打开进程 {pid} 失败：{err}"))?;

    let mut buffer = vec![0u16; PROCESS_PATH_BUFFER_CHARS];
    let mut size = buffer.len() as u32;
    let queried = unsafe {
        QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut size)
    };
    // 无论成败都要关句柄，否则每次调用漏一个内核对象。
    unsafe {
        let _ = CloseHandle(process);
    }

    queried.map_err(|err| format!("读取进程 {pid} 的可执行文件路径失败：{err}"))?;
    Ok(PathBuf::from(OsString::from_wide(&buffer[..size as usize])))
}

pub fn window_rect(hwnd: HWND) -> WinResult<Rect> {
    let mut rect = windows::Win32::Foundation::RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }.map_err(|err| format!("读取窗口边界失败：{err}"))?;
    Ok(Rect {
        x: rect.left,
        y: rect.top,
        width: rect.right - rect.left,
        height: rect.bottom - rect.top,
    })
}

pub fn is_minimized(hwnd: HWND) -> bool {
    unsafe { IsIconic(hwnd) }.as_bool()
}

/// 把窗口的**外框**尺寸改成 `width`×`height`，位置不动、层级不动、不抢前台。
///
/// 三个标志缺一不可：
/// - `SWP_NOMOVE`：位置本来就不校验（只校验尺寸与 DPI），顺手挪窗口只会改变
///   操作者桌面上的布局，而且没必要；
/// - `SWP_NOZORDER`：不动 Z 序，免得把窗口从它所在的层级里拽出来；
/// - `SWP_NOACTIVATE`：不抢前台。前台切换是**异步**的、还可能被前台锁定策略吞掉
///   （见 [`wait_until_foreground`]），不该混进这一步——这一步只负责改尺寸。
///
/// 返回**重新量取的**边界：客户端可能有自己的最小尺寸限制，`SetWindowPos` 会报成功
/// 而窗口并没有变成请求的尺寸。判据只能是量出来的值，不能是请求值。
pub fn resize_window(hwnd: HWND, width: i32, height: i32) -> WinResult<Rect> {
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            0,
            0,
            width,
            height,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    }
    .map_err(|err| format!("调整窗口尺寸失败：{err}"))?;
    window_rect(hwnd)
}

/// 把窗口置于前台。先尝试还原最小化窗口，再设置前台。
///
/// **只有前台进程能成功**：Windows 只允许"当前前台进程"切换前台窗口，后台进程的请求
/// 会被静默吞掉（表现是任务栏闪一下）。返回 `true` 也只表示"请求被接受"，不代表已经
/// 切过去了——调用方必须用 [`wait_until_foreground`] 核实，不能立刻读 `GetForegroundWindow`。
///
/// ★★ **不要试图绕过前台锁定**（2026-09-19 实测，两条路都堵死了）：
///
/// 1. `AttachThreadInput` 这个老办法**在 Win10/11 上已经失效**。实测把本线程分别附到
///    前台线程与目标线程上（两次 `AttachThreadInput` 都返回成功）之后，
///    `SetForegroundWindow` **依然返回 `false`**，前台一动不动。
/// 2. `BringWindowToTop` / `SetWindowPos(HWND_TOPMOST)` 只改 **Z 序**，
///    **不给键盘焦点**——而 `SendInput` 只送给前台窗口，所以对输入毫无帮助。
///
/// 结论：**抢前台这条路走不通**。要么调用方自己就是前台进程（界面上点「开始任务」
/// 时就是这样），要么请操作者点一下目标窗口。失败时如实报错，不要兜底重试。
pub fn bring_to_foreground(hwnd: HWND) -> WinResult<()> {
    if is_minimized(hwnd) {
        let _ = unsafe { ShowWindow(hwnd, SW_RESTORE) };
    }
    if unsafe { SetForegroundWindow(hwnd) }.as_bool() {
        return Ok(());
    }
    Err("无法将窗口置于前台，可能被系统前台锁定策略拒绝".to_string())
}

pub fn foreground_window() -> HWND {
    unsafe { GetForegroundWindow() }
}

/// 有界等待 `hwnd` 成为前台窗口；超时返回 `false`。
///
/// **为什么需要等**：`SetForegroundWindow` 返回 `true` 只表示"请求被接受"，
/// **不代表前台已经切过去了**——实际切换由窗口管理器完成，且仍可能被
/// 前台锁定策略吞掉（表现为任务栏闪一下）。所以调用后**不能立刻**
/// 用 `GetForegroundWindow()` 判定，否则会把"还没切完"误判成"切换失败"。
///
/// 注意这不是"重试到成功"：**只等这一次请求生效**，超时就认失败。
/// 本项目明确禁止反复重试到成功，所以这里等的是一个有界的结算窗口，
/// 而不是"再试一次"。
pub fn wait_until_foreground(hwnd: HWND, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if same_window(foreground_window(), hwnd) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
}

pub fn same_window(a: HWND, b: HWND) -> bool {
    a.0 == b.0
}

// ── 显示器指标 ──────────────────────────────────────────────────────────

pub fn primary_screen_size() -> (u32, u32) {
    let width = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let height = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    (width.max(0) as u32, height.max(0) as u32)
}

/// 主显示器的缩放比例（1.0 表示 100%）。
///
/// 标定与任务挑选请用 [`scale_factor_for_rect`]：多显示器 DPI 不同时主屏值会记错档。
pub fn primary_scale_factor() -> f32 {
    unsafe {
        let dc = GetDC(None);
        if dc.is_invalid() {
            return 1.0;
        }
        let dpi = GetDeviceCaps(Some(dc), LOGPIXELSX);
        let _ = ReleaseDC(None, dc);
        if dpi <= 0 {
            1.0
        } else {
            dpi as f32 / 96.0
        }
    }
}

/// 窗口矩形所在显示器的缩放（有效 DPI / 96）。
pub fn scale_factor_for_rect(rect: Rect) -> f32 {
    let (_, _, scale) = metrics_for_rect(rect);
    scale
}

/// 窗口所在显示器的逻辑像素尺寸 + 缩放（与 [`scale_factor_for_rect`] 同一套屏）。
pub fn metrics_for_rect(rect: Rect) -> (u32, u32, f32) {
    let win_rect = windows::Win32::Foundation::RECT {
        left: rect.x,
        top: rect.y,
        right: rect.x.saturating_add(rect.width),
        bottom: rect.y.saturating_add(rect.height),
    };
    unsafe {
        let monitor = MonitorFromRect(&win_rect, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let mut width = 0u32;
        let mut height = 0u32;
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            width = (info.rcMonitor.right - info.rcMonitor.left).max(0) as u32;
            height = (info.rcMonitor.bottom - info.rcMonitor.top).max(0) as u32;
        }
        let mut dpi_x = 0u32;
        let mut dpi_y = 0u32;
        let scale = if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_ok()
            && dpi_x > 0
        {
            dpi_x as f32 / 96.0
        } else {
            primary_scale_factor()
        };
        if width == 0 || height == 0 {
            let (w, h) = primary_screen_size();
            return (w, h, scale);
        }
        (width, height, scale)
    }
}

// ── 屏幕捕获 ────────────────────────────────────────────────────────────

pub fn capture_region(region: Rect) -> WinResult<CapturedFrame> {
    if region.width <= 0 || region.height <= 0 {
        return Err("捕获区域尺寸无效".to_string());
    }
    let width = region.width;
    let height = region.height;
    let stride = width as usize * 4;
    let mut pixels = vec![0u8; stride * height as usize];

    unsafe {
        let screen_dc = GetDC(None);
        if screen_dc.is_invalid() {
            return Err("获取屏幕设备上下文失败".to_string());
        }
        let mem_dc = CreateCompatibleDC(Some(screen_dc));
        if mem_dc.is_invalid() {
            let _ = ReleaseDC(None, screen_dc);
            return Err("创建兼容设备上下文失败".to_string());
        }
        let bitmap = CreateCompatibleBitmap(screen_dc, width, height);
        if bitmap.is_invalid() {
            let _ = DeleteDC(mem_dc);
            let _ = ReleaseDC(None, screen_dc);
            return Err("创建兼容位图失败".to_string());
        }
        let previous = SelectObject(mem_dc, HGDIOBJ(bitmap.0));

        let blit = BitBlt(mem_dc, 0, 0, width, height, Some(screen_dc), region.x, region.y, SRCCOPY);

        let mut info = BITMAPINFO::default();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            // 负高度表示自上而下的行序，避免后续翻转。
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };

        let copied = if blit.is_ok() {
            GetDIBits(
                mem_dc,
                bitmap,
                0,
                height as u32,
                Some(pixels.as_mut_ptr() as *mut std::ffi::c_void),
                &mut info,
                DIB_RGB_COLORS,
            )
        } else {
            0
        };

        SelectObject(mem_dc, previous);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(mem_dc);
        let _ = ReleaseDC(None, screen_dc);

        if copied == 0 {
            return Err("读取屏幕像素失败".to_string());
        }
    }

    let fingerprint = fingerprint_of(&pixels, width as u32, height as u32);
    Ok(CapturedFrame { pixels, width: width as u32, height: height as u32, fingerprint })
}

/// 用 `PrintWindow` 抓**窗口自己渲染出来的画面**。
///
/// 与 [`capture_region`] 的区别很要紧：
///
/// - `capture_region` 走的是**屏幕 DC 的 `BitBlt`**，拿到的是「此刻屏幕上这块矩形里的像素」。
///   窗口被遮挡时会截到上层窗口；而 **Chromium / WebView2 这类用 GPU 合成的内容，
///   `BitBlt` 常常抓回来一片白或一片黑** —— 那是截图方式的局限，不是页面没渲染。
/// - `PrintWindow` 是让窗口自己把内容画到给定 DC 上，因此能拿到 WebView2 的真实画面。
///   必须带 `PW_RENDERFULLCONTENT`（值 2），少了它 DirectComposition 类窗口同样只给空白。
///
/// 只读：不改变窗口状态、不影响焦点。仅用于诊断，不参与业务流程。
pub fn print_window(hwnd: HWND) -> WinResult<CapturedFrame> {
    let region = window_rect(hwnd)?;
    if region.width <= 0 || region.height <= 0 {
        return Err("窗口尺寸无效（可能已最小化）".to_string());
    }
    let width = region.width;
    let height = region.height;
    let stride = width as usize * 4;
    let mut pixels = vec![0u8; stride * height as usize];

    unsafe {
        let screen_dc = GetDC(None);
        if screen_dc.is_invalid() {
            return Err("获取屏幕设备上下文失败".to_string());
        }
        let mem_dc = CreateCompatibleDC(Some(screen_dc));
        if mem_dc.is_invalid() {
            let _ = ReleaseDC(None, screen_dc);
            return Err("创建兼容设备上下文失败".to_string());
        }
        let bitmap = CreateCompatibleBitmap(screen_dc, width, height);
        if bitmap.is_invalid() {
            let _ = DeleteDC(mem_dc);
            let _ = ReleaseDC(None, screen_dc);
            return Err("创建兼容位图失败".to_string());
        }
        let previous = SelectObject(mem_dc, HGDIOBJ(bitmap.0));

        let printed = PrintWindow(hwnd, mem_dc, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)).as_bool();

        let mut info = BITMAPINFO::default();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };

        let copied = if printed {
            GetDIBits(
                mem_dc,
                bitmap,
                0,
                height as u32,
                Some(pixels.as_mut_ptr() as *mut std::ffi::c_void),
                &mut info,
                DIB_RGB_COLORS,
            )
        } else {
            0
        };

        SelectObject(mem_dc, previous);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(mem_dc);
        let _ = ReleaseDC(None, screen_dc);

        if copied == 0 {
            return Err("PrintWindow 没取到画面（窗口可能不响应 WM_PRINT）".to_string());
        }
    }

    let fingerprint = fingerprint_of(&pixels, width as u32, height as u32);
    Ok(CapturedFrame { pixels, width: width as u32, height: height as u32, fingerprint })
}

/// 截图的稳定指纹：尺寸 + 像素内容的 SHA-256。
pub fn fingerprint_of(pixels: &[u8], width: u32, height: u32) -> String {
    let mut hasher = Sha256::new();
    hasher.update(width.to_le_bytes());
    hasher.update(height.to_le_bytes());
    hasher.update(pixels);
    hex_lower(&hasher.finalize())
}

// ── 输入 ────────────────────────────────────────────────────────────────
//
// 键盘 / 鼠标 / 滚轮 / 逐字输入的原语都搬到 `input.rs` 了。
// 这里只留再导出，`winapi::left_click` 等原路径不变。
mod input;

pub use input::{
    left_click, scroll_wheel, send_ctrl_a, send_ctrl_v, send_delete, send_unicode_text,
};

// ── 光标轨迹 ────────────────────────────────────────────────────────────
//
// 轨迹那段（纯几何 + 一个 SendInput 原语）自成一类，已整体搬到 `cursor.rs`。
// 这里只留再导出，`winapi::move_cursor` 等原路径不变。
mod cursor;

pub use cursor::{move_cursor, move_cursor_circle, CircleTrace};

// ── 剪贴板 ──────────────────────────────────────────────────────────────

pub fn set_clipboard_text(text: &str) -> WinResult<()> {
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    utf16.push(0);
    let bytes = utf16.len() * std::mem::size_of::<u16>();

    unsafe {
        OpenClipboard(None).map_err(|err| format!("打开剪贴板失败：{err}"))?;
        let result = (|| -> WinResult<()> {
            EmptyClipboard().map_err(|err| format!("清空剪贴板失败：{err}"))?;
            let handle = GlobalAlloc(GMEM_MOVEABLE, bytes)
                .map_err(|err| format!("分配剪贴板内存失败：{err}"))?;
            let pointer = GlobalLock(handle);
            if pointer.is_null() {
                let _ = GlobalFree(Some(handle));
                return Err("锁定剪贴板内存失败".to_string());
            }
            std::ptr::copy_nonoverlapping(utf16.as_ptr() as *const u8, pointer as *mut u8, bytes);
            let _ = GlobalUnlock(handle);
            // 交给系统后由系统接管内存，不再释放。
            SetClipboardData(CF_UNICODETEXT_FORMAT, Some(HANDLE(handle.0)))
                .map_err(|err| format!("写入剪贴板失败：{err}"))?;
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}

pub fn clear_clipboard() -> WinResult<()> {
    unsafe {
        OpenClipboard(None).map_err(|err| format!("打开剪贴板失败：{err}"))?;
        let result = EmptyClipboard().map_err(|err| format!("清空剪贴板失败：{err}"));
        let _ = CloseClipboard();
        result
    }
}

/// 等待剪贴板被其它窗口取用完毕。
///
/// 必要性：`SendInput` 只是把按键**排进队列**就返回了，目标程序要等自己的
/// 消息循环跑到那一条才会去读剪贴板。如果在它读取之前就 `EmptyClipboard`，
/// 粘贴会拿到空内容 —— 表现为"点了、光标也在，但什么都没粘上"。
///
/// 判定方式：轮询 [`GetOpenClipboardWindow`]。观察到"被其它窗口占用"再
/// 恢复空闲，就说明确实有人来读过。
///
/// 返回 `true` 表示观察到过占用；`false` 表示整个 `timeout` 内都没看到
/// （例如目标程序自己缓存了内容），此时调用方应补一个保守等待。
pub fn wait_for_clipboard_release(poll: Duration, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let mut observed = false;
    loop {
        // 没有窗口占用时返回 NULL；windows-rs 把它包在 Result 里。
        let busy = unsafe { GetOpenClipboardWindow() }
            .map(|hwnd| !hwnd.0.is_null())
            .unwrap_or(false);
        if !busy {
            if observed {
                return true;
            }
        } else {
            observed = true;
        }
        if Instant::now() >= deadline {
            return observed;
        }
        std::thread::sleep(poll);
    }
}

// ── 进程 ────────────────────────────────────────────────────────────────

/// 不请求提权地启动一个可执行文件。
pub fn launch_process(exe: &Path) -> WinResult<()> {
    let mut command_line: Vec<u16> = OsString::from(exe.as_os_str())
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let startup = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut info = PROCESS_INFORMATION::default();
        CreateProcessW(
            PCWSTR::null(),
            Some(windows::core::PWSTR(command_line.as_mut_ptr())),
            None,
            None,
            false,
            Default::default(),
            None,
            PCWSTR::null(),
            &startup,
            &mut info,
        )
        .map_err(|err| format!("启动企业微信失败：{err}"))?;
        let _ = windows::Win32::Foundation::CloseHandle(info.hProcess);
        let _ = windows::Win32::Foundation::CloseHandle(info.hThread);
    }
    Ok(())
}

pub fn file_sha256(path: &Path) -> WinResult<String> {
    let mut file = std::fs::File::open(path).map_err(|err| format!("读取可执行文件失败：{err}"))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|err| format!("计算文件哈希失败：{err}"))?;
    Ok(hex_lower(&hasher.finalize()))
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

// 用例放在 `winapi/tests.rs`（与 `hotkey.rs` + `hotkey/tests.rs` 同一套写法）：
// 测试文件不计入规模上限，而这个源文件已经超标了。
#[cfg(test)]
mod tests;
