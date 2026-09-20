//! macOS 平台适配层。
//!
//! 实现 `automation-core` 的 [`automation_core::DesktopPlatform`] 端口：
//! 窗口定位与前台化、局部屏幕捕获、受保护的鼠标点击、剪贴板粘贴与发送快捷键。
//!
//! 安全边界与 Windows 侧一致（见 `docs/architecture.md` §8 与 `docs/macos.md`）：
//!
//! - 只操作用户当前可见、已解锁的交互式桌面；
//! - 只在用户显式配置且校验通过后启动客户端，不猜路径、不提权；
//! - 点击前校验前台窗口与标定一致，不一致就拒绝输入；
//! - 不注入、不 Hook、不读取其他进程内存；
//! - 首次真实使用前必须授予「辅助功能」与「屏幕录制」权限。
//!
//! ## 与 Windows 的配置语义差异
//!
//! macOS 没有 Win32「窗口类名」。配置里的 `window_class` 在本适配层被解释为
//! **窗口所有者名**（`CGWindowOwnerName`，即应用显示名，例如「企业微信」或「微信」）。
//! 指认窗口时会把该字段填成所有者名；可选的 `wecom_exe` 仍用于校验进程归属
//! （`.app` 包路径或其内部可执行文件路径）。

pub mod config;
pub mod desktop;
pub mod macosapi;

pub use config::{MacOSDesktopConfig, WindowMatcher};
pub use desktop::MacOSDesktop;

/// 本 crate 只在 macOS 上提供真实实现；其它平台可编译，但所有操作会返回明确错误。
pub const IS_SUPPORTED: bool = cfg!(target_os = "macos");
