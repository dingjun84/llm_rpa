//! 转发工作流的 OCR / 点击 / 滚动 / 弹出层辅助（从 `yolo_forward` 拆出）。
//!
//! **轮询与它的参数不在这里**，在 `yolo_forward_poll.rs`——那一段自成一类
//! （"等某个东西出现，再决定点哪儿"），而且把本文件顶过了 500 行上限。
//! 本文件只剩：一次性的 OCR、落点点击、以及滚动/右击这两个动作。

use crate::diagnostics::{Decision, Verdict};
use crate::ports::{AutomationError, PeerTopWindow, Point, Rect, TextBox};
use crate::yolo::{
    class, random_point_in_central_half,
};

use super::ocr_poll::{HEADER_POLL_BUDGET, HEADER_POLL_INTERVAL, MIN_OCR_ATTEMPTS};
use super::Run;

impl Run<'_> {

    /// 步骤3 校验：输入后、点下拉结果前，确认「搜索框里真的出现了输入的文字」——
    /// 即整窗 OCR 中有含 `needle` 的文字框与步骤2 定位的搜索框（屏幕坐标）**有重叠**。
    /// 不重叠 ⇒ 搜索框定位偏了（字打到别处），转人工，不去点下拉。
    pub(super) fn forward_verify_typed_in_search_box(
        &mut self,
        step: u32,
        needle: &str,
        search_box: Rect,
    ) -> Result<Rect, AutomationError> {
        use super::ocr_poll::{PollBudget, PollOutcome, Round};

        let window = self.ensure_calibrated()?;
        let origin = Point { x: window.x, y: window.y };
        let outcome = self.poll_bounded(
            PollBudget::WallClock(HEADER_POLL_BUDGET),
            HEADER_POLL_INTERVAL,
            |run| {
                let (_shot, boxes) =
                    run.capture_and_recognize(window, &format!("转发·{step}·搜索框内「{needle}」"))?;
                let hit = boxes
                    .iter()
                    .filter(|b| b.text.contains(needle))
                    .map(|b| b.bounds.to_screen(origin))
                    .find(|r| rects_overlap(*r, search_box));
                Ok(match hit {
                    Some(r) => Round::hit(r),
                    None => Round::missed(),
                })
            },
        )?;
        match outcome {
            PollOutcome::Found { value, stats } => {
                self.evidence.push(format!(
                    "步骤{step}：搜索框内已出现「{needle}」（文字框 {:?} 与搜索框 {:?} 重叠；OCR {} 次，墙钟 {}ms）",
                    value, search_box, stats.ocr_attempts, stats.wall.as_millis()
                ));
                Ok(value)
            }
            PollOutcome::Exhausted { stats } => Err(AutomationError::NeedsHumanReview(format!(
                "步骤{step}失败：输入后 {}ms 内（OCR {} 次）未见与搜索框 {:?} 重叠的「{needle}」文字框，\
                 搜索框可能定位不准（字没打进搜索框）。",
                stats.wall.as_millis(), stats.ocr_attempts, search_box
            ))),
        }
    }


    /// 步骤1：若消息导航已是选中态则跳过点击；否则点击后墙钟 2s 内每 200ms
    /// 重检（YOLO 找框 + HSV 蓝占比），超时则转人工。
    ///
    /// 选中判据只走 [`crate::nav_selected::is_nav_icon_selected`]，不在此另写阈值。
    pub(super) fn forward_ensure_message_nav_selected(&mut self) -> Result<(), AutomationError> {
        self.ensure_nav_selected(1, class::NAV_CHAT_ICON, "消息导航")
    }

    /// 通用导航选中：`icon_class` 为 `nav_chat_icon` / `nav_contacts_icon` 等。
    /// 已选中则跳过；否则点击后墙钟 2s 内每 200ms 重检，选中判据只走
    /// [`crate::nav_selected::is_nav_icon_selected`]。
    pub(super) fn ensure_nav_selected(
        &mut self,
        step: u32,
        icon_class: &str,
        what: &str,
    ) -> Result<(), AutomationError> {
        use std::time::{Duration, Instant};

        use crate::nav_selected::{is_nav_icon_selected, nav_icon_selected_ratio, SELECTED_RATIO};
        use crate::yolo::best_by_class;

        const POLL_BUDGET: Duration = Duration::from_millis(2000);
        const POLL_INTERVAL: Duration = Duration::from_millis(200);

        let (window, shot, dets) = self.yolo_detect_window(&format!("{step}·{what}"))?;
        let nav = self.yolo_require_class(
            &dets,
            icon_class,
            "请确认企业微信左侧导航可见。",
        )?;
        let bounds = nav.bounds_rect();
        let ratio = nav_icon_selected_ratio(&shot, bounds);
        if is_nav_icon_selected(&shot, bounds) {
            self.evidence.push(format!(
                "步骤{step}：{what}已选中（蓝占比 {ratio:.3} ≥ {SELECTED_RATIO}），跳过点击"
            ));
            return Ok(());
        }
        self.evidence.push(format!(
            "步骤{step}：{what}未选中（蓝占比 {ratio:.3} < {SELECTED_RATIO}），点击并轮询选中态"
        ));
        self.log_click_ctx(step, icon_class, None, window, &shot, bounds)?;
        self.yolo_click_detection(window, &shot, nav, what)?;

        // 超时从点击后**发起检测**起算（不含点击本身）。
        let started = Instant::now();
        let mut attempts = 0u32;
        let mut last_ratio = 0.0f32;
        let mut saw_icon = false;
        loop {
            self.check_cancel()?;
            attempts += 1;
            let (_w, shot, dets) = self.yolo_detect_window(&format!("{step}·{what}·选中态·第{attempts}次"))?;
            match best_by_class(&dets, icon_class) {
                Some(nav) => {
                    saw_icon = true;
                    let bounds = nav.bounds_rect();
                    last_ratio = nav_icon_selected_ratio(&shot, bounds);
                    if is_nav_icon_selected(&shot, bounds) {
                        self.evidence.push(format!(
                            "步骤{step}：点击后第 {attempts} 次检出选中（蓝占比 {last_ratio:.3}，墙钟 {}ms）",
                            started.elapsed().as_millis()
                        ));
                        return Ok(());
                    }
                }
                None => {
                    self.evidence.push(format!(
                        "步骤{step}：第 {attempts} 次未检出 {icon_class}（墙钟 {}ms）",
                        started.elapsed().as_millis()
                    ));
                }
            }
            if started.elapsed() >= POLL_BUDGET {
                break;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
        Err(AutomationError::NeedsHumanReview(format!(
            "步骤{step}失败：点击{what}后 {}ms 内轮询 {attempts} 次仍未选中             （最后蓝占比 {last_ratio:.3}，阈值 {SELECTED_RATIO}；              是否检出图标：{}）。请确认企业微信左侧该导航图标变为蓝色选中态。",
            started.elapsed().as_millis(),
            if saw_icon { "有" } else { "无" },
        )))
    }

    /// 点完导航后页面可能还没切过来：整窗上 `ocr_until_contains` 轮询，
    /// 再取阅读序第一个含 `needle` 的框（屏幕坐标）。证据对齐步骤4风格。
    pub(super) fn forward_ocr_first_label_polled(
        &mut self,
        step: u32,
        needle: &str,
        what: &str,
    ) -> Result<Rect, AutomationError> {
        let window = self.ensure_calibrated()?;
        let poll = timed!(
            self,
            &format!("ocr_until_contains 轮询·「{needle}」"),
            self.ocr_until_contains(
                window,
                &format!("转发·{step}·「{needle}」"),
                needle,
                HEADER_POLL_BUDGET,
                HEADER_POLL_INTERVAL,
            )
        )?;
        self.evidence.push(format!(
            "步骤{step}：整窗轮询「{needle}」→ {}（等待 {}ms，OCR {} 次（最少 {} 次），\
             停稳截帧 {} 次，最后一次读前{}停稳；预算 {}ms 间隔 {}ms）",
            if poll.found { "成功" } else { "未找到" },
            poll.waited.as_millis(),
            poll.ocr_attempts,
            MIN_OCR_ATTEMPTS,
            poll.settle_frames,
            if poll.settled { "已" } else { "未" },
            HEADER_POLL_BUDGET.as_millis(),
            HEADER_POLL_INTERVAL.as_millis(),
        ));
        if !poll.found {
            return Err(AutomationError::NeedsHumanReview(format!(
                "步骤{step}失败：点击导航后 {}ms 内（OCR {} 次，最后一次读前{}停稳）未见「{needle}」（{what}），\
                 可能消息页尚未切过来。",
                poll.waited.as_millis(),
                poll.ocr_attempts,
                if poll.settled { "已" } else { "未" },
            )));
        }
        let mut hits: Vec<&TextBox> = poll
            .boxes
            .iter()
            .filter(|b| b.text.contains(needle))
            .collect();
        hits.sort_by_key(|b| (b.bounds.y, b.bounds.x));
        let hit = hits.first().ok_or_else(|| {
            AutomationError::NeedsHumanReview(format!(
                "步骤{step}失败：轮询声称找到「{needle}」但 boxes 中无匹配框。"
            ))
        })?;
        Ok(hit.bounds.to_screen(Point {
            x: window.x,
            y: window.y,
        }))
    }

    pub(super) fn forward_click_screen_box(
        &mut self,
        step: u32,
        what: &str,
        peer: Option<&PeerTopWindow>,
        screen_box: Rect,
    ) -> Result<(), AutomationError> {
        let target = random_point_in_central_half(screen_box);
        let (cls, title, id, wrect) = match peer {
            Some(p) => (
                p.class_name.as_str(),
                p.title.as_str(),
                p.id.as_str(),
                p.rect,
            ),
            None => {
                let w = self.window.unwrap_or(screen_box);
                ("(main)", "(main)", "-", w)
            }
        };
        self.evidence.push(format!(
            "步骤{step}：找「{what}」框 ({}, {}) {}x{} → 点 ({}, {})；\
             窗 class=`{cls}` title=`{title}` id=`{id}`；窗 ({}, {}) {}x{}",
            screen_box.x,
            screen_box.y,
            screen_box.width,
            screen_box.height,
            target.x,
            target.y,
            wrect.x,
            wrect.y,
            wrect.width,
            wrect.height
        ));
        let overlay = TextBox {
            text: what.into(),
            bounds: Rect {
                x: screen_box.x - wrect.x,
                y: screen_box.y - wrect.y,
                width: screen_box.width,
                height: screen_box.height,
            },
            confidence: 1.0,
        };
        // 不抢焦点：直接 capture 已有区域可能失败；用 peer/main 矩形做一次观测
        if let Ok(shot) = self.runner.ports.platform.capture(wrect) {
            self.report(
                &format!("点击{what}"),
                wrect,
                &shot,
                &[overlay.clone()],
                None,
                None,
            );
        }
        self.report_decision(
            &format!("点击{what}"),
            Decision {
                step: String::new(),
                question: format!("步骤{step}要点击哪个「{what}」？"),
                rule: "OCR 框 → random_point_in_central_half → guarded_click".into(),
                outcome: format!("屏幕 ({}, {})", target.x, target.y),
                passed: true,
                min_confidence: 0.0,
                replay: None,
                candidates: vec![Verdict::passed(&overlay, format!("落点 ({}, {})", target.x, target.y))],
            },
        );
        let expected = self.window.unwrap_or(wrect);
        self.ensure_not_frozen(&format!("已取消点击{what}"))?;
        self.runner
            .ports
            .platform
            .guarded_click(target, expected)?;
        self.check_deadline(&format!("点击{what}"))
    }

    pub(super) fn forward_verify_chat_header(
        &mut self,
        step: u32,
        file_helper: &str,
        hit: Rect,
    ) -> Result<(), AutomationError> {
        let window = self.ensure_calibrated()?;
        // 命中点右上方附近：取命中点右侧偏上一条带
        let band = Rect {
            x: hit.x + hit.width / 2,
            y: window.y,
            width: (window.x + window.width - (hit.x + hit.width / 2)).max(1),
            height: (hit.y + hit.height * 2 - window.y).max(40).min(window.height),
        };
        // 点完立刻 OCR 会读到切换前的旧标题（任务 39a1754e）：先等标题带停稳再读，
        // 读不到就在预算内重试，见 `ocr_poll`。
        let poll = timed!(
            self,
            "ocr_until_contains 轮询·会话标题",
            self.ocr_until_contains(
                band,
                &format!("转发·{step}·会话标题带"),
                file_helper,
                HEADER_POLL_BUDGET,
                HEADER_POLL_INTERVAL,
            )
        )?;
        let read = if self.cfg().log_ocr_candidates && !poll.found {
            format!("；最后一次读到：{}", crate::candidates::describe_candidates(&poll.boxes))
        } else {
            String::new()
        };
        self.evidence.push(format!(
            "步骤{step}：在命中点右上方区域找「{file_helper}」→ {}（等待 {}ms，OCR {} 次（最少 {} 次），\
             停稳截帧 {} 次，最后一次读前{}停稳；预算 {}ms 间隔 {}ms）{read}",
            if poll.found { "成功" } else { "未找到" },
            poll.waited.as_millis(),
            poll.ocr_attempts,
            MIN_OCR_ATTEMPTS,
            poll.settle_frames,
            if poll.settled { "已" } else { "未" },
            HEADER_POLL_BUDGET.as_millis(),
            HEADER_POLL_INTERVAL.as_millis(),
        ));
        if !poll.found {
            return Err(AutomationError::NeedsHumanReview(format!(
                "步骤{step}失败：点击后 {}ms 内（OCR {} 次）右上方未见「{file_helper}」，可能未进入会话。",
                poll.waited.as_millis(),
                poll.ocr_attempts
            )));
        }
        Ok(())
    }
}

/// 两个屏幕矩形是否有正面积重叠。
fn rects_overlap(a: Rect, b: Rect) -> bool {
    a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
}

#[cfg(test)]
mod overlap_tests {
    use super::*;
    fn r(x: i32, y: i32, w: i32, h: i32) -> Rect { Rect { x, y, width: w, height: h } }
    #[test]
    fn overlap_cases() {
        assert!(rects_overlap(r(0, 0, 10, 10), r(5, 5, 10, 10)));
        assert!(!rects_overlap(r(0, 0, 10, 10), r(10, 0, 5, 5)));
        assert!(!rects_overlap(r(0, 0, 10, 10), r(0, 30, 10, 10)));
    }
}
