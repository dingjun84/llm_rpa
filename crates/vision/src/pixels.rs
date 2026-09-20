//! 像素与图像之间的转换、裁切与预处理。
//!
//! 约定：`Screenshot::pixels` 为 **BGRA**、自上而下的 32 位像素。

use automation_core::{Rect, Screenshot};
use image::{GrayImage, RgbaImage};
use sha2::{Digest, Sha256};
use std::borrow::Cow;

use crate::{VisionError, VisionResult};

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn fingerprint(pixels: &[u8], width: u32, height: u32) -> String {
    let mut hasher = Sha256::new();
    hasher.update(width.to_le_bytes());
    hasher.update(height.to_le_bytes());
    hasher.update(pixels);
    hex_lower(&hasher.finalize())
}

/// 把 BGRA 截图转换为 RGBA 图像。
pub fn to_rgba(screenshot: &Screenshot) -> VisionResult<RgbaImage> {
    if screenshot.width == 0 || screenshot.height == 0 {
        return Err(VisionError::InvalidDimensions);
    }
    let expected = screenshot.width as usize * screenshot.height as usize * 4;
    if screenshot.pixels.len() != expected {
        return Err(VisionError::PixelBufferMismatch {
            expected,
            actual: screenshot.pixels.len(),
        });
    }
    let mut buffer = screenshot.pixels.clone();
    for pixel in buffer.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    RgbaImage::from_raw(screenshot.width, screenshot.height, buffer)
        .ok_or(VisionError::InvalidDimensions)
}

/// 裁切出局部区域，并重新计算指纹。
///
/// 这是"只处理局部区域"的落地点：调用方永远不该把整屏交给后续步骤。
pub fn crop(screenshot: &Screenshot, region: Rect) -> VisionResult<Screenshot> {
    let image = to_rgba(screenshot)?;
    if region.is_degenerate() {
        return Err(VisionError::CropOutOfBounds);
    }
    let right = region.x + region.width;
    let bottom = region.y + region.height;
    if region.x < 0
        || region.y < 0
        || right > screenshot.width as i32
        || bottom > screenshot.height as i32
    {
        return Err(VisionError::CropOutOfBounds);
    }

    let cropped = image::imageops::crop_imm(
        &image,
        region.x as u32,
        region.y as u32,
        region.width as u32,
        region.height as u32,
    )
    .to_image();

    // RGBA -> BGRA，回到与平台层一致的表示。
    let mut pixels = cropped.into_raw();
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let width = region.width as u32;
    let height = region.height as u32;
    Ok(Screenshot {
        fingerprint: fingerprint(&pixels, width, height),
        pixels,
        width,
        height,
        captured_at: screenshot.captured_at,
    })
}

pub fn to_grayscale(image: &RgbaImage) -> GrayImage {
    image::imageops::grayscale(image)
}

/// 按阈值二值化：低于阈值置黑，其余置白。
pub fn binarize(gray: &GrayImage, threshold: u8) -> GrayImage {
    let mut output = gray.clone();
    for pixel in output.pixels_mut() {
        pixel.0[0] = if pixel.0[0] < threshold { 0 } else { 255 };
    }
    output
}

pub fn encode_png(image: &RgbaImage) -> VisionResult<Vec<u8>> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    image.write_to(&mut buffer, image::ImageFormat::Png)?;
    Ok(buffer.into_inner())
}

