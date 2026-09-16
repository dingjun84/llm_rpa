//! `DesktopPlatform` 的 Windows 实现。

use std::sync::Mutex;
use std::time::Duration;

use automation_core::{
    AutomationError, DesktopPlatform, Point, Rect, ScreenMetrics, Screenshot,
};
use windows::Win32::Foundation::HWND;

use crate::config::{WindowMatcher, WindowsDesktopConfig};
use crate::winapi::{self, WinResult};

/// 轮询"剪贴板是否被取用"的间隔。
const CLIPBOARD_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// 没能观察到剪贴板被占用时，退化的保守等待时长。
const CLIPBOARD_SETTLE_FALLBACK: Duration = Duration::from_millis(200);

/// 真实平台实现。
///
/// 不注入、不 Hook、不读取其他进程内存；所有输入操作都先校验前台窗口。
pub struct WindowsDesktop {
    config: WindowsDesktopConfig,
    /// 最近一次成功定位到的窗口句柄，以 `isize` 保存以保持 `Send + Sync`。
    target: Mutex<Option<isize>>,
}

impl WindowsDesktop {
    pub fn new(config: WindowsDesktopConfig) -> Self {
        Self { config, target: Mutex::new(None) }
    }

    pub fn config(&self) -> &WindowsDesktopConfig {
        &self.config
    }

    /// 只读预览：定位目标窗口并整窗截屏，**不改变焦点、不产生任何输入事件**。
    ///
    /// 与 [`DesktopPlatform::focus_wecom`] 的关键区别是它**不会把窗口带到前台**。
    /// 区域标定只是要"看一眼窗口长什么样"，没必要抢焦点——而且往往也抢不到：
    /// Windows 的前台锁定策略会拒绝一个自身不在前台的进程调用 `SetForegroundWindow`。
    ///
    /// 捕获范围严格限制在已定位窗口的边界内，因此不会扫描整屏。
    /// 窗口被其它窗口遮挡或有一部分在屏幕外时，那部分会是黑的——
    /// 这是刻意的：本工具不做屏幕内容"猜测"。
    pub fn preview(&self) -> Result<(Rect, Screenshot), AutomationError> {
        let hwnd = self.locate()?;
        let rect = winapi::window_rect(hwnd).map_err(AutomationError::Platform)?;
        if rect.is_degenerate() {
            return Err(AutomationError::ClientNotReady);
        }
        let frame = winapi::capture_region(rect).map_err(AutomationError::Platform)?;
        Ok((
            rect,
            Screenshot {
                pixels: frame.pixels,
                width: frame.width,
                height: frame.height,
                captured_at: std::time::SystemTime::now(),
                fingerprint: frame.fingerprint,
            },
        ))
    }

    fn locate(&self) -> Result<HWND, AutomationError> {
        let found: WinResult<HWND> = match &self.config.window_matcher {
            WindowMatcher::ClassName(class) => winapi::find_window_by_class(class),
            WindowMatcher::TitlePrefix(prefix) => winapi::find_window_by_title_prefix(prefix),
        };
        match found {
            Ok(hwnd) => {
                *self.target.lock().unwrap() = Some(hwnd.0 as isize);
                Ok(hwnd)
            }
            Err(detail) => {
                // 保留诊断信息，但对外返回稳定的错误码。
                eprintln!("[platform-windows] 定位窗口失败：{detail}");
                Err(AutomationError::ClientNotReady)
            }
        }
    }

    fn current_target(&self) -> Result<HWND, AutomationError> {
        let stored = *self.target.lock().unwrap();
        match stored {
            Some(value) => Ok(HWND(value as *mut std::ffi::c_void)),
            None => Err(AutomationError::ClientNotReady),
        }
    }

    /// 点击前的守卫：前台窗口必须是目标窗口，且窗口边界与标定一致。
    fn verify_guard(&self, expected_window: Rect) -> Result<HWND, AutomationError> {
        let hwnd = self.current_target()?;
        if !winapi::same_window(winapi::foreground_window(), hwnd) {
            return Err(AutomationError::ClientNotReady);
        }
        let current = winapi::window_rect(hwnd).map_err(AutomationError::Platform)?;
        if current != expected_window {
            return Err(AutomationError::ScreenChanged);
        }
        Ok(hwnd)
    }

    fn ensure_region_inside_window(&self, region: Rect) -> Result<(), AutomationError> {
        // ⚠️ 这里**不能**把 `self.target.lock()` 写在 `match` 的受检表达式里。
        //
        // Rust 会把 `match` 受检表达式里的临时值保留到**整个 match 结束**，
        // 于是 `MutexGuard` 在进入分支时仍然存活；而分支里调用的
        // `current_target()` 会再次锁同一把锁 —— `std::sync::Mutex` 不可重入，
        // 结果就是**自己把自己锁死**：进程不占 CPU、不报错，永远不返回。
        //
        // 用 `current_target()` 统一做"是否已定位"的判断即可，
        // 它在未定位时已经返回 `ClientNotReady`。
        let window =
            winapi::window_rect(self.current_target()?).map_err(AutomationError::Platform)?;
        let inside = region.x >= window.x
            && region.y >= window.y
            && region.x + region.width <= window.x + window.width
            && region.y + region.height <= window.y + window.height;
        if !inside {
            return Err(AutomationError::NeedsHumanReview(
                "拒绝捕获企业微信窗口以外的屏幕区域".into(),
            ));
        }
        Ok(())
    }
}

