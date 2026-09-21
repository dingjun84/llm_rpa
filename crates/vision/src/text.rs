//! 在 PNG 上写文字：系统字体定位 + 字形栅格化。
//!
//! ## 为什么需要它
//!
//! 过程诊断图要在画面上写中文——步骤名、识别到的**文字原文**、置信度。
//! 没有字就只能画一堆框，而"框里读成了什么"恰恰是复盘时要看的东西
//! （见 `docs/todo.md` T6）。
//!
//! ## 为什么字体要按候选列表去试
//!
//! 字体是**系统资产**，不是我们随程序分发的东西，所以本机一定有什么、
//! 一定没有什么，只能试。中文必须落在带 CJK 字形的字体上——拿
//! `Helvetica` 渲染会得到一串豆腐块（.notdef），比不写更误导人。
//! 试完全都失败时**报错而不是凑合**：宁可退回"只有框、没有字"，
//! 也不要留下一张全是方块的图让人以为识别结果就是那样。

use ab_glyph::{Font, FontVec, ScaleFont};
use image::{Rgba, RgbaImage};

use crate::{VisionError, VisionResult};

/// 候选字体（按优先级）。`.ttc` 是字体集合，取第 0 个面。
///
/// macOS 上 `Hiragino Sans GB` 是简体中文字面最全的一档；Windows 上
/// `msyh.ttc`（微软雅黑）同理。都放进来是因为这个 crate 两个平台都在用。
const FONT_CANDIDATES: &[(&str, u32)] = &[
    ("/System/Library/Fonts/Hiragino Sans GB.ttc", 0),
    ("/System/Library/Fonts/STHeiti Medium.ttc", 0),
    ("/System/Library/Fonts/Supplemental/Songti.ttc", 0),
    ("/System/Library/Fonts/Supplemental/Arial Unicode.ttf", 0),
    ("C:/Windows/Fonts/msyh.ttc", 0),
    ("C:/Windows/Fonts/simhei.ttf", 0),
    ("C:/Windows/Fonts/simsun.ttc", 0),
];

/// 一个可用于写字的字体。
pub struct TextFont {
    font: FontVec,
}

impl TextFont {
    /// 按候选列表找本机可用的中文字体。
    pub fn load_system() -> VisionResult<Self> {
        let mut tried: Vec<String> = Vec::new();
        for (path, index) in FONT_CANDIDATES {
            let Ok(bytes) = std::fs::read(path) else {
                tried.push((*path).to_string());
                continue;
            };
            match FontVec::try_from_vec_and_index(bytes, *index) {
                Ok(font) => return Ok(Self { font }),
                Err(err) => tried.push(format!("{path}（{err}）")),
            }
        }
        Err(VisionError::FontUnavailable { tried: tried.join("、") })
    }

    /// 一行文字占多宽（像素）。用于排版时决定栏宽、以及是否需要截断。
    pub fn measure(&self, px: f32, text: &str) -> f32 {
        let scaled = self.font.as_scaled(px);
        let mut caret = 0.0f32;
        let mut previous = None;
        for ch in text.chars() {
            let id = scaled.glyph_id(ch);
            if let Some(prev) = previous {
                caret += scaled.kern(id, prev);
            }
            caret += scaled.h_advance(id);
            previous = Some(id);
        }
        caret
    }

    /// 把文字画到 `(x, top)`，`top` 是**文字行顶端**（不是基线）。
    ///
    /// 超出画布的字形会被丢掉，不会 panic——一张诊断图不值得为排版溢出崩溃。
    pub fn draw(
        &self,
        image: &mut RgbaImage,
        x: i32,
        top: i32,
        px: f32,
        color: [u8; 4],
        text: &str,
    ) {
        let scaled = self.font.as_scaled(px);
        let baseline = top as f32 + scaled.ascent();
        let mut caret = x as f32;
        let mut previous = None;
        for ch in text.chars() {
            let id = scaled.glyph_id(ch);
            if let Some(prev) = previous {
                caret += scaled.kern(id, prev);
            }
            let glyph = id.with_scale_and_position(px, ab_glyph::point(caret, baseline));
            if let Some(outline) = self.font.outline_glyph(glyph) {
                let bounds = outline.px_bounds();
                outline.draw(|gx, gy, coverage| {
                    let px_x = bounds.min.x as i32 + gx as i32;
                    let px_y = bounds.min.y as i32 + gy as i32;
                    blend(image, px_x, px_y, color, coverage);
                });
            }
            caret += scaled.h_advance(id);
            previous = Some(id);
        }
    }

