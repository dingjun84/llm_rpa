//! `DesktopPlatform` 的 macOS 实现。

use std::sync::Mutex;
use std::time::Duration;

use automation_core::{
    AutomationError, DesktopPlatform, Point, Rect, ScreenMetrics, Screenshot,
};

use crate::config::{MacOSDesktopConfig, WindowMatcher};
use crate::macosapi::{self, MacResult, WindowRef};

const CLIPBOARD_POLL_INTERVAL: Duration = Duration::from_millis(10);
const CLIPBOARD_SETTLE_FALLBACK: Duration = Duration::from_millis(200);
const CURSOR_LANDING_TOLERANCE_PX: i32 = 2;
const CLEAR_KEY_GAP: Duration = Duration::from_millis(120);

/// 真实平台实现。
///
/// 不注入、不 Hook、不读取其他进程内存；所有输入操作都先校验前台窗口。
pub struct MacOSDesktop {
    config: MacOSDesktopConfig,
    /// 最近一次成功定位到的窗口。
    target: Mutex<Option<WindowRef>>,
}


fn metrics_for_window_rect(rect: Rect) -> ScreenMetrics {
    let (width, height, scale_factor) = macosapi::metrics_for_rect(rect);
    if width == 0 || height == 0 {
        let (width, height) = macosapi::primary_screen_size();
        return ScreenMetrics {
            width,
            height,
            scale_factor: macosapi::scale_factor_for_rect(rect),
        };
    }
    ScreenMetrics {
        width,
        height,
        scale_factor,
    }
}

impl MacOSDesktop {
    pub fn new(config: MacOSDesktopConfig) -> Self {
        Self {
            config,
            target: Mutex::new(None),
        }
    }

    pub fn config(&self) -> &MacOSDesktopConfig {
        &self.config
    }

    /// 只读预览：定位目标窗口并整窗截屏，**不改变焦点、不产生任何输入事件**。
    pub fn preview(&self) -> Result<(Rect, Screenshot), AutomationError> {
        let w = self.locate()?;
        let rect = macosapi::window_rect(w).map_err(AutomationError::Platform)?;
        if rect.is_degenerate() {
            return Err(AutomationError::ClientNotReady);
        }
        let pixels_budget = rect.width as u64 * rect.height as u64;
        if pixels_budget > self.config.preview_max_pixels {
            return Err(AutomationError::NeedsHumanReview(format!(
                "目标窗口过大（{pixels_budget} 像素），超过预览上限 {}，拒绝截取",
                self.config.preview_max_pixels
            )));
        }
        let frame = macosapi::capture_region(rect).map_err(AutomationError::Platform)?;
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

    /// 只读地量一次目标窗口的几何与当前显示器指标，**不截屏、不聚焦**。
    pub fn measure(&self) -> Result<(Rect, ScreenMetrics), AutomationError> {
        let w = self.locate()?;
        let rect = macosapi::window_rect(w).map_err(AutomationError::Platform)?;
        if rect.is_degenerate() {
            return Err(AutomationError::ClientNotReady);
        }
        // 缩放必须用**窗口所在屏**，与任务装配 `screen_metrics` 同一套。
        Ok((rect, metrics_for_window_rect(rect)))
    }

    fn locate(&self) -> Result<WindowRef, AutomationError> {
        let matcher = &self.config.window_matcher;
        let configured_exe = self.config.wecom_exe.as_deref();

        let found: MacResult<WindowRef> = match (matcher, configured_exe) {
            (WindowMatcher::ClassName(owner), Some(exe)) => {
                macosapi::find_window_by_owner_in_process(owner, exe)
            }
            (WindowMatcher::ClassName(owner), None) => macosapi::find_window_by_owner_name(owner),
            (WindowMatcher::TitlePrefix(prefix), _) => {
                macosapi::find_window_by_title_prefix(prefix)
            }
        };

        match found {
            Ok(w) => {
                *self.target.lock().unwrap() = Some(w);
                Ok(w)
            }
            Err(detail) => {
                eprintln!("[platform-macos] 定位窗口失败：{detail}");
                if let (WindowMatcher::ClassName(owner), Some(exe)) = (matcher, configured_exe) {
                    if let Ok(other) = macosapi::find_window_by_owner_name(owner) {
                        let actual = macosapi::window_process_path(other)
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|_| "<读取失败>".to_string());
                        return Err(AutomationError::NeedsHumanReview(format!(
                            "找到所有者名为「{owner}」的可见窗口，但它属于「{actual}」，\
                             而配置的目标程序是「{}」——拒绝操作。\
                             请确认没有指认错窗口，或更新配置后重试。",
                            exe.display()
                        )));
                    }
                }
                Err(AutomationError::ClientNotReady)
            }
        }
    }

