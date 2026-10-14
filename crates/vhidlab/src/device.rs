//! Windows DeviceIoControl client for `\\.\VhidLab`.

use std::sync::{Arc, Mutex, OnceLock};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;

use crate::error::VhidlabError;
use crate::protocol::{
    button_id_to_bits, wheel_report_steps, MouseIn, AXIS_MAX, AXIS_MIN, IOCTL_MOUSE_BUTTON,
    IOCTL_MOUSE_MOVE, IOCTL_MOUSE_WHEEL, WHEEL_MAX, WHEEL_MIN,
};

/// Process-wide open handle (open once, like GhostBox `shared_device_session`).
static SHARED: OnceLock<Mutex<Option<Arc<VhidDevice>>>> = OnceLock::new();

fn slot() -> &'static Mutex<Option<Arc<VhidDevice>>> {
    SHARED.get_or_init(|| Mutex::new(None))
}

/// Opened `\\.\VhidLab` device.
pub struct VhidDevice {
    handle: HANDLE,
}

// SAFETY: HANDLE is used only under Windows APIs; we serialize via Mutex for shared session.
unsafe impl Send for VhidDevice {}
unsafe impl Sync for VhidDevice {}

impl Drop for VhidDevice {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

impl VhidDevice {
    /// Open `\\.\VhidLab`.
    pub fn open() -> Result<Self, VhidlabError> {
        let path: Vec<u16> = "\\\\.\\VhidLab\0".encode_utf16().collect();
        let handle = unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                (GENERIC_READ.0 | GENERIC_WRITE.0),
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }
        .map_err(|e| VhidlabError::OpenDevice {
            code: e.code().0 as u32,
        })?;
        if handle == INVALID_HANDLE_VALUE {
            return Err(VhidlabError::OpenDevice { code: 0 });
        }
        Ok(Self { handle })
    }

    fn ioctl(&self, code: u32, op: &'static str, mouse: &MouseIn) -> Result<(), VhidlabError> {
        let mut bytes = 0u32;
        let result = unsafe {
            DeviceIoControl(
                self.handle,
                code,
                Some((mouse as *const MouseIn).cast::<core::ffi::c_void>()),
                std::mem::size_of::<MouseIn>() as u32,
                None,
                0,
                Some(&mut bytes as *mut u32),
                None,
            )
        };
        if let Err(err) = result {
            return Err(VhidlabError::Ioctl {
                op,
                code: err.code().0 as u32,
            });
        }
        Ok(())
    }

    /// Relative move one HID report (must already be within ±127).
    pub fn move_relative(&self, dx: i32, dy: i32) -> Result<(), VhidlabError> {
        if !(AXIS_MIN..=AXIS_MAX).contains(&dx) || !(AXIS_MIN..=AXIS_MAX).contains(&dy) {
            return Err(VhidlabError::AxisOutOfRange { dx, dy });
        }
        self.ioctl(
            IOCTL_MOUSE_MOVE,
            "MOUSE_MOVE",
            &MouseIn::relative(dx as i8, dy as i8),
        )
    }

    /// Emit relative move, chunking oversized deltas.
    pub fn move_relative_chunked(&self, dx: i32, dy: i32) -> Result<usize, VhidlabError> {
        let chunks = crate::bezier::chunk_relative(dx, dy);
        for (sx, sy) in &chunks {
            self.move_relative(*sx as i32, *sy as i32)?;
            // Short paced gap between oversized chunks (~0.5–1 ms), mirrors GhostBox wait_chunk_gap.
            std::thread::sleep(std::time::Duration::from_micros(700));
        }
        Ok(chunks.len())
    }

    /// Click (press+release) using GhostBox-style button id 1/2/3.
    pub fn click_button(&self, button_id: i32) -> Result<(), VhidlabError> {
        let bits = button_id_to_bits(button_id).ok_or(VhidlabError::Ioctl {
            op: "MOUSE_BUTTON",
            code: 0,
        })?;
        self.ioctl(IOCTL_MOUSE_BUTTON, "MOUSE_BUTTON_CLICK", &MouseIn::click(bits))
    }

    /// Wheel Z: positive = up, negative = down (GhostBox convention).
    ///
    /// ★ 不要一次塞很大的 |Z|：Windows 对 VHF 滚轮时常把单包大值当成「几乎一格」，
    /// 表现为滚不动 / 幅度极小。这里拆成多次小包（每包最多 [`WHEEL_REPORT_MAX`]），
    /// 更接近真鼠标连滚。调用方若需要 |Z|>127，先自行分段再调本函数。
    pub fn wheel(&self, z: i32) -> Result<(), VhidlabError> {
        if !(WHEEL_MIN..=WHEEL_MAX).contains(&z) {
            return Err(VhidlabError::WheelOutOfRange { z });
        }
        if z == 0 {
            return Ok(());
        }
        for step in wheel_report_steps(z) {
            self.ioctl(IOCTL_MOUSE_WHEEL, "MOUSE_WHEEL", &MouseIn::wheel(step))?;
            // ~4ms：够 HID 栈吃下一包，整段 120 格也不会拖到秒级。
            std::thread::sleep(std::time::Duration::from_millis(4));
        }
        Ok(())
    }
}

/// Get or open the process-wide device session.
pub fn shared_device() -> Result<Arc<VhidDevice>, VhidlabError> {
    let mut guard = slot().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(dev) = guard.as_ref() {
        return Ok(Arc::clone(dev));
    }
    let dev = Arc::new(VhidDevice::open()?);
    *guard = Some(Arc::clone(&dev));
    Ok(dev)
}

/// Drop the shared session (next call re-opens).
pub fn reset_shared_device() {
    let mut guard = slot().lock().unwrap_or_else(|e| e.into_inner());
    *guard = None;
}

pub fn shared_is_open() -> bool {
    slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some()
}
