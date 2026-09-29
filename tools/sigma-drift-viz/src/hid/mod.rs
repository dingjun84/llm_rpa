//! HID mouse capture for Record-tab hardware-replay samples.
//!
//! - **macOS:** IOKit `IOHIDManager` input-value callbacks (relative X/Y + buttons).
//! - **Windows:** Raw Input (`WM_INPUT`) via a message-only HWND on a worker
//!   thread (relative X/Y + button bitfield; works alongside eframe).
//!
//! ## Permissions (macOS)
//! Opening mouse HID devices usually works without Accessibility. On some
//! macOS versions / Input Monitoring policies, `IOHIDManagerOpen` may return
//! `kIOReturnNotPermitted`. In that case we fail gracefully: pixel/egui
//! recording continues and the Record status shows the HID error.
//!
//! Grant **Input Monitoring** (System Settings → Privacy & Security) to the
//! terminal or app that launches `sigma-drift-viz` if HID capture is empty
//! or reports a permission error. Accessibility is not required for
//! non-seize `IOHIDManagerOpen`.
//!
//! ## Windows notes
//! Capture uses `RIDEV_INPUTSINK` so reports arrive even when the egui window
//! is not focused. No special privacy grant is required for Raw Input mouse.

mod platform;

pub use platform::{backend_label, HidSession};

use serde::{Deserialize, Serialize};

/// One coalesced HID mouse sample for later hardware replay.
///
/// `t_ms` is milliseconds since the Record Start instant (same epoch as
/// `RecordedPoint::t_ms`). `dx`/`dy` are relative report units; `buttons`
/// is a bitfield (bit0 = button1/primary, bit1 = button2, …).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HidEvent {
    pub t_ms: f64,
    pub dx: i32,
    pub dy: i32,
    pub buttons: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hid_event_json_shape() {
        let ev = HidEvent {
            t_ms: 1.5,
            dx: -2,
            dy: 3,
            buttons: 0x01,
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["t_ms"], 1.5);
        assert_eq!(v["dx"], -2);
        assert_eq!(v["dy"], 3);
        assert_eq!(v["buttons"], 1);
    }

    #[test]
    fn missing_hid_events_defaults_empty() {
        #[derive(serde::Deserialize)]
        struct Wrap {
            #[serde(default)]
            hid_events: Vec<HidEvent>,
        }
        let w: Wrap = serde_json::from_str("{}").unwrap();
        assert!(w.hid_events.is_empty());
    }
}
