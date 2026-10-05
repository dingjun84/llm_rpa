//! 工作流 `chat_list_send`：会话列表找人并发送（Flow A）。
//!
//! 1. 点 `nav_chat_icon`
//! 2. 在 `conversation_item` 里 OCR 首行找联系人（可滚动重试）
//! 3. 点中进入聊天，确认 `input_bar`/`send_button`（或遗留 `message_input`）
//! 4. 估计输入区并点击 → 逐字输入 → 点发送

use crate::ports::AutomationError;
use crate::state::TaskState;
use crate::yolo::class;

use super::Run;

impl Run<'_> {
    /// Flow A：已在/进入聊天页 → 会话列表找人 → 发消息。
    pub(super) fn run_chat_list_send(&mut self) -> Result<(), AutomationError> {
        self.advance(
            TaskState::NavigatingToView,
            Some("目标：消息导航图标".into()),
        )?;
        {
            let (window, shot, dets) = self.yolo_detect_window("导航·消息")?;
            let nav = self.yolo_require_class(
                &dets,
                class::NAV_CHAT_ICON,
                "请确认企业微信左侧导航可见。",
            )?;
            self.yolo_click_detection(window, &shot, nav, "消息导航")?;
        }

        self.advance(TaskState::SearchingContact, Some("在会话列表中查找".into()))?;
        // 列表查找里的首行 OCR 已经对上名字。命中后直接点这一帧的 conversation_item，
        // 不再对每个会话框做「点击前确认会话名」。坐标用产生命中的那一帧。
        let (window, shot, matched) = self.yolo_scroll_find_contact(class::CONVERSATION_ITEM)?;

        self.advance(
            TaskState::VerifyingCandidate,
            Some(format!("命中会话条目 conf={:.2}", matched.conf)),
        )?;
        self.yolo_click_detection(window, &shot, &matched, "会话条目")?;

        self.advance(TaskState::VerifyingChatHeader, Some("确认已进入聊天".into()))?;
        let (window, shot, dets) = self.yolo_detect_window("聊天页确认")?;
        if best_input_or_send(&dets).is_none() {
            return Err(AutomationError::NeedsHumanReview(
                "点击会话后未检出 input_bar / send_button（或遗留 message_input），可能未打开聊天或界面被遮挡。"
                    .into(),
            ));
        }

        self.advance(TaskState::PreparingMessage, None)?;
        self.yolo_type_and_send(window, &shot, &dets)
    }

    /// 滚动会话/通讯录列表直到找到联系人或超过上限。
    pub(super) fn yolo_scroll_find_contact(
        &mut self,
        item_class: &str,
    ) -> Result<(crate::ports::Rect, crate::ports::Screenshot, crate::yolo::YoloDetection), AutomationError> {
        let name = self.task.external_contact_name.clone();
        let max = self.cfg().max_scroll_attempts;
        for attempt in 0..=max {
            let (window, shot, dets) =
                self.yolo_detect_window(&format!("列表查找·第{}次", attempt + 1))?;
            if let Some(hit) = self.yolo_find_name_in_items(
                window,
                &shot,
                &dets,
                item_class,
                &name,
                "列表 OCR",
            )? {
                // 把产生命中的那一帧一并交还，调用方用同一帧的截图尺寸做点击换算。
                return Ok((window, shot, hit));
            }
            if attempt == max {
                break;
            }
            self.yolo_scroll_list(window, &shot, &dets, item_class)?;
        }
        Err(AutomationError::NeedsHumanReview(format!(
            "在「{item_class}」列表中滚了 {max} 次仍未找到「{name}」。\
             请确认联系人在列表中、名称与 OCR 一致，或增大 max_scroll_attempts。"
        )))
    }
}

fn best_input_or_send(dets: &[crate::yolo::YoloDetection]) -> Option<()> {
    use crate::yolo::best_by_class;
    if best_by_class(dets, class::INPUT_BAR).is_some()
        || best_by_class(dets, class::MESSAGE_INPUT).is_some()
        || best_by_class(dets, class::SEND_BUTTON).is_some()
    {
        Some(())
    } else {
        None
    }
}
