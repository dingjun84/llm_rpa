//! Errors for the VhidLab user-mode client.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum VhidlabError {
    #[error("打开 \\\\.\\VhidLab 失败（Win32 {code}）：请确认驱动已安装且设备正常")]
    OpenDevice { code: u32 },
    #[error("DeviceIoControl({op}) 失败（Win32 {code}）")]
    Ioctl { op: &'static str, code: u32 },
    #[error("相对移动轴超出 ±127：dx={dx}, dy={dy}")]
    AxisOutOfRange { dx: i32, dy: i32 },
    #[error("滚轮幅度超出 ±127：z={z}")]
    WheelOutOfRange { z: i32 },
    #[error("GetCursorPos 失败（Win32 {code}）")]
    CursorPos { code: u32 },
    #[error("移动超时：目标 ({tx},{ty})，当前 ({cx},{cy})，已用 {elapsed_ms} ms")]
    MoveTimeout {
        tx: i32,
        ty: i32,
        cx: i32,
        cy: i32,
        elapsed_ms: u64,
    },
    #[error("VhidLab 仅支持 Windows")]
    UnsupportedOs,
}
