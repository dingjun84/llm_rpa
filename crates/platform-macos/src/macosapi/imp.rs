//! 真实 macOS 实现（仅在 `target_os = "macos"` 下编译）。
//!
//! 依赖：CoreGraphics（窗口列表 / 截屏 / 事件）、AppKit（前台应用 / 剪贴板）、
//! ApplicationServices（辅助功能信任查询）。

#![allow(non_snake_case)]

use std::ffi::{c_void, CStr};
use std::path::{Path, PathBuf};
use std::ptr;
use std::time::{Duration, Instant};

use automation_core::Rect;
use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
use core_foundation::boolean::{CFBoolean, CFBooleanRef};
use core_foundation::dictionary::{CFDictionaryGetValueIfPresent, CFDictionaryRef};
use core_foundation::number::{CFNumberGetValue, CFNumberRef, kCFNumberFloat64Type, kCFNumberSInt64Type};
use core_foundation::string::{CFStringCreateWithCString, CFStringGetCString, CFStringRef, kCFStringEncodingUTF8};
use core_graphics::display::{
    kCGNullWindowID, kCGWindowImageBoundsIgnoreFraming, kCGWindowListExcludeDesktopElements,
    kCGWindowListOptionOnScreenOnly, CGDisplayBounds, CGDisplayPixelsHigh, CGDisplayPixelsWide,
    CGMainDisplayID, CGWindowListCopyWindowInfo, CGWindowListCreateImage,
};
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGKeyCode, CGMouseButton,
    ScrollEventUnit,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use objc::runtime::{Class, Object};
use objc::{msg_send, sel, sel_impl};

use super::{fingerprint_of, CapturedFrame, MacResult, WindowInfo, WindowRef};

const POINTER_MIN_DURATION: Duration = Duration::from_millis(80);
const POINTER_MAX_DURATION: Duration = Duration::from_millis(900);
const POINTER_STEP_INTERVAL: Duration = Duration::from_millis(8);
const WHEEL_STEP_DELAY: Duration = Duration::from_millis(15);

// ── CoreFoundation 小工具 ───────────────────────────────────────────────

fn cfstr(s: &str) -> CFStringRef {
    unsafe {
        CFStringCreateWithCString(
            ptr::null(),
            std::ffi::CString::new(s).unwrap().as_ptr(),
            kCFStringEncodingUTF8,
        )
    }
}

fn dict_string(dict: CFDictionaryRef, key: &str) -> String {
    unsafe {
        let mut value: *const c_void = ptr::null();
        let k = cfstr(key);
        let present = CFDictionaryGetValueIfPresent(dict, k as *const c_void, &mut value);
        CFRelease(k as CFTypeRef);
        if present == 0 || value.is_null() {
            return String::new();
        }
        let mut buf = [0i8; 1024];
        if CFStringGetCString(
            value as CFStringRef,
            buf.as_mut_ptr(),
            buf.len() as isize,
            kCFStringEncodingUTF8,
        ) != 0 {
            CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
        } else {
            String::new()
        }
    }
}

fn dict_i64(dict: CFDictionaryRef, key: &str) -> Option<i64> {
    unsafe {
        let mut value: *const c_void = ptr::null();
        let k = cfstr(key);
        let present = CFDictionaryGetValueIfPresent(dict, k as *const c_void, &mut value);
        CFRelease(k as CFTypeRef);
        if present == 0 || value.is_null() {
            return None;
        }
        let mut out: i64 = 0;
        if CFNumberGetValue(value as CFNumberRef, kCFNumberSInt64Type, &mut out as *mut _ as *mut c_void) {
            Some(out)
        } else {
            None
        }
    }
}

fn dict_f64(dict: CFDictionaryRef, key: &str) -> Option<f64> {
    unsafe {
        let mut value: *const c_void = ptr::null();
        let k = cfstr(key);
        let present = CFDictionaryGetValueIfPresent(dict, k as *const c_void, &mut value);
        CFRelease(k as CFTypeRef);
        if present == 0 || value.is_null() {
            return None;
        }
        let mut out: f64 = 0.0;
        if CFNumberGetValue(value as CFNumberRef, kCFNumberFloat64Type, &mut out as *mut _ as *mut c_void) {
            Some(out)
        } else {
            None
        }
    }
}

fn dict_bool(dict: CFDictionaryRef, key: &str) -> bool {
    unsafe {
        let mut value: *const c_void = ptr::null();
        let k = cfstr(key);
        let present = CFDictionaryGetValueIfPresent(dict, k as *const c_void, &mut value);
        CFRelease(k as CFTypeRef);
        if present == 0 || value.is_null() {
            return false;
        }
        // core-foundation 0.10 不再导出 CFBooleanGetValue，改走 CFBoolean → bool。
        bool::from(CFBoolean::wrap_under_get_rule(value as CFBooleanRef))
    }
}

