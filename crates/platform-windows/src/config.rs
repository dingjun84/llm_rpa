//! 平台适配层配置。
//!
//! 所有可能影响收件人的参数都必须由使用者显式提供，
//! 代码不做任何"猜测路径"或"自动提权"的行为。

use std::path::PathBuf;
use std::time::Duration;

/// 定位企业微信窗口的方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowMatcher {
    /// 按窗口类名精确匹配（推荐，例如企业微信主窗口类名）。
    ClassName(String),
    /// 按窗口标题前缀匹配，作为类名不可用时的兜底。
    TitlePrefix(String),
}

impl Default for WindowMatcher {
    fn default() -> Self {
        // 企业微信桌面端主窗口的类名。
        Self::ClassName("WeWorkWindow".to_string())
    }
}

#[derive(Debug, Clone)]
pub struct WindowsDesktopConfig {
    /// 用户显式配置的企业微信可执行文件路径。为 `None` 时拒绝启动。
    pub wecom_exe: Option<PathBuf>,
    /// 可执行文件期望的 SHA-256（小写十六进制）。为 `None` 时跳过哈希校验。
    pub wecom_exe_sha256: Option<String>,
    pub window_matcher: WindowMatcher,
    /// 粘贴完成后清除临时剪贴板内容。
    pub clear_clipboard_after_paste: bool,
    /// 发送粘贴快捷键后，等待目标程序读取剪贴板的最长时间。
    ///
    /// `SendInput` 只把按键排队，目标程序稍后才会打开剪贴板读内容；
    /// 在它读完之前清空剪贴板会让这次粘贴变成空操作。默认 600ms。
    pub clipboard_read_timeout: Duration,
    /// 单次捕获允许的最大像素数，防止误传整屏导致内存暴涨。
    pub max_capture_pixels: u64,
    /// 只读预览允许的最大像素数。
    ///
    /// 比 [`Self::max_capture_pixels`] 宽松得多，因为标定**本来就要看整个窗口**，
    /// 而窗口可能是最大化的。默认 4000 万像素（约 8K），足以覆盖任何真实显示器，
    /// 又能挡住"某个窗口谎报了一个天文数字般的矩形"这种情况。
    pub preview_max_pixels: u64,
    /// 发出置前请求后，等前台真正切过去的最长时间。默认 400ms。
    ///
    /// `SetForegroundWindow` 返回成功只表示"请求被接受"，实际切换由窗口管理器
    /// **异步**完成。立刻去读 `GetForegroundWindow()` 会把"还没切完"误判成
    /// "切换失败"，于是明明能用的窗口被判成不可用。
    ///
    /// 400ms 的依据：真实切换通常在几十毫秒内完成；而被前台锁定策略吞掉的请求
    /// **永远不会**生效，等再久也没用。所以这是个"等结算"而不是"等重试"的窗口，
    /// 取一个远大于正常耗时、又短到不会让人察觉的值。
    pub foreground_settle_timeout: Duration,
    /// 逐字输入时，字符之间的间隔。默认 30ms。
    ///
    /// ## 为什么需要它
    ///
    /// 客户端顶部的搜索框是**联想式**的：每收到一个字符就发一次查询、刷新下拉列表。
    /// 连珠炮式地发完，联想请求会互相打断，下拉列表可能只按第一个字符的结果定格——
    /// 而现象是"搜出来的东西不对"，不会让人想到是**输入太快**。
    ///
    /// 30ms 的依据：常见联想框的防抖窗口在几十毫秒量级；这个值只让"输入 10 个字"
    /// 多花 0.3 秒，换来的是一条稳定的联想链。嫌慢就调小——它是配置，不是常量。
    pub typing_interval: Duration,
    /// 点击输入控件之后，等它真正拿到键盘焦点的时间。默认 250ms。
    ///
    /// ## 为什么需要它
    ///
    /// 点击只是把一次鼠标事件交给了目标程序；**键盘焦点要等它自己的消息循环
    /// 处理完那次点击之后**才会进到那个控件里。不等就发按键（清空、输入），
    /// 按键会落到上一个有焦点的控件上——而发送按键的 API 只负责把事件排进队列，
    /// 落点对不对它都返回成功。于是错误不在原地暴露，而是过一会儿以
    /// "搜索框里什么都没有"这种面目出现。
    ///
    /// 250ms 的依据：本仓库的诊断工具 `screen_probe clear-input` 在真实客户端上
    /// 手工验证"点输入框 → 清空"时用的就是这个值（点击 → 等 250ms → Ctrl+A →
    /// 等 120ms → Delete），实测可用。做成字段而不是常量，是为了换一台更慢的机器时
    /// 能在装配点直接调大，而不用改这里的代码。
    pub focus_settle_timeout: Duration,
    /// 光标移动的"人速"，单位：像素/秒。默认 1200。
    ///
    /// ## 为什么鼠标要走轨迹，而不是一步跳过去
    ///
    /// 一步跳过去在系统里留下的是"光标凭空出现在别处"：操作者看不出程序做了什么，
    /// 目标程序的悬停/移入事件也只收到一个终态。按真鼠标那样分步走，光标是
    /// **看得见地飞过去**的，客户端依次收到的也是与真鼠标同型的移动消息。
    ///
    /// ## 1200 的依据
    ///
    /// 人手做一次指向动作，峰值速度大致在每秒一千到两千像素这个量级。取 1200，
    /// 一次 500px 的移动约花 0.4 秒——明显看得见，又不至于拖慢任务。
    /// 嫌快嫌慢直接改这个值：**它是配置，不是常量**。
    ///
    /// ⚠️ 总时长还会被上下限夹住（见 `winapi` 的 `POINTER_MIN_DURATION` /
    /// `POINTER_MAX_DURATION`）：太短就成了瞬移，太长就不像移动而像卡住。
    pub pointer_speed_px_per_sec: f64,
}

impl Default for WindowsDesktopConfig {
    fn default() -> Self {
        Self {
            wecom_exe: None,
            wecom_exe_sha256: None,
            window_matcher: WindowMatcher::default(),
            clear_clipboard_after_paste: true,
            clipboard_read_timeout: Duration::from_millis(600),
            max_capture_pixels: 4_000_000,
            preview_max_pixels: 40_000_000,
            foreground_settle_timeout: Duration::from_millis(400),
            typing_interval: Duration::from_millis(30),
            focus_settle_timeout: Duration::from_millis(250),
            pointer_speed_px_per_sec: 1200.0,
        }
    }
}

impl WindowsDesktopConfig {
    /// 面向测试或自定义目标的配置：按标题前缀匹配任意窗口。
    pub fn for_title_prefix(prefix: impl Into<String>) -> Self {
        Self {
            window_matcher: WindowMatcher::TitlePrefix(prefix.into()),
            ..Self::default()
        }
    }
}
