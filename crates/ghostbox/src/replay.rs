//! Timed HID relative-sequence replay through GhostBox mouse APIs.

use crate::api::{
    append_replay_log, GBMAPI, MOUSE_BUTTON_LEFT, MOUSE_BUTTON_MIDDLE, MOUSE_BUTTON_RIGHT,
};
use crate::error::GhostboxError;
use crate::timing::{self, TimerResolutionGuard};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// One coalesced HID sample (relative counts + optional button bitfield).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HidStep {
    /// Timestamp in ms from sequence start (or any shared timeline).
    pub t_ms: u64,
    /// Relative X counts (HID report units).
    pub dx: i32,
    /// Relative Y counts.
    pub dy: i32,
    /// Button bitfield; bit0=left, bit1=right, bit2=middle. Unchanged bits are no-ops.
    #[serde(default)]
    pub buttons: u8,
}

/// Optional mid-replay progress: `(emitted_so_far, coalesced_total)`.
pub type ProgressFn<'a> = dyn FnMut(usize, usize) + Send + 'a;

/// Request to replay a relative HID path, optionally snapping to an absolute end.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplayRequest {
    pub steps: Vec<HidStep>,
    /// Absolute screen position to MoveMouseTo before relative playback.
    #[serde(default)]
    pub start_pos: Option<(i32, i32)>,
    /// If true (default), call `MoveMouseTo(start)` when `start_pos` is `Some`.
    #[serde(default = "default_true")]
    pub move_to_start: bool,
    /// Final desired screen position after relative playback.
    #[serde(default)]
    pub final_pos: Option<(i32, i32)>,
    /// If true (default), call `MoveMouseTo(final)` when `final_pos` is `Some`.
    #[serde(default = "default_true")]
    pub snap_final: bool,
    /// If true (default), skip per-step timeline waits (minimal/no gap between emits).
    #[serde(default = "default_true")]
    pub fast: bool,
}

fn default_true() -> bool {
    true
}

impl Default for ReplayRequest {
    fn default() -> Self {
        Self {
            steps: Vec::new(),
            start_pos: None,
            move_to_start: true,
            final_pos: None,
            snap_final: true,
            fast: true,
        }
    }
}

/// Summary returned by [`replay_hid_sequence`].
#[derive(Clone, Debug, Serialize)]
pub struct ReplayReport {
    pub steps_in: usize,
    pub steps_coalesced: usize,
    pub move_calls: usize,
    pub button_events: usize,
    pub duration_ms: u64,
    pub mouse_x: i32,
    pub mouse_y: i32,
    pub moved_to_start: bool,
    pub snapped_final: bool,
}

const MAX_HID_AXIS: i32 = 127;
/// Merge consecutive same-button steps whose `t_ms` span is within this window (timed mode).
const COALESCE_WINDOW_MS: u64 = 8;
/// Report progress at least every N coalesced emits.
const PROGRESS_EVERY_EMITS: usize = 25;
/// Or at least every this many milliseconds of wall time.
const PROGRESS_EVERY_MS: u128 = 250;
/// Settle delay after MoveMouseTo(start) before relative playback.
const START_SETTLE_MS: u64 = 150;

/// Replay `req.steps` with timeline waits, chunking, and optional final snap.
pub fn replay_hid_sequence(
    api: &GBMAPI,
    req: &ReplayRequest,
) -> Result<ReplayReport, GhostboxError> {
    replay_hid_sequence_with_progress(api, req, None)
}

/// Like [`replay_hid_sequence`], with optional mid-replay progress callback.
pub fn replay_hid_sequence_with_progress(
    api: &GBMAPI,
    req: &ReplayRequest,
    progress: Option<&mut ProgressFn<'_>>,
) -> Result<ReplayReport, GhostboxError> {
    let result = replay_hid_sequence_inner(api, req, progress);
    if let Err(err) = &result {
        append_replay_log(&format!("replay_hid_sequence: error: {err}"));
    }
    result
}

