//! 会话列表发送（A5–A9）/ 通讯录搜索发送（B9–B13）共用的后半段：
//! 核验聊天标题 → 聚焦输入框 → 逐字输入并 OCR 确认 → 点发送 → 确认输入框清空 → 核验送达。
//!
//! 每一步都是"动作 + 有界轮询确认"，判据在 [`super::yolo_verify_rules`]。

use crate::audit::MessageDigest;
use crate::ports::{AutomationError, Rect};
use crate::state::TaskState;
use crate::yolo::{best_by_class, class, estimate_message_input_region};

use super::ocr_poll::{PollBudget, PollOutcome, Round, HEADER_POLL_BUDGET, HEADER_POLL_INTERVAL, PEER_POLL_BUDGET};
use super::yolo_forward_scroll::{chat_area_img, img_rect_to_screen};
use super::yolo_verify_rules::{boxes_contain_text, judge_delivery, DeliveryVerdict};
use super::Run;

impl Run<'_> {
    /// 失败文案补上"那次点击有没有让画面变化"（`last_click_reacted`）。
    pub(super) fn annotate_click_effect(&self, err: AutomationError) -> AutomationError {
        let effect = match self.last_click_reacted {
            Some(true) => "点击后画面有变化：点击已生效，但可能点错了人或未切到该会话",
            Some(false) => "点击后画面没有任何变化：点击可能未生效（客户端卡死 / 被遮挡 / 点到空白）",
            None => "未记录点击效果",
        };
        match err {
            AutomationError::NeedsHumanReview(msg) => {
                AutomationError::NeedsHumanReview(format!("{msg}（{effect}）"))
            }
            other => other,
        }
    }

    /// A5–A9 / B9–B13。`anchor` 见 [`super::yolo_verify_rules::header_anchor`]。
    pub(super) fn yolo_verify_chat_and_send(
        &mut self,
        step: u32,
        name: &str,
        anchor: Rect,
    ) -> Result<(), AutomationError> {
        // ── 标题核验 ──
        if let Err(err) = self.forward_verify_chat_header(step, name, anchor) {
            return Err(self.annotate_click_effect(err));
        }
        self.advance(TaskState::PreparingMessage, None)?;

        let text = self.task.text.clone();
        if text.trim().is_empty() {
            return Err(AutomationError::NeedsHumanReview("消息正文为空".into()));
        }
        let (window, shot, dets) = self.yolo_detect_window("聊天页确认")?;
        let input_img = estimate_message_input_region(&dets)
            .or_else(|| best_by_class(&dets, class::MESSAGE_INPUT).map(|d| d.bounds_rect()))
            .ok_or_else(|| {
                AutomationError::NeedsHumanReview(
                    "聊天页未检出 input_bar / send_button（亦无遗留 message_input），无法聚焦输入框。".into(),
                )
            })?;
        let input = img_rect_to_screen(input_img, window, shot.width, shot.height);
        let chat_area = img_rect_to_screen(chat_area_img(shot.width, shot.height, &dets), window, shot.width, shot.height);
        self.yolo_click_image_rect_jittered(window, &shot, input_img, "输入框")?;

        // ── 输入 + OCR 确认；不在则清空重输一次 ──
        self.type_body(&text)?;
        if !self.input_shows_text(input, &text, "输入后")? {
            self.evidence.push("输入框 OCR 未见正文 → 清空后重输一次".into());
            let expected = self.ensure_calibrated()?;
            self.runner.ports.platform.clear_text_field(expected)?;
            self.type_body(&text)?;
            if !self.input_shows_text(input, &text, "重输后")? {
                return Err(AutomationError::NeedsHumanReview(format!(
                    "逐字输入并重输一次后，输入框 {input:?} 内仍未读到正文，可能焦点不在输入框。"
                )));
            }
        }

        if self.cfg().stop_before_send {
            self.advance(
                TaskState::Prepared,
                Some(format!("已把 {} 个字符逐字填入输入框，未发送", text.chars().count())),
            )?;
            return Ok(());
        }

        // ── 发送前再确认一次正文仍在 ──
        let (_s, boxes) = self.capture_and_recognize(input, "发送前·输入框")?;
        if !boxes_contain_text(&boxes, &text) {
            return Err(AutomationError::NeedsHumanReview(
                "点发送前输入框里已读不到正文（可能被清空或焦点变了），未发送。".into(),
            ));
        }

        self.runner.ledger.claim(self.task.id)?;
        self.message_digest = Some(MessageDigest::of(&text));
        self.advance(TaskState::Sending, None)?;
        let before_fp = self.capture_frame(chat_area, "发送前聊天区")?.fingerprint;
        let (w, s, d) = self.yolo_detect_window("发送前检测")?;
        let send = self
            .yolo_require_class(&d, class::SEND_BUTTON, "请确认输入区可见且发送按钮未被遮挡。")?
            .clone();
        self.yolo_click_detection(w, &s, &send, "发送按钮")?;

        // ── 点击后输入框应清空 ──
        if !self.input_cleared(input, &text)? {
            return Err(AutomationError::NeedsHumanReview(
                "点击发送后输入框里仍是正文，发送可能未生效。".into(),
            ));
        }

        self.advance(TaskState::VerifyingDelivery, None)?;
        self.verify_delivery_polled(chat_area, &before_fp, &text)?;
        self.advance(TaskState::Completed, Some("聊天区已出现本条消息".into()))
    }

    fn type_body(&mut self, text: &str) -> Result<(), AutomationError> {
        let expected = self.ensure_calibrated()?;
        self.evidence.push(format!("逐字输入正文（{} 字）", text.chars().count()));
        self.ensure_not_frozen("已取消填入消息正文")?;
        self.runner.ports.platform.type_text(text, expected)?;
        self.check_deadline("输入正文")
    }

    /// 有界轮询：输入框 OCR 读到正文前缀即 `true`。
    fn input_shows_text(&mut self, input: Rect, text: &str, what: &str) -> Result<bool, AutomationError> {
        let label = format!("输入框确认·{what}");
        let outcome = self.poll_bounded(PollBudget::WallClock(HEADER_POLL_BUDGET), HEADER_POLL_INTERVAL, |run| {
            let (_s, boxes) = run.capture_and_recognize(input, &label)?;
            Ok(if boxes_contain_text(&boxes, text) { Round::hit(()) } else { Round::missed() })
        })?;
        let (ok, stats) = match outcome {
            PollOutcome::Found { stats, .. } => (true, stats),
            PollOutcome::Exhausted { stats } => (false, stats),
        };
        self.evidence.push(format!(
            "输入框确认（{what}）：{}（OCR {} 次，墙钟 {}ms）",
            if ok { "已见正文" } else { "未见正文" },
            stats.ocr_attempts,
            stats.wall.as_millis()
        ));
        Ok(ok)
    }

    /// 有界轮询：输入框里不再读到正文即 `true`。
    fn input_cleared(&mut self, input: Rect, text: &str) -> Result<bool, AutomationError> {
        let outcome = self.poll_bounded(PollBudget::WallClock(HEADER_POLL_BUDGET), HEADER_POLL_INTERVAL, |run| {
            let (_s, boxes) = run.capture_and_recognize(input, "发送后·输入框")?;
            Ok(if boxes_contain_text(&boxes, text) { Round::missed() } else { Round::hit(()) })
        })?;
        let ok = matches!(outcome, PollOutcome::Found { .. });
        self.evidence.push(format!("发送后输入框{}", if ok { "已清空" } else { "仍有正文" }));
        Ok(ok)
    }

    /// 送达核验：聊天区指纹相对发送前变了 **且** OCR 读到正文才算完成；有界轮询。
    fn verify_delivery_polled(&mut self, chat_area: Rect, before_fp: &str, text: &str) -> Result<(), AutomationError> {
        let min_conf = self.cfg().min_confidence;
        let mut last = DeliveryVerdict::Unchanged;
        let outcome = self.poll_bounded(PollBudget::WallClock(PEER_POLL_BUDGET), HEADER_POLL_INTERVAL, |run| {
            let (shot, boxes) = run.capture_and_recognize(chat_area, "送达核验识别")?;
            last = judge_delivery(before_fp, &shot.fingerprint, &boxes, text, min_conf);
            Ok(if last == DeliveryVerdict::Confirmed { Round::hit(()) } else { Round::missed() })
        })?;
        match outcome {
            PollOutcome::Found { stats, .. } => {
                self.evidence.push(format!(
                    "送达核验通过：聊天区已变化且读到正文（OCR {} 次，墙钟 {}ms）",
                    stats.ocr_attempts,
                    stats.wall.as_millis()
                ));
                Ok(())
            }
            PollOutcome::Exhausted { stats } => {
                let why = match last {
                    DeliveryVerdict::Unchanged => {
                        let frozen = match self.runner.ports.platform.is_responsive() {
                            Ok(false) => "（且系统判定客户端未响应，疑似卡死）",
                            _ => "",
                        };
                        format!("发送后聊天区截图未发生变化{frozen}，无法确认消息已出现")
                    }
                    _ => "聊天区已变化但未识别到本条消息，拒绝判定为已送达".to_string(),
                };
                Err(AutomationError::NeedsHumanReview(format!(
                    "{why}（{}ms 内 OCR {} 次）",
                    stats.wall.as_millis(),
                    stats.ocr_attempts
                )))
            }
        }
    }
}
