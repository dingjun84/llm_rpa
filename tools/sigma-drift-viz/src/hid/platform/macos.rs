//! macOS IOHIDManager mouse capture (relative X/Y + button bitfield).
//!
//! Runs a dedicated CFRunLoop thread so egui/winit's main loop is untouched.
//! Values are coalesced by AbsoluteTime into `{t_ms, dx, dy, buttons}` samples.
//!
//! # Permissions
//! Non-seize open. If `IOHIDManagerOpen` fails (often Input Monitoring on
//! newer macOS), [`HidSession::start`] returns `Err` and the Record tab
//! keeps pixel/egui capture only.

#![allow(non_snake_case, non_camel_case_types, dead_code)]

use crate::hid::HidEvent;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

// ── IOKit / CoreFoundation FFI ───────────────────────────────────────────

type IOReturn = i32;
type IOOptionBits = u32;
type CFIndex = isize;
type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFNumberRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFMutableDictionaryRef = *mut c_void;
type CFAllocatorRef = *const c_void;
type CFRunLoopRef = *mut c_void;
type CFRunLoopMode = CFStringRef;
type IOHIDManagerRef = *mut c_void;
type IOHIDValueRef = *const c_void;
type IOHIDElementRef = *const c_void;

const K_IO_RETURN_SUCCESS: IOReturn = 0;
const K_IOHID_OPTIONS_NONE: IOOptionBits = 0x0;

const K_HID_PAGE_GENERIC_DESKTOP: u32 = 0x01;
const K_HID_USAGE_GD_MOUSE: u32 = 0x02;
const K_HID_USAGE_GD_X: u32 = 0x30;
const K_HID_USAGE_GD_Y: u32 = 0x31;
const K_HID_PAGE_BUTTON: u32 = 0x09;

const K_CF_NUMBER_SINT32_TYPE: u32 = 3;
const K_CF_STRING_ENCODING_UTF8: u32 = 0x08000100;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDManagerCreate(allocator: CFAllocatorRef, options: IOOptionBits) -> IOHIDManagerRef;
    fn IOHIDManagerSetDeviceMatching(manager: IOHIDManagerRef, matching: CFDictionaryRef);
    fn IOHIDManagerRegisterInputValueCallback(
        manager: IOHIDManagerRef,
        callback: Option<
            unsafe extern "C" fn(
                context: *mut c_void,
                result: IOReturn,
                sender: *mut c_void,
                value: IOHIDValueRef,
            ),
        >,
        context: *mut c_void,
    );
    fn IOHIDManagerScheduleWithRunLoop(
        manager: IOHIDManagerRef,
        run_loop: CFRunLoopRef,
        mode: CFRunLoopMode,
    );
    fn IOHIDManagerUnscheduleFromRunLoop(
        manager: IOHIDManagerRef,
        run_loop: CFRunLoopRef,
        mode: CFRunLoopMode,
    );
    fn IOHIDManagerOpen(manager: IOHIDManagerRef, options: IOOptionBits) -> IOReturn;
    fn IOHIDManagerClose(manager: IOHIDManagerRef, options: IOOptionBits) -> IOReturn;

    fn IOHIDValueGetElement(value: IOHIDValueRef) -> IOHIDElementRef;
    fn IOHIDValueGetIntegerValue(value: IOHIDValueRef) -> i64;
    fn IOHIDValueGetTimeStamp(value: IOHIDValueRef) -> u64;

    fn IOHIDElementGetUsagePage(element: IOHIDElementRef) -> u32;
    fn IOHIDElementGetUsage(element: IOHIDElementRef) -> u32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFAllocatorDefault: CFAllocatorRef;
    static kCFRunLoopDefaultMode: CFRunLoopMode;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;

    fn CFRelease(cf: CFTypeRef);
    fn CFRetain(cf: CFTypeRef) -> CFTypeRef;

    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopRun();
    fn CFRunLoopStop(rl: CFRunLoopRef);

    fn CFStringCreateWithCString(
        alloc: CFAllocatorRef,
        c_str: *const i8,
        encoding: u32,
    ) -> CFStringRef;

    fn CFNumberCreate(
        allocator: CFAllocatorRef,
        the_type: u32,
        value_ptr: *const c_void,
    ) -> CFNumberRef;

    fn CFDictionaryCreateMutable(
        allocator: CFAllocatorRef,
        capacity: CFIndex,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFMutableDictionaryRef;

    fn CFDictionarySetValue(
        the_dict: CFMutableDictionaryRef,
        key: *const c_void,
        value: *const c_void,
    );
}

