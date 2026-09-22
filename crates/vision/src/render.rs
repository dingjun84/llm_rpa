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
//!
//! ## 底图为什么是**整窗**而不是裁图（2026-09-21 用户要求）
//!
//! 只贴裁图时，那圈橙色的「要识别的区域」恒等于图像边界——**看不出这块区域
//! 落在窗口的哪个位置**，而"区域标定偏了"恰恰是最常见的一类故障。
//! 有整窗当底图，区域框才落在一个能一眼判定的地方（对应自己被调去查的那个区域）。
//! 取不到整窗（还没进客户端、演练替身、截屏失败）时退回只画裁图：
//! 那时区域框仍等于图像边界，但画面本身照样是能看的。

use automation_core::{IconHit, Rect, Screenshot, TextBox};
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
    /// 本次截图（BGRA），位于本帧图像坐标。
    pub frame: &'a Screenshot,
    /// **整窗**底图：窗口矩形 + 那一帧画面；拿不到时为 `None`（退回只画裁图）。
    ///
    /// 它是"区域框落在窗口哪儿"这个问题的唯一答案，所以只要拿得到就用它当底图。
    pub window: Option<(&'a Screenshot, Rect)>,
    /// 识别到的文字块，位于本帧图像坐标。
    pub text_boxes: &'a [TextBox],
    /// 图标匹配的命中（框 + 模板名 + 分数）；不做图标匹配的步骤为 `None`。
    pub icon: Option<&'a IconHit>,
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
/// 要识别的区域。有了整窗底图后它是一块**落在窗口内某处**的框，
/// 而不再等于图像边界——正是这个差别让"标定偏了"能一眼看出来。
const REGION_LINE: [u8; 4] = [255, 168, 64, 255];
const BOX_LINE: [u8; 4] = [80, 220, 130, 255];
const BOX_CHIP: [u8; 4] = [16, 72, 40, 255];
const REGION_CHIP: [u8; 4] = [96, 58, 16, 255];
/// 图标匹配的命中框。**必须与文字框区分开**：两者画在同一张图上时，
/// "哪个框是文字、哪个框是图标"只能靠颜色回答。
const ICON_LINE: [u8; 4] = [120, 180, 255, 255];
const ICON_CHIP: [u8; 4] = [24, 52, 96, 255];

const BODY_PX: f32 = 14.0;
const SMALL_PX: f32 = 12.0;
const TITLE_PX: f32 = 18.0;

/// 画一页：底图 + 区域框 + 文字块框 + 图标命中框 + 右侧文字栏。
pub fn annotate_step(step: &Step<'_>, font: &TextFont) -> VisionResult<RgbaImage> {
    // 底图优先用**整窗**：只有它能让「要识别的区域」落在一个能判定的位置上。
    // 拿不到就退回裁图（那时区域框等于图像边界，退化成"只核对版面"）。
    let (base, base_rect) = match step.window {
        Some((shot, rect)) => (to_rgba(shot)?, rect),
        None => (to_rgba(step.frame)?, step.region),
    };
    let base_scale_x = base.width() as f32 / base_rect.width.max(1) as f32;
    let base_scale_y = base.height() as f32 / base_rect.height.max(1) as f32;
    // 帧图像坐标 → 屏幕逻辑点：Retina 上帧是物理像素（2x），除以这个比例才回到逻辑点。
    let frame_scale_x = step.frame.width as f32 / step.region.width.max(1) as f32;
    let frame_scale_y = step.frame.height as f32 / step.region.height.max(1) as f32;

    let column_extra = step.icon.is_some() as i32;
    let frame_col = base.width() as i32;
    let column_x = MARGIN + frame_col.max(MIN_FRAME_COL) + COLUMN_GAP;
    let page_w = column_x + COLUMN_W + MARGIN;
    let frame_top = MARGIN + HEADER_H + HEADER_GAP;
    let area = (MARGIN, frame_top, base.width() as i32, base.height() as i32);
    let column_h = (step.text_boxes.len() as i32 + 2 + column_extra) * LINE_H;
    let page_h = frame_top + (base.height() as i32).max(column_h) + MARGIN;

    let mut page = RgbaImage::from_pixel(page_w as u32, page_h as u32, Rgba(BG));
    draw_header(&mut page, step, font, page_w, base_rect);
    blit(&mut page, &base, MARGIN, frame_top);

    // 把**屏幕坐标**的一块映射到页面上。区域框与命中框都走这一条。
    let to_page = |rect: Rect| -> (i32, i32, i32, i32) {
        (
            area.0 + ((rect.x - base_rect.x) as f32 * base_scale_x).round() as i32,
            area.1 + ((rect.y - base_rect.y) as f32 * base_scale_y).round() as i32,
            (rect.width as f32 * base_scale_x).round() as i32,
            (rect.height as f32 * base_scale_y).round() as i32,
        )
    };
    // 帧图像坐标（文字框、图标命中框）先回到屏幕坐标，再走上面那条映射。
    let frame_to_page = |bounds: Rect| -> (i32, i32, i32, i32) {
        to_page(Rect {
            x: step.region.x + (bounds.x as f32 / frame_scale_x).round() as i32,
            y: step.region.y + (bounds.y as f32 / frame_scale_y).round() as i32,
            width: (bounds.width as f32 / frame_scale_x).round() as i32,
            height: (bounds.height as f32 / frame_scale_y).round() as i32,
        })
    };

    let region_area = to_page(step.region);
    // 区域框可能整块落在底图外（区域与窗口对不上——正是要看的故障），裁掉即可。
    if let Some(rect) = clip(region_area, area) {
        stroke(&mut page, rect, REGION_LINE);
    }
    draw_region_chip(&mut page, region_area, step.region, page_w, font);

    let box_rects: Vec<_> = step.text_boxes.iter().map(|b| frame_to_page(b.bounds)).collect();
    draw_text_boxes(&mut page, area, &box_rects, font);
    if let Some(hit) = step.icon {
        draw_icon_hit(&mut page, area, frame_to_page(hit.bounds), column_x, hit, font);
    }
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

fn draw_header(page: &mut RgbaImage, step: &Step<'_>, font: &TextFont, page_w: i32, base_rect: Rect) {
    fill(page, (0, 0, page_w, HEADER_H), HEADER_BG);
    let scale_x = step.frame.width as f32 / step.region.width.max(1) as f32;
    let scale_y = step.frame.height as f32 / step.region.height.max(1) as f32;
    // 底图是整窗还是裁图**必须写明**：两者看上去都是一张截图，而"区域框为什么在这儿"
    // 完全取决于它。看错这一条，会把"标定偏了"读成"底图裁错了"。
    let source = match step.window {
        Some(_) => format!(
            "底图 整窗({}, {}) {}x{}",
            base_rect.x, base_rect.y, base_rect.width, base_rect.height
        ),
        None => "底图 裁图（没取到整窗）".to_string(),
    };
    let line = format!(
        "{}  ·  {}  ·  区域 屏幕({}, {}) {}x{}  ·  {}  ·  帧 {}x{} 缩放 {:.2}x{:.2}  ·  读到 {} 块",
        step.label,
        step.stamp,
        step.region.x,
        step.region.y,
        step.region.width,
        step.region.height,
        source,
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

fn draw_text_boxes(
    page: &mut RgbaImage,
    area: (i32, i32, i32, i32),
    rects: &[(i32, i32, i32, i32)],
    font: &TextFont,
) {
    for (index, rect) in rects.iter().enumerate() {
        let Some(rect) = clip(*rect, area) else { continue };
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

/// 画图标匹配的命中：蓝框 + 一块写明「模板名 + 分数」的牌子。
///
/// ## 为什么牌子上的字要限制在右侧文字栏以左
///
/// 牌子挂在命中框的左上角，往右延伸。命中框通常贴着窗口左边（导航栏就在那儿），
/// 所以右边空间足够；真到了窗口最右侧、放不下时宁可**不画牌子**——
/// 压到文字栏上会把两处信息都弄花，而完整信息在文字栏里也已经列了一份。
fn draw_icon_hit(
    page: &mut RgbaImage,
    area: (i32, i32, i32, i32),
    rect: (i32, i32, i32, i32),
    column_x: i32,
    hit: &IconHit,
    font: &TextFont,
) {
    let Some(rect) = clip(rect, area) else { return };
    stroke(page, rect, ICON_LINE);

    let room = (column_x - rect.0 - 8).min(COLUMN_W);
    if room < 80 {
        return;
    }
    let text = font.fit(SMALL_PX, &format!("图标 {} {:.3}", hit.template, hit.score), room as f32);
    if text.is_empty() {
        return;
    }
    let width = font.measure(SMALL_PX, &text) as i32 + 8;
    let height = 18;
    let top = (rect.1 - height).max(0);
    fill(page, (rect.0, top, width, height), ICON_CHIP);
    font.draw(page, rect.0 + 4, top + 3, SMALL_PX, ICON_LINE, &text);
}

fn draw_text_column(page: &mut RgbaImage, x: i32, top: i32, step: &Step<'_>, font: &TextFont) {
    let mut y = top;
    // 图标命中先列：图标上没有文字，这一行是它唯一的可读结果。
    if let Some(hit) = step.icon {
        let line = format!("图标命中 「{}」 分数 {:.3}", hit.template, hit.score);
        let fitted = font.fit(BODY_PX, &line, COLUMN_W as f32);
        font.draw(page, x, y, BODY_PX, ICON_LINE, &fitted);
        y += LINE_H;
    }
    let title = format!("识别到的文字（{} 块）", step.text_boxes.len());
    font.draw(page, x, y, BODY_PX, DIM, &title);
    if step.text_boxes.is_empty() {
        let hint = font.fit(SMALL_PX, "（本步只截了指纹做比对，没有识别文字）", COLUMN_W as f32);
        font.draw(page, x, y + LINE_H, SMALL_PX, DIM, &hint);
        return;
    }
    let width = COLUMN_W as f32;
    for (index, item) in step.text_boxes.iter().enumerate() {
        let raw = format!("#{}  {:.2}  {}", index + 1, item.confidence, item.text);
        let fitted = font.fit(BODY_PX, &raw, width);
        font.draw(page, x, y + (index as i32 + 1) * LINE_H, BODY_PX, TEXT, &fitted);
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
mod tests;
