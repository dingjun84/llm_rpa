//! 「点击后验文字」的有界轮询：先等区域停稳，再 OCR；没读到就在预算内再来一轮。
//!
//! ## 为什么要有它
//!
//! 任务 39a1754e 步骤4：点完「文件传输助手」几乎立刻裁标题带做 OCR，
//! 那一帧会话还没切过去，读到的是旧标题（「0-1 @微信」），于是判"未进入会话"。
//! 写死 `sleep` 在慢机器上照样偏短、在快机器上白等；这里改成：
//!
//! 1. 对**同一块区域**按指纹轮询，连续两帧一致 = 停稳（与 `wait_for_settle` 同一思路）；
//! 2. 停稳（或预算用完）后 OCR 一次，读到目标就收工；
//! 3. 没读到且预算还够，隔一个间隔再来一轮（界面可能停在过渡帧上）。
//!
//! 总时长 ≤ 预算 + 最后一次 OCR 的耗时；等了多久、截了几帧、OCR 了几次全部带回，
//! 由调用方写进 evidence——"是没等够还是真没切过去"不该靠猜。

use std::time::{Duration, Instant};

use crate::ports::{AutomationError, Rect, TextBox};

use super::Run;

/// 点击后验标题的默认总预算（不含最后一次 OCR 本身的耗时）。
pub(super) const HEADER_POLL_BUDGET: Duration = Duration::from_millis(1800);
/// 轮询间隔：截一帧本身几十毫秒，再密只是重复截同一帧。
pub(super) const HEADER_POLL_INTERVAL: Duration = Duration::from_millis(200);
/// 弹出层（菜单 / 转发对话框）出现偶发更慢：右击→菜单、点「转发」→选人窗等。
/// evidence 会写明用的是本预算而非 HEADER_POLL_BUDGET。
pub(super) const PEER_POLL_BUDGET: Duration = Duration::from_millis(2500);

/// 一次轮询的结果与轨迹。
pub(super) struct OcrPoll {
    /// 最后一次 OCR 读到的文字框（逻辑坐标，相对区域左上角）。
    pub boxes: Vec<TextBox>,
    /// 最后一次 OCR 是否读到了目标。
    pub found: bool,
    /// 从开始轮询到拿到结论的总耗时。
    pub waited: Duration,
    /// OCR 次数（≥ 1）。
    pub ocr_attempts: u32,
    /// 为判断"停稳"截的指纹帧数。
    pub settle_frames: u32,
    /// 最后一次 OCR 之前区域是否已停稳（`false` = 预算到了还在变，硬着头皮读的）。
    pub settled: bool,
}

impl Run<'_> {
    /// 在 `region` 上「等停稳 → OCR」，直到某次读到含 `needle` 的文字或预算用完。
    ///
    /// 预算用完**不报错**：返回 `found = false` 由调用方决定怎么失败（文案归它）。
    /// 平台 / OCR 的硬错误照常向上抛。
    pub(super) fn ocr_until_contains(
        &mut self,
        region: Rect,
        label: &str,
        needle: &str,
        budget: Duration,
        interval: Duration,
    ) -> Result<OcrPoll, AutomationError> {
        let started = Instant::now();
        let deadline = started + budget;
        let settle_label = format!("{label}·停稳");
        let mut ocr_attempts = 0;
        let mut settle_frames = 0;
        loop {
            let settled =
                self.wait_region_settled(region, &settle_label, interval, deadline, &mut settle_frames)?;
            ocr_attempts += 1;
            let (_shot, boxes) = self.capture_and_recognize(region, label)?;
            let found = boxes.iter().any(|b| b.text.contains(needle));
            if found || Instant::now() + interval >= deadline {
                return Ok(OcrPoll {
                    boxes,
                    found,
                    waited: started.elapsed(),
                    ocr_attempts,
                    settle_frames,
                    settled,
                });
            }
            self.check_cancel()?;
            std::thread::sleep(interval);
        }
    }

    /// 截 `region` 的指纹直到连续两帧一致（`true`）或到 `deadline`（`false`）。
    ///
    /// 只截帧不 OCR（走 [`Self::capture_frame`]）：OCR 太贵，而"动没动"只要指纹。
    fn wait_region_settled(
        &mut self,
        region: Rect,
        label: &str,
        interval: Duration,
        deadline: Instant,
        frames: &mut u32,
    ) -> Result<bool, AutomationError> {
        let mut previous = self.capture_frame(region, label)?.fingerprint;
        *frames += 1;
        while Instant::now() < deadline {
            self.check_cancel()?;
            std::thread::sleep(interval);
            let current = self.capture_frame(region, label)?.fingerprint;
            *frames += 1;
            if current == previous {
                return Ok(true);
            }
            previous = current;
        }
        Ok(false)
    }
}
