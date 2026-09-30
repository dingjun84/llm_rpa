//! 远程 YOLO 检测：截目标窗口全分辨率图 → `POST /predict` → 屏幕坐标。
//!
//! ★ 坐标判据只用 [`crate::shot_point_to_screen`]（与图标点击同一处），
//!   禁止在本模块另写一套截图像素→屏幕换算。
//! ★ 预览图 `preview_data_url` 会压到 1280 宽，**不能**拿去喂 API；
//!   这里编码的是原始 `Screenshot` PNG。

use automation_core::{Point, Rect};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// 远程检测请求（界面草稿里的窗口定位参数 + API 参数）。
#[derive(Debug, Clone, Deserialize)]
pub struct YoloDetectRequest {
    pub window_class: String,
    pub wecom_exe: Option<String>,
    /// API 根地址，如 `http://192.168.1.22:8080`。
    pub api_base: String,
    /// 置信度阈值；缺省 0.25。
    #[serde(default)]
    pub conf: Option<f32>,
}

/// 单条检测（坐标已含截图像素中心与屏幕绝对坐标）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YoloDetectionView {
    pub class_id: i32,
    pub class_name: String,
    pub conf: f32,
    /// 原图像素中心 `[cx, cy]`。
    pub center: [f32; 2],
    /// `xyxy` 原图像素。
    pub xyxy: [f32; 4],
    /// 换算后的屏幕绝对坐标（可直接 `MoveMouseTo`）。
    pub screen: Point,
}

/// `yolo_detect_target_window` 的返回。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YoloDetectResult {
    pub window: Rect,
    pub shot_width: u32,
    pub shot_height: u32,
    pub count: u32,
    pub detections: Vec<YoloDetectionView>,
    /// 标注图 data URL（来自 `/predict?annotated=1`）；没有则为 `null`。
    pub annotated_image: Option<String>,
    /// 面向操作者的状态行。
    pub notice: String,
}

#[derive(Debug, Deserialize)]
struct PredictBody {
    success: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    count: u32,
    #[serde(default)]
    detections: Vec<PredictDet>,
    #[serde(default)]
    annotated_image: Option<String>,
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

/// 截目标窗口全分辨率图，调用远程 `/predict?annotated=1`，返回检测框与屏幕坐标。
#[tauri::command]
pub fn yolo_detect_target_window(request: YoloDetectRequest) -> Result<YoloDetectResult, String> {
    let class = request.window_class.trim().to_string();
    if class.is_empty() {
        return Err("窗口类名为空，无法定位目标窗口。先点「指认窗口」或手工填写类名。".into());
    }

    let base = request.api_base.trim().trim_end_matches('/').to_string();
    if base.is_empty() {
        return Err("API 地址为空。".into());
    }

    let conf = request.conf.unwrap_or(0.25).clamp(0.0, 1.0);

    #[cfg(any(windows, target_os = "macos"))]
    {
        let desktop = crate::desktop_for(&class, request.wecom_exe.as_deref());
        let (window, shot) = desktop
            .preview()
            .map_err(|err| format!("未能截取目标窗口（类名「{class}」）：{err}"))?;

        let rgba = vision::pixels::to_rgba(&shot).map_err(|err| err.to_string())?;
        let png = vision::pixels::encode_png(&rgba).map_err(|err| err.to_string())?;

        let url = format!("{base}/predict?conf={conf}&imgsz=1280&annotated=1");
        let response = ureq::post(&url)
            .timeout(Duration::from_secs(60))
            .set("Content-Type", "image/png")
            .send_bytes(&png)
            .map_err(|err| format!("远程检测请求失败（{url}）：{err}"))?;

        let status = response.status();
        let body_text = response
            .into_string()
            .map_err(|err| format!("读取远程响应失败：{err}"))?;
        if !(200..300).contains(&status) {
            return Err(format!("远程检测 HTTP {status}：{body_text}"));
        }

        let body: PredictBody = serde_json::from_str(&body_text)
            .map_err(|err| format!("解析远程 JSON 失败：{err}；原文前 200 字：{}", trunc(&body_text, 200)))?;
        if !body.success {
            return Err(body
                .error
                .unwrap_or_else(|| "远程检测 success=false，未给出 error 字段。".into()));
        }

        let mut detections = Vec::with_capacity(body.detections.len());
        for det in &body.detections {
            let cx = det.center[0].round() as i32;
            let cy = det.center[1].round() as i32;
            let screen = crate::shot_point_to_screen(cx, cy, window, shot.width, shot.height);
            detections.push(YoloDetectionView {
                class_id: det.class_id,
                class_name: det.class_name.clone(),
                conf: det.conf,
                center: det.center,
                xyxy: det.xyxy,
                screen,
            });
        }

        let notice = format!(
            "远程 /predict 检出 {} 个目标（原图 {}×{}，conf≥{conf:.2}）。",
            body.count, shot.width, shot.height
        );

        Ok(YoloDetectResult {
            window,
            shot_width: shot.width,
            shot_height: shot.height,
            count: body.count,
            detections,
            annotated_image: body.annotated_image,
            notice,
        })
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (class, request, conf, base);
        Err("远程检测目前只支持 Windows 与 macOS".into())
    }
}

fn trunc(s: &str, max: usize) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i >= max {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}
