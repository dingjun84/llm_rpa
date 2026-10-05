//! 转发工作流的 OCR / 点击 / 滚动 / 弹出层辅助（从 `yolo_forward` 拆出）。

use crate::diagnostics::{Decision, Verdict};
use crate::ports::{AutomationError, PeerTopWindow, Point, Rect, TextBox};
use crate::yolo::{
    all_by_class, class, random_point_in_central_half, shot_point_to_screen,
};

use std::time::Instant;

use super::ocr_poll::{HEADER_POLL_BUDGET, HEADER_POLL_INTERVAL, MIN_OCR_ATTEMPTS, PEER_POLL_BUDGET};
use super::Run;

impl Run<'_> {
    /// 整窗 OCR，取阅读序（上→下、左→右）第一个包含 `needle` 的文字框（屏幕坐标）。
    pub(super) fn forward_ocr_first_label(&mut self, needle: &str, region: Option<Rect>) -> Result<Rect, AutomationError> {
        let window = match region {
            Some(r) => r,
            None => self.ensure_calibrated()?,
        };
        let (shot, boxes) = timed!(
            self,
            &format!("OCR 单次·「{needle}」"),
            self.capture_and_recognize(window, &format!("转发·OCR「{needle}」"))
        )?;
        let _ = shot;
        let mut hits: Vec<&TextBox> = boxes
            .iter()
            .filter(|b| b.text.contains(needle))
            .collect();
        hits.sort_by_key(|b| (b.bounds.y, b.bounds.x));
        let hit = hits.first().ok_or_else(|| {
            AutomationError::NeedsHumanReview(format!(
                "未 OCR 到「{needle}」（区域 ({}, {}) {}x{}）。",
                window.x, window.y, window.width, window.height
            ))
        })?;
        Ok(hit.bounds.to_screen(Point {
            x: window.x,
            y: window.y,
        }))
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

    /// 搜索框与结果行之间的最小垂直间隙（像素）。
    /// 命中顶边须 ≥ anchor 底边 + GAP，排除搜索框带内刚输入的文字。
    const FORWARD_BELOW_GAP_PX: i32 = 10;

    /// 搜索框下方列表区：y 从 anchor 底边起至 `origin`（窗/弹层）底，宽度跟 origin。
    fn forward_list_region_below(origin: Rect, anchor: Rect) -> Rect {
        let y = (anchor.y + anchor.height).max(origin.y);
        let bottom = origin.y + origin.height;
        Rect {
            x: origin.x,
            y,
            width: origin.width,
            height: (bottom - y).max(1),
        }
    }

    /// 输入后结果行可能晚到：整窗轮询直到列表区相对输入后基线有变化，
    /// 且 `anchor` 下方出现含 `needle` 的框。
    pub(super) fn forward_ocr_below_polled(
        &mut self,
        step: u32,
        needle: &str,
        anchor: Rect,
    ) -> Result<Rect, AutomationError> {
        let window = self.ensure_calibrated()?;
        let budget = HEADER_POLL_BUDGET;
        let interval = HEADER_POLL_INTERVAL;
        let list_region = Self::forward_list_region_below(window, anchor);
        let baseline = self
            .capture_frame(list_region, &format!("转发·{step}·列表指纹基准"))?
            .fingerprint;
        let started = Instant::now();
        let deadline = started + budget;
        let mut attempts = 0u32;
        let mut list_changed = false;
        loop {
            attempts += 1;
            self.check_cancel()?;
            let current_fp = self
                .capture_frame(list_region, &format!("转发·{step}·列表指纹"))?
                .fingerprint;
            if current_fp != baseline {
                list_changed = true;
            }
            let (_shot, boxes) = self.capture_and_recognize(
                window,
                &format!("转发·{step}·下方「{needle}」"),
            )?;
            if list_changed {
                if let Some(hit) = Self::forward_filter_below(&boxes, window, needle, anchor) {
                    self.evidence.push(format!(
                        "步骤{step}：整窗轮询下方「{needle}」→ 成功（等待 {}ms，OCR {attempts} 次（最少 {} 次）；\
                         列表指纹已变化；预算 {}ms 间隔 {}ms）",
                        started.elapsed().as_millis(),
                        MIN_OCR_ATTEMPTS,
                        budget.as_millis(),
                        interval.as_millis(),
                    ));
                    return Ok(hit);
                }
            }
            let budget_exhausted = Instant::now() + interval >= deadline;
            if attempts >= MIN_OCR_ATTEMPTS && budget_exhausted {
                break;
            }
            std::thread::sleep(interval);
        }
        let changed_zh = if list_changed { "已" } else { "未" };
        self.evidence.push(format!(
            "步骤{step}：整窗轮询下方「{needle}」→ 未找到（等待 {}ms，OCR {attempts} 次（最少 {} 次）；\
             列表指纹{changed_zh}变化；预算 {}ms 间隔 {}ms）",
            started.elapsed().as_millis(),
            MIN_OCR_ATTEMPTS,
            budget.as_millis(),
            interval.as_millis(),
        ));
        Err(AutomationError::NeedsHumanReview(format!(
            "步骤{step}失败：输入后 {}ms 内（OCR {attempts} 次，最少 {} 次）搜索框下方未见「{needle}」；\
             列表指纹{changed_zh}变化，可能搜索结果未刷新。",
            started.elapsed().as_millis(),
            MIN_OCR_ATTEMPTS,
        )))
    }

    /// 从 OCR 框里筛 `anchor` 下方含 `needle` 的第一个（阅读序）。
    /// 命中顶边须 ≥ anchor 底边 + [`Self::FORWARD_BELOW_GAP_PX`]，排除搜索框带内文字。
    fn forward_filter_below(
        boxes: &[TextBox],
        origin: Rect,
        needle: &str,
        anchor: Rect,
    ) -> Option<Rect> {
        let min_y = anchor.y + anchor.height + Self::FORWARD_BELOW_GAP_PX;
        let mut hits: Vec<Rect> = boxes
            .iter()
            .filter(|b| b.text.contains(needle))
            .map(|b| {
                b.bounds.to_screen(Point {
                    x: origin.x,
                    y: origin.y,
                })
            })
            .filter(|r| r.y >= min_y)
            .collect();
        hits.sort_by_key(|r| (r.y, r.x));
        hits.into_iter().next()
    }

    /// 输入联系人后结果行可能晚到：在 `region` 上轮询直到列表区相对输入后基线有变化，
    /// 且 `anchor` 下方出现最大匹配。
    pub(super) fn forward_ocr_best_below_polled(
        &mut self,
        step: u32,
        needle: &str,
        anchor: Rect,
        region: Rect,
    ) -> Result<Rect, AutomationError> {
        let budget = PEER_POLL_BUDGET;
        let interval = HEADER_POLL_INTERVAL;
        let list_region = Self::forward_list_region_below(region, anchor);
        let baseline = self
            .capture_frame(list_region, &format!("转发·{step}·列表指纹基准"))?
            .fingerprint;
        let started = Instant::now();
        let deadline = started + budget;
        let mut attempts = 0u32;
        let mut list_changed = false;
        loop {
            attempts += 1;
            self.check_cancel()?;
            let current_fp = self
                .capture_frame(list_region, &format!("转发·{step}·列表指纹"))?
                .fingerprint;
            if current_fp != baseline {
                list_changed = true;
            }
            let (_shot, boxes) = self.capture_and_recognize(
                region,
                &format!("转发·{step}·匹配「{needle}」"),
            )?;
            if list_changed {
                if let Some(hit) = Self::forward_filter_best_below(&boxes, region, needle, anchor)
                {
                    self.evidence.push(format!(
                        "步骤{step}：弹层轮询下方匹配「{needle}」→ 成功（等待 {}ms，OCR {attempts} 次（最少 {} 次）；\
                         列表指纹已变化；预算 {}ms 间隔 {}ms）",
                        started.elapsed().as_millis(),
                        MIN_OCR_ATTEMPTS,
                        budget.as_millis(),
                        interval.as_millis(),
                    ));
                    return Ok(hit);
                }
            }
            let budget_exhausted = Instant::now() + interval >= deadline;
            if attempts >= MIN_OCR_ATTEMPTS && budget_exhausted {
                break;
            }
            std::thread::sleep(interval);
        }
        let changed_zh = if list_changed { "已" } else { "未" };
        self.evidence.push(format!(
            "步骤{step}：弹层轮询下方匹配「{needle}」→ 未找到（等待 {}ms，OCR {attempts} 次（最少 {} 次）；\
             列表指纹{changed_zh}变化；预算 {}ms 间隔 {}ms）",
            started.elapsed().as_millis(),
            MIN_OCR_ATTEMPTS,
            budget.as_millis(),
            interval.as_millis(),
        ));
        Err(AutomationError::NeedsHumanReview(format!(
            "步骤{step}失败：输入后 {}ms 内（OCR {attempts} 次，最少 {} 次）搜索框下未见包含「{needle}」的匹配行；\
             列表指纹{changed_zh}变化，可能搜索结果未刷新。",
            started.elapsed().as_millis(),
            MIN_OCR_ATTEMPTS,
        )))
    }

    /// 第一个最大包含匹配：面积降序，再阅读序；且须在 `anchor` 下方。
    /// 命中顶边须 ≥ anchor 底边 + [`Self::FORWARD_BELOW_GAP_PX`]。
    fn forward_filter_best_below(
        boxes: &[TextBox],
        origin: Rect,
        needle: &str,
        anchor: Rect,
    ) -> Option<Rect> {
        let min_y = anchor.y + anchor.height + Self::FORWARD_BELOW_GAP_PX;
        let mut hits: Vec<(usize, Rect)> = boxes
            .iter()
            .filter(|b| b.text.contains(needle))
            .map(|b| {
                let screen = b.bounds.to_screen(Point {
                    x: origin.x,
                    y: origin.y,
                });
                (
                    b.bounds.width.max(0) as usize * b.bounds.height.max(0) as usize,
                    screen,
                )
            })
            .filter(|(_, r)| r.y >= min_y)
            .collect();
        hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.y.cmp(&b.1.y)).then(a.1.x.cmp(&b.1.x)));
        hits.into_iter().next().map(|(_, r)| r)
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
        // 多气泡同一时刻 OCR：并发（上限见 ocr_batch::MAX_OCR_CONCURRENCY），
        // 再按原 det 顺序挑第一条命中，证据仍可按序回放。
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

    fn summarize_peers(peers: &[PeerTopWindow]) -> String {
        if peers.is_empty() {
            return "(无)".into();
        }
        peers
            .iter()
            .map(|p| {
                format!(
                    "[id={} class=`{}` title=`{}` {}x{} main={}]",
                    p.id, p.class_name, p.title, p.rect.width, p.rect.height, p.is_main
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// 静默枚举并按 pred 选最大窗（轮询中间不写 evidence，避免刷屏）。
    fn forward_try_pick_peer(
        &mut self,
        pred: &impl Fn(&PeerTopWindow) -> bool,
    ) -> Result<(Vec<PeerTopWindow>, Option<PeerTopWindow>), AutomationError> {
        let peers = self.runner.ports.platform.list_peer_top_windows()?;
        let picked = peers
            .iter()
            .filter(|p| pred(p))
            .max_by_key(|p| (p.rect.width as i64) * (p.rect.height as i64))
            .cloned();
        Ok((peers, picked))
    }

    fn forward_prefer_hit(hits: &mut [Rect], prefer: Prefer) {
        match prefer {
            Prefer::FirstTopLeft => hits.sort_by_key(|r| (r.y, r.x)),
            Prefer::BottomRightMost => {
                hits.sort_by_key(|r| (-(r.y + r.height), -(r.x + r.width)))
            }
        }
    }

    /// 弹出层点文字：预算内反复枚举 peer + OCR，直到出现 `needle` 再按 Prefer 点击。
    /// 覆盖步骤 7（菜单「转发」）、8（「创建聊天」）、11（「创建并发送」）。
    ///
    /// 超时预算只累计「等界面」时间（`sleep(interval)`），不含 `list_peer` /
    /// `capture_and_recognize` 的墙钟；等 OCR 返回后再判断是否超时。
    pub(super) fn forward_click_in_peer(
        &mut self,
        step: u32,
        needle: &str,
        pred: impl Fn(&PeerTopWindow) -> bool,
        prefer: Prefer,
    ) -> Result<(), AutomationError> {
        let budget = PEER_POLL_BUDGET;
        let interval = HEADER_POLL_INTERVAL;
        let wall_started = Instant::now();
        let mut waited = std::time::Duration::ZERO;
        let mut attempts = 0u32;
        let mut ocr_attempts = 0u32;
        let mut last_peers;
        loop {
            attempts += 1;
            self.check_cancel()?;
            let (peers, picked) = self.forward_try_pick_peer(&pred)?;
            last_peers = Self::summarize_peers(&peers);
            let mut did_ocr = false;
            if let Some(peer) = picked {
                ocr_attempts += 1;
                did_ocr = true;
                let (_shot, boxes) = self.capture_and_recognize(
                    peer.rect,
                    &format!("转发·{step}·OCR「{needle}」"),
                )?;
                let mut hits: Vec<Rect> = boxes
                    .iter()
                    .filter(|b| b.text.contains(needle))
                    .map(|b| {
                        b.bounds.to_screen(Point {
                            x: peer.rect.x,
                            y: peer.rect.y,
                        })
                    })
                    .collect();
                if !hits.is_empty() {
                    Self::forward_prefer_hit(&mut hits, prefer);
                    let hit = hits[0];
                    self.evidence.push(format!(
                        "步骤{step}：弹层轮询「{needle}」→ 成功（等待 {}ms，墙钟 {}ms，尝试 {attempts} 次、OCR {ocr_attempts} 次（最少 {} 次）；预算 {}ms 间隔 {}ms；窗 class=`{}` title=`{}` id=`{}`）",
                        waited.as_millis(),
                        wall_started.elapsed().as_millis(),
                        MIN_OCR_ATTEMPTS,
                        budget.as_millis(),
                        interval.as_millis(),
                        peer.class_name,
                        peer.title,
                        peer.id,
                    ));
                    return self.forward_click_screen_box(step, needle, Some(&peer), hit);
                }
            }
            // OCR / list_peer 耗时不计入预算；等本次 OCR 返回后再判断。
            // 优先凑满真正 OCR；从未有窗时退而保证完整尝试循环 ≥ MIN。
            if waited >= budget {
                if ocr_attempts >= MIN_OCR_ATTEMPTS
                    || (!did_ocr && attempts >= MIN_OCR_ATTEMPTS)
                {
                    break;
                }
            }
            std::thread::sleep(interval);
            waited += interval;
        }
        Err(AutomationError::NeedsHumanReview(format!(
            "步骤{step}失败：等待 {}ms（墙钟 {}ms；尝试 {attempts} 次、OCR {ocr_attempts} 次，最少 {} 次）未见弹层「{needle}」。最后顶层窗：{}",
            waited.as_millis(),
            wall_started.elapsed().as_millis(),
            MIN_OCR_ATTEMPTS,
            last_peers,
        )))
    }

    /// 步骤9：轮询直到转发窗出现且 OCR 到「搜索」，返回 (peer, 搜索框屏幕矩形)。
    pub(super) fn forward_ocr_first_in_peer_polled(
        &mut self,
        step: u32,
        needle: &str,
        pred: impl Fn(&PeerTopWindow) -> bool,
    ) -> Result<(PeerTopWindow, Rect), AutomationError> {
        let budget = PEER_POLL_BUDGET;
        let interval = HEADER_POLL_INTERVAL;
        let started = Instant::now();
        let deadline = started + budget;
        let mut attempts = 0u32;
        let mut ocr_attempts = 0u32;
        let mut last_peers;
        loop {
            attempts += 1;
            self.check_cancel()?;
            let (peers, picked) = self.forward_try_pick_peer(&pred)?;
            last_peers = Self::summarize_peers(&peers);
            let mut did_ocr = false;
            if let Some(peer) = picked {
                ocr_attempts += 1;
                did_ocr = true;
                let (_shot, boxes) = self.capture_and_recognize(
                    peer.rect,
                    &format!("转发·{step}·OCR「{needle}」"),
                )?;
                let mut hits: Vec<&TextBox> = boxes
                    .iter()
                    .filter(|b| b.text.contains(needle))
                    .collect();
                hits.sort_by_key(|b| (b.bounds.y, b.bounds.x));
                if let Some(hit) = hits.first() {
                    let screen = hit.bounds.to_screen(Point {
                        x: peer.rect.x,
                        y: peer.rect.y,
                    });
                    self.evidence.push(format!(
                        "步骤{step}：弹层轮询「{needle}」→ 成功（等待 {}ms，尝试 {attempts} 次、OCR {ocr_attempts} 次（最少 {} 次）；预算 {}ms 间隔 {}ms；窗 class=`{}` title=`{}` id=`{}`）",
                        started.elapsed().as_millis(),
                        MIN_OCR_ATTEMPTS,
                        budget.as_millis(),
                        interval.as_millis(),
                        peer.class_name,
                        peer.title,
                        peer.id,
                    ));
                    return Ok((peer, screen));
                }
            }
            let budget_exhausted = Instant::now() + interval >= deadline;
            // 优先凑满真正 OCR；从未有窗时退而保证完整尝试循环 ≥ MIN。
            if budget_exhausted {
                if ocr_attempts >= MIN_OCR_ATTEMPTS
                    || (!did_ocr && attempts >= MIN_OCR_ATTEMPTS)
                {
                    break;
                }
            }
            std::thread::sleep(interval);
        }
        Err(AutomationError::NeedsHumanReview(format!(
            "步骤{step}失败：等待 {}ms（尝试 {attempts} 次、OCR {ocr_attempts} 次，最少 {} 次）未见弹层「{needle}」。最后顶层窗：{}",
            started.elapsed().as_millis(),
            MIN_OCR_ATTEMPTS,
            last_peers,
        )))
    }
}

#[derive(Clone, Copy)]
pub(super) enum Prefer {
    FirstTopLeft,
    BottomRightMost,
}
