//! Win32 调用的最小封装。
//!
//! 所有 `unsafe` 都集中在本模块，上层只看到 `Result`。
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
    GetDeviceCaps, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    HGDIOBJ, LOGPIXELSX, SRCCOPY,
};
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
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, VIRTUAL_KEY, VK_A,
    VK_CONTROL, VK_DELETE, VK_RETURN, VK_V,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetAncestor, GetClassNameW, GetCursorPos, GetForegroundWindow,
    GetSystemMetrics, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsHungAppWindow, IsIconic, IsWindowVisible,
    SetForegroundWindow, SetWindowPos, ShowWindow, WindowFromPoint, GA_ROOT, PW_RENDERFULLCONTENT,
    SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SW_RESTORE,
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

fn key_input(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    let mut input = INPUT::default();
    input.r#type = INPUT_KEYBOARD;
    input.Anonymous.ki = KEYBDINPUT {
        wVk: vk,
        wScan: 0,
        dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() },
        time: 0,
        dwExtraInfo: 0,
    };
    input
}

fn mouse_input(flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS) -> INPUT {
    mouse_input_with(flags, 0)
}

fn mouse_input_with(
    flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS,
    mouse_data: i32,
) -> INPUT {
    let mut input = INPUT::default();
    input.r#type = INPUT_MOUSE;
    input.Anonymous.mi = MOUSEINPUT {
        dx: 0,
        dy: 0,
        mouseData: mouse_data as u32,
        dwFlags: flags,
        time: 0,
        dwExtraInfo: 0,
    };
    input
}

/// 一次绝对坐标的鼠标移动。
///
/// `dx` / `dy` 是**已归一化**到 0..=65535 的坐标（见 [`move_cursor_absolute`]），
/// 不是像素——所以这里不能复用按像素说话的 [`mouse_input_with`]。
fn mouse_move_input(dx: i32, dy: i32) -> INPUT {
    let mut input = INPUT::default();
    input.r#type = INPUT_MOUSE;
    input.Anonymous.mi = MOUSEINPUT {
        dx,
        dy,
        mouseData: 0,
        dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        time: 0,
        dwExtraInfo: 0,
    };
    input
}

/// Win32 的 `WHEEL_DELTA`：滚轮一格对应的 `mouseData` 增量。
const WHEEL_DELTA_UNITS: i32 = 120;

/// 逐格发送滚轮事件之间的间隔。
///
/// 把多格合并成一次 `SendInput`（`mouseData = ±360`）在多数程序里可用，
/// 但 WebView 类界面（微信 4.x 就是）可能只当作一次滚动，实际滚动量不足。
/// 逐格发送更接近真实滚轮——这是**为了稳妥的刻意选择**，不是在真机上对比测出的结论。
const WHEEL_STEP_DELAY: Duration = Duration::from_millis(15);

/// 在光标当前位置滚动鼠标滚轮。
///
/// `notches > 0` 表示**向下滚动内容**（看列表里更靠后的项），`< 0` 表示向上。
/// 注意 Win32 的约定与直觉相反：`mouseData` 为**正**表示滚轮向远离用户的方向转，
/// 内容向上移动；所以向下滚要传负值。
///
/// 调用方必须先把光标移到目标控件上——滚轮事件只会送给光标下的窗口。
pub fn scroll_wheel(notches: i32) -> WinResult<()> {
    if notches == 0 {
        return Ok(());
    }
    let step = if notches > 0 { -WHEEL_DELTA_UNITS } else { WHEEL_DELTA_UNITS };
    for index in 0..notches.unsigned_abs() {
        let input = mouse_input_with(MOUSEEVENTF_WHEEL, step);
        let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
        if sent != 1 {
            return Err(format!("发送滚轮事件失败（第 {} 格）", index + 1));
        }
        if index + 1 < notches.unsigned_abs() {
            std::thread::sleep(WHEEL_STEP_DELAY);
        }
    }
    Ok(())
}

// ── 光标轨迹 ────────────────────────────────────────────────────────────

/// 光标轨迹相邻两步之间的间隔。
///
/// 8ms ≈ 125Hz，正是最常见 USB 鼠标的**默认回报率**：真鼠标每秒只向系统报告约
/// 125 次位置，系统的鼠标消息也按这个节奏合并。按它走，轨迹在系统里留下的痕迹
/// 与真鼠标是同一种东西。
///
/// 写死而不做成配置，是因为它描述的是"真鼠标长什么样"，**不随机器变**
/// （换台机器，USB 鼠标还是 125Hz）。该由人调的是**速度**，那个是配置。
const POINTER_STEP_INTERVAL: Duration = Duration::from_millis(8);

