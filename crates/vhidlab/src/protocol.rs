//! Wire constants shared with the KMDF driver (`vhidlab_protocol.h`).

/// CTL_CODE(FILE_DEVICE_UNKNOWN=0x22, fn, METHOD_BUFFERED=0, FILE_ANY_ACCESS=0)
pub const fn ioctl(fn_code: u32) -> u32 {
    (0x22u32 << 16) | (fn_code << 2)
}

pub const IOCTL_MOUSE_MOVE: u32 = ioctl(0x800);
pub const IOCTL_MOUSE_BUTTON: u32 = ioctl(0x801);
pub const IOCTL_MOUSE_WHEEL: u32 = ioctl(0x802);
pub const IOCTL_KEY_EVENT: u32 = ioctl(0x803);

pub const BTN_LEFT: u8 = 0x01;
pub const BTN_RIGHT: u8 = 0x02;
pub const BTN_MIDDLE: u8 = 0x04;
pub const FLAG_CLICK: u8 = 0x01;

/// GhostBox-compatible button ids: 1=left, 2=middle, 3=right.
pub const MOUSE_BUTTON_LEFT: i32 = 1;
pub const MOUSE_BUTTON_MIDDLE: i32 = 2;
pub const MOUSE_BUTTON_RIGHT: i32 = 3;

pub const AXIS_MIN: i32 = -127;
pub const AXIS_MAX: i32 = 127;
pub const WHEEL_MIN: i32 = -127;
pub const WHEEL_MAX: i32 = 127;

#[repr(C, packed)]
#[derive(Clone, Copy, Debug)]
pub struct MouseIn {
    pub dx: i8,
    pub dy: i8,
    pub wheel: i8,
    pub buttons: u8,
    pub is_down: u8,
    pub flags: u8,
}

impl MouseIn {
    pub fn relative(dx: i8, dy: i8) -> Self {
        Self {
            dx,
            dy,
            wheel: 0,
            buttons: 0,
            is_down: 0,
            flags: 0,
        }
    }

    pub fn wheel(z: i8) -> Self {
        Self {
            dx: 0,
            dy: 0,
            wheel: z,
            buttons: 0,
            is_down: 0,
            flags: 0,
        }
    }

    pub fn click(buttons: u8) -> Self {
        Self {
            dx: 0,
            dy: 0,
            wheel: 0,
            buttons,
            is_down: 1,
            flags: FLAG_CLICK,
        }
    }

    pub fn press(buttons: u8) -> Self {
        Self {
            dx: 0,
            dy: 0,
            wheel: 0,
            buttons,
            is_down: 1,
            flags: 0,
        }
    }

    pub fn release() -> Self {
        Self {
            dx: 0,
            dy: 0,
            wheel: 0,
            buttons: 0,
            is_down: 0,
            flags: 0,
        }
    }
}

/// Map GhostBox-style button id (1/2/3) to HID button bits.
pub fn button_id_to_bits(id: i32) -> Option<u8> {
    match id {
        MOUSE_BUTTON_LEFT => Some(BTN_LEFT),
        MOUSE_BUTTON_MIDDLE => Some(BTN_MIDDLE),
        MOUSE_BUTTON_RIGHT => Some(BTN_RIGHT),
        _ => None,
    }
}

/// RPA `notches>0` = scroll content down → HID wheel Z negative (GhostBox convention).
pub fn notches_to_wheel_z(notches: i32) -> i32 {
    (-notches).clamp(WHEEL_MIN, WHEEL_MAX)
}

/// Plan per-report wheel steps for a GhostBox-signed Z (may be any i32; clamped piecewise by caller).
/// Each step is in [-3, 3] excluding 0, same sign as z.
pub fn wheel_report_steps(z: i32) -> Vec<i8> {
    const MAX: i32 = 1;
    if z == 0 {
        return Vec::new();
    }
    let sign = if z > 0 { 1i32 } else { -1 };
    let mut left = z.abs();
    let mut out = Vec::new();
    while left > 0 {
        let step = left.min(MAX);
        out.push((sign * step) as i8);
        left -= step;
    }
    out
}