/// 按最大宽度等比缩小；宽度已在限制内时**原样借用**，不做任何复制。
///
/// 用途是压小经 IPC 送进界面的预览图：一张 4K 窗口截图编码成 PNG 再转 base64
/// 会膨胀到几十 MB，webview 渲染起来又慢又占内存。
///
/// 缩小不影响标定精度——界面上的区域叠加层用的是**百分比**，
/// 与图像的实际像素尺寸无关。
pub fn downscale_to_max_width(image: &RgbaImage, max_width: u32) -> Cow<'_, RgbaImage> {
    if max_width == 0 || image.width() <= max_width {
        return Cow::Borrowed(image);
    }
    let scale = max_width as f32 / image.width() as f32;
    // 至少留 1 像素高：极端细长的图不能缩成 0，否则后续编码会失败。
    let height = ((image.height() as f32 * scale).round() as u32).max(1);
    Cow::Owned(image::imageops::resize(
        image,
        max_width,
        height,
        image::imageops::FilterType::Triangle,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn sample(width: u32, height: u32) -> Screenshot {
        // 构造可预测的 BGRA 数据。
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[
                    (x * 4) as u8,
                    (y * 4) as u8,
                    128,
                    255,
                ]);
            }
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
    fn converts_bgra_to_rgba_by_swapping_channels() {
        let shot = sample(2, 1);
        let rgba = to_rgba(&shot).unwrap();
        // 原始像素 (B,G,R,A) = (0,0,128,255) → RGBA 应为 (128,0,0,255)
        assert_eq!(rgba.get_pixel(0, 0).0, [128, 0, 0, 255]);
        // 第二像素 (B,G,R,A) = (4,0,128,255)
        assert_eq!(rgba.get_pixel(1, 0).0, [128, 0, 4, 255]);
    }

    #[test]
    fn rejects_a_pixel_buffer_that_does_not_match_the_dimensions() {
        let mut shot = sample(2, 2);
        shot.pixels.truncate(8);
        assert!(matches!(
            to_rgba(&shot),
            Err(VisionError::PixelBufferMismatch { .. })
        ));
    }

    #[test]
    fn crop_returns_only_the_requested_region() {
        let shot = sample(8, 8);
        let cropped = crop(&shot, Rect { x: 2, y: 3, width: 4, height: 2 }).unwrap();

        assert_eq!(cropped.width, 4);
        assert_eq!(cropped.height, 2);
        assert_eq!(cropped.pixels.len(), 4 * 2 * 4);
        // 裁切后左上角对应原图 (2,3)：B = 2*4 = 8
        assert_eq!(cropped.pixels[0], 8);
    }

    #[test]
    fn crop_recomputes_the_fingerprint() {
        let shot = sample(8, 8);
        let a = crop(&shot, Rect { x: 0, y: 0, width: 4, height: 4 }).unwrap();
        let b = crop(&shot, Rect { x: 4, y: 4, width: 4, height: 4 }).unwrap();
        assert_ne!(a.fingerprint, b.fingerprint);
        assert_ne!(a.fingerprint, shot.fingerprint);
    }

    #[test]
    fn crop_refuses_to_leave_the_image() {
        let shot = sample(4, 4);
        assert!(matches!(
            crop(&shot, Rect { x: 2, y: 2, width: 4, height: 4 }),
            Err(VisionError::CropOutOfBounds)
        ));
        assert!(matches!(
            crop(&shot, Rect { x: -1, y: 0, width: 2, height: 2 }),
            Err(VisionError::CropOutOfBounds)
        ));
    }

    #[test]
    fn binarize_splits_pixels_at_the_threshold() {
        let shot = sample(4, 1);
        let gray = to_grayscale(&to_rgba(&shot).unwrap());
        let binary = binarize(&gray, 128);
        assert!(binary.pixels().all(|p| p.0[0] == 0 || p.0[0] == 255));
    }

    #[test]
    fn encodes_a_png_that_carries_the_png_signature() {
        let shot = sample(4, 4);
        let png = encode_png(&to_rgba(&shot).unwrap()).unwrap();
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    }

    #[test]
    fn downscale_keeps_the_aspect_ratio_and_the_width_limit() {
        let image = to_rgba(&sample(800, 600)).unwrap();
        let scaled = downscale_to_max_width(&image, 400);

        assert_eq!(scaled.width(), 400);
        assert_eq!(scaled.height(), 300, "高度应等比缩放");
    }

    #[test]
    fn downscale_leaves_a_small_enough_image_untouched() {
        let image = to_rgba(&sample(320, 200)).unwrap();
        let scaled = downscale_to_max_width(&image, 1280);

        assert_eq!(scaled.width(), 320);
        assert_eq!(scaled.height(), 200);
        // 没触发缩放时应是借用，不是复制。
        assert!(
            matches!(scaled, Cow::Borrowed(_)),
            "宽度已在限制内时不应复制整张图"
        );
    }

    #[test]
    fn downscale_never_produces_a_zero_height() {
        // 极端细长的图：宽 4000、高 1，缩到宽 2 时高度四舍五入会变成 0，
        // 必须兜底到 1，否则后续 PNG 编码会失败。
        let image = to_rgba(&sample(4000, 1)).unwrap();
        let scaled = downscale_to_max_width(&image, 2);

        assert_eq!(scaled.width(), 2);
        assert_eq!(scaled.height(), 1);
        assert!(encode_png(&scaled).is_ok(), "缩放结果必须仍可编码");
    }

    #[test]
    fn downscale_is_a_noop_for_a_zero_limit() {
        // `max_width = 0` 视为"未配置"，不能把图缩成 0 宽。
        let image = to_rgba(&sample(64, 32)).unwrap();
        let scaled = downscale_to_max_width(&image, 0);

        assert_eq!(scaled.width(), 64);
        assert_eq!(scaled.height(), 32);
    }
}
