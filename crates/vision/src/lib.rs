//! 本地视觉层：局部裁切、预处理与离线 OCR。
//!
//! 铁律（见 `docs/architecture.md` §1、§6）：
//!
//! - 只处理本机内存中的局部截图，**不扫描整屏**；
//! - **禁止任何网络请求**：OCR 只能使用本机模型或本机进程；
//! - 识别结果必须带文字、边界框与置信度。

pub mod evidence;
pub mod layout;
pub mod ocr;
pub mod pixels;
pub mod render;
pub mod template;
pub mod text;

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
    /// 编码失败（`encode_png`）等**与某一张模板无关**的图像操作失败。
    ///
    /// 模板**读不进来**走 [`VisionError::TemplateUnreadable`]——那条要带上标签，
    /// 见它的说明。
    #[error("图像编解码失败：{0}")]
    Image(#[from] image::ImageError),
    /// 这张图标**读不出来**（文件损坏、根本不是图片、扩展名对不上）。
    ///
    /// 为什么不复用 [`VisionError::Image`]：一个图标名底下可以有多张图
    /// （选中 / 未选中 / 带气泡），只说"图像编解码失败"没法告诉人是**哪一张**坏了——
    /// 而修的时候正是要精确到那一张。尺寸越界的报错一直是带标签的，这里补齐。
    #[error("图标模板「{label}」读不出来：{reason}")]
    TemplateUnreadable { label: String, reason: String },
    #[error("裁切区域超出图像范围")]
    CropOutOfBounds,
    #[error(
        "图标模板「{label}」只有 {width}x{height} 像素，太小了：\
         请把整个图标框进去（最小 {MIN_TEMPLATE_SIDE}x{MIN_TEMPLATE_SIDE}）"
    )]
    TemplateTooSmall { label: String, width: u32, height: u32 },
    #[error(
        "图标模板「{label}」有 {width}x{height} 像素，太大了：它看起来不是一个小图标\
         （上限 {MAX_TEMPLATE_SIDE}x{MAX_TEMPLATE_SIDE}）"
    )]
    TemplateTooLarge { label: String, width: u32, height: u32 },
    #[error("本地 OCR 进程启动失败：{0}")]
    OcrSpawn(String),
    #[error("本地 OCR 进程超时（{0:?}）")]
    OcrTimeout(Duration),
    #[error("本地 OCR 输出无法解析：{0}")]
    OcrParse(String),
    #[error("文件或进程 IO 失败：{0}")]
    Io(#[from] std::io::Error),
    /// 本机找不到能写中文的字体。
    ///
    /// 这**不是**"图渲染失败"的笼统错误：它单列出来，是因为处置办法完全不同——
    /// 去装一个中文字体（或把候选路径补进 [`text`]），而不是去查识别为什么不准。
    #[error("找不到可用的中文字体，过程诊断图上的文字写不出来。试过：{tried}")]
    FontUnavailable { tried: String },
}

impl From<VisionError> for AutomationError {
    fn from(err: VisionError) -> Self {
        match err {
            VisionError::OcrTimeout(_) => AutomationError::Timeout(err.to_string()),
            VisionError::PixelBufferMismatch { .. }
            | VisionError::InvalidDimensions
            | VisionError::CropOutOfBounds
            // 模板尺寸不对、图读不出来都是**配置问题**，只有人能修：报 NeedsHumanReview
            // 会让任务停下来并说清是哪一张模板、多大、上限多少，
            // 而不是变成一个看起来像"识别不准"的模糊失败。
            | VisionError::TemplateTooSmall { .. }
            | VisionError::TemplateTooLarge { .. }
            | VisionError::TemplateUnreadable { .. } => AutomationError::NeedsHumanReview(err.to_string()),
            other => AutomationError::Platform(other.to_string()),
        }
    }
}

pub use evidence::{redact, RedactionPlan};
pub use ocr::{ExternalOcr, UnconfiguredOcr};
pub use pixels::{binarize, crop, decode_png, downscale_to_max_width, encode_png, to_grayscale, to_rgba};
pub use layout::{detect_three_pane_splits, suggested_nav_strip_width, ThreePaneSplits};
pub use template::{
    crop_template, load_icon_template, match_template, TemplateLocator, MAX_TEMPLATE_SIDE,
    MIN_TEMPLATE_SIDE,
};
