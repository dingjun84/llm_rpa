//! GhostBox DLL absolute-move self-test for the GhostBox tab.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Default)]
pub struct GbSelfTestShared {
    pub busy: bool,
    /// True while the reset button owns the two-second device settling window.
    pub resetting: bool,
    pub status: String,
}

fn set_status(shared: &Arc<Mutex<GbSelfTestShared>>, status: impl Into<String>) {
    let status = status.into();
    crate::gb_replay::append_replay_log(&status);
    let mut state = shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.status = status;
}

fn finish(shared: &Arc<Mutex<GbSelfTestShared>>) {
    let mut state = shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !state.resetting {
        state.busy = false;
    }
}

/// Reset the GhostBox device and release the self-test busy state.
///
/// The button is intentionally independent of the worker: it is usable while a
/// self-test is stuck. Keep the move button gated during the device reset, then
/// release it only after the requested two-second settling delay.
pub fn reset_self_test(shared: Arc<Mutex<GbSelfTestShared>>) {
    {
        let mut state = shared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.resetting = true;
        state.busy = true;
        state.status = "GhostBox self-test reset: closing/resetting device; waiting 2 s…".into();
    }
    crate::gb_replay::append_replay_log(
        "GhostBox self-test reset clicked: clearing shared session; CloseDevice -> ResetDevice -> sleep 2 s.",
    );

    thread::spawn(move || {
        let detail = match crate::gb_replay::resolve_dll_beside_exe() {
            Ok(dll) => ghostbox::reset_shared_device_session(&dll),
            Err(error) => format!("DLL resolve error (ignored): {error}"),
        };
        thread::sleep(Duration::from_secs(2));
        let status = format!("GhostBox self-test reset complete after 2 s: {detail}.");
        crate::gb_replay::append_replay_log(&status);
        let mut state = shared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.busy = false;
        state.resetting = false;
        state.status = status;
    });
}

#[cfg(windows)]
fn primary_screen_size() -> (i32, i32) {
    // SAFETY: GetSystemMetrics is a read-only Win32 call with no pointers.
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetSystemMetrics(index: i32) -> i32;
    }
    let width = unsafe { GetSystemMetrics(0) };
    let height = unsafe { GetSystemMetrics(1) };
    if width > 200 && height > 200 {
        (width, height)
    } else {
        (1200, 800)
    }
}

#[cfg(not(windows))]
fn primary_screen_size() -> (i32, i32) {
    (1200, 800)
}

#[cfg(windows)]
fn next_random(state: &mut u64, upper_inclusive: i32) -> i32 {
    // Small dependency-free xorshift generator is enough for a hardware smoke test.
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    (*state % (upper_inclusive as u64 + 1)) as i32
}

#[cfg(windows)]
pub fn spawn_self_test(shared: Arc<Mutex<GbSelfTestShared>>) {
    {
        let mut state = shared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.busy || state.resetting {
            drop(state);
            set_status(&shared, "GhostBox self-test already running.");
            return;
        }
        state.busy = true;
    }
    set_status(&shared, "GhostBox self-test: starting background worker.");

    thread::spawn(move || {
        let dll = match crate::gb_replay::resolve_dll_beside_exe() {
            Ok(path) => path,
            Err(error) => {
                set_status(&shared, format!("GhostBox self-test error: {error}"));
                finish(&shared);
                return;
            }
        };
        set_status(
            &shared,
            format!("GhostBox self-test: opening/reusing shared session from {}…", dll.display()),
        );
        let api = match ghostbox::shared_device_session(&dll, ghostbox::OPEN_DEVICE_TIMEOUT) {
            Ok(api) => api,
            Err(error) => {
                set_status(
                    &shared,
                    format!("GhostBox self-test OpenDevice error: {error}"),
                );
                finish(&shared);
                return;
            }
        };
        set_status(
            &shared,
            "GhostBox self-test: shared device session ready (mode=2, speed=5).",
        );

        let (screen_width, screen_height) = primary_screen_size();
        let x_max = (screen_width - 100).max(101);
        let y_max = (screen_height - 100).max(101);
        set_status(
            &shared,
            format!(
                "GhostBox self-test: primary display {}x{}; points constrained to x=100..{}, y=100..{}.",
                screen_width, screen_height, x_max, y_max
            ),
        );

        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(0)
            ^ 0x9e37_79b9_7f4a_7c15;
        let mut random_state = seed;
        let mut errors = 0u32;
        for point_index in 1..=3 {
            let x = 100 + next_random(&mut random_state, x_max - 100);
            let y = 100 + next_random(&mut random_state, y_max - 100);
            set_status(
                &shared,
                format!("GhostBox self-test: point {point_index}/3 MoveMouseTo({x}, {y})..."),
            );
            match api.MoveMouseTo(x, y) {
                Ok(code) if code >= 0 => set_status(
                    &shared,
                    format!("GhostBox self-test: point {point_index}/3 MoveMouseTo -> code {code}; waiting 350 ms."),
                ),
                Ok(code) => {
                    errors += 1;
                    set_status(
                        &shared,
                        format!("GhostBox self-test error: point {point_index}/3 MoveMouseTo returned code {code}."),
                    );
                }
                Err(error) => {
                    errors += 1;
                    set_status(
                        &shared,
                        format!("GhostBox self-test MoveMouseTo error at point {point_index}/3: {error}"),
                    );
                }
            }
            thread::sleep(Duration::from_millis(350));
            match (api.GetMouseX(), api.GetMouseY()) {
                (Ok(actual_x), Ok(actual_y)) => {
                    set_status(
                        &shared,
                        format!(
                            "GhostBox self-test: point {point_index}/3 GetMouseX={actual_x}, GetMouseY={actual_y} (requested {x}, {y})."
                        ),
                    );
                }
                (x_result, y_result) => {
                    errors += 1;
                    set_status(
                        &shared,
                        format!(
                            "GhostBox self-test error: point {point_index}/3 read failed: X={:?}, Y={:?}.",
                            x_result.err(),
                            y_result.err()
                        ),
                    );
                }
            }
        }

        set_status(
            &shared,
            format!("GhostBox self-test complete: 3 points, errors={errors}; shared session kept open."),
        );
        finish(&shared);
    });
}

#[cfg(not(windows))]
pub fn spawn_self_test(shared: Arc<Mutex<GbSelfTestShared>>) {
    set_status(
        &shared,
        "GhostBox self-test is Windows-only (gbilmd64.dll); build/run on Windows with the device attached.",
    );
    finish(&shared);
}