#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}

fn mach_delta_to_ms(delta_mach: u64) -> f64 {
    let mut info = MachTimebaseInfo { numer: 1, denom: 1 };
    unsafe {
        let _ = mach_timebase_info(&mut info);
    }
    if info.denom == 0 {
        return 0.0;
    }
    (delta_mach as f64) * (info.numer as f64) / (info.denom as f64) / 1_000_000.0
}

fn cfstr(s: &str) -> CFStringRef {
    let c = std::ffi::CString::new(s).expect("cfstr nul");
    unsafe { CFStringCreateWithCString(kCFAllocatorDefault, c.as_ptr(), K_CF_STRING_ENCODING_UTF8) }
}

fn mouse_matching_dict() -> CFMutableDictionaryRef {
    unsafe {
        let dict = CFDictionaryCreateMutable(
            kCFAllocatorDefault,
            0,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        );
        let page: i32 = K_HID_PAGE_GENERIC_DESKTOP as i32;
        let usage: i32 = K_HID_USAGE_GD_MOUSE as i32;
        let page_num = CFNumberCreate(
            kCFAllocatorDefault,
            K_CF_NUMBER_SINT32_TYPE,
            &page as *const i32 as *const c_void,
        );
        let usage_num = CFNumberCreate(
            kCFAllocatorDefault,
            K_CF_NUMBER_SINT32_TYPE,
            &usage as *const i32 as *const c_void,
        );
        let key_page = cfstr("DeviceUsagePage");
        let key_usage = cfstr("DeviceUsage");
        CFDictionarySetValue(dict, key_page as *const c_void, page_num as *const c_void);
        CFDictionarySetValue(dict, key_usage as *const c_void, usage_num as *const c_void);
        CFRelease(key_page);
        CFRelease(key_usage);
        CFRelease(page_num as CFTypeRef);
        CFRelease(usage_num as CFTypeRef);
        dict
    }
}

// ── Shared capture state ─────────────────────────────────────────────────

struct Pending {
    t_mach: u64,
    dx: i32,
    dy: i32,
    buttons: u8,
    dirty_motion: bool,
}

struct Shared {
    /// `mach_absolute_time` at Record Start (aligned with Instant epoch).
    start_mach: u64,
    events: Vec<HidEvent>,
    pending: Option<Pending>,
    buttons: u8,
}

impl Shared {
    fn t_ms_of(&self, t_mach: u64) -> f64 {
        if t_mach >= self.start_mach {
            mach_delta_to_ms(t_mach - self.start_mach)
        } else {
            0.0
        }
    }

    fn flush_pending(&mut self) {
        if let Some(p) = self.pending.take() {
            if p.dirty_motion {
                self.events.push(HidEvent {
                    t_ms: self.t_ms_of(p.t_mach),
                    dx: p.dx,
                    dy: p.dy,
                    buttons: p.buttons,
                });
            }
        }
    }

    fn on_value(&mut self, value: IOHIDValueRef) {
        unsafe {
            let element = IOHIDValueGetElement(value);
            if element.is_null() {
                return;
            }
            let page = IOHIDElementGetUsagePage(element);
            let usage = IOHIDElementGetUsage(element);
            let ival = IOHIDValueGetIntegerValue(value);
            let t_mach = IOHIDValueGetTimeStamp(value);

            if page == K_HID_PAGE_GENERIC_DESKTOP
                && (usage == K_HID_USAGE_GD_X || usage == K_HID_USAGE_GD_Y)
            {
                let need_new = match &self.pending {
                    Some(p) => p.t_mach != t_mach,
                    None => true,
                };
                if need_new {
                    self.flush_pending();
                    self.pending = Some(Pending {
                        t_mach,
                        dx: 0,
                        dy: 0,
                        buttons: self.buttons,
                        dirty_motion: false,
                    });
                }
                if let Some(p) = self.pending.as_mut() {
                    if usage == K_HID_USAGE_GD_X {
                        p.dx = ival as i32;
                    } else {
                        p.dy = ival as i32;
                    }
                    p.buttons = self.buttons;
                    p.dirty_motion = true;
                }
            } else if page == K_HID_PAGE_BUTTON && (1..=8).contains(&usage) {
                // Flush motion for the previous report before the click edge.
                self.flush_pending();
                let bit = 1u8 << (usage - 1);
                if ival != 0 {
                    self.buttons |= bit;
                } else {
                    self.buttons &= !bit;
                }
                // Include click edges as zero-motion samples.
                self.events.push(HidEvent {
                    t_ms: self.t_ms_of(t_mach),
                    dx: 0,
                    dy: 0,
                    buttons: self.buttons,
                });
            }
        }
    }
}