fn dict_bounds(dict: CFDictionaryRef) -> Option<Rect> {
    unsafe {
        let mut value: *const c_void = ptr::null();
        let k = cfstr("kCGWindowBounds");
        let present = CFDictionaryGetValueIfPresent(dict, k as *const c_void, &mut value);
        CFRelease(k as CFTypeRef);
        if present == 0 || value.is_null() {
            return None;
        }
        let bounds = value as CFDictionaryRef;
        Some(Rect {
            x: dict_f64(bounds, "X")? as i32,
            y: dict_f64(bounds, "Y")? as i32,
            width: dict_f64(bounds, "Width")? as i32,
            height: dict_f64(bounds, "Height")? as i32,
        })
    }
}

fn parse_window(dict: CFDictionaryRef) -> Option<WindowInfo> {
    let layer = dict_i64(dict, "kCGWindowLayer").unwrap_or(0);
    if layer != 0 {
        return None;
    }
    let id = dict_i64(dict, "kCGWindowNumber")? as u32;
    let owner_pid = dict_i64(dict, "kCGWindowOwnerPID").unwrap_or(0) as i32;
    let owner_name = dict_string(dict, "kCGWindowOwnerName");
    let title = dict_string(dict, "kCGWindowName");
    let rect = dict_bounds(dict)?;
    if rect.is_degenerate() {
        return None;
    }
    if title.is_empty() && owner_name.is_empty() {
        return None;
    }
    Some(WindowInfo {
        id,
        owner_pid,
        owner_name,
        title,
        rect,
        is_on_screen: dict_bool(dict, "kCGWindowIsOnscreen"),
        layer,
    })
}

pub fn list_visible_windows() -> Vec<WindowInfo> {
    unsafe {
        let info = CGWindowListCopyWindowInfo(
            kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
            kCGNullWindowID,
        );
        if info.is_null() {
            return Vec::new();
        }
        let len = core_foundation::array::CFArrayGetCount(info as _);
        let mut out = Vec::new();
        for i in 0..len {
            let item = core_foundation::array::CFArrayGetValueAtIndex(info as _, i);
            if item.is_null() {
                continue;
            }
            if let Some(w) = parse_window(item as CFDictionaryRef) {
                if w.is_on_screen {
                    out.push(w);
                }
            }
        }
        CFRelease(info as CFTypeRef);
        out.sort_by_key(|w| (w.rect.y, w.rect.x));
        out
    }
}

fn as_ref(info: &WindowInfo) -> WindowRef {
    WindowRef {
        id: info.id,
        owner_pid: info.owner_pid,
    }
}

pub fn find_window_by_owner_name(owner: &str) -> MacResult<WindowRef> {
    list_visible_windows()
        .into_iter()
        .find(|w| w.owner_name == owner)
        .map(|w| as_ref(&w))
        .ok_or_else(|| format!("未找到所有者名为「{owner}」的可见窗口"))
}

pub fn find_window_by_owner_in_process(owner: &str, exe: &Path) -> MacResult<WindowRef> {
    for w in list_visible_windows() {
        if w.owner_name != owner {
            continue;
        }
        let wr = as_ref(&w);
        if process_matches(wr, exe) {
            return Ok(wr);
        }
    }
    Err(format!(
        "未找到属于「{}」且所有者名为「{owner}」的可见窗口",
        exe.display()
    ))
}

pub fn find_window_by_title_prefix(prefix: &str) -> MacResult<WindowRef> {
    list_visible_windows()
        .into_iter()
        .find(|w| w.title.starts_with(prefix))
        .map(|w| as_ref(&w))
        .ok_or_else(|| format!("未找到标题以「{prefix}」开头的可见窗口"))
}

pub fn window_rect(w: WindowRef) -> MacResult<Rect> {
    list_visible_windows()
        .into_iter()
        .find(|info| info.id == w.id)
        .map(|info| info.rect)
        .ok_or_else(|| format!("窗口 {} 已不存在", w.id))
}

pub fn window_owner_name(w: WindowRef) -> String {
    list_visible_windows()
        .into_iter()
        .find(|info| info.id == w.id)
        .map(|info| info.owner_name)
        .unwrap_or_default()
}

pub fn window_title(w: WindowRef) -> String {
    list_visible_windows()
        .into_iter()
        .find(|info| info.id == w.id)
        .map(|info| info.title)
        .unwrap_or_default()
}

extern "C" {
    fn proc_pidpath(pid: i32, buffer: *mut i8, buffersize: u32) -> i32;
}

