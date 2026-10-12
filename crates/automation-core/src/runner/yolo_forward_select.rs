//! 转发步骤3：点击「文件传输助手」结果行后的选中态校验。
//!
//! 流程：整窗 YOLO `list_item` → 框内 OCR 首行含目标文案 → HSV 蓝占比 > 0.4；
//! 找到但未选中则左击该行，间隔 100ms，最多 3 次左击重试。

use std::time::Duration;

use crate::nav_selected::{is_list_item_selected, nav_icon_selected_ratio, LIST_ITEM_SELECTED_RATIO};
use crate::ports::{AutomationError, Rect, Screenshot, TextBox};
use crate::yolo::{all_by_class, class, random_point_in_central_half, shot_point_to_screen};

use super::Run;

/// 未选中时最多左击重试次数（验收口径：fail after 3 left-click retries）。
const LIST_ITEM_SELECT_MAX_CLICKS: u32 = 3;
/// 每次左击后、再检前的间隔。
const LIST_ITEM_SELECT_RETRY_INTERVAL: Duration = Duration::from_millis(100);

/// 单次选中检查结果（纯数据，便于单测驱动重试语义）。
#[derive(Debug, Clone, PartialEq)]
enum RowSelectStatus {
    /// 首行命中且蓝占比 > 阈值。
    Selected { ratio: f32, bounds: Rect },
    /// 首行命中但未过蓝阈值（可左击重试）。
    FoundUnselected { ratio: f32, bounds: Rect },
    /// 本帧没有任何 list_item 首行含目标文案。
    NotFound,
}

