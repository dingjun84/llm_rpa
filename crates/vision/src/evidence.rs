//! 审计证据的脱敏。
//!
//! 依据 `docs/architecture.md` §7：失败截图必须**局部裁切并脱敏**，
//! 不允许把整屏原图留存下来。
//!
//! 本模块产出的是 PNG 字节，交给 `storage::EvidenceStore` 落盘；
//! 视觉层不依赖存储层，避免层次倒置。

use automation_core::{Rect, Screenshot};

use crate::pixels::{crop, encode_png, to_rgba};
use crate::VisionResult;

/// 脱敏方案：只保留 `keep` 区域，并把 `mask` 中的区域涂成纯色。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactionPlan {
    /// 需要保留的局部区域。
    pub keep: Rect,
    /// 需要遮盖的区域（相对 `keep` 的坐标），例如联系人姓名、消息正文。
    pub mask: Vec<Rect>,
    /// 遮盖色，默认不透明中灰。
    pub mask_color: [u8; 4],
}

impl RedactionPlan {
    pub fn keep_only(keep: Rect) -> Self {
        Self { keep, mask: Vec::new(), mask_color: [128, 128, 128, 255] }
    }

    pub fn with_mask(mut self, mask: impl IntoIterator<Item = Rect>) -> Self {
        self.mask = mask.into_iter().collect();
        self
    }
}

/// 按方案裁切并遮盖，返回可直接落盘的 PNG 字节。
pub fn redact(screenshot: &Screenshot, plan: &RedactionPlan) -> VisionResult<Vec<u8>> {
    let cropped = crop(screenshot, plan.keep)?;
    let mut image = to_rgba(&cropped)?;

    for region in &plan.mask {
        let (x, y, width, height) = (
            region.x.max(0) as u32,
            region.y.max(0) as u32,
            region.width.max(0) as u32,
            region.height.max(0) as u32,
        );
        for px in x..(x + width).min(image.width()) {
            for py in y..(y + height).min(image.height()) {
                image.put_pixel(px, py, image::Rgba(plan.mask_color));
            }
        }
    }

    encode_png(&image)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn sample(width: u32, height: u32) -> Screenshot {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            pixels.extend_from_slice(&[10, 20, 30, 255]);
        }
        Screenshot {
            fingerprint: "origin".into(),
            pixels,
            width,
            height,
            captured_at: SystemTime::now(),
        }
    }

    #[test]
    fn redaction_keeps_only_the_requested_region() {
        let shot = sample(20, 20);
        let png = redact(&shot, &RedactionPlan::keep_only(Rect { x: 4, y: 4, width: 6, height: 6 }))
            .unwrap();

        let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(decoded.width(), 6);
        assert_eq!(decoded.height(), 6);
    }

    #[test]
    fn masked_pixels_are_replaced_with_the_mask_colour() {
        let shot = sample(20, 20);
        let plan = RedactionPlan::keep_only(Rect { x: 0, y: 0, width: 8, height: 8 })
            .with_mask([Rect { x: 0, y: 0, width: 4, height: 4 }]);
        let png = redact(&shot, &plan).unwrap();

        let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(decoded.get_pixel(0, 0).0, [128, 128, 128, 255]);
        assert_eq!(decoded.get_pixel(3, 3).0, [128, 128, 128, 255]);
        // 未遮盖区域保留原始内容（BGRA 10,20,30 → RGBA 30,20,10）。
        assert_eq!(decoded.get_pixel(5, 5).0, [30, 20, 10, 255]);
    }

    #[test]
    fn mask_regions_outside_the_crop_are_ignored_safely() {
        let shot = sample(8, 8);
        let plan = RedactionPlan::keep_only(Rect { x: 0, y: 0, width: 4, height: 4 })
            .with_mask([Rect { x: 100, y: 100, width: 50, height: 50 }]);
        assert!(redact(&shot, &plan).is_ok());
    }

    #[test]
    fn redaction_refuses_a_region_outside_the_screenshot() {
        let shot = sample(8, 8);
        let plan = RedactionPlan::keep_only(Rect { x: 4, y: 4, width: 8, height: 8 });
        assert!(redact(&shot, &plan).is_err());
    }
}
