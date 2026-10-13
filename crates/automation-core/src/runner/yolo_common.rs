//! YOLO 工作流共用步骤：整窗检测、列表首行 OCR、滚动落点。
//! 两轮滚动查找见 [`super::yolo_list_find`]；输入并发送见 [`super::yolo_send_verify`]。
//! 点检测框 / 点估计区见 [`super::yolo_click`]。
//!
//! ★ 坐标换算只用 [`crate::yolo::shot_point_to_screen`]；
//! ★ 按 class 挑框只用 [`crate::yolo::best_by_class`] / [`crate::yolo::all_by_class`]；
//! ★ 姓名是否接受仍问 [`ContactMatcher`]（判据不另开一处）。
//! ★ 每次 YOLO 检出都经 [`crate::yolo::detections_as_text_boxes`] 交给诊断，
//!   过程回放才能画出与 OCR 步骤同款的叠加框（不必改前端）。

use crate::diagnostics::{Decision, MatchTrail, ReplayInput, Verdict};
use crate::ports::{AutomationError, Point, Rect, Screenshot, TextBox};
use crate::yolo::{
    self, all_by_class, best_by_class, detections_as_text_boxes,
    item_tall_enough_for_two_lines, name_match_score, shot_point_to_screen, ListPage,
    YoloDetection, DEFAULT_MIN_ITEM_HEIGHT_PX,
};

use super::decision::name_match_decision;
use super::yolo_verify_rules::count_same_name;
use super::Run;

