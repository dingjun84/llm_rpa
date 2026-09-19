//! 「一键截屏」的全局热键接线。
//!
//! 热键本身（注册、消息循环、注销）在 `platform_windows::hotkey` 里。这一层只做两件事：
//!
//! 1. 把命令层的参数（修饰键 + 主键）转成平台层的 `HotkeySpec`，并持有注册句柄；
//! 2. 组合键命中时**只往界面发一条事件**，自己不动手截图。
//!
//! ★ 第 2 条是刻意的。截哪张图取决于界面上**当前正在标定哪个界面**、以及配置
//!   **草稿**里的窗口类名——这两样只有界面知道。这里要是去读已保存的配置，就会出现
//!   「界面上明明写着新类名，截出来的却是旧窗口」这种自相矛盾的画面。所以热键只说
//!   一句「可以截了」，实际截图仍走界面已经在用的那条 `preview_target_window`。
//!
//! ★ 这个热键**只服务标定**：它不参与任务执行，不产生任何输入，也不改变窗口焦点。
//!   注册是界面显式发起的（进标定页才注册、离开就注销），不是程序一启动就占着。

use std::sync::Mutex;

use serde::Deserialize;
use tauri::{AppHandle, Emitter, Runtime, State};

#[cfg(windows)]
use platform_windows::hotkey::{register as register_hotkey, HotkeyRegistration, HotkeySpec};

/// 组合键命中时发给界面的事件名。
///
/// 与 `task://updated` 同一种命名风格：`<域>://<动作>`。域取 `calibration`——
/// 这个热键只服务标定。
pub const EVENT_CAPTURE_HOTKEY: &str = "calibration://capture-hotkey";

/// 拿不到状态锁时的统一说法。锁只可能因为上一次操作 panic 而中毒，
/// 那种情况下重试一次就好，不必让操作者去理解"中毒"是什么。
const LOCK_BUSY: &str = "热键状态正被上一次操作占着，请重试。";

/// 非 Windows 上的占位类型。
///
/// 平台层整层只在 Windows 编译（`platform-windows` 在 `src-tauri/Cargo.toml` 里挂在
/// `[target.'cfg(windows)'.dependencies]`），但命令层在别的平台上也要能编过。
/// 那边注册永远失败，界面会明确提示改用「延时截图」。
#[cfg(not(windows))]
struct HotkeyRegistration;

#[cfg(not(windows))]
impl HotkeyRegistration {
    fn release(self) {}
}

/// 当前已注册的热键。`None` = 没注册。
///
/// **只留一个槽位**：同时注册多个热键对截屏没有意义，多出来的槽位只会带来
/// 「注销时该销哪个」的问题。注册新的会先把旧的释放掉。
#[derive(Default)]
pub struct HotkeyState(Mutex<Option<HotkeyRegistration>>);

/// 把热键状态挂到 Tauri 的托管容器上。
///
/// ★ **调用点必须在 `with_commands` 里，不能放 `run()` 的 `.setup()`。**
/// `with_commands` 是生产入口（`Wry`）与集成测试（`MockRuntime`，见 `tests/ipc_flow.rs`）
/// 共用的装配点，而 `Builder::setup` 只在 `App::run()` 里执行——测试的 `build()`
/// 不触发它。放在这里，两条路径的状态注册不可能不一致；放 setup 里就等于给测试
/// 埋一个「必须有人记得手动同步」的坑。
///
/// `HotkeyState::default()` 不需要 `AppHandle`，所以能挂在 builder 阶段。
/// （`AppState` 不行——它要读数据目录路径，只能在 setup 里造。）
///
/// ⚠️ 这里只挂**空状态**，不注册任何热键：注册由界面进标定页时显式发起，
/// 程序一启动就占着全局热键不是这个功能该有的行为。
pub fn attach<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder.manage(HotkeyState::default())
}

/// 界面传来的组合键。
#[derive(Debug, Clone, Deserialize)]
pub struct HotkeyRequest {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    /// 主键，例如 `S` / `7` / `F9`。解析规则见 `platform_windows::hotkey::HotkeyKey::parse`。
    pub key: String,
}

/// 注册「一键截屏」热键；命中时向界面发一条 [`EVENT_CAPTURE_HOTKEY`]。
///
/// 返回**规范化后的组合键写法**（例如 `Ctrl+Alt+S`）。让后端返回而不是界面自己拼，
/// 是为了两边不可能拼出两种写法——界面显示的和实际注册的永远是同一个。
///
/// 已经注册过时**先释放旧的再注册新的**：不释放就会有两个热键同时生效，
/// 操作者按旧组合照样触发截屏，而他以为已经改掉了。
#[tauri::command]
pub fn register_capture_hotkey<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, HotkeyState>,
    request: HotkeyRequest,
) -> Result<String, String> {
    #[cfg(not(windows))]
    {
        let _ = (app, state, request);
        return Err("全局热键目前只支持 Windows，请改用「延时截图」。".into());
    }

    #[cfg(windows)]
    {
        let spec = HotkeySpec::new(
            request.ctrl,
            request.alt,
            request.shift,
            request.win,
            &request.key,
        )?;

        let mut slot = state.0.lock().map_err(|_| LOCK_BUSY.to_string())?;
        if let Some(previous) = slot.take() {
            previous.release();
        }

        // 命中时只发一条事件。`emit` 是线程安全的，可以在这个后台线程上直接调。
        let registration = register_hotkey(spec, move || {
            let _ = app.emit(EVENT_CAPTURE_HOTKEY, ());
        })?;

        *slot = Some(registration);
        Ok(spec.label())
    }
}

/// 注销「一键截屏」热键。
///
/// **幂等**：没注册时也返回成功。界面在离开标定页、以及每次重新注册前都会调它，
/// 而"本来就没注册"和"已经注销掉了"对调用方是同一件事——
/// 让它返回错误只会逼界面写一堆无意义的判断。
#[tauri::command]
pub fn unregister_capture_hotkey(state: State<'_, HotkeyState>) -> Result<(), String> {
    let mut slot = state.0.lock().map_err(|_| LOCK_BUSY.to_string())?;
    if let Some(registration) = slot.take() {
        registration.release();
    }
    Ok(())
}
