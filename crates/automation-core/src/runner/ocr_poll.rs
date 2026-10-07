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
//! 3. 没读到且预算还够，隔一个间隔再来一轮（界面可能停在过渡帧上）；
//! 4. 预算用尽仍未找到时，若 OCR 次数 < [`MIN_OCR_ATTEMPTS`]，继续补 OCR 直到满次数
//!    （避免 settle 吃光预算后只 OCR 1 次就失败）。已找到则不必凑满。
//!
//! 总时长通常 ≤ 预算 + 末次 OCR；若触发最少次数补试，可能略超预算。
//! 等了多久、截了几帧、OCR 了几次全部带回，由调用方写进 evidence——
//! "是没等够还是真没切过去"不该靠猜。
//!
//! ## 通用驱动：[`Run::poll_bounded`]
//!
//! 上面那个是"固定区域 + 等停稳 + OCR"这**一种**。转发工作流还需要另外几种探测：
//! 枚举顶层窗找弹层、只看一块区域的指纹变没变、整窗 OCR 之后只认某一条带……
//! 原来每种各写一份循环，**四份之间只有参数不同，收尾判据却各写了一遍**，
//! 并且已经分叉（见 [`poll_should_stop`] 的说明）。
//!
//! 现在统一成 [`Run::poll_bounded`]：探测交给闭包，**预算口径与收尾判据只有一处**。
//! 调用点只回答三个问题——探测什么、预算怎么算、同一轮多个命中取哪一个。

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
/// 未命中时最少 OCR / 尝试次数（1 次初试 + 2 次重试）。找到可提前结束。
pub(super) const MIN_OCR_ATTEMPTS: u32 = 3;

/// 一次轮询的结果与轨迹。
pub(super) struct OcrPoll {
    /// 最后一次 OCR 读到的文字框（逻辑坐标，相对区域左上角）。
    pub boxes: Vec<TextBox>,
    /// 最后一次 OCR 是否读到了目标。
    pub found: bool,
    /// 从开始轮询到拿到结论的总耗时。
    pub waited: Duration,
    /// OCR 次数：找到时可 < [`MIN_OCR_ATTEMPTS`]；未找到时 ≥ [`MIN_OCR_ATTEMPTS`]。
    pub ocr_attempts: u32,
    /// 为判断"停稳"截的指纹帧数。
    pub settle_frames: u32,
    /// 最后一次 OCR 之前区域是否已停稳（`false` = 预算到了还在变，硬着头皮读的）。
    pub settled: bool,
}

