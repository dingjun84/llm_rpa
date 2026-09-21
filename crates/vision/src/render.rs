//! 过程诊断图：把每一步「要识别的区域 + 当时的画面 + 识别到的文字」渲染成能直接看的 PNG。
//!
//! ## 为什么要有它
//!
//! 失败证据图是**脱敏**的（每个文字框都被涂成中灰），只能核对版面，
//! 回答不了"它到底读成了什么"（`docs/todo.md` T6）。而真实故障里最要紧的判据
//! 恰恰是这一条：读到的是不是目标名字、置信度多少、框有没有落在应该落的地方。
//!
//! 所以这里画的是**原图 + 叠加层**，一页一步：
//!
//! - 顶部一条说明带：序号 / 时刻 / 这一步在做什么 / 屏幕坐标与尺寸 / 帧缩放 / 读到几块；
//! - 中间是那一帧本身，四边描一圈橙色 = **要识别的区域**的边界；
//! - 每个识别到的文字块描绿框，左上角标序号；
//! - 右侧文字栏逐条列出 `序号 置信度 文字原文`——窄区域（搜索下拉只有几十像素宽）
//!   里根本挤不下字，所以文字一律放到旁边这一栏，而不是压在画面上。

use automation_core::{Rect, Screenshot, TextBox};
use image::{Rgba, RgbaImage};

use crate::pixels::to_rgba;
use crate::text::TextFont;
use crate::VisionResult;

/// 一步的渲染输入。
///
/// 与 `automation_core::Observation` 一一对应，但**不复用那个类型**：
/// 它带生命周期、属于对外端口，而这里只要渲染需要的几样东西。
pub struct Step<'a> {
    /// 这一步在做什么（区域名 / 步骤名）。
    pub label: &'a str,
    /// 时刻，**已由调用方格式化**成给人看的样子（本地时区）。
    ///
    /// 为什么不让这里自己格式化：`vision` 只该管像素，取本地时区偏移是调用方的活
    /// ——否则这个 crate 为了写一行时间也要拖一个时区库进来。
    pub stamp: &'a str,
    /// 这一步要识别的区域，屏幕坐标。
    pub region: Rect,
    /// 本次截图（BGRA）。
    pub frame: &'a Screenshot,
    /// 识别到的文字块，位于本帧图像坐标。
    pub text_boxes: &'a [TextBox],
}

const MARGIN: i32 = 12;
const HEADER_H: i32 = 30;
const HEADER_GAP: i32 = 10;
const COLUMN_GAP: i32 = 16;
const COLUMN_W: i32 = 380;
const LINE_H: i32 = 20;
/// 画面栏的最小宽度。搜索下拉这类区域只有几十像素宽，照原宽排版会把上面的
/// 说明带挤到写不下字——说明带是这一页最要紧的信息，不能先牺牲它。
const MIN_FRAME_COL: i32 = 420;
const STEP_GAP: i32 = 16;
const TITLE_H: i32 = 52;

const BG: [u8; 4] = [30, 32, 38, 255];
const HEADER_BG: [u8; 4] = [18, 20, 26, 255];
const TEXT: [u8; 4] = [232, 234, 240, 255];
const DIM: [u8; 4] = [150, 155, 168, 255];
/// 要识别的区域（= 截图本身的范围）。
const REGION_LINE: [u8; 4] = [255, 168, 64, 255];
const BOX_LINE: [u8; 4] = [80, 220, 130, 255];
const BOX_CHIP: [u8; 4] = [16, 72, 40, 255];
const REGION_CHIP: [u8; 4] = [96, 58, 16, 255];

const BODY_PX: f32 = 14.0;
const SMALL_PX: f32 = 12.0;
const TITLE_PX: f32 = 18.0;