fn path_for_pid(pid: i32) -> MacResult<PathBuf> {
    if pid <= 0 {
        return Err("无效的进程 ID".into());
    }
    let mut buf = [0i8; 4096];
    let len = unsafe { proc_pidpath(pid, buf.as_mut_ptr(), buf.len() as u32) };
    if len <= 0 {
        return Err(format!("无法读取进程 {pid} 的可执行文件路径"));
    }
    let cstr = unsafe { CStr::from_ptr(buf.as_ptr()) };
    Ok(PathBuf::from(cstr.to_string_lossy().into_owned()))
}

pub fn window_process_path(w: WindowRef) -> MacResult<PathBuf> {
    path_for_pid(w.owner_pid)
}

pub fn process_matches(w: WindowRef, exe: &Path) -> bool {
    let Ok(actual) = window_process_path(w) else {
        return false;
    };
    let normalize = |path: &Path| {
        std::fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .into_owned()
    };
    let actual_s = normalize(&actual);
    let expected_s = normalize(exe);
    if actual_s == expected_s {
        return true;
    }
    if expected_s.ends_with(".app") {
        return actual_s.starts_with(&expected_s);
    }
    actual_s.starts_with(&expected_s) || expected_s.contains(&actual_s)
}

pub fn foreground_window() -> Option<WindowRef> {
    unsafe {
        let cls = Class::get("NSWorkspace")?;
        let workspace: *mut Object = msg_send![cls, sharedWorkspace];
        let app: *mut Object = msg_send![workspace, frontmostApplication];
        if app.is_null() {
            return None;
        }
        let pid: i32 = msg_send![app, processIdentifier];
        list_visible_windows()
            .into_iter()
            .find(|w| w.owner_pid == pid)
            .map(|w| as_ref(&w))
    }
}

pub fn same_window(a: WindowRef, b: WindowRef) -> bool {
    a.id == b.id
}

pub fn bring_to_foreground(w: WindowRef) -> MacResult<()> {
    unsafe {
        let cls = Class::get("NSWorkspace").ok_or("NSWorkspace 不可用")?;
        let workspace: *mut Object = msg_send![cls, sharedWorkspace];
        let running: *mut Object = msg_send![workspace, runningApplications];
        let count: usize = msg_send![running, count];
        let mut found: *mut Object = ptr::null_mut();
        for i in 0..count {
            let app: *mut Object = msg_send![running, objectAtIndex: i];
            let pid: i32 = msg_send![app, processIdentifier];
            if pid == w.owner_pid {
                found = app;
                break;
            }
        }
        if found.is_null() {
            return Err(format!("找不到 PID {} 对应的运行中应用", w.owner_pid));
        }
        // NSApplicationActivateIgnoringOtherApps = 1 << 1
        let options: u64 = 1 << 1;
        let ok: bool = msg_send![found, activateWithOptions: options];
        if !ok {
            return Err(
                "系统拒绝将目标应用带到前台（请确认已授予「辅助功能」权限）".into(),
            );
        }
        Ok(())
    }
}

