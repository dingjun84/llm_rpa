//! 光标轨迹：把光标**看得见地**送过去，而不是一步瞬移。
//!
//! ## 为什么单独成文件
//!
//! 这一段自成一类 —— 纯几何计算（[`circle_points`]）加一个输入原语
//! （[`move_cursor_absolute`]，走 `SendInput`）。它与窗口枚举、截屏、剪贴板、
//! 键盘事件没有任何耦合，却夹在 `winapi.rs` 中间把那个文件顶到基线之上。
//! 搬到这里以后，`winapi.rs` 回到「窗口 / 截屏 / 剪贴板 / 键鼠事件」的骨架。
//!
//! ## 两条不能改的约定
//!
//! 1. **走 `SendInput`，不用 `SetCursorPos`** —— 理由见 [`move_cursor_absolute`]。
//! 2. **不注入随机抖动** —— 轨迹必须可复现，否则「这次成功那次失败」查不清。
//!
//! ## 可见性
//!
//! 只有 [`move_cursor`] / [`move_cursor_circle`] / [`CircleTrace`] 需要对外，
//! 父模块把它们再导出成 `winapi::move_cursor` 等，调用方不必知道本模块存在。
//! [`CIRCLE_MIN_STEPS`] 只给同级的 `winapi::tests` 用，所以是 `pub(super)`。

use std::time::Duration;

use windows::Win32::UI::Input::KeyboardAndMouse::{SendInput, INPUT};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN,
};

use super::input::mouse_move_input;
use super::{cursor_position, WinResult};

// ── 光标轨迹 ────────────────────────────────────────────────────────────

/// 光标轨迹相邻两步之间的间隔。
///
/// 8ms ≈ 125Hz，正是最常见 USB 鼠标的**默认回报率**：真鼠标每秒只向系统报告约
/// 125 次位置，系统的鼠标消息也按这个节奏合并。按它走，轨迹在系统里留下的痕迹
/// 与真鼠标是同一种东西。
///
/// 写死而不做成配置，是因为它描述的是"真鼠标长什么样"，**不随机器变**
/// （换台机器，USB 鼠标还是 125Hz）。该由人调的是**速度**，那个是配置。
const POINTER_STEP_INTERVAL: Duration = Duration::from_millis(8);

/// 一次移动的最短耗时。
///
/// 低于这个时长，人眼看到的仍是一次瞬移——正是要消除的那个现象。
const POINTER_MIN_DURATION: Duration = Duration::from_millis(120);

/// 一次移动的最长耗时。
///
/// 人做一次大幅度移动也就是这个量级；再长就不像"移动"而像"卡住"，
/// 而且每个点击都要多等这么久。
///
/// **代价**：真正超长的距离（跨多个显示器）会被压缩到这个时长，轨迹会显得偏快。
/// 这是刻意的取舍——「看得见」比「严格按速度走完」更重要，而且速度本身是可配的。
const POINTER_MAX_DURATION: Duration = Duration::from_millis(800);

/// 把光标沿一条**人形轨迹**移到 `(x, y)`。
///
/// ## 为什么要走轨迹
///
/// 一步跳过去（`SetCursorPos`）在系统里留下的是"光标凭空出现在别处"：操作者
/// 看不出发生了什么，目标程序的悬停/移入事件也只收到一个终态。按真鼠标那样分步
/// 走，光标是**看得见地飞过去**的，目标程序依次收到的也是与真鼠标同型的移动消息。
///
/// ## 轨迹形状
///
/// 用 smoothstep（`3t² - 2t³`）缓动：起步慢、中间快、收尾慢，与人手做指向动作
/// 时的速度曲线同型。**不用随机抖动**——那会把「同一个点击为什么这次成功那次
/// 失败」变成查不清的问题，而本项目要求行为可复现。
///
/// ## 参数怎么来的
///
/// 总时长只由**距离与速度**算出（`距离 ÷ 速度`），再按 [`POINTER_STEP_INTERVAL`]
/// 切成若干步。速度由调用方从配置传入，所以"快慢"可调，"像不像人手"是算出来的。
pub fn move_cursor(x: i32, y: i32, speed_px_per_sec: f64) -> WinResult<()> {
    if !speed_px_per_sec.is_finite() || speed_px_per_sec <= 0.0 {
        return Err(format!("光标速度必须是正数，收到 {speed_px_per_sec}"));
    }

    let (from_x, from_y) = cursor_position()?;
    let dx = (x - from_x) as f64;
    let dy = (y - from_y) as f64;
    let distance = (dx * dx + dy * dy).sqrt();
    // 已经在那儿了（或差不到一个像素）就直接落位：别为了"像人"在原地抖满最短时长。
    if distance < 1.0 {
        return move_cursor_absolute(x, y);
    }

    let duration = Duration::from_secs_f64(distance / speed_px_per_sec)
        .clamp(POINTER_MIN_DURATION, POINTER_MAX_DURATION);
    let steps =
        ((duration.as_secs_f64() / POINTER_STEP_INTERVAL.as_secs_f64()).round() as u32).max(1);
    let step_delay = duration / steps;

    for step in 1..=steps {
        let t = step as f64 / steps as f64;
        let eased = t * t * (3.0 - 2.0 * t);
        let px = (from_x as f64 + dx * eased).round() as i32;
        let py = (from_y as f64 + dy * eased).round() as i32;
        move_cursor_absolute(px, py)?;
        // 最后一步之后不再睡：位置已经到位，多等一帧只是拖慢每个点击。
        if step < steps {
            std::thread::sleep(step_delay);
        }
    }
    Ok(())
}

