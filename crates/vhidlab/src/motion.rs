//! Bezier `move_to` with closed-loop cursor correction (Windows).

use std::time::{Duration, Instant};

use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use crate::bezier::plan_path;
use crate::device::VhidDevice;
use crate::error::VhidlabError;

const MOVE_TIMEOUT: Duration = Duration::from_secs(2);
const STEP_PAUSE: Duration = Duration::from_millis(12);
const FINAL_TOLERANCE: i32 = 3;

fn cursor_pos() -> Result<(i32, i32), VhidlabError> {
    let mut pt = POINT { x: 0, y: 0 };
    unsafe {
        GetCursorPos(&mut pt).map_err(|e| VhidlabError::CursorPos {
            code: e.code().0 as u32,
        })?;
    }
    Ok((pt.x, pt.y))
}

fn jitter_from_coords(x: i32, y: i32) -> f64 {
    // Deterministic 0..1 from coords so paths are reproducible per start/end.
    let h = (x as u32).wrapping_mul(7481) ^ (y as u32).wrapping_mul(9973);
    (h % 10_000) as f64 / 10_000.0
}

/// Move pointer to absolute screen `(tx, ty)` via Bezier relative HID + closed loop.
///
/// Mirrors GhostBox `MoveMouseTo` usage from the RPA layer: callers pass screen
/// coordinates; internals emit ±127 relative reports only.
pub fn move_to(dev: &VhidDevice, tx: i32, ty: i32) -> Result<(), VhidlabError> {
    let start = Instant::now();
    let (sx, sy) = cursor_pos()?;
    if (sx - tx).abs() <= FINAL_TOLERANCE && (sy - ty).abs() <= FINAL_TOLERANCE {
        return Ok(());
    }

    let jitter = jitter_from_coords(sx ^ tx, sy ^ ty);
    let path = plan_path((sx, sy), (tx, ty), jitter);

    for pt in &path {
        if start.elapsed() > MOVE_TIMEOUT {
            let (cx, cy) = cursor_pos()?;
            return Err(VhidlabError::MoveTimeout {
                tx,
                ty,
                cx,
                cy,
                elapsed_ms: start.elapsed().as_millis() as u64,
            });
        }
        let (cx, cy) = cursor_pos()?;
        let dx = pt.x - cx;
        let dy = pt.y - cy;
        if dx == 0 && dy == 0 {
            continue;
        }
        // Cap per-sample jump so pointer acceleration doesn't overshoot wildly.
        let dx = dx.clamp(-18, 18);
        let dy = dy.clamp(-18, 18);
        dev.move_relative_chunked(dx, dy)?;
        std::thread::sleep(STEP_PAUSE);
    }

    // Final closed-loop snap (pointer acceleration makes open-loop incomplete).
    loop {
        if start.elapsed() > MOVE_TIMEOUT {
            let (cx, cy) = cursor_pos()?;
            return Err(VhidlabError::MoveTimeout {
                tx,
                ty,
                cx,
                cy,
                elapsed_ms: start.elapsed().as_millis() as u64,
            });
        }
        let (cx, cy) = cursor_pos()?;
        let dx = tx - cx;
        let dy = ty - cy;
        if dx.abs() <= FINAL_TOLERANCE && dy.abs() <= FINAL_TOLERANCE {
            return Ok(());
        }
        // Small steps near the end for precision under acceleration.
        let step_x = dx.clamp(-8, 8);
        let step_y = dy.clamp(-8, 8);
        if step_x == 0 && step_y == 0 {
            // Stuck at non-zero error with clamp-to-zero? force at least 1 count.
            let fx = dx.signum().clamp(-1, 1);
            let fy = dy.signum().clamp(-1, 1);
            dev.move_relative_chunked(fx, fy)?;
        } else {
            dev.move_relative_chunked(step_x, step_y)?;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub fn current_cursor() -> Result<(i32, i32), VhidlabError> {
    cursor_pos()
}
