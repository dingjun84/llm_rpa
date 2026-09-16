//! 联系人匹配策略。
//!
//! 依据 `docs/architecture.md` §6.4 / §6.6 与 `docs/windows-mvp-interface.md`：
//! 姓名默认逐字精确匹配，别名规则必须由企业管理员显式配置；
//! 低置信度、多个精确匹配或文字框过于接近时必须拒绝，绝不"猜"联系人。

use std::collections::BTreeMap;

use crate::ports::{AutomationError, ContactMatcher, Rect, TextBox};

/// 默认拒绝阈值：两个候选框中心距离小于该像素数时视为无法区分。
pub const DEFAULT_MIN_SEPARATION_PX: i32 = 8;

#[derive(Debug, Clone)]
pub struct StrictContactMatcher {
    /// 管理员显式配置的别名表：目标名 → 允许的等价写法。默认空表，即不做任何放宽。
    pub aliases: BTreeMap<String, Vec<String>>,
    pub min_separation_px: i32,
}

impl Default for StrictContactMatcher {
    fn default() -> Self {
        Self { aliases: BTreeMap::new(), min_separation_px: DEFAULT_MIN_SEPARATION_PX }
    }
}

impl StrictContactMatcher {
    /// 仅去掉首尾空白；不做大小写折叠、不做相似度、不做子串匹配。
    fn canonical(text: &str) -> &str {
        text.trim()
    }

    fn accepted_terms<'a>(&'a self, expected_name: &'a str) -> Vec<&'a str> {
        let mut terms = vec![expected_name];
        if let Some(configured) = self.aliases.get(expected_name) {
            terms.extend(configured.iter().map(String::as_str));
        }
        terms
    }

    fn centers_too_close(a: Rect, b: Rect, min_separation_px: i32) -> bool {
        let ca = a.center();
        let cb = b.center();
        let dx = (ca.x - cb.x).abs();
        let dy = (ca.y - cb.y).abs();
        let threshold = min_separation_px.max(0);
        dx < threshold && dy < threshold
    }
}

