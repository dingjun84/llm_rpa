//! `winocr`：用 Windows 内置离线 OCR 引擎实现本项目的本地 OCR 契约。
//!
//! 契约见 `crates/vision/src/ocr.rs`：
//!
//! - **标准输入**：PNG 字节流（写完即关闭，作为输入结束信号）；
//! - **标准输出**：UTF-8 JSON 数组
//!   `[{"text":"张三","x":12,"y":40,"w":96,"h":24,"confidence":1.0}]`
//!
//! 引擎是系统自带的 `Windows.Media.Ocr`：**完全离线、不联网、不需要安装
//! 任何第三方模型**。语言优先级为「命令行第一个参数 → `zh-Hans-CN` →
//! 用户配置语言」。
//!
//! # 已知限制（务必知悉，不要当成缺陷掩盖过去）
//!
//! 1. **`Windows.Media.Ocr` 不提供逐词置信度。** 本工具因此统一输出
//!    `confidence = 1.0`。这意味着真实模式下 `min_confidence` 这道闸门
//!    实际上拦不住任何东西。若需要真正的置信度门控，应换用能给出置信度的
//!    本地引擎（例如 PaddleOCR）。这里把限制写在明处，而不是假装置信度可信。
//! 2. **按「行」输出文字框**（行内各词外接矩形的并集）。送达核验要求
//!    某个文字框的文本**包含**整条消息，所以过长的消息一旦在界面上折行，
//!    就可能无法被判定为同一框。
//! 3. 图片超过 `OcrEngine.MaxImageDimension` 时会等比缩小后再识别，
//!    此时输出的坐标是**缩小后图像**的坐标，与原始截图不再一致。

#[cfg(windows)]
mod imp {
    use std::io::{Read, Write};

    use serde::Serialize;
    use windows::core::HSTRING;
    use windows::Globalization::Language;
    use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::DataWriter;

    /// 输出契约中的单条结果。
    #[derive(Debug, Serialize)]
    struct OutBox {
        text: String,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        confidence: f32,
    }

    /// 判断是否属于中日韩文字或全角标点。
    fn is_cjk(c: char) -> bool {
        matches!(c as u32,
            0x3000..=0x303F |   // CJK 标点
            0x3400..=0x4DBF |   // 扩展 A
            0x4E00..=0x9FFF |   // 基本区
            0xF900..=0xFAFF |   // 兼容表意文字
            0xFF00..=0xFFEF     // 全角字符
        )
    }

    /// 去掉**两侧都是中日韩字符**的空格；拉丁文之间的空格保持不动。
    ///
    /// `Windows.Media.Ocr` 对中文按「字」切词，整行文本会变成「张 三」。
    /// 核心层的联系人匹配是**逐字精确匹配**、送达核验是**子串包含**，
    /// 两种都会被这种空格破坏，所以必须在这里还原成连续文本。
    fn collapse_cjk_spaces(text: &str) -> String {
        let chars: Vec<char> = text.chars().collect();
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        while i < chars.len() {
            if chars[i] != ' ' {
                out.push(chars[i]);
                i += 1;
                continue;
            }
            // 整段空格一起处理，避免连续空格只吃掉第一个。
            let start = i;
            while i < chars.len() && chars[i] == ' ' {
                i += 1;
            }
            let prev = start.checked_sub(1).and_then(|p| chars.get(p)).copied();
            let next = chars.get(i).copied();
            let between_cjk = matches!((prev, next), (Some(p), Some(n)) if is_cjk(p) && is_cjk(n));
            if !between_cjk {
                for _ in start..i {
                    out.push(' ');
                }
            }
        }
        out
    }

