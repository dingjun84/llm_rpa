//! 「画圆」自检：让光标沿圆周走一圈。
//!
//! 它是**给人看的**：把光标轨迹单独拎出来跑一遍，肉眼就能确认"鼠标是走过去的、
//! 不是跳过去的"，以及走起来顺不顺。混在完整流程里时，轨迹的问题会被
//! 「找不到联系人」「点歪了」之类的现象盖住，排查方向会一路偏掉。
//!
//! ★ 放在单独一个模块里，与 `capture_hotkey.rs` 同一个理由：`lib.rs` 的基线是
//!   1903 行，这一段自己就有近百行。命令仍然在 `with_commands` 里注册——
//!   **注册点只有一处**，别在这里再挂一个 `invoke_handler`。
//!
//! ★ 平台原语在 `platform_windows::winapi`：圆周的**纯计算**部分（`circle_points`）
//!   另有不碰光标的用例钉着（`winapi/tests.rs`），因为轨迹算错了 `SendInput`
//!   照样返回成功。取舍与验收清单见 `docs/todo.md` T24。

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};

/// 「画圆」自检的结果（回给界面显示）。
///
/// 这几个数界面**都要报出来**：操作者按的是个"看起来会等一会儿"的按钮，
/// 先说清圆多大、圆心在哪、要等多久，比事后解释好。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircleTraceView {
    /// 圆心（屏幕坐标）——就是调用那一刻的光标位置。
    pub center: [i32; 2],
    /// 半径（像素）。
    pub radius: i32,
    /// 半径是按**这个**窗口尺寸的短边一半算出来的（界面显示出来便于核对）。
    pub window_width: u32,
    pub window_height: u32,
    /// 圆周被切成了多少步。
    pub steps: u32,
    /// 实际走完一圈用掉的时长（毫秒）。
    pub duration_ms: u64,
    /// 用的是哪个速度（像素/秒）。回给界面是为了让「这里看到的快慢」与
    /// 「任务里点击时走的快慢」能被核对，而不是靠猜。
    pub speed_px_per_sec: f64,
    /// 走完之后**实测**的光标位置（屏幕坐标）。
    ///
    /// ## 为什么要有它
    ///
    /// "命令返回了"和"光标真的动了"是两件事。只报"我让它走到哪儿"的话，
    /// 一旦轨迹整段没生效（坐标被夹、被前台锁定吞掉…），界面会显示一个
    /// **看起来完全正常**的结果，而操作者眼里光标纹丝没动——两边对不上，
    /// 谁也不知道该信哪边。读一次真实位置，这个矛盾就消失了。
    pub end: [i32; 2],
    /// 实测终点到**圆心**的距离（像素）。
    ///
    /// 走对了的话它应当 ≈ `radius`。**界面把这两个数并排显示**，由人一眼对比——
    /// 这里刻意**不给"算不算动过"下结论**：那需要一个容差阈值，而阈值定在哪里
    /// 都是拍脑袋；这是给人看的自检，没有哪个自动判据依赖它。
    pub end_distance_px: i32,
}