fn replay_hid_sequence_inner(
    api: &GBMAPI,
    req: &ReplayRequest,
    mut progress: Option<&mut ProgressFn<'_>>,
) -> Result<ReplayReport, GhostboxError> {
    if req.steps.is_empty() {
        return Err(GhostboxError::EmptySequence);
    }

    let first = &req.steps[0];
    let last = req.steps.last().expect("non-empty sequence");
    append_replay_log(&format!(
        "replay_hid_sequence: steps={}, first=(t_ms={},dx={},dy={},buttons={}), last=(t_ms={},dx={},dy={},buttons={}), snap={}, fast={}, move_to_start={}, start_pos={:?}",
        req.steps.len(),
        first.t_ms,
        first.dx,
        first.dy,
        first.buttons,
        last.t_ms,
        last.dx,
        last.dy,
        last.buttons,
        req.snap_final,
        req.fast,
        req.move_to_start,
        req.start_pos
    ));
    let _timer = TimerResolutionGuard::acquire();
    let wall_origin = Instant::now();

    let mut moved_to_start = false;
    if req.move_to_start {
        if let Some((x, y)) = req.start_pos {
            append_replay_log(&format!("replay_hid_sequence: MoveMouseTo start ({x}, {y})"));
            let _ = api.MoveMouseTo(x, y)?;
            moved_to_start = true;
            timing::wait_until(Instant::now() + Duration::from_millis(START_SETTLE_MS));
        }
    }

    // Coalesce before emit: timed mode uses a small window; fast mode merges all
    // consecutive same-button steps (DLL MoveMouseRelative is slow per call).
    let coalesce_window = if req.fast {
        None
    } else {
        Some(COALESCE_WINDOW_MS)
    };
    let coalesced = coalesce_hid_steps(&req.steps, coalesce_window);
    let coalesced_n = coalesced.len();
    append_replay_log(&format!(
        "replay_hid_sequence: coalesced {} -> {} steps (fast={}, window_ms={:?})",
        req.steps.len(),
        coalesced_n,
        req.fast,
        coalesce_window
    ));

    let origin_t = coalesced[0].t_ms;
    let mut prev_buttons: u8 = 0;
    let mut move_calls = 0usize;
    let mut button_events = 0usize;
    let mut last_emit = Instant::now();
    let mut last_progress_at = Instant::now();

    report_progress(
        &mut progress,
        0,
        coalesced_n,
        &mut last_progress_at,
        /*force=*/ true,
    );

    for (idx, step) in coalesced.iter().enumerate() {
        if !req.fast {
            let target_offset_ms = step.t_ms.saturating_sub(origin_t);
            let mut deadline = wall_origin + Duration::from_millis(target_offset_ms);

            // Zero / missing gaps: keep a human-like ~1–2 ms between emissions.
            if idx > 0 {
                let prev_t = coalesced[idx - 1].t_ms;
                let dt = step.t_ms.saturating_sub(prev_t);
                if dt == 0 {
                    let min_deadline = last_emit + timing::default_zero_dt_gap();
                    if deadline < min_deadline {
                        deadline = min_deadline;
                    }
                }
            }

            timing::wait_until(deadline);
        }
        // fast mode: no timeline wait (DLL latency already paces emits)

        if step.buttons != prev_buttons {
            button_events += apply_button_delta(api, prev_buttons, step.buttons)?;
            prev_buttons = step.buttons;
        }

        if step.dx != 0 || step.dy != 0 {
            move_calls += emit_relative_chunked(api, step.dx, step.dy)?;
        }

        last_emit = Instant::now();
        report_progress(
            &mut progress,
            idx + 1,
            coalesced_n,
            &mut last_progress_at,
            /*force=*/ false,
        );
    }

    // Release any buttons still held at end of sequence.
    if prev_buttons != 0 {
        button_events += apply_button_delta(api, prev_buttons, 0)?;
    }

    let mut snapped_final = false;
    if req.snap_final {
        if let Some((x, y)) = req.final_pos {
            let _ = api.MoveMouseTo(x, y)?;
            snapped_final = true;
        }
    }

    let mouse_x = api.GetMouseX()?;
    let mouse_y = api.GetMouseY()?;
    let duration_ms = wall_origin.elapsed().as_millis() as u64;
    append_replay_log(&format!(
        "replay_hid_sequence: coalesced={coalesced_n}, move_calls={move_calls}, moved_to_start={moved_to_start}, snap={snapped_final}, duration_ms={duration_ms}"
    ));

    report_progress(
        &mut progress,
        coalesced_n,
        coalesced_n,
        &mut last_progress_at,
        /*force=*/ true,
    );

    Ok(ReplayReport {
        steps_in: req.steps.len(),
        steps_coalesced: coalesced_n,
        move_calls,
        button_events,
        duration_ms,
        mouse_x,
        mouse_y,
        moved_to_start,
        snapped_final,
    })
}

fn report_progress(
    progress: &mut Option<&mut ProgressFn<'_>>,
    done: usize,
    total: usize,
    last_at: &mut Instant,
    force: bool,
) {
    let due = force
        || done == total
        || done % PROGRESS_EVERY_EMITS == 0
        || last_at.elapsed().as_millis() >= PROGRESS_EVERY_MS;
    if !due {
        return;
    }
    append_replay_log(&format!("GhostBox replaying {done}/{total} …"));
    if let Some(cb) = progress.as_mut() {
        cb(done, total);
    }
    *last_at = Instant::now();
}

