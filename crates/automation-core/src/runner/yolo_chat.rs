//! 工作流 `chat_list_send`：会话列表找人并发送（Flow A）。
//!
//! 1. 点 `nav_chat_icon`
//! 2. 在 `conversation_item` 里 OCR 首行找联系人（可滚动重试）
//! 3. 点中进入聊天，确认 `input_bar`/`send_button`
//! 4. 点输入框 → 逐字输入 → 点发送

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
        let matched = self.yolo_scroll_find_contact(class::CONVERSATION_ITEM)?;

        self.advance(
            TaskState::VerifyingCandidate,
            Some(format!("命中会话条目 conf={:.2}", matched.conf)),
        )?;

        {
            let (window, shot, _) = self.yolo_detect_window("点击会话前复检")?;
            // 用刚找到的条目坐标：复检帧里按同 class 最高分近似同一行。
            // 更稳的做法是保存 matched 的 xyxy；这里直接点 matched（相对上一帧）。
            // 若布局已变，下面的聊天页确认会拦住。
            let _ = (window, shot);
        }
        {
            let (window, shot, dets) = self.yolo_detect_window("点击会话")?;
            // 优先用上一轮 matched 的 class_name 在新帧里再找同名联系人；
            // 找不到则点 conf 最高的 conversation_item 中再次 OCR 匹配。
            let click_target = self
                .yolo_find_name_in_items(
                    window,
                    &shot,
                    &dets,
                    class::CONVERSATION_ITEM,
                    &self.task.external_contact_name,
                    "点击前确认会话名",
                )?
                .unwrap_or(matched);
            self.yolo_click_detection(window, &shot, &click_target, "会话条目")?;
        }

        self.advance(TaskState::VerifyingChatHeader, Some("确认已进入聊天".into()))?;
        let (window, shot, dets) = self.yolo_detect_window("聊天页确认")?;
        if best_input_or_send(&dets).is_none() {
            return Err(AutomationError::NeedsHumanReview(
                "点击会话后未检出 input_bar / send_button，可能未打开聊天或界面被遮挡。"
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
    ) -> Result<crate::yolo::YoloDetection, AutomationError> {
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
                return Ok(hit);
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
