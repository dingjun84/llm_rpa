//! 姓名匹配判据的**轨迹**：同一个函数同时给出「选中谁」与「每个候选为什么」。
//!
//! ## 为什么判定体搬到这里，而不是在 `policy.rs` 里顺手记一笔日志
//!
//! 一张标注图能回答「程序看到了什么」，回答不了「它为什么这么判」——
//! 而排查时缺的恰恰是后者（见 [`crate::diagnostics::Verdict`] 里记的那个现场）。
//! 要让「界面上看到的理由」和「实际用的规则」永远一致，唯一的办法是
//! **让判据函数自己把轨迹交出来**（`CONVENTIONS.md` §1.3）。
//!
//! 所以两个匹配器的判定体都落在这里，并且都走同一个形状：
//!
//! ```text
//! ① evaluate_strict / relaxed_outcome   ← 每个候选过没过（中间结果）
//! ② result_of / blocked_error           ← 由中间结果得出**结论**
//! ③ verdicts_strict / verdicts_relaxed  ← 由中间结果得出**轨迹**
//! ```
//!
//! ②③ 读的是同一份中间结果，「先判一次、再照判据另写一遍理由」在这条路上走不通。
//!
//! ③（轨迹的措辞）单独在 [`verdicts`] 里——它讲的是"每块文字为什么"，
//! 与这里"选中谁"是两件事，拆开之后改措辞不用碰判据。
//!
//! ## 为什么用**下标**而不是 `&TextBox`
//!
//! 轨迹要按画面上的顺序逐块写出来（一条 [`Verdict`](crate::diagnostics::Verdict)
//! 对应一块文字），下标是唯一稳定的对齐方式；用引用会给每个字段都挂上生命周期参数，
//! 而这里没有任何省下克隆的价值（一次判定几十条）。

use super::{ContainsNameMatcher, StrictContactMatcher};
use crate::diagnostics::MatchTrail;
use crate::ports::{AutomationError, TextBox};

mod verdicts;
use verdicts::{verdicts_all_rejected, verdicts_relaxed, verdicts_strict};

/// 严格匹配这条判据的名字（写进决策记录，重放时按它选判据）。
const STRICT_RULE: &str = "姓名逐字精确匹配";
/// 放宽层这条判据的名字。它**先走严格**，只有严格落空才退化（见 [`ContainsNameMatcher`]）。
const CONTAINS_RULE: &str = "姓名包含匹配（临时放宽层）";

/// 判定为什么没选中任何人。
///
/// 结论的文案（[`blocked_error`]）与每一行的理由（[`verdicts_strict`]）都从这个变体派生。
enum Blocked {
    /// 一个候选都没达到最低置信度。
    NoCandidate,
    /// 达到置信度的里面，没有一个逐字相等。
    NoExactMatch,
    /// 逐字相等的有 `n` 个 ⇒ 拒绝猜测。
    Duplicate(usize),
    /// 唯一的逐字命中，但文字框尺寸无效（宽或高为 0，点了也不知道点在哪）。
    Degenerate,
    /// 唯一的逐字命中，但它与下标 `other` 的候选近到分不开。
    TooClose { other: usize },
}

/// 严格判据的中间结果。结论与轨迹都从它派生。
struct Eval {
    /// 达到最低置信度的候选下标，顺序与画面一致。
    accepted: Vec<usize>,
    /// 其中逐字等于目标名或别名的下标。
    exact: Vec<usize>,
    /// 唯一的逐字命中——**哪怕它随后被最后两道检查拦下**（拦下的理由在 `blocked`）。
    unique_exact: Option<usize>,
    blocked: Option<Blocked>,
}

/// 放宽层的中间结果。与 [`Eval`] 同一个道理。
enum Relaxed {
    /// 一个包含命中的都没有。
    NoHit,
    /// 选中的是它（这一帧只有一个包含命中）。
    Selected(usize),
    /// 命中多个，取最短的那一行。
    Shortest { picked: usize, shortest: usize },
    /// 最短的并列（长度相同）⇒ 拒绝猜测。
    Tie { shortest: usize },
    /// 最后两道检查拦下来了；`on` 是被拦下的那一块（写理由时要指名道姓）。
    Blocked { blocked: Blocked, on: usize },
}

/// 严格判据：挑出**逐字**相等的那个人。返回结论与轨迹。
pub(super) fn strict_judge(
    matcher: &StrictContactMatcher,
    expected_name: &str,
    candidates: &[TextBox],
    min_confidence: f32,
) -> (Result<TextBox, AutomationError>, MatchTrail) {
    if let Err(err) = StrictContactMatcher::ensure_name_present(expected_name) {
        let trail = MatchTrail {
            rule: STRICT_RULE,
            relaxed: false,
            candidates: verdicts_all_rejected(candidates, "目标名称为空，没有进入判定"),
        };
        return (Err(err), trail);
    }
    let eval = evaluate_strict(matcher, expected_name, candidates, min_confidence);
    let trail = MatchTrail {
        rule: STRICT_RULE,
        relaxed: false,
        candidates: verdicts_strict(&eval, matcher, expected_name, candidates, min_confidence),
    };
    (result_of(&eval, expected_name, candidates, min_confidence), trail)
}

