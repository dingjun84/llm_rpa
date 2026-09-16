//! 平台层测试。
//!
//! 不依赖企业微信：凡是需要真实窗口的用例都会在缺少交互式桌面时优雅跳过。
//! 真正被验证的是"不猜路径、不越界、不误输入"这几条硬约束。

#![cfg(windows)]

use automation_core::{AutomationError, DesktopPlatform, Rect};
use platform_windows::{winapi, WindowMatcher, WindowsDesktop, WindowsDesktopConfig};

#[test]
fn fingerprint_is_deterministic_and_content_sensitive() {
    let a = vec![1u8, 2, 3, 4];
    let b = vec![1u8, 2, 3, 5];

    assert_eq!(
        winapi::fingerprint_of(&a, 2, 2),
        winapi::fingerprint_of(&a, 2, 2)
    );
    assert_ne!(
        winapi::fingerprint_of(&a, 2, 2),
        winapi::fingerprint_of(&b, 2, 2)
    );
    // 同样的像素但尺寸不同，指纹也必须不同。
    assert_ne!(
        winapi::fingerprint_of(&a, 2, 2),
        winapi::fingerprint_of(&a, 4, 1)
    );
    assert_eq!(winapi::fingerprint_of(&a, 2, 2).len(), 64);
}

#[test]
fn screen_metrics_are_plausible() {
    let desktop = WindowsDesktop::new(WindowsDesktopConfig::default());
    let metrics = desktop.screen_metrics().expect("应能读取主显示器指标");

    assert!(metrics.width >= 640, "宽度异常：{}", metrics.width);
    assert!(metrics.height >= 480, "高度异常：{}", metrics.height);
    assert!(metrics.scale_factor > 0.0 && metrics.scale_factor <= 8.0);
}

#[test]
fn capture_region_returns_pixels_of_the_requested_size() {
    let region = Rect { x: 0, y: 0, width: 64, height: 48 };
    let frame = match winapi::capture_region(region) {
        Ok(frame) => frame,
        Err(err) => {
            eprintln!("跳过：当前会话没有可用的交互式桌面（{err}）");
            return;
        }
    };

    assert_eq!(frame.width, 64);
    assert_eq!(frame.height, 48);
    assert_eq!(frame.pixels.len(), 64 * 48 * 4, "应为 BGRA 每像素 4 字节");
    assert_eq!(frame.fingerprint.len(), 64);
    assert!(frame.pixels.iter().any(|byte| *byte != 0), "画面不应全黑");
}

#[test]
fn launching_without_a_configured_path_is_refused() {
    let desktop = WindowsDesktop::new(WindowsDesktopConfig::default());
    let err = desktop.launch_wecom().unwrap_err();
    assert!(
        matches!(err, AutomationError::NeedsHumanReview(_)),
        "未配置路径时必须拒绝启动，而不是猜路径：{err:?}"
    );
}

#[test]
fn launching_a_missing_executable_is_refused() {
    let config = WindowsDesktopConfig {
        wecom_exe: Some(std::path::PathBuf::from("C:/definitely/not/here/WXWork.exe")),
        ..Default::default()
    };
    let desktop = WindowsDesktop::new(config);
    let err = desktop.launch_wecom().unwrap_err();
    assert!(matches!(err, AutomationError::NeedsHumanReview(_)));
}

#[test]
fn launching_with_a_mismatched_hash_is_refused() {
    // 用本测试进程自己的可执行文件冒充"配置的企业微信"，但给出错误的哈希。
    let exe = std::env::current_exe().expect("应能取到当前可执行文件路径");
    let config = WindowsDesktopConfig {
        wecom_exe: Some(exe),
        wecom_exe_sha256: Some("00".repeat(32)),
        ..Default::default()
    };
    let desktop = WindowsDesktop::new(config);

    let err = desktop.launch_wecom().unwrap_err();
    assert!(
        matches!(err, AutomationError::NeedsHumanReview(_)),
        "哈希不匹配时必须拒绝启动：{err:?}"
    );
}

#[test]
fn hash_check_passes_for_the_real_file_hash() {
    // 注意：这里**绝不能**用 `std::env::current_exe()`。
    // 那样等于让测试二进制启动它自己，子进程会再跑一遍全部用例、
    // 再启动一次自己，形成递归；同时每个后代都挂在同一个控制台上，
    // 表现为 `cargo test --workspace` 长时间不返回、输出重复且混乱。
    //
    // 改用系统自带的 hostname.exe：它会立刻打印主机名并退出，
    // 既能拿到真实哈希，又不会留下任何常驻进程。
    let Some(exe) = system_executable("hostname.exe") else {
        eprintln!("跳过：找不到 hostname.exe");
        return;
    };
    let real = winapi::file_sha256(&exe).expect("应能计算文件哈希");
    assert_eq!(real.len(), 64);

    // 校验逻辑本身可用：把真实哈希写进配置后，只会在启动阶段失败，
    // 而不会在"校验可执行文件"这一步被拒绝。
    let config = WindowsDesktopConfig {
        wecom_exe: Some(exe),
        wecom_exe_sha256: Some(real),
        ..Default::default()
    };
    let desktop = WindowsDesktop::new(config);
    let err = desktop.launch_wecom();
    // 这里不关心能否真的启动，只要求不是"哈希不匹配"这一类拒绝。
    if let Err(error) = err {
        let text = error.to_string();
        assert!(!text.contains("哈希"), "不应因哈希被拒绝：{text}");
    }
}

