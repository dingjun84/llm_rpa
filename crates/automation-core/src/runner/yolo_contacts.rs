//! 工作流 `contacts_search_send`：通讯录搜索找人并发送（Flow B）。
//!
//! 1. 点 `nav_contacts_icon`
//! 2. 点 `search_bar`，逐字输入联系人名
//! 3. 在 `contact_item` 里 OCR 找人并点击
//! 4. 右侧资料页 OCR「发消息」并点击（无 YOLO 类）
//! 5. 进入聊天后同 Flow A：input_bar → 输入 → send_button

use crate::ports::{AutomationError, Point, Rect};
use crate::state::TaskState;
use crate::yolo::{best_by_class, class};

use super::Run;

/// 资料页「发消息」按钮文案（无 YOLO class，只能 OCR）。
const SEND_MESSAGE_LABEL: &str = "发消息";

impl Run<'_> {
    /// Flow B：通讯录搜索 → 资料页发消息 → 聊天发送。
    pub(super) fn run_contacts_search_send(&mut self) -> Result<(), AutomationError> {
        self.advance(
            TaskState::NavigatingToView,
            Some("目标：通讯录导航图标".into()),
        )?;
        {
            let (window, shot, dets) = self.yolo_detect_window("导航·通讯录")?;
            let nav = self.yolo_require_class(
                &dets,
                class::NAV_CONTACTS_ICON,
                "请确认企业微信左侧导航可见。",
            )?;
            self.yolo_click_detection(window, &shot, nav, "通讯录导航")?;
        }

        self.advance(TaskState::SearchingContact, Some("通讯录搜索".into()))?;
        {
            let (window, shot, dets) = self.yolo_detect_window("搜索框")?;
            let bar = self.yolo_require_class(
                &dets,
                class::SEARCH_BAR,
                "请确认通讯录页顶部搜索框可见。",
            )?;
            self.yolo_click_detection(window, &shot, bar, "搜索框")?;
            let expected = self.ensure_calibrated()?;
            // 清空后再逐字输入，避免残留上次关键词。
            let _ = self.runner.ports.platform.clear_text_field(expected);
            let name = self.task.external_contact_name.clone();
            self.evidence
                .push(format!("逐字输入联系人「{name}」以过滤列表"));
            self.runner
                .ports
                .platform
                .type_text(&name, expected)?;
            self.check_deadline("输入搜索关键词")?;
            // 等联想/过滤出结果。
            std::thread::sleep(self.cfg().scroll_settle_timeout);
        }

        let matched = self.yolo_scroll_find_contact(class::CONTACT_ITEM)?;
        self.advance(
            TaskState::VerifyingCandidate,
            Some(format!("命中联系人条目 conf={:.2}", matched.conf)),
        )?;
        {
            let (window, shot, dets) = self.yolo_detect_window("点击联系人")?;
            let click_target = self
                .yolo_find_name_in_items(
                    window,
                    &shot,
                    &dets,
                    class::CONTACT_ITEM,
                    &self.task.external_contact_name,
                    "点击前确认联系人名",
                )?
                .unwrap_or(matched);
            self.yolo_click_detection(window, &shot, &click_target, "联系人条目")?;
        }

        self.advance(TaskState::VerifyingProfile, Some("资料页找「发消息」".into()))?;
        self.yolo_click_send_message_on_profile()?;

        self.advance(TaskState::VerifyingChatHeader, Some("确认已进入聊天".into()))?;
        let (window, shot, dets) = self.yolo_detect_window("聊天页确认")?;
        if best_by_class(&dets, class::INPUT_BAR).is_none()
            && best_by_class(&dets, class::MESSAGE_INPUT).is_none()
            && best_by_class(&dets, class::SEND_BUTTON).is_none()
        {
            return Err(AutomationError::NeedsHumanReview(
                "点击「发消息」后未检出聊天输入区，请确认资料页入口点对了。".into(),
            ));
        }

        self.advance(TaskState::PreparingMessage, None)?;
        self.yolo_type_and_send(window, &shot, &dets)
    }

    /// 在窗口右半侧 OCR 找「发消息」并点击。
    pub(super) fn yolo_click_send_message_on_profile(&mut self) -> Result<(), AutomationError> {
        let window = self.window.ok_or(AutomationError::ClientNotReady)?;
        // 右栏：大约窗口右 55%（企微三栏布局）。
        let region = Rect {
            x: window.x + (window.width as f32 * 0.45).round() as i32,
            y: window.y,
            width: (window.width as f32 * 0.55).round() as i32,
            height: window.height,
        };
        let (shot, boxes) = self.capture_and_recognize(region, "资料页·发消息")?;
        if self.cfg().log_ocr_candidates {
            self.evidence.push(format!(
                "资料页右栏读到 {} 块：{}",
                boxes.len(),
                crate::candidates::describe_candidates(&boxes)
            ));
        }
        let needle = SEND_MESSAGE_LABEL;
        let hit = boxes
            .iter()
            .filter(|b| b.confidence >= self.cfg().min_confidence.min(0.5))
            .find(|b| b.text.contains(needle))
            .ok_or_else(|| {
                AutomationError::NeedsHumanReview(format!(
                    "资料页右侧未找到「{needle}」按钮（亦可能显示为其它文案）。\
                     请确认已打开联系人资料且该入口可见。"
                ))
            })?;
        let screen = hit.bounds.to_screen(Point {
            x: region.x,
            y: region.y,
        });
        let target = screen.center();
        self.evidence.push(format!(
            "点击「{needle}」：屏幕 ({}, {}) 文字「{}」",
            target.x,
            target.y,
            hit.text.trim()
        ));
        let expected = self.ensure_calibrated()?;
        self.ensure_not_frozen("已取消打开聊天")?;
        self.runner
            .ports
            .platform
            .guarded_click(target, expected)?;
        self.check_deadline("点击发消息")?;
        // 等聊天页打开。
        std::thread::sleep(self.cfg().scroll_settle_timeout);
        let _ = shot;
        Ok(())
    }
}