/// 画一圈的最短耗时。
///
/// 低于这个时长，人眼只会看到"闪了一下"，看不出那是条圆形轨迹——而**看得见**
/// 正是这条功能存在的唯一理由。
const CIRCLE_MIN_DURATION: Duration = Duration::from_millis(400);

/// 画一圈的最长耗时。
///
/// 速度是配置项（`pointer_speed_px_per_sec`），配得极小、或者窗口特别大时，
/// 一圈会走很久，看起来像程序卡死。这里封顶，并且**把实际时长回报给界面**
/// （见 [`CircleTrace`]），免得人只能靠猜。
///
/// **代价**：触顶时实际走的速度会快于配置的速度。这是刻意的取舍——
/// "别看起来像卡死"比"严格按配置速度走完"更重要，而且这是一条给人看的演示。
const CIRCLE_MAX_DURATION: Duration = Duration::from_secs(8);

/// 一圈最少切多少步。
///
/// 8ms 一步的算法在"半径小 + 速度快"时会算出很少的步数，少到圆周变成肉眼可见的
/// 多边形（正三角形也是"圆"）。这里给一个下限，保证它看上去是圆的。
pub(super) const CIRCLE_MIN_STEPS: u32 = 24;

/// 走完一圈的实际情况（回报给调用方）。
///
/// 界面要把它显示出来：操作者按的是个"看起来会等一会儿"的按钮，
/// 先说清要等多久、圆有多大，比事后解释好。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CircleTrace {
    /// 圆心（屏幕坐标）——就是调用那一刻的光标位置。
    pub center: (i32, i32),
    /// 半径（像素）。
    pub radius: i32,
    /// 圆周被切成了多少步。
    pub steps: u32,
    /// 实际走完一圈用的时长。
    pub duration: Duration,
}

/// 圆周上均匀分布的 `steps` 个点（从正右方开始，屏幕坐标系里顺时针走）。
///
/// 单独抽出来是因为它是**纯计算**：不碰 Win32、不产生任何输入，可以直接用用例
/// 钉住"每个点都落在圆周上""首尾闭合"这两件事。而轨迹里最容易错的恰恰是这一步
/// （半径当成直径、角度少走一圈、角度步长算错），它错了只会表现为"圆画得不对"，
/// 从 `SendInput` 的返回值上**完全看不出来**。
///
/// `steps` 为 0 时返回空表（除零会算出 NaN，而 NaN 转 `i32` 是未定义行为）。
pub fn circle_points(center: (i32, i32), radius: i32, steps: u32) -> Vec<(i32, i32)> {
    if steps == 0 {
        return Vec::new();
    }
    let (cx, cy) = center;
    let radius = radius as f64;
    (1..=steps)
        .map(|step| {
            let angle = std::f64::consts::TAU * step as f64 / steps as f64;
            (
                cx + (radius * angle.cos()).round() as i32,
                cy + (radius * angle.sin()).round() as i32,
            )
        })
        .collect()
}

