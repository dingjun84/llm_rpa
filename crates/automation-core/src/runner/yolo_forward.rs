//! 工作流 `forward_to_contact`：从「文件传输助手」转发一条气泡到指定联系人。
//!
//! 参数映射：`file_helper` 默认「文件传输助手」；`message_text` = `task.text`；
//! `contact_query` = `task.external_contact_name`。
//!
//! 鼠标点击/右键/滚轮一律走平台端口（Windows 上由 GhostBox 封装）。
//! 弹出层按主窗同进程可见顶层窗枚举；截图不抢焦点。

use std::time::Duration;

use crate::ports::{AutomationError, PeerTopWindow, Rect, Screenshot};
use crate::state::TaskState;
use crate::yolo::{
    best_by_class, class, random_point_in_central_half, shot_point_to_screen,
};

use super::yolo_forward_poll::Prefer;
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

        // ── 1. 已在会话列表则跳过；否则点消息导航并轮询选中态 ──
        self.step_begin(1, "点导航消息 icon → 会话列表")?;
        self.forward_ensure_message_nav_selected()?;

        self.advance(TaskState::SearchingContact, Some("转发：搜索文件传输助手".into()))?;

        // ── 2. 定位搜索框并点击至焦点（指纹变化）；YOLO search_bar 优先，未检出则 OCR「搜索」──
        self.step_begin(2, "定位并点击搜索框至焦点（API 优先，OCR 回退）")?;
        let search_box = self.forward_locate_search_box(2, LABEL_SEARCH)?;
        self.forward_click_search_box_until_focused(2, search_box)?;

        // ── 3. 先拍列表指纹基线，再输入；在搜索框下方找该文字并点 ──
        self.step_begin(3, "输入文件传输助手并轮询下方结果")?;
        let window = self.ensure_calibrated()?;
        // 基线必须在输入前：输入后下拉可能已展开，比较区在搜索框下方不应受框内文字影响。
        let list_baseline = self.forward_capture_list_baseline(3, window, search_box)?;
        {
            let expected = window;
            let _ = self.runner.ports.platform.clear_text_field(expected);
            self.evidence
                .push(format!("步骤3：逐字输入「{file_helper}」"));
            self.runner
                .ports
                .platform
                .type_text(&file_helper, expected)?;
            self.check_deadline("输入文件传输助手")?;
        }
        self.forward_verify_typed_in_search_box(3, file_helper.as_str(), search_box)?;
        let helper_hit = self.forward_ocr_below_polled(
            3,
            file_helper.as_str(),
            search_box,
            &list_baseline,
        )?;
        self.forward_click_screen_box(3, "文件传输助手结果行", None, helper_hit)?;
        // 点击后校验：list_item 首行「文件传输助手」且蓝占比 >0.4；否则左击重试最多 3 次。
        self.forward_verify_file_helper_row_selected(3, file_helper.as_str())?;

        self.advance(
            TaskState::VerifyingCandidate,
            Some("转发：已点文件传输助手".into()),
        )?;

        // ── 4. 命中点右上方附近有「文件传输助手」→ 进会话成功 ──
        self.step_begin(4, "确认已进入文件传输助手会话")?;
        self.forward_verify_chat_header(4, &file_helper, helper_hit)?;

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

        self.advance(TaskState::PreparingMessage, Some("转发：准备弹出层操作".into()))?;

        // ── 7. 轮询菜单窗出现且 OCR「转发」再点 ──
        self.step_begin(7, "轮询菜单点「转发」")?;
        let _ = self.forward_click_in_peer(
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

        // ── 8. 轮询转发窗「创建聊天」 ──
        self.step_begin(8, "轮询转发窗点「创建聊天」")?;
        let create_chat_peer = self.forward_click_in_peer(
            8,
            LABEL_CREATE_CHAT,
            |w| {
                w.title.contains("选择联系人")
                    || w.class_name.contains("SelectForward")
                    || (!w.is_main && w.rect.width >= 300 && w.rect.height >= 300)
            },
            Prefer::FirstTopLeft,
        )?;
        // 点击后校验：100ms 后 OCR 弹层，仍见「创建聊天」= 未跳转 → 重新定位再点，最多 3 次。
        self.forward_verify_create_chat_left(8, LABEL_CREATE_CHAT, &create_chat_peer)?;

        // ── 9. 轮询最上层窗「搜索」再输入 contact_query ──
        self.step_begin(9, "轮询转发窗搜索联系人")?;
        let peer_pred = |w: &PeerTopWindow| {
            w.title.contains("选择联系人")
                || w.class_name.contains("SelectForward")
                || (!w.is_main && w.rect.width >= 300 && w.rect.height >= 300)
        };
        let (peer, search_in_peer) =
            self.forward_ocr_first_in_peer_polled(9, LABEL_SEARCH, peer_pred)?;
        self.forward_click_screen_box(9, "转发窗搜索框", Some(&peer), search_in_peer)?;
        // 步骤10 的列表指纹基线必须在输入前拍下（比较区 = 搜索框下方）。
        let peer_list_baseline =
            self.forward_capture_list_baseline(10, peer.rect, search_in_peer)?;
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
        }

        // ── 10. 轮询搜索框下第一个最大包含匹配行 ──
        self.step_begin(10, "轮询点搜索结果联系人行")?;
        let row = self.forward_ocr_best_below_polled(
            10,
            &contact_query,
            search_in_peer,
            peer.rect,
            &peer_list_baseline,
        )?;
        self.forward_click_screen_box(10, "联系人匹配行", Some(&peer), row)?;

        // ── 11. 轮询最靠右下「创建并发送」 ──
        self.step_begin(11, "轮询点「创建并发送」")?;
        let _ = self.forward_click_in_peer(
            11,
            LABEL_CREATE_SEND,
            |w| {
                w.title.contains("选择联系人")
                    || w.class_name.contains("SelectForward")
                    || (!w.is_main && w.rect.width >= 300 && w.rect.height >= 300)
            },
            Prefer::BottomRightMost,
        )?;

        self.finish_forward_step();
        self.advance(TaskState::Sending, Some("已点击「创建并发送」".into()))?;
        self.advance(TaskState::VerifyingDelivery, None)?;
        self.advance(
            TaskState::Completed,
            Some("转发到联系人流程完成".into()),
        )?;
        Ok(())
    }

    /// 步骤2：定位主窗搜索框。
    ///
    /// 1. 先 YOLO 检出 `search_bar` → 用其屏幕矩形（位移/落点都走这框）；
    /// 2. 未检出 → 回退现有 OCR「搜索」轮询逻辑。
    pub(super) fn forward_locate_search_box(
        &mut self,
        step: u32,
        ocr_needle: &str,
    ) -> Result<Rect, AutomationError> {
        let (window, shot, dets) =
            self.yolo_detect_window(&format!("转发·{step}·YOLO搜索框"))?;
        if let Some(bar) = best_by_class(&dets, class::SEARCH_BAR) {
            let img = bar.bounds_rect();
            let tl = shot_point_to_screen(img.x, img.y, window, shot.width, shot.height);
            let br = shot_point_to_screen(
                img.x + img.width,
                img.y + img.height,
                window,
                shot.width,
                shot.height,
            );
            let screen = Rect {
                x: tl.x,
                y: tl.y,
                width: (br.x - tl.x).max(1),
                height: (br.y - tl.y).max(1),
            };
            self.evidence.push(format!(
                "步骤{step}：YOLO 检出 search_bar conf={:.2} → 屏幕框 ({}, {}) {}x{}",
                bar.conf, screen.x, screen.y, screen.width, screen.height
            ));
            return Ok(screen);
        }
        self.evidence.push(format!(
            "步骤{step}：YOLO 未检出 search_bar，回退 OCR「{ocr_needle}」"
        ));
        self.forward_ocr_first_label_polled(step, ocr_needle, "搜索框")
    }

    /// 开始新步骤：先落上一步耗时，再写步骤分隔行并启动计时。
    fn step_begin(&mut self, n: u32, title: &str) -> Result<(), AutomationError> {
        self.finish_forward_step();
        self.evidence.push(format!("—— 步骤{n}：{title} ——"));
        self.forward_step = Some((n, title.to_string(), std::time::Instant::now()));
        self.check_deadline(&format!("转发步骤{n}"))?;
        Ok(())
    }


    /// 步骤2：点击搜索框并确认焦点——框内区域指纹在点击后必须变化。
    ///
    /// 每次尝试：拍框内指纹 → 在中心半区落点（重试时先 `move_pointer` 挪进框内）→
    /// 点击 → 短暂等待后再拍同一区域。指纹变了 = 焦点已获取；否则最多共
    /// [`SEARCH_FOCUS_MAX_ATTEMPTS`] 次。
    pub(super) fn forward_click_search_box_until_focused(
        &mut self,
        step: u32,
        search_box: Rect,
    ) -> Result<(), AutomationError> {
        for attempt in 1..=SEARCH_FOCUS_MAX_ATTEMPTS {
            self.check_cancel()?;
            let before = self
                .capture_frame(
                    search_box,
                    &format!("转发·{step}·搜索框焦点前·第{attempt}次"),
                )?
                .fingerprint;
            let target = random_point_in_central_half(search_box);
            self.evidence.push(format!(
                "步骤{step}：搜索框焦点尝试 {attempt}/{SEARCH_FOCUS_MAX_ATTEMPTS} → 点 ({}, {})；框 ({}, {}) {}x{}；点击前指纹 `{before}`",
                target.x,
                target.y,
                search_box.x,
                search_box.y,
                search_box.width,
                search_box.height
            ));
            let expected = self.window.unwrap_or(search_box);
            // 重试：先把指针挪进框内再点，避免落在旧坐标/框外。
            if attempt > 1 {
                self.runner.ports.platform.move_pointer(target)?;
            }
            self.ensure_not_frozen("已取消点击搜索框")?;
            self.runner
                .ports
                .platform
                .guarded_click(target, expected)?;
            self.check_deadline("点击搜索框")?;
            // 等 UI 画出焦点/光标后再比；过短会把慢机器上的成功当成失败。
            std::thread::sleep(SEARCH_FOCUS_REACT_WAIT);
            let after = self
                .capture_frame(
                    search_box,
                    &format!("转发·{step}·搜索框焦点后·第{attempt}次"),
                )?
                .fingerprint;
            if search_box_focus_acquired(&before, &after) {
                self.evidence.push(format!(
                    "步骤{step}：焦点检查 attempt={attempt} → changed（后指纹 `{after}`）"
                ));
                return Ok(());
            }
            self.evidence.push(format!(
                "步骤{step}：焦点检查 attempt={attempt} → same（后指纹 `{after}`）；{}",
                if attempt < SEARCH_FOCUS_MAX_ATTEMPTS {
                    "将在框内挪点重试"
                } else {
                    "已达上限"
                }
            ));
        }
        Err(AutomationError::NeedsHumanReview(format!(
            "步骤{step}失败：点击搜索框 {SEARCH_FOCUS_MAX_ATTEMPTS} 次后区域指纹仍未变化，可能未获得输入焦点。请确认搜索框可见且可点击。"
        )))
    }

    pub(super) fn log_click_ctx(
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

/// 步骤2 点击搜索框：最多尝试次数（含首次；指纹不变则框内挪点再点）。
const SEARCH_FOCUS_MAX_ATTEMPTS: u32 = 3;
/// 每次点击后、再拍指纹前的等待（给焦点/光标一点绘制时间）。
const SEARCH_FOCUS_REACT_WAIT: Duration = Duration::from_millis(200);

/// 搜索框区域指纹是否在点击后变化（变化 = 视为已获焦点）。
fn search_box_focus_acquired(before: &str, after: &str) -> bool {
    before != after
}

/// 纯重试驱动：最多 `max_attempts` 次；`reacted(attempt)` 为真则返回该次（1-based）。
///
/// 与 [`Run::forward_click_search_box_until_focused`] 同语义，便于单测不接平台。
fn search_focus_attempts_until<F>(max_attempts: u32, mut reacted: F) -> Option<u32>
where
    F: FnMut(u32) -> bool,
{
    for attempt in 1..=max_attempts {
        if reacted(attempt) {
            return Some(attempt);
        }
    }
    None
}

#[cfg(test)]
mod search_focus_tests {
    use super::*;

    #[test]
    fn focus_acquired_iff_fingerprint_differs() {
        assert!(search_box_focus_acquired("aaa", "bbb"));
        assert!(!search_box_focus_acquired("aaa", "aaa"));
        assert!(!search_box_focus_acquired("", ""));
    }

    #[test]
    fn retry_loop_succeeds_on_second_attempt() {
        let mut calls = 0u32;
        let got = search_focus_attempts_until(3, |_| {
            calls += 1;
            calls >= 2
        });
        assert_eq!(got, Some(2));
        assert_eq!(calls, 2);
    }

    #[test]
    fn retry_loop_fails_after_three_unchanged() {
        let mut calls = 0u32;
        let got = search_focus_attempts_until(SEARCH_FOCUS_MAX_ATTEMPTS, |_| {
            calls += 1;
            false
        });
        assert_eq!(got, None);
        assert_eq!(calls, SEARCH_FOCUS_MAX_ATTEMPTS);
    }

    #[test]
    fn retry_loop_succeeds_on_first_attempt() {
        let got = search_focus_attempts_until(3, |_| true);
        assert_eq!(got, Some(1));
    }
}
