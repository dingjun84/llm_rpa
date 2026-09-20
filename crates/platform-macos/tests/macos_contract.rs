//! 平台层契约测试（不依赖真实桌面）。
//!
//! 真实窗口相关用例仅在 macOS 上启用。

use platform_macos::macosapi;

#[test]
fn fingerprint_is_deterministic_and_content_sensitive() {
    let a = vec![1u8, 2, 3, 4];
    let b = vec![1u8, 2, 3, 5];
    assert_eq!(
        macosapi::fingerprint_of(&a, 2, 2),
        macosapi::fingerprint_of(&a, 2, 2)
    );
    assert_ne!(
        macosapi::fingerprint_of(&a, 2, 2),
        macosapi::fingerprint_of(&b, 2, 2)
    );
    assert_eq!(macosapi::fingerprint_of(&a, 2, 2).len(), 64);
}

#[test]
fn is_supported_matches_target() {
    assert_eq!(platform_macos::IS_SUPPORTED, cfg!(target_os = "macos"));
}

#[cfg(target_os = "macos")]
#[test]
fn screen_metrics_are_plausible_on_macos() {
    use automation_core::DesktopPlatform;
    use platform_macos::{MacOSDesktop, MacOSDesktopConfig};

    let desktop = MacOSDesktop::new(MacOSDesktopConfig::default());
    let metrics = desktop.screen_metrics().expect("应能读取主显示器指标");
    assert!(metrics.width >= 640, "宽度异常：{}", metrics.width);
    assert!(metrics.height >= 480, "高度异常：{}", metrics.height);
    assert!(metrics.scale_factor > 0.0 && metrics.scale_factor <= 8.0);
}
