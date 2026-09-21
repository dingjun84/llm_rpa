//! 从整窗截图估计三栏布局的竖向分界（左导航 / 列表 / 主内容）。
//!
//! 不依赖 OpenCV：对每一列求平均颜色，再看相邻列色差，找最强的两条竖边。

use automation_core::Screenshot;

use crate::{VisionError, VisionResult};

/// 三栏竖向分界（相对窗口宽度 0–1，以及像素坐标）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThreePaneSplits {
    /// 左导航右边界（相对宽）。
    pub nav_right: f32,
    /// 列表右边界 / 主内容左边界（相对宽）。
    pub list_right: f32,
    pub nav_right_px: i32,
    pub list_right_px: i32,
}

/// 估计三栏分界。要求画面里至少有两条明显的竖向色差边。
pub fn detect_three_pane_splits(frame: &Screenshot) -> VisionResult<ThreePaneSplits> {
    if frame.width < 32 || frame.height < 32 {
        return Err(VisionError::InvalidDimensions);
    }
    let w = frame.width as usize;
    let h = frame.height as usize;
    let expected = w * h * 4;
    if frame.pixels.len() != expected {
        return Err(VisionError::PixelBufferMismatch {
            expected,
            actual: frame.pixels.len(),
        });
    }

    // 每列平均 BGR（跳过 A）。
    let mut col_b = vec![0.0f64; w];
    let mut col_g = vec![0.0f64; w];
    let mut col_r = vec![0.0f64; w];
    let inv_h = 1.0 / h as f64;
    for y in 0..h {
        let row = y * w * 4;
        for x in 0..w {
            let i = row + x * 4;
            col_b[x] += frame.pixels[i] as f64;
            col_g[x] += frame.pixels[i + 1] as f64;
            col_r[x] += frame.pixels[i + 2] as f64;
        }
    }
    for x in 0..w {
        col_b[x] *= inv_h;
        col_g[x] *= inv_h;
        col_r[x] *= inv_h;
    }

    // 相邻列色差；两端各留 2% 边距，避免窗口阴影。
    let margin = (w / 50).max(2);
    let mut edges: Vec<(usize, f64)> = Vec::with_capacity(w);
    for x in margin..(w - margin) {
        let db = col_b[x] - col_b[x - 1];
        let dg = col_g[x] - col_g[x - 1];
        let dr = col_r[x] - col_r[x - 1];
        let score = (db * db + dg * dg + dr * dr).sqrt();
        edges.push((x, score));
    }

    // 非极大抑制：在模板宽度量级的邻域里只留峰。
    let radius = (w / 40).max(8);
    let mut peaks: Vec<(usize, f64)> = Vec::new();
    for &(x, score) in &edges {
        let local_max = edges
            .iter()
            .filter(|(ox, _)| ox.abs_diff(x) <= radius)
            .map(|(_, s)| *s)
            .fold(0.0_f64, f64::max);
        if (score - local_max).abs() < 1e-9 {
            peaks.push((x, score));
        }
    }
    peaks.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // 至少两条峰；取 x 较小的两条作为 nav / list 分界（再按 x 排序）。
    if peaks.len() < 2 {
        return Err(VisionError::InvalidDimensions);
    }
    let mut top2: Vec<(usize, f64)> = peaks.into_iter().take(8).collect();
    // 在左侧 45% 内优先找导航边，在 20%–75% 找列表边。
    let nav_limit = (w as f32 * 0.45) as usize;
    let list_lo = (w as f32 * 0.15) as usize;
    let list_hi = (w as f32 * 0.80) as usize;

    let mut nav = top2
        .iter()
        .filter(|(x, _)| *x <= nav_limit)
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(x, _)| *x);
    let mut list = top2
        .iter()
        .filter(|(x, _)| *x >= list_lo && *x <= list_hi && Some(*x) != nav)
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(x, _)| *x);

    // 回退：按 x 排序取前两强峰。
    if nav.is_none() || list.is_none() {
        top2.sort_by_key(|(x, _)| *x);
        if top2.len() >= 2 {
            nav = Some(top2[0].0);
            list = Some(top2[1].0);
        }
    }
    let (nav_x, list_x) = match (nav, list) {
        (Some(a), Some(b)) if a < b => (a, b),
        (Some(a), Some(b)) if b < a => (b, a),
        _ => return Err(VisionError::InvalidDimensions),
    };

    Ok(ThreePaneSplits {
        nav_right: nav_x as f32 / w as f32,
        list_right: list_x as f32 / w as f32,
        nav_right_px: nav_x as i32,
        list_right_px: list_x as i32,
    })
}

/// 建议的相对导航条宽度（在分界上加一点余量，夹在 5%–35%）。
pub fn suggested_nav_strip_width(splits: ThreePaneSplits) -> f32 {
    (splits.nav_right + 0.01).clamp(0.05, 0.35)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::frame_of;

    #[test]
    fn finds_two_vertical_color_boundaries() {
        // 三块纯色：左 40px 深蓝、中 80px 灰、右其余浅绿。
        let frame = frame_of(200, 100, |x, _| {
            if x < 40 {
                [180, 40, 20, 255]
            } else if x < 120 {
                [200, 200, 200, 255]
            } else {
                [40, 180, 80, 255]
            }
        });
        let splits = detect_three_pane_splits(&frame).unwrap();
        assert!((splits.nav_right_px - 40).abs() <= 2, "nav {:?}", splits);
        assert!((splits.list_right_px - 120).abs() <= 2, "list {:?}", splits);
    }
}
