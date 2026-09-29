//! Windows Raw Input mouse capture (`WM_INPUT` + message-only HWND).
//!
//! Runs a dedicated thread with a hidden message-only window so egui/eframe's
//! main loop is untouched. Each `RAWMOUSE` report becomes one
//! `{t_ms, dx, dy, buttons}` sample (button edges included as zero-motion).
//!
//! Absolute-position devices (tablets / some touchpads reporting
//! `MOUSE_MOVE_ABSOLUTE`) contribute button state only — relative `dx`/`dy`
//! are skipped so hardware-replay samples stay in relative report units.

#![allow(non_snake_case, non_camel_case_types, dead_code)]

use crate::hid::HidEvent;
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

// ── Win32 types / constants ──────────────────────────────────────────────

type BOOL = i32;
type DWORD = u32;
type UINT = u32;
type LONG = i32;
type ULONG = u32;
type USHORT = u16;
type WCHAR = u16;
type ATOM = u16;
type LRESULT = isize;
type WPARAM = usize;
type LPARAM = isize;
type HWND = *mut c_void;
type HINSTANCE = *mut c_void;
type HMENU = *mut c_void;
type HICON = *mut c_void;
type HCURSOR = *mut c_void;
type HBRUSH = *mut c_void;
type HRAWINPUT = *mut c_void;

const FALSE: BOOL = 0;

const WM_DESTROY: UINT = 0x0002;
const WM_CLOSE: UINT = 0x0010;
const WM_INPUT: UINT = 0x00FF;

const HWND_MESSAGE: HWND = -3isize as HWND;

const RIDEV_REMOVE: DWORD = 0x00000001;
const RIDEV_INPUTSINK: DWORD = 0x00000100;

const RID_INPUT: UINT = 0x10000003;
const RIM_TYPEMOUSE: DWORD = 0;

const MOUSE_MOVE_ABSOLUTE: USHORT = 0x01;

const RI_MOUSE_LEFT_BUTTON_DOWN: USHORT = 0x0001;
const RI_MOUSE_LEFT_BUTTON_UP: USHORT = 0x0002;
const RI_MOUSE_RIGHT_BUTTON_DOWN: USHORT = 0x0004;
const RI_MOUSE_RIGHT_BUTTON_UP: USHORT = 0x0008;
const RI_MOUSE_MIDDLE_BUTTON_DOWN: USHORT = 0x0010;
const RI_MOUSE_MIDDLE_BUTTON_UP: USHORT = 0x0020;
const RI_MOUSE_BUTTON_4_DOWN: USHORT = 0x0040;
const RI_MOUSE_BUTTON_4_UP: USHORT = 0x0080;
const RI_MOUSE_BUTTON_5_DOWN: USHORT = 0x0100;
const RI_MOUSE_BUTTON_5_UP: USHORT = 0x0200;

const HID_USAGE_PAGE_GENERIC: USHORT = 0x01;
const HID_USAGE_GENERIC_MOUSE: USHORT = 0x02;

/// `GWLP_USERDATA` — pointer-sized user data slot on the HWND.
const GWLP_USERDATA: i32 = -21;

#[repr(C)]
struct POINT {
    x: LONG,
    y: LONG,
}

#[repr(C)]
struct MSG {
    hwnd: HWND,
    message: UINT,
    wParam: WPARAM,
    lParam: LPARAM,
    time: DWORD,
    pt: POINT,
}

#[repr(C)]
struct WNDCLASSEXW {
    cbSize: UINT,
    style: UINT,
    lpfnWndProc: Option<unsafe extern "system" fn(HWND, UINT, WPARAM, LPARAM) -> LRESULT>,
    cbClsExtra: i32,
    cbWndExtra: i32,
    hInstance: HINSTANCE,
    hIcon: HICON,
    hCursor: HCURSOR,
    hbrBackground: HBRUSH,
    lpszMenuName: *const WCHAR,
    lpszClassName: *const WCHAR,
    hIconSm: HICON,
}

#[repr(C)]
struct RAWINPUTDEVICE {
    usUsagePage: USHORT,
    usUsage: USHORT,
    dwFlags: DWORD,
    hwndTarget: HWND,
}

#[repr(C)]
struct RAWINPUTHEADER {
    dwType: DWORD,
    dwSize: DWORD,
    hDevice: *mut c_void,
    wParam: WPARAM,
}

/// MSVC layout: `usFlags` (2) + pad (2) + button union (4) + rest.
#[repr(C)]
struct RAWMOUSE {
    usFlags: USHORT,
    _pad_align: USHORT,
    usButtonFlags: USHORT,
    usButtonData: USHORT,
    ulRawButtons: ULONG,
    lLastX: LONG,
    lLastY: LONG,
    ulExtraInformation: ULONG,
}

