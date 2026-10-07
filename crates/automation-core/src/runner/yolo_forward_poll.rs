//! 转发工作流里**所有**有界轮询及其参数（从 `yolo_forward_ops` 拆出）。
//!
//! ## 为什么单独一个文件
//!
//! 这一段自成一类：**等某个东西出现，再决定点哪儿**。它原来在 `yolo_forward_ops.rs`
//! 里有四份各自内联的循环，收尾条件各写一遍且已经分叉；收敛成一份之后仍有三百行，
//! 把那个文件顶到了 500 行上限之上（`CONVENTIONS.md` §2/§9）。
//! 于是整段搬出来——搬的是**一整类功能**，不是"按行数对半切"。
//!
//! ## 机制与参数分家
//!
//! - **机制**（预算、收尾判据、重试节奏）只有一处：`runner/ocr_poll.rs::poll_bounded`。
//! - **参数**（这一步探测什么、预算口径、命中了取哪一个）在本文件：
//!   步骤3/10 看 [`BelowPoll`]，步骤7/8/9/11 看 [`PeerPoll`]。
//!
//! 想知道"两个步骤到底差在哪"，只看那两个结构体就够了。

use crate::ports::{AutomationError, PeerTopWindow, Point, Rect, TextBox};

use std::time::Duration;

use super::ocr_poll::{
    PollBudget, PollOutcome, PollStats, Round, HEADER_POLL_BUDGET, HEADER_POLL_INTERVAL,
    MIN_OCR_ATTEMPTS, PEER_POLL_BUDGET,
};
use super::Run;

/// 同一轮里多个命中按什么几何偏好取一个。
///
/// 它**不是**文字判据：客户端自己把最匹配的那一项排在它该在的位置上（菜单项在菜单里、
/// "创建并发送"在右下角），比我们拿文字长短去猜可信。同 `dropdown.rs`「多命中取最上面」。
#[derive(Clone, Copy)]
pub(super) enum Prefer {
    FirstTopLeft,
    BottomRightMost,
}

/// 「输入后列表下方出现目标行」的轮询参数：步骤3（主窗）与步骤10（转发窗）的差异全在这里。
struct BelowPoll<'a> {
    step: u32,
    needle: &'a str,
    /// 锚点（搜索框）屏幕矩形：命中必须在它下方，
    /// 否则会把搜索框里**刚输入的关键词本身**当成一条结果。
    anchor: Rect,
    /// OCR 与「列表指纹」所在区域（主窗 / 转发窗）。
    origin: Rect,
    budget: Duration,
    pick: BelowPick,
    /// OCR 步骤名里的动作词（"下方" / "匹配"），诊断里靠它认出是哪一次识别。
    ocr_label: &'a str,
}

/// 同一轮里出现多个命中框时取哪一个。
#[derive(Clone, Copy)]
enum BelowPick {
    /// 阅读序第一行。
    FirstRow,
    /// 面积最大的一块：下拉里同一行常被 OCR 切成几块（名字、备注名分开），
    /// 取最大那块最接近"整行"，落点也不容易落在边缘上。
    LargestArea,
}

/// 「列表下方轮询」的产物——只交事实，措辞留在调用点（两处文案本来就不一样）。
struct BelowPolled {
    hit: Option<Rect>,
    stats: PollStats,
    /// 列表指纹相对「输入后的基线」是否变化过。
    list_changed: bool,
}

/// 「在弹出层里轮询目标文字」的参数：步骤7/8/11（窗上按钮）与步骤9（窗内搜索框）
/// 的差异全在这里。
struct PeerPoll<'a> {
    step: u32,
    needle: &'a str,
    /// 判定"哪个顶层窗才是这次的弹层"。
    pred: &'a dyn Fn(&PeerTopWindow) -> bool,
    budget: PollBudget,
    /// 同一轮里多个命中按什么几何偏好取一个。
    prefer: Prefer,
}

/// 「弹层轮询」的结论——只交事实，成功/失败文案各留调用点。
enum PeerPolled {
    Found { peer: PeerTopWindow, hit: Rect, stats: PollStats },
    /// 预算用尽。`last_peers` 是最后一次枚举到的顶层窗清单——
    /// 失败文案靠它回答"当时屏幕上到底有哪些窗"，这是弹层流程唯一能拿到的现场。
    Exhausted { stats: PollStats, last_peers: String },
}

