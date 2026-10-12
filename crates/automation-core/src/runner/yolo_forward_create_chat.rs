//! 转发步骤8：点击「创建聊天」之后的跳转校验。
//!
//! 点击后等 100ms → OCR 当前转发窗；仍含「创建聊天」= 页面未跳转 → 用这次 OCR
//! 重新定位按钮再点，然后再检。最多 3 次额外点击；仍在 → NeedsHumanReview。

use std::time::Duration;

use crate::ports::{AutomationError, PeerTopWindow, Point, Rect};

use super::yolo_forward_poll::Prefer;
use super::Run;

/// 「创建聊天」仍在时最多额外点击次数（不含首次点击）。
const CREATE_CHAT_MAX_RETRY_CLICKS: u32 = 3;
/// 每次点击后、OCR 再检前的等待。
const CREATE_CHAT_VERIFY_WAIT: Duration = Duration::from_millis(100);

/// 单次检查结果（纯数据，便于单测驱动重试语义）。
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq)]
enum CreateChatStatus {
    /// 窗内已无「创建聊天」→ 已跳转。
    Gone,
    /// 仍可见，附重新定位到的屏幕框。
    StillPresent(Rect),
}

/// 纯重试驱动：先检 → 仍在则点（用本次定位框）→ 再检；最多 `max_clicks` 次额外点击。
///
/// `Ok(n)` = 第 n 次检查（1-based）通过；`Err(last_box)` = 点满仍在。
#[cfg_attr(not(test), allow(dead_code))]
fn create_chat_retry_until<C, K>(
    max_clicks: u32,
    mut check: C,
    mut click: K,
) -> Result<u32, Rect>
where
    C: FnMut(u32) -> CreateChatStatus,
    K: FnMut(u32, Rect),
{
    let mut check_no = 1u32;
    let mut clicks = 0u32;
    loop {
        match check(check_no) {
            CreateChatStatus::Gone => return Ok(check_no),
            CreateChatStatus::StillPresent(b) => {
                if clicks >= max_clicks {
                    return Err(b);
                }
                clicks += 1;
                click(clicks, b);
                check_no += 1;
            }
        }
    }
}

impl Run<'_> {
    /// 步骤8 点「创建聊天」之后：确认弹层已跳转（OCR 不再出现 `needle`）。
    ///
    /// 与 [`create_chat_retry_until`] 同语义；平台调用出错直接上抛。
    pub(super) fn forward_verify_create_chat_left(
        &mut self,
        step: u32,
        needle: &str,
        peer: &PeerTopWindow,
    ) -> Result<(), AutomationError> {
        let mut check_no = 1u32;
        let mut clicks = 0u32;
        loop {
            self.check_cancel()?;
            std::thread::sleep(CREATE_CHAT_VERIFY_WAIT);
            let (_shot, boxes) = self.capture_and_recognize(
                peer.rect,
                &format!("转发·{step}·创建聊天跳转校验·第{check_no}次"),
            )?;
            let mut hits: Vec<Rect> = boxes
                .iter()
                .filter(|b| b.text.contains(needle))
                .map(|b| b.bounds.to_screen(Point { x: peer.rect.x, y: peer.rect.y }))
                .collect();
            if hits.is_empty() {
                self.evidence.push(format!(
                    "步骤{step}：跳转校验 第{check_no}次 → 「{needle}」已消失（额外点击 {clicks} 次），页面已跳转"
                ));
                return Ok(());
            }
            Self::forward_prefer_hit(&mut hits, Prefer::FirstTopLeft);
            let b = hits[0];
            if clicks >= CREATE_CHAT_MAX_RETRY_CLICKS {
                self.evidence.push(format!(
                    "步骤{step}：跳转校验 第{check_no}次 → 仍见「{needle}」；已额外点击 {clicks}/{CREATE_CHAT_MAX_RETRY_CLICKS} 次，放弃"
                ));
                return Err(AutomationError::NeedsHumanReview(format!(
                    "步骤{step}失败：点击「{needle}」后又重试点击 {CREATE_CHAT_MAX_RETRY_CLICKS} 次，转发窗仍显示「{needle}」，页面未跳转。最后位置 ({}, {}) {}x{}；窗 class=`{}` title=`{}`。",
                    b.x, b.y, b.width, b.height, peer.class_name, peer.title
                )));
            }
            clicks += 1;
            self.evidence.push(format!(
                "步骤{step}：跳转校验 第{check_no}次 → 仍见「{needle}」于 ({}, {}) {}x{}；重试点击 {clicks}/{CREATE_CHAT_MAX_RETRY_CLICKS}",
                b.x, b.y, b.width, b.height
            ));
            self.forward_click_screen_box(step, needle, Some(peer), b)?;
            check_no += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r() -> Rect {
        Rect { x: 1, y: 2, width: 3, height: 4 }
    }

    #[test]
    fn gone_on_first_check_no_clicks() {
        let mut clicks = 0;
        let got = create_chat_retry_until(3, |_| CreateChatStatus::Gone, |_, _| clicks += 1);
        assert_eq!(got, Ok(1));
        assert_eq!(clicks, 0);
    }

    #[test]
    fn gone_after_two_retry_clicks() {
        let mut clicks = 0;
        let got = create_chat_retry_until(
            3,
            |n| if n >= 3 { CreateChatStatus::Gone } else { CreateChatStatus::StillPresent(r()) },
            |_, b| {
                assert_eq!(b, r());
                clicks += 1
            },
        );
        assert_eq!(got, Ok(3));
        assert_eq!(clicks, 2);
    }

    #[test]
    fn gone_after_exactly_three_retry_clicks() {
        let mut clicks = 0;
        let got = create_chat_retry_until(
            CREATE_CHAT_MAX_RETRY_CLICKS,
            |n| if n == 4 { CreateChatStatus::Gone } else { CreateChatStatus::StillPresent(r()) },
            |_, _| clicks += 1,
        );
        assert_eq!(got, Ok(4));
        assert_eq!(clicks, 3);
    }

    #[test]
    fn fails_after_three_retry_clicks() {
        let mut clicks = 0;
        let mut checks = 0;
        let got = create_chat_retry_until(
            CREATE_CHAT_MAX_RETRY_CLICKS,
            |_| {
                checks += 1;
                CreateChatStatus::StillPresent(r())
            },
            |_, _| clicks += 1,
        );
        assert_eq!(got, Err(r()));
        assert_eq!(clicks, CREATE_CHAT_MAX_RETRY_CLICKS);
        assert_eq!(checks, CREATE_CHAT_MAX_RETRY_CLICKS + 1);
    }
}