/// 以 `center` 为圆心，让光标沿圆周**走满一圈**。
///
/// ## 为什么单独有这个函数
///
/// 它是**给人看的**：把光标轨迹单独拎出来跑一遍，肉眼就能确认"鼠标是走过去的、
/// 不是跳过去的"，以及走起来顺不顺。混在完整流程里时，轨迹的问题会被
/// "找不到联系人""点歪了"之类的现象盖住，排查方向会一路偏掉。
///
/// ## 时序
///
/// 与 [`move_cursor`] 同一套：总时长 = 周长 ÷ 速度，按 [`POINTER_STEP_INTERVAL`]
/// 切步。**不做缓动**——smoothstep 是给"从 A 点到 B 点"这种有始有终的动作用的；
/// 圆周是匀速运动，逐段缓动只会让它一顿一顿的。
///
/// 开始之前先按普通轨迹走到圆的起点（正右方那一点）：**不能直接跳过去**，
/// 跳过去正是"光标凭空出现在别处"，而这条功能存在的意义就是让人看见它怎么走。
///
/// 只移动光标，**不点击、不输入、不抢前台**。
pub fn move_cursor_circle(
    center: (i32, i32),
    radius: i32,
    speed_px_per_sec: f64,
) -> WinResult<CircleTrace> {
    if radius <= 0 {
        return Err(format!("圆的半径必须是正数，收到 {radius}"));
    }
    if !speed_px_per_sec.is_finite() || speed_px_per_sec <= 0.0 {
        return Err(format!("光标速度必须是正数，收到 {speed_px_per_sec}"));
    }

    let (cx, cy) = center;
    move_cursor(cx + radius, cy, speed_px_per_sec)?;

    let circumference = std::f64::consts::TAU * radius as f64;
    let duration = Duration::from_secs_f64(circumference / speed_px_per_sec)
        .clamp(CIRCLE_MIN_DURATION, CIRCLE_MAX_DURATION);
    let steps = ((duration.as_secs_f64() / POINTER_STEP_INTERVAL.as_secs_f64()).round() as u32)
        .max(CIRCLE_MIN_STEPS);
    let step_delay = duration / steps;

    let points = circle_points(center, radius, steps);
    let last = points.len();
    for (index, (x, y)) in points.into_iter().enumerate() {
        move_cursor_absolute(x, y)?;
        // 最后一步之后不再睡：位置已经到位，多等一帧只是拖长这次演示。
        if index + 1 < last {
            std::thread::sleep(step_delay);
        }
    }

    Ok(CircleTrace { center, radius, steps, duration })
}

/// 发一次绝对坐标的鼠标移动。
///
/// 走 `SendInput` 而不是 `SetCursorPos`：前者是**真实输入管线**，返回值就是系统
/// 实际接受的事件数——被拦下时是 0，我们能当场发现。后者存在"返回成功、光标却没动"
/// 的情形（前台窗口属于更高完整性的进程等），而"鼠标没动"正是最难查的成因：
/// 后面那次点击会落到光标**实际停着**的地方。
fn move_cursor_absolute(x: i32, y: i32) -> WinResult<()> {
    let (left, top, width, height) = virtual_screen_rect();
    if width <= 1 || height <= 1 {
        return Err(format!("虚拟桌面尺寸异常（{width}x{height}），拒绝发送绝对坐标"));
    }
    // 绝对坐标要归一化到 0..=65535，且 65535 对应虚拟桌面的**右下角像素**，
    // 所以分母是 (尺寸 - 1)：用尺寸会让整条轨迹往右下偏一格。
    let nx = ((x - left) as i64 * 65_535 / (width as i64 - 1)) as i32;
    let ny = ((y - top) as i64 * 65_535 / (height as i64 - 1)) as i32;

    let input = mouse_move_input(nx, ny);
    let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
    if sent != 1 {
        return Err(format!(
            "移动鼠标到 ({x}, {y}) 失败：系统没有接受这次输入（目标窗口可能属于更高权限的进程）"
        ));
    }
    Ok(())
}

/// 虚拟桌面（所有显示器合起来）的左上角与尺寸。
///
/// 多显示器下主屏左上角不一定是 (0,0)——左侧或上方的显示器会让坐标为负。
/// 而 `MOUSEEVENTF_ABSOLUTE` 的归一化基准是整个**虚拟桌面**，不是主屏；
/// 按主屏算，副屏上的坐标会整体偏移。
fn virtual_screen_rect() -> (i32, i32, i32, i32) {
    // SAFETY: `GetSystemMetrics` 是纯只读查询，无指针、无所有权。
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}
