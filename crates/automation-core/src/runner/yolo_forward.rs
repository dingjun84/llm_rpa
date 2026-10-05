//! 工作流 `forward_to_contact`：从「文件传输助手」转发一条气泡到指定联系人。
//!
//! 参数映射：`file_helper` 默认「文件传输助手」；`message_text` = `task.text`；
//! `contact_query` = `task.external_contact_name`。
//!
//! 鼠标点击/右键/滚轮一律走平台端口（Windows 上由 GhostBox 封装）。
//! 弹出层按主窗同进程可见顶层窗枚举；截图不抢焦点。

use crate::ports::{AutomationError, PeerTopWindow, Rect, Screenshot};
use crate::state::TaskState;
use crate::yolo::{class, random_point_in_central_half, shot_point_to_screen};

use super::yolo_forward_ops::Prefer;
use super::Run;

const DEFAULT_FILE_HELPER: &str = "文件传输助手";
const LABEL_SEARCH: &str = "搜索";
const LABEL_FORWARD: &str = "转发";
const LABEL_CREATE_CHAT: &str = "创建聊天";
const LABEL_CREATE_SEND: &str = "创建并发送";

impl Run<'_> {
    /// Flow C：消息页 → 文件传输助手 → 右键转发 → 选联系人 → 创建并发送。
    pub(super) fn run_forward_to_contact(&mut self) -> Result<(), AutomationError> {
        let file_helper = DEFAULT_FILE_HELPER.to_string();
        let message_text = self.task.text.clone();
        let contact_query = self.task.external_contact_name.clone();
        if message_text.trim().is_empty() {
            return Err(AutomationError::NeedsHumanReview(
                "步骤参数：message_text（消息正文）为空，无法定位要转发的气泡。".into(),
            ));
        }
        if contact_query.trim().is_empty() {
            return Err(AutomationError::NeedsHumanReview(
                "步骤参数：contact_query（外部联系人名称）为空，无法选择转发目标。".into(),
            ));
        }

        self.advance(
            TaskState::NavigatingToView,
            Some("转发：目标消息导航图标".into()),
        )?;

        // ── 1. 点导航消息 icon ──
        self.step_begin(1, "点导航消息 icon → 会话列表")?;
        {
            let (window, shot, dets) = self.yolo_detect_window("转发·1·导航消息")?;
            let nav = self.yolo_require_class(
                &dets,
                class::NAV_CHAT_ICON,
                "请确认企业微信左侧导航可见。",
            )?;
            self.log_click_ctx(1, "nav_chat_icon", None, window, &shot, nav.bounds_rect())?;
            self.yolo_click_detection(window, &shot, nav, "消息导航")?;
        }

        self.advance(TaskState::SearchingContact, Some("转发：搜索文件传输助手".into()))?;

        // ── 2. OCR 第一个「搜索」并点击 ──
        self.step_begin(2, "OCR 第一个「搜索」= 搜索框")?;
        let search_box = self.forward_ocr_first_label(LABEL_SEARCH, None)?;
        self.forward_click_screen_box(2, "搜索框", None, search_box)?;

        // ── 3. 输入文件传输助手；在搜索框下方找该文字并点 ──
        self.step_begin(3, "输入文件传输助手并点下方结果")?;
        {
            let expected = self.ensure_calibrated()?;
            let _ = self.runner.ports.platform.clear_text_field(expected);
            self.evidence
                .push(format!("步骤3：逐字输入「{file_helper}」"));
            self.runner
                .ports
                .platform
                .type_text(&file_helper, expected)?;
            self.check_deadline("输入文件传输助手")?;
            std::thread::sleep(self.cfg().scroll_settle_timeout);
        }
        let helper_hit = self.forward_ocr_below(file_helper.as_str(), search_box)?;
        self.forward_click_screen_box(3, "文件传输助手结果行", None, helper_hit)?;

        self.advance(
            TaskState::VerifyingCandidate,
            Some("转发：已点文件传输助手".into()),
        )?;

        // ── 4. 命中点右上方附近有「文件传输助手」→ 进会话成功 ──
        self.step_begin(4, "确认已进入文件传输助手会话")?;
        self.forward_verify_chat_header(&file_helper, helper_hit)?;

        self.advance(
            TaskState::VerifyingChatHeader,
            Some("转发：已进入文件传输助手会话".into()),
        )?;

        // ── 5. 找气泡；移到消息区中部；缓慢滚轮到底 ──
        self.step_begin(5, "消息区滚到底")?;
        self.forward_scroll_chat_to_bottom()?;

        // ── 6. 含 message_text 的 bubble 上右击 ──
        self.step_begin(6, "右击目标气泡")?;
        self.forward_right_click_bubble(&message_text)?;
        std::thread::sleep(self.cfg().scroll_settle_timeout);

        self.advance(TaskState::PreparingMessage, Some("转发：准备弹出层操作".into()))?;

        // ── 7. 上层菜单窗点「转发」 ──
        self.step_begin(7, "菜单点「转发」")?;
        self.forward_click_in_peer(
            7,
            LABEL_FORWARD,
            |w| {
                let title = w.title.to_lowercase();
                title.contains("menu")
                    || w.class_name.contains("DuiMenu")
                    || w.class_name.contains("Menu")
                    || (!w.is_main && w.rect.height < 400 && w.rect.width < 400)
            },
            Prefer::FirstTopLeft,
        )?;
        std::thread::sleep(self.cfg().scroll_settle_timeout);

        // ── 8. 转发窗第一个「创建聊天」 ──
        self.step_begin(8, "转发窗点「创建聊天」")?;
        self.forward_click_in_peer(
            8,
            LABEL_CREATE_CHAT,
            |w| {
                w.title.contains("选择联系人")
                    || w.class_name.contains("SelectForward")
                    || (!w.is_main && w.rect.width >= 300 && w.rect.height >= 300)
            },
            Prefer::FirstTopLeft,
        )?;
        std::thread::sleep(self.cfg().scroll_settle_timeout);

        // ── 9. 最上层窗搜索框输入 contact_query ──
        self.step_begin(9, "转发窗搜索联系人")?;
        let peer = self.forward_pick_peer(|w| {
            w.title.contains("选择联系人")
                || w.class_name.contains("SelectForward")
                || (!w.is_main && w.rect.width >= 300 && w.rect.height >= 300)
        })?;
        let search_in_peer = self.forward_ocr_first_in_rect(LABEL_SEARCH, peer.rect)?;
        self.forward_click_screen_box(9, "转发窗搜索框", Some(&peer), search_in_peer)?;
        {
            // 弹出层：用主窗矩形做守卫（同进程允许）；不清空失败也不致命。
            let expected = self.window.unwrap_or(peer.rect);
            let _ = self.runner.ports.platform.clear_text_field(expected);
            self.evidence
                .push(format!("步骤9：逐字输入联系人「{contact_query}」"));
            self.runner
                .ports
                .platform
                .type_text(&contact_query, expected)?;
            self.check_deadline("输入转发联系人")?;
            std::thread::sleep(self.cfg().scroll_settle_timeout);
        }

        // ── 10. 搜索框下第一个最大包含匹配行 ──
        self.step_begin(10, "点搜索结果联系人行")?;
        let row = self.forward_ocr_best_below(&contact_query, search_in_peer, peer.rect)?;
        self.forward_click_screen_box(10, "联系人匹配行", Some(&peer), row)?;
        std::thread::sleep(self.cfg().scroll_settle_timeout);

        // ── 11. 最靠右下「创建并发送」 ──
        self.step_begin(11, "点「创建并发送」")?;
        self.forward_click_in_peer(
            11,
            LABEL_CREATE_SEND,
            |w| {
                w.title.contains("选择联系人")
                    || w.class_name.contains("SelectForward")
                    || (!w.is_main && w.rect.width >= 300 && w.rect.height >= 300)
            },
            Prefer::BottomRightMost,
        )?;

        self.advance(TaskState::Sending, Some("已点击「创建并发送」".into()))?;
        self.advance(TaskState::VerifyingDelivery, None)?;
        self.advance(
            TaskState::Completed,
            Some("转发到联系人流程完成".into()),
        )?;
        Ok(())
    }

    fn step_begin(&mut self, n: u32, title: &str) -> Result<(), AutomationError> {
        self.evidence.push(format!("—— 步骤{n}：{title} ——"));
        self.check_deadline(&format!("转发步骤{n}"))?;
        Ok(())
    }

    fn log_click_ctx(
        &mut self,
        step: u32,
        what: &str,
        peer: Option<&PeerTopWindow>,
        window: Rect,
        shot: &Screenshot,
        img_bounds: Rect,
    ) -> Result<(), AutomationError> {
        let img_pt = random_point_in_central_half(img_bounds);
        let screen = shot_point_to_screen(img_pt.x, img_pt.y, window, shot.width, shot.height);
        let (cls, title, id) = match peer {
            Some(p) => (p.class_name.as_str(), p.title.as_str(), p.id.as_str()),
            None => ("(main)", "(main)", "-"),
        };
        self.evidence.push(format!(
            "步骤{step}：找「{what}」→ 点屏幕 ({}, {})；窗 class=`{cls}` title=`{title}` id=`{id}`；\
             窗矩形 ({}, {}) {}x{}",
            screen.x, screen.y, window.x, window.y, window.width, window.height
        ));
        Ok(())
    }

}