#[repr(C)]
struct RAWINPUT_MOUSE {
    header: RAWINPUTHEADER,
    mouse: RAWMOUSE,
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassExW(pcwcx: *const WNDCLASSEXW) -> ATOM;
    fn UnregisterClassW(lpClassName: *const WCHAR, hInstance: HINSTANCE) -> BOOL;
    fn CreateWindowExW(
        dwExStyle: DWORD,
        lpClassName: *const WCHAR,
        lpWindowName: *const WCHAR,
        dwStyle: DWORD,
        x: i32,
        y: i32,
        nWidth: i32,
        nHeight: i32,
        hWndParent: HWND,
        hMenu: HMENU,
        hInstance: HINSTANCE,
        lpParam: *mut c_void,
    ) -> HWND;
    fn DestroyWindow(hWnd: HWND) -> BOOL;
    fn DefWindowProcW(hWnd: HWND, Msg: UINT, wParam: WPARAM, lParam: LPARAM) -> LRESULT;
    fn GetMessageW(lpMsg: *mut MSG, hWnd: HWND, wMsgFilterMin: UINT, wMsgFilterMax: UINT) -> BOOL;
    fn TranslateMessage(lpMsg: *const MSG) -> BOOL;
    fn DispatchMessageW(lpMsg: *const MSG) -> LRESULT;
    fn PostMessageW(hWnd: HWND, Msg: UINT, wParam: WPARAM, lParam: LPARAM) -> BOOL;
    fn PostQuitMessage(nExitCode: i32);
    fn SetWindowLongPtrW(hWnd: HWND, nIndex: i32, dwNewLong: isize) -> isize;
    fn GetWindowLongPtrW(hWnd: HWND, nIndex: i32) -> isize;
    fn RegisterRawInputDevices(
        pRawInputDevices: *const RAWINPUTDEVICE,
        uiNumDevices: UINT,
        cbSize: UINT,
    ) -> BOOL;
    fn GetRawInputData(
        hRawInput: HRAWINPUT,
        uiCommand: UINT,
        pData: *mut c_void,
        pcbSize: *mut UINT,
        cbSizeHeader: UINT,
    ) -> UINT;
    fn IsWindow(hWnd: HWND) -> BOOL;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(lpModuleName: *const WCHAR) -> HINSTANCE;
    fn GetLastError() -> DWORD;
}

fn wide(s: &str) -> Vec<WCHAR> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ── Shared capture state ─────────────────────────────────────────────────

struct Shared {
    start: Instant,
    events: Vec<HidEvent>,
    buttons: u8,
}

impl Shared {
    fn t_ms_now(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }

    fn apply_button_flags(&mut self, flags: USHORT) -> bool {
        let before = self.buttons;
        let pairs: &[(USHORT, USHORT, u8)] = &[
            (RI_MOUSE_LEFT_BUTTON_DOWN, RI_MOUSE_LEFT_BUTTON_UP, 0),
            (RI_MOUSE_RIGHT_BUTTON_DOWN, RI_MOUSE_RIGHT_BUTTON_UP, 1),
            (RI_MOUSE_MIDDLE_BUTTON_DOWN, RI_MOUSE_MIDDLE_BUTTON_UP, 2),
            (RI_MOUSE_BUTTON_4_DOWN, RI_MOUSE_BUTTON_4_UP, 3),
            (RI_MOUSE_BUTTON_5_DOWN, RI_MOUSE_BUTTON_5_UP, 4),
        ];
        for &(down, up, bit) in pairs {
            let mask = 1u8 << bit;
            if flags & down != 0 {
                self.buttons |= mask;
            }
            if flags & up != 0 {
                self.buttons &= !mask;
            }
        }
        self.buttons != before
    }

    fn on_raw_mouse(&mut self, mouse: &RAWMOUSE) {
        let absolute = mouse.usFlags & MOUSE_MOVE_ABSOLUTE != 0;
        let dx = if absolute { 0 } else { mouse.lLastX };
        let dy = if absolute { 0 } else { mouse.lLastY };
        let button_changed = self.apply_button_flags(mouse.usButtonFlags);
        let moved = dx != 0 || dy != 0;

        if !moved && !button_changed {
            return;
        }

        self.events.push(HidEvent {
            t_ms: self.t_ms_now(),
            dx,
            dy,
            buttons: self.buttons,
        });
    }
}

struct ThreadCtx {
    shared: Arc<Mutex<Shared>>,
}

