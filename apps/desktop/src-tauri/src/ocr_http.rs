//! 远程 OCR：和 YOLO 共用 `yolo_api_base`，调用 `POST {base}/ocr`。
//!
//! 成功响应与本机 `macosocr` / `winocr` 相同，是 JSON 数组：
//! `[{"text":"张三","x":12,"y":40,"w":96,"h":24,"confidence":0.98}]`
//! 坐标是所传图片的像素，左上角为原点。

use std::time::Duration;

use automation_core::{AutomationError, LocalOcr, Screenshot, TextBox};

/// 调用 `POST {base}/ocr` 的识别器。
#[derive(Debug, Clone)]
pub struct HttpOcr {
    pub api_base: String,
    pub timeout: Duration,
    /// 识别前放大倍数，坐标由服务端换回原图。与服务默认一致，默认 2。
    pub upscale: f32,
    /// 低于此分的行不返回。与服务默认一致，默认 0.3。
    pub text_score: f32,
}

impl HttpOcr {
    pub fn new(api_base: impl Into<String>, timeout: Duration) -> Self {
        Self {
            api_base: api_base.into().trim().trim_end_matches('/').to_string(),
            timeout,
            upscale: 2.0,
            text_score: 0.3,
        }
    }
}

impl LocalOcr for HttpOcr {
    fn recognize(&self, image: &Screenshot) -> Result<Vec<TextBox>, AutomationError> {
        Ok(self.recognize_with_raw(image)?.0)
    }

    fn recognize_with_raw(
        &self,
        image: &Screenshot,
    ) -> Result<(Vec<TextBox>, String), AutomationError> {
        if self.api_base.is_empty() {
            return Err(AutomationError::NeedsHumanReview(
                "检测服务地址为空，OCR 与图标检测共用这一项".into(),
            ));
        }
        let rgba = vision::pixels::to_rgba(image)
            .map_err(|err| AutomationError::Platform(format!("截图转 RGBA 失败：{err}")))?;
        let png = vision::pixels::encode_png(&rgba)
            .map_err(|err| AutomationError::Platform(format!("编码 PNG 失败：{err}")))?;

        let url = format!(
            "{}/ocr?upscale={}&text_score={}",
            self.api_base, self.upscale, self.text_score
        );
        let response = ureq::post(&url)
            .timeout(self.timeout)
            .set("Content-Type", "image/png")
            .send_bytes(&png)
            .map_err(|err| AutomationError::Platform(format!("OCR 请求失败（{url}）：{err}")))?;

        let status = response.status();
        let body = response
            .into_string()
            .map_err(|err| AutomationError::Platform(format!("读取 OCR 响应失败：{err}")))?;
        if !(200..300).contains(&status) {
            return Err(AutomationError::Platform(format!(
                "OCR HTTP {status}：{}",
                trunc(&body, 200)
            )));
        }
        let trimmed = body.trim();
        if trimmed.starts_with('{') {
            return Err(AutomationError::Platform(format!(
                "OCR 服务返回的不是文字框数组：{}",
                trunc(trimmed, 200)
            )));
        }
        let boxes = vision::ocr::parse_ocr_json(trimmed).map_err(AutomationError::from)?;
        Ok((boxes, body))
    }
}

fn trunc(s: &str, max: usize) -> String {
    let mut t: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        t.push('…');
    }
    t
}
