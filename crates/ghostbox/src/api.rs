//! Trimmed `gbilmd64.dll` binding (symbols match vendor DEMO `ghostboxm.rs`).

use crate::error::GhostboxError;
use libloading::{Library, Symbol};
use std::fs::OpenOptions;
use std::io::Write;
use std::ffi::{CStr, OsStr};
use std::os::raw::c_int;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Default OpenDevice wait before treating the call as hung.
pub const OPEN_DEVICE_TIMEOUT: Duration = Duration::from_secs(3);
/// Append a timestamped GhostBox replay diagnostic to temp and exe-dir logs.
///
/// Logging is best-effort and deliberately does not affect device operations.
pub(crate) fn append_replay_log(line: &str) {
    let mut paths = vec![std::env::temp_dir().join("ghostbox-replay.log")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let exe_path = dir.join("ghostbox-replay.log");
            if !paths.iter().any(|path| path == &exe_path) {
                paths.push(exe_path);
            }
        }
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_else(|_| "time-before-epoch".into());
    for path in paths {
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(file, "[{stamp}] {line}");
        }
    }
}

/// Mouse button IDs for `PressMouseButton` / `ReleaseMouseButton`.
///
/// Vendor Python demo uses `PressMouseButton(1)` for primary click.
/// Mapped from HID bitfield: bit0→1 (left), bit1→2 (right), bit2→3 (middle).
pub const MOUSE_BUTTON_LEFT: c_int = 1;
pub const MOUSE_BUTTON_RIGHT: c_int = 2;
pub const MOUSE_BUTTON_MIDDLE: c_int = 3;

/// Dynamically loaded GhostBox SDK (`gbilmd64.dll`).
pub struct GBMAPI {
    lib: Library,
}

#[allow(non_snake_case)]
impl GBMAPI {
    /// Load the vendor DLL from `path` (typically `gbilmd64.dll` beside the exe).
    pub fn load<P: AsRef<OsStr> + ?Sized>(path: &P) -> Result<Self, GhostboxError> {
        // SAFETY: `Library::new` loads a user-supplied DLL path; symbols are resolved lazily.
        let lib = unsafe { Library::new(path).map_err(GhostboxError::load)? };
        Ok(Self { lib })
    }

    /// `IsConnected` — non-zero when the SDK sees a device.
    pub fn IsConnected(&self) -> Result<c_int, GhostboxError> {
        type Fn = unsafe extern "C" fn() -> c_int;
        // SAFETY: symbol name matches vendor export; no args; return int status.
        unsafe {
            let func: Symbol<Fn> = self.lib.get(b"IsConnected").map_err(GhostboxError::symbol)?;
            Ok(func())
        }
    }

    /// `DeviceConnectState` — external device link state (0 = not connected).
    pub fn DeviceConnectState(&self) -> Result<c_int, GhostboxError> {
        type Fn = unsafe extern "C" fn() -> c_int;
        // SAFETY: vendor export; no args.
        unsafe {
            let func: Symbol<Fn> = self
                .lib
                .get(b"DeviceConnectState")
                .map_err(GhostboxError::symbol)?;
            Ok(func())
        }
    }

    /// Refuse open when both connection probes report 0 (DEMO: OpenDevice can hang forever).
    pub fn ensure_connected(&self) -> Result<(c_int, c_int), GhostboxError> {
        let connected = self.IsConnected()?;
        let state = self.DeviceConnectState()?;
        if connected == 0 && state == 0 {
            return Err(GhostboxError::DeviceNotConnected { connected, state });
        }
        Ok((connected, state))
    }

    /// `OpenDevice` — may block indefinitely if the device is missing; prefer [`open_device_guarded`].
    pub fn OpenDevice(&self) -> Result<c_int, GhostboxError> {
        type Fn = unsafe extern "C" fn() -> c_int;
        // SAFETY: vendor export; no args.
        unsafe {
            let func: Symbol<Fn> = self.lib.get(b"OpenDevice").map_err(GhostboxError::symbol)?;
            Ok(func())
        }
    }

    /// Set mouse movement mode (`SetMouseMovementMode`).
    pub fn SetMouseMovementMode(&self, mode: c_int) -> Result<c_int, GhostboxError> {
        type Fn = unsafe extern "C" fn(c_int) -> c_int;
        // SAFETY: vendor symbol takes one plain integer and returns an integer status.
        unsafe {
            let func: Symbol<Fn> = self
                .lib
                .get(b"SetMouseMovementMode")
                .map_err(GhostboxError::symbol)?;
            Ok(func(mode))
        }
    }

