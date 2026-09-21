//! 仅用于测试工作流，不会捕获屏幕或生成任何真实输入事件。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::SystemTime;

use automation_core::{AutomationError, DesktopPlatform, Point, Rect, ScreenMetrics, Screenshot};

use crate::fault::MockFaults;

pub const DEFAULT_WINDOW: Rect = Rect { x: 0, y: 0, width: 1280, height: 720 };
pub const DEFAULT_METRICS: ScreenMetrics =
    ScreenMetrics { width: 1920, height: 1080, scale_factor: 1.0 };

/// 默认的列表底部偏移。
///
/// 取一个大到"测试里滚不到头"的值，等于"列表可以一直往下滚"。
/// 需要模拟"滚到底"的用例请用 [`MockDesktop::script_scroll_bottom`]。
const DEFAULT_SCROLL_BOTTOM: i32 = 1_000;

/// 可脚本化的平台替身。
///
/// 截图指纹**由界面状态决定，而不是由"第几次截屏"决定**——这一点很关键：
/// 编排器用"指纹有没有变"来判断"画面动没动"，如果指纹每次调用都变，
/// 那"滚动到底"和"客户端卡死"这两种情况在替身上就永远测不出来。
///
/// 参与指纹的状态有三样：
/// - 捕获区域本身（不同区域的画面当然不同）；
/// - `view_offset`：模拟可滚动列表的视图偏移，**两端钳制**——
///   滚到顶/底之后继续滚不会有任何变化，这正是"画面没动"的来源；
/// - `screen_version`：点击、发送这类"应该改变画面"的动作把它 +1。
pub struct MockDesktop {
    pub operations: Mutex<Vec<String>>,
    window: Mutex<Rect>,
    metrics: Mutex<ScreenMetrics>,
    window_sequence: Mutex<VecDeque<Rect>>,
    metrics_sequence: Mutex<VecDeque<ScreenMetrics>>,
    faults: Mutex<MockFaults>,
    stable_fingerprint: AtomicBool,
    responsive: AtomicBool,
    clicks_change_screen: AtomicBool,
    view_offset: AtomicI32,
    scroll_bottom: AtomicI32,
    screen_version: AtomicU32,
    send_count: AtomicU32,
    /// 模拟客户端自己的最小窗口尺寸。
    ///
    /// 请求比它小的尺寸时**被夹到它**，而调用照样"成功"——真实的
    /// `SetWindowPos` 就是这样：报成功，窗口却没变成你要的尺寸。
    /// "调完必须再量一遍"这条判据就是靠它才测得出来。
    min_window_size: Mutex<Option<(i32, i32)>>,
    pub clicks: Mutex<Vec<Point>>,
    pub pasted: Mutex<Vec<String>>,
    /// 逐字输入过的文本（按调用顺序）。与 `pasted` 分开记：
    /// "走的是粘贴还是逐字"本身就是被测的行为，混在一起就验不出来了。
    pub typed: Mutex<Vec<String>>,
    /// 每次滚动的位置与格数。`notches > 0` 表示向下滚。
    pub scrolls: Mutex<Vec<(Point, i32)>>,
    /// 每次**请求**调整到的窗口尺寸（按调用顺序）。记的是请求值而不是结果值：
    /// "有没有去调"和"调成了没有"是两条不同的判据，结果值在 `window()` 里。
    pub resizes: Mutex<Vec<(i32, i32)>>,
    pub focus_calls: AtomicU32,
}

impl Default for MockDesktop {
    fn default() -> Self {
        Self::new()
    }
}