pub fn wait_until_foreground(w: WindowRef, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(fg) = foreground_window() {
            if fg.owner_pid == w.owner_pid {
                return true;
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

pub fn resize_window(w: WindowRef, width: i32, height: i32) -> MacResult<Rect> {
    let owner = window_owner_name(w);
    if owner.is_empty() {
        return Err("无法读取窗口所有者名，拒绝调整尺寸".into());
    }
    // 转义 AppleScript 字符串中的引号
    let owner_esc = owner.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        r#"tell application "System Events"
  tell process "{owner_esc}"
    set frontWindow to first window whose value of attribute "AXMain" is true
    set size of frontWindow to {{{width}, {height}}}
  end tell
end tell"#
    );
    let status = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .status()
        .map_err(|err| format!("调用 osascript 调整窗口失败：{err}"))?;
    if !status.success() {
        return Err(format!(
            "调整窗口尺寸失败（退出码 {:?}）。请确认已授予「辅助功能」权限。",
            status.code()
        ));
    }
    std::thread::sleep(Duration::from_millis(80));
    window_rect(w)
}

pub fn is_responsive(w: WindowRef) -> bool {
    path_for_pid(w.owner_pid).is_ok() && window_rect(w).is_ok()
}

/// 主屏尺寸（**逻辑点** / Cocoa points）。
///
/// 与 `CGWindowBounds`、`CGEvent` 全局坐标同一单位。Retina 上不能用
/// `CGDisplayPixelsWide/High`（物理像素）做 Y 翻转，否则会偏约 scale 倍。
pub fn primary_screen_size() -> (u32, u32) {
    unsafe {
        let bounds = CGDisplayBounds(CGMainDisplayID());
        let w = bounds.size.width.round().max(0.0) as u32;
        let h = bounds.size.height.round().max(0.0) as u32;
        (w, h)
    }
}

/// 主屏物理像素尺寸。截图像素缓冲等仍可能需要；鼠标坐标请用 [`primary_screen_size`]。
pub fn primary_screen_pixel_size() -> (u32, u32) {
    unsafe {
        let id = CGMainDisplayID();
        (
            CGDisplayPixelsWide(id) as u32,
            CGDisplayPixelsHigh(id) as u32,
        )
    }
}

fn accessibility_required_error() -> String {
    "未授予「辅助功能」权限，系统会静默忽略鼠标移动与点击。请到「系统设置 → 隐私与安全性 → 辅助功能」中启用本应用；若还需要截屏/找窗口，请同时开启「屏幕录制」。"
        .into()
}

fn ensure_accessibility() -> MacResult<()> {
    if accessibility_trusted() {
        Ok(())
    } else {
        Err(accessibility_required_error())
    }
}

pub fn primary_scale_factor() -> f32 {
    // 简化：Retina 通常为 2.0。用像素宽 / 点宽更准确，但需要 CGDisplayMode。
    // 这里用 NSScreen.backingScaleFactor。
    unsafe {
        let cls = match Class::get("NSScreen") {
            Some(c) => c,
            None => return 1.0,
        };
        let screen: *mut Object = msg_send![cls, mainScreen];
        if screen.is_null() {
            return 1.0;
        }
        let scale: f64 = msg_send![screen, backingScaleFactor];
        if scale.is_finite() && scale > 0.0 {
            scale as f32
        } else {
            1.0
        }
    }
}

/// AppKit 矩形（与 `NSScreen.frame` 同布局）。
#[repr(C)]
#[derive(Clone, Copy)]
struct NsRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

/// 某点所在显示器的缩放（点坐标与 `kCGWindowBounds` 相同：主屏左上原点）。
///
/// ★ 标定与任务挑选必须用这个，不能用 [`primary_scale_factor`]：外接屏 1.0、内建 2.0 时
/// 读主屏会把窗口记错档。找不到命中屏时退回主屏缩放。
pub fn scale_factor_at_point(x: f64, y_top_left: f64) -> f32 {
    unsafe {
        let cls = match Class::get("NSScreen") {
            Some(c) => c,
            None => return primary_scale_factor(),
        };
        let main: *mut Object = msg_send![cls, mainScreen];
        if main.is_null() {
            return primary_scale_factor();
        }
        let main_frame: NsRect = msg_send![main, frame];
        // CGWindowBounds 风格（左上）→ Cocoa（左下，原点在主屏左下）
        let cocoa_y = main_frame.height - y_top_left;
        let screens: *mut Object = msg_send![cls, screens];
        if screens.is_null() {
            return primary_scale_factor();
        }
        let count: usize = msg_send![screens, count];
        for i in 0..count {
            let screen: *mut Object = msg_send![screens, objectAtIndex: i];
            if screen.is_null() {
                continue;
            }
            let frame: NsRect = msg_send![screen, frame];
            if x >= frame.x
                && x < frame.x + frame.width
                && cocoa_y >= frame.y
                && cocoa_y < frame.y + frame.height
            {
                let scale: f64 = msg_send![screen, backingScaleFactor];
                if scale.is_finite() && scale > 0.0 {
                    return scale as f32;
                }
            }
        }
        primary_scale_factor()
    }
}

/// 窗口矩形中心所在显示器的缩放。
pub fn scale_factor_for_rect(rect: Rect) -> f32 {
    let cx = rect.x as f64 + (rect.width as f64) * 0.5;
    let cy = rect.y as f64 + (rect.height as f64) * 0.5;
    scale_factor_at_point(cx, cy)
}

/// 窗口所在显示器的逻辑点尺寸 + 缩放（与 [`scale_factor_for_rect`] 同一套屏）。
pub fn metrics_for_rect(rect: Rect) -> (u32, u32, f32) {
    let cx = rect.x as f64 + (rect.width as f64) * 0.5;
    let cy = rect.y as f64 + (rect.height as f64) * 0.5;
    unsafe {
        let cls = match Class::get("NSScreen") {
            Some(c) => c,
            None => {
                let (w, h) = primary_screen_size();
                return (w, h, primary_scale_factor());
            }
        };
        let main: *mut Object = msg_send![cls, mainScreen];
        if main.is_null() {
            let (w, h) = primary_screen_size();
            return (w, h, primary_scale_factor());
        }
        let main_frame: NsRect = msg_send![main, frame];
        let cocoa_y = main_frame.height - cy;
        let screens: *mut Object = msg_send![cls, screens];
        if screens.is_null() {
            let (w, h) = primary_screen_size();
            return (w, h, primary_scale_factor());
        }
        let count: usize = msg_send![screens, count];
        for i in 0..count {
            let screen: *mut Object = msg_send![screens, objectAtIndex: i];
            if screen.is_null() {
                continue;
            }
            let frame: NsRect = msg_send![screen, frame];
            if cx >= frame.x
                && cx < frame.x + frame.width
                && cocoa_y >= frame.y
                && cocoa_y < frame.y + frame.height
            {
                let scale: f64 = msg_send![screen, backingScaleFactor];
                let w = frame.width.round().max(0.0) as u32;
                let h = frame.height.round().max(0.0) as u32;
                let scale = if scale.is_finite() && scale > 0.0 {
                    scale as f32
                } else {
                    primary_scale_factor()
                };
                return (w, h, scale);
            }
        }
    }
    let (w, h) = primary_screen_size();
    (w, h, primary_scale_factor())
}

pub fn capture_region(region: Rect) -> MacResult<CapturedFrame> {
    if region.width <= 0 || region.height <= 0 {
        return Err("捕获区域尺寸无效".into());
    }
    // 实测：本机用与 kCGWindowBounds 相同的左上原点矩形即可截到正确窗口内容。
    // （文档写 Quartz 左下；若再翻转 Y，预览会与真窗口错位。）
    let rect = CGRect::new(
        &CGPoint::new(region.x as f64, region.y as f64),
        &CGSize::new(region.width as f64, region.height as f64),
    );
    unsafe {
        let image = CGWindowListCreateImage(
            rect,
            kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
            kCGNullWindowID,
            kCGWindowImageBoundsIgnoreFraming,
        );
        if image.is_null() {
            return Err(
                "截屏失败：请到「系统设置 → 隐私与安全性 → 屏幕录制」中授权本程序".into(),
            );
        }
        let result = cgimage_to_bgra(image as *const c_void);
        // CGImageRelease
        extern "C" {
            fn CGImageRelease(image: *const c_void);
        }
        CGImageRelease(image as *const c_void);
        result
    }
}

unsafe fn cgimage_to_bgra(image: *const c_void) -> MacResult<CapturedFrame> {
    extern "C" {
        fn CGImageGetWidth(image: *const c_void) -> usize;
        fn CGImageGetHeight(image: *const c_void) -> usize;
        fn CGImageGetDataProvider(image: *const c_void) -> *const c_void;
        fn CGDataProviderCopyData(provider: *const c_void) -> *const c_void;
        fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
        fn CFDataGetLength(data: *const c_void) -> isize;
        fn CGImageGetBitsPerPixel(image: *const c_void) -> usize;
        fn CGImageGetBytesPerRow(image: *const c_void) -> usize;
    }

    let width = CGImageGetWidth(image) as u32;
    let height = CGImageGetHeight(image) as u32;
    if width == 0 || height == 0 {
        return Err("截到的画面尺寸为 0".into());
    }
    let provider = CGImageGetDataProvider(image);
    if provider.is_null() {
        return Err("无法读取截图像素".into());
    }
    let data = CGDataProviderCopyData(provider);
    if data.is_null() {
        return Err("无法复制截图像素".into());
    }
    let ptr = CFDataGetBytePtr(data);
    let len = CFDataGetLength(data) as usize;
    let bpp = CGImageGetBitsPerPixel(image);
    let stride = CGImageGetBytesPerRow(image);
    if ptr.is_null() || bpp != 32 {
        CFRelease(data as CFTypeRef);
        return Err(format!("不支持的像素格式（bpp={bpp}）"));
    }
    let src = std::slice::from_raw_parts(ptr, len);
    let mut pixels = vec![0u8; (width as usize) * (height as usize) * 4];
    for y in 0..height as usize {
        let src_row = &src[y * stride..y * stride + (width as usize) * 4];
        let dst_row = &mut pixels[y * (width as usize) * 4..(y + 1) * (width as usize) * 4];
        // macOS 通常是 BGRA 或 RGBA；按字节序拷贝，下游 vision 按 BGRA 解释。
        // CGImage 在小端上常见为 BGRA。
        dst_row.copy_from_slice(src_row);
    }
    CFRelease(data as CFTypeRef);
    let fingerprint = fingerprint_of(&pixels, width, height);
    Ok(CapturedFrame {
        pixels,
        width,
        height,
        fingerprint,
    })
}

pub fn cursor_position() -> MacResult<(i32, i32)> {
    let source = event_source()?;
    let event = CGEvent::new(source).map_err(|_| "无法读取光标位置".to_string())?;
    let loc = event.location();
    // 与 kCGWindowBounds 同一套：主屏左上为原点的全局坐标。
    // 若再按「Quartz 左下」做一次 Y 翻转，相对轨迹自检仍能画圆（读写一致），
    // 但按窗口矩形算出来的绝对目标会整体偏掉——正是「绿框对、鼠标偏」的症状。
    Ok((loc.x.round() as i32, loc.y.round() as i32))
}

pub fn window_from_point(x: i32, y: i32) -> Option<WindowRef> {
    // 与 Windows `WindowFromPoint` 对齐：取光标下**最具体**的那扇窗，不要求先获焦。
    // CGWindowList 的前后序在多屏/全屏场景下不可靠；包含该点的窗口里取**面积最小**的，
    // 避免命中背后那块几乎铺满屏的大窗（用户感觉像「取到了最大的窗口」）。
    let mut best: Option<WindowInfo> = None;
    let mut best_area: i64 = i64::MAX;
    for w in list_visible_windows() {
        if !w.is_on_screen {
            continue;
        }
        if x < w.rect.x
            || y < w.rect.y
            || x >= w.rect.x + w.rect.width
            || y >= w.rect.y + w.rect.height
        {
            continue;
        }
        let area = (w.rect.width as i64).saturating_mul(w.rect.height as i64);
        if area <= 0 {
            continue;
        }
        if area < best_area {
            best_area = area;
            best = Some(w);
        }
    }
    best.map(|w| as_ref(&w))
}

fn to_quartz_point(x: i32, y: i32) -> CGPoint {
    CGPoint::new(x as f64, y as f64)
}

fn event_source() -> MacResult<CGEventSource> {
    CGEventSource::new(CGEventSourceStateID::CombinedSessionState).map_err(|_| {
        "无法创建事件源：请到「系统设置 → 隐私与安全性 → 辅助功能」中授权本程序".into()
    })
}

pub fn move_cursor(x: i32, y: i32, speed_px_per_sec: f64) -> MacResult<()> {
    ensure_accessibility()?;
    let (mut sx, mut sy) = cursor_position()?;
    let mut dx = (x - sx) as f64;
    let mut dy = (y - sy) as f64;
    let mut distance = (dx * dx + dy * dy).sqrt();
    // 人若在确认点击前挪开了鼠标，必须再滑回去；距离太近时也绕一下，
    // 避免「瞬移」看起来像没动。
    const MIN_VISIBLE: f64 = 56.0;
    if distance < 1.0 {
        // 已在目标上：先挪开再滑回，保证第二下「定位并点击」仍看得见轨迹。
        let lift_x = sx + MIN_VISIBLE as i32;
        let lift_y = sy;
        warp_cursor(lift_x, lift_y)?;
        std::thread::sleep(POINTER_STEP_INTERVAL);
        sx = lift_x;
        sy = lift_y;
        dx = (x - sx) as f64;
        dy = (y - sy) as f64;
        distance = (dx * dx + dy * dy).sqrt();
    } else if distance < MIN_VISIBLE {
        let ux = dx / distance;
        let uy = dy / distance;
        let mid_x = (sx as f64 + ux * (distance * 0.5) - uy * (MIN_VISIBLE * 0.35)).round() as i32;
        let mid_y = (sy as f64 + uy * (distance * 0.5) + ux * (MIN_VISIBLE * 0.35)).round() as i32;
        warp_cursor(mid_x, mid_y)?;
        std::thread::sleep(POINTER_STEP_INTERVAL);
        sx = mid_x;
        sy = mid_y;
        dx = (x - sx) as f64;
        dy = (y - sy) as f64;
        distance = (dx * dx + dy * dy).sqrt().max(1.0);
    }
    let speed = speed_px_per_sec.max(1.0);
    let mut duration = Duration::from_secs_f64(distance / speed);
    // 下限抬高：短距离也要看得出在滑。
    let min_dur = POINTER_MIN_DURATION.max(Duration::from_millis(280));
    duration = duration.clamp(min_dur, POINTER_MAX_DURATION);
    let steps = ((duration.as_secs_f64() / POINTER_STEP_INTERVAL.as_secs_f64()).ceil() as usize)
        .max(12);
    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        warp_cursor(
            (sx as f64 + dx * t).round() as i32,
            (sy as f64 + dy * t).round() as i32,
        )?;
        if i < steps {
            std::thread::sleep(POINTER_STEP_INTERVAL);
        }
    }
    Ok(())
}

fn warp_cursor(x: i32, y: i32) -> MacResult<()> {
    let source = event_source()?;
    let point = to_quartz_point(x, y);
    let event =
        CGEvent::new_mouse_event(source, CGEventType::MouseMoved, point, CGMouseButton::Left)
            .map_err(|_| "创建鼠标移动事件失败".to_string())?;
    event.post(CGEventTapLocation::HID);
    event.post(CGEventTapLocation::Session);
    Ok(())
}


const CIRCLE_MIN_DURATION: Duration = Duration::from_millis(400);
const CIRCLE_MAX_DURATION: Duration = Duration::from_secs(8);
const CIRCLE_MIN_STEPS: u32 = 24;
const CIRCLE_EXTRA_TURNS: f64 = 0.25;

/// 「画圆」自检结果：圆心 / 半径 / 步数 / 时长 / 实测终点。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CircleTrace {
    pub center: (i32, i32),
    pub radius: i32,
    pub steps: u32,
    pub duration: Duration,
    pub end: (i32, i32),
}