impl ContactMatcher for StrictContactMatcher {
    fn find_unique_exact_match(
        &self,
        expected_name: &str,
        candidates: &[TextBox],
        min_confidence: f32,
    ) -> Result<TextBox, AutomationError> {
        if Self::canonical(expected_name).is_empty() {
            return Err(AutomationError::NeedsHumanReview("目标联系人名称为空".into()));
        }

        let accepted: Vec<&TextBox> =
            candidates.iter().filter(|c| c.confidence >= min_confidence).collect();

        if accepted.is_empty() {
            return Err(AutomationError::NeedsHumanReview(format!(
                "该区域没有达到最低置信度 {min_confidence} 的文字候选"
            )));
        }

        let terms = self.accepted_terms(expected_name);
        let matches: Vec<&TextBox> = accepted
            .iter()
            .copied()
            .filter(|c| terms.iter().any(|t| Self::canonical(&c.text) == Self::canonical(t)))
            .collect();

        match matches.len() {
            0 => Err(AutomationError::NeedsHumanReview(format!(
                "未找到与「{}」逐字匹配的联系人",
                expected_name.trim()
            ))),
            1 => {
                let matched = matches[0];
                if matched.bounds.is_degenerate() {
                    return Err(AutomationError::AmbiguousVision(
                        "匹配到的联系人文字框尺寸无效".into(),
                    ));
                }
                let too_close = accepted.iter().any(|c| {
                    !std::ptr::eq(*c, matched)
                        && Self::centers_too_close(c.bounds, matched.bounds, self.min_separation_px)
                });
                if too_close {
                    return Err(AutomationError::AmbiguousVision(format!(
                        "「{}」附近存在过于接近的候选文字框，无法确定点击目标",
                        matched.text.trim()
                    )));
                }
                Ok(matched.clone())
            }
            n => Err(AutomationError::AmbiguousVision(format!(
                "存在 {n} 个与「{}」逐字匹配的联系人，拒绝猜测",
                expected_name.trim()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tb(text: &str, x: i32, y: i32, confidence: f32) -> TextBox {
        TextBox {
            text: text.into(),
            bounds: Rect { x, y, width: 120, height: 28 },
            confidence,
        }
    }

    const MIN: f32 = 0.85;

    #[test]
    fn returns_the_only_exact_match() {
        let matcher = StrictContactMatcher::default();
        let candidates = vec![tb("张三", 10, 10, 0.99), tb("李四", 10, 100, 0.97)];
        let found = matcher.find_unique_exact_match("张三", &candidates, MIN).unwrap();
        assert_eq!(found.text, "张三");
    }

    #[test]
    fn rejects_when_nothing_matches_exactly() {
        let matcher = StrictContactMatcher::default();
        let candidates = vec![tb("张三丰", 10, 10, 0.99), tb("小张三", 10, 100, 0.99)];
        let err = matcher.find_unique_exact_match("张三", &candidates, MIN).unwrap_err();
        assert!(matches!(err, AutomationError::NeedsHumanReview(_)));
    }

    #[test]
    fn rejects_duplicate_exact_matches() {
        let matcher = StrictContactMatcher::default();
        let candidates = vec![tb("张三", 10, 10, 0.99), tb("张三", 10, 200, 0.98)];
        let err = matcher.find_unique_exact_match("张三", &candidates, MIN).unwrap_err();
        assert!(matches!(err, AutomationError::AmbiguousVision(_)));
    }

    #[test]
    fn rejects_low_confidence_match() {
        let matcher = StrictContactMatcher::default();
        let candidates = vec![tb("张三", 10, 10, 0.40)];
        let err = matcher.find_unique_exact_match("张三", &candidates, MIN).unwrap_err();
        assert!(matches!(err, AutomationError::NeedsHumanReview(_)));
    }

    #[test]
    fn ignores_low_confidence_candidates_when_a_good_one_exists() {
        let matcher = StrictContactMatcher::default();
        let candidates = vec![tb("张三", 10, 10, 0.30), tb("张三", 10, 300, 0.99)];
        let found = matcher.find_unique_exact_match("张三", &candidates, MIN).unwrap();
        assert_eq!(found.bounds.y, 300);
    }

    #[test]
    fn rejects_candidates_that_are_too_close_to_tell_apart() {
        let matcher = StrictContactMatcher::default();
        let candidates = vec![tb("张三", 10, 10, 0.99), tb("张三丰", 10, 14, 0.99)];
        let err = matcher.find_unique_exact_match("张三", &candidates, MIN).unwrap_err();
        assert!(matches!(err, AutomationError::AmbiguousVision(_)));
    }

    #[test]
    fn aliases_only_apply_when_explicitly_configured() {
        let mut aliases = BTreeMap::new();
        aliases.insert("张三".to_string(), vec!["三哥".to_string()]);
        let matcher = StrictContactMatcher { aliases, ..Default::default() };
        let candidates = vec![tb("三哥", 10, 10, 0.99)];
        assert_eq!(
            matcher.find_unique_exact_match("张三", &candidates, MIN).unwrap().text,
            "三哥"
        );
    }

    #[test]
    fn aliases_do_not_enable_fuzzy_matching() {
        let matcher = StrictContactMatcher::default();
        let candidates = vec![tb("三哥", 10, 10, 0.99)];
        assert!(matcher.find_unique_exact_match("张三", &candidates, MIN).is_err());
    }

    #[test]
    fn trims_surrounding_whitespace_only() {
        let matcher = StrictContactMatcher::default();
        let candidates = vec![tb(" 张三 ", 10, 10, 0.99)];
        assert_eq!(matcher.find_unique_exact_match("张三", &candidates, MIN).unwrap().text, " 张三 ");
    }
}
