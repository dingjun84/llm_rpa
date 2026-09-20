//! 全局热键：本程序**不在前台**时也能触发的一键动作。
//!
//! 为什么需要它：标定「搜索下拉列表」这类区域时，客户端里的下拉框是
//! **失焦即收**的临时弹层。操作者一旦点击本程序的按钮，前台就转到了本程序，
//! 弹层在点击的**那一刻**已经收起——等截屏真正执行时，看到的是收起后的画面。
//! 热键把「触发截屏」这个动作挪出了本程序的窗口，操作者全程不必让出前台。
//!
//! ★ 边界说明：`RegisterHotKey` 是标准 Win32 API。它**不注入**任何进程、
//!   **不挂 Hook**、**不读取按键内容**，只由系统在组合键命中时向本线程投递一条
//!   `WM_HOTKEY`。与项目的「不注入、不 Hook」约束不冲突。
//!
//! 本模块只负责「组合键命中时叫一声」，**不知道**这一声要触发什么——
//! 那是调用方的事（见 `apps/desktop` 把它接到截屏上）。

use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT, MOD_WIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, PeekMessageW, PostThreadMessageW, MSG, PM_NOREMOVE, WM_HOTKEY, WM_QUIT,
};

/// `RegisterHotKey` 失败码：这个组合已经被别的程序占用了。
const ERROR_HOTKEY_ALREADY_REGISTERED: u32 = 1409;

/// 注册与注销共用的热键编号。
///
/// `RegisterHotKey` 要求调用方给一个 id，系统在 `WM_HOTKEY` 的 `wParam` 里回传它。
/// 本程序**同一时刻只注册一个**热键，所以固定用 1：id 的用途是区分同一线程注册的
/// 多个热键，只有一个时它没有区分任务。哪天要支持多个热键，这里才需要改成分配。
const HOTKEY_ID: i32 = 1;

/// 允许绑定的按键。
///
/// **故意不做通用按键表**：`RegisterHotKey` 收的是虚拟键码，支持全表就等于把一份
/// Win32 常量表抄进本模块，而标定只需要一个「不跟别的程序打架」的组合。
/// 这里只收三组最不容易冲突的键：字母、数字、F1–F12。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyKey {
    /// `b'A'..=b'Z'`（大写）。
    Letter(u8),
    /// `b'0'..=b'9'`。
    Digit(u8),
    /// 1..=12。
    Function(u8),
}

impl HotkeyKey {
    /// 从界面传来的字符串解析。接受 `A`–`Z`、`0`–`9`、`F1`–`F12`，大小写与首尾空格都不计较。
    pub fn parse(raw: &str) -> Option<Self> {
        let text = raw.trim().to_ascii_uppercase();

        // `F` 后面跟数字才算功能键：`F1`–`F12` 是功能键，而单独的 `F` 是字母 F。
        // 两种写法重叠，所以先试「F + 数字」这一支，不成立再按字母处理。
        if let Some(digits) = text.strip_prefix('F') {
            if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) {
                let number: u8 = digits.parse().ok()?;
                return (1..=12).contains(&number).then_some(HotkeyKey::Function(number));
            }
        }

        let mut chars = text.chars();
        let ch = chars.next()?;
        if chars.next().is_some() {
            return None;
        }
        match ch {
            'A'..='Z' => Some(HotkeyKey::Letter(ch as u8)),
            '0'..='9' => Some(HotkeyKey::Digit(ch as u8)),
            _ => None,
        }
    }

    /// 对应的虚拟键码。三组键在 Win32 里各占一段**连续**区间，直接算出来，
    /// 不抄常量表：`A`–`Z` 从 0x41 起、`0`–`9` 从 0x30 起、`VK_F1` = 0x70。
    fn vk(self) -> u32 {
        match self {
            HotkeyKey::Letter(ch) => ch as u32,
            HotkeyKey::Digit(ch) => ch as u32,
            HotkeyKey::Function(number) => 0x70 + (number as u32 - 1),
        }
    }

    /// 给人看的写法。与 [`HotkeyKey::parse`] 互为逆运算，界面直接显示它。
    pub fn label(self) -> String {
        match self {
            HotkeyKey::Letter(ch) => (ch as char).to_string(),
            HotkeyKey::Digit(ch) => (ch as char).to_string(),
            HotkeyKey::Function(number) => format!("F{number}"),
        }
    }
}

/// 一个热键组合：修饰键 + 一个主键。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeySpec {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub key: HotkeyKey,
}