/// 一次移动的最短耗时。
///
/// 低于这个时长，人眼看到的仍是一次瞬移——正是要消除的那个现象。
const POINTER_MIN_DURATION: Duration = Duration::from_millis(120);

/// 一次移动的最长耗时。
///
/// 人做一次大幅度移动也就是这个量级；再长就不像"移动"而像"卡住"，
/// 而且每个点击都要多等这么久。
///
/// **代价**：真正超长的距离（跨多个显示器）会被压缩到这个时长，轨迹会显得偏快。
/// 这是刻意的取舍——「看得见」比「严格按速度走完」更重要，而且速度本身是可配的。
const POINTER_MAX_DURATION: Duration = Duration::from_millis(800);

/// 把光标沿一条**人形轨迹**移到 `(x, y)`。
///
/// ## 为什么要走轨迹
///
/// 一步跳过去（`SetCursorPos`）在系统里留下的是"光标凭空出现在别处"：操作者
/// 看不出发生了什么，目标程序的悬停/移入事件也只收到一个终态。按真鼠标那样分步
/// 走，光标是**看得见地飞过去**的，目标程序依次收到的也是与真鼠标同型的移动消息。
///
/// ## 轨迹形状
///
/// 用 smoothstep（`3t² - 2t³`）缓动：起步慢、中间快、收尾慢，与人手做指向动作
/// 时的速度曲线同型。**不用随机抖动**——那会把「同一个点击为什么这次成功那次
/// 失败」变成查不清的问题，而本项目要求行为可复现。
///
/// ## 参数怎么来的
///
/// 总时长只由**距离与速度**算出（`距离 ÷ 速度`），再按 [`POINTER_STEP_INTERVAL`]
/// 切成若干步。速度由调用方从配置传入，所以"快慢"可调，"像不像人手"是算出来的。
pub fn move_cursor(x: i32, y: i32, speed_px_per_sec: f64) -> WinResult<()> {
    if !speed_px_per_sec.is_finite() || speed_px_per_sec <= 0.0 {
        return Err(format!("光标速度必须是正数，收到 {speed_px_per_sec}"));
    }

    let (from_x, from_y) = cursor_position()?;
    let dx = (x - from_x) as f64;
    let dy = (y - from_y) as f64;
    let distance = (dx * dx + dy * dy).sqrt();
    // 已经在那儿了（或差不到一个像素）就直接落位：别为了"像人"在原地抖满最短时长。
    if distance < 1.0 {
        return move_cursor_absolute(x, y);
    }

    let duration = Duration::from_secs_f64(distance / speed_px_per_sec)
        .clamp(POINTER_MIN_DURATION, POINTER_MAX_DURATION);
    let steps =
        ((duration.as_secs_f64() / POINTER_STEP_INTERVAL.as_secs_f64()).round() as u32).max(1);
    let step_delay = duration / steps;

    for step in 1..=steps {
        let t = step as f64 / steps as f64;
        let eased = t * t * (3.0 - 2.0 * t);
        let px = (from_x as f64 + dx * eased).round() as i32;
        let py = (from_y as f64 + dy * eased).round() as i32;
        move_cursor_absolute(px, py)?;
        // 最后一步之后不再睡：位置已经到位，多等一帧只是拖慢每个点击。
        if step < steps {
            std::thread::sleep(step_delay);
        }
    }
    Ok(())
}

/// 发一次绝对坐标的鼠标移动。
///
/// 走 `SendInput` 而不是 `SetCursorPos`：前者是**真实输入管线**，返回值就是系统
/// 实际接受的事件数——被拦下时是 0，我们能当场发现。后者存在"返回成功、光标却没动"
/// 的情形（前台窗口属于更高完整性的进程等），而"鼠标没动"正是最难查的成因：
/// 后面那次点击会落到光标**实际停着**的地方。
fn move_cursor_absolute(x: i32, y: i32) -> WinResult<()> {
    let (left, top, width, height) = virtual_screen_rect();
    if width <= 1 || height <= 1 {
        return Err(format!("虚拟桌面尺寸异常（{width}x{height}），拒绝发送绝对坐标"));
    }
    // 绝对坐标要归一化到 0..=65535，且 65535 对应虚拟桌面的**右下角像素**，
    // 所以分母是 (尺寸 - 1)：用尺寸会让整条轨迹往右下偏一格。
    let nx = ((x - left) as i64 * 65_535 / (width as i64 - 1)) as i32;
    let ny = ((y - top) as i64 * 65_535 / (height as i64 - 1)) as i32;

    let input = mouse_move_input(nx, ny);
    let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
    if sent != 1 {
        return Err(format!(
            "移动鼠标到 ({x}, {y}) 失败：系统没有接受这次输入（目标窗口可能属于更高权限的进程）"
        ));
    }
    Ok(())
}

