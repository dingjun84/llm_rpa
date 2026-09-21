//! 把匹配器给出的**轨迹**装配成一条决策记录。
//!
//! ## 为什么单独一个文件
//!
//! 这段活是"翻译"：把端口侧的 [`MatchTrail`] 变成可落盘的 [`Decision`]。
//! 它既不判东西（判据在匹配器里），也不走流程（那是 `mod.rs`），
//! 放进 `runner/mod.rs` 只会让那个已经超标的文件再添一份职责。

use super::*;

/// 由姓名匹配的结论与轨迹拼出一条决策记录。
///
/// **一句判据都不在这里**：通过/淘汰的措辞来自匹配器自己给的 [`Verdict`]，
/// 这里只补上"在问什么"和"结论是什么"。
///
/// 公开是为了**离线重放**（`tools/replay`）能拼出同一条记录——见 `runner/mod.rs`
/// 里那段说明。
pub fn name_match_decision(
    trail: &MatchTrail,
    expected_name: &str,
    min_confidence: f32,
    result: &Result<TextBox, AutomationError>,
) -> Decision {
    let outcome = match result {
        Ok(found) => format!(
            "选中「{}」（置信度 {:.2}）",
            found.text.trim(),
            found.confidence
        ),
        Err(err) => format!("转人工：{err}"),
    };
    Decision {
        step: String::new(),
        question: format!("这一屏的文字块里，哪一块是目标联系人「{}」？", expected_name.trim()),
        rule: format!("{}；最低置信度 {min_confidence:.2}", trail.rule),
        outcome,
        // 「成没成」= 匹配器给的那个 `Result`，不另判一遍（§1.3）。
        passed: result.is_ok(),
        min_confidence,
        replay: Some(ReplayInput::NameMatch {
            expected_name: expected_name.to_string(),
            relaxed: trail.relaxed,
        }),
        candidates: trail.candidates.clone(),
    }
}