    /// 建引擎：优先用命令行指定的语言标签，其次简体中文，最后跟随用户配置。
    fn build_engine(preferred: Option<&str>) -> Result<OcrEngine, String> {
        let mut tried: Vec<String> = Vec::new();

        let mut candidates: Vec<String> = Vec::new();
        if let Some(tag) = preferred {
            candidates.push(tag.to_string());
        }
        candidates.push("zh-Hans-CN".to_string());

        for tag in &candidates {
            tried.push(tag.clone());
            let language = match Language::CreateLanguage(&HSTRING::from(tag.as_str())) {
                Ok(language) => language,
                Err(_) => continue,
            };
            if let Ok(engine) = OcrEngine::TryCreateFromLanguage(&language) {
                return Ok(engine);
            }
        }

        if let Ok(engine) = OcrEngine::TryCreateFromUserProfileLanguages() {
            return Ok(engine);
        }

        Err(format!(
            "无法创建 OCR 引擎（已尝试语言：{}，用户配置语言也不可用）。\
             请在系统设置中安装至少一个 OCR 语言包。",
            tried.join(", ")
        ))
    }

    /// RGBA8 → BGRA8（`Windows.Media.Ocr` 期望 Bgra8）。
    fn to_bgra(rgba: &image::RgbaImage) -> Vec<u8> {
        let mut bgra = Vec::with_capacity(rgba.as_raw().len());
        for px in rgba.pixels() {
            bgra.push(px[2]);
            bgra.push(px[1]);
            bgra.push(px[0]);
            bgra.push(px[3]);
        }
        bgra
    }

    pub fn run() -> Result<(), String> {
        let preferred: Option<String> = std::env::args().nth(1);

        // ── 读入 PNG ────────────────────────────────────────────────
        let mut png = Vec::new();
        std::io::stdin()
            .read_to_end(&mut png)
            .map_err(|err| format!("读取标准输入失败：{err}"))?;

        // 空输入视为「没有文字」，而不是错误：调用方可能截到了全空区域。
        if png.is_empty() {
            return write_boxes(&[]);
        }

        let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .map_err(|err| format!("PNG 解码失败：{err}"))?
            .to_rgba8();

        // ── 超过引擎上限则等比缩小 ──────────────────────────────────
        let max_dim = OcrEngine::MaxImageDimension().unwrap_or(0);
        let (mut width, mut height) = (decoded.width(), decoded.height());
        let mut image = decoded;
        if max_dim > 0 && (width > max_dim || height > max_dim) {
            let ratio = (max_dim as f32 / width.max(height) as f32).min(1.0);
            let new_w = ((width as f32 * ratio).floor() as u32).max(1);
            let new_h = ((height as f32 * ratio).floor() as u32).max(1);
            image = image::imageops::resize(
                &image,
                new_w,
                new_h,
                image::imageops::FilterType::Triangle,
            );
            width = new_w;
            height = new_h;
        }

        // ── 组装 SoftwareBitmap ─────────────────────────────────────
        let writer = DataWriter::new().map_err(|err| format!("创建数据写入器失败：{err}"))?;
        writer
            .WriteBytes(&to_bgra(&image))
            .map_err(|err| format!("写入像素数据失败：{err}"))?;
        let buffer = writer
            .DetachBuffer()
            .map_err(|err| format!("取出像素缓冲失败：{err}"))?;
        let bitmap = SoftwareBitmap::CreateCopyFromBuffer(
            &buffer,
            BitmapPixelFormat::Bgra8,
            width as i32,
            height as i32,
        )
        .map_err(|err| format!("创建 SoftwareBitmap 失败：{err}"))?;

        // ── 识别 ────────────────────────────────────────────────────
        let engine = build_engine(preferred.as_deref())?;
        let result = engine
            .RecognizeAsync(&bitmap)
            .map_err(|err| format!("启动识别失败：{err}"))?
            // windows-future 0.3 的阻塞等待叫 `join()`（旧版叫 `get()`）。
            .join()
            .map_err(|err| format!("识别过程失败：{err}"))?;

        // ── 逐行汇总（行内各词外接矩形的并集） ──────────────────────
        let mut boxes: Vec<OutBox> = Vec::new();
        let lines = result.Lines().map_err(|err| format!("读取识别结果失败：{err}"))?;
        for index in 0..lines.Size().map_err(|err| format!("读取行数失败：{err}"))? {
            let line = match lines.GetAt(index) {
                Ok(line) => line,
                Err(_) => continue,
            };
            let text = line.Text().map(|t| t.to_string()).unwrap_or_default();
            // 先归一化中日韩字间空格，再做空判断（全空格的行会被这一步滤掉）。
            let text = collapse_cjk_spaces(text.trim());
            if text.is_empty() {
                continue;
            }

            let words = match line.Words() {
                Ok(words) => words,
                Err(_) => continue,
            };
            let word_count = words.Size().unwrap_or(0);

            let mut min_x = f32::MAX;
            let mut min_y = f32::MAX;
            let mut max_x = f32::MIN;
            let mut max_y = f32::MIN;
            for wi in 0..word_count {
                let word = match words.GetAt(wi) {
                    Ok(word) => word,
                    Err(_) => continue,
                };
                let rect = match word.BoundingRect() {
                    Ok(rect) => rect,
                    Err(_) => continue,
                };
                min_x = min_x.min(rect.X);
                min_y = min_y.min(rect.Y);
                max_x = max_x.max(rect.X + rect.Width);
                max_y = max_y.max(rect.Y + rect.Height);
            }

            // 引擎偶尔给出没有词的行；这种行没有可用坐标，跳过而不是猜一个。
            if min_x > max_x || min_y > max_y {
                continue;
            }

            boxes.push(OutBox {
                text,
                x: min_x.round() as i32,
                y: min_y.round() as i32,
                w: (max_x - min_x).round() as i32,
                h: (max_y - min_y).round() as i32,
                // 见文件头「已知限制 1」：本引擎不提供置信度。
                confidence: 1.0,
            });
        }

        write_boxes(&boxes)
    }