impl Run<'_> {
    /// 步骤3 点击结果行之后：确认「文件传输助手」所在 `list_item` 已呈蓝色选中态。
    ///
    /// 1. 先检一次；已选中则直接过。
    /// 2. 找到但未选中 → 移到该框中心半区并 **左击**，睡 100ms，再检；最多 3 次左击。
    /// 3. 仍未选中或始终找不到匹配行 → [`AutomationError::NeedsHumanReview`]。
    ///
    /// 左击次数 / 再检顺序与 [`select_row_attempts_until`] 单测一致（先检 → 未过则左击 → 100ms → 再检，最多 3 次左击）。
    pub(super) fn forward_verify_file_helper_row_selected(
        &mut self,
        step: u32,
        needle: &str,
    ) -> Result<(), AutomationError> {
        // 首次：点击后立刻检。
        match self.forward_check_list_item_row_selected(step, needle, 0)? {
            RowSelectStatus::Selected { ratio, bounds } => {
                self.evidence.push(format!(
                    "步骤{step}：选中校验通过（点击后首次；首行「{needle}」；蓝占比 {ratio:.3} > {LIST_ITEM_SELECTED_RATIO}；框 ({}, {}) {}x{}）",
                    bounds.x, bounds.y, bounds.width, bounds.height
                ));
                return Ok(());
            }
            RowSelectStatus::NotFound => {
                return Err(AutomationError::NeedsHumanReview(format!(
                    "步骤{step}失败：点击后未见首行含「{needle}」的 list_item，无法确认选中态。"
                )));
            }
            RowSelectStatus::FoundUnselected { ratio, bounds } => {
                self.evidence.push(format!(
                    "步骤{step}：选中校验未过（点击后首次；蓝占比 {ratio:.3} ≤ {LIST_ITEM_SELECTED_RATIO}）；将左击重试"
                ));
                self.forward_click_list_item_box(step, bounds, 1)?;
            }
        }

        for attempt in 1..=LIST_ITEM_SELECT_MAX_CLICKS {
            std::thread::sleep(LIST_ITEM_SELECT_RETRY_INTERVAL);
            match self.forward_check_list_item_row_selected(step, needle, attempt)? {
                RowSelectStatus::Selected { ratio, bounds } => {
                    self.evidence.push(format!(
                        "步骤{step}：选中校验通过（左击重试 {attempt}/{LIST_ITEM_SELECT_MAX_CLICKS} 后；蓝占比 {ratio:.3} > {LIST_ITEM_SELECTED_RATIO}；框 ({}, {}) {}x{}）",
                        bounds.x, bounds.y, bounds.width, bounds.height
                    ));
                    return Ok(());
                }
                RowSelectStatus::NotFound => {
                    return Err(AutomationError::NeedsHumanReview(format!(
                        "步骤{step}失败：左击重试 {attempt}/{LIST_ITEM_SELECT_MAX_CLICKS} 后未见首行含「{needle}」的 list_item。"
                    )));
                }
                RowSelectStatus::FoundUnselected { ratio, bounds } => {
                    self.evidence.push(format!(
                        "步骤{step}：选中校验未过（左击重试 {attempt}/{LIST_ITEM_SELECT_MAX_CLICKS}；蓝占比 {ratio:.3} ≤ {LIST_ITEM_SELECTED_RATIO}）"
                    ));
                    if attempt == LIST_ITEM_SELECT_MAX_CLICKS {
                        return Err(AutomationError::NeedsHumanReview(format!(
                            "步骤{step}失败：左击重试 {LIST_ITEM_SELECT_MAX_CLICKS} 次后「{needle}」行仍未选中\
                             （最后蓝占比 {ratio:.3}，阈值 > {LIST_ITEM_SELECTED_RATIO}）。请确认结果行变为蓝色选中态。"
                        )));
                    }
                    self.forward_click_list_item_box(step, bounds, attempt + 1)?;
                }
            }
        }
        Err(AutomationError::NeedsHumanReview(format!(
            "步骤{step}失败：选中校验耗尽重试仍未通过。"
        )))
    }

    /// 整窗 YOLO + 各 list_item OCR 首行匹配 + 蓝占比。
    ///
    /// `bounds` 为**图像坐标**（与 YOLO / HSV 同源）；左击前再换算屏幕点。
    fn forward_check_list_item_row_selected(
        &mut self,
        step: u32,
        needle: &str,
        after_click: u32,
    ) -> Result<RowSelectStatus, AutomationError> {
        let label = if after_click == 0 {
            format!("转发·{step}·选中校验·点击后")
        } else {
            format!("转发·{step}·选中校验·左击后第{after_click}次")
        };
        let (window, shot, dets) = self.yolo_detect_window(&label)?;
        let items: Vec<_> = all_by_class(&dets, class::LIST_ITEM)
            .into_iter()
            .cloned()
            .collect();
        self.evidence.push(format!(
            "步骤{step}：选中校验检出 list_item {} 个（{label}）",
            items.len()
        ));
        if items.is_empty() {
            return Ok(RowSelectStatus::NotFound);
        }
        let ocr_by_item = self.yolo_ocr_bboxes(window, &shot, &items, &label)?;
        classify_list_item_selection(&shot, &items, &ocr_by_item, needle)
    }

    /// 在 list_item 图像框的中心半区落点并左击（平台 `guarded_click`，含移入）。
    fn forward_click_list_item_box(
        &mut self,
        step: u32,
        img_bounds: Rect,
        attempt: u32,
    ) -> Result<(), AutomationError> {
        let window = self.ensure_calibrated()?;
        let shot = self.runner.ports.platform.capture(window)?;
        let img_pt = random_point_in_central_half(img_bounds);
        let target = shot_point_to_screen(img_pt.x, img_pt.y, window, shot.width, shot.height);
        self.evidence.push(format!(
            "步骤{step}：选中未过 → 左击 list_item 第 {attempt}/{LIST_ITEM_SELECT_MAX_CLICKS} 次 → 屏幕 ({}, {})；图像框 ({}, {}) {}x{}",
            target.x, target.y, img_bounds.x, img_bounds.y, img_bounds.width, img_bounds.height
        ));
        self.ensure_not_frozen("已取消左击结果行")?;
        // 显式先 move，再 left-click（guarded_click 内也会再移一次，与 forward_click 路径一致）。
        self.runner.ports.platform.move_pointer(target)?;
        self.runner
            .ports
            .platform
            .guarded_click(target, window)?;
        self.check_deadline("左击文件传输助手结果行")
    }
}