    fn current_target(&self) -> Result<WindowRef, AutomationError> {
        self.target
            .lock()
            .unwrap()
            .ok_or(AutomationError::ClientNotReady)
    }

    fn verify_guard(&self, expected_window: Rect) -> Result<WindowRef, AutomationError> {
        let w = self.current_target()?;
        match macosapi::foreground_window() {
            Some(fg) if fg.owner_pid == w.owner_pid => {}
            _ => return Err(AutomationError::ClientNotReady),
        }
        let current = macosapi::window_rect(w).map_err(AutomationError::Platform)?;
        if current != expected_window {
            return Err(AutomationError::ScreenChanged);
        }
        Ok(w)
    }

    fn ensure_region_inside_window(&self, region: Rect) -> Result<(), AutomationError> {
        let window =
            macosapi::window_rect(self.current_target()?).map_err(AutomationError::Platform)?;
        let inside = region.x >= window.x
            && region.y >= window.y
            && region.x + region.width <= window.x + window.width
            && region.y + region.height <= window.y + window.height;
        if !inside {
            return Err(AutomationError::NeedsHumanReview(
                "拒绝捕获目标窗口以外的屏幕区域".into(),
            ));
        }
        Ok(())
    }


    fn require_accessibility(&self) -> Result<(), AutomationError> {
        if macosapi::accessibility_trusted() {
            return Ok(());
        }
        Err(AutomationError::NeedsHumanReview(
            "未授予「辅助功能」权限，系统会静默忽略鼠标移动与点击。请到「系统设置 → 隐私与安全性 → 辅助功能」中启用本应用；若还需要截屏/找窗口，请同时开启「屏幕录制」。"
                .into(),
        ))
    }

    fn ensure_cursor_over_target(&self, at: Point, target: WindowRef) -> Result<(), AutomationError> {
        let (x, y) = macosapi::cursor_position().map_err(AutomationError::Platform)?;
        if (x - at.x).abs() > CURSOR_LANDING_TOLERANCE_PX
            || (y - at.y).abs() > CURSOR_LANDING_TOLERANCE_PX
        {
            return Err(AutomationError::NeedsHumanReview(format!(
                "鼠标没有移动到目标位置：要求 ({}, {})，实际停在 ({}, {})。\
                 滚轮/点击事件送给光标实际所在的窗口，继续等于操作别的窗口，所以拒绝执行。\
                 常见原因：未授予「辅助功能」权限，系统静默忽略了光标移动。",
                at.x, at.y, x, y
            )));
        }
        match macosapi::window_from_point(x, y) {
            Some(found) if found.owner_pid == target.owner_pid => Ok(()),
            Some(_) => Err(AutomationError::NeedsHumanReview(format!(
                "鼠标位置 ({x}, {y}) 上压着的不是目标窗口，拒绝操作：\
                 常见原因：目标窗口被弹窗或其它窗口遮挡。"
            ))),
            None => Err(AutomationError::NeedsHumanReview(format!(
                "鼠标位置 ({x}, {y}) 上没有窗口，拒绝操作。"
            ))),
        }
    }
}

