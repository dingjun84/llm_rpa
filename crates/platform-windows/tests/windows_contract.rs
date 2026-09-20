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

/// 滚动与点击受同一套守卫保护：没定位到窗口就不许滚。
///
/// 滚轮事件送给**光标下**的窗口，滚错窗口会把别人的界面滚走——
/// 所以它和点击一样需要"前台窗口 == 目标窗口"这道闸。
#[test]
fn scrolling_is_refused_when_no_window_has_been_located() {
    let desktop = WindowsDesktop::new(WindowsDesktopConfig::for_title_prefix("不存在的窗口标题"));

    let err = desktop
        .scroll(
            automation_core::Point { x: 1, y: 1 },
            3,
            Rect { x: 0, y: 0, width: 100, height: 100 },
        )
        .unwrap_err();
    assert!(matches!(err, AutomationError::ClientNotReady));
}

/// 滚 0 格是合法的空操作，但同样要先有已定位的窗口——
/// 不能因为"反正什么都不做"就绕开守卫。
#[test]
fn scrolling_zero_notches_still_requires_a_located_window() {
    let desktop = WindowsDesktop::new(WindowsDesktopConfig::for_title_prefix("不存在的窗口标题"));

    let err = desktop
        .scroll(
            automation_core::Point { x: 1, y: 1 },
            0,
            Rect { x: 0, y: 0, width: 100, height: 100 },
        )
        .unwrap_err();
    assert!(matches!(err, AutomationError::ClientNotReady));
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

/// 回归：`preview` 的"只读"承诺必须成立——**不抢焦点**。
///
/// 区域标定用的是 `preview` 而不是 `focus_wecom`，理由正是
/// `SetForegroundWindow` 在 Windows 前台锁定策略下经常被拒绝
/// （调用方自身不在前台时），会让这个功能时灵时不灵。
///
/// 这个用例把"不抢焦点"变成可执行的断言：如果以后有人为了"让截图更好看"
/// 往 `preview` 里加一次置前，这里会立刻失败。
#[test]
fn preview_does_not_steal_the_foreground_window() {
    let before = winapi::foreground_window();

    // `Progman` 是桌面窗口，任何交互式会话里都存在，且正常不会是前台窗口。
    let desktop = WindowsDesktop::new(WindowsDesktopConfig {
        window_matcher: WindowMatcher::ClassName("Progman".to_string()),
        ..WindowsDesktopConfig::default()
    });

    let preview = match desktop.preview() {
        Ok(value) => value,
        Err(err) => {
            eprintln!("跳过：当前会话没有可用的交互式桌面（{err}）");
            return;
        }
    };

    let after = winapi::foreground_window();
    assert!(
        winapi::same_window(before, after),
        "preview 改变了前台窗口：标定预览绝不能抢焦点"
    );

    let (rect, shot) = preview;
    assert!(!rect.is_degenerate(), "窗口矩形不应退化：{rect:?}");
    assert_eq!(
        shot.width, rect.width as u32,
        "截图宽度应与窗口矩形一致"
    );
    assert_eq!(
        shot.height, rect.height as u32,
        "截图高度应与窗口矩形一致"
    );
    assert_eq!(
        shot.pixels.len(),
        rect.width as usize * rect.height as usize * 4,
        "应为 BGRA 每像素 4 字节"
    );
    assert_eq!(shot.fingerprint.len(), 64);
}

/// `preview` 是只读的：即使从没调用过 `focus_wecom`，也应该能独立定位并截图。
///
/// 这一点很重要——标定面板是用户打开应用后**第一件**要做的事，
/// 此时还没有任何任务跑过，`self.target` 还是空的。
#[test]
fn preview_works_without_a_prior_focus_call() {
    let desktop = WindowsDesktop::new(WindowsDesktopConfig {
        window_matcher: WindowMatcher::ClassName("Progman".to_string()),
        ..WindowsDesktopConfig::default()
    });

    match desktop.preview() {
        Ok((rect, shot)) => {
            assert!(!rect.is_degenerate());
            assert!(shot.pixels.iter().any(|byte| *byte != 0), "画面不应全黑");
        }
        Err(err) => eprintln!("跳过：当前会话没有可用的交互式桌面（{err}）"),
    }
}

/// 定位不到窗口时，`preview` 必须报错而不是返回一张空白图。
#[test]
fn preview_refuses_when_the_window_is_absent() {
    let desktop = WindowsDesktop::new(WindowsDesktopConfig {
        window_matcher: WindowMatcher::ClassName("绝对不存在的窗口类_7f3a9c".to_string()),
        ..WindowsDesktopConfig::default()
    });

    let err = desktop.preview().unwrap_err();
    assert!(
        matches!(err, AutomationError::ClientNotReady),
        "找不到窗口时必须报 ClientNotReady，不能拿空白图糊弄：{err:?}"
    );
}

/// 预览也要有资源上限：窗口矩形是外部数据，不能拿它直接分配内存。
#[test]
fn preview_is_refused_when_the_window_exceeds_the_preview_budget() {
    let config = WindowsDesktopConfig {
        window_matcher: WindowMatcher::ClassName("Progman".to_string()),
        preview_max_pixels: 100,
        ..WindowsDesktopConfig::default()
    };
    let desktop = WindowsDesktop::new(config);

    match desktop.preview() {
        Err(AutomationError::NeedsHumanReview(message)) => {
            assert!(
                message.contains("预览上限"),
                "应说明是被预览上限拒绝：{message}"
            );
        }
        Err(other) => eprintln!("跳过：当前会话没有可用的交互式桌面（{other}）"),
        Ok(_) => panic!("超过预览上限时必须拒绝，而不是照单全收"),
    }
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

// ── 「用鼠标指认目标窗口」的地基 ────────────────────────────────────────

#[test]
fn cursor_position_is_readable() {
    let (x, y) = winapi::cursor_position().expect("应能读取光标位置");
    // 多显示器下出现负坐标是合法的，所以不能断言 >= 0；
    // 但 ±32767 这种值说明读到的是未初始化的内存。
    assert!(x.abs() < 32_768 && y.abs() < 32_768, "光标坐标异常：({x},{y})");
}

#[test]
fn window_from_point_is_total() {
    // 屏外坐标：不能 panic，且应当没有窗口。
    assert!(
        winapi::window_from_point(-100_000, -100_000).is_none(),
        "屏外坐标不该返回窗口"
    );

    let (x, y) = winapi::cursor_position().expect("应能读取光标位置");
    let Some(hwnd) = winapi::window_from_point(x, y) else {
        // 光标停在桌面本体上，这是合法情况，跳过。
        return;
    };
    assert!(!hwnd.0.is_null());
    // 拿到的一定是**顶层**窗口：它必须有有效的边界。
    let rect = winapi::window_rect(hwnd).expect("顶层窗口应当有有效边界");
    assert!(rect.width > 0 && rect.height > 0, "窗口边界无效：{rect:?}");
}

#[test]
fn window_process_path_points_at_a_real_executable() {
    let foreground = winapi::foreground_window();
    if foreground.0.is_null() {
        return; // 无头环境：跳过，不把环境问题当缺陷。
    }
    let path = match winapi::window_process_path(foreground) {
        Ok(path) => path,
        // 权限受限时读不到进程路径，这也是环境问题，不是缺陷。
        Err(_) => return,
    };
    assert!(path.is_file(), "进程路径应当真实存在：{}", path.display());
    assert!(
        path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("exe")),
        "可执行文件应当以 .exe 结尾：{}",
        path.display()
    );
}
