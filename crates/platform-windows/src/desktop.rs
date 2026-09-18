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

/// 「光标是否停到了要求的位置」允许的偏差（像素）。
///
/// 取 2 而不是 0：`SetCursorPos` 与 `GetCursorPos` 用的都是整数屏幕坐标，
/// 正常情况下完全一致，这点容差只是吸收多显示器 / DPI 换算可能带来的取整差。
///
/// **刻意取得很小**——它的用途是"吸收取整"，不是"容忍移错了地方"：
/// 真正的缺陷（光标压根没动、或落到了别的窗口上）偏差是几十上百像素，
/// 这点容差拦不住，也不该拦。
const CURSOR_LANDING_TOLERANCE_PX: i32 = 2;

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
        Ok((rect, self.screen_metrics()?))
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

    /// 确认光标**真的**停在了目标窗口上，然后才允许发滚轮。
    ///
    /// ## 为什么非查不可
    ///
    /// 滚轮事件送给的是**光标实际所在**的那个窗口，不是"我们想滚的那个窗口"。
    /// 而 `SetCursorPos` 存在**返回成功、光标却没动**的情况（前台窗口属于更高
    /// 完整性的进程、UIPI 限制等）。此时后面那次 `scroll_wheel` 会滚到光标
    /// 实际停着的地方。
    ///
    /// 现场表现是最难查的一类：**「列表确实滚了，但鼠标从头到尾没动过」**——
    /// 因为光标本来就压在列表上，于是"滚对了"掩盖了"根本没移过去"。
    /// 换一台机器、换一个光标起始位置，同一个缺陷立刻变成"滚了别人的窗口"。
    ///
    /// ## 两道判据，缺一不可
    ///
    /// - **位置**：`GetCursorPos` 读回来的坐标必须与要求的一致
    ///   （容差 [`CURSOR_LANDING_TOLERANCE_PX`]）；
    /// - **归属**：光标下那个顶层窗口必须就是目标窗口。位置对了但压着别的窗口
    ///   （被弹窗盖住、被遮挡）同样会滚错对象——只查坐标查不出这一种。
    ///
    /// 查不过就**报错，不重试**：这是"先验证再动作"，不是"多试几次总能成功"。
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
            Some(found) if winapi::same_window(found, hwnd) => Ok(()),
            Some(_) => Err(AutomationError::NeedsHumanReview(format!(
                "鼠标位置 ({x}, {y}) 上压着的不是目标窗口，拒绝滚动：\
                 滚轮事件送给光标下的窗口，滚下去会动到别的程序。\
                 常见原因：目标窗口被弹窗或其它窗口遮挡。"
            ))),
            None => Err(AutomationError::NeedsHumanReview(format!(
                "鼠标位置 ({x}, {y}) 上没有窗口，拒绝滚动。"
            ))),
        }
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

        if !winapi::same_window(winapi::foreground_window(), hwnd) {
            if let Err(detail) = winapi::bring_to_foreground(hwnd) {
                eprintln!("[platform-windows] 置前失败：{detail}");
                return Err(AutomationError::NeedsHumanReview(format!(
                    "已找到目标窗口（{target}），但把它带到前台时被系统拒绝：{detail}。\
                     请先在任务栏点一下目标窗口，让它成为前台窗口，再重试。"
                )));
            }
            // 关键分支：`SetForegroundWindow` 返回成功**不代表前台已经切过去了**——
            // 实际切换由窗口管理器异步完成，且仍可能被前台锁定策略吞掉（只闪一下任务栏）。
            // 所以不能立刻读 `GetForegroundWindow()`，否则会把"还没切完"误判成"切换失败"，
            // 把一个本来能用的窗口判成不可用。
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

    fn screen_metrics(&self) -> Result<ScreenMetrics, AutomationError> {
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
        self.verify_guard(expected_window)?;
        winapi::move_cursor(target.x, target.y).map_err(AutomationError::Platform)?;
        // 移动后再次确认前台窗口没有被抢走。
        self.verify_guard(expected_window)?;
        winapi::left_click().map_err(AutomationError::Platform)
    }

    fn scroll(
        &self,
        at: Point,
        notches: i32,
        expected_window: Rect,
    ) -> Result<(), AutomationError> {
        // 和点击同一套守卫：滚轮事件送给光标下的窗口，滚错窗口会把别人的界面滚走。
        //
        // 守卫**无条件**执行，`notches == 0` 的短路放在它后面。
        // 反过来写就等于留了一条"传 0 格就能绕过前台窗口校验"的捷径，
        // 而且会让 `Ok` 的含义变得含糊——调用方应当能认定
        // 「scroll 返回 Ok ⇒ 守卫已经通过」。
        // 这里拿到的句柄就是"打算滚的那个窗口"，下面核对光标落点时要用它。
        let target = self.verify_guard(expected_window)?;
        if notches == 0 {
            return Ok(());
        }
        winapi::move_cursor(at.x, at.y).map_err(AutomationError::Platform)?;
        // 移动后再次确认前台窗口没有被抢走，再真正滚动。
        // 这次只看"有没有被抢走"，句柄不另取——`current_target()` 在一次运行内不会变。
        self.verify_guard(expected_window)?;
        // 然后确认**光标真的到了**：滚轮送给光标下的窗口，没到就等于滚了别的窗口。
        // 这是"先验证再动作"。少了这一步，缺陷只在"光标本来就压着列表"时被掩盖
        // （现象是"列表滚了、鼠标没动"），换台机器就变成"滚了别人的窗口"。
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

    fn send_message_shortcut(&self, expected_window: Rect) -> Result<(), AutomationError> {
        self.verify_guard(expected_window)?;
        winapi::send_enter().map_err(AutomationError::Platform)
    }
}