    fn write_boxes(boxes: &[OutBox]) -> Result<(), String> {
        let json = serde_json::to_string(boxes).map_err(|err| format!("序列化失败：{err}"))?;
        let mut stdout = std::io::stdout();
        stdout
            .write_all(json.as_bytes())
            .map_err(|err| format!("写入标准输出失败：{err}"))?;
        stdout.flush().map_err(|err| format!("刷新标准输出失败：{err}"))?;
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn cjk_ranges_are_recognised() {
            assert!(is_cjk('张'));
            assert!(is_cjk('三'));
            assert!(is_cjk('，'));
            assert!(is_cjk('。'));
            assert!(!is_cjk('A'));
            assert!(!is_cjk(' '));
            assert!(!is_cjk(','));
        }

        #[test]
        fn collapses_spaces_between_chinese_characters() {
            // Windows OCR 对中文按字切词，这是它最典型的输出形态。
            assert_eq!(collapse_cjk_spaces("张 三"), "张三");
            assert_eq!(collapse_cjk_spaces("外 部 联 系 人"), "外部联系人");
            assert_eq!(collapse_cjk_spaces("你好， 世 界"), "你好，世界");
        }

        #[test]
        fn keeps_spaces_between_latin_words() {
            assert_eq!(collapse_cjk_spaces("hello world"), "hello world");
            assert_eq!(collapse_cjk_spaces("DeepSeek V4 Flash"), "DeepSeek V4 Flash");
        }

        #[test]
        fn keeps_spaces_at_the_boundary_between_scripts() {
            // 中文与拉丁文之间是真实的分隔，不能吃掉。
            assert_eq!(collapse_cjk_spaces("你好 world"), "你好 world");
            assert_eq!(collapse_cjk_spaces("WorkBuddy 客户 端"), "WorkBuddy 客户端");
        }

        #[test]
        fn collapses_whole_runs_of_spaces_not_just_the_first() {
            assert_eq!(collapse_cjk_spaces("张   三"), "张三");
        }

        #[test]
        fn empty_and_whitespace_only_input_is_safe() {
            assert_eq!(collapse_cjk_spaces(""), "");
            assert_eq!(collapse_cjk_spaces("   "), "   ");
        }
    }
}

fn main() {
    #[cfg(windows)]
    {
        if let Err(err) = imp::run() {
            eprintln!("winocr: {err}");
            std::process::exit(1);
        }
    }
    #[cfg(not(windows))]
    {
        eprintln!("winocr: 仅支持 Windows");
        std::process::exit(1);
    }
}
