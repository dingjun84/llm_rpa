//! 以窗口为基准的相对区域。
//!
//! 依据 `docs/architecture.md` §6.1：优先使用经过标定的局部区域，不扫描整屏。
//! 因此核心层只持有相对窗口的比例区域，由本模块换算为屏幕坐标。

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ports::{Point, Rect};

#[derive(Debug, Error, PartialEq)]
pub enum RegionError {
    #[error("相对区域取值必须在 0.0–1.0 之间：{0:?}")]
    OutOfRange(RelativeRegion),
    #[error("相对区域必须完全落在窗口内：{0:?}")]
    OutsideWindow(RelativeRegion),
    #[error("相对落点取值必须在 0.0–1.0 之间：{0:?}")]
    PointOutOfRange(RelativePoint),
}

/// 相对某个矩形内部的比例落点，取值 0.0–1.0。
///
/// 有些动作只需要一个**落点**而不需要一整块区域（最典型的就是滚动：
/// 滚轮事件发到哪里决定了滚的是哪个列表）。落点同样必须是比例而不是像素——
/// 写死像素值换台机器、换个窗口大小就落到别的地方去了。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RelativePoint {
    pub x: f32,
    pub y: f32,
}

impl RelativePoint {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub fn validate(&self) -> Result<(), RegionError> {
        let in_range = |v: f32| (0.0..=1.0).contains(&v);
        if !in_range(self.x) || !in_range(self.y) {
            return Err(RegionError::PointOutOfRange(*self));
        }
        Ok(())
    }

    /// 换算为屏幕坐标，**保证落在矩形内**（含边界）。
    ///
    /// 按 `width - 1` 而不是 `width` 缩放：比例取到 1.0 时应当落在最后一个像素上，
    /// 而不是矩形右边界之外那一列。差一像素对滚动无所谓，但"落点在区域外"这种事
    /// 一旦发生，表现出来就是"滚了半天没反应"，属于本项目最难查的一类现象，
    /// 所以在这里直接掐掉。
    pub fn resolve(&self, rect: Rect) -> Point {
        let span_x = rect.width.saturating_sub(1).max(0) as f32;
        let span_y = rect.height.saturating_sub(1).max(0) as f32;
        Point {
            x: rect.x + (span_x * self.x).round() as i32,
            y: rect.y + (span_y * self.y).round() as i32,
        }
    }
}

/// 相对窗口的比例区域，取值 0.0–1.0。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RelativeRegion {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl RelativeRegion {
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self { x, y, width, height }
    }

    pub fn validate(&self) -> Result<(), RegionError> {
        let in_range = |v: f32| (0.0..=1.0).contains(&v);
        if !in_range(self.x)
            || !in_range(self.y)
            || !in_range(self.width)
            || !in_range(self.height)
            || self.x + self.width > 1.0
            || self.y + self.height > 1.0
        {
            return Err(RegionError::OutOfRange(*self));
        }
        if self.width <= 0.0 || self.height <= 0.0 {
            return Err(RegionError::OutOfRange(*self));
        }
        Ok(())
    }

    /// 换算为窗口内的屏幕坐标区域。
    pub fn resolve(&self, window: Rect) -> Rect {
        Rect {
            x: window.x + (window.width as f32 * self.x).round() as i32,
            y: window.y + (window.height as f32 * self.y).round() as i32,
            width: (window.width as f32 * self.width).round() as i32,
            height: (window.height as f32 * self.height).round() as i32,
        }
    }

    /// 校验换算结果确实落在窗口内；调用方在截图前必须通过此项检查。
    pub fn resolve_within(&self, window: Rect) -> Result<Rect, RegionError> {
        self.validate()?;
        let resolved = self.resolve(window);
        let fits = resolved.x >= window.x
            && resolved.y >= window.y
            && resolved.x + resolved.width <= window.x + window.width
            && resolved.y + resolved.height <= window.y + window.height;
        if !fits {
            return Err(RegionError::OutsideWindow(*self));
        }
        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> Rect {
        Rect { x: 100, y: 50, width: 1000, height: 800 }
    }

    #[test]
    fn resolves_relative_region_to_screen_coordinates() {
        let region = RelativeRegion::new(0.25, 0.5, 0.5, 0.25);
        assert_eq!(
            region.resolve(window()),
            Rect { x: 350, y: 450, width: 500, height: 200 }
        );
    }

    #[test]
    fn rejects_region_that_escapes_the_window() {
        let region = RelativeRegion::new(0.8, 0.0, 0.5, 0.5);
        assert_eq!(region.validate(), Err(RegionError::OutOfRange(region)));
    }

    #[test]
    fn rejects_zero_sized_region() {
        let region = RelativeRegion::new(0.1, 0.1, 0.0, 0.2);
        assert_eq!(region.validate(), Err(RegionError::OutOfRange(region)));
    }

    #[test]
    fn resolves_within_window_for_calibrated_region() {
        let region = RelativeRegion::new(0.0, 0.0, 1.0, 1.0);
        assert_eq!(region.resolve_within(window()).unwrap(), window());
    }

    #[test]
    fn resolves_relative_point_inside_the_rect() {
        let rect = Rect { x: 100, y: 50, width: 101, height: 51 };
        // 中心：按 width-1 缩放 ⇒ (100+50, 50+25)。
        assert_eq!(RelativePoint::new(0.5, 0.5).resolve(rect), Point { x: 150, y: 75 });
    }

    #[test]
    fn relative_point_at_ratio_one_stays_on_the_last_pixel() {
        let rect = Rect { x: 10, y: 20, width: 100, height: 40 };
        // 关键性质：比例取满也不能落到矩形右/下边界之外。
        assert_eq!(
            RelativePoint::new(1.0, 1.0).resolve(rect),
            Point { x: 109, y: 59 }
        );
    }

    #[test]
    fn rejects_relative_point_out_of_range() {
        let point = RelativePoint::new(1.2, 0.5);
        assert_eq!(point.validate(), Err(RegionError::PointOutOfRange(point)));
    }
}