/// 在已 OCR 的 list_item 里找首行含 `needle` 的行，并判蓝选中。
///
/// 多行命中时：任一选中即 Selected；否则取阅读序（上→下、左→右）第一条作左击目标。
fn classify_list_item_selection(
    shot: &Screenshot,
    items: &[crate::yolo::YoloDetection],
    ocr_by_item: &[Vec<TextBox>],
    needle: &str,
) -> Result<RowSelectStatus, AutomationError> {
    // 按阅读序（上→下、左→右）扫描，保证未选中时左击目标稳定。
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|&i| {
        let b = items[i].bounds_rect();
        (b.y, b.x)
    });
    let mut unselected: Option<(f32, Rect)> = None;
    for i in order {
        let det = &items[i];
        let boxes = &ocr_by_item[i];
        if !ocr_first_line_contains(boxes, needle) {
            continue;
        }
        let bounds = det.bounds_rect();
        let ratio = nav_icon_selected_ratio(shot, bounds);
        if is_list_item_selected(shot, bounds) {
            return Ok(RowSelectStatus::Selected { ratio, bounds });
        }
        if unselected.is_none() {
            unselected = Some((ratio, bounds));
        }
    }
    Ok(match unselected {
        Some((ratio, bounds)) => RowSelectStatus::FoundUnselected { ratio, bounds },
        None => RowSelectStatus::NotFound,
    })
}

/// OCR 框集合的「首行」是否包含 `needle`（y 最小，同行取最左）——与 [`Run::yolo_first_line_text`] 同口径。
fn ocr_first_line_contains(boxes: &[TextBox], needle: &str) -> bool {
    first_line_box(boxes)
        .map(|b| b.text.contains(needle))
        .unwrap_or(false)
}

fn first_line_box(boxes: &[TextBox]) -> Option<&TextBox> {
    boxes.iter().min_by_key(|b| (b.bounds.y, b.bounds.x))
}