/// 放宽层：先走严格；严格**已经选出人**或报**歧义**时不降级，
/// 只有「一个都没匹配上」才退化为包含匹配。返回结论与轨迹。
pub(super) fn contains_judge(
    matcher: &ContainsNameMatcher,
    expected_name: &str,
    candidates: &[TextBox],
    min_confidence: f32,
) -> (Result<TextBox, AutomationError>, MatchTrail) {
    if let Err(err) = StrictContactMatcher::ensure_name_present(expected_name) {
        let trail = MatchTrail {
            rule: CONTAINS_RULE,
            relaxed: false,
            candidates: verdicts_all_rejected(candidates, "目标名称为空，没有进入判定"),
        };
        return (Err(err), trail);
    }
    let strict = evaluate_strict(matcher.strict(), expected_name, candidates, min_confidence);
    if !matches!(strict.blocked, Some(Blocked::NoExactMatch)) {
        // 命中、歧义、没有合格候选——三条路都**不降级**：放宽只会让它更歧义，
        // 绝不会更确定；而"没有合格候选"降到包含匹配也一样没有候选。
        let trail = MatchTrail {
            rule: CONTAINS_RULE,
            relaxed: false,
            candidates: verdicts_strict(
                &strict,
                matcher.strict(),
                expected_name,
                candidates,
                min_confidence,
            ),
        };
        let result = result_of(&strict, expected_name, candidates, min_confidence);
        return (result, trail);
    }
    relaxed_judge(matcher.strict(), expected_name, candidates, min_confidence)
}

/// 放宽匹配：把「文字**包含**目标名」的候选挑出来。
///
/// 走到这里的前提是严格判据报了「没有逐字相等的」——所以 `accepted` 一定非空
/// （空的话严格判据报的是「没有合格候选」，在上面就返回了）。
fn relaxed_judge(
    matcher: &StrictContactMatcher,
    expected_name: &str,
    candidates: &[TextBox],
    min_confidence: f32,
) -> (Result<TextBox, AutomationError>, MatchTrail) {
    let name = expected_name.trim();
    let accepted = accepted_indices(candidates, min_confidence);
    let hits: Vec<usize> = accepted
        .iter()
        .copied()
        .filter(|&i| candidates[i].text.contains(name))
        .collect();
    let outcome = relaxed_outcome(matcher, candidates, &accepted, &hits);
    let result = match &outcome {
        Relaxed::Selected(i) => Ok(candidates[*i].clone()),
        Relaxed::Shortest { picked, .. } => Ok(candidates[*picked].clone()),
        Relaxed::NoHit => Err(AutomationError::NeedsHumanReview(format!(
            "未找到名称包含「{name}」的联系人（已放宽为包含匹配）"
        ))),
        Relaxed::Tie { shortest } => Err(AutomationError::AmbiguousVision(format!(
            "有 {} 个候选都包含「{name}」，且其中最短的几个一样长（{shortest} 字），\
             无法确定点击目标",
            hits.len()
        ))),
        Relaxed::Blocked { blocked, on } => {
            Err(blocked_error(blocked, expected_name, candidates, Some(*on), min_confidence))
        }
    };
    let trail = MatchTrail {
        rule: CONTAINS_RULE,
        relaxed: true,
        candidates: verdicts_relaxed(
            &outcome,
            candidates,
            &accepted,
            &hits,
            name,
            min_confidence,
            matcher.min_separation_px,
        ),
    };
    (result, trail)
}

/// 严格判据的中间结果。**判据只有这一处**，`result_of` 与 `verdicts_strict` 都读它。
fn evaluate_strict(
    matcher: &StrictContactMatcher,
    expected_name: &str,
    candidates: &[TextBox],
    min_confidence: f32,
) -> Eval {
    let accepted = accepted_indices(candidates, min_confidence);
    if accepted.is_empty() {
        return Eval {
            accepted,
            exact: Vec::new(),
            unique_exact: None,
            blocked: Some(Blocked::NoCandidate),
        };
    }
    let exact = exact_indices(matcher, expected_name, candidates, &accepted);
    match exact.len() {
        0 => Eval { accepted, exact, unique_exact: None, blocked: Some(Blocked::NoExactMatch) },
        1 => {
            let hit = exact[0];
            let blocked = confirm_blocked(matcher, candidates, &accepted, hit);
            Eval { accepted, exact, unique_exact: Some(hit), blocked }
        }
        n => Eval { accepted, exact, unique_exact: None, blocked: Some(Blocked::Duplicate(n)) },
    }
}