/// Merge consecutive same-button steps.
///
/// - `window_ms = Some(w)`: only merge while `t_ms - group_first.t_ms <= w`
/// - `window_ms = None`: merge all consecutive same-button steps (fast mode)
fn coalesce_hid_steps(steps: &[HidStep], window_ms: Option<u64>) -> Vec<HidStep> {
    let mut out: Vec<HidStep> = Vec::with_capacity(steps.len());
    let mut acc: Option<HidStep> = None;
    let mut group_t0: u64 = 0;

    for step in steps {
        match acc.as_mut() {
            None => {
                group_t0 = step.t_ms;
                acc = Some(step.clone());
            }
            Some(cur) => {
                let same_buttons = cur.buttons == step.buttons;
                let in_window = match window_ms {
                    None => true,
                    Some(w) => step.t_ms.saturating_sub(group_t0) <= w,
                };
                if same_buttons && in_window {
                    cur.dx = cur.dx.saturating_add(step.dx);
                    cur.dy = cur.dy.saturating_add(step.dy);
                    cur.t_ms = step.t_ms;
                } else {
                    out.push(acc.take().expect("acc present"));
                    group_t0 = step.t_ms;
                    acc = Some(step.clone());
                }
            }
        }
    }
    if let Some(cur) = acc {
        out.push(cur);
    }
    out
}

fn emit_relative_chunked(api: &GBMAPI, mut dx: i32, mut dy: i32) -> Result<usize, GhostboxError> {
    let mut calls = 0usize;
    while dx != 0 || dy != 0 {
        let sx = dx.clamp(-MAX_HID_AXIS, MAX_HID_AXIS);
        let sy = dy.clamp(-MAX_HID_AXIS, MAX_HID_AXIS);
        let _ = api.MoveMouseRelative(sx, sy)?;
        calls += 1;
        dx -= sx;
        dy -= sy;
        if dx != 0 || dy != 0 {
            timing::wait_chunk_gap();
        }
    }
    Ok(calls)
}

fn apply_button_delta(api: &GBMAPI, prev: u8, next: u8) -> Result<usize, GhostboxError> {
    let mut events = 0usize;
    let pairs = [
        (0u8, MOUSE_BUTTON_LEFT),
        (1u8, MOUSE_BUTTON_RIGHT),
        (2u8, MOUSE_BUTTON_MIDDLE),
    ];
    let changed = prev ^ next;
    for (bit, btn_id) in pairs {
        let mask = 1u8 << bit;
        if changed & mask == 0 {
            continue;
        }
        if next & mask != 0 {
            let _ = api.PressMouseButton(btn_id)?;
        } else {
            let _ = api.ReleaseMouseButton(btn_id)?;
        }
        events += 1;
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_roundtrip_request() {
        let req = ReplayRequest {
            steps: vec![HidStep {
                t_ms: 0,
                dx: 1,
                dy: -1,
                buttons: 0,
            }],
            start_pos: Some((10, 20)),
            move_to_start: true,
            final_pos: Some((100, 200)),
            snap_final: true,
            fast: true,
        };
        let s = serde_json::to_string(&req).unwrap();
        let back: ReplayRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back.steps.len(), 1);
        assert_eq!(back.start_pos, Some((10, 20)));
        assert_eq!(back.final_pos, Some((100, 200)));
        assert!(back.snap_final);
        assert!(back.fast);
        assert!(back.move_to_start);
    }

    #[test]
    fn coalesce_sums_same_buttons_within_window() {
        let steps = vec![
            HidStep {
                t_ms: 0,
                dx: 1,
                dy: 0,
                buttons: 0,
            },
            HidStep {
                t_ms: 2,
                dx: 1,
                dy: 1,
                buttons: 0,
            },
            HidStep {
                t_ms: 4,
                dx: 1,
                dy: 0,
                buttons: 0,
            },
            HidStep {
                t_ms: 20,
                dx: 5,
                dy: 0,
                buttons: 0,
            },
        ];
        let out = coalesce_hid_steps(&steps, Some(8));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].dx, 3);
        assert_eq!(out[0].dy, 1);
        assert_eq!(out[0].t_ms, 4);
        assert_eq!(out[1].dx, 5);
        assert_eq!(out[1].t_ms, 20);
    }

    #[test]
    fn coalesce_splits_on_button_change() {
        let steps = vec![
            HidStep {
                t_ms: 0,
                dx: 1,
                dy: 0,
                buttons: 0,
            },
            HidStep {
                t_ms: 1,
                dx: 1,
                dy: 0,
                buttons: 1,
            },
            HidStep {
                t_ms: 2,
                dx: 1,
                dy: 0,
                buttons: 1,
            },
        ];
        let out = coalesce_hid_steps(&steps, None);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].buttons, 0);
        assert_eq!(out[0].dx, 1);
        assert_eq!(out[1].buttons, 1);
        assert_eq!(out[1].dx, 2);
    }

    #[test]
    fn clamp_math_preserves_total() {
        let mut dx = 300i32;
        let mut dy = -250i32;
        let mut sx_sum = 0i32;
        let mut sy_sum = 0i32;
        while dx != 0 || dy != 0 {
            let sx = dx.clamp(-127, 127);
            let sy = dy.clamp(-127, 127);
            sx_sum += sx;
            sy_sum += sy;
            dx -= sx;
            dy -= sy;
        }
        assert_eq!(sx_sum, 300);
        assert_eq!(sy_sum, -250);
    }
}
