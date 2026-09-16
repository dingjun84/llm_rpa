//! 真机诊断工具：定位窗口 → 截取画面 → 受保护地点击与粘贴。
//!
//! 这是给"实机验收条件 1–3"准备的排查工具：在没有企业微信、
//! 或者 OCR 还没接好的时候，先用它确认平台层本身是通的
//! （能不能找到窗口、能不能截到画面、点击与粘贴的坐标对不对）。
//!
//! 用法（在仓库根目录）：
//!
//! ```text
//! cargo run -p platform-windows --example screen_probe -- list
//! cargo run -p platform-windows --example screen_probe -- capture "WorkBuddy AI" shot.png
//! cargo run -p platform-windows --example screen_probe -- click   "WorkBuddy AI" 400 620
//! cargo run -p platform-windows --example screen_probe -- paste   "WorkBuddy AI" "要输入的文字"
//! cargo run -p platform-windows --example screen_probe -- shot    "WorkBuddy AI" shot.png 400 620 600 40
//! ```
//!
//! 坐标一律是**窗口内相对坐标**（左上角为原点），与 `RunnerConfig`
//! 里的相对区域是同一套语义。
//!
//! 安全约定：
//!
//! - `click` / `paste` 在动作前会重新确认目标窗口是前台窗口，不是就拒绝；
//! - `paste` 只做"点进输入框 + Ctrl+V"，**不会按回车**，因此不会真的发出去；
//! - 所有子命令都只影响本机可见的交互式桌面，不注入、不 Hook、不联网。

#![cfg(windows)]

use std::time::SystemTime;

use automation_core::{DesktopPlatform, Point, Rect, Screenshot};
use platform_windows::{winapi, WindowsDesktop, WindowsDesktopConfig};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first().map(String::as_str) else {
        usage();
        std::process::exit(2);
    };

    let result = match command {
        "list" => list_windows(),
        "capture" => capture_window(&args),
        "shot" => capture_region_of_window(&args),
        "click" => click(&args),
        "paste" => paste(&args),
        "type" => type_into(&args),
        "clear" => clear_input(&args),
        "-h" | "--help" | "help" => {
            usage();
            return;
        }
        other => Err(format!("未知子命令：{other}")),
    };

    if let Err(err) = result {
        eprintln!("失败：{err}");
        std::process::exit(1);
    }
}

fn usage() {
    println!(
        "\
screen_probe —— 真机诊断工具（坐标均为窗口内相对坐标）

  list                                    列出所有可见窗口
  capture <标题前缀> <输出.png>            截取整个窗口
  shot <标题前缀> <输出.png> <x> <y> <w> <h>  截取窗口内的一个区域
  click <标题前缀> <x> <y>                 受保护地点击窗口内某点
  paste <标题前缀> <文字>                  受保护地粘贴文字（不按回车）
  type <标题前缀> <x> <y> <文字>           聚焦到某点后粘贴文字（单进程完成，推荐）
  clear <标题前缀> <x> <y>                清空该点所在的输入控件（Ctrl+A + Delete）
"
    );
}

/// 定位窗口并返回它在屏幕上的边界。只读，不改变前台窗口。
fn locate(prefix: &str) -> Result<Rect, String> {
    let hwnd = winapi::find_window_by_title_prefix(prefix)?;
    let rect = winapi::window_rect(hwnd)?;
    if rect.is_degenerate() {
        return Err(format!("窗口「{prefix}」当前尺寸无效（可能已最小化）"));
    }
    Ok(rect)
}

fn list_windows() -> Result<(), String> {
    let windows = winapi::list_visible_windows();
    if windows.is_empty() {
        println!("没有找到任何可见且有标题的窗口。");
        return Ok(());
    }
    println!("{:<10} {:<28} {:<22} {}", "HWND", "类名", "位置/尺寸", "标题");
    println!("{}", "-".repeat(110));
    for info in windows {
        let marker = if info.is_foreground { " [前台]" } else { "" };
        let minimized = if info.is_minimized { " [最小化]" } else { "" };
        println!(
            "{:<10} {:<28} {:<22} {}{}{}",
            format!("{:#x}", info.hwnd),
            info.class_name,
            format!(
                "({},{}) {}x{}",
                info.rect.x, info.rect.y, info.rect.width, info.rect.height
            ),
            info.title,
            marker,
            minimized
        );
    }
    Ok(())
}

fn capture_window(args: &[String]) -> Result<(), String> {
    let prefix = arg(args, 1, "标题前缀")?;
    let out = arg(args, 2, "输出 PNG 路径")?;
    let rect = locate(&prefix)?;
    save(rect, &out)
}

fn capture_region_of_window(args: &[String]) -> Result<(), String> {
    let prefix = arg(args, 1, "标题前缀")?;
    let out = arg(args, 2, "输出 PNG 路径")?;
    let window = locate(&prefix)?;
    let region = Rect {
        x: window.x + number(args, 3, "x")?,
        y: window.y + number(args, 4, "y")?,
        width: number(args, 5, "宽度")?,
        height: number(args, 6, "高度")?,
    };
    save(region, &out)
}

