//! VhidLab 鼠标适配：把 `DesktopPlatform` 的移动/点击/滚动落到虚拟 HID。
//!
//! ★ 仅 Windows；进程级 `shared_device` 开一次。
//! ★ **不修改** `ghostbox_mouse` / `crates/ghostbox`；本文件是并列后端。
//! ★ 绝对移动走 crate 内贝塞尔 + GetCursorPos 闭环（厂商 GhostBox 的曲线在 DLL 里）。
//! ★ 键盘仍委托内层 OS 实现（与 GhostBox 后端一致）。

use std::sync::Arc;

use automation_core::{
    AutomationError, DesktopPlatform, Point, Rect, ScreenMetrics, Screenshot,
};

/// 包装真实桌面端口：鼠标相关走 VhidLab，其余委托内层。
pub struct VhidlabMouseDesktop {
    pub inner: Arc<dyn DesktopPlatform>,
}

impl VhidlabMouseDesktop {
    pub fn new(inner: Arc<dyn DesktopPlatform>) -> Self {
        Self { inner }
    }
}

#[cfg(windows)]
fn with_dev<T>(
    f: impl FnOnce(&vhidlab::VhidDevice) -> Result<T, AutomationError>,
) -> Result<T, AutomationError> {
    let dev = vhidlab::shared_device()
        .map_err(|err| AutomationError::Platform(format!("打开 VhidLab 失败：{err}")))?;
    f(dev.as_ref())
}

#[cfg(windows)]
fn vhid_move_to(target: Point) -> Result<(), AutomationError> {
    with_dev(|dev| {
        vhidlab::move_to(dev, target.x, target.y)
            .map_err(|err| AutomationError::Platform(format!("VhidLab move_to 失败：{err}")))
    })
}

#[cfg(windows)]
fn vhid_left_click() -> Result<(), AutomationError> {
    with_dev(|dev| {
        dev.click_button(vhidlab::MOUSE_BUTTON_LEFT)
            .map_err(|err| AutomationError::Platform(format!("VhidLab 左键失败：{err}")))
    })
}

#[cfg(windows)]
fn vhid_right_click() -> Result<(), AutomationError> {
    with_dev(|dev| {
        dev.click_button(vhidlab::MOUSE_BUTTON_RIGHT)
            .map_err(|err| AutomationError::Platform(format!("VhidLab 右键失败：{err}")))
    })
}

/// VHF 每「仓库格」折合多少 HID 滚轮单位。
///
/// 实测（任务 06231c04）：单包大 Z 几乎不滚；拆包后单格仍比幽灵盒弱，
/// 连发小包已修好「几乎不滚」；增益改回 1，避免滚得过快（任务反馈）。
#[cfg(windows)]
const VHID_NOTCH_GAIN: i32 = 1;

#[cfg(windows)]
fn vhid_scroll(notches: i32) -> Result<(), AutomationError> {
    if notches == 0 {
        return Ok(());
    }
    // 仓库 notches>0 = 向下；先乘增益，再按 ±127 分段转成 GhostBox 向的 Z。
    let mut remaining = notches.saturating_mul(VHID_NOTCH_GAIN);
    with_dev(|dev| {
        while remaining != 0 {
            let chunk = remaining.clamp(vhidlab::WHEEL_MIN, vhidlab::WHEEL_MAX);
            let z = vhidlab::notches_to_wheel_z(chunk);
            dev.wheel(z).map_err(|err| {
                AutomationError::Platform(format!("VhidLab 滚轮(chunk={chunk}, z={z}) 失败：{err}"))
            })?;
            remaining -= chunk;
        }
        Ok(())
    })
}

impl DesktopPlatform for VhidlabMouseDesktop {
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
        let _ = self.inner.measure_target_window()?;
        let _ = expected_window;
        #[cfg(windows)]
        {
            vhid_move_to(target)?;
            vhid_left_click()
        }
        #[cfg(not(windows))]
        {
            let _ = target;
            Err(AutomationError::Platform(
                "VhidLab 鼠标仅支持 Windows（需要已安装的 vhidlab 驱动）".into(),
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
            vhid_move_to(target)?;
            vhid_right_click()
        }
        #[cfg(not(windows))]
        {
            let _ = target;
            Err(AutomationError::Platform(
                "VhidLab 鼠标仅支持 Windows（需要已安装的 vhidlab 驱动）".into(),
            ))
        }
    }

    fn list_peer_top_windows(&self) -> Result<Vec<automation_core::PeerTopWindow>, AutomationError> {
        self.inner.list_peer_top_windows()
    }

    fn move_pointer(&self, target: Point) -> Result<(), AutomationError> {
        #[cfg(windows)]
        {
            vhid_move_to(target)
        }
        #[cfg(not(windows))]
        {
            let _ = target;
            Err(AutomationError::Platform(
                "VhidLab 鼠标仅支持 Windows（需要已安装的 vhidlab 驱动）".into(),
            ))
        }
    }

    fn scroll(
        &self,
        at: Point,
        notches: i32,
        expected_window: Rect,
    ) -> Result<(), AutomationError> {
        let _ = expected_window;
        #[cfg(windows)]
        {
            let _ = self.inner.measure_target_window()?;
            vhid_move_to(at)?;
            return vhid_scroll(notches);
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
