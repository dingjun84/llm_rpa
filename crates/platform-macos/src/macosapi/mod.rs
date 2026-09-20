//! macOS 系统调用的最小封装。
//!
//! 所有 `unsafe` / Objective-C 调用都集中在本模块，上层只看到 `Result`。
//! 非 macOS 目标提供同签名桩实现，使本 crate 能在 CI（Linux）上编译并通过契约测试中的纯逻辑部分。

use std::path::{Path, PathBuf};
use std::time::Duration;

use automation_core::Rect;
use sha2::{Digest, Sha256};

pub type MacResult<T> = Result<T, String>;

/// 捕获到的一帧画面（BGRA，自上而下）——与 Windows 侧像素约定一致。
pub struct CapturedFrame {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub fingerprint: String,
}

/// 一个可见顶层窗口的快照。
#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub id: u32,
    pub owner_pid: i32,
    pub owner_name: String,
    pub title: String,
    pub rect: Rect,
    pub is_on_screen: bool,
    pub layer: i64,
}

/// 窗口身份：CGWindowID + 所有者 PID（用于前台校验与进程路径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowRef {
    pub id: u32,
    pub owner_pid: i32,
}

pub fn fingerprint_of(pixels: &[u8], width: u32, height: u32) -> String {
    let mut hasher = Sha256::new();
    hasher.update(width.to_le_bytes());
    hasher.update(height.to_le_bytes());
    hasher.update(pixels);
    hex_lower(&hasher.finalize())
}

pub fn file_sha256(path: &Path) -> MacResult<String> {
    let mut file = std::fs::File::open(path).map_err(|err| format!("读取文件失败：{err}"))?;
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

#[cfg(target_os = "macos")]
mod imp;

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;

    fn unsupported() -> String {
        "platform-macos 只能在 macOS 上执行真实桌面操作".into()
    }

    pub fn list_visible_windows() -> Vec<WindowInfo> {
        Vec::new()
    }

    pub fn find_window_by_owner_name(owner: &str) -> MacResult<WindowRef> {
        let _ = owner;
        Err(unsupported())
    }

    pub fn find_window_by_owner_in_process(owner: &str, exe: &Path) -> MacResult<WindowRef> {
        let _ = (owner, exe);
        Err(unsupported())
    }

    pub fn find_window_by_title_prefix(prefix: &str) -> MacResult<WindowRef> {
        let _ = prefix;
        Err(unsupported())
    }

    pub fn window_rect(_w: WindowRef) -> MacResult<Rect> {
        Err(unsupported())
    }

    pub fn window_owner_name(_w: WindowRef) -> String {
        String::new()
    }

    pub fn window_title(_w: WindowRef) -> String {
        String::new()
    }

    pub fn window_process_path(_w: WindowRef) -> MacResult<PathBuf> {
        Err(unsupported())
    }

    pub fn process_matches(_w: WindowRef, _exe: &Path) -> bool {
        false
    }

    pub fn foreground_window() -> Option<WindowRef> {
        None
    }

    pub fn same_window(a: WindowRef, b: WindowRef) -> bool {
        a == b
    }

    pub fn bring_to_foreground(_w: WindowRef) -> MacResult<()> {
        Err(unsupported())
    }

    pub fn wait_until_foreground(_w: WindowRef, _timeout: Duration) -> bool {
        false
    }

    pub fn resize_window(_w: WindowRef, _width: i32, _height: i32) -> MacResult<Rect> {
        Err(unsupported())
    }

    pub fn is_responsive(_w: WindowRef) -> bool {
        true
    }

    pub fn primary_screen_size() -> (u32, u32) {
        (0, 0)
    }

    pub fn primary_scale_factor() -> f32 {
        1.0
    }

    pub fn capture_region(_region: Rect) -> MacResult<CapturedFrame> {
        Err(unsupported())
    }

    pub fn cursor_position() -> MacResult<(i32, i32)> {
        Err(unsupported())
    }

    pub fn window_from_point(_x: i32, _y: i32) -> Option<WindowRef> {
        None
    }

    pub fn move_cursor(_x: i32, _y: i32, _speed_px_per_sec: f64) -> MacResult<()> {
        Err(unsupported())
    }

    pub fn left_click() -> MacResult<()> {
        Err(unsupported())
    }

    pub fn scroll_wheel(_notches: i32) -> MacResult<()> {
        Err(unsupported())
    }

    pub fn set_clipboard_text(_text: &str) -> MacResult<()> {
        Err(unsupported())
    }

    pub fn clear_clipboard() -> MacResult<()> {
        Err(unsupported())
    }

    pub fn wait_for_clipboard_release(_poll: Duration, _timeout: Duration) -> bool {
        false
    }

    pub fn send_cmd_v() -> MacResult<()> {
        Err(unsupported())
    }

    pub fn send_cmd_a() -> MacResult<()> {
        Err(unsupported())
    }

    pub fn send_delete() -> MacResult<()> {
        Err(unsupported())
    }

    pub fn send_enter() -> MacResult<()> {
        Err(unsupported())
    }

    pub fn send_unicode_text(_text: &str, _interval: Duration) -> MacResult<()> {
        Err(unsupported())
    }

    pub fn launch_process(_exe: &Path) -> MacResult<()> {
        Err(unsupported())
    }

    pub fn accessibility_trusted() -> bool {
        false
    }
}

pub use imp::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_deterministic_and_content_sensitive() {
        let a = vec![1u8, 2, 3, 4];
        let b = vec![1u8, 2, 3, 5];
        assert_eq!(fingerprint_of(&a, 2, 2), fingerprint_of(&a, 2, 2));
        assert_ne!(fingerprint_of(&a, 2, 2), fingerprint_of(&b, 2, 2));
        assert_ne!(fingerprint_of(&a, 2, 2), fingerprint_of(&a, 4, 1));
        assert_eq!(fingerprint_of(&a, 2, 2).len(), 64);
    }
}