impl DesktopPlatform for MacOSDesktop {
    fn launch_wecom(&self) -> Result<(), AutomationError> {
        let exe = self.config.wecom_exe.as_ref().ok_or_else(|| {
            AutomationError::NeedsHumanReview(
                "尚未配置客户端可执行文件 / .app 路径，拒绝猜测路径启动".into(),
            )
        })?;

        let exists = exe.exists()
            || (exe.extension().and_then(|e| e.to_str()) == Some("app") && exe.is_dir());
        if !exists {
            return Err(AutomationError::NeedsHumanReview(format!(
                "配置的客户端路径不存在：{}",
                exe.display()
            )));
        }

        if let Some(expected) = &self.config.wecom_exe_sha256 {
            // .app 包是目录：对包内主可执行文件算哈希；若直接是文件则算文件本身。
            let hash_target = if exe.is_dir() {
                // 尝试 Contents/MacOS 下与包同名的可执行文件
                let name = exe
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let candidate = exe.join("Contents/MacOS").join(&name);
                if candidate.is_file() {
                    candidate
                } else {
                    return Err(AutomationError::NeedsHumanReview(
                        "配置了 SHA-256，但无法在 .app 包内定位主可执行文件".into(),
                    ));
                }
            } else {
                exe.clone()
            };
            let actual = macosapi::file_sha256(&hash_target).map_err(AutomationError::Platform)?;
            if !actual.eq_ignore_ascii_case(expected.trim()) {
                return Err(AutomationError::NeedsHumanReview(format!(
                    "可执行文件哈希与配置不一致，拒绝启动：{}",
                    hash_target.display()
                )));
            }
        }

        macosapi::launch_process(exe).map_err(AutomationError::Platform)
    }

    fn focus_wecom(&self) -> Result<Rect, AutomationError> {
        let target = match &self.config.window_matcher {
            WindowMatcher::ClassName(owner) => format!("所有者名「{owner}」"),
            WindowMatcher::TitlePrefix(prefix) => format!("标题以「{prefix}」开头"),
        };
        let owned_by = match self.config.wecom_exe.as_deref() {
            Some(exe) => format!("、且属于程序「{}」", exe.display()),
            None => String::new(),
        };

        let w = match self.locate() {
            Ok(w) => w,
            Err(AutomationError::NeedsHumanReview(reason)) => {
                return Err(AutomationError::NeedsHumanReview(reason));
            }
            Err(_) => {
                return Err(AutomationError::NeedsHumanReview(format!(
                    "找不到目标窗口：没有找到可见的顶层窗口（{target}{owned_by}）。\
                     请确认客户端已经启动并登录，主窗口没有最小化。\
                     若尚未授权，请到「系统设置 → 隐私与安全性」开启屏幕录制与辅助功能。"
                )));
            }
        };

        let already_front = macosapi::foreground_window()
            .map(|fg| fg.owner_pid == w.owner_pid)
            .unwrap_or(false);
        if !already_front {
            if let Err(detail) = macosapi::bring_to_foreground(w) {
                eprintln!("[platform-macos] 置前失败：{detail}");
                return Err(AutomationError::NeedsHumanReview(format!(
                    "已找到目标窗口（{target}），但把它带到前台时被系统拒绝：{detail}。\
                     请先手动点一下目标窗口，或检查「辅助功能」权限。"
                )));
            }
            if !macosapi::wait_until_foreground(w, self.config.foreground_settle_timeout) {
                return Err(AutomationError::NeedsHumanReview(
                    "已找到目标窗口并发出了置前请求，但它仍然不是前台应用。\
                     请手动点一下目标窗口再重试。"
                        .into(),
                ));
            }
        }

        let rect = macosapi::window_rect(w).map_err(AutomationError::Platform)?;
        if rect.is_degenerate() {
            return Err(AutomationError::NeedsHumanReview(format!(
                "目标窗口的矩形退化了（{}x{}）——请重新指认窗口。",
                rect.width, rect.height
            )));
        }
        Ok(rect)
    }

    fn resize_wecom(&self, width: i32, height: i32) -> Result<Rect, AutomationError> {
        if width <= 0 || height <= 0 {
            return Err(AutomationError::NeedsHumanReview(format!(
                "标定记录的窗口尺寸不合法（{width}×{height}），不能拿它去调整窗口。\
                 请重新点「记录窗口尺寸」并保存配置。"
            )));
        }
        let w = self.current_target()?;
        macosapi::resize_window(w, width, height).map_err(AutomationError::Platform)
    }

    fn measure_target_window(&self) -> Result<(Rect, ScreenMetrics), AutomationError> {
        self.measure()
    }

