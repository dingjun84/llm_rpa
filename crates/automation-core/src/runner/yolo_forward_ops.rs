//! 转发工作流的 OCR / 点击 / 滚动 / 弹出层辅助（从 `yolo_forward` 拆出）。

use crate::diagnostics::{Decision, Verdict};
use crate::ports::{AutomationError, PeerTopWindow, Point, Rect, TextBox};
use crate::yolo::{
    all_by_class, class, random_point_in_central_half, shot_point_to_screen,
};

use super::ocr_poll::{HEADER_POLL_BUDGET, HEADER_POLL_INTERVAL};
use super::Run;

impl Run<'_> {
    /// 整窗 OCR，取阅读序（上→下、左→右）第一个包含 `needle` 的文字框（屏幕坐标）。
    pub(super) fn forward_ocr_first_label(&mut self, needle: &str, region: Option<Rect>) -> Result<Rect, AutomationError> {
        let window = match region {
            Some(r) => r,
            None => self.ensure_calibrated()?,
        };
        let (shot, boxes) = self.capture_and_recognize(window, &format!("转发·OCR「{needle}」"))?;
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

    pub(super) fn forward_ocr_first_in_rect(&mut self, needle: &str, region: Rect) -> Result<Rect, AutomationError> {
        self.forward_ocr_first_label(needle, Some(region))
    }

    /// 在 `anchor` 下方（y 更大）找包含 `needle` 的文字框。
    pub(super) fn forward_ocr_below(&mut self, needle: &str, anchor: Rect) -> Result<Rect, AutomationError> {
        let window = self.ensure_calibrated()?;
        let (shot, boxes) = self.capture_and_recognize(window, &format!("转发·下方「{needle}」"))?;
        let _ = shot;
        let mut hits: Vec<Rect> = boxes
            .iter()
            .filter(|b| b.text.contains(needle))
            .map(|b| {
                b.bounds.to_screen(Point {
                    x: window.x,
                    y: window.y,
                })
            })
            .filter(|r| r.y > anchor.y + anchor.height / 2)
            .collect();
        hits.sort_by_key(|r| (r.y, r.x));
        hits.into_iter().next().ok_or_else(|| {
            AutomationError::NeedsHumanReview(format!(
                "搜索框下方未找到「{needle}」。请确认搜索结果已弹出。"
            ))
        })
    }

    pub(super) fn forward_ocr_best_below(
        &mut self,
        needle: &str,
        anchor: Rect,
        region: Rect,
    ) -> Result<Rect, AutomationError> {
        let (shot, boxes) = self.capture_and_recognize(region, &format!("转发·匹配「{needle}」"))?;
        let _ = shot;
        let mut hits: Vec<(usize, Rect)> = boxes
            .iter()
            .filter(|b| b.text.contains(needle))
            .map(|b| {
                let screen = b.bounds.to_screen(Point {
                    x: region.x,
                    y: region.y,
                });
                (b.bounds.width.max(0) as usize * b.bounds.height.max(0) as usize, screen)
            })
            .filter(|(_, r)| r.y > anchor.y + anchor.height / 2)
            .collect();
        // 第一个最大包含匹配：先按面积降序，再按阅读序
        hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.y.cmp(&b.1.y)).then(a.1.x.cmp(&b.1.x)));
        hits.into_iter()
            .next()
            .map(|(_, r)| r)
            .ok_or_else(|| {
                AutomationError::NeedsHumanReview(format!(
                    "搜索框下未找到包含「{needle}」的匹配行。"
                ))
            })
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
        let poll = self.ocr_until_contains(
            band,
            "转发·4·会话标题带",
            file_helper,
            HEADER_POLL_BUDGET,
            HEADER_POLL_INTERVAL,
        )?;
        let read = if self.cfg().log_ocr_candidates && !poll.found {
            format!("；最后一次读到：{}", crate::candidates::describe_candidates(&poll.boxes))
        } else {
            String::new()
        };
        self.evidence.push(format!(
            "步骤4：在命中点右上方区域找「{file_helper}」→ {}（等待 {}ms，OCR {} 次，\
             停稳截帧 {} 次，最后一次读前{}停稳；预算 {}ms 间隔 {}ms）{read}",
            if poll.found { "成功" } else { "未找到" },
            poll.waited.as_millis(),
            poll.ocr_attempts,
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
                return Ok(());
            }
            prev = next;
        }
        self.evidence
            .push("步骤5：达到滚动上限，按已到底继续".into());
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
        self.runner
            .ports
            .platform
            .guarded_right_click(target, expected)?;
        self.check_deadline("右击气泡")
    }

    pub(super) fn forward_pick_peer(
        &mut self,
        pred: impl Fn(&PeerTopWindow) -> bool,
    ) -> Result<PeerTopWindow, AutomationError> {
        let peers = self.runner.ports.platform.list_peer_top_windows()?;
        self.evidence.push(format!(
            "同进程顶层窗 {} 个：{}",
            peers.len(),
            peers
                .iter()
                .map(|p| format!(
                    "[id={} class=`{}` title=`{}` {}x{} main={}]",
                    p.id, p.class_name, p.title, p.rect.width, p.rect.height, p.is_main
                ))
                .collect::<Vec<_>>()
                .join("; ")
        ));
        peers
            .into_iter()
            .filter(|p| pred(p))
            .max_by_key(|p| (p.rect.width as i64) * (p.rect.height as i64))
            .ok_or_else(|| {
                AutomationError::NeedsHumanReview(
                    "未找到符合条件的同进程弹出窗（菜单/转发对话框）。".into(),
                )
            })
    }

    pub(super) fn forward_click_in_peer(
        &mut self,
        step: u32,
        needle: &str,
        pred: impl Fn(&PeerTopWindow) -> bool,
        prefer: Prefer,
    ) -> Result<(), AutomationError> {
        let peer = self.forward_pick_peer(pred)?;
        let (shot, boxes) =
            self.capture_and_recognize(peer.rect, &format!("转发·{step}·OCR「{needle}」"))?;
        let _ = shot;
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
        if hits.is_empty() {
            return Err(AutomationError::NeedsHumanReview(format!(
                "步骤{step}失败：在窗 class=`{}` title=`{}` id=`{}` 内未找到「{needle}」。",
                peer.class_name, peer.title, peer.id
            )));
        }
        match prefer {
            Prefer::FirstTopLeft => hits.sort_by_key(|r| (r.y, r.x)),
            Prefer::BottomRightMost => hits.sort_by_key(|r| (-(r.y + r.height), -(r.x + r.width))),
        }
        let hit = hits[0];
        self.forward_click_screen_box(step, needle, Some(&peer), hit)
    }
}

pub(super) enum Prefer {
    FirstTopLeft,
    BottomRightMost,
}
