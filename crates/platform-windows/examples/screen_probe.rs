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

use std::time::{Duration, Instant, SystemTime};

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
        "pick" => pick(&args),
        "focus" => focus_window(&args),
        "annotate" => annotate(&args),
        "capture" => capture_window(&args),
        "shot" => capture_region_of_window(&args),
        "printshot" => print_shot(&args),
        "template" => cut_icon_template(&args),
        "findicon" => find_icon(&args),
        "click" => click(&args),
        "scroll" => scroll(&args),
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
  pick [秒数]                             打印光标下的窗口（类名/标题/exe）；给秒数则持续采样
  focus <窗口类名> [exe路径]                只做「接管窗口」：定位 + 带到前台，然后报结果
  annotate <标题前缀> <输出.png> [标定.json]  截图上画出四个区域 + 10% 网格，供人工确认
  capture <标题前缀> <输出.png>            截取整个窗口
  shot <标题前缀> <输出.png> <x> <y> <w> <h>  截取窗口内的一个区域
  printshot <标题前缀> <输出.png>          用 PrintWindow 抓窗口自身画面（能抓到 WebView2 内容）
  template <标题前缀> <输出.png> <x> <y> <w> <h>
                                        从窗口里裁出一块**图标模板**存成 PNG（供「先点击导航图标跳转」用）
                                        并打印可直接粘进配置的路径
  findicon <标题前缀> <模板.png> [最低分] [x y w h]
                                        在当前画面上按模板匹配找图标，报出**分数与位置**
                                        搜索区默认用 core 的 DEFAULT_NAV_STRIP；给 x y w h 可临时改
                                        这是标定「模板对不对、阈值定多少」的唯一手段
  click <标题前缀> <x> <y>                 受保护地点击窗口内某点
  scroll <标题前缀> <x> <y> <格数>         把光标移到某点后滚动滚轮（正数向下、负数向上）
                                        并打印光标前后位置，用来确认「鼠标真的移过去了」
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

/// 描述光标下那个顶层窗口的完整身份。
///
/// 这是「让操作者用鼠标指认目标窗口」的只读版本：**不需要点击**，
/// 把鼠标停在目标窗口上就够了。
///
/// 为什么刻意不要求点击：点击可能在目标程序里产生副作用
/// （在会话列表上点一下就把会话打开了、在别处点一下就把草稿框丢了）。
/// 悬停 + 读取没有任何副作用。
fn describe_window_under_cursor() -> Option<String> {
    let (x, y) = winapi::cursor_position().ok()?;
    let hwnd = winapi::window_from_point(x, y)?;
    let rect = winapi::window_rect(hwnd).ok()?;
    let exe = winapi::window_process_path(hwnd)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|err| format!("<读取失败：{err}>"));
    Some(format!(
        "hwnd={:#x}\n    类名    {}\n    标题    {}\n    位置    ({},{}) {}x{}\n    所属 exe {}",
        hwnd.0 as isize,
        winapi::window_class_name(hwnd),
        winapi::window_title(hwnd),
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        exe
    ))
}