impl Run<'_> {
    /// 在 `region` 上「等停稳 → OCR」，直到某次读到含 `needle` 的文字，
    /// 或预算用尽且 OCR 已满 [`MIN_OCR_ATTEMPTS`] 次。
    ///
    /// 预算用完**不报错**：返回 `found = false` 由调用方决定怎么失败（文案归它）。
    /// 若预算尽时 `ocr_attempts < MIN_OCR_ATTEMPTS` 且仍未找到，继续补 OCR 直到满次数。
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
            let budget_exhausted = Instant::now() + interval >= deadline;
            // 找到立即收工；未找到须满最少次数，且预算将尽/已尽才退出。
            if found || (ocr_attempts >= MIN_OCR_ATTEMPTS && budget_exhausted) {
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

/// 有界轮询的**预算口径**。
///
/// ## 为什么保留两种，而不是统一成一种
///
/// 它们回答的是**不同的问题**，而且两处都真的有人需要：
///
/// - [`PollBudget::WallClock`]：从轮询开始到现在一共花了多久，**含** OCR 与枚举顶层窗。
/// - [`PollBudget::SleepOnly`]：我们**给了界面多少时间**——只累计 `sleep(interval)`。
///
/// 一次 OCR、一次 `list_peer_top_windows` 都是几百毫秒。把它算进预算，
/// 慢机器上会有可观的一部分预算被"探测本身"吃掉，等于等待被悄悄压缩；
/// 反过来，只累计 sleep 时，一次卡住的 OCR 不会让轮询提前收尾，总墙钟可能远超预算。
///
/// 哪个对取决于这一步在等什么，所以它是**参数**：原来的四处调用点各自的选择
/// 原样保留在两个变体上，没有替它们做判断（要统一得先看现场数据，
/// 见代码里的 `TODO(T33)`）。
#[derive(Debug, Clone, Copy)]
pub(super) enum PollBudget {
    /// 墙钟口径：`budget` 是总时长上限。
    WallClock(Duration),
    /// 只累计「等界面」：`budget` 只约束 `sleep` 的总和。
    SleepOnly(Duration),
}

impl PollBudget {
    /// `waited` = 累计 sleep；`wall` = 从轮询开始的墙钟；`interval` = 下一次会睡多久。
    ///
    /// 墙钟口径**预支**下一次间隔（`wall + interval >= budget`）：既然再睡一轮必然
    /// 越过预算，这一轮就该收尾，而不是先睡满再判。只累计 sleep 的口径不预支——
    /// 它的 `waited` 只是 sleep 之和，不存在"接下来必然越界"的说法。
    fn exhausted(&self, waited: Duration, wall: Duration, interval: Duration) -> bool {
        match *self {
            Self::WallClock(budget) => wall + interval >= budget,
            Self::SleepOnly(budget) => waited >= budget,
        }
    }
}

/// 一次有界轮询的轨迹，供调用方写进 evidence。
///
/// 带上它是因为 **「没等够」和「动作真没生效」是两种失败**，处置方向相反：
/// 前者该调大预算，后者该去查客户端。只报一句"等待超时"会把两者合成一个，
/// 排查就失去方向。
#[derive(Debug, Clone, Copy)]
pub(super) struct PollStats {
    /// 循环轮数。
    pub(super) attempts: u32,
    /// 真正做了 OCR 的轮数（没有可用窗口 / 取不到帧的轮次不计）。
    pub(super) ocr_attempts: u32,
    /// 计入预算的等待时长。
    pub(super) waited: Duration,
    /// 墙钟时长。`SleepOnly` 口径下它与 `waited` 会明显不等——那个差就是探测本身的开销。
    pub(super) wall: Duration,
}

/// 有界轮询的结论。
pub(super) enum PollOutcome<T> {
    Found { value: T, stats: PollStats },
    /// 预算用尽（且已凑够最少尝试次数）。**不是错误**——见 [`Run::poll_bounded`]。
    Exhausted { stats: PollStats },
}

/// 单轮探测的结果：**有没有做 OCR** 与 **有没有命中** 是两件事。
///
/// - 「这一轮没有可用窗口」（弹层还没出现）⇒ [`Round::skipped`]；
/// - 「有窗口，但没读到目标文字」⇒ [`Round::missed`]。
///
/// 收尾判据（[`poll_should_stop`]）按这个区分决定要不要凑满 OCR 次数：
/// 一直等不到窗口时 `ocr_attempts` 永远是 0，只能退而看循环次数。
pub(super) struct Round<T> {
    did_ocr: bool,
    hit: Option<T>,
}

impl<T> Round<T> {
    /// 这一轮没有可探测的对象（没有窗口 / 取不到帧）。
    pub(super) fn skipped() -> Self {
        Self { did_ocr: false, hit: None }
    }

    /// 做了 OCR，但没命中。
    pub(super) fn missed() -> Self {
        Self { did_ocr: true, hit: None }
    }

    /// 命中。
    pub(super) fn hit(value: T) -> Self {
        Self { did_ocr: true, hit: Some(value) }
    }
}

/// 有界轮询的**唯一收尾判据**：预算用尽 **且** 已凑够最少尝试次数。
///
/// ## 为什么凭"预算用尽"还不够，还要凑够次数
///
/// 慢机器上预算用尽时可能只截到 1~2 帧（一次截图本身就要几十毫秒）。
/// 此时收尾并把"没读到"当结论，等于把「我们没看够」说成「界面上没有」。
/// 所以还要求 [`MIN_OCR_ATTEMPTS`] 次**真正的 OCR**。
///
/// ## `did_ocr_last_round` 为什么是"本轮"而不是"历史上有没有过"
///
/// 弹层一直没出现时整轮都做不了 OCR（`ocr_attempts` 停在 0），
/// 这时退而用循环次数兜底。⚠️ 用**本轮**的标志意味着「上一轮有窗、这一轮没窗」
/// 会走次数兜底——这是从原来那几份手写循环里**原样保留**的行为，
/// 不是为了新写法才这么定的（见 `TODO(T33)`）。
fn poll_should_stop(
    budget_exhausted: bool,
    ocr_attempts: u32,
    attempts: u32,
    did_ocr_last_round: bool,
) -> bool {
    if !budget_exhausted {
        return false;
    }
    ocr_attempts >= MIN_OCR_ATTEMPTS || (!did_ocr_last_round && attempts >= MIN_OCR_ATTEMPTS)
}

impl<'a> Run<'a> {
    /// 有界轮询：预算内反复 `probe`，命中即返回；预算用尽且凑够次数则如实收尾。
    ///
    /// ## 它替掉了什么
    ///
    /// 转发工作流里原来有**四份**结构相同、收尾条件各写一遍的循环
    /// （`runner/yolo_forward_ops.rs` 里两处"找结果行"、两处"找弹层按钮"）。
    /// 四份的差异只有三样：**探测什么**、**预算怎么算**、**同一轮多个命中取哪个**
    /// ——那是参数，不该是四份代码（`CONVENTIONS.md` §1.3「判据只有一处」）。
    ///
    /// ## 预算用尽**不**返回错误
    ///
    /// 返回 [`PollOutcome::Exhausted`]：失败文案必须写清"等的是哪个弹层、
    /// 最后顶层窗长什么样"，那只有调用点知道。平台层与 OCR 的**硬错误**
    /// 照常向上抛——那是另一回事，不该被吞成"没等到"。
    pub(super) fn poll_bounded<T>(
        &mut self,
        budget: PollBudget,
        interval: Duration,
        mut probe: impl FnMut(&mut Run<'a>) -> Result<Round<T>, AutomationError>,
    ) -> Result<PollOutcome<T>, AutomationError> {
        let started = Instant::now();
        let mut waited = Duration::ZERO;
        let mut attempts = 0u32;
        let mut ocr_attempts = 0u32;
        loop {
            attempts += 1;
            self.check_cancel()?;
            let round = probe(self)?;
            if round.did_ocr {
                ocr_attempts += 1;
            }
            let stats = PollStats {
                attempts,
                ocr_attempts,
                waited,
                wall: started.elapsed(),
            };
            if let Some(value) = round.hit {
                return Ok(PollOutcome::Found { value, stats });
            }
            let exhausted = budget.exhausted(waited, started.elapsed(), interval);
            if poll_should_stop(exhausted, ocr_attempts, attempts, round.did_ocr) {
                return Ok(PollOutcome::Exhausted { stats });
            }
            std::thread::sleep(interval);
            waited += interval;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_not_exhausted_never_stops_the_poll() {
        // 只要预算还有余量，无论看了多少轮都不收尾——
        // 收尾意味着"再等也不会变"，而预算没到就不成立。
        assert!(!poll_should_stop(false, 99, 99, true));
        assert!(!poll_should_stop(false, 0, 99, false));
    }

    #[test]
    fn stops_only_after_min_ocr_attempts_when_a_window_keeps_being_found() {
        // 每轮都能 OCR 时，看的是 OCR 次数：不到 MIN 就不许收尾，
        // 否则"我们只看了 1 帧"会被说成"界面上没有"。
        assert!(!poll_should_stop(true, MIN_OCR_ATTEMPTS - 1, 99, true));
        assert!(poll_should_stop(true, MIN_OCR_ATTEMPTS, 1, true));
    }

    #[test]
    fn falls_back_to_round_count_when_no_window_ever_appeared() {
        // 弹层一直没出现 ⇒ ocr_attempts 恒为 0。此时若仍按 OCR 次数收尾，
        // 会永远凑不满，轮询不会结束。所以要按循环轮数兜底。
        assert!(!poll_should_stop(true, 0, MIN_OCR_ATTEMPTS - 1, false));
        assert!(poll_should_stop(true, 0, MIN_OCR_ATTEMPTS, false));
    }

    #[test]
    fn a_window_that_vanishes_this_round_uses_the_round_fallback() {
        // ⚠️ 这是**保留**的既有行为：兜底看的是"本轮有没有做 OCR"，
        // 不是"历史上有没有做过"。所以"上一轮有窗、这一轮没窗"会走轮数兜底，
        // 即使 OCR 次数一个都没攒下。钉住它是为了改动它的人先看见这条注释。
        assert!(poll_should_stop(true, 0, MIN_OCR_ATTEMPTS, false));
    }

    #[test]
    fn wall_clock_budget_anticipates_the_next_sleep() {
        let budget = PollBudget::WallClock(Duration::from_millis(1000));
        let interval = Duration::from_millis(200);
        // 已经走了 800ms：再睡 200ms 正好到点 ⇒ 这一轮就该收尾，不必先睡满。
        assert!(budget.exhausted(Duration::ZERO, Duration::from_millis(800), interval));
        assert!(!budget.exhausted(Duration::ZERO, Duration::from_millis(799), interval));
        // 墙钟口径**不看** waited——两者在它这里是同一个量的两种记法。
        assert!(!budget.exhausted(Duration::from_secs(99), Duration::ZERO, interval));
    }

    #[test]
    fn sleep_only_budget_ignores_wall_clock() {
        let budget = PollBudget::SleepOnly(Duration::from_millis(1000));
        let interval = Duration::from_millis(200);
        // 墙钟已经 99 秒也不收尾：那 99 秒是 OCR / 枚举窗的耗时，不是"给界面的时间"。
        assert!(!budget.exhausted(Duration::from_millis(999), Duration::from_secs(99), interval));
        assert!(budget.exhausted(Duration::from_millis(1000), Duration::ZERO, interval));
    }
}
