//! 工作流 `chat_list_send`：会话列表找人并发送（Flow A）。
//!
//! A2 点 `nav_chat_icon` 并轮询选中态（[`Run::forward_ensure_message_nav_selected`]）
//! A3 两轮滚动查找（[`Run::yolo_scroll_find_contact`]）：指纹判到底 / 卡死，同名即停
//! A4 点击前复检 + 点击后选中校验（[`Run::yolo_click_list_row_verified`]）
//! A5–A9 标题核验 → 输入并 OCR 确认 → 发送 → 输入框清空 → 送达（[`Run::yolo_verify_chat_and_send`]）

use crate::ports::AutomationError;
use crate::state::TaskState;
use crate::yolo::ListPage;

use super::yolo_verify_rules::header_anchor;
use super::Run;

impl Run<'_> {
    /// Flow A：已在/进入聊天页 → 会话列表找人 → 发消息。
    pub(super) fn run_chat_list_send(&mut self) -> Result<(), AutomationError> {
        let name = self.task.external_contact_name.trim().to_string();
        if name.is_empty() {
            return Err(AutomationError::NeedsHumanReview("外部联系人名称为空。".into()));
        }
        self.advance(TaskState::NavigatingToView, Some("目标：消息导航图标".into()))?;
        self.forward_ensure_message_nav_selected()?;

        self.advance(TaskState::SearchingContact, Some("在会话列表中查找".into()))?;
        let (window, shot, matched) = self.yolo_scroll_find_contact(ListPage::ChatList)?;

        self.advance(
            TaskState::VerifyingCandidate,
            Some(format!("命中会话条目 conf={:.2}", matched.conf)),
        )?;
        let row = self.yolo_click_list_row_verified(4, ListPage::ChatList, (window, &shot, &matched), "会话条目")?;

        self.advance(TaskState::VerifyingChatHeader, Some("确认已进入聊天".into()))?;
        let window = self.ensure_calibrated()?;
        self.yolo_verify_chat_and_send(5, &name, header_anchor(window, row.x + row.width))
    }
}
