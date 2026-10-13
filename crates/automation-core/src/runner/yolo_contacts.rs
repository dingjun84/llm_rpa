//! 工作流 `contacts_search_send`：通讯录搜索找人并发送（Flow B）。
//!
//! B2 点 `nav_contacts_icon` 并轮询选中态（[`Run::ensure_nav_selected`]）
//! B3 定位搜索框并点到获得焦点（复用转发的 `forward_locate_search_box` / `forward_click_search_box_until_focused`）
//! B4 输入前拍列表基线 → 清空（错误不吞）→ 逐字输入 → 确认字在搜索框里
//! B5 轮询搜索框下方直到出现目标名（无固定 sleep）；超时报出搜索框实际内容
//! B6 结果在一屏内：直接用 B5 的命中；同名多条即停
//! B7 点击命中行 → 选中校验 + 左击重试（不再回退到旧坐标盲点）
//! B8 资料页：先核名字 → 轮询「发消息」→ 点击前后比指纹，最多再点一次；
//!    已有会话直接进了聊天也不算失败，交给标题核验
//! B9–B13 同 Flow A 的 A5–A9（[`Run::yolo_verify_chat_and_send`]）

use crate::diagnostics::{Decision, Verdict};
use crate::ports::{AutomationError, Point, Rect, TextBox};
use crate::state::TaskState;
use crate::yolo::{all_by_class, best_by_class, class, random_point_in_central_half};

use super::ocr_poll::{PollBudget, PollOutcome, Round, HEADER_POLL_BUDGET, HEADER_POLL_INTERVAL, PEER_POLL_BUDGET};
use super::yolo_verify_rules::{count_same_name, header_anchor};
use super::Run;

/// 资料页「发消息」按钮文案（无 YOLO class，只能 OCR）。
const SEND_MESSAGE_LABEL: &str = "发消息";
/// 搜索框 OCR 回退时找的文字。
const LABEL_SEARCH: &str = "搜索";
/// 资料页「发消息」点击后最多再补点几次。
const PROFILE_RECLICKS: u32 = 1;

/// 资料页轮询结论。
enum ProfileState {
    /// 资料页上读到了目标名和「发消息」（屏幕坐标）。
    Profile { button: Rect },
    /// 已有会话：客户端直接进了聊天（检出 input_bar / send_button）。
    AlreadyChat,
}