struct ThreadCtx {
    shared: Arc<Mutex<Shared>>,
}

unsafe extern "C" fn input_value_callback(
    context: *mut c_void,
    _result: IOReturn,
    _sender: *mut c_void,
    value: IOHIDValueRef,
) {
    if context.is_null() || value.is_null() {
        return;
    }
    let ctx = &*(context as *const ThreadCtx);
    if let Ok(mut guard) = ctx.shared.lock() {
        guard.on_value(value);
    }
}

// ── Public session ───────────────────────────────────────────────────────

pub fn backend_label() -> &'static str {
    "HID: IOHID (Mac)"
}

/// Raw CFRunLoop pointer stored as usize so the Arc can cross threads.
/// Only the HID worker creates it; Start/Drop call `CFRunLoopStop` which is
/// documented as safe to invoke from another thread.
#[derive(Clone, Copy)]
struct RunLoopPtr(usize);

impl RunLoopPtr {
    fn from_raw(rl: CFRunLoopRef) -> Self {
        Self(rl as usize)
    }
    fn as_raw(self) -> CFRunLoopRef {
        self.0 as CFRunLoopRef
    }
}

// SAFETY: CFRunLoopStop is thread-safe; we never dereference the pointer
// except to pass it to that API / CFRelease on the owning worker thread.
unsafe impl Send for RunLoopPtr {}
unsafe impl Sync for RunLoopPtr {}

pub struct HidSession {
    shared: Arc<Mutex<Shared>>,
    run_loop: Arc<Mutex<Option<RunLoopPtr>>>,
    stop_flag: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    status: String,
    /// Instant captured with `start_mach` — use as Record `rec_started_at`.
    epoch: Instant,
}

impl HidSession {
    /// Start capturing mouse HID on a background CFRunLoop thread.
    pub fn start() -> Result<Self, String> {
        // Instant first, then mach — pixel elapsed and HID AbsoluteTime share one Start.
        let start_instant = Instant::now();
        let start_mach = unsafe { mach_absolute_time() };

        let shared = Arc::new(Mutex::new(Shared {
            start_mach,
            events: Vec::with_capacity(4096),
            pending: None,
            buttons: 0,
        }));
        let run_loop_slot: Arc<Mutex<Option<RunLoopPtr>>> = Arc::new(Mutex::new(None));
        let stop_flag = Arc::new(AtomicBool::new(false));

        let shared_thread = Arc::clone(&shared);
        let run_loop_thread = Arc::clone(&run_loop_slot);
        let stop_thread = Arc::clone(&stop_flag);

        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();

        let join = thread::Builder::new()
            .name("sigma-hid-iohid".into())
            .spawn(move || {
                worker_main(shared_thread, run_loop_thread, stop_thread, ready_tx);
            })
            .map_err(|e| format!("HID thread spawn failed: {e}"))?;

        match ready_rx.recv_timeout(std::time::Duration::from_secs(3)) {
            Ok(Ok(())) => Ok(Self {
                shared,
                run_loop: run_loop_slot,
                stop_flag,
                join: Some(join),
                status: format!("{} — capturing", backend_label()),
                epoch: start_instant,
            }),
            Ok(Err(e)) => {
                stop_flag.store(true, Ordering::SeqCst);
                stop_run_loop(&run_loop_slot);
                let _ = join.join();
                Err(e)
            }
            Err(_) => {
                stop_flag.store(true, Ordering::SeqCst);
                stop_run_loop(&run_loop_slot);
                let _ = join.join();
                Err("HID: IOHIDManager open timed out (check Input Monitoring permission)".into())
            }
        }
    }

    pub fn status_message(&self) -> &str {
        &self.status
    }

    /// Instant aligned with HID `start_mach` (same Record Start epoch).
    pub fn epoch(&self) -> Instant {
        self.epoch
    }