// ── Window proc ──────────────────────────────────────────────────────────

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_INPUT => {
            let ctx_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ThreadCtx;
            if !ctx_ptr.is_null() {
                handle_wm_input(&*ctx_ptr, lparam as HRAWINPUT);
            }
            0
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            let rid = RAWINPUTDEVICE {
                usUsagePage: HID_USAGE_PAGE_GENERIC,
                usUsage: HID_USAGE_GENERIC_MOUSE,
                dwFlags: RIDEV_REMOVE,
                hwndTarget: ptr::null_mut(),
            };
            let _ = RegisterRawInputDevices(&rid, 1, std::mem::size_of::<RAWINPUTDEVICE>() as UINT);
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn handle_wm_input(ctx: &ThreadCtx, raw: HRAWINPUT) {
    let header_size = std::mem::size_of::<RAWINPUTHEADER>() as UINT;
    let mut size: UINT = 0;
    let got = GetRawInputData(raw, RID_INPUT, ptr::null_mut(), &mut size, header_size);
    if got as i32 == -1 || size == 0 {
        return;
    }
    if size as usize > 4096 {
        return;
    }
    let mut buf = vec![0u8; size as usize];
    let written = GetRawInputData(
        raw,
        RID_INPUT,
        buf.as_mut_ptr() as *mut c_void,
        &mut size,
        header_size,
    );
    if written as i32 == -1 || written == 0 {
        return;
    }
    if (written as usize) < std::mem::size_of::<RAWINPUT_MOUSE>() {
        return;
    }
    let packet = &*(buf.as_ptr() as *const RAWINPUT_MOUSE);
    if packet.header.dwType != RIM_TYPEMOUSE {
        return;
    }
    if let Ok(mut guard) = ctx.shared.lock() {
        guard.on_raw_mouse(&packet.mouse);
    }
}

// ── HWND slot (cross-thread stop) ────────────────────────────────────────

#[derive(Clone, Copy)]
struct HwndPtr(usize);

impl HwndPtr {
    fn from_raw(h: HWND) -> Self {
        Self(h as usize)
    }
    fn as_raw(self) -> HWND {
        self.0 as HWND
    }
}

// SAFETY: we only PostMessageW / never dereference the HWND from other threads.
unsafe impl Send for HwndPtr {}
unsafe impl Sync for HwndPtr {}

fn post_close(slot: &Arc<Mutex<Option<HwndPtr>>>) {
    if let Ok(guard) = slot.lock() {
        if let Some(h) = *guard {
            unsafe {
                let _ = PostMessageW(h.as_raw(), WM_CLOSE, 0, 0);
            }
        }
    }
}

// ── Public session ───────────────────────────────────────────────────────

pub fn backend_label() -> &'static str {
    "HID: Raw Input (Windows)"
}

pub struct HidSession {
    shared: Arc<Mutex<Shared>>,
    hwnd: Arc<Mutex<Option<HwndPtr>>>,
    stop_flag: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    status: String,
    epoch: Instant,
}

impl HidSession {
    /// Start capturing mouse Raw Input on a background message-only HWND thread.
    pub fn start() -> Result<Self, String> {
        let start_instant = Instant::now();
        let shared = Arc::new(Mutex::new(Shared {
            start: start_instant,
            events: Vec::with_capacity(4096),
            buttons: 0,
        }));
        let hwnd_slot: Arc<Mutex<Option<HwndPtr>>> = Arc::new(Mutex::new(None));
        let stop_flag = Arc::new(AtomicBool::new(false));

        let shared_thread = Arc::clone(&shared);
        let hwnd_thread = Arc::clone(&hwnd_slot);
        let stop_thread = Arc::clone(&stop_flag);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();

        let join = thread::Builder::new()
            .name("sigma-hid-rawinput".into())
            .spawn(move || {
                worker_main(shared_thread, hwnd_thread, stop_thread, ready_tx);
            })
            .map_err(|e| format!("HID thread spawn failed: {e}"))?;

        match ready_rx.recv_timeout(std::time::Duration::from_secs(3)) {
            Ok(Ok(())) => Ok(Self {
                shared,
                hwnd: hwnd_slot,
                stop_flag,
                join: Some(join),
                status: format!("{} — capturing", backend_label()),
                epoch: start_instant,
            }),
            Ok(Err(e)) => {
                stop_flag.store(true, Ordering::SeqCst);
                post_close(&hwnd_slot);
                let _ = join.join();
                Err(e)
            }
            Err(_) => {
                stop_flag.store(true, Ordering::SeqCst);
                post_close(&hwnd_slot);
                let _ = join.join();
                Err("HID: Raw Input window setup timed out".into())
            }
        }
    }

    pub fn status_message(&self) -> &str {
        &self.status
    }

    pub fn epoch(&self) -> Instant {
        self.epoch
    }