impl MockDesktop {
    pub fn new() -> Self {
        Self {
            operations: Mutex::new(Vec::new()),
            window: Mutex::new(DEFAULT_WINDOW),
            metrics: Mutex::new(DEFAULT_METRICS),
            window_sequence: Mutex::new(VecDeque::new()),
            metrics_sequence: Mutex::new(VecDeque::new()),
            faults: Mutex::new(MockFaults::default()),
            stable_fingerprint: AtomicBool::new(false),
            responsive: AtomicBool::new(true),
            clicks_change_screen: AtomicBool::new(true),
            view_offset: AtomicI32::new(0),
            scroll_bottom: AtomicI32::new(DEFAULT_SCROLL_BOTTOM),
            screen_version: AtomicU32::new(0),
            send_count: AtomicU32::new(0),
            min_window_size: Mutex::new(None),
            clicks: Mutex::new(Vec::new()),
            pasted: Mutex::new(Vec::new()),
            typed: Mutex::new(Vec::new()),
            scrolls: Mutex::new(Vec::new()),
            resizes: Mutex::new(Vec::new()),
            focus_calls: AtomicU32::new(0),
        }
    }

    pub fn set_window(&self, window: Rect) {
        *self.window.lock().unwrap() = window;
    }

    /// 模拟客户端的最小窗口尺寸：请求比它小的尺寸会被**夹到它**，调用照样成功。
    ///
    /// 用途是测「自动调整没生效」那条分支——真实客户端（微信）确实有最小尺寸，
    /// 标定值比它小的时候就会这样。
    pub fn set_min_window_size(&self, min: (i32, i32)) {
        *self.min_window_size.lock().unwrap() = Some(min);
    }

    pub fn window(&self) -> Rect {
        *self.window.lock().unwrap()
    }

    pub fn set_metrics(&self, metrics: ScreenMetrics) {
        *self.metrics.lock().unwrap() = metrics;
    }

    /// 让后续每次 `focus_wecom` 依次返回给定窗口，模拟窗口被替换。
    pub fn script_windows(&self, windows: impl IntoIterator<Item = Rect>) {
        *self.window_sequence.lock().unwrap() = windows.into_iter().collect();
    }

    /// 让后续每次 `screen_metrics` 依次返回给定指标，模拟缩放变化。
    pub fn script_metrics(&self, metrics: impl IntoIterator<Item = ScreenMetrics>) {
        *self.metrics_sequence.lock().unwrap() = metrics.into_iter().collect();
    }