    /// Stop the run loop, flush pending motion, slice to `[start_ts, end_ts]`.
    pub fn stop_and_slice(mut self, start_ts: f64, end_ts: f64) -> Vec<HidEvent> {
        self.stop_flag.store(true, Ordering::SeqCst);
        stop_run_loop(&self.run_loop);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
        let mut events = if let Ok(mut guard) = self.shared.lock() {
            guard.flush_pending();
            std::mem::take(&mut guard.events)
        } else {
            Vec::new()
        };
        // Inclusive slice so click edges on the boundary are kept.
        events.retain(|e| e.t_ms >= start_ts && e.t_ms <= end_ts);
        events
    }
}

impl Drop for HidSession {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        stop_run_loop(&self.run_loop);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

fn stop_run_loop(slot: &Arc<Mutex<Option<RunLoopPtr>>>) {
    if let Ok(guard) = slot.lock() {
        if let Some(rl) = *guard {
            unsafe { CFRunLoopStop(rl.as_raw()) };
        }
    }
}

fn worker_main(
    shared: Arc<Mutex<Shared>>,
    run_loop_slot: Arc<Mutex<Option<RunLoopPtr>>>,
    stop_flag: Arc<AtomicBool>,
    ready_tx: std::sync::mpsc::Sender<Result<(), String>>,
) {
    unsafe {
        let manager = IOHIDManagerCreate(kCFAllocatorDefault, K_IOHID_OPTIONS_NONE);
        if manager.is_null() {
            let _ = ready_tx.send(Err("HID: IOHIDManagerCreate failed".into()));
            return;
        }

        let matching = mouse_matching_dict();
        IOHIDManagerSetDeviceMatching(manager, matching as CFDictionaryRef);
        CFRelease(matching as CFTypeRef);

        let ctx = Box::new(ThreadCtx {
            shared: Arc::clone(&shared),
        });
        let ctx_ptr = Box::into_raw(ctx);
        IOHIDManagerRegisterInputValueCallback(
            manager,
            Some(input_value_callback),
            ctx_ptr as *mut c_void,
        );

        let rl = CFRunLoopGetCurrent();
        let _ = CFRetain(rl as CFTypeRef);
        if let Ok(mut slot) = run_loop_slot.lock() {
            *slot = Some(RunLoopPtr::from_raw(rl));
        }

        IOHIDManagerScheduleWithRunLoop(manager, rl, kCFRunLoopDefaultMode);

        let open_status = IOHIDManagerOpen(manager, K_IOHID_OPTIONS_NONE);
        if open_status != K_IO_RETURN_SUCCESS {
            IOHIDManagerUnscheduleFromRunLoop(manager, rl, kCFRunLoopDefaultMode);
            let _ = IOHIDManagerClose(manager, K_IOHID_OPTIONS_NONE);
            CFRelease(manager as CFTypeRef);
            let _ = Box::from_raw(ctx_ptr);
            if let Ok(mut slot) = run_loop_slot.lock() {
                *slot = None;
            }
            CFRelease(rl as CFTypeRef);
            let _ = ready_tx.send(Err(format!(
                "HID: IOHIDManagerOpen failed (IOReturn={open_status}). \
                 Grant Input Monitoring to this app/terminal if needed."
            )));
            return;
        }

        if ready_tx.send(Ok(())).is_err() {
            // Starter gone — shut down immediately.
            IOHIDManagerUnscheduleFromRunLoop(manager, rl, kCFRunLoopDefaultMode);
            let _ = IOHIDManagerClose(manager, K_IOHID_OPTIONS_NONE);
            CFRelease(manager as CFTypeRef);
            let _ = Box::from_raw(ctx_ptr);
            if let Ok(mut slot) = run_loop_slot.lock() {
                *slot = None;
            }
            CFRelease(rl as CFTypeRef);
            return;
        }

        // Block until Stop Recording / Drop calls CFRunLoopStop.
        // Periodically check stop_flag in case Stop races before Run starts.
        if !stop_flag.load(Ordering::SeqCst) {
            CFRunLoopRun();
        }

        IOHIDManagerUnscheduleFromRunLoop(manager, rl, kCFRunLoopDefaultMode);
        let _ = IOHIDManagerClose(manager, K_IOHID_OPTIONS_NONE);
        CFRelease(manager as CFTypeRef);
        let _ = Box::from_raw(ctx_ptr);
        if let Ok(mut slot) = run_loop_slot.lock() {
            *slot = None;
        }
        CFRelease(rl as CFTypeRef);
    }
}