/// 画一页：一步的截图 + 区域框 + 文字块框 + 文字栏。
pub fn annotate_step(step: &Step<'_>, font: &TextFont) -> VisionResult<RgbaImage> {
    let frame = to_rgba(step.frame)?;
    let frame_col = frame.width() as i32;
    let column_x = MARGIN + frame_col.max(MIN_FRAME_COL) + COLUMN_GAP;
    let page_w = column_x + COLUMN_W + MARGIN;
    let frame_top = MARGIN + HEADER_H + HEADER_GAP;
    let column_h = (step.text_boxes.len() as i32 + 2) * LINE_H;
    let page_h = frame_top + (frame.height() as i32).max(column_h) + MARGIN;

    let mut page = RgbaImage::from_pixel(page_w as u32, page_h as u32, Rgba(BG));
    draw_header(&mut page, step, font, page_w);
    blit(&mut page, &frame, MARGIN, frame_top);

    let frame_area = (MARGIN, frame_top, frame.width() as i32, frame.height() as i32);
    stroke(&mut page, frame_area, REGION_LINE);
    draw_region_chip(&mut page, frame_area, step.region, page_w, font);
    draw_text_boxes(&mut page, frame_area, step.text_boxes, font);
    draw_text_column(&mut page, column_x, frame_top, step, font);
    Ok(page)
}

/// 把若干页纵向拼成一张总图。
///
/// `note` 是给"这张总图被截过"这类说明留的位置；为空时那一行不画。
pub fn compose_overview(
    title: &str,
    note: &str,
    font: &TextFont,
    pages: &[RgbaImage],
) -> VisionResult<RgbaImage> {
    let width = pages.iter().map(|p| p.width()).max().unwrap_or(720).max(320);
    let height =
        TITLE_H as u32 + pages.iter().map(|p| p.height() + STEP_GAP as u32).sum::<u32>() + MARGIN as u32;
    let mut sheet = RgbaImage::from_pixel(width, height, Rgba(BG));

    fill(&mut sheet, (0, 0, width as i32, TITLE_H), HEADER_BG);
    font.draw(&mut sheet, MARGIN, 8, TITLE_PX, TEXT, &font.fit(TITLE_PX, title, (width as i32 - 2 * MARGIN) as f32));
    if !note.is_empty() {
        let fitted = font.fit(SMALL_PX, note, (width as i32 - 2 * MARGIN) as f32);
        let at = MARGIN + font.measure(TITLE_PX, title) as i32 + 12;
        font.draw(&mut sheet, at, 13, SMALL_PX, DIM, &fitted);
    }

    let mut y = TITLE_H as i32;
    for page in pages {
        blit(&mut sheet, page, 0, y);
        fill(&mut sheet, (0, y - 2, width as i32, 2), HEADER_BG);
        y += page.height() as i32 + STEP_GAP;
    }
    Ok(sheet)
}

fn draw_header(page: &mut RgbaImage, step: &Step<'_>, font: &TextFont, page_w: i32) {
    fill(page, (0, 0, page_w, HEADER_H), HEADER_BG);
    let scale_x = step.frame.width as f32 / step.region.width.max(1) as f32;
    let scale_y = step.frame.height as f32 / step.region.height.max(1) as f32;
    let line = format!(
        "{}  ·  {}  ·  屏幕({}, {}) {}x{}  ·  帧 {}x{} 缩放 {:.2}x{:.2}  ·  读到 {} 块",
        step.label,
        step.stamp,
        step.region.x,
        step.region.y,
        step.region.width,
        step.region.height,
        step.frame.width,
        step.frame.height,
        scale_x,
        scale_y,
        step.text_boxes.len(),
    );
    let fitted = font.fit(BODY_PX, &line, (page_w - 2 * MARGIN) as f32);
    font.draw(page, MARGIN, 7, BODY_PX, TEXT, &fitted);
}

/// 在区域框的左上角挂一块牌子，写明这块是**屏幕上的哪一块**。
///
/// 为什么要写字而不只画个框：截图本身看不出"这块是屏幕的哪一处"，
/// 而"区域标定偏了"正是要靠这一行才能判——偏 20 像素时画面上可能仍有字，
/// 但坐标与标定值对不上。
///
/// 牌子的宽度按**整页**而不是截图的宽度算：搜索下拉这类区域只有几十像素宽，
/// 按截图宽度截断会把坐标恰好切掉——而那正是这块牌子唯一要说的事。
fn draw_region_chip(
    page: &mut RgbaImage,
    area: (i32, i32, i32, i32),
    region: Rect,
    page_w: i32,
    font: &TextFont,
) {
    let text = format!("识别区域 ({}, {}) {}x{}", region.x, region.y, region.width, region.height);
    let text = font.fit(SMALL_PX, &text, (page_w - area.0 - MARGIN) as f32);
    if text.is_empty() {
        return;
    }
    let width = font.measure(SMALL_PX, &text) as i32 + 8;
    let height = 18;
    let top = (area.1 - height).max(0);
    fill(page, (area.0, top, width, height), REGION_CHIP);
    font.draw(page, area.0 + 4, top + 3, SMALL_PX, TEXT, &text);
}

