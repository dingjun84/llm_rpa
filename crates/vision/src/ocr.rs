//! 离线 OCR 适配器。
//!
//! 本模块**只**支持本机进程与本机模型：把局部截图编码为 PNG 后通过标准输入
//! 交给本地 OCR 程序，从标准输出读回 JSON。全程不产生任何网络请求。
//!
//! 本地 OCR 程序的输出约定（UTF-8 JSON 数组）：
//!
//! ```json
//! [{"text":"张三","x":12,"y":40,"w":96,"h":24,"confidence":0.98}]
//! ```

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use automation_core::{AutomationError, LocalOcr, Rect, Screenshot, TextBox};
use serde::Deserialize;

use crate::pixels::{encode_png, to_rgba};
use crate::{VisionError, VisionResult};

/// 本地 OCR 程序输出的单条结果。
#[derive(Debug, Clone, Deserialize)]
pub struct RawBox {
    pub text: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    #[serde(default = "one")]
    pub confidence: f32,
}

fn one() -> f32 {
    1.0
}

/// 把本地 OCR 程序的 JSON 输出解析为 [`TextBox`]。
///
/// 置信度会被夹到 `0.0..=1.0`；非有限值按 `0.0` 处理，避免 NaN 污染后续比较。
pub fn parse_ocr_json(output: &str) -> VisionResult<Vec<TextBox>> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let raw: Vec<RawBox> = serde_json::from_str(trimmed).map_err(|err| {
        VisionError::OcrParse(format!("{err}；原始输出片段：{}", truncate(trimmed, 200)))
    })?;
    Ok(raw
        .into_iter()
        .map(|item| TextBox {
            text: item.text,
            bounds: Rect {
                x: item.x,
                y: item.y,
                width: item.w.max(0),
                height: item.h.max(0),
            },
            confidence: if item.confidence.is_finite() {
                item.confidence.clamp(0.0, 1.0)
            } else {
                0.0
            },
        })
        .collect())
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    text.chars().take(limit).collect::<String>() + "…"
}

/// 调用本机 OCR 程序完成识别。
#[derive(Debug, Clone)]
pub struct ExternalOcr {
    pub command: PathBuf,
    pub args: Vec<String>,
    pub timeout: Duration,
}

impl ExternalOcr {
    pub fn new(command: impl Into<PathBuf>) -> Self {
        Self { command: command.into(), args: Vec::new(), timeout: Duration::from_secs(10) }
    }

    pub fn with_args(mut self, args: impl IntoIterator<Item = String>) -> Self {
        self.args = args.into_iter().collect();
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn run(&self, image: &Screenshot) -> VisionResult<Vec<TextBox>> {
        let png = encode_png(&to_rgba(image)?)?;

        let mut child = Command::new(&self.command)
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| VisionError::OcrSpawn(format!("{}：{err}", self.command.display())))?;

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| VisionError::OcrSpawn("无法打开 OCR 进程的标准输入".into()))?;
        let writer = std::thread::spawn(move || {
            // 写完后 `stdin` 被丢弃，等于关闭管道，OCR 程序据此得知输入结束。
            let _ = stdin.write_all(&png);
            let _ = stdin.flush();
        });

        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| VisionError::OcrSpawn("无法打开 OCR 进程的标准输出".into()))?;
        let reader = std::thread::spawn(move || {
            let mut buffer = Vec::new();
            let _ = stdout.read_to_end(&mut buffer);
            buffer
        });

        let deadline = Instant::now() + self.timeout;
        let mut timed_out = false;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        timed_out = true;
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(err) => return Err(VisionError::OcrSpawn(err.to_string())),
            }
        }

        let _ = writer.join();
        let output = reader.join().unwrap_or_default();

        if timed_out {
            return Err(VisionError::OcrTimeout(self.timeout));
        }
        parse_ocr_json(&String::from_utf8_lossy(&output))
    }
}

impl LocalOcr for ExternalOcr {
    fn recognize(&self, image: &Screenshot) -> Result<Vec<TextBox>, AutomationError> {
        self.run(image).map_err(AutomationError::from)
    }
}

/// 未配置本地 OCR 引擎时的默认实现：明确拒绝，而不是退回任何远程服务。
#[derive(Debug, Default, Clone, Copy)]
pub struct UnconfiguredOcr;

impl LocalOcr for UnconfiguredOcr {
    fn recognize(&self, _image: &Screenshot) -> Result<Vec<TextBox>, AutomationError> {
        Err(AutomationError::NeedsHumanReview(
            "尚未配置本地 OCR 引擎；本项目拒绝使用任何远程识别服务".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_well_formed_response() {
        let boxes = parse_ocr_json(
            r#"[{"text":"张三","x":12,"y":40,"w":96,"h":24,"confidence":0.98}]"#,
        )
        .unwrap();
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].text, "张三");
        assert_eq!(boxes[0].bounds, Rect { x: 12, y: 40, width: 96, height: 24 });
        assert!((boxes[0].confidence - 0.98).abs() < 1e-6);
    }

    #[test]
    fn empty_output_yields_no_boxes() {
        assert!(parse_ocr_json("   ").unwrap().is_empty());
        assert!(parse_ocr_json("[]").unwrap().is_empty());
    }

    #[test]
    fn confidence_is_clamped_into_range() {
        let boxes = parse_ocr_json(
            r#"[{"text":"a","x":0,"y":0,"w":1,"h":1,"confidence":5.0},
                {"text":"b","x":0,"y":0,"w":1,"h":1,"confidence":-2.0}]"#,
        )
        .unwrap();
        assert_eq!(boxes[0].confidence, 1.0);
        assert_eq!(boxes[1].confidence, 0.0);
    }

    #[test]
    fn confidence_defaults_to_one_when_absent() {
        let boxes = parse_ocr_json(r#"[{"text":"a","x":0,"y":0,"w":1,"h":1}]"#).unwrap();
        assert_eq!(boxes[0].confidence, 1.0);
    }

    #[test]
    fn negative_sizes_are_clamped_to_zero() {
        let boxes =
            parse_ocr_json(r#"[{"text":"a","x":0,"y":0,"w":-5,"h":-5,"confidence":0.9}]"#).unwrap();
        assert_eq!(boxes[0].bounds.width, 0);
        assert_eq!(boxes[0].bounds.height, 0);
    }

    #[test]
    fn malformed_output_reports_a_parse_error() {
        let err = parse_ocr_json("not json at all").unwrap_err();
        assert!(matches!(err, VisionError::OcrParse(_)));
    }

    #[test]
    fn unconfigured_ocr_refuses_instead_of_calling_out() {
        let ocr = UnconfiguredOcr;
        let screenshot = Screenshot {
            pixels: vec![0; 4],
            width: 1,
            height: 1,
            captured_at: std::time::SystemTime::now(),
            fingerprint: "x".into(),
        };
        let err = ocr.recognize(&screenshot).unwrap_err();
        assert!(matches!(err, AutomationError::NeedsHumanReview(_)));
    }
}
