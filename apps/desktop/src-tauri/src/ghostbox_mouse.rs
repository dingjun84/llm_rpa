//! GhostBox 鼠标适配：把 `DesktopPlatform` 的移动/点击/滚动落到 HID。
//!
//! ★ 仅 Windows；进程级 `shared_device_session` 开一次，**不**在每次操作后 CloseDevice。
//! ★ 滚轮 Windows 上走 `MoveMouseWheel`（官方：Z 正上负下，返回 1=成功）。
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
        let code = api
            .PressAndReleaseMouseButton(ghostbox::MOUSE_BUTTON_LEFT)
            .map_err(|err| {
                AutomationError::Platform(format!("PressAndReleaseMouseButton(左) 失败：{err}"))
            })?;
        ghostbox::ensure_mouse_button_ok("PressAndReleaseMouseButton(1=左)", code).map_err(
            |err| AutomationError::Platform(format!("左键未成功：{err}")),
        )?;
        Ok(())
    })
}

#[cfg(windows)]
fn ghost_right_click() -> Result<(), AutomationError> {
    with_api(|api| {
        // 官方：3=右键；用 PressAndRelease（按下→释放延时由 SetMouseMovementDelay 控制）。
        let code = api
            .PressAndReleaseMouseButton(ghostbox::MOUSE_BUTTON_RIGHT)
            .map_err(|err| {
                AutomationError::Platform(format!("PressAndReleaseMouseButton(右) 失败：{err}"))
            })?;
        ghostbox::ensure_mouse_button_ok("PressAndReleaseMouseButton(3=右)", code).map_err(
            |err| AutomationError::Platform(format!("右键未成功：{err}")),
        )?;
        Ok(())
    })
}

/// RPA `notches>0` = 向下滚内容 → GhostBox `MoveMouseWheel` 的 Z 为负（官方：正上负下）。
#[cfg(windows)]
fn ghost_scroll(notches: i32) -> Result<(), AutomationError> {
    if notches == 0 {
        return Ok(());
    }
    let z = ghostbox::notches_to_wheel_z(notches);
    with_api(|api| {
        let code = api.MoveMouseWheel(z).map_err(|err| {
            AutomationError::Platform(format!("MoveMouseWheel({z}) 失败：{err}"))
        })?;
        ghostbox::ensure_mouse_wheel_ok("MoveMouseWheel", code).map_err(|err| {
            AutomationError::Platform(format!(
                "滚轮未成功：MoveMouseWheel({z}) → {err}（期望返回 1）"
            ))
        })?;
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
        // 不强制 focus 主窗：同进程菜单/转发窗被拉主窗前台时会关掉。
        // 任务开始时已 focus；此处只量窗确认目标进程仍在，再 HID 点击。
        let _ = self.inner.measure_target_window()?;
        let _ = expected_window;
        #[cfg(windows)]
        {
            ghost_move_to(target)?;
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

    fn guarded_right_click(
        &self,
        target: Point,
        expected_window: Rect,
    ) -> Result<(), AutomationError> {
        let _ = self.inner.measure_target_window()?;
        let _ = expected_window;
        #[cfg(windows)]
        {
            ghost_move_to(target)?;
            ghost_right_click()
        }
        #[cfg(not(windows))]
        {
            let _ = target;
            Err(AutomationError::Platform(
                "幽灵盒鼠标仅支持 Windows（需要 gbilmd64.dll）".into(),
            ))
        }
    }

    fn list_peer_top_windows(&self) -> Result<Vec<automation_core::PeerTopWindow>, AutomationError> {
        self.inner.list_peer_top_windows()
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
        // Windows：移动 + 滚轮都走幽灵盒 HID（MoveMouseWheel；Z 正上负下）。
        // 非 Windows：仍委托内层。
        let _ = expected_window;
        #[cfg(windows)]
        {
            let _ = self.inner.measure_target_window()?;
            ghost_move_to(at)?;
            return ghost_scroll(notches);
        }
        #[cfg(not(windows))]
        {
            self.inner.scroll(at, notches, expected_window)
        }
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