    /// Stop the message loop and slice events to `[start_ts, end_ts]` (inclusive).
    pub fn stop_and_slice(mut self, start_ts: f64, end_ts: f64) -> Vec<HidEvent> {
        self.stop_flag.store(true, Ordering::SeqCst);
        post_close(&self.hwnd);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
        let mut events = if let Ok(mut guard) = self.shared.lock() {
            std::mem::take(&mut guard.events)
        } else {
            Vec::new()
        };
        events.retain(|e| e.t_ms >= start_ts && e.t_ms <= end_ts);
        events
    }
}

impl Drop for HidSession {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        post_close(&self.hwnd);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

fn worker_main(
    shared: Arc<Mutex<Shared>>,
    hwnd_slot: Arc<Mutex<Option<HwndPtr>>>,
    stop_flag: Arc<AtomicBool>,
    ready_tx: std::sync::mpsc::Sender<Result<(), String>>,
) {
    unsafe {
        let hinstance = GetModuleHandleW(ptr::null());
        if hinstance.is_null() {
            let _ = ready_tx.send(Err(format!(
                "HID: GetModuleHandleW failed (err={})",
                GetLastError()
            )));
            return;
        }

        // Unique class per process so rapid Start/Stop does not collide.
        let class_name = wide(&format!("SigmaDriftHidRawInput_{}", std::process::id()));
        let window_name = wide("sigma-drift-hid");

        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as UINT,
            style: 0,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: ptr::null_mut(),
            hCursor: ptr::null_mut(),
            hbrBackground: ptr::null_mut(),
            lpszMenuName: ptr::null(),
            lpszClassName: class_name.as_ptr(),
            hIconSm: ptr::null_mut(),
        };

        let atom = RegisterClassExW(&wc);
        if atom == 0 {
            let _ = ready_tx.send(Err(format!(
                "HID: RegisterClassExW failed (err={})",
                GetLastError()
            )));
            return;
        }

        let ctx = Box::new(ThreadCtx {
            shared: Arc::clone(&shared),
        });
        let ctx_ptr = Box::into_raw(ctx);

        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            window_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            ptr::null_mut(),
            hinstance,
            ptr::null_mut(),
        );

        if hwnd.is_null() {
            let err = GetLastError();
            let _ = Box::from_raw(ctx_ptr);
            let _ = UnregisterClassW(class_name.as_ptr(), hinstance);
            let _ = ready_tx.send(Err(format!(
                "HID: CreateWindowExW (HWND_MESSAGE) failed (err={err})"
            )));
            return;
        }

        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ctx_ptr as isize);

        if let Ok(mut slot) = hwnd_slot.lock() {
            *slot = Some(HwndPtr::from_raw(hwnd));
        }

        let rid = RAWINPUTDEVICE {
            usUsagePage: HID_USAGE_PAGE_GENERIC,
            usUsage: HID_USAGE_GENERIC_MOUSE,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: hwnd,
        };
        if RegisterRawInputDevices(&rid, 1, std::mem::size_of::<RAWINPUTDEVICE>() as UINT) == FALSE
        {
            let err = GetLastError();
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            let _ = Box::from_raw(ctx_ptr);
            let _ = DestroyWindow(hwnd);
            // Drain WM_DESTROY / WM_QUIT from DestroyWindow.
            let mut msg = std::mem::zeroed::<MSG>();
            while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if let Ok(mut slot) = hwnd_slot.lock() {
                *slot = None;
            }
            let _ = UnregisterClassW(class_name.as_ptr(), hinstance);
            let _ = ready_tx.send(Err(format!(
                "HID: RegisterRawInputDevices failed (err={err})"
            )));
            return;
        }

        if stop_flag.load(Ordering::SeqCst) || ready_tx.send(Ok(())).is_err() {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            let _ = Box::from_raw(ctx_ptr);
            let _ = DestroyWindow(hwnd);
            let mut msg = std::mem::zeroed::<MSG>();
            while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if let Ok(mut slot) = hwnd_slot.lock() {
                *slot = None;
            }
            let _ = UnregisterClassW(class_name.as_ptr(), hinstance);
            return;
        }

        let mut msg = std::mem::zeroed::<MSG>();
        loop {
            if stop_flag.load(Ordering::SeqCst) {
                if IsWindow(hwnd) != FALSE {
                    let _ = DestroyWindow(hwnd);
                }
            }
            let r = GetMessageW(&mut msg, ptr::null_mut(), 0, 0);
            if r == 0 || r == -1 {
                break;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // Free context exactly once (userdata cleared so a late WM_INPUT is a no-op).
        if IsWindow(hwnd) != FALSE {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        }
        let _ = Box::from_raw(ctx_ptr);

        if let Ok(mut slot) = hwnd_slot.lock() {
            *slot = None;
        }
        let _ = UnregisterClassW(class_name.as_ptr(), hinstance);
    }
}