    fn screen_metrics(&self) -> Result<ScreenMetrics, AutomationError> {
        // 已定位到目标窗时，用它所在显示器；否则退回主屏。
        // 记录窗口尺寸（measure）与任务挑选标定都走这里的缩放语义。
        if let Ok(w) = self.current_target() {
            if let Ok(rect) = macosapi::window_rect(w) {
                if !rect.is_degenerate() {
                    return Ok(metrics_for_window_rect(rect));
                }
            }
        }
        let (width, height) = macosapi::primary_screen_size();
        if width == 0 || height == 0 {
            return Err(AutomationError::Platform("无法读取主显示器尺寸".into()));
        }
        Ok(ScreenMetrics {
            width,
            height,
            scale_factor: macosapi::primary_scale_factor(),
        })
    }

    fn is_responsive(&self) -> Result<bool, AutomationError> {
        let w = self.current_target()?;
        Ok(macosapi::is_responsive(w))
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
        let frame = macosapi::capture_region(region).map_err(AutomationError::Platform)?;
        Ok(Screenshot {
            pixels: frame.pixels,
            width: frame.width,
            height: frame.height,
            captured_at: std::time::SystemTime::now(),
            fingerprint: frame.fingerprint,
        })
    }

    fn guarded_click(&self, target: Point, expected_window: Rect) -> Result<(), AutomationError> {
        self.require_accessibility()?;
        // 先滑过去：原先把 verify_guard 放在移动前，窗口矩形稍有变化就直接返回，
        // 操作者会看到「完全没动鼠标」，无法区分是坐标错还是守卫拦了。
        macosapi::move_cursor(target.x, target.y, self.config.pointer_speed_px_per_sec)
            .map_err(AutomationError::Platform)?;
        let w = self.verify_guard(expected_window)?;
        self.ensure_cursor_over_target(target, w)?;
        macosapi::left_click().map_err(AutomationError::Platform)
    }

    fn move_pointer(&self, target: Point) -> Result<(), AutomationError> {
        self.require_accessibility()?;
        macosapi::move_cursor(target.x, target.y, self.config.pointer_speed_px_per_sec)
            .map_err(AutomationError::Platform)
    }

    fn scroll(
        &self,
        at: Point,
        notches: i32,
        expected_window: Rect,
    ) -> Result<(), AutomationError> {
        self.require_accessibility()?;
        if notches == 0 {
            return Ok(());
        }
        macosapi::move_cursor(at.x, at.y, self.config.pointer_speed_px_per_sec)
            .map_err(AutomationError::Platform)?;
        let target = self.verify_guard(expected_window)?;
        self.ensure_cursor_over_target(at, target)?;
        macosapi::scroll_wheel(notches).map_err(AutomationError::Platform)
    }

    fn paste_text(&self, text: &str, expected_window: Rect) -> Result<(), AutomationError> {
        self.verify_guard(expected_window)?;
        macosapi::set_clipboard_text(text).map_err(AutomationError::Platform)?;
        let result = macosapi::send_cmd_v().map_err(AutomationError::Platform);
        if self.config.clear_clipboard_after_paste {
            let read = macosapi::wait_for_clipboard_release(
                CLIPBOARD_POLL_INTERVAL,
                self.config.clipboard_read_timeout,
            );
            if !read {
                std::thread::sleep(CLIPBOARD_SETTLE_FALLBACK);
            }
            let _ = macosapi::clear_clipboard();
        }
        result
    }

    fn type_text(&self, text: &str, expected_window: Rect) -> Result<(), AutomationError> {
        self.verify_guard(expected_window)?;
        macosapi::send_unicode_text(text, self.config.typing_interval)
            .map_err(AutomationError::Platform)?;
        self.verify_guard(expected_window).map(|_| ())
    }

    fn clear_text_field(&self, expected_window: Rect) -> Result<(), AutomationError> {
        self.verify_guard(expected_window)?;
        std::thread::sleep(self.config.focus_settle_timeout);
        macosapi::send_cmd_a().map_err(AutomationError::Platform)?;
        std::thread::sleep(CLEAR_KEY_GAP);
        macosapi::send_delete().map_err(AutomationError::Platform)?;
        self.verify_guard(expected_window).map(|_| ())
    }

    fn send_message_shortcut(&self, expected_window: Rect) -> Result<(), AutomationError> {
        self.verify_guard(expected_window)?;
        macosapi::send_enter().map_err(AutomationError::Platform)
    }
}