/// 取一个一定会立刻退出的系统可执行文件。
fn system_executable(name: &str) -> Option<std::path::PathBuf> {
    let root = std::env::var_os("SystemRoot")?;
    let path = std::path::Path::new(&root).join("System32").join(name);
    path.is_file().then_some(path)
}

#[test]
fn waiting_for_the_clipboard_is_bounded_when_nobody_reads_it() {
    // 回归测试：曾经因为"发完 Ctrl+V 就立刻清空剪贴板"，
    // 导致目标程序还没来得及读就被清掉，粘贴变成空操作
    // （真机上表现为：光标已就位，但一个字都没进去）。
    //
    // 修复办法是等剪贴板被取用完毕再清空。这里验证等待逻辑本身：
    // 没有竞争者时要老实等到超时、返回 false，交给调用方退化处理，
    // 而且必须有界，不能无限等。
    let start = std::time::Instant::now();
    let observed = winapi::wait_for_clipboard_release(
        std::time::Duration::from_millis(10),
        std::time::Duration::from_millis(150),
    );
    let elapsed = start.elapsed();

    assert!(!observed, "没有其它进程读剪贴板时不应报告「被占用」");
    assert!(
        elapsed >= std::time::Duration::from_millis(120),
        "应至少等到接近超时，实际 {elapsed:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "等待必须有界，实际 {elapsed:?}"
    );
}

#[test]
fn clipboard_read_timeout_defaults_to_a_sane_value() {
    let config = WindowsDesktopConfig::default();
    assert!(config.clear_clipboard_after_paste, "默认应清空剪贴板，不留正文");
    assert!(
        config.clipboard_read_timeout >= std::time::Duration::from_millis(200),
        "等待目标程序读取剪贴板的时间不能太短"
    );
    assert!(
        config.clipboard_read_timeout <= std::time::Duration::from_secs(3),
        "也不能长到让一次发送明显卡顿"
    );
}

#[test]
fn input_is_refused_when_no_window_has_been_located() {
    let desktop = WindowsDesktop::new(WindowsDesktopConfig::for_title_prefix("不存在的窗口标题"));

    let click = desktop.guarded_click(
        automation_core::Point { x: 1, y: 1 },
        Rect { x: 0, y: 0, width: 100, height: 100 },
    );
    assert!(matches!(click, Err(AutomationError::ClientNotReady)));

    let paste = desktop.paste_text("不应被写入剪贴板", Rect { x: 0, y: 0, width: 100, height: 100 });
    assert!(matches!(paste, Err(AutomationError::ClientNotReady)));

    let send = desktop.send_message_shortcut(Rect { x: 0, y: 0, width: 100, height: 100 });
    assert!(matches!(send, Err(AutomationError::ClientNotReady)));
}

#[test]
fn capture_is_refused_before_a_window_is_located() {
    let desktop = WindowsDesktop::new(WindowsDesktopConfig::for_title_prefix("不存在的窗口标题"));
    let err = desktop
        .capture(Rect { x: 0, y: 0, width: 32, height: 32 })
        .unwrap_err();
    assert!(matches!(err, AutomationError::ClientNotReady));
}

#[test]
fn oversized_capture_is_refused() {
    let config = WindowsDesktopConfig {
        max_capture_pixels: 100,
        ..WindowsDesktopConfig::for_title_prefix("不存在的窗口标题")
    };
    let desktop = WindowsDesktop::new(config);
    let err = desktop
        .capture(Rect { x: 0, y: 0, width: 1000, height: 1000 })
        .unwrap_err();
    assert!(
        matches!(err, AutomationError::NeedsHumanReview(_)),
        "过大的捕获区域必须被拒绝：{err:?}"
    );
}

#[test]
fn finding_a_window_that_does_not_exist_reports_an_error() {
    let result = winapi::find_window_by_title_prefix("绝对不存在的窗口标题_7f3a9c");
    assert!(result.is_err());
}

/// 回归：`capture` 曾经会**自己把自己锁死**。
///
/// 原因是在 `match` 的受检表达式里写了 `self.target.lock()`——
/// Rust 会把受检表达式的临时值保留到整个 `match` 结束，
/// 于是分支里调用的 `current_target()` 再次锁同一把锁；
/// `std::sync::Mutex` 不可重入，进程就此不占 CPU、不报错、永不返回。
///
/// 这个缺陷只有真实窗口才会暴露：`MockDesktop` 不经过
/// `ensure_region_inside_window`，既有的 `oversized_capture_is_refused`
/// 又在更早的像素预算检查处就返回了。
///
/// 用带超时的子线程来测：真死锁时用例**失败**，而不是把整个测试进程挂住。
#[test]
fn capture_does_not_deadlock_on_its_own_lock() {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // `Progman` 是桌面窗口，任何交互式会话里都存在。
        let desktop = WindowsDesktop::new(WindowsDesktopConfig {
            window_matcher: WindowMatcher::ClassName("Progman".to_string()),
            ..WindowsDesktopConfig::default()
        });
        let outcome = match desktop.focus_wecom() {
            Ok(window) => desktop
                .capture(Rect { x: window.x + 4, y: window.y + 4, width: 32, height: 32 })
                .is_ok(),
            // 没有交互式桌面（例如无头环境）就跳过，不把环境问题当缺陷。
            Err(_) => true,
        };
        let _ = sender.send(outcome);
    });

    match receiver.recv_timeout(std::time::Duration::from_secs(15)) {
        Ok(true) => {}
        Ok(false) => panic!("capture 返回了失败"),
        Err(_) => panic!(
            "capture 15 秒内没有返回：极可能是 ensure_region_inside_window 自锁死\
             —— 不要在 match 的受检表达式里持锁，再去调用会再次加锁的方法"
        ),
    }
}
