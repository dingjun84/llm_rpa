//! **每一块文字为什么过 / 为什么被淘汰**——轨迹的措辞层。
//!
//! 拆出来的理由不是"文件太长"，而是这里讲的是**另一件事**：
//! 父模块（`trail.rs`）决定"选中谁"，这里只把那个决定翻成人能读的理由。
//! 两者读的是同一份中间结果（[`Eval`] / [`Relaxed`]），所以
//! 「先判一次、再照判据另写一遍理由」在这条路上走不通（`CONVENTIONS.md` §1.3）。
//!
//! ⚠️ 这里**不重写判据**。改判定只看父模块；改措辞只改这里。

use super::*;
use crate::diagnostics::Verdict;

/// 严格路径的**轨迹**：逐块写清它过没过、为什么。
pub(super) fn verdicts_strict(
    eval: &Eval,
    matcher: &StrictContactMatcher,
    expected_name: &str,
    candidates: &[TextBox],
    min_confidence: f32,
) -> Vec<Verdict> {
    candidates
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if !eval.accepted.contains(&i) {
                return Verdict::rejected(
                    c,
                    format!(
                        "置信度 {:.2} 低于阈值 {:.2}，没参与判定",
                        c.confidence, min_confidence
                    ),
                );
            }
            // 「两块太近」这条要写在**两行**上，而且措辞不同：命中那一块说"旁边有谁"，
            // 挡住它的那一块说"我挡住了谁"。两行都由这一个函数给，免得各说各话。
            if let Some(reason) = blocked_row_reason(
                eval.blocked.as_ref(),
                eval.unique_exact,
                i,
                candidates,
                matcher.min_separation_px,
            ) {
                return Verdict::rejected(c, reason);
            }
            if !eval.exact.contains(&i) {
                return Verdict::rejected(
                    c,
                    format!("文字与「{}」不逐字相等（本判据不做包含、不做相似匹配）", expected_name.trim()),
                );
            }
            let via = match exact_term(matcher, expected_name, &c.text) {
                Some(term) => format!("「{term}」"),
                None => format!("「{}」", expected_name.trim()),
            };
            if eval.blocked.is_none() {
                return Verdict::passed(c, format!("逐字等于{via}，是本次唯一的精确匹配"));
            }
            let why = match &eval.blocked {
                Some(Blocked::Duplicate(n)) => {
                    format!("逐字等于{via}，但一共有 {n} 个都逐字相等 → 拒绝猜测")
                }
                Some(Blocked::Degenerate) => {
                    format!("逐字等于{via}，但文字框的宽或高为 0，点了也不知道点在哪")
                }
                _ => format!("逐字等于{via}，但不是这一帧选中的那一块"),
            };
            Verdict::rejected(c, why)
        })
        .collect()
}

/// 放宽路径的**轨迹**。每条命中都要说清它为什么被取或被舍——
/// 「取最短的那一行」正是最容易被当成"随便挑一个"的地方。
pub(super) fn verdicts_relaxed(
    outcome: &Relaxed,
    candidates: &[TextBox],
    accepted: &[usize],
    hits: &[usize],
    name: &str,
    min_confidence: f32,
    min_separation_px: i32,
) -> Vec<Verdict> {
    let (blocked, on) = match outcome {
        Relaxed::Blocked { blocked, on } => (Some(blocked), Some(*on)),
        _ => (None, None),
    };
    candidates
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if !accepted.contains(&i) {
                return Verdict::rejected(
                    c,
                    format!(
                        "置信度 {:.2} 低于阈值 {:.2}，没参与判定",
                        c.confidence, min_confidence
                    ),
                );
            }
            if let Some(reason) =
                blocked_row_reason(blocked, on, i, candidates, min_separation_px)
            {
                return Verdict::rejected(c, reason);
            }
            if !hits.contains(&i) {
                return Verdict::rejected(
                    c,
                    format!("文字里没有「{name}」（精确匹配落空后已放宽到包含匹配，仍然不认它）"),
                );
            }
            let len = c.text.trim().chars().count();
            match outcome {
                Relaxed::Selected(_) => {
                    Verdict::passed(c, format!("文字里包含「{name}」，是这一帧唯一的包含命中"))
                }
                Relaxed::Shortest { picked, shortest } if *picked == i => Verdict::passed(
                    c,
                    format!(
                        "文字里包含「{name}」，并且在 {} 个命中里是最短的一行（{shortest} 字）",
                        hits.len()
                    ),
                ),
                Relaxed::Shortest { shortest, .. } => Verdict::rejected(
                    c,
                    format!("文字里包含「{name}」，但比最短的那一行长（{len} 字 > {shortest} 字）"),
                ),
                Relaxed::Tie { shortest } => Verdict::rejected(
                    c,
                    format!(
                        "文字里包含「{name}」，且最短的几个一样长（{shortest} 字）→ 无法确定点哪个"
                    ),
                ),
                // 「太近」「尺寸无效」那两行上面已经写过理由了，剩下的只有被拦下的那一块自己。
                Relaxed::Blocked { .. } => Verdict::rejected(
                    c,
                    format!("文字里包含「{name}」，但这块本身有问题，这一帧没能定下点击目标"),
                ),
                // 走到这里说明"有命中却报了没有命中"——不该发生；如实写出来，不编理由。
                Relaxed::NoHit => Verdict::rejected(c, "这一帧没有任何包含命中"),
            }
        })
        .collect()
}

/// 「这一块为什么没能成为答案」——只在它**是被拦下的那一块**或**挡住它的那一块**时才给理由。
///
/// `on` 是被拦下的那一块（严格路径是唯一的逐字命中，放宽路径是最后选中的那一块）。
fn blocked_row_reason(
    blocked: Option<&Blocked>,
    on: Option<usize>,
    i: usize,
    candidates: &[TextBox],
    min_separation_px: i32,
) -> Option<String> {
    let blocked = blocked?;
    let on = on?;
    match blocked {
        Blocked::Degenerate if i == on => {
            Some("文字框的宽或高为 0，点了也不知道点在哪".to_string())
        }
        Blocked::TooClose { other } if i == on => Some(format!(
            "与「{}」近到分不开（中心距离小于 {min_separation_px} px），这一帧无法确定点哪个",
            candidates[*other].text.trim()
        )),
        Blocked::TooClose { other } if i == *other => Some(format!(
            "与命中的那一块「{}」近到分不开（中心距离小于 {min_separation_px} px），\
             所以这一帧谁也点不得",
            candidates[on].text.trim()
        )),
        _ => None,
    }
}

/// 全部候选写同一条理由（用在"还没来得及看候选就失败了"的场合，例如目标名为空）。
pub(super) fn verdicts_all_rejected(candidates: &[TextBox], reason: &str) -> Vec<Verdict> {
    candidates.iter().map(|c| Verdict::rejected(c, reason)).collect()
}