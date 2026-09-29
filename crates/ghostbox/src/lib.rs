//! GhostBox (幽灵盒子) hardware mouse SDK binding and HID sequence replay.
//!
//! Dynamically loads `gbilmd64.dll` (vendor symbols match the DEMO `ghostboxm.rs`).
//! Place the DLL beside the executable (or pass an absolute path).
//!
//! # Hang note
//! Vendor `OpenDevice` can block forever when the device is unplugged. Always use
//! [`api::open_device_guarded`] (CloseDevice + ResetDevice + timeout) rather than raw `OpenDevice`.

pub mod api;
pub mod error;
pub mod replay;
pub mod timing;

pub use api::{
    open_device_guarded, reset_shared_device_session, shared_device_session, GBMAPI,
    MOUSE_BUTTON_LEFT, MOUSE_BUTTON_MIDDLE, MOUSE_BUTTON_RIGHT, OPEN_DEVICE_TIMEOUT,
};
pub use error::GhostboxError;
pub use replay::{
    replay_hid_sequence, replay_hid_sequence_with_progress, HidStep, ProgressFn, ReplayReport,
    ReplayRequest,
};