/// 「画圆」：以**当前光标位置**为圆心，让光标沿圆周走满一圈。
///
/// ## 这是给人看的
///
/// 把光标轨迹单独拎出来跑一遍，肉眼就能确认「鼠标是走过去的、不是跳过去的」，
/// 以及走起来顺不顺。混在完整流程里时，轨迹的问题会被「找不到联系人」「点歪了」
/// 之类的现象盖住，排查方向会一路偏掉。
///
/// ## 圆心与半径怎么定
///
/// - **圆心 = 调用这一刻的光标位置。** 界面是「点按钮 → 倒计时 → 再调这个命令」，
///   所以操作者可以在倒计时里把鼠标挪到想要的位置；不挪就是按钮那一点。
/// - **半径 = 本程序窗口短边的一半。** 刻意**不**按目标客户端窗口算：这条功能测的
///   是鼠标轨迹本身，不该依赖微信开着、也不该依赖窗口标定做没做。
///   按**短边**而不是宽度，是为了让圆一定放得下——宽窗口按宽度算会超出屏幕高度。
///
/// ## 画完之后光标停在哪儿
///
/// **不回起点**：走满一圈之后会再多走一小段，停在圆上的另一个位置（与起点差 90°）。
/// 停在起点上的话，"走过一整圈"和"它根本没动"在事后看光标位置时是分不出来的。
/// 另外还会**实测**一次终点位置回报给界面（见 [`CircleTraceView::end`]）——
/// 位置对不对，数字说了算，不靠肉眼。
///
/// ## 速度为什么由这里定、不从界面传
///
/// 光标速度目前**不是**可配项（界面上没有这个字段）：`to_runner_config()` 给
/// 平台层的 `WindowsDesktopConfig` 用的是 `..default()`，速度就落在
/// `WindowsDesktopConfig::default().pointer_speed_px_per_sec` 上。
///
/// 所以这里也**只认那一个来源**。让界面传一个值进来的话，界面就得自己造一个数
/// （或者把平台层的默认值在 TS 里再抄一遍）——那就成了两处真相：以后有人调了
/// 平台层默认值，自检里看到的快慢会和任务里不一致，而**两边都看不出来**。
///
/// ## 为什么不受运行模式限制
///
/// 它只移动光标，**不点击、不输入、不抢前台**，所以演练模式下照样能用——
/// 与「界面标定」那组命令同一个理由（见 `docs/windows-mvp-interface.md`）。
///
/// ★ **必须是 `pub`**：命令注册在 `lib.rs` 的 `with_commands` 里，跨模块引用时
/// `#[tauri::command]` 生成的辅助宏跟着函数一起要可见。写成私有的表现是
/// `E0603: macro import ... is private`，而那行错指向 `generate_handler!`，
/// 看着像宏的问题。`capture_hotkey.rs` 里那两条同理。
#[tauri::command]
pub fn draw_cursor_circle<R: Runtime>(app: AppHandle<R>) -> Result<CircleTraceView, String> {
    #[cfg(windows)]
    {
        use platform_windows::{winapi, WindowsDesktopConfig};

        // 窗口尺寸取**内容区**：外框含不可见的缩放边框与标题栏，而这条功能是给人
        // 对着屏幕估的——差几十像素无所谓，说清取的是哪一个更重要。
        let window = app
            .get_webview_window("main")
            .ok_or_else(|| "找不到本程序的主窗口".to_string())?;
        let size = window
            .inner_size()
            .map_err(|err| format!("读不到本程序窗口的尺寸：{err}"))?;

        let radius = (size.width.min(size.height) / 2) as i32;
        if radius <= 0 {
            return Err(format!(
                "本程序窗口太小（{}×{}），算不出半径——先把窗口拉大一点。",
                size.width, size.height
            ));
        }

        let speed_px_per_sec = WindowsDesktopConfig::default().pointer_speed_px_per_sec;
        let center = winapi::cursor_position()?;
        let trace = winapi::move_cursor_circle(center, radius, speed_px_per_sec)?;

        Ok(CircleTraceView {
            center: [trace.center.0, trace.center.1],
            radius: trace.radius,
            window_width: size.width,
            window_height: size.height,
            steps: trace.steps,
            duration_ms: trace.duration.as_millis() as u64,
            speed_px_per_sec,
            end: [trace.end.0, trace.end.1],
            end_distance_px: trace.end_distance_px(),
        })
    }
    #[cfg(target_os = "macos")]
    {
        use platform_macos::{macosapi, MacOSDesktopConfig};

        let window = app
            .get_webview_window("main")
            .ok_or_else(|| "找不到本程序的主窗口".to_string())?;
        let size = window
            .inner_size()
            .map_err(|err| format!("读不到本程序窗口的尺寸：{err}"))?;

        let radius = (size.width.min(size.height) / 2) as i32;
        if radius <= 0 {
            return Err(format!(
                "本程序窗口太小（{}×{}），算不出半径——先把窗口拉大一点。",
                size.width, size.height
            ));
        }

        if !macosapi::accessibility_trusted() {
            return Err(
                "未授予「辅助功能」权限，系统会静默忽略鼠标移动。请到「系统设置 → 隐私与安全性 → 辅助功能」中启用本应用；若还需要截屏/找窗口，请同时开启「屏幕录制」。"
                    .into(),
            );
        }

        let speed_px_per_sec = MacOSDesktopConfig::default().pointer_speed_px_per_sec;
        let center = macosapi::cursor_position()?;
        let trace = macosapi::move_cursor_circle(center, radius, speed_px_per_sec)?;

        Ok(CircleTraceView {
            center: [trace.center.0, trace.center.1],
            radius: trace.radius,
            window_width: size.width,
            window_height: size.height,
            steps: trace.steps,
            duration_ms: trace.duration.as_millis() as u64,
            speed_px_per_sec,
            end: [trace.end.0, trace.end.1],
            end_distance_px: trace.end_distance_px(),
        })
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = app;
        Err("鼠标轨迹自检目前只支持 Windows / macOS".to_string())
    }
}