    pub fn faults(&self) -> std::sync::MutexGuard<'_, MockFaults> {
        self.faults.lock().unwrap()
    }

    pub fn inject(&self, apply: impl FnOnce(&mut MockFaults)) {
        apply(&mut self.faults.lock().unwrap());
    }

    /// 截图指纹不再变化，用于模拟"发送后界面没有任何反应"。
    pub fn freeze_fingerprints(&self) {
        self.stable_fingerprint.store(true, Ordering::SeqCst);
    }

    /// 把列表底部设成给定偏移（格）。设成 `0` 就是"列表只有一屏，根本滚不动"。
    pub fn script_scroll_bottom(&self, bottom: i32) {
        self.scroll_bottom.store(bottom.max(0), Ordering::SeqCst);
    }

    /// 当前视图偏移（格）。`0` = 列表顶部。
    pub fn view_offset(&self) -> i32 {
        self.view_offset.load(Ordering::SeqCst)
    }

    /// 点击**不再**改变画面，用于模拟"点击没有生效"。
    ///
    /// 对应真实里的几种情况：窗口被别的窗口压住、有弹窗挡着、
    /// 或者点在了列表的空白处——动作发出去了，界面没有任何反应。
    pub fn script_clicks_without_effect(&self) {
        self.clicks_change_screen.store(false, Ordering::SeqCst);
    }

    /// 让 `is_responsive` 返回"未响应"，模拟客户端卡死。
    pub fn set_responsive(&self, responsive: bool) {
        self.responsive.store(responsive, Ordering::SeqCst);
    }

    pub fn operations(&self) -> Vec<String> {
        self.operations.lock().unwrap().clone()
    }

    pub fn send_count(&self) -> u32 {
        self.send_count.load(Ordering::SeqCst)
    }

    pub fn pasted_texts(&self) -> Vec<String> {
        self.pasted.lock().unwrap().clone()
    }

    /// 逐字输入过的文本，按先后顺序。
    ///
    /// 与 [`Self::pasted_texts`] 分开记：粘贴和逐字输入是**两条不同的输入路径**，
    /// 混在一起时，"搜索框那一次到底走的是哪条路"就分不出来了——
    /// 而搜索框必须逐字敲才会触发联想，用粘贴的话下拉根本不弹。
    pub fn typed_texts(&self) -> Vec<String> {
        self.typed.lock().unwrap().clone()
    }

    /// 已发生的滚动次数。
    pub fn scroll_count(&self) -> usize {
        self.scrolls.lock().unwrap().len()
    }

    /// 累计向下滚动的格数（向上滚为负）。
    pub fn scrolled_down_notches(&self) -> i32 {
        self.scrolls.lock().unwrap().iter().map(|(_, notches)| *notches).sum()
    }

    /// 累计向上滚动的格数（返回正数）。
    pub fn scrolled_up_notches(&self) -> i32 {
        self.scrolls
            .lock()
            .unwrap()
            .iter()
            .map(|(_, notches)| -*notches)
            .filter(|notches| *notches > 0)
            .sum()
    }

    /// 指纹由"界面此刻长什么样"决定：区域 + 视图偏移 + 界面版本号。
    fn fingerprint_of(&self, region: Rect) -> String {
        if self.stable_fingerprint.load(Ordering::SeqCst) {
            return "mock-stable".to_string();
        }
        format!(
            "mock-{}+{}x{}x{}-o{}-v{}",
            region.x,
            region.y,
            region.width,
            region.height,
            self.view_offset.load(Ordering::SeqCst),
            self.screen_version.load(Ordering::SeqCst),
        )
    }

    fn record(&self, op: &str) {
        self.operations.lock().unwrap().push(op.to_string());
    }
}

impl DesktopPlatform for MockDesktop {
    fn launch_wecom(&self) -> Result<(), AutomationError> {
        if let Some(fault) = self.faults.lock().unwrap().launch.take() {
            return Err(fault.into_error());
        }
        self.record("launch");
        Ok(())
    }

    fn is_responsive(&self) -> Result<bool, AutomationError> {
        Ok(self.responsive.load(Ordering::SeqCst))
    }

    fn focus_wecom(&self) -> Result<Rect, AutomationError> {
        if let Some(fault) = self.faults.lock().unwrap().focus.take() {
            return Err(fault.into_error());
        }
        self.focus_calls.fetch_add(1, Ordering::SeqCst);
        self.record("focus");
        let scripted = self.window_sequence.lock().unwrap().pop_front();
        match scripted {
            Some(window) => {
                *self.window.lock().unwrap() = window;
                Ok(window)
            }
            None => Ok(self.window()),
        }
    }

    fn resize_wecom(&self, width: i32, height: i32) -> Result<Rect, AutomationError> {
        if let Some(fault) = self.faults.lock().unwrap().resize.take() {
            return Err(fault.into_error());
        }
        self.resizes.lock().unwrap().push((width, height));
        self.record("resize");
        // 客户端自己的最小尺寸会把请求夹住，而调用照样"成功"——真实
        // `SetWindowPos` 就是这个行为，也是"调完必须再量一遍"的来源。
        let (width, height) = match *self.min_window_size.lock().unwrap() {
            Some((min_width, min_height)) => (width.max(min_width), height.max(min_height)),
            None => (width, height),
        };
        let mut window = self.window.lock().unwrap();
        window.width = width;
        window.height = height;
        Ok(*window)
    }

    fn screen_metrics(&self) -> Result<ScreenMetrics, AutomationError> {
        if let Some(fault) = self.faults.lock().unwrap().metrics.take() {
            return Err(fault.into_error());
        }
        let scripted = self.metrics_sequence.lock().unwrap().pop_front();
        match scripted {
            Some(metrics) => {
                *self.metrics.lock().unwrap() = metrics;
                Ok(metrics)
            }
            None => Ok(*self.metrics.lock().unwrap()),
        }
    }

