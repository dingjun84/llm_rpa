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
