//! 远程 YOLO HTTP 适配器：实现 `automation_core::YoloDetector`。
//!
//! ★ 编码全分辨率 PNG（禁止预览压缩图）；坐标换算留给编排层
//!   [`automation_core::shot_point_to_screen`]。

use std::time::Duration;

use automation_core::{AutomationError, Screenshot, YoloDetection, YoloDetector};
use serde::Deserialize;

/// 调用 `POST {base}/predict` 的检测器。
#[derive(Debug, Clone)]
pub struct HttpYoloDetector {
    pub api_base: String,
}

impl HttpYoloDetector {
    pub fn new(api_base: impl Into<String>) -> Self {
        Self {
            api_base: api_base.into().trim().trim_end_matches('/').to_string(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct PredictBody {
    success: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    detections: Vec<PredictDet>,
}

#[derive(Debug, Deserialize)]
struct PredictDet {
    #[serde(default)]
    class_id: i32,
    class_name: String,
    conf: f32,
    center: [f32; 2],
    #[serde(default)]
    xyxy: [f32; 4],
}

impl YoloDetector for HttpYoloDetector {
    fn detect(
        &self,
        frame: &Screenshot,
        conf: f32,
    ) -> Result<Vec<YoloDetection>, AutomationError> {
        if self.api_base.is_empty() {
            return Err(AutomationError::Platform("YOLO API 地址为空".into()));
        }
        let rgba = vision::pixels::to_rgba(frame)
            .map_err(|err| AutomationError::Platform(format!("截图转 RGBA 失败：{err}")))?;
        let png = vision::pixels::encode_png(&rgba)
            .map_err(|err| AutomationError::Platform(format!("编码 PNG 失败：{err}")))?;

        let conf = conf.clamp(0.0, 1.0);
        let url = format!(
            "{}/predict?conf={conf}&imgsz=1280",
            self.api_base
        );
        let response = ureq::post(&url)
            .timeout(Duration::from_secs(60))
            .set("Content-Type", "image/png")
            .send_bytes(&png)
            .map_err(|err| AutomationError::Platform(format!("YOLO 请求失败（{url}）：{err}")))?;

        let status = response.status();
        let body_text = response
            .into_string()
            .map_err(|err| AutomationError::Platform(format!("读取 YOLO 响应失败：{err}")))?;
        if !(200..300).contains(&status) {
            return Err(AutomationError::Platform(format!(
                "YOLO HTTP {status}：{}",
                trunc(&body_text, 200)
            )));
        }
        let body: PredictBody = serde_json::from_str(&body_text).map_err(|err| {
            AutomationError::Platform(format!(
                "解析 YOLO JSON 失败：{err}；原文前 200 字：{}",
                trunc(&body_text, 200)
            ))
        })?;
        if !body.success {
            return Err(AutomationError::Platform(
                body.error
                    .unwrap_or_else(|| "YOLO success=false".into()),
            ));
        }
        Ok(body
            .detections
            .into_iter()
            .map(|d| YoloDetection {
                class_id: d.class_id,
                class_name: d.class_name,
                conf: d.conf,
                center: d.center,
                xyxy: d.xyxy,
            })
            .collect())
    }
}

fn trunc(s: &str, max: usize) -> String {
    let mut t: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        t.push('…');
    }
    t
}