impl DesktopPlatform for WindowsDesktop {
    fn launch_wecom(&self) -> Result<(), AutomationError> {
        let exe = self.config.wecom_exe.as_ref().ok_or_else(|| {
            AutomationError::NeedsHumanReview(
                "尚未配置企业微信可执行文件路径，拒绝猜测路径启动".into(),
            )
        })?;

        if !exe.is_file() {
            return Err(AutomationError::NeedsHumanReview(format!(
                "配置的企业微信路径不存在：{}",
                exe.display()
            )));
        }

        if let Some(expected) = &self.config.wecom_exe_sha256 {
            let actual = winapi::file_sha256(exe).map_err(AutomationError::Platform)?;
            if !actual.eq_ignore_ascii_case(expected.trim()) {
                return Err(AutomationError::NeedsHumanReview(format!(
                    "可执行文件哈希与配置不一致，拒绝启动：{}",
                    exe.display()
                )));
            }
        }

        winapi::launch_process(exe).map_err(AutomationError::Platform)
    }

    fn focus_wecom(&self) -> Result<Rect, AutomationError> {
        let hwnd = self.locate()?;

        if !winapi::same_window(winapi::foreground_window(), hwnd) {
            winapi::bring_to_foreground(hwnd).map_err(|detail| {
                eprintln!("[platform-windows] 置前失败：{detail}");
                AutomationError::ClientNotReady
            })?;
            if !winapi::same_window(winapi::foreground_window(), hwnd) {
                return Err(AutomationError::ClientNotReady);
            }
        }

        let rect = winapi::window_rect(hwnd).map_err(AutomationError::Platform)?;
        if rect.is_degenerate() {
            return Err(AutomationError::ClientNotReady);
        }
        Ok(rect)
    }

    fn screen_metrics(&self) -> Result<ScreenMetrics, AutomationError> {
        let (width, height) = winapi::primary_screen_size();
        if width == 0 || height == 0 {
            return Err(AutomationError::Platform("无法读取主显示器尺寸".into()));
        }
        Ok(ScreenMetrics { width, height, scale_factor: winapi::primary_scale_factor() })
    }

    fn capture(&self, region: Rect) -> Result<Screenshot, AutomationError> {
        if region.is_degenerate() {
            return Err(AutomationError::NeedsHumanReview("捕获区域尺寸无效".into()));
        }
        let pixels_budget = region.width as u64 * region.height as u64;
        if pixels_budget > self.config.max_capture_pixels {
            return Err(AutomationError::NeedsHumanReview(format!(
                "捕获区域过大（{pixels_budget} 像素），拒绝执行以免扫描整屏"
            )));
        }
        self.ensure_region_inside_window(region)?;

        let frame = winapi::capture_region(region).map_err(AutomationError::Platform)?;
        Ok(Screenshot {
            pixels: frame.pixels,
            width: frame.width,
            height: frame.height,
            captured_at: std::time::SystemTime::now(),
            fingerprint: frame.fingerprint,
        })
    }

    fn guarded_click(&self, target: Point, expected_window: Rect) -> Result<(), AutomationError> {
        self.verify_guard(expected_window)?;
        winapi::move_cursor(target.x, target.y).map_err(AutomationError::Platform)?;
        // 移动后再次确认前台窗口没有被抢走。
        self.verify_guard(expected_window)?;
        winapi::left_click().map_err(AutomationError::Platform)
    }

    fn paste_text(&self, text: &str, expected_window: Rect) -> Result<(), AutomationError> {
        self.verify_guard(expected_window)?;
        winapi::set_clipboard_text(text).map_err(AutomationError::Platform)?;

        let result = winapi::send_ctrl_v().map_err(AutomationError::Platform);

        if self.config.clear_clipboard_after_paste {
            // 必须等目标程序真的读过剪贴板再清空。
            //
            // `SendInput` 只是把 Ctrl+V 排进队列就返回，目标程序要等自己的
            // 消息循环跑到那一条才会打开剪贴板。若在这里立刻清空，
            // 粘贴就会拿到空内容 —— 现象是"光标已就位，但一个字都没进去"。
            let read = winapi::wait_for_clipboard_release(
                CLIPBOARD_POLL_INTERVAL,
                self.config.clipboard_read_timeout,
            );
            if !read {
                // 没观察到占用（目标程序可能自己缓存了内容），
                // 补一个保守等待，确保它有机会处理这次粘贴。
                std::thread::sleep(CLIPBOARD_SETTLE_FALLBACK);
            }
            // 无论粘贴成功与否，都不把正文留在剪贴板里。
            let _ = winapi::clear_clipboard();
        }
        result
    }

    fn send_message_shortcut(&self, expected_window: Rect) -> Result<(), AutomationError> {
        self.verify_guard(expected_window)?;
        winapi::send_enter().map_err(AutomationError::Platform)
    }
}