impl HotkeySpec {
    /// 校验并规范化。`key` 是界面传来的原始字符串。
    ///
    /// **必须至少带一个修饰键。** 裸按键一旦注册成功，那个键在整个系统里就都被本程序
    /// 截走了——注册一个「全局 A」会让用户在**任何**程序里都打不出 a。这不是「配置没生效」，
    /// 而是把别人的键盘弄坏，所以在这里直接拒绝，不留任何兜底。
    pub fn new(ctrl: bool, alt: bool, shift: bool, win: bool, key: &str) -> Result<Self, String> {
        if !(ctrl || alt || shift || win) {
            return Err(
                "热键必须带至少一个修饰键（Ctrl / Alt / Shift / Win）。只有主键的组合会把那个键\
                 在整个系统里占掉，别的程序就没法输入了。"
                    .into(),
            );
        }
        let key = HotkeyKey::parse(key)
            .ok_or_else(|| format!("不支持的按键「{key}」——只能是 A–Z、0–9 或 F1–F12。"))?;
        Ok(Self { ctrl, alt, shift, win, key })
    }

    /// 送给 `RegisterHotKey` 的修饰键位。
    ///
    /// 带上 `MOD_NOREPEAT`：按住不放时系统只投递一次。截屏是个一次性动作，
    /// 按键自动重复只会让它连打好几张同样的图。
    fn modifiers(self) -> HOT_KEY_MODIFIERS {
        let mut bits = MOD_NOREPEAT.0;
        if self.ctrl {
            bits |= MOD_CONTROL.0;
        }
        if self.alt {
            bits |= MOD_ALT.0;
        }
        if self.shift {
            bits |= MOD_SHIFT.0;
        }
        if self.win {
            bits |= MOD_WIN.0;
        }
        HOT_KEY_MODIFIERS(bits)
    }

    /// 给人看的写法，例如 `Ctrl+Alt+S`。界面直接显示它，免得两边各拼一次、拼出两种写法。
    pub fn label(self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("Ctrl".to_string());
        }
        if self.alt {
            parts.push("Alt".to_string());
        }
        if self.shift {
            parts.push("Shift".to_string());
        }
        if self.win {
            parts.push("Win".to_string());
        }
        parts.push(self.key.label());
        parts.join("+")
    }
}

/// 已注册的热键。
///
/// **drop 即注销**：热键是全局资源，忘了注销就会一直占着那个组合，
/// 别的程序再也注册不上。所以注销不依赖调用方记得调 [`release`](Self::release)。
pub struct HotkeyRegistration {
    /// 注册它的那个线程。注销必须回到这个线程去做，见 [`HotkeyRegistration::stop`]。
    thread_id: u32,
    /// `None` = 已经收过尾了（`release` 调用过，或线程早已退出）。
    join: Option<JoinHandle<()>>,
}

impl HotkeyRegistration {
    /// 注销并等收尾线程退出。
    pub fn release(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        let Some(join) = self.join.take() else {
            return;
        };

        // 往目标线程的消息队列里塞一条 `WM_QUIT`：它的 `GetMessageW` 随即返回 0，
        // 跳出循环，并在退出前自己把热键注销掉。
        //
        // **为什么让子线程自己注销**：`UnregisterHotKey(None, id)` 注销的是
        // **调用线程**注册的热键。从别的线程调它，注销的不是这一个——
        // 表面上"成功"了，热键却还占着，下一个注册同样的组合会拿到 1409。
        // SAFETY: `thread_id` 来自 `GetCurrentThreadId()`，是那个线程真实存在的 id；
        // 消息参数全是常量，没有指针。投递失败（线程已退出）不影响正确性——
        // 线程退出时系统会自动回收它注册的热键。
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        let _ = join.join();
    }
}