    /// Set mouse movement speed (`SetMouseMovementSpeed`).
    pub fn SetMouseMovementSpeed(&self, speed: c_int) -> Result<c_int, GhostboxError> {
        type Fn = unsafe extern "C" fn(c_int) -> c_int;
        // SAFETY: vendor symbol takes one plain integer and returns an integer status.
        unsafe {
            let func: Symbol<Fn> = self
                .lib
                .get(b"SetMouseMovementSpeed")
                .map_err(GhostboxError::symbol)?;
            Ok(func(speed))
        }
    }
    /// `CloseDevice`.
    pub fn CloseDevice(&self) -> Result<c_int, GhostboxError> {
        type Fn = unsafe extern "C" fn() -> c_int;
        // SAFETY: vendor export; no args.
        unsafe {
            let func: Symbol<Fn> = self.lib.get(b"CloseDevice").map_err(GhostboxError::symbol)?;
            Ok(func())
        }
    }

    /// `ResetDevice`.
    pub fn ResetDevice(&self) -> Result<c_int, GhostboxError> {
        type Fn = unsafe extern "C" fn() -> c_int;
        // SAFETY: vendor export; no args.
        unsafe {
            let func: Symbol<Fn> = self.lib.get(b"ResetDevice").map_err(GhostboxError::symbol)?;
            Ok(func())
        }
    }

    /// `SDKType` string from the DLL.
    pub fn SDKType(&self) -> Result<String, GhostboxError> {
        self.read_cstr_fn(b"SDKType")
    }

    /// `SDKVersion` string from the DLL.
    pub fn SDKVersion(&self) -> Result<String, GhostboxError> {
        self.read_cstr_fn(b"SDKVersion")
    }

    /// Relative mouse move in HID counts.
    pub fn MoveMouseRelative(&self, x: c_int, y: c_int) -> Result<c_int, GhostboxError> {
        type Fn = unsafe extern "C" fn(c_int, c_int) -> c_int;
        // SAFETY: vendor export; x/y are plain ints.
        unsafe {
            let func: Symbol<Fn> = self
                .lib
                .get(b"MoveMouseRelative")
                .map_err(GhostboxError::symbol)?;
            Ok(func(x, y))
        }
    }

    /// Absolute screen move.
    pub fn MoveMouseTo(&self, x: c_int, y: c_int) -> Result<c_int, GhostboxError> {
        type Fn = unsafe extern "C" fn(c_int, c_int) -> c_int;
        // SAFETY: vendor export; x/y are screen coordinates.
        unsafe {
            let func: Symbol<Fn> = self.lib.get(b"MoveMouseTo").map_err(GhostboxError::symbol)?;
            Ok(func(x, y))
        }
    }

    /// Current cursor X (as reported by the SDK).
    pub fn GetMouseX(&self) -> Result<c_int, GhostboxError> {
        type Fn = unsafe extern "C" fn() -> c_int;
        // SAFETY: vendor export; no args.
        unsafe {
            let func: Symbol<Fn> = self.lib.get(b"GetMouseX").map_err(GhostboxError::symbol)?;
            Ok(func())
        }
    }

    /// Current cursor Y (as reported by the SDK).
    pub fn GetMouseY(&self) -> Result<c_int, GhostboxError> {
        type Fn = unsafe extern "C" fn() -> c_int;
        // SAFETY: vendor export; no args.
        unsafe {
            let func: Symbol<Fn> = self.lib.get(b"GetMouseY").map_err(GhostboxError::symbol)?;
            Ok(func())
        }
    }

    /// Press mouse button (`MOUSE_BUTTON_*`).
    pub fn PressMouseButton(&self, mouse_button: c_int) -> Result<c_int, GhostboxError> {
        type Fn = unsafe extern "C" fn(c_int) -> c_int;
        // SAFETY: vendor export; button id is a small int (1/2/3).
        unsafe {
            let func: Symbol<Fn> = self
                .lib
                .get(b"PressMouseButton")
                .map_err(GhostboxError::symbol)?;
            Ok(func(mouse_button))
        }
    }

    /// Release mouse button (`MOUSE_BUTTON_*`).
    pub fn ReleaseMouseButton(&self, mouse_button: c_int) -> Result<c_int, GhostboxError> {
        type Fn = unsafe extern "C" fn(c_int) -> c_int;
        // SAFETY: vendor export; button id is a small int (1/2/3).
        unsafe {
            let func: Symbol<Fn> = self
                .lib
                .get(b"ReleaseMouseButton")
                .map_err(GhostboxError::symbol)?;
            Ok(func(mouse_button))
        }
    }

    fn read_cstr_fn(&self, name: &[u8]) -> Result<String, GhostboxError> {
        type Fn = unsafe extern "C" fn() -> *mut i8;
        // SAFETY: vendor exports return a C string pointer (possibly null); we copy immediately.
        unsafe {
            let func: Symbol<Fn> = self.lib.get(name).map_err(GhostboxError::symbol)?;
            let ptr = func();
            if ptr.is_null() {
                Ok(String::new())
            } else {
                Ok(CStr::from_ptr(ptr).to_string_lossy().into_owned())
            }
        }
    }
}

/// Process-wide open GhostBox session shared by self-test and replay.
static SHARED_DEVICE_SESSION: OnceLock<Mutex<Option<Arc<GBMAPI>>>> = OnceLock::new();

