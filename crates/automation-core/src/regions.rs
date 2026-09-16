//! 以窗口为基准的相对区域。
//!
//! 依据 `docs/architecture.md` §6.1：优先使用经过标定的局部区域，不扫描整屏。
//! 因此核心层只持有相对窗口的比例区域，由本模块换算为屏幕坐标。

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ports::Rect;

#[derive(Debug, Error, PartialEq)]
pub enum RegionError {
    #[error("相对区域取值必须在 0.0–1.0 之间：{0:?}")]
    OutOfRange(RelativeRegion),
    #[error("相对区域必须完全落在窗口内：{0:?}")]
    OutsideWindow(RelativeRegion),
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
}