impl Run<'_> {
    /// Flow B：通讯录搜索 → 资料页发消息 → 聊天发送。
    pub(super) fn run_contacts_search_send(&mut self) -> Result<(), AutomationError> {
        let name = self.task.external_contact_name.trim().to_string();
        if name.is_empty() {
            return Err(AutomationError::NeedsHumanReview("外部联系人名称为空。".into()));
        }
        self.advance(TaskState::NavigatingToView, Some("目标：通讯录导航图标".into()))?;
        self.ensure_nav_selected(2, class::NAV_CONTACTS_ICON, "通讯录导航")?;

        self.advance(TaskState::SearchingContact, Some("通讯录搜索".into()))?;
        let search_box = self.forward_locate_search_box(3, LABEL_SEARCH)?;
        self.forward_click_search_box_until_focused(3, search_box)?;

        let window = self.ensure_calibrated()?;
        let baseline = self.forward_capture_list_baseline(4, window, search_box)?;
        self.runner.ports.platform.clear_text_field(window)?;
        self.evidence.push(format!("步骤4：逐字输入联系人「{name}」以过滤列表"));
        self.runner.ports.platform.type_text(&name, window)?;
        self.check_deadline("输入搜索关键词")?;
        self.forward_verify_typed_in_search_box(4, &name, search_box)?;

        let hit = match self.forward_ocr_best_below_polled(5, &name, search_box, window, &baseline) {
            Ok(hit) => hit,
            Err(AutomationError::NeedsHumanReview(msg)) => {
                let actual = self.read_search_box(search_box)?;
                return Err(AutomationError::NeedsHumanReview(format!(
                    "{msg} 搜索框实际内容：「{actual}」"
                )));
            }
            Err(other) => return Err(other),
        };
        self.yolo_report_search_dropdown_results()?;
        // B6：同屏同名多条即停。
        let list = Self::forward_list_region_below(window, search_box);
        let (_s, boxes) = self.capture_and_recognize(list, "搜索结果·同名检查")?;
        let same = count_same_name(boxes.iter().map(|b| b.text.as_str()), &name);
        if same > 1 {
            return Err(AutomationError::AmbiguousVision(format!(
                "搜索结果里有 {same} 条名字都是「{name}」，无法确定是哪一个，停止。"
            )));
        }

        self.advance(TaskState::VerifyingCandidate, Some("命中联系人搜索结果".into()))?;
        let pane = Self::right_of(window, hit.x + hit.width);
        let before = self.capture_frame(pane, "点击前右侧区")?.fingerprint;
        self.forward_click_screen_box(7, "联系人搜索结果", None, hit)?;
        self.forward_verify_file_helper_row_selected(7, &name)?;
        let after = self.capture_frame(pane, "点击后右侧区")?.fingerprint;
        self.last_click_reacted = Some(before != after);

        self.advance(TaskState::VerifyingProfile, Some("资料页核名并找「发消息」".into()))?;
        match self.yolo_poll_profile(&name)? {
            ProfileState::AlreadyChat => {
                self.evidence.push("步骤8：未停在资料页，已直接进入聊天（已有会话），交给标题核验".into());
            }
            ProfileState::Profile { button } => {
                self.advance(TaskState::OpeningChatFromProfile, Some("点击「发消息」".into()))?;
                self.yolo_click_send_message_on_profile(button)?;
            }
        }

        self.advance(TaskState::VerifyingChatHeader, Some("确认已进入聊天".into()))?;
        let window = self.ensure_calibrated()?;
        let list_right = window.x + (window.width as f32 * 0.30).round() as i32;
        self.yolo_verify_chat_and_send(9, &name, header_anchor(window, list_right))
    }

    /// 读搜索框里的实际文字（失败文案用）。
    fn read_search_box(&mut self, search_box: Rect) -> Result<String, AutomationError> {
        let (_s, boxes) = self.capture_and_recognize(search_box, "搜索框实际内容")?;
        let mut sorted: Vec<&TextBox> = boxes.iter().collect();
        sorted.sort_by_key(|b| (b.bounds.y, b.bounds.x));
        Ok(sorted.iter().map(|b| b.text.trim()).collect::<Vec<_>>().join(" "))
    }

    /// 资料页右栏（窗口右 55%）。
    fn profile_region(window: Rect) -> Rect {
        Rect {
            x: window.x + (window.width as f32 * 0.45).round() as i32,
            y: window.y,
            width: (window.width as f32 * 0.55).round() as i32,
            height: window.height,
        }
    }

    /// B8：轮询资料页——同时读到目标名与「发消息」才算资料页；
    /// 若检出聊天输入区则视为已直接进聊天。
    fn yolo_poll_profile(&mut self, name: &str) -> Result<ProfileState, AutomationError> {
        let window = self.ensure_calibrated()?;
        let region = Self::profile_region(window);
        let min_conf = self.cfg().min_confidence.min(0.5);
        let mut saw_name = false;
        let outcome = self.poll_bounded(PollBudget::WallClock(PEER_POLL_BUDGET), HEADER_POLL_INTERVAL, |run| {
            let (_s, boxes) = run.capture_and_recognize(region, "资料页·姓名与发消息")?;
            let has_name = boxes.iter().any(|b| b.text.contains(name));
            saw_name |= has_name;
            let button = boxes
                .iter()
                .filter(|b| b.confidence >= min_conf)
                .find(|b| b.text.contains(SEND_MESSAGE_LABEL))
                .map(|b| b.bounds.to_screen(Point { x: region.x, y: region.y }));
            if let (true, Some(button)) = (has_name, button) {
                return Ok(Round::hit(ProfileState::Profile { button }));
            }
            let (_w, _s, dets) = run.yolo_detect_window("资料页·是否已进聊天")?;
            if best_by_class(&dets, class::INPUT_BAR).is_some() || best_by_class(&dets, class::SEND_BUTTON).is_some() {
                return Ok(Round::hit(ProfileState::AlreadyChat));
            }
            Ok(Round::missed())
        })?;
        match outcome {
            PollOutcome::Found { value, .. } => Ok(value),
            PollOutcome::Exhausted { stats } => Err(self.annotate_click_effect(AutomationError::NeedsHumanReview(format!(
                "步骤8失败：{}ms 内（OCR {} 次）资料页{}，且未检出聊天输入区。",
                stats.wall.as_millis(),
                stats.ocr_attempts,
                if saw_name { format!("读到「{name}」但没有「{SEND_MESSAGE_LABEL}」") } else { format!("未读到「{name}」（可能点错了人）") }
            )))),
        }
    }

    /// 输入关键词并停稳后：整窗 YOLO 一次，步骤名固定为「搜索下拉结果」。
    ///
    /// 决策里列出全部 `list_item`（旧权重的 `contact_item` / `conversation_item` 同样算；通过 = 检出）；若一个都没有则 `passed=false`，
    /// 过程回放默认会停在这一步——这正是「下拉里没点到人」时首先要看的画面。
    fn yolo_report_search_dropdown_results(&mut self) -> Result<(), AutomationError> {
        let (_window, _shot, dets) = self.yolo_detect_window("搜索下拉结果")?;
        let items = all_by_class(&dets, class::LIST_ITEM);
        let candidates: Vec<Verdict> = items
            .iter()
            .map(|d| {
                let tb = TextBox {
                    text: format!("{} {:.2}", d.class_name, d.conf),
                    bounds: d.bounds_rect(),
                    confidence: d.conf,
                };
                Verdict::passed(&tb, "检出联系人条目（list_item）")
            })
            .collect();
        let passed = !candidates.is_empty();
        let outcome = if passed {
            format!(
                "检出 {} 个联系人条目 list_item（同屏总检出 {}）",
                candidates.len(),
                dets.len()
            )
        } else {
            format!(
                "未检出联系人条目 list_item（同屏总检出 {}；请核对 YOLO class / 搜索是否已过滤出结果）",
                dets.len()
            )
        };
        self.evidence.push(format!("搜索下拉结果：{outcome}"));
        self.report_decision(
            "搜索下拉结果",
            Decision {
                step: String::new(),
                question: format!(
                    "搜索「{}」后，下拉/列表里有哪些联系人条目？",
                    self.task.external_contact_name.trim()
                ),
                rule: "YOLO class=`list_item`（兼容 contact_item / conversation_item；本步只记录检出，不做姓名 OCR）".into(),
                outcome,
                passed,
                min_confidence: self.cfg().yolo_conf,
                replay: None,
                candidates,
            },
        );
        Ok(())
    }

    /// B8：点「发消息」；点击前后比较右栏指纹，没变则最多再点 [`PROFILE_RECLICKS`] 次。
    ///
    /// 落点：[`random_point_in_central_half`]。最终是否进对了聊天交给标题核验。
    fn yolo_click_send_message_on_profile(&mut self, button: Rect) -> Result<(), AutomationError> {
        let window = self.ensure_calibrated()?;
        let region = Self::profile_region(window);
        for attempt in 0..=PROFILE_RECLICKS {
            let before = self.capture_frame(region, "资料页·点击前")?.fingerprint;
            let target = random_point_in_central_half(button);
            self.evidence.push(format!(
                "步骤8：点击「{SEND_MESSAGE_LABEL}」第 {} 次 → 屏幕 ({}, {})；文字框 {button:?}",
                attempt + 1,
                target.x,
                target.y
            ));
            self.ensure_not_frozen("已取消打开聊天")?;
            self.runner.ports.platform.guarded_click(target, window)?;
            self.check_deadline("点击发消息")?;
            let outcome = self.poll_bounded(PollBudget::WallClock(HEADER_POLL_BUDGET), HEADER_POLL_INTERVAL, |run| {
                let fp = run.capture_frame(region, "资料页·点击后")?.fingerprint;
                Ok(if fp != before { Round::hit(()) } else { Round::skipped() })
            })?;
            if matches!(outcome, PollOutcome::Found { .. }) {
                self.last_click_reacted = Some(true);
                return Ok(());
            }
            self.evidence.push(format!("步骤8：点击「{SEND_MESSAGE_LABEL}」后右栏画面未变化"));
        }
        // 不在这里判死：交给标题核验，并带上"点击无效果"。
        self.last_click_reacted = Some(false);
        Ok(())
    }
}