impl Run<'_> {
    /// 搜索框与结果行之间的最小垂直间隙（像素）。
    /// 命中顶边须 ≥ anchor 底边 + GAP，排除搜索框带内刚输入的文字。
    const FORWARD_BELOW_GAP_PX: i32 = 10;

    /// 搜索框下方列表区：y 从 anchor 底边起至 `origin`（窗/弹层）底，宽度跟 origin。
    fn forward_list_region_below(origin: Rect, anchor: Rect) -> Rect {
        let y = (anchor.y + anchor.height).max(origin.y);
        let bottom = origin.y + origin.height;
        Rect {
            x: origin.x,
            y,
            width: origin.width,
            height: (bottom - y).max(1),
        }
    }

    /// 「输入后列表下方出现目标行」的轮询（步骤3 / 步骤10 共用）。
    ///
    /// 命中须**同时**满足：列表指纹相对"输入后的基线"变了、且锚点下方出现了目标文字。
    /// 只认文字不够——输入前列表里可能还留着上一次搜索的同名旧内容，
    /// 那会点进一个与这次输入无关的结果。
    fn forward_poll_below(&mut self, p: BelowPoll<'_>) -> Result<BelowPolled, AutomationError> {
        let list_region = Self::forward_list_region_below(p.origin, p.anchor);
        let fp_label = format!("转发·{}·列表指纹", p.step);
        let baseline = self
            .capture_frame(list_region, &format!("{fp_label}基准"))?
            .fingerprint;
        let mut list_changed = false;
        let outcome = self.poll_bounded(
            PollBudget::WallClock(p.budget),
            HEADER_POLL_INTERVAL,
            |run| {
                // 指纹先取、OCR 后取（与原来一致）：指纹便宜，且"列表变了"这个
                // 事实必须在解释这一帧之前就定下来。
                if run.capture_frame(list_region, &fp_label)?.fingerprint != baseline {
                    list_changed = true;
                }
                let (_shot, boxes) = run.capture_and_recognize(
                    p.origin,
                    &format!("转发·{}·{}「{}」", p.step, p.ocr_label, p.needle),
                )?;
                if !list_changed {
                    // 列表还是输入前那一屏 ⇒ 这一帧上的文字不能算结果。
                    return Ok(Round::missed());
                }
                Ok(match Self::forward_filter_below(&boxes, p.origin, p.needle, p.anchor, p.pick) {
                    Some(hit) => Round::hit(hit),
                    None => Round::missed(),
                })
            },
        )?;
        Ok(match outcome {
            PollOutcome::Found { value, stats } => {
                BelowPolled { hit: Some(value), stats, list_changed }
            }
            PollOutcome::Exhausted { stats } => BelowPolled { hit: None, stats, list_changed },
        })
    }

    /// 从 OCR 框里筛 `anchor` 下方含 `needle` 的框，按 [`BelowPick`] 取一个。
    ///
    /// 命中顶边须 ≥ anchor 底边 + [`Run::FORWARD_BELOW_GAP_PX`]，
    /// 把搜索框带内那些字（关键词本身、清除按钮的残字）排除掉。
    /// 两个调用点只差"取哪一块"，所以那是一个参数，不是两份函数（原来确实是两份）。
    fn forward_filter_below(
        boxes: &[TextBox],
        origin: Rect,
        needle: &str,
        anchor: Rect,
        pick: BelowPick,
    ) -> Option<Rect> {
        let min_y = anchor.y + anchor.height + Self::FORWARD_BELOW_GAP_PX;
        let mut hits: Vec<(usize, Rect)> = boxes
            .iter()
            .filter(|b| b.text.contains(needle))
            .map(|b| {
                let area = b.bounds.width.max(0) as usize * b.bounds.height.max(0) as usize;
                (area, b.bounds.to_screen(Point { x: origin.x, y: origin.y }))
            })
            .filter(|(_, r)| r.y >= min_y)
            .collect();
        match pick {
            BelowPick::FirstRow => hits.sort_by_key(|(_, r)| (r.y, r.x)),
            BelowPick::LargestArea => hits
                .sort_by(|a, b| b.0.cmp(&a.0).then(a.1.y.cmp(&b.1.y)).then(a.1.x.cmp(&b.1.x))),
        }
        hits.first().map(|(_, r)| *r)
    }

    /// 步骤3：输入「文件传输助手」后，在**主窗**搜索框下方轮询到那一条结果行。
    ///
    /// 用主窗矩形当 OCR 区域——这一步还没有弹层可枚举（见 [`Self::forward_poll_peer`]）。
    pub(super) fn forward_ocr_below_polled(
        &mut self,
        step: u32,
        needle: &str,
        anchor: Rect,
    ) -> Result<Rect, AutomationError> {
        let window = self.ensure_calibrated()?;
        let polled = self.forward_poll_below(BelowPoll {
            step,
            needle,
            anchor,
            origin: window,
            budget: HEADER_POLL_BUDGET,
            pick: BelowPick::FirstRow,
            ocr_label: "下方",
        })?;
        let stats = polled.stats;
        let changed = if polled.list_changed { "已" } else { "未" };
        self.evidence.push(format!(
            "步骤{step}：整窗轮询下方「{needle}」→ {}（等待 {}ms，OCR {} 次（最少 {} 次）；\
             列表指纹{changed}变化；预算 {}ms 间隔 {}ms）",
            if polled.hit.is_some() { "成功" } else { "未找到" },
            stats.wall.as_millis(),
            stats.attempts,
            MIN_OCR_ATTEMPTS,
            HEADER_POLL_BUDGET.as_millis(),
            HEADER_POLL_INTERVAL.as_millis(),
        ));
        match polled.hit {
            Some(hit) => Ok(hit),
            None => Err(AutomationError::NeedsHumanReview(format!(
                "步骤{step}失败：输入后 {}ms 内（OCR {} 次，最少 {} 次）搜索框下方未见「{needle}」；\
                 列表指纹{changed}变化，可能搜索结果未刷新。",
                stats.wall.as_millis(),
                stats.attempts,
                MIN_OCR_ATTEMPTS,
            ))),
        }
    }

    /// 步骤10：在**转发窗**（`region`）搜索框下方轮询到联系人匹配行。
    ///
    /// 预算比步骤3 长（[`PEER_POLL_BUDGET`]）：前面刚点过菜单与选人窗，弹出层响应更慢。
    /// 取"面积最大的一块"而不是阅读序第一行——这里的命中必须是**被选中的人**。
    pub(super) fn forward_ocr_best_below_polled(
        &mut self,
        step: u32,
        needle: &str,
        anchor: Rect,
        region: Rect,
    ) -> Result<Rect, AutomationError> {
        let polled = self.forward_poll_below(BelowPoll {
            step,
            needle,
            anchor,
            origin: region,
            budget: PEER_POLL_BUDGET,
            pick: BelowPick::LargestArea,
            ocr_label: "匹配",
        })?;
        let stats = polled.stats;
        let changed = if polled.list_changed { "已" } else { "未" };
        self.evidence.push(format!(
            "步骤{step}：弹层轮询下方匹配「{needle}」→ {}（等待 {}ms，OCR {} 次（最少 {} 次）；\
             列表指纹{changed}变化；预算 {}ms 间隔 {}ms）",
            if polled.hit.is_some() { "成功" } else { "未找到" },
            stats.wall.as_millis(),
            stats.attempts,
            MIN_OCR_ATTEMPTS,
            PEER_POLL_BUDGET.as_millis(),
            HEADER_POLL_INTERVAL.as_millis(),
        ));
        match polled.hit {
            Some(hit) => Ok(hit),
            None => Err(AutomationError::NeedsHumanReview(format!(
                "步骤{step}失败：输入后 {}ms 内（OCR {} 次，最少 {} 次）搜索框下未见包含「{needle}」的匹配行；\
                 列表指纹{changed}变化，可能搜索结果未刷新。",
                stats.wall.as_millis(),
                stats.attempts,
                MIN_OCR_ATTEMPTS,
            ))),
        }
    }

    fn summarize_peers(peers: &[PeerTopWindow]) -> String {
        if peers.is_empty() {
            return "(无)".into();
        }
        peers
            .iter()
            .map(|p| {
                format!(
                    "[id={} class=`{}` title=`{}` {}x{} main={}]",
                    p.id, p.class_name, p.title, p.rect.width, p.rect.height, p.is_main
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// 静默枚举并按 `pred` 选最大窗（轮询中间不写 evidence，避免刷屏）。
    ///
    /// `pred` 收 `&dyn Fn`：调用方把谓词**存在** [`PeerPoll`] 里，泛型借出去之后推断不出来。
    fn forward_try_pick_peer(
        &mut self,
        pred: &dyn Fn(&PeerTopWindow) -> bool,
    ) -> Result<(Vec<PeerTopWindow>, Option<PeerTopWindow>), AutomationError> {
        let peers = self.runner.ports.platform.list_peer_top_windows()?;
        let picked = peers
            .iter()
            .filter(|p| pred(p))
            .max_by_key(|p| (p.rect.width as i64) * (p.rect.height as i64))
            .cloned();
        Ok((peers, picked))
    }

    fn forward_prefer_hit(hits: &mut [Rect], prefer: Prefer) {
        match prefer {
            Prefer::FirstTopLeft => hits.sort_by_key(|r| (r.y, r.x)),
            Prefer::BottomRightMost => {
                hits.sort_by_key(|r| (-(r.y + r.height), -(r.x + r.width)))
            }
        }
    }

    /// 在弹出层里轮询目标文字（步骤7/8/9/11 共用）。
    ///
    /// 弹层是**独立顶层窗**（右键菜单、选人窗），位置随内容变，没法预先标定区域。
    /// 所以每轮先 `list_peer_top_windows` 找候选，只对选中那个窗 OCR——比整屏便宜，
    /// 也避免把主窗上的同名文字（会话列表里恰好有这个名字）认成弹层内容。
    ///
    /// ⚠️ 窗口不存在时这一轮做不了 OCR（[`Round::skipped`]），收尾判据对这种轮次
    /// 退而看循环轮数兜底；少了那一条，「弹层还没出现」会被记成「弹层里没有这个按钮」。
    fn forward_poll_peer(&mut self, p: PeerPoll<'_>) -> Result<PeerPolled, AutomationError> {
        let mut last_peers = String::from("(无)");
        let outcome = self.poll_bounded(p.budget, HEADER_POLL_INTERVAL, |run| {
            let (peers, picked) = run.forward_try_pick_peer(p.pred)?;
            last_peers = Self::summarize_peers(&peers);
            let Some(peer) = picked else {
                return Ok(Round::skipped());
            };
            let (_shot, boxes) = run.capture_and_recognize(
                peer.rect,
                &format!("转发·{}·OCR「{}」", p.step, p.needle),
            )?;
            let mut hits: Vec<Rect> = boxes
                .iter()
                .filter(|b| b.text.contains(p.needle))
                .map(|b| b.bounds.to_screen(Point { x: peer.rect.x, y: peer.rect.y }))
                .collect();
            if hits.is_empty() {
                return Ok(Round::missed());
            }
            // 多个命中取哪一个：几何偏好，不是文字判据。客户端自己把最匹配的那一项
            // 排在它该在的位置上（菜单项在菜单里、"创建并发送"在右下角），比我们
            // 拿文字长短去猜可信——与 `dropdown.rs`「取最上面」同一条思路。
            Self::forward_prefer_hit(&mut hits, p.prefer);
            Ok(Round::hit((peer, hits[0])))
        })?;
        Ok(match outcome {
            PollOutcome::Found { value: (peer, hit), stats } => PeerPolled::Found { peer, hit, stats },
            PollOutcome::Exhausted { stats } => PeerPolled::Exhausted { stats, last_peers },
        })
    }

    /// 步骤7/8/11：在弹出层里轮询到 `needle`，再点它。覆盖菜单「转发」、
    /// 「创建聊天」、「创建并发送」。
    ///
    /// 预算走"只累计等界面"的口径：这里每轮都要枚举一次顶层窗 + 一次 OCR，各是几百毫秒，
    /// 算进预算会把等待悄悄压缩。这是原来定下的口径，**没有**替它与步骤9 统一
    /// （要统一得先看现场数据，见 `TODO(T33)`）。
    pub(super) fn forward_click_in_peer(
        &mut self,
        step: u32,
        needle: &str,
        pred: impl Fn(&PeerTopWindow) -> bool,
        prefer: Prefer,
    ) -> Result<(), AutomationError> {
        let polled = self.forward_poll_peer(PeerPoll {
            step,
            needle,
            pred: &pred,
            budget: PollBudget::SleepOnly(PEER_POLL_BUDGET),
            prefer,
        })?;
        match polled {
            PeerPolled::Found { peer, hit, stats } => {
                self.evidence.push(format!(
                    "步骤{step}：弹层轮询「{needle}」→ 成功（等待 {}ms，墙钟 {}ms，尝试 {} 次、OCR {} 次（最少 {} 次）；预算 {}ms 间隔 {}ms；窗 class=`{}` title=`{}` id=`{}`）",
                    stats.waited.as_millis(),
                    stats.wall.as_millis(),
                    stats.attempts,
                    stats.ocr_attempts,
                    MIN_OCR_ATTEMPTS,
                    PEER_POLL_BUDGET.as_millis(),
                    HEADER_POLL_INTERVAL.as_millis(),
                    peer.class_name,
                    peer.title,
                    peer.id,
                ));
                self.forward_click_screen_box(step, needle, Some(&peer), hit)
            }
            PeerPolled::Exhausted { stats, last_peers } => {
                Err(AutomationError::NeedsHumanReview(format!(
                    "步骤{step}失败：等待 {}ms（墙钟 {}ms；尝试 {} 次、OCR {} 次，最少 {} 次）未见弹层「{needle}」。最后顶层窗：{last_peers}",
                    stats.waited.as_millis(),
                    stats.wall.as_millis(),
                    stats.attempts,
                    stats.ocr_attempts,
                    MIN_OCR_ATTEMPTS,
                )))
            }
        }
    }

    /// 步骤9：轮询到转发窗出现、且能在窗里 OCR 到 `needle`，返回 (窗, 命中框屏幕矩形)。
    ///
    /// 预算走**墙钟**口径（与步骤7/8/11 不同，见 `TODO(T33)`）；取阅读序第一行——
    /// 这一步只要找到**搜索框**在哪，不需要在多个命中里做取舍。
    pub(super) fn forward_ocr_first_in_peer_polled(
        &mut self,
        step: u32,
        needle: &str,
        pred: impl Fn(&PeerTopWindow) -> bool,
    ) -> Result<(PeerTopWindow, Rect), AutomationError> {
        let polled = self.forward_poll_peer(PeerPoll {
            step,
            needle,
            pred: &pred,
            budget: PollBudget::WallClock(PEER_POLL_BUDGET),
            prefer: Prefer::FirstTopLeft,
        })?;
        match polled {
            PeerPolled::Found { peer, hit, stats } => {
                self.evidence.push(format!(
                    "步骤{step}：弹层轮询「{needle}」→ 成功（等待 {}ms，尝试 {} 次、OCR {} 次（最少 {} 次）；预算 {}ms 间隔 {}ms；窗 class=`{}` title=`{}` id=`{}`）",
                    stats.wall.as_millis(),
                    stats.attempts,
                    stats.ocr_attempts,
                    MIN_OCR_ATTEMPTS,
                    PEER_POLL_BUDGET.as_millis(),
                    HEADER_POLL_INTERVAL.as_millis(),
                    peer.class_name,
                    peer.title,
                    peer.id,
                ));
                Ok((peer, hit))
            }
            PeerPolled::Exhausted { stats, last_peers } => {
                Err(AutomationError::NeedsHumanReview(format!(
                    "步骤{step}失败：等待 {}ms（尝试 {} 次、OCR {} 次，最少 {} 次）未见弹层「{needle}」。最后顶层窗：{last_peers}",
                    stats.wall.as_millis(),
                    stats.attempts,
                    stats.ocr_attempts,
                    MIN_OCR_ATTEMPTS,
                )))
            }
        }
    }
}
