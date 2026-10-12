//! 转发工作流的 OCR / 点击 / 滚动 / 弹出层辅助（从 `yolo_forward` 拆出）。
//!
//! **轮询与它的参数不在这里**，在 `yolo_forward_poll.rs`——那一段自成一类
//! （"等某个东西出现，再决定点哪儿"），而且把本文件顶过了 500 行上限。
//! 本文件只剩：一次性的 OCR、落点点击、以及滚动/右击这两个动作。

use crate::diagnostics::{Decision, Verdict};
use crate::ports::{AutomationError, PeerTopWindow, Point, Rect, TextBox};
use crate::yolo::{
    all_by_class, class, random_point_in_central_half, shot_point_to_screen,
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
        use std::time::{Duration, Instant};

        use crate::nav_selected::{is_nav_icon_selected, nav_icon_selected_ratio, SELECTED_RATIO};
        use crate::yolo::best_by_class;

        const POLL_BUDGET: Duration = Duration::from_millis(2000);
        const POLL_INTERVAL: Duration = Duration::from_millis(200);

        let (window, shot, dets) = self.yolo_detect_window("转发·1·导航消息")?;
        let nav = self.yolo_require_class(
            &dets,
            class::NAV_CHAT_ICON,
            "请确认企业微信左侧导航可见。",
        )?;
        let bounds = nav.bounds_rect();
        let ratio = nav_icon_selected_ratio(&shot, bounds);
        if is_nav_icon_selected(&shot, bounds) {
            self.evidence.push(format!(
                "步骤1：消息导航已选中（蓝占比 {ratio:.3} ≥ {SELECTED_RATIO}），跳过点击"
            ));
            return Ok(());
        }
        self.evidence.push(format!(
            "步骤1：消息导航未选中（蓝占比 {ratio:.3} < {SELECTED_RATIO}），点击并轮询选中态"
        ));
        self.log_click_ctx(1, "nav_chat_icon", None, window, &shot, bounds)?;
        self.yolo_click_detection(window, &shot, nav, "消息导航")?;

        // 超时从点击后**发起检测**起算（不含点击本身）。
        let started = Instant::now();
        let mut attempts = 0u32;
        let mut last_ratio = 0.0f32;
        let mut saw_icon = false;
        loop {
            self.check_cancel()?;
            attempts += 1;
            let (_w, shot, dets) = self.yolo_detect_window(&format!("转发·1·选中态·第{attempts}次"))?;
            match best_by_class(&dets, class::NAV_CHAT_ICON) {
                Some(nav) => {
                    saw_icon = true;
                    let bounds = nav.bounds_rect();
                    last_ratio = nav_icon_selected_ratio(&shot, bounds);
                    if is_nav_icon_selected(&shot, bounds) {
                        self.evidence.push(format!(
                            "步骤1：点击后第 {attempts} 次检出选中（蓝占比 {last_ratio:.3}，墙钟 {}ms）",
                            started.elapsed().as_millis()
                        ));
                        return Ok(());
                    }
                }
                None => {
                    self.evidence.push(format!(
                        "步骤1：第 {attempts} 次未检出 nav_chat_icon（墙钟 {}ms）",
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
            "步骤1失败：点击消息导航后 {}ms 内轮询 {attempts} 次仍未选中             （最后蓝占比 {last_ratio:.3}，阈值 {SELECTED_RATIO}；              是否检出图标：{}）。请确认企业微信左侧消息图标变为蓝色选中态。",
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
                "转发·4·会话标题带",
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
            "步骤4：在命中点右上方区域找「{file_helper}」→ {}（等待 {}ms，OCR {} 次（最少 {} 次），\
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
                "步骤4失败：点击后 {}ms 内（OCR {} 次）右上方未见「{file_helper}」，可能未进入会话。",
                poll.waited.as_millis(),
                poll.ocr_attempts
            )));
        }
        Ok(())
    }

    pub(super) fn forward_scroll_chat_to_bottom(&mut self) -> Result<(), AutomationError> {
        let scroll_started = std::time::Instant::now();
        let (window, shot, dets) = self.yolo_detect_window("转发·5·气泡区")?;
        let bubbles: Vec<_> = all_by_class(&dets, class::INCOMING_BUBBLE)
            .into_iter()
            .chain(all_by_class(&dets, class::OUTGOING_BUBBLE))
            .collect();
        self.evidence.push(format!(
            "步骤5：检出气泡 {} 个（incoming+outgoing）",
            bubbles.len()
        ));
        let at = if bubbles.is_empty() {
            Point {
                x: window.x + (window.width as f32 * 0.65).round() as i32,
                y: window.y + window.height / 2,
            }
        } else {
            let mut x1 = i32::MAX;
            let mut y1 = i32::MAX;
            let mut x2 = i32::MIN;
            let mut y2 = i32::MIN;
            for b in &bubbles {
                let r = b.bounds_rect();
                x1 = x1.min(r.x);
                y1 = y1.min(r.y);
                x2 = x2.max(r.x + r.width);
                y2 = y2.max(r.y + r.height);
            }
            shot_point_to_screen((x1 + x2) / 2, (y1 + y2) / 2, window, shot.width, shot.height)
        };
        self.evidence.push(format!(
            "步骤5：移到消息区中部 ({}, {})，缓慢滚轮到底",
            at.x, at.y
        ));
        self.runner.ports.platform.move_pointer(at)?;
        let expected = window;

        // 临时关闭：任务489c反馈定位到右击太慢
        // 「判断消息是否滚到最后一条」：原逻辑每次 scroll(1) + scroll_settle_timeout + 指纹比对，
        // 直到画面不变才算到底；聊天未在底部时会滚很多轮、每轮都长等。先短路退出该判断。
        const DISABLE_SCROLL_BOTTOM_DETECT: bool = true; // 临时关闭：任务489c反馈定位到右击太慢
        if DISABLE_SCROLL_BOTTOM_DETECT {
            // 保留少量滚轮（一次多格），短停后直接继续；不做「滚后指纹不变才算到底」循环。
            self.runner
                .ports
                .platform
                .scroll(at, 3, expected)?;
            std::thread::sleep(std::time::Duration::from_millis(150));
            self.evidence.push(
                "步骤5：临时关闭「判断已滚到最后」；仅少量滚动后继续（任务489c）".into(),
            );
            self.note_elapsed(
                "滚轮 scroll_to_bottom",
                concat!(file!(), ":", line!()),
                scroll_started.elapsed(),
            );
            return Ok(());
        }

        // --- 以下为原「滚到最后 / 指纹停稳到底」逻辑（保留，开关关上即可恢复）---
        let mut prev = self
            .runner
            .ports
            .platform
            .capture(window)?
            .fingerprint
            .clone();
        let limit = self.cfg().max_scroll_attempts.max(1) + 2;
        for i in 0..limit {
            // 缓慢：每次 1 格
            self.runner.ports.platform.scroll(at, 1, expected)?;
            std::thread::sleep(self.cfg().scroll_settle_timeout);
            let next = self.runner.ports.platform.capture(window)?.fingerprint;
            if next == prev {
                self.evidence
                    .push(format!("步骤5：第{}次滚动后画面无变化 → 已到底", i + 1));
                self.note_elapsed(
                    "滚轮 scroll_to_bottom",
                    concat!(file!(), ":", line!()),
                    scroll_started.elapsed(),
                );
                return Ok(());
            }
            prev = next;
        }
        self.evidence
            .push("步骤5：达到滚动上限，按已到底继续".into());
        self.note_elapsed(
            "滚轮 scroll_to_bottom",
            concat!(file!(), ":", line!()),
            scroll_started.elapsed(),
        );
        Ok(())
    }

    pub(super) fn forward_right_click_bubble(&mut self, message_text: &str) -> Result<(), AutomationError> {
        let (window, shot, dets) = self.yolo_detect_window("转发·6·找气泡")?;
        let bubbles: Vec<_> = all_by_class(&dets, class::INCOMING_BUBBLE)
            .into_iter()
            .chain(all_by_class(&dets, class::OUTGOING_BUBBLE))
            .cloned()
            .collect();
        if bubbles.is_empty() {
            return Err(AutomationError::NeedsHumanReview(
                "步骤6失败：未检出 incoming_bubble / outgoing_bubble。".into(),
            ));
        }
        // 多气泡同一时刻 OCR：从整窗截图裁切后并发识别（上限见
        // ocr_batch::MAX_OCR_CONCURRENCY），再按原 det 顺序挑第一条命中，
        // 证据仍可按序回放。
        let ocr_by_bubble = self.yolo_ocr_bboxes(window, &shot, &bubbles, "转发·6·气泡OCR")?;
        let det = bubbles
            .iter()
            .zip(ocr_by_bubble.into_iter())
            .find(|(_, boxes)| boxes.iter().any(|b| b.text.contains(message_text)))
            .map(|(det, _)| det.clone())
            .ok_or_else(|| {
                AutomationError::NeedsHumanReview(format!(
                    "步骤6失败：气泡中未找到包含「{message_text}」的内容。"
                ))
            })?;
        let img_pt = random_point_in_central_half(det.bounds_rect());
        let target = shot_point_to_screen(img_pt.x, img_pt.y, window, shot.width, shot.height);
        self.evidence.push(format!(
            "步骤6：右击气泡 {} conf={:.2} → 屏幕 ({}, {})；窗 (main) ({}, {}) {}x{}",
            det.class_name, det.conf, target.x, target.y, window.x, window.y, window.width, window.height
        ));
        let expected = window;
        self.ensure_not_frozen("已取消右击气泡")?;
        timed!(self, "右击 guarded_right_click", {
            self.runner
                .ports
                .platform
                .guarded_right_click(target, expected)
        })?;
        self.check_deadline("右击气泡")
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
