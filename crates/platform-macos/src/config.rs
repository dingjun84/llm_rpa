//! 平台适配层配置。
//!
//! 所有可能影响收件人的参数都必须由使用者显式提供，
//! 代码不做任何"猜测路径"或"自动提权"的行为。

use std::path::PathBuf;
use std::time::Duration;

/// 定位目标窗口的方式。
///
/// ## macOS 语义
///
/// - [`WindowMatcher::ClassName`]：**所有者名**精确匹配（`CGWindowOwnerName`），
///   不是 Win32 类名。界面上仍叫「窗口类名」是为了与配置字段兼容——
///   在 Mac 上请填应用显示名（指认窗口会自动填好）。
/// - [`WindowMatcher::TitlePrefix`]：按窗口标题前缀匹配，作为兜底。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowMatcher {
    /// 按所有者名（应用显示名）精确匹配。
    ClassName(String),
    /// 按窗口标题前缀匹配。
    TitlePrefix(String),
}

impl Default for WindowMatcher {
    fn default() -> Self {
        // 企业微信 macOS 客户端常见显示名；不同语言环境可能不同，
        // 真实使用请用「指认窗口」写入实际值。
        Self::ClassName("企业微信".to_string())
    }
}

#[derive(Debug, Clone)]
pub struct MacOSDesktopConfig {
    /// 用户显式配置的客户端可执行文件 / `.app` 路径。为 `None` 时拒绝启动。
    pub wecom_exe: Option<PathBuf>,
    /// 可执行文件期望的 SHA-256（小写十六进制）。为 `None` 时跳过哈希校验。
    pub wecom_exe_sha256: Option<String>,
    pub window_matcher: WindowMatcher,
    /// 粘贴完成后清除临时剪贴板内容。
    pub clear_clipboard_after_paste: bool,
    /// 发送粘贴快捷键后，等待目标程序读取剪贴板的最长时间。默认 600ms。
    pub clipboard_read_timeout: Duration,
    /// 单次捕获允许的最大像素数。
    pub max_capture_pixels: u64,
    /// 只读预览允许的最大像素数。
    pub preview_max_pixels: u64,
    /// 发出置前请求后，等前台真正切过去的最长时间。默认 400ms。
    pub foreground_settle_timeout: Duration,
    /// 逐字输入时，字符之间的间隔。默认 30ms。
    pub typing_interval: Duration,
    /// 点击输入控件之后，等它真正拿到键盘焦点的时间。默认 250ms。
    pub focus_settle_timeout: Duration,
    /// 光标移动速度（像素/秒）。默认 1200。
    pub pointer_speed_px_per_sec: f64,
}

impl Default for MacOSDesktopConfig {
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

impl MacOSDesktopConfig {
    /// 面向测试或自定义目标的配置：按标题前缀匹配任意窗口。
    pub fn for_title_prefix(prefix: impl Into<String>) -> Self {
        Self {
            window_matcher: WindowMatcher::TitlePrefix(prefix.into()),
            ..Self::default()
        }
    }
}
