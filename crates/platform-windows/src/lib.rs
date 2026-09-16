//! Windows 平台适配层。
//!
//! 实现 `automation-core` 的 [`automation_core::DesktopPlatform`] 端口：
//! 窗口定位与前台化、局部屏幕捕获、受保护的鼠标点击、剪贴板粘贴与发送快捷键。
//!
//! 安全边界（见 `docs/architecture.md` §8 与 `docs/windows-mvp-interface.md`）：
//!
//! - 只操作用户当前可见、已解锁的交互式桌面；
//! - 只在用户显式配置且校验通过后启动企业微信，不猜路径、不提权；
//! - 点击前校验前台窗口与标定一致，不一致就拒绝输入；
//! - 不注入、不 Hook、不读取其他进程内存。

pub mod config;
pub mod desktop;

#[cfg(windows)]
pub mod winapi;

pub use config::{WindowMatcher, WindowsDesktopConfig};
pub use desktop::WindowsDesktop;

/// 本 crate 只在 Windows 上提供真实实现。
pub const IS_SUPPORTED: bool = cfg!(windows);