impl CircleTrace {
    pub fn end_distance_px(&self) -> i32 {
        let dx = (self.end.0 - self.center.0) as f64;
        let dy = (self.end.1 - self.center.1) as f64;
        (dx * dx + dy * dy).sqrt().round() as i32
    }
}

fn circle_points(center: (i32, i32), radius: i32, steps: u32, turns: f64) -> Vec<(i32, i32)> {
    if steps == 0 {
        return Vec::new();
    }
    let (cx, cy) = center;
    let radius = radius as f64;
    (1..=steps)
        .map(|step| {
            let angle = std::f64::consts::TAU * turns * step as f64 / steps as f64;
            (
                cx + (radius * angle.cos()).round() as i32,
                cy + (radius * angle.sin()).round() as i32,
            )
        })
        .collect()
}

/// 以当前坐标系（逻辑点、原点在主屏左上）走圆周，供「鼠标轨迹自检」使用。
pub fn move_cursor_circle(
    center: (i32, i32),
    radius: i32,
    speed_px_per_sec: f64,
) -> MacResult<CircleTrace> {
    ensure_accessibility()?;
    if radius <= 0 {
        return Err(format!("圆的半径必须是正数，收到 {radius}"));
    }
    if !speed_px_per_sec.is_finite() || speed_px_per_sec <= 0.0 {
        return Err(format!("光标速度必须是正数，收到 {speed_px_per_sec}"));
    }

    let (cx, cy) = center;
    move_cursor(cx + radius, cy, speed_px_per_sec)?;

    let turns = 1.0 + CIRCLE_EXTRA_TURNS;
    let path = std::f64::consts::TAU * radius as f64 * turns;
    let duration = Duration::from_secs_f64(path / speed_px_per_sec)
        .clamp(CIRCLE_MIN_DURATION, CIRCLE_MAX_DURATION);
    let steps = ((duration.as_secs_f64() / POINTER_STEP_INTERVAL.as_secs_f64()).round() as u32)
        .max(CIRCLE_MIN_STEPS);
    let step_delay = duration / steps;

    let points = circle_points(center, radius, steps, turns);
    let last = points.len();
    for (index, (x, y)) in points.into_iter().enumerate() {
        warp_cursor(x, y)?;
        if index + 1 < last {
            std::thread::sleep(step_delay);
        }
    }

    let end = cursor_position()?;
    Ok(CircleTrace {
        center,
        radius,
        steps,
        duration,
        end,
    })
}

