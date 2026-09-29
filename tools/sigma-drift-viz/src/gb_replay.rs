//! GhostBox (幽灵盒) HID relative replay for the Record tab.
//!
//! Windows builds call `ghostbox::replay_hid_sequence` in-process (loads
//! `gbilmd64.dll` beside the exe, guarded OpenDevice). Non-Windows builds only
//! expose a disabled status string — no DLL linkage path is exercised.

use crate::hid::HidEvent;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

/// Shared progress / error line for the background replay worker.
#[derive(Clone, Debug)]
pub struct GbReplayShared {
    pub busy: bool,
    pub status: String,
}

impl Default for GbReplayShared {
    fn default() -> Self {
        Self {
            busy: false,
            status: String::new(),
        }
    }
}

/// Append one timestamped line beside the running executable.
pub(crate) fn append_replay_log(line: &str) {
    let path = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("ghostbox-replay.log")))
        .unwrap_or_else(|| PathBuf::from("ghostbox-replay.log"));
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_else(|_| "time-before-epoch".into());
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "[{stamp}] {line}");
    }
}

/// Update the UI status and mirror the exact text to the replay log.
fn set_status(shared: &Arc<Mutex<GbReplayShared>>, status: impl Into<String>) {
    let status = status.into();
    append_replay_log(&status);
    let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
    g.status = status;
}

/// Resolve `gbilmd64.dll` next to the running executable (same as ghostbox-play).
pub fn resolve_dll_beside_exe() -> Result<PathBuf, String> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("gbilmd64.dll"));
        }
    }
    candidates.push(PathBuf::from("gbilmd64.dll"));
    for c in &candidates {
        if c.is_file() {
            return Ok(c.clone());
        }
    }
    Err(format!(
        "gbilmd64.dll not found beside exe or in cwd; tried: {}",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// Build a GhostBox [`ghostbox::ReplayRequest`] from viz `hid_events` + start/end.
#[cfg(windows)]
fn build_request(
    hid_events: &[HidEvent],
    start: [f64; 2],
    end: [f64; 2],
    snap_final: bool,
    fast: bool,
) -> ghostbox::ReplayRequest {
    let steps: Vec<ghostbox::HidStep> = hid_events
        .iter()
        .map(|e| ghostbox::HidStep {
            t_ms: e.t_ms.max(0.0).round() as u64,
            dx: e.dx,
            dy: e.dy,
            buttons: e.buttons,
        })
        .collect();
    ghostbox::ReplayRequest {
        steps,
        start_pos: Some((start[0].round() as i32, start[1].round() as i32)),
        move_to_start: true,
        final_pos: Some((end[0].round() as i32, end[1].round() as i32)),
        snap_final,
        fast,
    }
}

/// Spawn a background thread that loads the DLL, opens the device (guarded),
/// and replays relative HID steps. Updates `shared` for the UI status line.
#[cfg(windows)]
pub fn spawn_hid_replay(
    hid_events: Vec<HidEvent>,
    start: [f64; 2],
    end: [f64; 2],
    snap_final: bool,
    fast: bool,
    shared: Arc<Mutex<GbReplayShared>>,
) {
    {
        let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
        if g.busy {
            drop(g);
            set_status(&shared, "GhostBox replay already running.");
            return;
        }
        g.busy = true;
    }
    set_status(
        &shared,
        format!(
            "GhostBox: starting replay ({} HID steps, snap_final={snap_final}, fast={fast})…",
            hid_events.len(),
        ),
    );

    thread::spawn(move || {
        let result = (|| -> Result<String, String> {
            if hid_events.is_empty() {
                let msg = "GhostBox error: hid_events is empty";
                set_status(&shared, msg);
                return Err(msg.into());
            }

            let dll = match resolve_dll_beside_exe() {
                Ok(path) => path,
                Err(err) => {
                    set_status(&shared, format!("GhostBox error: {err}"));
                    return Err(err);
                }
            };
            set_status(
                &shared,
                format!("GhostBox: opening/reusing shared session from {}…", dll.display()),
            );

            let api = match ghostbox::shared_device_session(&dll, ghostbox::OPEN_DEVICE_TIMEOUT) {
                Ok(api) => api,
                Err(err) => {
                    let msg = format!("GhostBox OpenDevice error: {err}");
                    set_status(&shared, msg.as_str());
                    return Err(msg);
                }
            };
            set_status(
                &shared,
                format!("GhostBox shared device session ready; replaying {} steps…", hid_events.len()),
            );

            let req = build_request(&hid_events, start, end, snap_final, fast);
            let shared_progress = Arc::clone(&shared);
            let mut progress = move |done: usize, total: usize| {
                let status = format!("GhostBox replaying {done}/{total} …");
                let mut g = shared_progress.lock().unwrap_or_else(|e| e.into_inner());
                g.status = status;
            };
            let report =
                match ghostbox::replay_hid_sequence_with_progress(&api, &req, Some(&mut progress))
                {
                    Ok(report) => report,
                    Err(err) => {
                        let msg = format!("GhostBox replay error: {err}");
                        set_status(&shared, msg.as_str());
                        return Err(msg);
                    }
                };

            Ok(format!(
                "GhostBox done: steps={}, coalesced={}, moves={}, buttons={}, {} ms, mouse=({}, {}), start={}, snapped={}",
                report.steps_in,
                report.steps_coalesced,
                report.move_calls,
                report.button_events,
                report.duration_ms,
                report.mouse_x,
                report.mouse_y,
                report.moved_to_start,
                report.snapped_final
            ))
        })();

        let final_status = match result {
            Ok(msg) => msg,
            Err(err) if err.starts_with("GhostBox ") => err,
            Err(err) => format!("GhostBox error: {err}"),
        };
        set_status(&shared, final_status);
        let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
        g.busy = false;
    });
}

#[cfg(not(windows))]
pub fn spawn_hid_replay(
    _hid_events: Vec<HidEvent>,
    _start: [f64; 2],
    _end: [f64; 2],
    _snap_final: bool,
    _fast: bool,
    shared: Arc<Mutex<GbReplayShared>>,
) {
    let status =
        "GhostBox HID replay is Windows-only (gbilmd64.dll). Build/run on Windows with the device attached.";
    set_status(&shared, status);
    let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
    g.busy = false;
}

/// Short label for the Replay button (CJK when font available).
pub fn button_label(cjk: bool) -> &'static str {
    if cjk {
        "幽灵盒回放"
    } else {
        "GhostBox Replay"
    }
}

/// Hover / disabled hint.
pub fn disabled_hint(cjk: bool) -> &'static str {
    if cjk {
        "需要非空 hid_events（Raw Input 录制）。Windows + gbilmd64.dll + 已连接幽灵盒。"
    } else {
        "Requires non-empty hid_events (Raw Input). Windows + gbilmd64.dll + connected GhostBox."
    }
}