impl Run<'_> {
    /// 截整窗 → YOLO 检测。返回窗口矩形、截图、检测列表。
    ///
    /// 诊断侧会落一张带 YOLO 框的标注图：`text` = `class conf`，bounds 为本帧图像坐标。
    /// 故意**不**走 [`Self::capture_frame`]（那会先报一条空框观察），避免过程回放多一步空图。
    pub(super) fn yolo_detect_window(
        &mut self,
        step: &str,
    ) -> Result<(Rect, Screenshot, Vec<YoloDetection>), AutomationError> {
        // 每次检测前重读当前窗口。任务开始时冻住的矩形在窗口变宽后会截偏。
        let detect_started = std::time::Instant::now();
        let what = format!("YOLO 整窗检测·{step}");
        let result = (|| {
            let window = self.ensure_calibrated()?;
            let shot = self.with_retry(step, || self.runner.ports.platform.capture(window))?;
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
            let overlays = detections_as_text_boxes(&dets);
            self.report(step, window, &shot, &overlays, None, None);
            self.evidence.push(format!(
                "{step}：YOLO 检出 {} 个（conf≥{conf:.2}，图 {}×{}）",
                dets.len(),
                shot.width,
                shot.height
            ));
            Ok((window, shot, dets))
        })();
        self.note_elapsed(&what, concat!(file!(), ":", line!()), detect_started.elapsed());
        result
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

    /// 在 `list_item`（旧权重的 `conversation_item` / `contact_item` 同样接受）里用首行 OCR 找联系人。
    ///
    /// 是会话行还是联系人行不看 class 名，由 `page`（runner 刚导航到的页面）决定，只影响文案。
    ///
    /// 策略：跳过高度不够两行的裁切条目；姓名匹配优先首行中**更靠前**出现的；
    /// 是否接受目标名仍问 matcher（与全库其它姓名判据同源）。
    ///
    /// 每个候选的通过 / 淘汰都会经 [`Self::report_decision`] 落盘，过程回放右侧候选表可核对。
    pub(super) fn yolo_find_name_in_items(
        &mut self,
        window: Rect,
        shot: &Screenshot,
        dets: &[YoloDetection],
        page: ListPage,
        expected_name: &str,
        step: &str,
    ) -> Result<ItemMatch, AutomationError> {
        let items = all_by_class(dets, yolo::class::LIST_ITEM);
        let item_label = page.item_label();
        if items.is_empty() {
            self.report_decision(
                step,
                Decision {
                    step: String::new(),
                    question: format!(
                        "{}里哪一条{item_label}是目标联系人「{}」？",
                        page.label(),
                        expected_name.trim()
                    ),
                    rule: "YOLO class=`list_item`（兼容 conversation_item / contact_item）首行 OCR + ContactMatcher".into(),
                    outcome: format!("本帧未检出任何{item_label}（list_item）"),
                    passed: false,
                    min_confidence: self.cfg().min_confidence,
                    replay: Some(ReplayInput::NameMatch {
                        expected_name: expected_name.to_string(),
                        relaxed: true,
                    }),
                    candidates: Vec::new(),
                },
            );
            return Ok(ItemMatch::default());
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

        let mut best: Option<(YoloDetection, usize, f32, TextBox)> = None;
        let mut runners_up: Vec<TextBox> = Vec::new();
        let mut verdicts: Vec<Verdict> = Vec::new();
        let mut first_lines: Vec<String> = Vec::new();
        // 先筛半截条目，再对剩余框裁切后并发 OCR（上限见 ocr_batch::MAX_OCR_CONCURRENCY）。
        let mut tall: Vec<YoloDetection> = Vec::new();
        for det in items {
            let det_box = TextBox {
                text: format!("{} {:.2}", det.class_name, det.conf),
                bounds: det.bounds_rect(),
                confidence: det.conf,
            };
            let bounds = det.bounds_rect();
            if !item_tall_enough_for_two_lines(bounds, min_h) {
                self.evidence.push(format!(
                    "{step}：跳过半截条目（高 {}px < {min_h}）",
                    bounds.height
                ));
                verdicts.push(Verdict::rejected(
                    &det_box,
                    format!("半截条目 高 {}px < {min_h}", bounds.height),
                ));
                continue;
            }
            tall.push(det.clone());
        }
        let ocr_by_item = self.yolo_ocr_bboxes(window, shot, &tall, step)?;
        for (det, boxes) in tall.iter().zip(ocr_by_item.into_iter()) {
            let det_box = TextBox {
                text: format!("{} {:.2}", det.class_name, det.conf),
                bounds: det.bounds_rect(),
                confidence: det.conf,
            };
            let Some(first) = Self::yolo_first_line_text(&boxes) else {
                verdicts.push(Verdict::rejected(&det_box, "bbox 内无 OCR 文字"));
                continue;
            };
            first_lines.push(first.text.clone());
            if self.cfg().log_ocr_candidates {
                self.evidence.push(format!(
                    "{step}：首行「{}」conf={:.2}",
                    first.text.trim(),
                    first.confidence
                ));
            }
            // 构造一个 TextBox 问 matcher（bounds 用 OCR 首行框，便于回放对齐文字）。
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
                    verdicts.push(Verdict::rejected(
                        &candidate,
                        format!("不匹配「{}」", expected_name.trim()),
                    ));
                    continue;
                }
            }
            let score = name_match_score(&first.text, expected_name)
                .or_else(|| Some((0, first.confidence)))
                .unwrap_or((usize::MAX, 0.0));
            let replace = match &best {
                None => true,
                Some((_, best_idx, best_conf, _)) => {
                    score.0 < *best_idx || (score.0 == *best_idx && score.1 > *best_conf)
                }
            };
            if replace {
                if let Some((_, _, _, prev)) = best.take() {
                    runners_up.push(prev);
                }
                best = Some((
                    det.clone(),
                    score.0,
                    score.1.max(first.confidence),
                    candidate,
                ));
            } else {
                runners_up.push(candidate);
            }
        }
        for prev in &runners_up {
            verdicts.push(Verdict::rejected(
                prev,
                "匹配成立但不是本屏最佳命中",
            ));
        }
        if let Some((_, idx, conf, ref tb)) = &best {
            verdicts.push(Verdict::passed(
                tb,
                format!(
                    "首行「{}」匹配「{}」（idx={idx} conf={conf:.2}）",
                    tb.text.trim(),
                    expected_name.trim(),
                ),
            ));
        }

        let match_result: Result<TextBox, AutomationError> = match &best {
            Some((_, _, _, tb)) => Ok(tb.clone()),
            None => Err(AutomationError::NeedsHumanReview(format!(
                "本帧{}的{item_label}中未匹配到「{}」",
                page.label(),
                expected_name.trim()
            ))),
        };
        let trail = MatchTrail {
            rule: "YOLO 条目首行 OCR + ContactMatcher / name_match_score",
            relaxed: true,
            candidates: verdicts,
        };
        self.report_decision(
            step,
            name_match_decision(
                &trail,
                expected_name,
                self.cfg().min_confidence,
                &match_result,
            ),
        );
        let same_name = count_same_name(first_lines.iter().map(String::as_str), expected_name);
        Ok(ItemMatch { best: best.map(|(d, _, _, _)| d), same_name })
    }

    /// 列表区中心作为滚轮落点：取全部 list_item 的包围盒中心；没有则窗口水平 35%、垂直居中。
    pub(super) fn yolo_list_scroll_point(
        window: Rect,
        shot: &Screenshot,
        dets: &[YoloDetection],
    ) -> Point {
        let items = all_by_class(dets, yolo::class::LIST_ITEM);
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
}

/// [`Run::yolo_find_name_in_items`] 的结论：最佳命中 + 本帧首行与目标名逐字相等的行数。
#[derive(Default)]
pub(super) struct ItemMatch {
    pub(super) best: Option<YoloDetection>,
    /// >1 ⇒ 同名歧义，调用方须停下（`AmbiguousVision`）。
    pub(super) same_name: usize,
}
