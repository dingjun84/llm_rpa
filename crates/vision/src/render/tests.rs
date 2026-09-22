//! `annotate_step` / `compose_overview` 的用例：底图选谁、框画在哪儿、页面长多大。
//!
//! 拆成独立文件是因为主体已经贴着文件行数上限，而**测试文件不限行数**
//! （`CONVENTIONS.md` §2/§9），与 `policy/trail.rs` + `policy/trail/tests.rs` 同一做法。

use super::*;
use automation_core::{IconHit, Rect, Screenshot, TextBox};
use std::time::SystemTime;
use crate::text::TextFont;

fn font() -> Option<TextFont> {
    TextFont::load_system().ok()
}

fn frame(width: u32, height: u32) -> Screenshot {
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for _ in 0..width * height {
        pixels.extend_from_slice(&[40, 40, 40, 255]);
    }
    Screenshot { fingerprint: "f".into(), pixels, width, height, captured_at: SystemTime::now() }
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

/// 一张纯色帧，颜色用来区分"哪张图被贴到了页面上"。
fn solid(width: u32, height: u32, gray: u8) -> Screenshot {
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for _ in 0..width * height {
        pixels.extend_from_slice(&[gray, gray, gray, 255]);
    }
    Screenshot { fingerprint: format!("f{gray}"), pixels, width, height, captured_at: SystemTime::now() }
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
            window: None,
            text_boxes: &list,
            icon: None,
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
            window: None,
            text_boxes: &[],
            icon: None,
        },
        &font,
    )
    .unwrap();

    // 截图原色 (40,40,40) 必须能在页面上找到（说明画面真的贴上去了）。
    assert!(page.pixels().any(|p| p.0 == [40, 40, 40, 255]));
}

/// ★ 用户要的那一条：有整窗时**用它当底图**，并且区域框落在窗口内的位置上。
///
/// 这一条钉住两件事：
/// - 底图是整窗（裁图那 40 灰**不该**出现在页面上）；
/// - 区域框画在「区域相对窗口的偏移」处，而不是贴在图像边界。
#[test]
fn a_window_base_puts_the_region_box_where_it_really_is() {
    let Some(font) = font() else { return };
    let crop = solid(120, 60, 40);
    let window_shot = solid(400, 300, 90);
    let window_rect = Rect { x: 0, y: 0, width: 400, height: 300 };
    let region = Rect { x: 100, y: 50, width: 120, height: 60 };
    let page = annotate_step(
        &Step {
            label: "搜索下拉列表",
            stamp: "t",
            region,
            frame: &crop,
            window: Some((&window_shot, window_rect)),
            text_boxes: &[],
            icon: None,
        },
        &font,
    )
    .unwrap();

    assert!(page.pixels().any(|p| p.0 == [90, 90, 90, 255]), "整窗那一帧要贴在页面上");
    assert!(
        !page.pixels().any(|p| p.0 == [40, 40, 40, 255]),
        "有整窗时不该再贴裁图——否则区域框的位置就没有参照物了"
    );
    assert_eq!(
        page.get_pixel(
            (MARGIN + region.x) as u32,
            (MARGIN + HEADER_H + HEADER_GAP + region.y) as u32
        )
        .0,
        REGION_LINE,
        "区域框要画在「区域相对窗口的偏移」处"
    );
}

#[test]
fn a_region_outside_the_window_is_clipped_away() {
    let Some(font) = font() else { return };
    let crop = solid(120, 60, 40);
    let window_shot = solid(400, 300, 90);
    let page = annotate_step(
        &Step {
            label: "区域跑到窗口外",
            stamp: "t",
            region: Rect { x: 900, y: 900, width: 120, height: 60 },
            frame: &crop,
            window: Some((&window_shot, Rect { x: 0, y: 0, width: 400, height: 300 })),
            text_boxes: &[],
            icon: None,
        },
        &font,
    );
    assert!(page.is_ok(), "区域整块落在窗口外时应该被裁掉，而不是出错");
}

/// 图标命中要画出蓝框（与文字框的绿框区分开）。
#[test]
fn an_icon_hit_is_drawn_as_its_own_box() {
    let Some(font) = font() else { return };
    let crop = solid(200, 120, 40);
    let hit = IconHit {
        bounds: Rect { x: 20, y: 30, width: 32, height: 32 },
        score: 0.987,
        template: "通讯录.png".into(),
    };
    let page = annotate_step(
        &Step {
            label: "导航图标搜索区",
            stamp: "t",
            region: Rect { x: 0, y: 0, width: 200, height: 120 },
            frame: &crop,
            window: None,
            text_boxes: &[],
            icon: Some(&hit),
        },
        &font,
    )
    .unwrap();

    assert_eq!(
        page.get_pixel((MARGIN + 20) as u32, (MARGIN + HEADER_H + HEADER_GAP + 30) as u32).0,
        ICON_LINE,
        "命中框要画在帧图像坐标处"
    );
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
            window: None,
            text_boxes: &list,
            icon: None,
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
            window: None,
            text_boxes: &[],
            icon: None,
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