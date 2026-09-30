//! YOLO 工作流共用步骤：整窗检测、点检测框、列表首行 OCR、滚动重试。
//!
//! ★ 坐标换算只用 [`crate::yolo::shot_point_to_screen`]；
//! ★ 按 class 挑框只用 [`crate::yolo::best_by_class`] / [`crate::yolo::all_by_class`]；
//! ★ 姓名是否接受仍问 [`ContactMatcher`]（判据不另开一处）。

use crate::ports::{AutomationError, Point, Rect, Screenshot, TextBox};
use crate::yolo::{
    self, all_by_class, best_by_class, item_tall_enough_for_two_lines, name_match_score,
    shot_point_to_screen, YoloDetection, DEFAULT_MIN_ITEM_HEIGHT_PX,
};

use super::Run;

impl Run<'_> {
    /// 截整窗 → YOLO 检测。返回窗口矩形、截图、检测列表。
    pub(super) fn yolo_detect_window(
        &mut self,
        step: &str,
    ) -> Result<(Rect, Screenshot, Vec<YoloDetection>), AutomationError> {
        let window = self.window.ok_or(AutomationError::ClientNotReady)?;
        let shot = self.capture_frame(window, step)?;
        let conf = self.cfg().yolo_conf;
        let dets = self
            .runner
            .ports
            .yolo
            .detect(&shot, conf)
            .map_err(|err| match err {
                AutomationError::Platform(msg) => AutomationError::Platform(format!(
                    "{step}：YOLO 检测失败：{msg}"
                )),
                other => other,
            })?;
        self.evidence.push(format!(
            "{step}：YOLO 检出 {} 个（conf≥{conf:.2}，图 {}×{}）",
            dets.len(),
            shot.width,
            shot.height
        ));
        Ok((window, shot, dets))
    }

    /// 把检测中心换算成屏幕坐标并 `guarded_click`（Windows 上应由 GhostBox 实现）。
    pub(super) fn yolo_click_detection(
        &mut self,
        window: Rect,
        shot: &Screenshot,
        det: &YoloDetection,
        what: &str,
    ) -> Result<(), AutomationError> {
        let expected_window = self.ensure_calibrated()?;
        let c = det.center_point();
        let target = shot_point_to_screen(c.x, c.y, window, shot.width, shot.height);
        self.evidence.push(format!(
            "点击{what}：{} conf={:.2} → 屏幕 ({}, {})",
            det.class_name, det.conf, target.x, target.y
        ));
        self.ensure_not_frozen(&format!("已取消点击{what}"))?;
        self.runner
            .ports
            .platform
            .guarded_click(target, expected_window)?;
        self.check_deadline(&format!("点击{what}"))
    }

    /// 在检测列表里找 class，没有则转人工。
    pub(super) fn yolo_require_class<'a>(
        &self,
        dets: &'a [YoloDetection],
        class_name: &str,
        hint: &str,
    ) -> Result<&'a YoloDetection, AutomationError> {
        best_by_class(dets, class_name).ok_or_else(|| {
            AutomationError::NeedsHumanReview(format!(
                "画面上没有检出「{class_name}」。{hint}"
            ))
        })
    }

    /// 对单个 YOLO bbox 做 OCR，返回**图像坐标**下的文字框（相对整窗截图）。
    pub(super) fn yolo_ocr_bbox(
        &mut self,
        window: Rect,
        shot: &Screenshot,
        det: &YoloDetection,
        step: &str,
    ) -> Result<Vec<TextBox>, AutomationError> {
        let bounds = det.bounds_rect();
        // 原图像素 → 屏幕区域，再 capture；OCR bounds 会再经 scale_boxes_to_logical。
        let tl = shot_point_to_screen(bounds.x, bounds.y, window, shot.width, shot.height);
        let br = shot_point_to_screen(
            bounds.x + bounds.width,
            bounds.y + bounds.height,
            window,
            shot.width,
            shot.height,
        );
        let region = Rect {
            x: tl.x,
            y: tl.y,
            width: (br.x - tl.x).max(1),
            height: (br.y - tl.y).max(1),
        };
        let (_crop, boxes) = self.capture_and_recognize(region, step)?;
        // capture_and_recognize 已把 bounds 换成相对 region 的逻辑坐标；
        // 再叠到整窗图像坐标系，方便与其它条目比「首行更靠前」。
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
        Ok(boxes
            .into_iter()
            .map(|mut b| {
                b.bounds.x += origin_img_x;
                b.bounds.y += origin_img_y;
                b
            })
            .collect())
    }

    /// 取 bbox 内 OCR **第一行**（y 最小的一块；同行取最左）。
    pub(super) fn yolo_first_line_text(boxes: &[TextBox]) -> Option<&TextBox> {
        boxes.iter().min_by_key(|b| (b.bounds.y, b.bounds.x))
    }

    /// 在 conversation_item / contact_item 里用首行 OCR 找联系人。
    ///
    /// 策略：跳过高度不够两行的裁切条目；姓名匹配优先首行中**更靠前**出现的；
    /// 是否接受目标名仍问 matcher（与全库其它姓名判据同源）。
    pub(super) fn yolo_find_name_in_items(
        &mut self,
        window: Rect,
        shot: &Screenshot,
        dets: &[YoloDetection],
        item_class: &str,
        expected_name: &str,
        step: &str,
    ) -> Result<Option<YoloDetection>, AutomationError> {
        let items = all_by_class(dets, item_class);
        if items.is_empty() {
            return Ok(None);
        }
        let min_h = {
            // 把默认像素阈值按 shot/window 比例调到原图像素。
            let scale = if window.height > 0 {
                shot.height as f32 / window.height as f32
            } else {
                1.0
            };
            (DEFAULT_MIN_ITEM_HEIGHT_PX as f32 * scale).round() as i32
        };

        let mut best: Option<(YoloDetection, usize, f32)> = None;
        for det in items {
            let bounds = det.bounds_rect();
            if !item_tall_enough_for_two_lines(bounds, min_h) {
                self.evidence.push(format!(
                    "{step}：跳过半截条目（高 {}px < {min_h}）",
                    bounds.height
                ));
                continue;
            }
            let boxes = self.yolo_ocr_bbox(window, shot, det, step)?;
            let Some(first) = Self::yolo_first_line_text(&boxes) else {
                continue;
            };
            if self.cfg().log_ocr_candidates {
                self.evidence.push(format!(
                    "{step}：首行「{}」conf={:.2}",
                    first.text.trim(),
                    first.confidence
                ));
            }
            // 构造一个 TextBox 问 matcher（bounds 仅占位）。
            let candidate = TextBox {
                text: first.text.clone(),
                bounds: first.bounds,
                confidence: first.confidence,
            };
            if !self
                .runner
                .ports
                .matcher
                .accepts(expected_name, &candidate)
            {
                // 放宽：若配置用 contains 匹配器，accepts 已覆盖；
                // 若严格匹配失败，再试「首行包含目标名」（与产品「首行姓名」约定一致）。
                if name_match_score(&first.text, expected_name).is_none() {
                    continue;
                }
            }
            let score = name_match_score(&first.text, expected_name)
                .or_else(|| Some((0, first.confidence)))
                .unwrap_or((usize::MAX, 0.0));
            let replace = match &best {
                None => true,
                Some((_, best_idx, best_conf)) => {
                    score.0 < *best_idx || (score.0 == *best_idx && score.1 > *best_conf)
                }
            };
            if replace {
                best = Some((det.clone(), score.0, score.1.max(first.confidence)));
            }
        }
        Ok(best.map(|(d, _, _)| d))
    }

    /// 列表区中心作为滚轮落点：取全部 item 的包围盒中心；没有则窗口水平 35%、垂直居中。
    pub(super) fn yolo_list_scroll_point(
        window: Rect,
        shot: &Screenshot,
        dets: &[YoloDetection],
        item_class: &str,
    ) -> Point {
        let items = all_by_class(dets, item_class);
        if items.is_empty() {
            return Point {
                x: window.x + (window.width as f32 * 0.35).round() as i32,
                y: window.y + window.height / 2,
            };
        }
        let mut x1 = i32::MAX;
        let mut y1 = i32::MAX;
        let mut x2 = i32::MIN;
        let mut y2 = i32::MIN;
        for det in items {
            let b = det.bounds_rect();
            x1 = x1.min(b.x);
            y1 = y1.min(b.y);
            x2 = x2.max(b.x + b.width);
            y2 = y2.max(b.y + b.height);
        }
        let cx = (x1 + x2) / 2;
        let cy = (y1 + y2) / 2;
        shot_point_to_screen(cx, cy, window, shot.width, shot.height)
    }

    /// 向下滚一格并等停稳。
    pub(super) fn yolo_scroll_list(
        &mut self,
        window: Rect,
        shot: &Screenshot,
        dets: &[YoloDetection],
        item_class: &str,
    ) -> Result<(), AutomationError> {
        let at = Self::yolo_list_scroll_point(window, shot, dets, item_class);
        let expected = self.ensure_calibrated()?;
        let notches = self.cfg().scroll_notches_per_step;
        self.evidence.push(format!(
            "列表滚动：屏幕 ({}, {})，向下 {notches} 格",
            at.x, at.y
        ));
        self.runner
            .ports
            .platform
            .scroll(at, notches, expected)?;
        self.wait_for_settle(window)
    }

    /// 点 input_bar（或 message_input）→ 逐字输入 → 视配置点 send_button。
    ///
    /// 调用前须已处于 [`TaskState::PreparingMessage`]。
    pub(super) fn yolo_type_and_send(
        &mut self,
        window: Rect,
        shot: &Screenshot,
        dets: &[YoloDetection],
    ) -> Result<(), AutomationError> {
        use crate::audit::MessageDigest;
        use crate::state::TaskState;

        let input = best_by_class(dets, yolo::class::INPUT_BAR)
            .or_else(|| best_by_class(dets, yolo::class::MESSAGE_INPUT))
            .ok_or_else(|| {
                AutomationError::NeedsHumanReview(
                    "聊天页未检出 input_bar / message_input，无法聚焦输入框。                     请确认已打开与目标的会话且输入区未被遮挡。"
                        .into(),
                )
            })?;
        self.yolo_click_detection(window, shot, input, "输入框")?;

        let text = self.task.text.clone();
        let expected = self.ensure_calibrated()?;
        self.evidence
            .push(format!("逐字输入正文（{} 字）", text.chars().count()));
        self.ensure_not_frozen("已取消填入消息正文")?;
        self.runner
            .ports
            .platform
            .type_text(&text, expected)?;
        self.check_deadline("输入正文")?;

        if self.cfg().stop_before_send {
            self.advance(
                TaskState::Prepared,
                Some(format!(
                    "已把 {} 个字符逐字填入输入框，未发送",
                    text.chars().count()
                )),
            )?;
            return Ok(());
        }

        self.runner.ledger.claim(self.task.id)?;
        self.message_digest = Some(MessageDigest::of(&text));
        self.advance(TaskState::Sending, None)?;

        let (window, shot, dets) = self.yolo_detect_window("发送前检测")?;
        let send = self.yolo_require_class(
            &dets,
            yolo::class::SEND_BUTTON,
            "请确认输入区可见且发送按钮未被遮挡。",
        )?;
        self.yolo_click_detection(window, &shot, send, "发送按钮")?;

        self.advance(TaskState::VerifyingDelivery, None)?;
        let (_w, _s, after) = self.yolo_detect_window("发送后检测")?;
        let has_out = best_by_class(&after, yolo::class::OUTGOING_BUBBLE).is_some();
        if !has_out {
            // 不把「暂时没框到气泡」当成硬失败：YOLO 对气泡召回不稳定。
            self.evidence
                .push("发送后未检出 outgoing_bubble（已点击发送；请人工核对）".into());
        }
        self.advance(TaskState::Completed, Some("已点击发送".into()))?;
        Ok(())
    }
}