/// 捕获并落盘为 PNG。
fn save(region: Rect, out: &str) -> Result<(), String> {
    let frame = winapi::capture_region(region)?;
    let shot = Screenshot {
        pixels: frame.pixels,
        width: frame.width,
        height: frame.height,
        captured_at: SystemTime::now(),
        fingerprint: frame.fingerprint,
    };
    let rgba = vision::to_rgba(&shot).map_err(|err| err.to_string())?;
    let png = vision::encode_png(&rgba).map_err(|err| err.to_string())?;
    std::fs::write(out, &png).map_err(|err| format!("写入 {out} 失败：{err}"))?;
    println!(
        "已保存 {out}（{}x{}，{} 字节，指纹 {}）",
        shot.width,
        shot.height,
        png.len(),
        &shot.fingerprint[..16]
    );
    Ok(())
}

fn click(args: &[String]) -> Result<(), String> {
    let prefix = arg(args, 1, "标题前缀")?;
    let desktop = WindowsDesktop::new(WindowsDesktopConfig::for_title_prefix(prefix.clone()));
    // 与真实工作流一致：先定位并置于前台，拿到标定用的窗口边界。
    // `guarded_click` 内部会重新校验"前台窗口 == 目标窗口且边界未变"。
    let window = desktop.focus_wecom().map_err(|err| err.to_string())?;
    let target = Point {
        x: window.x + number(args, 2, "x")?,
        y: window.y + number(args, 3, "y")?,
    };
    desktop.guarded_click(target, window).map_err(|err| err.to_string())?;
    println!(
        "已点击窗口内 ({}, {}) → 屏幕 ({}, {})",
        target.x - window.x,
        target.y - window.y,
        target.x,
        target.y
    );
    Ok(())
}

fn paste(args: &[String]) -> Result<(), String> {
    let prefix = arg(args, 1, "标题前缀")?;
    let text = arg(args, 2, "要粘贴的文字")?;
    let desktop = WindowsDesktop::new(WindowsDesktopConfig::for_title_prefix(prefix.clone()));
    let window = desktop.focus_wecom().map_err(|err| err.to_string())?;
    desktop.paste_text(&text, window).map_err(|err| err.to_string())?;
    println!(
        "已把 {} 个字符粘贴到「{prefix}」的当前焦点控件（未按回车）",
        text.chars().count()
    );
    Ok(())
}

/// 聚焦到窗口内某点后粘贴文字。
///
/// **必须在一个进程里完成**：点击与粘贴分成两次调用时，中间只要有任何
/// 窗口抢走前台焦点（例如本工具自己的控制台），目标窗口内的输入焦点就会丢失，
/// 后续的 Ctrl+V 会落到别处。真实工作流也是"聚焦 → 点击 → 粘贴"一口气做完的。
fn type_into(args: &[String]) -> Result<(), String> {
    let prefix = arg(args, 1, "标题前缀")?;
    let x = number(args, 2, "x")?;
    let y = number(args, 3, "y")?;
    let text = arg(args, 4, "要输入的文字")?;
    // 诊断用：保留剪贴板内容，便于区分"没设置成功"和"清得太早"。
    let keep_clipboard = args.get(5).map(String::as_str) == Some("keep");

    let mut config = WindowsDesktopConfig::for_title_prefix(prefix.clone());
    if keep_clipboard {
        config.clear_clipboard_after_paste = false;
    }
    let desktop = WindowsDesktop::new(config);
    let window = desktop.focus_wecom().map_err(|err| err.to_string())?;
    let target = Point { x: window.x + x, y: window.y + y };
    desktop
        .guarded_click(target, window)
        .map_err(|err| err.to_string())?;

    // 等输入框真正拿到键盘焦点；这一步不能省。
    std::thread::sleep(std::time::Duration::from_millis(300));

    desktop
        .paste_text(&text, window)
        .map_err(|err| err.to_string())?;
    println!(
        "已在窗口内 ({x}, {y}) 聚焦并粘贴 {} 个字符（未按回车，剪贴板{}）",
        text.chars().count(),
        if keep_clipboard { "保留" } else { "已清空" }
    );
    Ok(())
}

/// 清空某个输入控件：聚焦 → Ctrl+A → Delete。
///
/// 只用于手工验证时清理测试输入框；生产端口不暴露任意按键序列。
fn clear_input(args: &[String]) -> Result<(), String> {
    let prefix = arg(args, 1, "标题前缀")?;
    let x = number(args, 2, "x")?;
    let y = number(args, 3, "y")?;

    let desktop = WindowsDesktop::new(WindowsDesktopConfig::for_title_prefix(prefix.clone()));
    let window = desktop.focus_wecom().map_err(|err| err.to_string())?;
    let target = Point { x: window.x + x, y: window.y + y };
    desktop
        .guarded_click(target, window)
        .map_err(|err| err.to_string())?;
    std::thread::sleep(std::time::Duration::from_millis(250));

    winapi::send_ctrl_a()?;
    std::thread::sleep(std::time::Duration::from_millis(120));
    winapi::send_delete()?;
    println!("已清空窗口内 ({x}, {y}) 处的输入控件");
    Ok(())
}

fn arg(args: &[String], index: usize, name: &str) -> Result<String, String> {    args.get(index)
        .cloned()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("缺少参数：{name}"))
}

fn number(args: &[String], index: usize, name: &str) -> Result<i32, String> {
    arg(args, index, name)?
        .parse::<i32>()
        .map_err(|_| format!("参数 {name} 必须是整数"))
}