    /// 太宽就截断成 `...`。返回的文字保证能塞进 `max_width`。
    ///
    /// 为什么逐个字符退而不是按字节切：中文字形在 `str` 里是 3 字节，
    /// 按字节切会切出半个字符，`&str` 直接 panic。
    pub fn fit(&self, px: f32, text: &str, max_width: f32) -> String {
        if self.measure(px, text) <= max_width {
            return text.to_string();
        }
        let mut kept = String::new();
        for ch in text.chars() {
            let mut candidate = kept.clone();
            candidate.push(ch);
            candidate.push('…');
            if self.measure(px, &candidate) > max_width {
                break;
            }
            kept.push(ch);
        }
        if kept.is_empty() {
            // 连一个字符加省略号都放不下：宁可不画（空串），不要画一个越界的框。
            return String::new();
        }
        kept.push('…');
        kept
    }
}

/// 按覆盖率把颜色混到画布上（把 `u8` 覆盖值还原成 0~1 后线性混合）。
fn blend(image: &mut RgbaImage, x: i32, y: i32, color: [u8; 4], coverage: f32) {
    if x < 0 || y < 0 || x as u32 >= image.width() || y as u32 >= image.height() {
        return;
    }
    let alpha = coverage.clamp(0.0, 1.0);
    let target = image.get_pixel_mut(x as u32, y as u32);
    let mut mixed = [0u8; 4];
    for channel in 0..3 {
        let base = target.0[channel] as f32;
        mixed[channel] = (color[channel] as f32 * alpha + base * (1.0 - alpha)).round() as u8;
    }
    // 不透明底之上写字：结果一律不透明，免得叠出一层半透明的怪色。
    mixed[3] = 255;
    *target = Rgba(mixed);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 找不到系统字体时跳过——**这不是被测代码的错误**，
    /// 而是这台机器上没有候选字体。CI 上出现这种情况不该算失败。
    fn font() -> Option<TextFont> {
        TextFont::load_system().ok()
    }

    #[test]
    fn measuring_grows_with_the_text() {
        let Some(font) = font() else { return };
        let one = font.measure(14.0, "过");
        let two = font.measure(14.0, "太过");
        assert!(one > 0.0, "一个字必须有宽度");
        assert!(two > one, "两个字必须比一个字宽");
    }

    #[test]
    fn drawing_changes_pixels_inside_the_glyph_box_only() {
        let Some(font) = font() else { return };
        let mut image = RgbaImage::from_pixel(120, 40, Rgba([0, 0, 0, 255]));
        font.draw(&mut image, 4, 4, 20.0, [255, 255, 255, 255], "太过活跃");

        let painted = image.pixels().filter(|p| p.0[0] > 0).count();
        assert!(painted > 0, "画完之后应该有像素被点亮");
        // 行高之外必须原样（说明没有把整块涂白）。
        assert_eq!(image.get_pixel(0, 39).0, [0, 0, 0, 255]);
    }

    #[test]
    fn fitting_shrinks_the_text_until_it_fits() {
        let Some(font) = font() else { return };
        let long = "太过活跃李小明（本人）彩色打印";
        let limit = 60.0;
        let fitted = font.fit(14.0, long, limit);

        assert_ne!(fitted, long);
        assert!(fitted.ends_with('…'));
        assert!(
            font.measure(14.0, &fitted) <= limit,
            "截断之后必须真的塞得下"
        );
    }

    #[test]
    fn fitting_leaves_short_text_alone() {
        let Some(font) = font() else { return };
        assert_eq!(font.fit(14.0, "太过", 500.0), "太过");
    }

    #[test]
    fn fitting_gives_up_when_even_one_character_does_not_fit() {
        let Some(font) = font() else { return };
        // 宽 0 是"这一栏没地方写字"，此时**不写字**比画半个字出去更安全。
        assert_eq!(font.fit(14.0, "太过活跃", 0.0), "");
    }
}