fn draw_text_boxes(page: &mut RgbaImage, area: (i32, i32, i32, i32), boxes: &[TextBox], font: &TextFont) {
    for (index, item) in boxes.iter().enumerate() {
        let rect = clip(
            (area.0 + item.bounds.x, area.1 + item.bounds.y, item.bounds.width, item.bounds.height),
            area,
        );
        let Some(rect) = rect else { continue };
        stroke(page, rect, BOX_LINE);
        // 序号画在框内左上角，与右侧文字栏的编号对应。
        let chip = format!("#{}", index + 1);
        let width = font.measure(SMALL_PX, &chip) as i32 + 6;
        let height = 15;
        if rect.2 < 12 || rect.3 < 12 {
            // 框太小，画不下牌子，只留绿框（文字栏里照样能对上号）。
            continue;
        }
        fill(page, (rect.0, rect.1, width, height), BOX_CHIP);
        font.draw(page, rect.0 + 3, rect.1 + 1, SMALL_PX, BOX_LINE, &chip);
    }
}

fn draw_text_column(page: &mut RgbaImage, x: i32, top: i32, step: &Step<'_>, font: &TextFont) {
    let title = format!("识别到的文字（{} 块）", step.text_boxes.len());
    font.draw(page, x, top, BODY_PX, DIM, &title);
    if step.text_boxes.is_empty() {
        let hint = font.fit(SMALL_PX, "（本步只截了指纹做比对，没有识别文字）", COLUMN_W as f32);
        font.draw(page, x, top + LINE_H, SMALL_PX, DIM, &hint);
        return;
    }
    let width = COLUMN_W as f32;
    for (index, item) in step.text_boxes.iter().enumerate() {
        let raw = format!("#{}  {:.2}  {}", index + 1, item.confidence, item.text);
        let fitted = font.fit(BODY_PX, &raw, width);
        font.draw(page, x, top + (index as i32 + 1) * LINE_H, BODY_PX, TEXT, &fitted);
    }
}

// ── 像素原语 ────────────────────────────────────────────────────────────

fn fill(image: &mut RgbaImage, area: (i32, i32, i32, i32), color: [u8; 4]) {
    let (left, top, right, bottom) = clamp_to(image, area);
    for y in top..bottom {
        for x in left..right {
            image.put_pixel(x as u32, y as u32, Rgba(color));
        }
    }
}

/// 描一圈 2 像素的边。用两条实心矩形拼（上下、左右），比按周长逐点画好读。
fn stroke(image: &mut RgbaImage, area: (i32, i32, i32, i32), color: [u8; 4]) {
    let (x, y, width, height) = area;
    fill(image, (x, y, width, 2), color);
    fill(image, (x, y + height - 2, width, 2), color);
    fill(image, (x, y, 2, height), color);
    fill(image, (x + width - 2, y, 2, height), color);
}

fn blit(page: &mut RgbaImage, source: &RgbaImage, x: i32, y: i32) {
    for (sx, sy, pixel) in source.enumerate_pixels() {
        let (dx, dy) = (x + sx as i32, y + sy as i32);
        if dx >= 0 && dy >= 0 && (dx as u32) < page.width() && (dy as u32) < page.height() {
            page.put_pixel(dx as u32, dy as u32, *pixel);
        }
    }
}

/// 把矩形裁进画布，返回 `(left, top, right, bottom)`；完全在画布外时返回空区间。
fn clamp_to(image: &RgbaImage, area: (i32, i32, i32, i32)) -> (i32, i32, i32, i32) {
    let (x, y, width, height) = area;
    let left = x.max(0);
    let top = y.max(0);
    let right = (x + width).min(image.width() as i32);
    let bottom = (y + height).min(image.height() as i32);
    if right <= left || bottom <= top {
        return (0, 0, 0, 0);
    }
    (left, top, right, bottom)
}