/// 虚拟桌面（所有显示器合起来）的左上角与尺寸。
///
/// 多显示器下主屏左上角不一定是 (0,0)——左侧或上方的显示器会让坐标为负。
/// 而 `MOUSEEVENTF_ABSOLUTE` 的归一化基准是整个**虚拟桌面**，不是主屏；
/// 按主屏算，副屏上的坐标会整体偏移。
fn virtual_screen_rect() -> (i32, i32, i32, i32) {
    // SAFETY: `GetSystemMetrics` 是纯只读查询，无指针、无所有权。
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

pub fn left_click() -> WinResult<()> {
    let inputs = [mouse_input(MOUSEEVENTF_LEFTDOWN), mouse_input(MOUSEEVENTF_LEFTUP)];
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent != inputs.len() as u32 {
        return Err("发送鼠标事件失败".to_string());
    }
    Ok(())
}

pub fn send_ctrl_v() -> WinResult<()> {
    send_ctrl_key(VK_V, "粘贴快捷键")
}

/// Ctrl+A：仅用于诊断工具清空测试输入框。
pub fn send_ctrl_a() -> WinResult<()> {
    send_ctrl_key(VK_A, "全选快捷键")
}

/// Delete：仅用于诊断工具清空测试输入框。
pub fn send_delete() -> WinResult<()> {
    let inputs = [key_input(VK_DELETE, false), key_input(VK_DELETE, true)];
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent != inputs.len() as u32 {
        return Err("发送删除键失败".to_string());
    }
    Ok(())
}

fn send_ctrl_key(key: VIRTUAL_KEY, what: &str) -> WinResult<()> {
    let inputs = [
        key_input(VK_CONTROL, false),
        key_input(key, false),
        key_input(key, true),
        key_input(VK_CONTROL, true),
    ];
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent != inputs.len() as u32 {
        return Err(format!("发送{what}失败"));
    }
    Ok(())
}

/// 一个 Unicode 字符（UTF-16 码元）的按下 / 抬起事件。
fn unicode_input(unit: u16, up: bool) -> INPUT {
    let mut input = INPUT::default();
    input.r#type = INPUT_KEYBOARD;
    input.Anonymous.ki = KEYBDINPUT {
        // 走 `KEYEVENTF_UNICODE` 时 `wVk` 必须为 0，字符本身放在 `wScan` 里。
        wVk: VIRTUAL_KEY(0),
        wScan: unit,
        dwFlags: if up { KEYEVENTF_KEYUP | KEYEVENTF_UNICODE } else { KEYEVENTF_UNICODE },
        time: 0,
        dwExtraInfo: 0,
    };
    input
}

/// 逐字符输入文本，**不经过剪贴板**。
///
/// ## 为什么用 `KEYEVENTF_UNICODE` 而不是虚拟键码
///
/// 虚拟键码只能表达键盘上**真实存在**的键，中文根本没有对应的键。
/// `KEYEVENTF_UNICODE` 直接把一个 UTF-16 码元交给目标窗口的键盘消息处理，
/// 与输入法上屏走的是同一条路——这是逐字输入中文唯一可行的办法。
///
/// ## 为什么按 UTF-16 码元而不是 `char`
///
/// BMP 之外的字符（emoji 等）在 Windows 上是**代理对**，占两个码元。
/// 按 `char` 发会让目标窗口收到半个代理对，显示成一个方块。
///
/// ## 为什么字符之间要等
///
/// 客户端的搜索框是**联想式**的：收到一个字符就发一次查询、刷新下拉列表。
/// 连珠炮式地发完，联想请求会互相打断，下拉列表可能只按第一个字符的结果定格——
/// 而现象是"搜出来的东西不对"，不会让人想到是**输入太快**。
/// 间隔由调用方给（平台侧配置），这里不写死。
pub fn send_unicode_text(text: &str, interval: Duration) -> WinResult<()> {
    let units: Vec<u16> = text.encode_utf16().collect();
    for (index, unit) in units.iter().enumerate() {
        let inputs = [unicode_input(*unit, false), unicode_input(*unit, true)];
        let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent != inputs.len() as u32 {
            return Err(format!(
                "发送第 {} 个字符失败（共 {} 个）",
                index + 1,
                units.len()
            ));
        }
        if !interval.is_zero() && index + 1 < units.len() {
            std::thread::sleep(interval);
        }
    }
    Ok(())
}

pub fn send_enter() -> WinResult<()> {
    let inputs = [key_input(VK_RETURN, false), key_input(VK_RETURN, true)];
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent != inputs.len() as u32 {
        return Err("发送回车键失败".to_string());
    }
    Ok(())
}

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