pub fn left_click() -> MacResult<()> {
    ensure_accessibility()?;
    let source = event_source()?;
    let (x, y) = cursor_position()?;
    let point = to_quartz_point(x, y);
    let down = CGEvent::new_mouse_event(
        source.clone(),
        CGEventType::LeftMouseDown,
        point,
        CGMouseButton::Left,
    )
    .map_err(|_| "创建鼠标按下事件失败".to_string())?;
    let up =
        CGEvent::new_mouse_event(source, CGEventType::LeftMouseUp, point, CGMouseButton::Left)
            .map_err(|_| "创建鼠标抬起事件失败".to_string())?;
    down.post(CGEventTapLocation::HID);
    up.post(CGEventTapLocation::HID);
    Ok(())
}

pub fn scroll_wheel(notches: i32) -> MacResult<()> {
    if notches == 0 {
        return Ok(());
    }
    let source = event_source()?;
    let step = if notches > 0 { -1 } else { 1 };
    for index in 0..notches.unsigned_abs() {
        let event =
            CGEvent::new_scroll_event(source.clone(), ScrollEventUnit::LINE, 1, step, 0, 0)
                .map_err(|_| format!("创建滚轮事件失败（第 {} 格）", index + 1))?;
        event.post(CGEventTapLocation::HID);
        if index + 1 < notches.unsigned_abs() {
            std::thread::sleep(WHEEL_STEP_DELAY);
        }
    }
    Ok(())
}

