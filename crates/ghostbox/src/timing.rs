//! High-resolution wait helpers for HID replay scheduling (Windows).

use std::time::{Duration, Instant};

/// Lower Windows multimedia timer resolution to 1 ms for the guard's lifetime.
pub struct TimerResolutionGuard {
    active: bool,
}

impl TimerResolutionGuard {
    /// Call `timeBeginPeriod(1)` on Windows; no-op elsewhere.
    pub fn acquire() -> Self {
        #[cfg(windows)]
        {
            // SAFETY: winmm `timeBeginPeriod` is a process-wide hint; matched by Drop/`timeEndPeriod`.
            let rc = unsafe { winmm::timeBeginPeriod(1) };
            Self { active: rc == 0 }
        }
        #[cfg(not(windows))]
        {
            Self { active: false }
        }
    }
}

impl Drop for TimerResolutionGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        #[cfg(windows)]
        {
            // SAFETY: pairs with successful `timeBeginPeriod(1)` in `acquire`.
            unsafe {
                let _ = winmm::timeEndPeriod(1);
            }
        }
    }
}

#[cfg(windows)]
mod winmm {
    #[link(name = "winmm")]
    extern "system" {
        pub fn timeBeginPeriod(u_period: u32) -> u32;
        pub fn timeEndPeriod(u_period: u32) -> u32;
    }
}

/// Sleep until `deadline`, sleeping coarsely then spinning the last ~1–2 ms.
pub fn wait_until(deadline: Instant) {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        let remaining = deadline.saturating_duration_since(now);
        if remaining > Duration::from_millis(2) {
            std::thread::sleep(remaining - Duration::from_millis(1));
        } else {
            while Instant::now() < deadline {
                std::hint::spin_loop();
            }
            return;
        }
    }
}

/// Short paced wait used between oversized relative chunks (~0.5–1 ms).
pub fn wait_chunk_gap() {
    wait_until(Instant::now() + Duration::from_micros(750));
}

/// Human-like gap when consecutive samples share the same timestamp (~1–2 ms).
pub fn default_zero_dt_gap() -> Duration {
    Duration::from_micros(1500)
}