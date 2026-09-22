//! 输入原语：键盘、鼠标、滚轮、逐字文本。
//!
//! ## 为什么单独成文件
//!
//! 这一段自成一类 —— 都是「造一个 `INPUT` 结构、丢给 `SendInput`」，
//! 与窗口枚举、截屏、剪贴板没有任何耦合。搬出来以后 `winapi.rs` 回到
//! 「窗口 / 截屏 / 剪贴板 / 进程」的骨架。
//!
//! ## 三条不能改的约定
//!
//! 1. **一律走 `SendInput`，不用 `SetCursorPos` / `keybd_event`** ——
//!    返回值就是系统实际接受的事件数，被拦下时是 0，能当场发现。
//! 2. **滚轮逐格发**（`WHEEL_STEP_DELAY`）—— 合并成一次 `mouseData = ±360`
//!    在 WebView 类界面里可能只算一次滚动。理由写在那个常量上。
//! 3. **逐字输入按 UTF-16 码元发**，且字符之间留间隔 —— 理由写在
//!    [`send_unicode_text`] 上。
//!
//! ## 可见性
//!
//! 对外的原语由父模块再导出成 `winapi::left_click` 等。
//! [`mouse_move_input`] 是例外：它只被**同级**的 `cursor.rs`（光标轨迹）调用，
//! 所以是 `pub(super)` 且**不再导出**。

use std::time::Duration;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, VIRTUAL_KEY, VK_A,
    VK_CONTROL, VK_DELETE, VK_V,
};

use super::WinResult;

// ── 输入 ────────────────────────────────────────────────────────────────

fn key_input(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    let mut input = INPUT::default();
    input.r#type = INPUT_KEYBOARD;
    input.Anonymous.ki = KEYBDINPUT {
        wVk: vk,
        wScan: 0,
        dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() },
        time: 0,
        dwExtraInfo: 0,
    };
    input
}

fn mouse_input(flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS) -> INPUT {
    mouse_input_with(flags, 0)
}

fn mouse_input_with(
    flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS,
    mouse_data: i32,
) -> INPUT {
    let mut input = INPUT::default();
    input.r#type = INPUT_MOUSE;
    input.Anonymous.mi = MOUSEINPUT {
        dx: 0,
        dy: 0,
        mouseData: mouse_data as u32,
        dwFlags: flags,
        time: 0,
        dwExtraInfo: 0,
    };
    input
}

/// 一次绝对坐标的鼠标移动。
///
/// `dx` / `dy` 是**已归一化**到 0..=65535 的坐标（换算见 `cursor.rs` 的
/// `move_cursor_absolute`），
/// 不是像素——所以这里不能复用按像素说话的 [`mouse_input_with`]。
pub(super) fn mouse_move_input(dx: i32, dy: i32) -> INPUT {
    let mut input = INPUT::default();
    input.r#type = INPUT_MOUSE;
    input.Anonymous.mi = MOUSEINPUT {
        dx,
        dy,
        mouseData: 0,
        dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        time: 0,
        dwExtraInfo: 0,
    };
    input
}

/// Win32 的 `WHEEL_DELTA`：滚轮一格对应的 `mouseData` 增量。
const WHEEL_DELTA_UNITS: i32 = 120;

/// 逐格发送滚轮事件之间的间隔。
///
/// 把多格合并成一次 `SendInput`（`mouseData = ±360`）在多数程序里可用，
/// 但 WebView 类界面（微信 4.x 就是）可能只当作一次滚动，实际滚动量不足。
/// 逐格发送更接近真实滚轮——这是**为了稳妥的刻意选择**，不是在真机上对比测出的结论。
const WHEEL_STEP_DELAY: Duration = Duration::from_millis(15);

/// 在光标当前位置滚动鼠标滚轮。
///
/// `notches > 0` 表示**向下滚动内容**（看列表里更靠后的项），`< 0` 表示向上。
/// 注意 Win32 的约定与直觉相反：`mouseData` 为**正**表示滚轮向远离用户的方向转，
/// 内容向上移动；所以向下滚要传负值。
///
/// 调用方必须先把光标移到目标控件上——滚轮事件只会送给光标下的窗口。
pub fn scroll_wheel(notches: i32) -> WinResult<()> {
    if notches == 0 {
        return Ok(());
    }
    let step = if notches > 0 { -WHEEL_DELTA_UNITS } else { WHEEL_DELTA_UNITS };
    for index in 0..notches.unsigned_abs() {
        let input = mouse_input_with(MOUSEEVENTF_WHEEL, step);
        let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
        if sent != 1 {
            return Err(format!("发送滚轮事件失败（第 {} 格）", index + 1));
        }
        if index + 1 < notches.unsigned_abs() {
            std::thread::sleep(WHEEL_STEP_DELAY);
        }
    }
    Ok(())
}