pub fn set_clipboard_text(text: &str) -> MacResult<()> {
    unsafe {
        let pb_cls = Class::get("NSPasteboard").ok_or("NSPasteboard 不可用")?;
        let pb: *mut Object = msg_send![pb_cls, generalPasteboard];
        let _: usize = msg_send![pb, clearContents];
        let str_cls = Class::get("NSString").ok_or("NSString 不可用")?;
        let ns_text: *mut Object = msg_send![str_cls, stringWithUTF8String: std::ffi::CString::new(text).map_err(|e| e.to_string())?.as_ptr()];
        let type_str: *mut Object = msg_send![str_cls, stringWithUTF8String: b"public.utf8-plain-text\0".as_ptr()];
        let ok: bool = msg_send![pb, setString: ns_text forType: type_str];
        if !ok {
            return Err("写入剪贴板失败".into());
        }
        Ok(())
    }
}

pub fn clear_clipboard() -> MacResult<()> {
    unsafe {
        let pb_cls = Class::get("NSPasteboard").ok_or("NSPasteboard 不可用")?;
        let pb: *mut Object = msg_send![pb_cls, generalPasteboard];
        let _: usize = msg_send![pb, clearContents];
        Ok(())
    }
}

pub fn wait_for_clipboard_release(_poll: Duration, timeout: Duration) -> bool {
    std::thread::sleep(timeout.min(Duration::from_millis(200)));
    false
}

