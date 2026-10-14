//! VhidLab virtual HID client for llm_rpa.
//!
//! Talks to the self-built KMDF+VHF driver via `\\\\.\\VhidLab`.
//! Relative moves are limited to ±127 (same as GhostBox `MoveMouseRelative`).
//! Absolute `move_to` is implemented in software with a cubic Bezier path and
//! closed-loop `GetCursorPos` correction (GhostBox does Bezier inside the vendor DLL).

pub mod bezier;
pub mod error;
pub mod protocol;

#[cfg(windows)]
mod device;
#[cfg(windows)]
mod motion;

pub use error::VhidlabError;
pub use protocol::{
    button_id_to_bits, notches_to_wheel_z, wheel_report_steps, MouseIn, AXIS_MAX, AXIS_MIN,
    BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, MOUSE_BUTTON_LEFT, MOUSE_BUTTON_MIDDLE, MOUSE_BUTTON_RIGHT,
    WHEEL_MAX, WHEEL_MIN,
};

#[cfg(windows)]
pub use device::{reset_shared_device, shared_device, shared_is_open, VhidDevice};
#[cfg(windows)]
pub use motion::{current_cursor, move_to};
