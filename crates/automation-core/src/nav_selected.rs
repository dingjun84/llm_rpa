//! 企微左侧导航图标「选中态」判据（HSV 蓝占比）。
//!
//! ★ 判据只此一处：`nav_chat_icon` / `nav_contacts_icon` 共用同一套参数；
//! runner 只问 [`is_nav_icon_selected`]，不要在别处再写 H/S/V 或阈值。
//!
//! 定稿口径（与测量脚本 / 交接文档一致，2026-10-12）：
//! - 色相命中区间 **180°~250°**（只算蓝青，避开未读红点）
//! - S ≥ 60、V ≥ 50（滤掉灰态）
//! - 占比阈值 **0.35**；检测框 **inset=0**（整框参与）
//! - 像素格式与 [`crate::ports::Screenshot`] 一致：BGRA、自上而下

use crate::ports::{Rect, Screenshot};

/// 命中色相下界（度）。
pub const HUE_DEG_MIN: f32 = 180.0;
/// 命中色相上界（度）。
pub const HUE_DEG_MAX: f32 = 250.0;
/// 饱和度下限（0~255）。
pub const S_MIN: u8 = 60;
/// 亮度下限（0~255）。
pub const V_MIN: u8 = 50;
/// 选中判定：命中像素占比 ≥ 本阈值。
pub const SELECTED_RATIO: f32 = 0.35;

/// 框内「蓝且够饱和」像素占比；空框 / 越界框返回 0。
pub fn nav_icon_selected_ratio(shot: &Screenshot, bounds: Rect) -> f32 {
    let (hit, total) = count_hit_pixels(shot, bounds);
    if total == 0 {
        return 0.0;
    }
    hit as f32 / total as f32
}

/// 占比 ≥ [`SELECTED_RATIO`] 视为选中。
pub fn is_nav_icon_selected(shot: &Screenshot, bounds: Rect) -> bool {
    nav_icon_selected_ratio(shot, bounds) >= SELECTED_RATIO
}

fn count_hit_pixels(shot: &Screenshot, bounds: Rect) -> (usize, usize) {
    if bounds.is_degenerate() || shot.width == 0 || shot.height == 0 {
        return (0, 0);
    }
    let x0 = bounds.x.max(0) as u32;
    let y0 = bounds.y.max(0) as u32;
    let x1 = ((bounds.x + bounds.width) as u32).min(shot.width);
    let y1 = ((bounds.y + bounds.height) as u32).min(shot.height);
    if x0 >= x1 || y0 >= y1 {
        return (0, 0);
    }
    let stride = (shot.width as usize) * 4;
    let mut hit = 0usize;
    let mut total = 0usize;
    for y in y0..y1 {
        let row = (y as usize) * stride;
        for x in x0..x1 {
            let i = row + (x as usize) * 4;
            // BGRA
            let b = shot.pixels[i];
            let g = shot.pixels[i + 1];
            let r = shot.pixels[i + 2];
            total += 1;
            if pixel_is_hit(b, g, r) {
                hit += 1;
            }
        }
    }
    (hit, total)
}

/// BGR → 是否落在定稿命中条件（与 OpenCV HSV 度数口径对齐）。
fn pixel_is_hit(b: u8, g: u8, r: u8) -> bool {
    let (h_deg, s, v) = bgr_to_hsv_deg(b, g, r);
    if s < S_MIN || v < V_MIN {
        return false;
    }
    h_deg >= HUE_DEG_MIN && h_deg <= HUE_DEG_MAX
}

/// OpenCV 风格 HSV：H 用度数 0~360，S/V 0~255。
fn bgr_to_hsv_deg(b: u8, g: u8, r: u8) -> (f32, u8, u8) {
    let bf = b as f32;
    let gf = g as f32;
    let rf = r as f32;
    let max = bf.max(gf).max(rf);
    let min = bf.min(gf).min(rf);
    let delta = max - min;
    let v = max.round().clamp(0.0, 255.0) as u8;
    let s = if max <= 0.0 {
        0
    } else {
        ((delta / max) * 255.0).round().clamp(0.0, 255.0) as u8
    };
    if delta <= 0.0 {
        return (0.0, s, v);
    }
    let mut h = if (max - rf).abs() < f32::EPSILON {
        60.0 * (((gf - bf) / delta) % 6.0)
    } else if (max - gf).abs() < f32::EPSILON {
        60.0 * (((bf - rf) / delta) + 2.0)
    } else {
        60.0 * (((rf - gf) / delta) + 4.0)
    };
    if h < 0.0 {
        h += 360.0;
    }
    (h, s, v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::Screenshot;
    use std::time::SystemTime;

    fn solid_shot(w: u32, h: u32, b: u8, g: u8, r: u8) -> Screenshot {
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            pixels.extend_from_slice(&[b, g, r, 255]);
        }
        Screenshot {
            pixels,
            width: w,
            height: h,
            captured_at: SystemTime::UNIX_EPOCH,
            fingerprint: "test".into(),
        }
    }

    #[test]
    fn selected_blue_passes_threshold() {
        // 近似企微选中蓝：H≈214°, 高 S/V
        // BGR for ~214°: cyan-blue, e.g. B=240, G=160, R=40
        let shot = solid_shot(10, 10, 240, 160, 40);
        let bounds = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        };
        let ratio = nav_icon_selected_ratio(&shot, bounds);
        assert!(
            ratio >= SELECTED_RATIO,
            "expected selected blue ratio ≥ {SELECTED_RATIO}, got {ratio}"
        );
        assert!(is_nav_icon_selected(&shot, bounds));
    }

    #[test]
    fn gray_icon_is_unselected() {
        let shot = solid_shot(10, 10, 180, 180, 180);
        let bounds = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        };
        assert_eq!(nav_icon_selected_ratio(&shot, bounds), 0.0);
        assert!(!is_nav_icon_selected(&shot, bounds));
    }

    #[test]
    fn red_unread_not_counted_as_selected() {
        // 纯红：H≈0，不应进入 180–250
        let shot = solid_shot(8, 8, 40, 40, 220);
        let bounds = Rect {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
        };
        assert_eq!(nav_icon_selected_ratio(&shot, bounds), 0.0);
    }

    #[test]
    fn hue_of_selected_blue_near_214() {
        let (h, s, v) = bgr_to_hsv_deg(240, 160, 40);
        assert!(s >= S_MIN && v >= V_MIN, "s={s} v={v}");
        assert!(
            (200.0..=230.0).contains(&h),
            "expected ~214°, got {h}"
        );
    }
}