fn post_key(key: CGKeyCode, flags: CGEventFlags, down: bool) -> MacResult<()> {
    let source = event_source()?;
    let event = CGEvent::new_keyboard_event(source, key, down).map_err(|_| "创建键盘事件失败".to_string())?;
    event.set_flags(flags);
    event.post(CGEventTapLocation::HID);
    Ok(())
}

fn chord(key: CGKeyCode, flags: CGEventFlags) -> MacResult<()> {
    post_key(key, flags, true)?;
    post_key(key, flags, false)?;
    Ok(())
}

pub fn send_cmd_v() -> MacResult<()> {
    chord(0x09, CGEventFlags::CGEventFlagCommand)
}

pub fn send_cmd_a() -> MacResult<()> {
    chord(0x00, CGEventFlags::CGEventFlagCommand)
}

pub fn send_delete() -> MacResult<()> {
    chord(0x33, CGEventFlags::empty())
}

pub fn send_unicode_text(text: &str, interval: Duration) -> MacResult<()> {
    // 逐字用 CGEventKeyboardSetUnicodeString 注入，直接追加进输入框。
    //
    // 以前走「每字写剪贴板 + Cmd+V」：联想式搜索框常会在粘贴后选中刚贴的字，
    // 下一字再 Cmd+V 就变成**覆盖**而不是追加——看起来就像「只打进去一个字」。
    // Unicode 键盘事件没有这个问题，也能驱动中文。
    ensure_accessibility()?;
    let source = event_source()?;
    // 联想搜索框要一点间隔；配置里 0 时仍给一个下限，避免连发被客户端吞字。
    let gap = if interval.is_zero() {
        Duration::from_millis(40)
    } else {
        interval.max(Duration::from_millis(40))
    };
    let total = text.chars().count();
    for (index, ch) in text.chars().enumerate() {
        let unit = ch.to_string();
        let down = CGEvent::new_keyboard_event(source.clone(), 0, true)
            .map_err(|_| format!("创建键盘按下事件失败（第 {} 字）", index + 1))?;
        down.set_string(&unit);
        down.post(CGEventTapLocation::HID);
        let up = CGEvent::new_keyboard_event(source.clone(), 0, false)
            .map_err(|_| format!("创建键盘抬起事件失败（第 {} 字）", index + 1))?;
        up.set_string(&unit);
        up.post(CGEventTapLocation::HID);
        if index + 1 < total {
            std::thread::sleep(gap);
        }
    }
    Ok(())
}

pub fn launch_process(exe: &Path) -> MacResult<()> {
    let path_str = exe.to_string_lossy();
    if path_str.ends_with(".app") || exe.extension().and_then(|e| e.to_str()) == Some("app") {
        let status = std::process::Command::new("open")
            .arg(exe)
            .status()
            .map_err(|err| format!("启动应用失败：{err}"))?;
        if !status.success() {
            return Err(format!("open 启动失败（退出码 {:?}）", status.code()));
        }
        return Ok(());
    }
    std::process::Command::new(exe)
        .spawn()
        .map_err(|err| format!("启动进程失败：{err}"))?;
    Ok(())
}

pub fn accessibility_trusted() -> bool {
    extern "C" {
        fn AXIsProcessTrusted() -> u8;
    }
    unsafe { AXIsProcessTrusted() != 0 }
}