fn pick(args: &[String]) -> Result<(), String> {
    let seconds: u64 = match args.get(1) {
        Some(value) => value
            .parse()
            .map_err(|_| "秒数必须是整数".to_string())?,
        None => 0,
    };

    if seconds == 0 {
        return match describe_window_under_cursor() {
            Some(described) => {
                println!("{described}");
                Ok(())
            }
            None => Err("光标下没有窗口（可能停在桌面本体上）".to_string()),
        };
    }

    println!("采样 {seconds} 秒：把鼠标移到目标窗口上**停住**，不用点击。");
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut seen: Vec<String> = Vec::new();
    while Instant::now() < deadline {
        if let Some(described) = describe_window_under_cursor() {
            if !seen.contains(&described) {
                seen.push(described.clone());
                println!("\n{described}");
            }
        }
        std::thread::sleep(Duration::from_millis(120));
    }
    if seen.is_empty() {
        println!("\n这 {seconds} 秒内没有采到任何窗口。");
    } else {
        println!("\n共采到 {} 个不同的窗口。", seen.len());
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
// ── 区域标注：让操作者肉眼确认标定 ──────────────────────────────────────

/// 四个区域的标注颜色，与 [`REGION_NAMES`] 一一对应。
///
/// 刻意挑在浅色和深色界面上都看得清的饱和色。顺序固定，方便对着控制台读图。
/// **与界面 `RegionCalibration.tsx::REGIONS` 的 `color` 保持一致**。
const REGION_COLORS: [[u8; 3]; 4] = [
    [226, 75, 74],
    [55, 138, 221],
    [99, 153, 34],
    [239, 159, 39],
];

/// 四个区域的名称，与 [`REGION_COLORS`] 一一对应。
///
/// **必须与界面 `RegionCalibration.tsx::REGIONS` 的编号和名称一致**：
/// 两边都会把编号画出来，操作者用「把 1 的左边界挪到 32%」这种话沟通。
const REGION_NAMES: [&str; 4] = ["联系人候选区", "聊天页标题区", "聊天正文区", "消息输入框区"];

/// 与 `RuntimeConfig::default().regions` 保持一致。
///
/// 现在**直接从 `automation_core::DEFAULT_REGIONS` 展开**，不再手抄一份：
/// 探针与产品路径共用同一组常量，画出来的框必然就是任务真正会裁的框。
/// 此前这里是手抄的字面量，两边不一致时探针会把人引到错的方向
/// （「框明明画对了，任务却说识别不到」）。
const DEFAULT_REGIONS: [[f32; 4]; 4] = [
    flatten(automation_core::DEFAULT_REGIONS[0]),
    flatten(automation_core::DEFAULT_REGIONS[1]),
    flatten(automation_core::DEFAULT_REGIONS[2]),
    flatten(automation_core::DEFAULT_REGIONS[3]),
];

/// 把 `RelativeRegion` 摊平成探针内部用的 `[x, y, w, h]`。
///
/// 写成 `const fn` 而不是闭包：`const` 初始化式里不允许调用闭包。
const fn flatten(region: automation_core::RelativeRegion) -> [f32; 4] {
    [region.x, region.y, region.width, region.height]
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct RegionsFile {
    regions: Option<RegionsBlock>,
}

/// 每个字段都可选：手写的标定文件往往只想改其中一两个区域。
#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct RegionsBlock {
    contact_panel: Option<[f32; 4]>,
    chat_header: Option<[f32; 4]>,
    chat_body: Option<[f32; 4]>,
    composer: Option<[f32; 4]>,
}

type Canvas = image::RgbaImage;

/// 5x7 点阵数字，低位在右（`bit 4` 是最左那一列）。
///
/// 自带点阵而不是引字体文件：探针要能随手拷到别的机器上跑，
/// 不该依赖外部字体资源。
const DIGITS: [[u8; 7]; 10] = [
    [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
    [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
    [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
    [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110],
    [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
    [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
    [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
    [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
    [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
    [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
];

/// 按 alpha 把颜色混进某个像素。越界坐标静默忽略 —— 标注框经常贴边。
fn blend(canvas: &mut Canvas, x: i32, y: i32, color: [u8; 3], alpha: f32) {
    if x < 0 || y < 0 || x as u32 >= canvas.width() || y as u32 >= canvas.height() {
        return;
    }
    let pixel = canvas.get_pixel_mut(x as u32, y as u32);
    let weight = alpha.clamp(0.0, 1.0);
    for channel in 0..3 {
        let base = pixel.0[channel] as f32;
        pixel.0[channel] = (base * (1.0 - weight) + color[channel] as f32 * weight).round() as u8;
    }
}

fn fill_rect(canvas: &mut Canvas, x: i32, y: i32, w: i32, h: i32, color: [u8; 3], alpha: f32) {
    for py in y..y + h {
        for px in x..x + w {
            blend(canvas, px, py, color, alpha);
        }
    }
}

fn stroke_rect(
    canvas: &mut Canvas,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    thickness: i32,
    color: [u8; 3],
    alpha: f32,
) {
    for offset in 0..thickness {
        for px in x..x + w {
            blend(canvas, px, y + offset, color, alpha);
            blend(canvas, px, y + h - 1 - offset, color, alpha);
        }
        for py in y..y + h {
            blend(canvas, x + offset, py, color, alpha);
            blend(canvas, x + w - 1 - offset, py, color, alpha);
        }
    }
}

fn draw_digit(canvas: &mut Canvas, digit: usize, x: i32, y: i32, scale: i32, color: [u8; 3]) {
    for (row, bits) in DIGITS[digit % 10].iter().enumerate() {
        for col in 0..5 {
            if bits & (1 << (4 - col)) != 0 {
                fill_rect(
                    canvas,
                    x + col * scale,
                    y + row as i32 * scale,
                    scale,
                    scale,
                    color,
                    1.0,
                );
            }
        }
    }
}

/// 每 10% 画一条细线，每 50% 画一条粗一点的。
///
/// 目的是让操作者能用**百分比**回话（"左边界再往右挪到 30%"），
/// 而不是"再往右一点点"这种没法直接执行的描述。
fn draw_percent_grid(canvas: &mut Canvas) {
    let width = canvas.width() as i32;
    let height = canvas.height() as i32;
    for step in 1..10 {
        let major = step % 5 == 0;
        let alpha = if major { 0.5 } else { 0.22 };
        let thickness = if major { 2 } else { 1 };
        fill_rect(canvas, width * step / 10, 0, thickness, height, [128, 128, 128], alpha);
        fill_rect(canvas, 0, height * step / 10, width, thickness, [128, 128, 128], alpha);
    }
}

fn regions_from_file(path: &str) -> Result<[[f32; 4]; 4], String> {
    let raw = std::fs::read_to_string(path).map_err(|err| format!("读不到标定文件 {path}：{err}"))?;
    let parsed: RegionsFile =
        serde_json::from_str(&raw).map_err(|err| format!("标定文件 {path} 解析失败：{err}"))?;
    let block = parsed.regions.unwrap_or_default();
    Ok([
        block.contact_panel.unwrap_or(DEFAULT_REGIONS[0]),
        block.chat_header.unwrap_or(DEFAULT_REGIONS[1]),
        block.chat_body.unwrap_or(DEFAULT_REGIONS[2]),
        block.composer.unwrap_or(DEFAULT_REGIONS[3]),
    ])
}

fn annotate(args: &[String]) -> Result<(), String> {
    let prefix = arg(args, 1, "标题前缀")?;
    let out = arg(args, 2, "输出 PNG 路径")?;
    let regions = match args.get(3) {
        Some(path) => regions_from_file(path)?,
        None => DEFAULT_REGIONS,
    };

    let rect = locate(&prefix)?;
    let frame = winapi::capture_region(rect)?;
    let width = frame.width;
    let height = frame.height;
    let shot = Screenshot {
        pixels: frame.pixels,
        width,
        height,
        captured_at: SystemTime::now(),
        fingerprint: frame.fingerprint,
    };
    let mut canvas = vision::to_rgba(&shot).map_err(|err| err.to_string())?;

    draw_percent_grid(&mut canvas);

    // 线宽与角标大小随图像尺寸缩放，而不是写死像素值：
    // 在 4K 屏上固定 3px 边框会细得看不见，在 1024 宽的窗口上固定 30px 角标又会盖住内容。
    let thickness = (width / 700).clamp(2, 6) as i32;
    let scale = (width / 350).clamp(3, 8) as i32;

    let pixels_of = |region: &[f32; 4]| -> (i32, i32, i32, i32) {
        (
            (region[0] * width as f32).round() as i32,
            (region[1] * height as f32).round() as i32,
            (region[2] * width as f32).round() as i32,
            (region[3] * height as f32).round() as i32,
        )
    };

    for (index, region) in regions.iter().enumerate() {
        let color = REGION_COLORS[index];
        let (x, y, w, h) = pixels_of(region);
        fill_rect(&mut canvas, x, y, w, h, color, 0.12);
        stroke_rect(&mut canvas, x, y, w, h, thickness, color, 0.95);
        // 数字角标贴在区域左上角内侧；先垫一块深色底，保证在任何底色上都读得出来。
        let (bx, by) = (x + 2 * thickness, y + 2 * thickness);
        let pad = thickness;
        fill_rect(
            &mut canvas,
            bx - pad,
            by - pad,
            5 * scale + 2 * pad,
            7 * scale + 2 * pad,
            [0, 0, 0],
            0.6,
        );
        draw_digit(&mut canvas, index + 1, bx, by, scale, [255, 255, 255]);
    }

    let png = vision::encode_png(&canvas).map_err(|err| err.to_string())?;
    std::fs::write(&out, &png).map_err(|err| format!("写入 {out} 失败：{err}"))?;

    println!("已保存 {out}（{width}x{height}，{} 字节）", png.len());
    println!(
        "窗口「{prefix}」@ ({},{}) {}x{}",
        rect.x, rect.y, rect.width, rect.height
    );
    println!("\n图上框角数字 → 区域（比例是相对窗口的）：");
    for (index, region) in regions.iter().enumerate() {
        let (x, y, w, h) = pixels_of(region);
        println!(
            "  {}  {:<6} x={:.2} y={:.2} w={:.2} h={:.2}   → 像素 ({x},{y}) {w}x{h}",
            index + 1,
            REGION_NAMES[index],
            region[0],
            region[1],
            region[2],
            region[3],
        );
    }
    println!("\n请核对这四个框是否都框对了；要挪就按百分比说，例如「1 的左边界挪到 0.32」。");
    Ok(())
}

fn save(region: Rect, out: &str) -> Result<(), String> {
    save_frame(winapi::capture_region(region)?, out)
}

/// 用 `PrintWindow` 抓整个窗口，而不是屏幕 DC 的 `BitBlt`。
///
/// 用途单一但关键：**验证 WebView2 / Chromium 这类窗口到底画了什么**。
/// `capture` / `shot` 走 BitBlt，对这类用 GPU 合成的内容常常只能拿到一片白或一片黑，
/// 那会让人误判成「页面没渲染」。`printshot` 直接向窗口要画面，能分清两者。
fn print_shot(args: &[String]) -> Result<(), String> {
    let prefix = arg(args, 1, "标题前缀")?;
    let out = arg(args, 2, "输出 PNG 路径")?;
    let hwnd = winapi::find_window_by_title_prefix(&prefix)?;
    save_frame(winapi::print_window(hwnd)?, &out)
}

fn save_frame(frame: winapi::CapturedFrame, out: &str) -> Result<(), String> {
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

// ── 图标模板：裁一张、再在当前画面上试匹配 ──────────────────────────────

/// 从窗口里裁出一块**图标模板**存成 PNG。
///
/// ## 为什么要有这个子命令
///
/// 「先点导航图标切视图」这一步靠模板匹配，而**模板必须由人来截**：
/// 程序自动裁一块"看起来像图标"的区域，会把"点错了地方"变成一次看起来
/// 完全正常的运行——匹配分数照样很高，因为它匹配的就是它自己刚裁的那块。
/// 人截的话，"我圈的是哪个图标"这件事在截图那一刻就被确认了。
///
/// 尺寸在这里就卡住（[`vision::MIN_TEMPLATE_SIDE`]–[`vision::MAX_TEMPLATE_SIDE`]）：
/// 太小多半是手抖截歪了，太大说明圈进了整块界面。留到任务里才发作的话，
/// 症状是"分数很低"，而人只会去怀疑阈值。
fn cut_icon_template(args: &[String]) -> Result<(), String> {
    let prefix = arg(args, 1, "标题前缀")?;
    let out = arg(args, 2, "输出 PNG 路径")?;
    let window = locate(&prefix)?;
    let region = Rect {
        x: number(args, 3, "x")?,
        y: number(args, 4, "y")?,
        width: number(args, 5, "宽度")?,
        height: number(args, 6, "高度")?,
    };

    if region.width < vision::MIN_TEMPLATE_SIDE as i32
        || region.height < vision::MIN_TEMPLATE_SIDE as i32
    {
        return Err(format!(
            "圈得太小（{}x{}）：模板每边至少要 {} 像素，太小多半是截歪了",
            region.width,
            region.height,
            vision::MIN_TEMPLATE_SIDE
        ));
    }
    if region.width > vision::MAX_TEMPLATE_SIDE as i32
        || region.height > vision::MAX_TEMPLATE_SIDE as i32
    {
        return Err(format!(
            "圈得太大（{}x{}）：模板每边最多 {} 像素，再大就不是图标而是整块界面了",
            region.width,
            region.height,
            vision::MAX_TEMPLATE_SIDE
        ));
    }

    let frame = winapi::capture_region(Rect {
        x: window.x + region.x,
        y: window.y + region.y,
        width: region.width,
        height: region.height,
    })?;
    let shot = Screenshot {
        pixels: frame.pixels,
        width: frame.width,
        height: frame.height,
        captured_at: SystemTime::now(),
        fingerprint: frame.fingerprint,
    };
    // 用 `crop_template` 走一遍：这样存下来的像素与任务里裁出来的表示完全一致
    // （BGRA 顺序、行优先），不会出现"探针看着对、任务里偏色"这种事。
    let template = vision::crop_template(&shot, Rect { x: 0, y: 0, width: region.width, height: region.height }, "探针")
        .map_err(|err| err.to_string())?;
    let rgba = vision::to_rgba(&Screenshot {
        pixels: template.pixels,
        width: template.width,
        height: template.height,
        captured_at: SystemTime::now(),
        fingerprint: String::new(),
    })
    .map_err(|err| err.to_string())?;
    let png = vision::encode_png(&rgba).map_err(|err| err.to_string())?;
    std::fs::write(&out, &png).map_err(|err| format!("写入 {out} 失败：{err}"))?;

    println!(
        "已保存模板 {out}（{}x{}，{} 字节）",
        template.width,
        template.height,
        png.len()
    );
    println!();
    println!("下一步：把它填进配置，或直接用 findicon 量一次分数——");
    println!("  screen_probe findicon \"{prefix}\" \"{out}\"");
    println!();
    println!("⚠️ 一个图标在**选中 / 未选中**两种状态下长得不一样。");
    println!("   如果点完停在这个页面上，下次可能就匹配不上了——");
    println!("   建议把两种状态各截一张，都填进配置。");
    Ok(())
}

/// 在当前画面上按模板匹配找图标，报出**分数与位置**。
///
/// 这是标定「模板截得对不对、阈值该定多少」的唯一手段：
/// 分数是量出来的，不是猜出来的。模板可以给多张（逗号分隔），
/// 会逐张报分数——这正是分辨"该用哪张"的办法。
///
/// **纯只读**：只截屏，不点击、不聚焦、不产生任何输入。
/// 位置换算成窗口内相对坐标报出来，与配置里那套语义一致。
fn find_icon(args: &[String]) -> Result<(), String> {
    let prefix = arg(args, 1, "标题前缀")?;
    let templates_arg = arg(args, 2, "模板 PNG 路径（可逗号分隔多张）")?;
    let window = locate(&prefix)?;

    // 搜索区：默认用 core 的默认值，这样探针量到的范围就是任务真正会找的范围。
    let strip = match args.len() {
        3 | 4 => automation_core::DEFAULT_NAV_STRIP,
        _ => automation_core::RelativeRegion::new(
            number(args, 4, "搜索区 x")? as f32,
            number(args, 5, "搜索区 y")? as f32,
            number(args, 6, "搜索区 宽度")? as f32,
            number(args, 7, "搜索区 高度")? as f32,
        ),
    };
    let min_score: f32 = match args.get(3) {
        Some(raw) => raw.parse().map_err(|_| "最低分必须是数字（0–1）".to_string())?,
        None => automation_core::DEFAULT_NAV_ICON_MIN_SCORE,
    };
    if !(0.0..=1.0).contains(&min_score) {
        return Err(format!("最低分必须在 0–1 之间（当前 {min_score}）"));
    }

    let strip_screen = strip
        .resolve_within(window)
        .map_err(|err| format!("搜索区比例不合法或越出窗口：{err}"))?;
    let strip_in_window = Rect {
        x: strip_screen.x - window.x,
        y: strip_screen.y - window.y,
        width: strip_screen.width,
        height: strip_screen.height,
    };

    let paths: Vec<&str> = templates_arg
        .split(',')
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .collect();
    if paths.is_empty() {
        return Err("至少要给一张模板 PNG".into());
    }

    let shot = winapi::capture_region(strip_screen)?;
    let frame = Screenshot {
        pixels: shot.pixels,
        width: shot.width,
        height: shot.height,
        captured_at: SystemTime::now(),
        fingerprint: shot.fingerprint,
    };

    println!(
        "窗口「{prefix}」 {}x{} @ ({}, {})",
        window.width, window.height, window.x, window.y
    );
    println!(
        "搜索区（窗口内相对）：({}, {}) {}x{}    最低分 {min_score:.3}",
        strip_in_window.x, strip_in_window.y, strip_in_window.width, strip_in_window.height
    );
    println!("画面：{}x{}", frame.width, frame.height);
    println!();

    let mut best: Option<(f32, Rect, String)> = None;
    for path in &paths {
        let template = match vision::load_icon_template(std::path::Path::new(path), *path) {
            Ok(template) => template,
            Err(err) => {
                println!("  {path}");
                println!("    无法载入：{err}");
                continue;
            }
        };
        match vision::match_template(&frame, &template) {
            Ok(Some((bounds, score))) => {
                let accepted = if score >= min_score { "通过" } else { "不足" };
                println!("  {path}  ({}x{})", template.width, template.height);
                println!(
                    "    最高分 {score:.4}  [{accepted}]    \
                     搜索区内 ({}, {})    窗口内 ({}, {})    点击点 窗口内 ({}, {})",
                    bounds.x,
                    bounds.y,
                    strip_in_window.x + bounds.x,
                    strip_in_window.y + bounds.y,
                    strip_in_window.x + bounds.x + bounds.width / 2,
                    strip_in_window.y + bounds.y + bounds.height / 2
                );
                if best.as_ref().map(|(current, _, _)| score > *current).unwrap_or(true) {
                    best = Some((score, bounds, (*path).to_string()));
                }
            }
            Ok(None) => {
                println!("  {path}");
                println!("    没有结果：模板放不进搜索区，或者模板是纯色的（没有图案可匹配）");
            }
            Err(err) => {
                println!("  {path}");
                println!("    匹配失败：{err}");
            }
        }
    }

    println!();
    match best {
        Some((score, _, path)) if score >= min_score => {
            println!("结论：模板「{path}」最高 {score:.3}，达到最低分 {min_score:.3}。");
            println!("     可以用它跑任务了。");
        }
        Some((score, _, path)) => {
            println!("结论：最高只有 {score:.3}（模板「{path}」），低于最低分 {min_score:.3}。");
            println!("     先看上面的位置对不对——那是它认为最像的地方。");
            println!("     位置不对 ⇒ 模板截错了，或搜索区没盖住图标；");
            println!("     位置对但分数低 ⇒ 图标有缩放/主题差异，换个状态再截一张。");
        }
        None => println!("结论：所有模板都没能给出结果，先检查模板文件与搜索区。"),
    }
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

/// 把光标移到窗口内某点后滚动滚轮。
///
/// 滚轮事件只会送给**光标下**的窗口，所以必须先移动光标；
/// 与 `click` 一样会先确认目标窗口在前台，避免把别的界面滚走。
fn scroll(args: &[String]) -> Result<(), String> {
    let prefix = arg(args, 1, "标题前缀")?;
    let x = number(args, 2, "x")?;
    let y = number(args, 3, "y")?;
    let notches = number(args, 4, "滚动格数")?;
    if notches == 0 {
        return Err("滚动格数不能为 0".to_string());
    }

    // 先记下光标**原本**在哪：这是回答「鼠标到底有没有被挪到滚动区域」的关键。
    // 只打印结果的话，操作者只能凭肉眼盯着屏幕，而 `SetCursorPos` 是一帧内瞬移，
    // 很容易看漏；两个坐标一对比，动没动就是白纸黑字。
    let before = winapi::cursor_position().ok();
    match before {
        Some((bx, by)) => println!("光标起点：屏幕 ({bx}, {by})"),
        None => println!("光标起点：（读不到，后面无法对比）"),
    }

    let desktop = WindowsDesktop::new(WindowsDesktopConfig::for_title_prefix(prefix.clone()));
    let window = desktop.focus_wecom().map_err(|err| err.to_string())?;
    let at = Point { x: window.x + x, y: window.y + y };
    // 不提前 `?`：光标核对要**在失败时也打印**——移不过去正是最需要看到坐标的时候。
    let result = desktop.scroll(at, notches, window);
    match winapi::cursor_position() {
        Ok((ax, ay)) => println!("光标现在：屏幕 ({ax}, {ay})   要求落在 ({}, {})", at.x, at.y),
        Err(_) => println!("光标现在：（读不到）"),
    }
    result.map_err(|err| err.to_string())?;

    println!(
        "已在窗口内 ({x}, {y}) 向{}滚动 {} 格",
        if notches > 0 { "下" } else { "上" },
        notches.abs()
    );
    Ok(())
}

/// 只做「接管窗口」这一步：定位 + 带到前台，然后打印结果。
///
/// **不点击、不输入、不发送**，只改变前台窗口——与任务开始时做的事情完全一致。
/// 任务里接管失败时用这条命令单独验证，就不用去猜到底是
/// 「窗口没找到」还是「找到了但置前被拒」。
fn focus_window(args: &[String]) -> Result<(), String> {
    let class = arg(args, 1, "窗口类名")?;
    let exe = args
        .get(2)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty());

    let mut config = WindowsDesktopConfig {
        window_matcher: platform_windows::WindowMatcher::ClassName(class.clone()),
        ..WindowsDesktopConfig::default()
    };
    if let Some(exe) = exe {
        config.wecom_exe = Some(std::path::PathBuf::from(exe));
    }

    let desktop = WindowsDesktop::new(config);
    let window = desktop.focus_wecom().map_err(|err| err.to_string())?;
    let scale = desktop
        .screen_metrics()
        .map(|metrics| metrics.scale_factor)
        .unwrap_or(0.0);
    println!(
        "接管成功：类名「{class}」{}x{} @({}, {})，缩放 {scale}",
        window.width, window.height, window.x, window.y
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

fn arg(args: &[String], index: usize, name: &str) -> Result<String, String> {
    args.get(index)
        .cloned()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("缺少参数：{name}"))
}

fn number(args: &[String], index: usize, name: &str) -> Result<i32, String> {
    arg(args, index, name)?
        .parse::<i32>()
        .map_err(|_| format!("参数 {name} 必须是整数"))
}
