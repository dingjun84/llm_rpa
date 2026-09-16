//! Win32 调用的最小封装。
//!
//! 所有 `unsafe` 都集中在本模块，上层只看到 `Result`。
//! 这里不做任何策略判断，策略在 [`crate::desktop`] 中。

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::time::{Duration, Instant};

use automation_core::Rect;
use sha2::{Digest, Sha256};
use windows::core::{BOOL, PCWSTR};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HWND, LPARAM};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    GetDeviceCaps, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    HGDIOBJ, LOGPIXELSX, SRCCOPY,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetOpenClipboardWindow, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
};
use windows::Win32::System::Threading::{
    CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEINPUT, VIRTUAL_KEY, VK_A, VK_CONTROL,
    VK_DELETE, VK_RETURN, VK_V,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowW, GetClassNameW, GetForegroundWindow, GetSystemMetrics, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, IsIconic, IsWindowVisible, SetCursorPos,
    SetForegroundWindow, ShowWindow, SM_CXSCREEN, SM_CYSCREEN, SW_RESTORE,
};

/// `CF_UNICODETEXT`：Win32 中稳定的剪贴板格式常量。
const CF_UNICODETEXT_FORMAT: u32 = 13;

pub type WinResult<T> = Result<T, String>;

fn wide(text: &str) -> Vec<u16> {
    std::ffi::OsStr::new(text).encode_wide().chain(std::iter::once(0)).collect()
}

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

pub fn find_window_by_class(class_name: &str) -> WinResult<HWND> {
    let class = wide(class_name);
    let hwnd = unsafe { FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null()) }
        .map_err(|err| format!("按类名查找窗口失败：{err}"))?;
    if hwnd.0.is_null() {
        return Err(format!("未找到类名为「{class_name}」的窗口"));
    }
    Ok(hwnd)
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
    let mut buffer = vec![0u16; 256];
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

/// 把窗口置于前台。先尝试还原最小化窗口，再设置前台。
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
    let mut input = INPUT::default();
    input.r#type = INPUT_MOUSE;
    input.Anonymous.mi = MOUSEINPUT {
        dx: 0,
        dy: 0,
        mouseData: 0,
        dwFlags: flags,
        time: 0,
        dwExtraInfo: 0,
    };
    input
}

pub fn move_cursor(x: i32, y: i32) -> WinResult<()> {
    unsafe { SetCursorPos(x, y) }.map_err(|err| format!("移动鼠标失败：{err}"))
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
