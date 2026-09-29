//! Fallback stub for platforms without a HID capture backend.

use crate::hid::HidEvent;
use std::time::Instant;

/// UI / status label for the inactive HID backend.
pub fn backend_label() -> &'static str {
    "HID: unavailable"
}

/// No-op session that records nothing.
pub struct HidSession {
    status: String,
    epoch: Instant,
}

impl HidSession {
    /// Start stub capture. Always succeeds; never collects events.
    pub fn start() -> Result<Self, String> {
        Ok(Self {
            status: "HID unavailable on this platform — pixel recording only.".into(),
            epoch: Instant::now(),
        })
    }

    pub fn status_message(&self) -> &str {
        &self.status
    }

    pub fn epoch(&self) -> Instant {
        self.epoch
    }

    /// Stop and return events in `[start_ts, end_ts]` (always empty on stub).
    pub fn stop_and_slice(self, _start_ts: f64, _end_ts: f64) -> Vec<HidEvent> {
        Vec::new()
    }
}
