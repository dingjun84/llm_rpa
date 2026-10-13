//! 会话列表里找人（A3）与点击前复检 + 点击后选中校验（A4）。
//!
//! - 两轮：第一轮从当前位置向下扫；没找到就回顶再扫一轮（列表按最近消息排序，
//!   扫描期间目标可能被顶到身后）。
//! - 每次滚动比较列表区指纹：没变 ⇒ 到底（客户端仍响应）或卡死（[`classify_scroll`]）。
//! - 同一帧里首行与目标名逐字相等的行 >1 ⇒ `AmbiguousVision`，不猜。

use crate::ports::{AutomationError, Rect, Screenshot};
use crate::yolo::{all_by_class, class, ListPage, YoloDetection};

use super::yolo_common::ItemMatch;
use super::yolo_forward_scroll::img_rect_to_screen;
use super::yolo_verify_rules::{classify_scroll, nearest_row, ScrollOutcome};
use super::Run;

/// 查找轮数：当前位置向下一轮 + 回顶后一轮。
const FIND_PASSES: u32 = 2;

impl Run<'_> {
    fn ambiguity_guard(&self, m: &ItemMatch, name: &str, page: ListPage) -> Result<(), AutomationError> {
        if m.same_name > 1 {
            return Err(AutomationError::AmbiguousVision(format!(
                "{}同一屏里有 {} 行首行都是「{}」，无法确定是哪一个，停止。",
                page.label(),
                m.same_name,
                name.trim()
            )));
        }
        Ok(())
    }

    /// 列表区（全部 list_item 包围盒）屏幕矩形；没有条目则取窗口左侧 15%..45%。
    fn yolo_list_region(window: Rect, shot: &Screenshot, dets: &[YoloDetection]) -> Rect {
        let items = all_by_class(dets, class::LIST_ITEM);
        if items.is_empty() {
            return Rect {
                x: window.x + (window.width as f32 * 0.15) as i32,
                y: window.y,
                width: (window.width as f32 * 0.30) as i32,
                height: window.height,
            };
        }
        let (mut x1, mut y1, mut x2, mut y2) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for d in items {
            let b = d.bounds_rect();
            x1 = x1.min(b.x);
            y1 = y1.min(b.y);
            x2 = x2.max(b.x + b.width);
            y2 = y2.max(b.y + b.height);
        }
        let img = Rect { x: x1, y: y1, width: x2 - x1, height: y2 - y1 };
        img_rect_to_screen(img, window, shot.width, shot.height)
    }

    /// 滚一下（`notches` 正=向下），按列表区指纹判断动了 / 到头 / 卡死。
    fn yolo_scroll_list_step(
        &mut self,
        window: Rect,
        shot: &Screenshot,
        dets: &[YoloDetection],
        notches: i32,
    ) -> Result<ScrollOutcome, AutomationError> {
        let region = Self::yolo_list_region(window, shot, dets);
        let at = Self::yolo_list_scroll_point(window, shot, dets);
        let before = self.capture_frame(region, "列表区指纹·滚动前")?.fingerprint;
        let expected = self.ensure_calibrated()?;
        self.runner.ports.platform.scroll(at, notches, expected)?;
        self.wait_for_settle(region)?;
        let after = self.capture_frame(region, "列表区指纹·滚动后")?.fingerprint;
        let responsive = if before == after {
            self.runner.ports.platform.is_responsive().ok()
        } else {
            None
        };
        let outcome = classify_scroll(&before, &after, responsive);
        self.evidence.push(format!(
            "列表滚动 {notches} 格 @ ({}, {}) → {outcome:?}",
            at.x, at.y
        ));
        if outcome == ScrollOutcome::Frozen {
            return Err(AutomationError::NeedsHumanReview(
                "滚动列表后画面没有变化，且系统判定客户端未响应（疑似卡死），停止查找。".into(),
            ));
        }
        Ok(outcome)
    }

    /// 回到列表顶部：向上滚直到列表区指纹不再变化。
    fn yolo_scroll_list_to_top(&mut self) -> Result<(), AutomationError> {
        let limit = self.cfg().max_scroll_attempts.max(1) * 3 + 1;
        let notches = self.cfg().scroll_notches_per_step;
        for i in 0..limit {
            let (window, shot, dets) = self.yolo_detect_window(&format!("列表回顶·第{}次", i + 1))?;
            if self.yolo_scroll_list_step(window, &shot, &dets, -notches)? == ScrollOutcome::ReachedEnd {
                return Ok(());
            }
        }
        Err(AutomationError::NeedsHumanReview(format!(
            "向上滚动 {limit} 次仍未回到列表顶部，拒绝继续猜测扫描起点。"
        )))
    }

    /// A3：两轮滚动查找。返回命中那一帧的窗口 / 截图 / 检测框。
    pub(super) fn yolo_scroll_find_contact(
        &mut self,
        page: ListPage,
    ) -> Result<(Rect, Screenshot, YoloDetection), AutomationError> {
        let name = self.task.external_contact_name.clone();
        let max = self.cfg().max_scroll_attempts;
        let notches = self.cfg().scroll_notches_per_step;
        for pass in 1..=FIND_PASSES {
            if pass > 1 {
                self.evidence.push(format!("第 {} 轮未找到「{name}」→ 回顶再扫", pass - 1));
                self.yolo_scroll_list_to_top()?;
            }
            for attempt in 0..=max {
                let (window, shot, dets) =
                    self.yolo_detect_window(&format!("列表查找·第{pass}轮·第{}屏", attempt + 1))?;
                let m = self.yolo_find_name_in_items(window, &shot, &dets, page, &name, "列表 OCR")?;
                self.ambiguity_guard(&m, &name, page)?;
                if let Some(hit) = m.best {
                    return Ok((window, shot, hit));
                }
                if attempt == max {
                    break;
                }
                if self.yolo_scroll_list_step(window, &shot, &dets, notches)? == ScrollOutcome::ReachedEnd {
                    self.evidence.push(format!("第 {pass} 轮：列表区指纹不再变化，已到底"));
                    break;
                }
            }
        }
        Err(AutomationError::NeedsHumanReview(format!(
            "在{}中扫了 {FIND_PASSES} 轮（每轮最多滚 {max} 次）仍未找到「{name}」。\
             请确认联系人在列表中、名称与 OCR 一致，或增大 max_scroll_attempts。",
            page.label()
        )))
    }

    /// A4：点击前在原坐标附近复检（防列表重排），点击后校验选中态（最多 3 次左击重试），
    /// 并记录点击是否让右侧画面变化（`last_click_reacted`）。返回所点行的屏幕矩形。
    pub(super) fn yolo_click_list_row_verified(
        &mut self,
        step: u32,
        page: ListPage,
        found: (Rect, &Screenshot, &YoloDetection),
        what: &str,
    ) -> Result<Rect, AutomationError> {
        let name = self.task.external_contact_name.clone();
        let (fw, fshot, fdet) = found;
        let prev = img_rect_to_screen(fdet.bounds_rect(), fw, fshot.width, fshot.height);

        let (window, shot, dets) = self.yolo_detect_window("点击前复检")?;
        let items: Vec<YoloDetection> = all_by_class(&dets, class::LIST_ITEM).into_iter().cloned().collect();
        let rows: Vec<Rect> = items
            .iter()
            .map(|d| img_rect_to_screen(d.bounds_rect(), window, shot.width, shot.height))
            .collect();
        let mut target = None;
        if let Some(i) = nearest_row(&rows, prev) {
            let boxes = self.yolo_ocr_bbox(window, &shot, &items[i], "点击前复检")?;
            if Self::yolo_first_line_text(&boxes).is_some_and(|b| b.text.contains(name.trim())) {
                target = Some(items[i].clone());
            }
        }
        let target = match target {
            Some(t) => t,
            None => {
                self.evidence.push("点击前复检：原坐标附近已不是目标行（列表可能重排），本帧重新定位".into());
                let m = self.yolo_find_name_in_items(window, &shot, &dets, page, &name, "点击前重定位")?;
                self.ambiguity_guard(&m, &name, page)?;
                m.best.ok_or_else(|| {
                    AutomationError::NeedsHumanReview(format!(
                        "点击前复检：列表已变化，本帧找不到「{}」，停止（不点旧坐标）。",
                        name.trim()
                    ))
                })?
            }
        };
        let row = img_rect_to_screen(target.bounds_rect(), window, shot.width, shot.height);
        let pane = Self::right_of(window, row.x + row.width);
        let before = self.capture_frame(pane, "点击前右侧区")?.fingerprint;
        self.yolo_click_detection(window, &shot, &target, what)?;
        self.forward_verify_file_helper_row_selected(step, name.trim())?;
        let after = self.capture_frame(pane, "点击后右侧区")?.fingerprint;
        self.last_click_reacted = Some(before != after);
        Ok(row)
    }

    /// 窗口里 `x` 右侧的整列。
    pub(super) fn right_of(window: Rect, x: i32) -> Rect {
        let x = x.clamp(window.x, window.x + window.width - 1);
        Rect { x, y: window.y, width: (window.x + window.width - x).max(1), height: window.height }
    }
}
