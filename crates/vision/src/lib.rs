//! 本地视觉层：局部裁切、预处理与离线 OCR。
//!
//! 铁律（见 `docs/architecture.md` §1、§6）：
//!
//! - 只处理本机内存中的局部截图，**不扫描整屏**；
//! - **禁止任何网络请求**：OCR 只能使用本机模型或本机进程；
//! - 识别结果必须带文字、边界框与置信度。

pub mod evidence;
pub mod ocr;
pub mod pixels;

use std::time::Duration;

use automation_core::AutomationError;
use thiserror::Error;

pub type VisionResult<T> = Result<T, VisionError>;

#[derive(Debug, Error)]
pub enum VisionError {
    #[error("像素缓冲区长度与尺寸不匹配：期望 {expected} 字节，实际 {actual} 字节")]
    PixelBufferMismatch { expected: usize, actual: usize },
    #[error("图像尺寸无效")]
    InvalidDimensions,
    #[error("图像编解码失败：{0}")]
    Image(#[from] image::ImageError),
    #[error("裁切区域超出图像范围")]
    CropOutOfBounds,
    #[error("本地 OCR 进程启动失败：{0}")]
    OcrSpawn(String),
    #[error("本地 OCR 进程超时（{0:?}）")]
    OcrTimeout(Duration),
    #[error("本地 OCR 输出无法解析：{0}")]
    OcrParse(String),
    #[error("文件或进程 IO 失败：{0}")]
    Io(#[from] std::io::Error),
}

impl From<VisionError> for AutomationError {
    fn from(err: VisionError) -> Self {
        match err {
            VisionError::OcrTimeout(_) => AutomationError::Timeout(err.to_string()),
            VisionError::PixelBufferMismatch { .. }
            | VisionError::InvalidDimensions
            | VisionError::CropOutOfBounds => AutomationError::NeedsHumanReview(err.to_string()),
            other => AutomationError::Platform(other.to_string()),
        }
    }
}

pub use evidence::{redact, RedactionPlan};
pub use ocr::{ExternalOcr, UnconfiguredOcr};
pub use pixels::{binarize, crop, downscale_to_max_width, encode_png, to_grayscale, to_rgba};
