//! `DesktopPlatform` 的 Windows 实现。

use std::sync::Mutex;
use std::time::Duration;

use automation_core::{
    PeerTopWindow,
    AutomationError, DesktopPlatform, Point, Rect, ScreenMetrics, Screenshot,
};
use windows::Win32::Foundation::HWND;

use crate::config::{WindowMatcher, WindowsDesktopConfig};
use crate::winapi::{self, WinResult};

const CLIPBOARD_POLL_INTERVAL: Duration = Duration::from_millis(10);
const CLIPBOARD_SETTLE_FALLBACK: Duration = Duration::from_millis(200);
const CURSOR_LANDING_TOLERANCE_PX: i32 = 2;
const CLEAR_KEY_GAP: Duration = Duration::from_millis(120);

/// 真实平台实现。
///
/// 不注入、不 Hook、不读取其他进程内存；所有输入操作都先校验前台窗口。
pub struct WindowsDesktop {
    config: WindowsDesktopConfig,
    /// 最近一次成功定位到的窗口句柄，以 `isize` 保存以保持 `Send + Sync`。
    target: Mutex<Option<isize>>,
}


fn metrics_for_window_rect(rect: Rect) -> ScreenMetrics {
    let (width, height, scale_factor) = winapi::metrics_for_rect(rect);
    if width == 0 || height == 0 {
        let (width, height) = winapi::primary_screen_size();
        return ScreenMetrics {
            width,
            height,
            scale_factor: winapi::scale_factor_for_rect(rect),
        };
    }
    ScreenMetrics {
        width,
        height,
        scale_factor,
    }
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
        // 与 `capture` 用不同的预算：标定需要看整个窗口，所以放宽；
        // 但仍要挡住"窗口谎报了一个天文数字般的矩形"这种会直接吃掉几个 GB 的情况。
        let pixels_budget = rect.width as u64 * rect.height as u64;
        if pixels_budget > self.config.preview_max_pixels {
            return Err(AutomationError::NeedsHumanReview(format!(
                "目标窗口过大（{pixels_budget} 像素），超过预览上限 {}，拒绝截取",
                self.config.preview_max_pixels
            )));
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

    /// 只读地量一次目标窗口的几何与当前显示器指标，**不截屏、不聚焦**。
    ///
    /// 供界面上的「记录窗口尺寸」使用。记录这件事必须发生在真实窗口上，
    /// 而且必须走和任务**同一套**定位规则（同样按类名 + 归属程序匹配），
    /// 否则"记下来的"和"任务看到的"可能不是同一个窗口。
    pub fn measure(&self) -> Result<(Rect, ScreenMetrics), AutomationError> {
        let hwnd = self.locate()?;
        let rect = winapi::window_rect(hwnd).map_err(AutomationError::Platform)?;
        if rect.is_degenerate() {
            return Err(AutomationError::ClientNotReady);
        }
        // 缩放必须用**窗口所在屏**，与任务装配 `screen_metrics` 同一套。
        Ok((rect, metrics_for_window_rect(rect)))
    }

    fn locate(&self) -> Result<HWND, AutomationError> {
        let matcher = &self.config.window_matcher;
        let configured_exe = self.config.wecom_exe.as_deref();

        let found: WinResult<HWND> = match (matcher, configured_exe) {
            // 配了目标程序路径就**同时**校验窗口归属。Qt 系程序（微信 4.x 就是）
            // 所有顶层窗口共用同一个类名，只按类名匹配拿到的可能是个登录窗。
            (WindowMatcher::ClassName(class), Some(exe)) => {
                winapi::find_window_by_class_in_process(class, exe)
            }
            (WindowMatcher::ClassName(class), None) => winapi::find_window_by_class(class),
            (WindowMatcher::TitlePrefix(prefix), _) => winapi::find_window_by_title_prefix(prefix),
        };

        match found {
            Ok(hwnd) => {
                *self.target.lock().unwrap() = Some(hwnd.0 as isize);
                Ok(hwnd)
            }
            Err(detail) => {
                // 保留诊断信息，但对外返回稳定的错误码。
                eprintln!("[platform-windows] 定位窗口失败：{detail}");

                // "类名匹配上了、但它属于别的程序"是最容易被误当成"窗口根本没打开"
                // 的一种情况，单独报出来，省得操作者反复确认程序开没开。
                if let (WindowMatcher::ClassName(class), Some(exe)) = (matcher, configured_exe) {
                    if let Ok(other) = winapi::find_window_by_class(class) {
                        let actual = winapi::window_process_path(other)
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|_| "<读取失败>".to_string());
                        return Err(AutomationError::NeedsHumanReview(format!(
                            "找到类名为「{class}」的可见窗口，但它属于「{actual}」，\
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

    fn current_target(&self) -> Result<HWND, AutomationError> {
        let stored = *self.target.lock().unwrap();
        match stored {
            Some(value) => Ok(HWND(value as *mut std::ffi::c_void)),
            None => Err(AutomationError::ClientNotReady),
        }
    }

    /// 点击前的守卫：前台必须是目标窗口。
    ///
    /// 尺寸和位置不再比对。窗口变宽、变高或挪动几个像素不算失败；
    /// 前台换成别的窗口则拒绝输入，避免点到别的程序。
    fn verify_guard(&self, _expected_window: Rect) -> Result<HWND, AutomationError> {
        let hwnd = self.current_target()?;
        let fg = winapi::foreground_window();
        if !winapi::same_window(fg, hwnd) && !winapi::same_process(fg, hwnd) {
            return Err(AutomationError::ClientNotReady);
        }
        Ok(hwnd)
    }

    fn ensure_region_inside_window(&self, region: Rect) -> Result<(), AutomationError> {
        // 用 current_target()（勿在 match 里持锁再重入，见历史死锁）。
        let hwnd = self.current_target()?;
        let window = winapi::window_rect(hwnd).map_err(AutomationError::Platform)?;
        if region_inside(region, window) {
            return Ok(());
        }
        let pid = winapi::window_pid(hwnd);
        for peer in winapi::list_visible_windows_of_pid(pid) {
            if region_inside(region, peer.rect) {
                return Ok(());
            }
        }
        Err(AutomationError::NeedsHumanReview(
            "拒绝捕获主窗/同进程顶层窗以外的屏幕区域".into(),
        ))
    }

    /// 确认光标停在目标（或同进程）窗口上。
    fn ensure_cursor_over_target(&self, at: Point, hwnd: HWND) -> Result<(), AutomationError> {
        let (x, y) = winapi::cursor_position().map_err(AutomationError::Platform)?;
        if (x - at.x).abs() > CURSOR_LANDING_TOLERANCE_PX
            || (y - at.y).abs() > CURSOR_LANDING_TOLERANCE_PX
        {
            return Err(AutomationError::NeedsHumanReview(format!(
                "鼠标没有移动到滚动位置：要求 ({}, {})，实际停在 ({}, {})。\
                 滚轮事件送给光标实际所在的窗口，继续滚等于滚别的窗口，所以拒绝执行。\
                 常见原因：前台窗口属于更高权限的进程，系统静默忽略了这次光标移动。",
                at.x, at.y, x, y
            )));
        }
        match winapi::window_from_point(x, y) {
            Some(found)
                if winapi::same_window(found, hwnd) || winapi::same_process(found, hwnd) =>
            {
                Ok(())
            }
            Some(_) => Err(AutomationError::NeedsHumanReview(format!(
                "鼠标位置 ({x}, {y}) 上压着的不是目标/同进程窗口，拒绝操作。"
            ))),
            None => Err(AutomationError::NeedsHumanReview(format!(
                "鼠标位置 ({x}, {y}) 上没有窗口，拒绝操作。"
            ))),
        }
    }
}

fn region_inside(region: Rect, window: Rect) -> bool {
    region.x >= window.x
        && region.y >= window.y
        && region.x + region.width <= window.x + window.width
        && region.y + region.height <= window.y + window.height
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
        // 把"哪一步失败"说清楚。以前这里一律折叠成 `ClientNotReady`，
        // 上层只能报一句「找不到窗口，或者带不到前台」——**操作者没法据此排查**，
        // 因为这两件事的处置方式完全不同：前者是"客户端没开/没登录"，
        // 后者是"窗口开着但被系统前台锁定挡住了"。
        let target = match &self.config.window_matcher {
            WindowMatcher::ClassName(class) => format!("类名「{class}」"),
            WindowMatcher::TitlePrefix(prefix) => format!("标题以「{prefix}」开头"),
        };
        let owned_by = match self.config.wecom_exe.as_deref() {
            Some(exe) => format!("、且属于程序「{}」", exe.display()),
            None => String::new(),
        };

        let hwnd = match self.locate() {
            Ok(hwnd) => hwnd,
            // 已经带了精确说明的（例如"类名匹配上了但属于别的程序"）原样透传。
            Err(AutomationError::NeedsHumanReview(reason)) => {
                return Err(AutomationError::NeedsHumanReview(reason));
            }
            Err(_) => {
                return Err(AutomationError::NeedsHumanReview(format!(
                    "找不到目标窗口：没有找到可见的顶层窗口（{target}{owned_by}）。\
                     请确认客户端已经启动并登录，主窗口没有最小化。"
                )));
            }
        };

        let fg = winapi::foreground_window();
        if !winapi::same_window(fg, hwnd) && !winapi::same_process(fg, hwnd) {
            if let Err(detail) = winapi::bring_to_foreground(hwnd) {
                eprintln!("[platform-windows] 置前失败：{detail}");
                return Err(AutomationError::NeedsHumanReview(format!(
                    "已找到目标窗口（{target}），但把它带到前台时被系统拒绝：{detail}。\
                     请先在任务栏点一下目标窗口，让它成为前台窗口，再重试。"
                )));
            }
            if !winapi::wait_until_foreground(hwnd, self.config.foreground_settle_timeout) {
                return Err(AutomationError::NeedsHumanReview(
                    "已找到目标窗口并发出了置前请求，但它仍然不是前台窗口——\
                     被 Windows 前台锁定策略拦下了。请手动点一下目标窗口再重试。"
                        .into(),
                ));
            }
        }

        let rect = winapi::window_rect(hwnd).map_err(AutomationError::Platform)?;
        if rect.is_degenerate() {
            return Err(AutomationError::NeedsHumanReview(format!(
                "目标窗口的矩形退化了（{}x{}）——拿它算区域只会得到一堆废坐标。\
                 多半是命中了同类的隐藏窗口，请重新指认窗口。",
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
        let hwnd = self.current_target()?;
        winapi::resize_window(hwnd, width, height).map_err(AutomationError::Platform)
    }

    fn measure_target_window(&self) -> Result<(Rect, ScreenMetrics), AutomationError> {
        self.measure()
    }

    fn screen_metrics(&self) -> Result<ScreenMetrics, AutomationError> {
        // 已定位到目标窗时，用它所在显示器；否则退回主屏。
        if let Ok(hwnd) = self.current_target() {
            if let Ok(rect) = winapi::window_rect(hwnd) {
                if !rect.is_degenerate() {
                    return Ok(metrics_for_window_rect(rect));
                }
            }
        }
        let (width, height) = winapi::primary_screen_size();
        if width == 0 || height == 0 {
            return Err(AutomationError::Platform("无法读取主显示器尺寸".into()));
        }
        Ok(ScreenMetrics { width, height, scale_factor: winapi::primary_scale_factor() })
    }

    fn is_responsive(&self) -> Result<bool, AutomationError> {
        // 必须先有目标窗口：没定位到就没法问"它卡没卡死"，
        // 这里如实报 `ClientNotReady`，不要为了"让调用方继续跑"而假装它在响应。
        let hwnd = self.current_target()?;
        Ok(!winapi::is_hung_window(hwnd))
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
        let hwnd = self.verify_guard(expected_window)?;
        winapi::move_cursor(target.x, target.y, self.config.pointer_speed_px_per_sec)
            .map_err(AutomationError::Platform)?;
        self.verify_guard(expected_window)?;
        self.ensure_cursor_over_target(target, hwnd)?;
        winapi::left_click().map_err(AutomationError::Platform)
    }

    fn guarded_right_click(
        &self,
        target: Point,
        expected_window: Rect,
    ) -> Result<(), AutomationError> {
        let hwnd = self.verify_guard(expected_window)?;
        winapi::move_cursor(target.x, target.y, self.config.pointer_speed_px_per_sec)
            .map_err(AutomationError::Platform)?;
        self.verify_guard(expected_window)?;
        self.ensure_cursor_over_target(target, hwnd)?;
        winapi::right_click().map_err(AutomationError::Platform)
    }

    fn list_peer_top_windows(&self) -> Result<Vec<PeerTopWindow>, AutomationError> {
        let hwnd = self.current_target()?;
        let pid = winapi::window_pid(hwnd);
        let main = winapi::window_rect(hwnd).map_err(AutomationError::Platform)?;
        Ok(winapi::list_visible_windows_of_pid(pid)
            .into_iter()
            .map(|info| PeerTopWindow {
                id: info.hwnd.to_string(),
                title: info.title,
                class_name: info.class_name,
                rect: info.rect,
                is_main: info.hwnd == hwnd.0 as isize
                    || (info.rect.x == main.x
                        && info.rect.y == main.y
                        && info.rect.width == main.width
                        && info.rect.height == main.height),
            })
            .collect())
    }

        fn move_pointer(&self, target: Point) -> Result<(), AutomationError> {
        winapi::move_cursor(target.x, target.y, self.config.pointer_speed_px_per_sec)
            .map_err(AutomationError::Platform)
    }

fn scroll(
        &self,
        at: Point,
        notches: i32,
        expected_window: Rect,
    ) -> Result<(), AutomationError> {
        //
        let target = self.verify_guard(expected_window)?;
        if notches == 0 {
            return Ok(());
        }
        winapi::move_cursor(at.x, at.y, self.config.pointer_speed_px_per_sec)
            .map_err(AutomationError::Platform)?;
        self.verify_guard(expected_window)?;
        self.ensure_cursor_over_target(at, target)?;
        winapi::scroll_wheel(notches).map_err(AutomationError::Platform)
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

    fn type_text(&self, text: &str, expected_window: Rect) -> Result<(), AutomationError> {
        self.verify_guard(expected_window)?;
        winapi::send_unicode_text(text, self.config.typing_interval)
            .map_err(AutomationError::Platform)?;
        // 输入完之后**再确认一次**前台窗口没被抢走。
        //
        // 逐字输入是一个**持续几百毫秒**的动作，不像点击那样是一次瞬时事件：
        // 中途被别的窗口（弹窗、通知、用户自己切了一下）抢走焦点的话，
        // 后半段字符会敲进那个窗口里——而调用方从返回值上完全看不出来。
        //
        // 这一道**挡不住**中途被抢（那需要逐字符检查），它只保证
        // "结束时的状态是已知的"：真的被抢了，这里会如实报错，
        // 而不是让一次敲错地方的输入看起来完全正常。
        self.verify_guard(expected_window).map(|_| ())
    }

    fn clear_text_field(&self, expected_window: Rect) -> Result<(), AutomationError> {
        self.verify_guard(expected_window)?;

        // 先等焦点落定。调用方刚刚点过这个控件，但"点到了"和"键盘焦点已经
        // 进到里面"是两件事：后者要等目标程序的消息循环处理完那次点击。
        // 不等的话，下面这几个按键会敲进上一个有焦点的控件里，而
        // `SendInput` 照样返回成功——错误要过一会儿才以别的面目出现。
        std::thread::sleep(self.config.focus_settle_timeout);

        winapi::send_ctrl_a().map_err(AutomationError::Platform)?;
        std::thread::sleep(CLEAR_KEY_GAP);
        winapi::send_delete().map_err(AutomationError::Platform)?;

        // 与 `type_text` 同一个道理：清空是一串按键，中途被抢走焦点的话
        // 后半段会敲到别处。这一道保证"结束时的状态是已知的"。
        self.verify_guard(expected_window).map(|_| ())
    }
}