fn shared_device_slot() -> &'static Mutex<Option<Arc<GBMAPI>>> {
    SHARED_DEVICE_SESSION.get_or_init(|| Mutex::new(None))
}

/// Return the process-wide device session, opening it exactly once until reset.
pub fn shared_device_session(
    path: &Path,
    timeout: Duration,
) -> Result<Arc<GBMAPI>, GhostboxError> {
    let slot = shared_device_slot();
    let mut session = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(api) = session.as_ref() {
        return Ok(Arc::clone(api));
    }

    append_replay_log("shared_device_session: opening new process-wide session");
    let api = Arc::new(GBMAPI::load(path)?);
    open_device_guarded(&api, timeout)?;
    *session = Some(Arc::clone(&api));
    append_replay_log("shared_device_session: session stored for reuse");
    Ok(api)
}

/// Close/reset the shared session (or a freshly loaded DLL when no session exists).
///
/// The caller owns the settling delay; this function deliberately performs no open
/// and never propagates vendor errors, so the reset UI can remain an escape hatch.
/// Whether the process-wide shared session is currently open (no I/O).
pub fn shared_device_is_open() -> bool {
    let slot = shared_device_slot();
    let session = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    session.is_some()
}

pub fn reset_shared_device_session(path: &Path) -> String {
    let api = {
        let slot = shared_device_slot();
        let mut session = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        session.take()
    };
    let api = match api {
        Some(api) => Some(api),
        None => match GBMAPI::load(path) {
            Ok(api) => Some(Arc::new(api)),
            Err(error) => {
                let detail = format!("DLL load error (ignored): {error}");
                append_replay_log(&format!("reset_shared_device_session: {detail}"));
                return detail;
            }
        },
    };

    let api = api.expect("reset session API is present");
    let close_detail = match api.CloseDevice() {
        Ok(code) => format!("CloseDevice -> code {code}"),
        Err(error) => format!("CloseDevice error (ignored): {error}"),
    };
    append_replay_log(&format!("reset_shared_device_session: {close_detail}"));
    let reset_detail = match api.ResetDevice() {
        Ok(code) => format!("ResetDevice -> code {code}"),
        Err(error) => format!("ResetDevice error (ignored): {error}"),
    };
    append_replay_log(&format!("reset_shared_device_session: {reset_detail}"));
    format!("{close_detail}; {reset_detail}")
}
/// Prepare the device with `CloseDevice` then `ResetDevice`, then call `OpenDevice` with a timeout.
///
/// On timeout the worker thread may remain blocked inside the DLL for the process lifetime;
/// `api` is held by that thread via `Arc` so the library is not unloaded underneath it.
pub fn open_device_guarded(
    api: &Arc<GBMAPI>,
    timeout: Duration,
) -> Result<c_int, GhostboxError> {
    append_replay_log("open_device_guarded: begin (CloseDevice -> ResetDevice -> OpenDevice)");

    // Vendor-required lifecycle: CloseDevice -> ResetDevice -> OpenDevice.
    // The device may legitimately reject the first two calls when already closed;
    // continue regardless so OpenDevice is always attempted next.
    match api.CloseDevice() {
        Ok(code) => append_replay_log(&format!("CloseDevice before open Ok code={code}")),
        Err(err) => append_replay_log(&format!("CloseDevice before open Err: {err}")),
    }
    match api.ResetDevice() {
        Ok(code) => append_replay_log(&format!("ResetDevice before open Ok code={code}")),
        Err(err) => append_replay_log(&format!("ResetDevice before open Err: {err}")),
    }

    let api2 = Arc::clone(api);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(api2.OpenDevice());
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(code)) => {
            append_replay_log(&format!("OpenDevice result Ok code={code}"));
            if code < 0 {
                let err = GhostboxError::OpenDeviceFailed { code };
                append_replay_log(&format!("OpenDevice result Err: {err}"));
                Err(err)
            } else {
                // Apply the vendor demo's movement defaults after every successful open.
                match api.SetMouseMovementMode(2) {
                    Ok(mode_code) => append_replay_log(&format!(
                        "SetMouseMovementMode(2) result Ok code={mode_code}"
                    )),
                    Err(err) => append_replay_log(&format!(
                        "SetMouseMovementMode(2) result Err (ignored): {err}"
                    )),
                }
                match api.SetMouseMovementSpeed(5) {
                    Ok(speed_code) => append_replay_log(&format!(
                        "SetMouseMovementSpeed(5) result Ok code={speed_code}"
                    )),
                    Err(err) => append_replay_log(&format!(
                        "SetMouseMovementSpeed(5) result Err (ignored): {err}"
                    )),
                }
                Ok(code)
            }
        }        Ok(Err(err)) => {
            append_replay_log(&format!("OpenDevice result Err: {err}"));
            Err(err)
        }
        Err(_) => {
            let err = GhostboxError::OpenDeviceTimeout {
                timeout_ms: timeout.as_millis() as u64,
            };
            append_replay_log(&format!("OpenDevice timeout: {err}"));
            Err(err)
        }
    }
}