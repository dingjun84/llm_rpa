//! `strict_judge` / `contains_judge` 的用例：轨迹必须与结论**同源**，措辞也要钉住。
//!
//! 拆成独立文件是因为主体已经超过文件行数上限，而**测试文件不限行数**
//! （`CONVENTIONS.md` §2/§9），与 `dropdown.rs` + `dropdown/tests.rs` 同一做法。

use super::*;
use crate::ports::Rect;

fn tb(text: &str, x: i32, y: i32, confidence: f32) -> TextBox {
    TextBox { text: text.into(), bounds: Rect { x, y, width: 120, height: 28 }, confidence }
}

const MIN: f32 = 0.85;

/// 轨迹必须与结论**同源**：选中了谁，谁那一行就得是 `passed`，
/// 而且只有那一行是。这一条是整件事的地基（`CONVENTIONS.md` §1.3）。
#[test]
fn the_trail_marks_exactly_the_selected_candidate_as_passed() {
    let matcher = StrictContactMatcher::default();
    let candidates = vec![tb("张三丰", 10, 10, 0.99), tb("张三", 10, 100, 0.97)];
    let (result, trail) = strict_judge(&matcher, "张三", &candidates, MIN);
    let hit = result.expect("逐字相等的那一块应当被选中");
    assert_eq!(hit.text, "张三");
    assert_eq!(trail.candidates.len(), candidates.len(), "每块文字都该有一行，顺序一致");
    assert_eq!(
        trail.candidates.iter().filter(|v| v.passed).count(),
        1,
        "通过的那一行有且只有一行"
    );
    assert!(trail.candidates[1].passed);
    assert!(!trail.candidates[0].passed);
}

/// 低于阈值的候选必须**如实报出它自己的置信度与阈值**：
/// 2026-09-21 的现场就是分不清"这三个字没读到"还是"读到了但置信度不够"。
#[test]
fn a_low_confidence_candidate_says_its_own_number() {
    let matcher = StrictContactMatcher::default();
    let candidates = vec![tb("联系人", 10, 10, 0.61), tb("李小明", 10, 60, 0.99)];
    let (result, trail) = strict_judge(&matcher, "李小明", &candidates, MIN);
    assert!(result.is_ok());
    let low = &trail.candidates[0];
    assert!(!low.passed);
    assert!(low.reason.contains("0.61"), "理由里要有它自己的置信度：{}", low.reason);
    assert!(low.reason.contains("0.85"), "理由里要有阈值：{}", low.reason);
    assert_eq!(low.text, "联系人", "文字要原样带出来，它是排查时的关键线索");
}

/// 逐字相等但并列 ⇒ 两行都要说清"为什么没选你"，而不是默默只报一个总数。
#[test]
fn duplicates_explain_themselves_on_every_row() {
    let matcher = StrictContactMatcher::default();
    let candidates = vec![tb("张三", 10, 10, 0.99), tb("张三", 10, 200, 0.98)];
    let (result, trail) = strict_judge(&matcher, "张三", &candidates, MIN);
    assert!(matches!(result, Err(AutomationError::AmbiguousVision(_))));
    assert!(trail.candidates.iter().all(|v| !v.passed));
    assert!(trail.candidates.iter().all(|v| v.reason.contains("2 个")), "{:?}", trail.candidates);
}

/// 「两块太近」要写成两句话：命中那一块说"旁边有谁"，挡住它的那一块说"我挡住了谁"。
#[test]
fn a_too_close_pair_is_explained_from_both_sides() {
    let matcher = StrictContactMatcher::default();
    let candidates = vec![tb("张三丰", 10, 14, 0.99), tb("张三", 10, 10, 0.99)];
    let (result, trail) = strict_judge(&matcher, "张三", &candidates, MIN);
    assert!(matches!(result, Err(AutomationError::AmbiguousVision(_))));
    assert!(trail.candidates[1].reason.contains("张三丰"), "{:?}", trail.candidates[1]);
    assert!(trail.candidates[0].reason.contains("张三"), "{:?}", trail.candidates[0]);
}

/// 放宽层的轨迹要报出**它退化了**（`relaxed`），并且说清为什么取这一行：
/// 「取最短的」最容易被当成"随便挑一个"。用的是实测过的那条数据
/// （OCR 把头像红点并进了姓名行，读出 `0 李四`）。
#[test]
fn the_relaxed_trail_says_which_row_won_and_why() {
    let matcher = ContainsNameMatcher::default();
    let candidates = vec![tb("0 李四", 10, 10, 0.99), tb("李四：可以了，登录进去了", 10, 120, 0.99)];
    let (result, trail) = contains_judge(&matcher, "李四", &candidates, MIN);
    assert_eq!(result.expect("放宽层应当认出带噪声的姓名行").text, "0 李四");
    assert!(trail.relaxed, "退化到包含匹配这件事必须写在轨迹上");
    assert!(trail.candidates[0].passed);
    assert!(trail.candidates[0].reason.contains("最短"), "{}", trail.candidates[0].reason);
    assert!(trail.candidates[1].reason.contains("长"), "{}", trail.candidates[1].reason);
}

/// 精确匹配命中时**不算**放宽：轨迹上要如实写 `relaxed = false`，
/// 否则重放会按另一条判据重跑，结论自然对不上。
#[test]
fn an_exact_hit_is_not_reported_as_relaxed() {
    let matcher = ContainsNameMatcher::default();
    let candidates = vec![tb("李四", 10, 10, 0.99)];
    let (result, trail) = contains_judge(&matcher, "李四", &candidates, MIN);
    assert!(result.is_ok());
    assert!(!trail.relaxed);
    assert!(trail.candidates[0].reason.contains("逐字"), "{}", trail.candidates[0].reason);
}

/// 放宽层报「没有逐字相等的」时，轨迹仍旧来自严格判据——
/// 因为**判据确实是严格的那一条**，队列里那些"不逐字相等"的理由正是它有价值的输出。
#[test]
fn the_trail_of_a_relaxed_run_still_lists_every_block() {
    let matcher = ContainsNameMatcher::default();
    let candidates = vec![tb("张三", 10, 10, 0.99), tb("李四：可以了", 10, 120, 0.99)];
    let (result, trail) = contains_judge(&matcher, "李四", &candidates, MIN);
    assert_eq!(result.expect("包含匹配应当认出第二块").text, "李四：可以了");
    assert_eq!(trail.candidates.len(), 2);
    assert!(trail.candidates[0].reason.contains("没有"), "{}", trail.candidates[0].reason);
}

/// 目标名为空：`contains("")` 恒为真，这一步**必须先拦住**，
/// 而且候选照旧逐块记下来（那时候画面已经截了，正是要看的东西）。
#[test]
fn an_empty_name_is_blocked_before_anything_else() {
    let matcher = ContainsNameMatcher::default();
    let candidates = vec![tb("随便谁", 10, 10, 0.99)];
    let (result, trail) = contains_judge(&matcher, "   ", &candidates, MIN);
    assert!(matches!(result, Err(AutomationError::NeedsHumanReview(_))));
    assert_eq!(trail.candidates.len(), 1);
    assert!(trail.candidates[0].reason.contains("目标名称为空"), "{}", trail.candidates[0].reason);
}