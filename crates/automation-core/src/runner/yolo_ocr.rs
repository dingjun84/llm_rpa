//! YOLO bbox → OCR：单框串行、多框并发（上限见 [`crate::ocr_batch::MAX_OCR_CONCURRENCY`]）。

use crate::ocr_batch;
use crate::ports::{AutomationError, Rect, Screenshot, TextBox};
use crate::yolo::{shot_point_to_screen, YoloDetection};

use super::Run;

impl Run<'_> {
    /// 对单个 YOLO bbox 做 OCR，返回**图像坐标**下的文字框（相对整窗截图）。
    ///
    /// 单框保持串行（截屏 + OCR 同一次 `with_retry`），与整窗/单区域路径一致。
    pub(super) fn yolo_ocr_bbox(
        &mut self,
        window: Rect,
        shot: &Screenshot,
        det: &YoloDetection,
        step: &str,
    ) -> Result<Vec<TextBox>, AutomationError> {
        let started = std::time::Instant::now();
        let region = bbox_screen_region(window, shot, det);
        let result = self.capture_and_recognize(region, step);
        self.note_elapsed(
            &format!("OCR 单次·bbox·{step}"),
            concat!(file!(), ":", line!()),
            started.elapsed(),
        );
        let (_crop, boxes) = result?;
        Ok(remap_boxes_to_window(window, shot, region, boxes))
    }

    /// 对多个 YOLO bbox **并发** OCR（截屏仍串行；识别最多
    /// [`ocr_batch::MAX_OCR_CONCURRENCY`] 路并行）。
    ///
    /// 返回与 `dets` **等长、同序**；每个元素是该框在**整窗图像坐标**下的文字框。
    /// 诊断 / 证据按原 det 顺序逐条 `report`，与串行路径可回放对齐。
    ///
    /// `dets.len() <= 1` 时退回 [`Self::yolo_ocr_bbox`]，不启线程池。
    pub(super) fn yolo_ocr_bboxes(
        &mut self,
        window: Rect,
        shot: &Screenshot,
        dets: &[YoloDetection],
        step: &str,
    ) -> Result<Vec<Vec<TextBox>>, AutomationError> {
        if dets.is_empty() {
            return Ok(Vec::new());
        }
        if dets.len() == 1 {
            return Ok(vec![self.yolo_ocr_bbox(window, shot, &dets[0], step)?]);
        }

        let batch_started = std::time::Instant::now();
        let n = dets.len();
        let what = format!("OCR 多气泡 batch·{n}框·{step}");

        // 用闭包包住，保证成功/失败都落耗时（失败时仍要能看出 batch 卡了多久）。
        let result = (|| {
            // 1) 算屏幕区域并串行截屏（平台截屏不并行）。
            let mut regions = Vec::with_capacity(dets.len());
            let mut crops = Vec::with_capacity(dets.len());
            for det in dets {
                let region = bbox_screen_region(window, shot, det);
                let crop = self.with_retry(step, || self.runner.ports.platform.capture(region))?;
                regions.push(region);
                crops.push(crop);
            }

            // 2) 并发 OCR（可重试的瞬时错误在 worker 内按配置重试）。
            let attempts = self.cfg().max_attempts.max(1);
            let backoff = self.cfg().retry_backoff;
            let ocr = self.runner.ports.ocr.clone();
            let ocr_results = ocr_batch::recognize_many_with_raw_retrying(
                ocr.as_ref(),
                &crops,
                attempts,
                backoff,
            );

            // 3) 按原序：缩到逻辑坐标 → 上报证据 → 叠回整窗图像坐标。
            let mut all = Vec::with_capacity(dets.len());
            let mut last_frame: Option<(Screenshot, Vec<TextBox>)> = None;
            for (i, ocr_result) in ocr_results.into_iter().enumerate() {
                let (raw_boxes, raw) = ocr_result?;
                let region = regions[i];
                let crop = crops[i].clone();
                let (crop, boxes) = super::scale_boxes_to_logical(crop, raw_boxes, region);
                self.report(step, region, &crop, &boxes, Some(&raw), None);
                last_frame = Some((crop, boxes.clone()));
                all.push(remap_boxes_to_window(window, shot, region, boxes));
            }
            if let Some(frame) = last_frame {
                self.last_frame.replace(frame);
            }
            Ok(all)
        })();
        self.note_elapsed(&what, concat!(file!(), ":", line!()), batch_started.elapsed());
        result
    }

    /// 取 bbox 内 OCR **第一行**（y 最小的一块；同行取最左）。
    pub(super) fn yolo_first_line_text(boxes: &[TextBox]) -> Option<&TextBox> {
        boxes.iter().min_by_key(|b| (b.bounds.y, b.bounds.x))
    }
}

fn bbox_screen_region(window: Rect, shot: &Screenshot, det: &YoloDetection) -> Rect {
    let bounds = det.bounds_rect();
    let tl = shot_point_to_screen(bounds.x, bounds.y, window, shot.width, shot.height);
    let br = shot_point_to_screen(
        bounds.x + bounds.width,
        bounds.y + bounds.height,
        window,
        shot.width,
        shot.height,
    );
    Rect {
        x: tl.x,
        y: tl.y,
        width: (br.x - tl.x).max(1),
        height: (br.y - tl.y).max(1),
    }
}

fn remap_boxes_to_window(
    window: Rect,
    shot: &Screenshot,
    region: Rect,
    boxes: Vec<TextBox>,
) -> Vec<TextBox> {
    let scale_x = if shot.width > 0 {
        shot.width as f32 / window.width.max(1) as f32
    } else {
        1.0
    };
    let scale_y = if shot.height > 0 {
        shot.height as f32 / window.height.max(1) as f32
    } else {
        1.0
    };
    let origin_img_x = ((region.x - window.x) as f32 * scale_x).round() as i32;
    let origin_img_y = ((region.y - window.y) as f32 * scale_y).round() as i32;
    boxes
        .into_iter()
        .map(|mut b| {
            b.bounds.x += origin_img_x;
            b.bounds.y += origin_img_y;
            b
        })
        .collect()
}
