//! 仅用于测试工作流，不会捕获屏幕或生成任何真实输入事件。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::SystemTime;

use automation_core::{AutomationError, DesktopPlatform, Point, Rect, ScreenMetrics, Screenshot};

use crate::fault::MockFaults;

pub const DEFAULT_WINDOW: Rect = Rect { x: 0, y: 0, width: 1280, height: 720 };
pub const DEFAULT_METRICS: ScreenMetrics =
    ScreenMetrics { width: 1920, height: 1080, scale_factor: 1.0 };

/// 可脚本化的平台替身。
///
/// - `window_sequence` / `metrics_sequence` 按次弹出，用于模拟窗口被替换或缩放变化；
/// - `faults` 用于按次注入失败；
/// - 截图指纹默认每次递增，便于核验"发送后聊天区发生了变化"。
pub struct MockDesktop {
    pub operations: Mutex<Vec<String>>,
    window: Mutex<Rect>,
    metrics: Mutex<ScreenMetrics>,
    window_sequence: Mutex<VecDeque<Rect>>,
    metrics_sequence: Mutex<VecDeque<ScreenMetrics>>,
    faults: Mutex<MockFaults>,
    stable_fingerprint: AtomicBool,
    capture_count: AtomicU32,
    send_count: AtomicU32,
    pub clicks: Mutex<Vec<Point>>,
    pub pasted: Mutex<Vec<String>>,
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
            capture_count: AtomicU32::new(0),
            send_count: AtomicU32::new(0),
            clicks: Mutex::new(Vec::new()),
            pasted: Mutex::new(Vec::new()),
            focus_calls: AtomicU32::new(0),
        }
    }

    pub fn set_window(&self, window: Rect) {
        *self.window.lock().unwrap() = window;
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

    pub fn operations(&self) -> Vec<String> {
        self.operations.lock().unwrap().clone()
    }

    pub fn send_count(&self) -> u32 {
        self.send_count.load(Ordering::SeqCst)
    }

    pub fn pasted_texts(&self) -> Vec<String> {
        self.pasted.lock().unwrap().clone()
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
        let seq = self.capture_count.fetch_add(1, Ordering::SeqCst);
        let fingerprint = if self.stable_fingerprint.load(Ordering::SeqCst) {
            "mock-stable".to_string()
        } else {
            format!("mock-{seq}")
        };
        Ok(Screenshot {
            pixels: vec![],
            width: region.width.max(0) as u32,
            height: region.height.max(0) as u32,
            captured_at: SystemTime::now(),
            fingerprint,
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

    fn send_message_shortcut(&self, expected_window: Rect) -> Result<(), AutomationError> {
        if let Some(fault) = self.faults.lock().unwrap().send.take() {
            return Err(fault.into_error());
        }
        if expected_window != self.window() {
            return Err(AutomationError::ScreenChanged);
        }
        self.record("send");
        self.send_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