impl Drop for HotkeyRegistration {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 注册一个全局热键；组合键命中时在**独立线程**上调用 `on_press`。
///
/// 为什么必须是独立线程：`WM_HOTKEY` 投递到**消息队列**，得有线程去收。主线程的队列
/// 被 Tauri 的事件循环占着，塞进去等于要求 Tauri 帮我们转发；独立线程自己收自己处理，
/// 与界面线程完全解耦，也不占用界面的消息处理。
///
/// `on_press` 运行在那个线程上，**不要在里面做重活**——它一慢，后面的按键就堆在队列里。
/// 本程序的用法只是往界面发一条事件。
pub fn register(
    spec: HotkeySpec,
    on_press: impl Fn() + Send + 'static,
) -> Result<HotkeyRegistration, String> {
    let (tx, rx) = mpsc::channel::<Result<u32, String>>();

    let join = thread::Builder::new()
        .name("hotkey".into())
        .spawn(move || {
            let modifiers = spec.modifiers();
            let vk = spec.key.vk();

            // 先主动碰一次消息队列。消息队列是**惰性创建**的：`GetMessageW` 会顺手建，
            // 但 `PostThreadMessageW` 不会——它要求队列已经存在，否则直接失败。
            // 这里先建好，免得后面收尾时那条 `WM_QUIT` 因为队列还没建而投递不进去。
            let mut warmup = MSG::default();
            // SAFETY: `warmup` 是本栈上的 `MSG`，生命周期覆盖本次调用；
            // 其余参数是常量。`PM_NOREMOVE` 表示只探不看，不会取走任何消息。
            unsafe {
                let _ = PeekMessageW(&mut warmup, None, 0, 0, PM_NOREMOVE);
            }

            // `None` 作为窗口句柄 = 把热键挂到**本线程**上，`WM_HOTKEY` 进本线程队列。
            // SAFETY: 句柄传 `None`（挂到本线程），id 与键码是常量，没有指针参与。
            match unsafe { RegisterHotKey(None, HOTKEY_ID, modifiers, vk) } {
                Ok(()) => {
                    // SAFETY: 无参数、无指针，只取当前线程 id。
                    let thread_id = unsafe { GetCurrentThreadId() };
                    if tx.send(Ok(thread_id)).is_err() {
                        // 父线程已经不要结果了（提前退出）。别留着热键占坑。
                        // SAFETY: 与上面注册同一个线程、同一个 id，注销的正是刚注册的那一个。
                        unsafe {
                            let _ = UnregisterHotKey(None, HOTKEY_ID);
                        }
                        return;
                    }
                }
                Err(err) => {
                    let _ = tx.send(Err(describe_failure(err.code().0 as u32 & 0xFFFF, &err.to_string())));
                    return;
                }
            }

            loop {
                let mut msg = MSG::default();
                // SAFETY: `msg` 是本栈上的 `MSG`；句柄传 `None` = 取本线程队列里的消息。
                // 过滤范围 0..0 表示不过滤，任何消息都取。
                let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
                // 返回 0 = 收到 `WM_QUIT`；返回 -1 = 出错。两者都收工。
                if got.0 <= 0 {
                    break;
                }
                if msg.message == WM_HOTKEY {
                    on_press();
                }
                // 本线程没有窗口，取到的消息不需要 `DispatchMessage` 转发；
                // 队列里出现别的消息（理论上不会有）直接丢弃即可。
            }

            // SAFETY: 注销必须发生在**注册它的这个线程**上，这里正是那个线程；
            // id 与注册时一致。失败也无所谓——线程退出时系统会回收。
            unsafe {
                let _ = UnregisterHotKey(None, HOTKEY_ID);
            }
        })
        .map_err(|err| format!("无法创建热键线程：{err}"))?;

    // 等到子线程回报注册结果再返回：界面要凭这个结果决定是显示"已生效"还是提示换一个组合。
    match rx.recv() {
        Ok(Ok(thread_id)) => Ok(HotkeyRegistration { thread_id, join: Some(join) }),
        Ok(Err(reason)) => {
            let _ = join.join();
            Err(reason)
        }
        Err(_) => {
            let _ = join.join();
            Err("热键线程在报告注册结果之前就退出了。可以直接用「延时截图」。".into())
        }
    }
}

/// 把注册失败翻译成一句**能照着做**的话。
///
/// 单独拆出来是为了能直接对错误码写测试：`RegisterHotKey` 最常见的失败就是
/// 「这个组合已经被别的程序占用了」（1409），那时候唯一的出路是换一个组合——
/// 必须让操作者看到"换个键"这个动作，而不是一句原始错误码。
fn describe_failure(code: u32, detail: &str) -> String {
    if code == ERROR_HOTKEY_ALREADY_REGISTERED {
        format!(
            "这个组合键已经被别的程序占用了（错误码 {code}），换一个再试——\
             或者直接用「延时截图」。"
        )
    } else {
        format!("注册热键失败（错误码 {code}）：{detail}。可以直接用「延时截图」。")
    }
}

#[cfg(test)]
mod tests;