/// 放宽层的中间结果。
fn relaxed_outcome(
    matcher: &StrictContactMatcher,
    candidates: &[TextBox],
    accepted: &[usize],
    hits: &[usize],
) -> Relaxed {
    match hits.len() {
        0 => Relaxed::NoHit,
        1 => match confirm_blocked(matcher, candidates, accepted, hits[0]) {
            Some(blocked) => Relaxed::Blocked { blocked, on: hits[0] },
            None => Relaxed::Selected(hits[0]),
        },
        _ => {
            // 命中多个时取**最短**的那个：姓名行只有姓名，群消息预览行是
            // 「发送者：消息内容」，一定更长（实测数据见 `policy.rs` 的用例）。
            let shortest = hits
                .iter()
                .map(|&i| candidates[i].text.trim().chars().count())
                .min()
                .unwrap_or(0);
            let mut shortest_hits = hits
                .iter()
                .copied()
                .filter(|&i| candidates[i].text.trim().chars().count() == shortest);
            let first = shortest_hits.next().expect("hits 非空，最短的那个必然存在");
            if shortest_hits.next().is_some() {
                return Relaxed::Tie { shortest };
            }
            match confirm_blocked(matcher, candidates, accepted, first) {
                Some(blocked) => Relaxed::Blocked { blocked, on: first },
                None => Relaxed::Shortest { picked: first, shortest },
            }
        }
    }
}

/// 由中间结果得出**结论**。
fn result_of(
    eval: &Eval,
    expected_name: &str,
    candidates: &[TextBox],
    min_confidence: f32,
) -> Result<TextBox, AutomationError> {
    if let Some(blocked) = &eval.blocked {
        return Err(blocked_error(blocked, expected_name, candidates, eval.unique_exact, min_confidence));
    }
    let hit = eval.unique_exact.expect("没有 blocked 就只有一种情况：唯一的命中被选中了");
    Ok(candidates[hit].clone())
}

/// 把「为什么没选中」翻成给调用方看的那句话。
///
/// **只在这里写一次**：严格路径与放宽路径都从这里取文案。两条路各写一份的话，
/// 同一个原因迟早会出现两种措辞，而失败文案是操作者唯一能看到的东西。
fn blocked_error(
    blocked: &Blocked,
    expected_name: &str,
    candidates: &[TextBox],
    unique: Option<usize>,
    min_confidence: f32,
) -> AutomationError {
    let name = expected_name.trim();
    match blocked {
        Blocked::NoCandidate => AutomationError::NeedsHumanReview(format!(
            "该区域没有达到最低置信度 {min_confidence} 的文字候选"
        )),
        Blocked::NoExactMatch => AutomationError::NeedsHumanReview(format!(
            "未找到与「{name}」逐字匹配的联系人"
        )),
        Blocked::Duplicate(n) => AutomationError::AmbiguousVision(format!(
            "存在 {n} 个与「{name}」逐字匹配的联系人，拒绝猜测"
        )),
        Blocked::Degenerate => {
            AutomationError::AmbiguousVision("匹配到的联系人文字框尺寸无效".into())
        }
        Blocked::TooClose { .. } => {
            let text = unique.map(|i| candidates[i].text.trim().to_string()).unwrap_or_default();
            AutomationError::AmbiguousVision(format!(
                "「{text}」附近存在过于接近的候选文字框，无法确定点击目标"
            ))
        }
    }
}

/// 达到最低置信度的候选下标，顺序与画面一致。
fn accepted_indices(candidates: &[TextBox], min_confidence: f32) -> Vec<usize> {
    candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.confidence >= min_confidence)
        .map(|(i, _)| i)
        .collect()
}

/// 逐字等于目标名（或管理员配的别名）的候选下标。
fn exact_indices(
    matcher: &StrictContactMatcher,
    expected_name: &str,
    candidates: &[TextBox],
    accepted: &[usize],
) -> Vec<usize> {
    accepted
        .iter()
        .copied()
        .filter(|&i| exact_term(matcher, expected_name, &candidates[i].text).is_some())
        .collect()
}

/// 这段文字**逐字**等于哪个词（目标名本身或它的一条别名）；都不等则 `None`。
///
/// 「怎么算逐字相等」只由这一个函数说了算：判定（[`exact_indices`]）与
/// 写理由（`verdicts_strict`）都问它，于是理由里报的那个词一定是判据用的那个词。
fn exact_term<'a>(
    matcher: &'a StrictContactMatcher,
    expected_name: &'a str,
    text: &str,
) -> Option<&'a str> {
    let canonical = StrictContactMatcher::canonical(text);
    matcher
        .accepted_terms(expected_name)
        .into_iter()
        .find(|term| StrictContactMatcher::canonical(term) == canonical)
}

/// 唯一命中之后的最后两道检查。**与"怎么算命中"无关，两种模式共用**。
fn confirm_blocked(
    matcher: &StrictContactMatcher,
    candidates: &[TextBox],
    accepted: &[usize],
    hit: usize,
) -> Option<Blocked> {
    if candidates[hit].bounds.is_degenerate() {
        return Some(Blocked::Degenerate);
    }
    accepted
        .iter()
        .copied()
        .find(|&j| {
            j != hit
                && StrictContactMatcher::centers_too_close(
                    candidates[j].bounds,
                    candidates[hit].bounds,
                    matcher.min_separation_px,
                )
        })
        .map(|other| Blocked::TooClose { other })
}

#[cfg(test)]
mod tests;