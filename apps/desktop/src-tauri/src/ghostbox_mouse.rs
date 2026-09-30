//! GhostBox 鼠标适配：把 `DesktopPlatform` 的移动/点击/滚动落到 HID。
//!
//! ★ 仅 Windows；进程级 `shared_device_session` 开一次，**不**在每次操作后 CloseDevice。
//! ★ 键盘（`type_text` / `clear_text_field` / `paste_text`）仍走内层 OS 实现。

use std::sync::Arc;

use automation_core::{
    AutomationError, DesktopPlatform, Point, Rect, ScreenMetrics, Screenshot,
};

/// 包装真实桌面端口：鼠标相关走 GhostBox，其余委托内层。
pub struct GhostboxMouseDesktop {
    pub inner: Arc<dyn DesktopPlatform>,
}

impl GhostboxMouseDesktop {
    pub fn new(inner: Arc<dyn DesktopPlatform>) -> Self {
        Self { inner }
    }
}

#[cfg(windows)]
fn with_api<T>(
    f: impl FnOnce(&ghostbox::GBMAPI) -> Result<T, AutomationError>,
) -> Result<T, AutomationError> {
    let dll = resolve_dll().map_err(AutomationError::Platform)?;
    let api = ghostbox::shared_device_session(&dll, ghostbox::OPEN_DEVICE_TIMEOUT)
        .map_err(|err| AutomationError::Platform(format!("打开幽灵盒失败：{err}")))?;
    f(api.as_ref())
}

#[cfg(windows)]
fn resolve_dll() -> Result<std::path::PathBuf, String> {
    use std::path::PathBuf;
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("gbilmd64.dll"));
        }
    }
    candidates.push(PathBuf::from("gbilmd64.dll"));
    for c in &candidates {
        if c.is_file() {
            return Ok(c.clone());
        }
    }
    Err(format!(
        "找不到 gbilmd64.dll（已试：{}）",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

#[cfg(windows)]
fn ghost_move_to(target: Point) -> Result<(), AutomationError> {
    with_api(|api| {
        api.MoveMouseTo(target.x, target.y)
            .map_err(|err| AutomationError::Platform(format!("MoveMouseTo 失败：{err}")))?;
        Ok(())
    })
}

#[cfg(windows)]
fn ghost_left_click() -> Result<(), AutomationError> {
    with_api(|api| {
        api.PressMouseButton(1)
            .map_err(|err| AutomationError::Platform(format!("PressMouseButton 失败：{err}")))?;
        api.ReleaseMouseButton(1)
            .map_err(|err| AutomationError::Platform(format!("ReleaseMouseButton 失败：{err}")))?;
        Ok(())
    })
}

impl DesktopPlatform for GhostboxMouseDesktop {
    fn launch_wecom(&self) -> Result<(), AutomationError> {
        self.inner.launch_wecom()
    }

    fn focus_wecom(&self) -> Result<Rect, AutomationError> {
        self.inner.focus_wecom()
    }

    fn resize_wecom(&self, width: i32, height: i32) -> Result<Rect, AutomationError> {
        self.inner.resize_wecom(width, height)
    }

    fn is_responsive(&self) -> Result<bool, AutomationError> {
        self.inner.is_responsive()
    }

    fn measure_target_window(&self) -> Result<(Rect, ScreenMetrics), AutomationError> {
        self.inner.measure_target_window()
    }

    fn screen_metrics(&self) -> Result<ScreenMetrics, AutomationError> {
        self.inner.screen_metrics()
    }

    fn capture(&self, region: Rect) -> Result<Screenshot, AutomationError> {
        self.inner.capture(region)
    }

    fn guarded_click(&self, target: Point, expected_window: Rect) -> Result<(), AutomationError> {
        // 先让内层做窗口守卫（聚焦/校验），再改用 GhostBox 真正点下去。
        // 内层 guarded_click 会走 OS 鼠标；这里拆成：校验用 focus + 量窗，点击用 HID。
        let current = self.inner.focus_wecom()?;
        if current.width != expected_window.width || current.height != expected_window.height {
            return Err(AutomationError::ScreenChanged);
        }
        #[cfg(windows)]
        {
            ghost_move_to(target)?;
            // 移动后再确认前台。
            let _ = self.inner.focus_wecom()?;
            ghost_left_click()
        }
        #[cfg(not(windows))]
        {
            let _ = target;
            Err(AutomationError::Platform(
                "幽灵盒鼠标仅支持 Windows（需要 gbilmd64.dll）".into(),
            ))
        }
    }

    fn move_pointer(&self, target: Point) -> Result<(), AutomationError> {
        #[cfg(windows)]
        {
            ghost_move_to(target)
        }
        #[cfg(not(windows))]
        {
            let _ = target;
            Err(AutomationError::Platform(
                "幽灵盒鼠标仅支持 Windows（需要 gbilmd64.dll）".into(),
            ))
        }
    }

    fn scroll(
        &self,
        at: Point,
        notches: i32,
        expected_window: Rect,
    ) -> Result<(), AutomationError> {
        // 滚轮仍走内层 OS；Windows 上先用 GhostBox 把光标移到落点。
        #[cfg(windows)]
        {
            ghost_move_to(at)?;
        }
        self.inner.scroll(at, notches, expected_window)
    }

    fn paste_text(&self, text: &str, expected_window: Rect) -> Result<(), AutomationError> {
        self.inner.paste_text(text, expected_window)
    }

    fn type_text(&self, text: &str, expected_window: Rect) -> Result<(), AutomationError> {
        self.inner.type_text(text, expected_window)
    }

    fn clear_text_field(&self, expected_window: Rect) -> Result<(), AutomationError> {
        self.inner.clear_text_field(expected_window)
    }
}