    fn capture(&self, region: Rect) -> Result<Screenshot, AutomationError> {
        if let Some(fault) = self.faults.lock().unwrap().capture.pop_front() {
            return Err(fault.into_error());
        }
        self.record("capture");
        Ok(Screenshot {
            pixels: vec![],
            width: region.width.max(0) as u32,
            height: region.height.max(0) as u32,
            captured_at: SystemTime::now(),
            fingerprint: self.fingerprint_of(region),
        })
    }

    fn guarded_click(&self, target: Point, expected_window: Rect) -> Result<(), AutomationError> {
        if let Some(fault) = self.faults.lock().unwrap().click.take() {
            return Err(fault.into_error());
        }
        // 与真实实现保持一致：前台窗口与预期不符时拒绝输入。
        if expected_window != self.window() {
            return Err(AutomationError::ScreenChanged);
        }
        self.record("click");
        self.clicks.lock().unwrap().push(target);
        // 一次生效的点击会改变界面（选中会话、切换聊天），所以画面版本号 +1。
        if self.clicks_change_screen.load(Ordering::SeqCst) {
            self.screen_version.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }

        fn move_pointer(&self, target: Point) -> Result<(), AutomationError> {
        let _ = target;
        Ok(())
    }

fn scroll(
        &self,
        at: Point,
        notches: i32,
        expected_window: Rect,
    ) -> Result<(), AutomationError> {
        // 与真实实现保持一致：前台窗口与预期不符时拒绝输入。
        if expected_window != self.window() {
            return Err(AutomationError::ScreenChanged);
        }
        self.record("scroll");
        self.scrolls.lock().unwrap().push((at, notches));
        // 列表两端是**钳制**的：滚到顶或底之后继续滚，画面不会有任何变化。
        // 编排器正是靠这一点区分"已经到底了"和"客户端卡死了"。
        let bottom = self.scroll_bottom.load(Ordering::SeqCst);
        let current = self.view_offset.load(Ordering::SeqCst);
        self.view_offset
            .store((current + notches).clamp(0, bottom), Ordering::SeqCst);
        Ok(())
    }

    fn paste_text(&self, text: &str, expected_window: Rect) -> Result<(), AutomationError> {
        if let Some(fault) = self.faults.lock().unwrap().paste.take() {
            return Err(fault.into_error());
        }
        if expected_window != self.window() {
            return Err(AutomationError::ScreenChanged);
        }
        self.record("paste");
        self.pasted.lock().unwrap().push(text.to_string());
        Ok(())
    }

    fn type_text(&self, text: &str, expected_window: Rect) -> Result<(), AutomationError> {
        if let Some(fault) = self.faults.lock().unwrap().type_text.take() {
            return Err(fault.into_error());
        }
        if expected_window != self.window() {
            return Err(AutomationError::ScreenChanged);
        }
        self.record("type");
        self.typed.lock().unwrap().push(text.to_string());
        Ok(())
    }

    fn clear_text_field(&self, expected_window: Rect) -> Result<(), AutomationError> {
        if let Some(fault) = self.faults.lock().unwrap().clear.take() {
            return Err(fault.into_error());
        }
        if expected_window != self.window() {
            return Err(AutomationError::ScreenChanged);
        }
        self.record("clear");
        Ok(())
    }

    fn send_message_shortcut(&self, expected_window: Rect) -> Result<(), AutomationError> {
        if let Some(fault) = self.faults.lock().unwrap().send.take() {
            return Err(fault.into_error());
        }
        if expected_window != self.window() {
            return Err(AutomationError::ScreenChanged);
        }
        self.record("send");
        self.send_count.fetch_add(1, Ordering::SeqCst);
        // 发送之后聊天区多了一条消息 —— 送达核验靠的就是这个变化。
        self.screen_version.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