/// 左击重试驱动（无平台）：先检 → 未选中则 `on_retry` 最多 `max_clicks` 次。
///
/// 返回 `Ok(checks)`：`checks` 为成功时已执行的检查次数（含首次）。
fn select_row_attempts_until<C, R>(
    max_clicks: u32,
    mut check: C,
    mut on_retry: R,
) -> Result<u32, RowSelectStatus>
where
    C: FnMut(u32) -> RowSelectStatus,
    R: FnMut(u32, Rect),
{
    let mut status = check(0);
    if matches!(status, RowSelectStatus::Selected { .. }) {
        return Ok(1);
    }
    if matches!(status, RowSelectStatus::NotFound) {
        return Err(status);
    }
    for attempt in 1..=max_clicks {
        if let RowSelectStatus::FoundUnselected { bounds, .. } = &status {
            on_retry(attempt, *bounds);
        }
        status = check(attempt);
        match &status {
            RowSelectStatus::Selected { .. } => return Ok(attempt + 1),
            RowSelectStatus::NotFound => return Err(status),
            RowSelectStatus::FoundUnselected { .. } if attempt == max_clicks => {
                return Err(status);
            }
            RowSelectStatus::FoundUnselected { .. } => {}
        }
    }
    Err(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nav_selected::blue_ratio_exceeds;
    use crate::ports::TextBox;
    use std::time::SystemTime;

    fn tb(text: &str, x: i32, y: i32) -> TextBox {
        TextBox {
            text: text.into(),
            bounds: Rect {
                x,
                y,
                width: 40,
                height: 12,
            },
            confidence: 0.9,
        }
    }

    fn solid_blue(w: u32, h: u32) -> Screenshot {
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            pixels.extend_from_slice(&[240u8, 160, 40, 255]);
        }
        Screenshot {
            pixels,
            width: w,
            height: h,
            captured_at: SystemTime::UNIX_EPOCH,
            fingerprint: "t".into(),
        }
    }

    fn solid_gray(w: u32, h: u32) -> Screenshot {
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            pixels.extend_from_slice(&[180u8, 180, 180, 255]);
        }
        Screenshot {
            pixels,
            width: w,
            height: h,
            captured_at: SystemTime::UNIX_EPOCH,
            fingerprint: "t".into(),
        }
    }

    fn det(x: i32, y: i32, w: i32, h: i32) -> crate::yolo::YoloDetection {
        let x1 = x as f32;
        let y1 = y as f32;
        let x2 = (x + w) as f32;
        let y2 = (y + h) as f32;
        crate::yolo::YoloDetection {
            class_id: 0,
            class_name: class::LIST_ITEM.into(),
            conf: 0.9,
            center: [(x1 + x2) / 2.0, (y1 + y2) / 2.0],
            xyxy: [x1, y1, x2, y2],
        }
    }

    #[test]
    fn first_line_is_topmost_then_leftmost() {
        let boxes = vec![
            tb("第二行", 0, 20),
            tb("文件传输助手", 10, 0),
            tb("更左但同行", 0, 0),
        ];
        let first = first_line_box(&boxes).unwrap();
        assert_eq!(first.text, "更左但同行");
        assert!(!ocr_first_line_contains(&boxes, "文件传输助手"));
        let boxes2 = vec![tb("文件传输助手", 0, 0), tb("昨天", 0, 16)];
        assert!(ocr_first_line_contains(&boxes2, "文件传输助手"));
        assert!(!ocr_first_line_contains(&boxes2, "不存在"));
    }

    #[test]
    fn classify_selected_when_blue_above_threshold() {
        let shot = solid_blue(20, 20);
        let items = vec![det(0, 0, 20, 20)];
        let ocr = vec![vec![tb("文件传输助手", 0, 0)]];
        let st = classify_list_item_selection(&shot, &items, &ocr, "文件传输助手").unwrap();
        assert!(matches!(st, RowSelectStatus::Selected { .. }));
    }

    #[test]
    fn classify_found_unselected_when_gray() {
        let shot = solid_gray(20, 20);
        let items = vec![det(0, 0, 20, 20)];
        let ocr = vec![vec![tb("文件传输助手", 0, 0)]];
        let st = classify_list_item_selection(&shot, &items, &ocr, "文件传输助手").unwrap();
        match st {
            RowSelectStatus::FoundUnselected { ratio, .. } => {
                assert!(!blue_ratio_exceeds(ratio, LIST_ITEM_SELECTED_RATIO));
            }
            other => panic!("expected FoundUnselected, got {other:?}"),
        }
    }

    #[test]
    fn classify_ignores_needle_only_on_second_line() {
        let shot = solid_blue(20, 40);
        let items = vec![det(0, 0, 20, 40)];
        let ocr = vec![vec![tb("其它标题", 0, 0), tb("文件传输助手", 0, 18)]];
        let st = classify_list_item_selection(&shot, &items, &ocr, "文件传输助手").unwrap();
        assert_eq!(st, RowSelectStatus::NotFound);
    }

    #[test]
    fn retry_driver_succeeds_after_two_left_clicks() {
        let mut checks = 0u32;
        let mut clicks = 0u32;
        let got = select_row_attempts_until(
            3,
            |_| {
                checks += 1;
                if checks >= 3 {
                    RowSelectStatus::Selected {
                        ratio: 0.5,
                        bounds: Rect {
                            x: 0,
                            y: 0,
                            width: 1,
                            height: 1,
                        },
                    }
                } else {
                    RowSelectStatus::FoundUnselected {
                        ratio: 0.1,
                        bounds: Rect {
                            x: 1,
                            y: 2,
                            width: 3,
                            height: 4,
                        },
                    }
                }
            },
            |_, _| clicks += 1,
        );
        assert_eq!(got, Ok(3));
        assert_eq!(clicks, 2);
        assert_eq!(checks, 3);
    }

    #[test]
    fn retry_driver_fails_after_three_left_clicks() {
        let mut clicks = 0u32;
        let got = select_row_attempts_until(
            LIST_ITEM_SELECT_MAX_CLICKS,
            |_| RowSelectStatus::FoundUnselected {
                ratio: 0.1,
                bounds: Rect {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 2,
                },
            },
            |_, _| clicks += 1,
        );
        assert!(matches!(got, Err(RowSelectStatus::FoundUnselected { .. })));
        assert_eq!(clicks, LIST_ITEM_SELECT_MAX_CLICKS);
    }

    #[test]
    fn retry_driver_fails_immediately_when_not_found() {
        let mut clicks = 0u32;
        let got = select_row_attempts_until(
            3,
            |_| RowSelectStatus::NotFound,
            |_, _| clicks += 1,
        );
        assert_eq!(got, Err(RowSelectStatus::NotFound));
        assert_eq!(clicks, 0);
    }
}