pub fn left_click() -> WinResult<()> {
    let inputs = [mouse_input(MOUSEEVENTF_LEFTDOWN), mouse_input(MOUSEEVENTF_LEFTUP)];
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent != inputs.len() as u32 {
        return Err("发送鼠标事件失败".to_string());
    }
    Ok(())
}

pub fn send_ctrl_v() -> WinResult<()> {
    send_ctrl_key(VK_V, "粘贴快捷键")
}

/// Ctrl+A：仅用于诊断工具清空测试输入框。
pub fn send_ctrl_a() -> WinResult<()> {
    send_ctrl_key(VK_A, "全选快捷键")
}

/// Delete：仅用于诊断工具清空测试输入框。
pub fn send_delete() -> WinResult<()> {
    let inputs = [key_input(VK_DELETE, false), key_input(VK_DELETE, true)];
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent != inputs.len() as u32 {
        return Err("发送删除键失败".to_string());
    }
    Ok(())
}

fn send_ctrl_key(key: VIRTUAL_KEY, what: &str) -> WinResult<()> {
    let inputs = [
        key_input(VK_CONTROL, false),
        key_input(key, false),
        key_input(key, true),
        key_input(VK_CONTROL, true),
    ];
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent != inputs.len() as u32 {
        return Err(format!("发送{what}失败"));
    }
    Ok(())
}

/// 一个 Unicode 字符（UTF-16 码元）的按下 / 抬起事件。
fn unicode_input(unit: u16, up: bool) -> INPUT {
    let mut input = INPUT::default();
    input.r#type = INPUT_KEYBOARD;
    input.Anonymous.ki = KEYBDINPUT {
        // 走 `KEYEVENTF_UNICODE` 时 `wVk` 必须为 0，字符本身放在 `wScan` 里。
        wVk: VIRTUAL_KEY(0),
        wScan: unit,
        dwFlags: if up { KEYEVENTF_KEYUP | KEYEVENTF_UNICODE } else { KEYEVENTF_UNICODE },
        time: 0,
        dwExtraInfo: 0,
    };
    input
}

/// 逐字符输入文本，**不经过剪贴板**。
///
/// ## 为什么用 `KEYEVENTF_UNICODE` 而不是虚拟键码
///
/// 虚拟键码只能表达键盘上**真实存在**的键，中文根本没有对应的键。
/// `KEYEVENTF_UNICODE` 直接把一个 UTF-16 码元交给目标窗口的键盘消息处理，
/// 与输入法上屏走的是同一条路——这是逐字输入中文唯一可行的办法。
///
/// ## 为什么按 UTF-16 码元而不是 `char`
///
/// BMP 之外的字符（emoji 等）在 Windows 上是**代理对**，占两个码元。
/// 按 `char` 发会让目标窗口收到半个代理对，显示成一个方块。
///
/// ## 为什么字符之间要等
///
/// 客户端的搜索框是**联想式**的：收到一个字符就发一次查询、刷新下拉列表。
/// 连珠炮式地发完，联想请求会互相打断，下拉列表可能只按第一个字符的结果定格——
/// 而现象是"搜出来的东西不对"，不会让人想到是**输入太快**。
/// 间隔由调用方给（平台侧配置），这里不写死。
pub fn send_unicode_text(text: &str, interval: Duration) -> WinResult<()> {
    let units: Vec<u16> = text.encode_utf16().collect();
    for (index, unit) in units.iter().enumerate() {
        let inputs = [unicode_input(*unit, false), unicode_input(*unit, true)];
        let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent != inputs.len() as u32 {
            return Err(format!(
                "发送第 {} 个字符失败（共 {} 个）",
                index + 1,
                units.len()
            ));
        }
        if !interval.is_zero() && index + 1 < units.len() {
            std::thread::sleep(interval);
        }
    }
    Ok(())
}