/// 把矩形裁进另一块矩形，完全落在外面时返回 `None`。
fn clip(area: (i32, i32, i32, i32), outer: (i32, i32, i32, i32)) -> Option<(i32, i32, i32, i32)> {
    let left = area.0.max(outer.0);
    let top = area.1.max(outer.1);
    let right = (area.0 + area.2).min(outer.0 + outer.2);
    let bottom = (area.1 + area.3).min(outer.1 + outer.3);
    if right <= left || bottom <= top {
        return None;
    }
    Some((left, top, right - left, bottom - top))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn font() -> Option<TextFont> {
        TextFont::load_system().ok()
    }

    fn frame(width: u32, height: u32) -> Screenshot {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            pixels.extend_from_slice(&[40, 40, 40, 255]);
        }
        Screenshot {
            fingerprint: "f".into(),
            pixels,
            width,
            height,
            captured_at: SystemTime::now(),
        }
    }

    fn boxes(list: &[(&str, f32)]) -> Vec<TextBox> {
        list.iter()
            .enumerate()
            .map(|(i, (text, confidence))| TextBox {
                text: (*text).to_string(),
                bounds: Rect { x: 4, y: 6 + i as i32 * 24, width: 60, height: 18 },
                confidence: *confidence,
            })
            .collect()
    }

    #[test]
    fn a_step_page_is_bigger_than_the_frame_it_shows() {
        let Some(font) = font() else { return };
        let shot = frame(200, 120);
        let list = boxes(&[("太过活跃", 0.83)]);
        let page = annotate_step(
            &Step {
                label: "搜索下拉列表",
                stamp: "2026-09-21 17:52:01.123",
                region: Rect { x: 516, y: 182, width: 200, height: 120 },
                frame: &shot,
                text_boxes: &list,
            },
            &font,
        )
        .unwrap();

        assert!(page.width() > 200, "右侧要留出文字栏");
        assert!(page.height() > 120, "顶部要留出说明带");
    }

    #[test]
    fn the_frame_itself_is_drawn_onto_the_page() {
        let Some(font) = font() else { return };
        let shot = frame(120, 60);
        let page = annotate_step(
            &Step {
                label: "搜索框",
                stamp: "t",
                region: Rect { x: 0, y: 0, width: 120, height: 60 },
                frame: &shot,
                text_boxes: &[],
            },
            &font,
        )
        .unwrap();

        // 截图原色 (40,40,40) 必须能在页面上找到（说明画面真的贴上去了）。
        assert!(page.pixels().any(|p| p.0 == [40, 40, 40, 255]));
    }

    #[test]
    fn a_text_box_far_outside_the_frame_does_not_panic() {
        let Some(font) = font() else { return };
        let shot = frame(80, 40);
        let list = vec![TextBox {
            text: "越界".into(),
            bounds: Rect { x: 900, y: 900, width: 50, height: 20 },
            confidence: 0.5,
        }];
        let page = annotate_step(
            &Step {
                label: "越界框",
                stamp: "t",
                region: Rect { x: 0, y: 0, width: 80, height: 40 },
                frame: &shot,
                text_boxes: &list,
            },
            &font,
        );
        assert!(page.is_ok(), "框跑到画面外时应该被裁掉，而不是出错");
    }

    #[test]
    fn the_overview_stacks_every_page_without_shrinking_them() {
        let Some(font) = font() else { return };
        let shot = frame(100, 50);
        let page = annotate_step(
            &Step {
                label: "一步",
                stamp: "t",
                region: Rect { x: 0, y: 0, width: 100, height: 50 },
                frame: &shot,
                text_boxes: &[],
            },
            &font,
        )
        .unwrap();

        let sheet = compose_overview("任务 abc 过程诊断", "", &font, &[page.clone(), page.clone()]).unwrap();
        assert_eq!(sheet.width(), page.width());
        assert!(sheet.height() >= page.height() * 2 + TITLE_H as u32);
    }

    #[test]
    fn an_overview_without_pages_still_produces_an_image() {
        let Some(font) = font() else { return };
        let sheet = compose_overview("任务 abc 过程诊断", "", &font, &[]);
        assert!(sheet.is_ok(), "一步都没记上时也不能出错");
    }
}