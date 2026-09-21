//! 联系人匹配策略。
//!
//! 依据 `docs/architecture.md` §6.4 / §6.6 与 `docs/windows-mvp-interface.md`：
//! 姓名默认逐字精确匹配，别名规则必须由企业管理员显式配置；
//! 低置信度、多个精确匹配或文字框过于接近时必须拒绝，绝不"猜"联系人。
//!
//! 本模块同时提供 [`ContainsNameMatcher`]——一个**临时的**放宽层
//! （包含匹配即可），用于先把链路跑通。它**违反**上面那条架构约定，
//! 存在理由、已知风险与后续优化方向都写在它自己的文档注释和 `docs/todo.md` 里。
//!
//! ⚠️ **判定体本身不在这里**，在子模块 [`trail`]：它必须**同时**产出结论与轨迹
//! （每个候选过没过、为什么），而轨迹只能由判据自己给出（`CONVENTIONS.md` §1.3）。
//! 这里剩下的两件事是：策略的文档与配置（别名表、最小间距），以及一个薄薄的转发。

mod trail;

use std::collections::BTreeMap;

use crate::diagnostics::MatchTrail;
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

    /// 目标名不能为空。
    ///
    /// 为什么必须**先**拦这一条：`contains("")` 恒为真。放宽匹配（见
    /// [`ContainsNameMatcher`]）如果拿到一个空名字，会把"第一个候选"当成命中，
    /// 于是变成"随便点一个联系人"——比匹配失败危险得多。
    fn ensure_name_present(expected_name: &str) -> Result<(), AutomationError> {
        if Self::canonical(expected_name).is_empty() {
            return Err(AutomationError::NeedsHumanReview("目标联系人名称为空".into()));
        }
        Ok(())
    }
}

impl ContactMatcher for StrictContactMatcher {
    fn find_unique_exact_match(
        &self,
        expected_name: &str,
        candidates: &[TextBox],
        min_confidence: f32,
    ) -> Result<TextBox, AutomationError> {
        trail::strict_judge(self, expected_name, candidates, min_confidence).0
    }

    fn find_unique_exact_match_with_trail(
        &self,
        expected_name: &str,
        candidates: &[TextBox],
        min_confidence: f32,
    ) -> (Result<TextBox, AutomationError>, MatchTrail) {
        trail::strict_judge(self, expected_name, candidates, min_confidence)
    }

    fn accepts(&self, expected_name: &str, candidate: &TextBox) -> bool {
        // 必须走**和 `find_unique_exact_match` 同一套**候选词表（原名 + 别名），
        // 否则「匹配器认别名、复检只认原名」——别名一配上，任务就会在复检处
        // 被挡回来，而文案看起来像是识别不准。
        let text = Self::canonical(&candidate.text);
        self.accepted_terms(expected_name).iter().any(|term| text == Self::canonical(term))
    }
}

/// ⚠️ **临时放宽层**：精确匹配不到时，退化为「候选文本**包含**目标名」。
///
/// # 为什么存在
///
/// 2026-09-17 操作者明确要求：「先不用精确匹配，只要识别到包含的字符就行，
/// 先测试通过」。目的是**先把整条链路跑通**——不让 OCR 的个别错字卡住
/// 「点击联系人 → 标题核验 → 填入输入框」这一段的验证。
///
/// # 它违反了架构约定（必须知情）
///
/// `docs/architecture.md` §6.4/§6.6 要求姓名**逐字精确匹配**。这里刻意放宽，
/// 因此**不要在有真实发送需求时打开它**：它把「找不到人」变成「可能找错人」，
/// 而后者更危险。已知的误判风险与后续优化方向记在 `docs/todo.md`。
///
/// # 行为（按顺序）
///
/// 1. **先走正路**：调 [`StrictContactMatcher`]。命中就原样返回，
///    与严格模式**完全一致**——OCR 读干净时，这一层不改变任何行为。
/// 2. 精确匹配报**歧义**（多个精确匹配 / 候选太近）时**不降级**：
///    放宽只会让它更歧义，绝不会更确定。
/// 3. 只有「一个都没匹配上」才退化到包含匹配。
/// 4. 包含匹配命中多个时取**最短**的那个。依据（实测）：联系人姓名行只有姓名本身，
///    而群消息预览行是「发送者：消息内容」，一定更长。实测「李四」同时出现在
///    姓名行 `李四` 和群「测试群一」的预览行 `李四：可以了，登录进去了` 里，
///    两条都在候选区中。
/// 5. 最短的仍有并列（长度相同）⇒ 仍然按歧义拒绝，**不猜**。
///
/// 注意第 4 条只是"在已知数据上成立"的启发式，不是保证——
/// 这也正是它必须被当成临时措施的原因。
#[derive(Debug, Clone, Default)]
pub struct ContainsNameMatcher {
    strict: StrictContactMatcher,
}

impl ContainsNameMatcher {
    pub fn new(strict: StrictContactMatcher) -> Self {
        Self { strict }
    }

    /// 内层严格匹配器，供调用方按需调整别名表 / 最小间距。
    pub fn strict(&self) -> &StrictContactMatcher {
        &self.strict
    }
}

impl ContactMatcher for ContainsNameMatcher {
    fn find_unique_exact_match(
        &self,
        expected_name: &str,
        candidates: &[TextBox],
        min_confidence: f32,
    ) -> Result<TextBox, AutomationError> {
        // 空名必须**先**查（`contains("")` 恒为真，会把第一个候选当成命中），
        // 放宽与否的两条路都在 `trail` 里，判据与轨迹同源。
        trail::contains_judge(self, expected_name, candidates, min_confidence).0
    }

    fn find_unique_exact_match_with_trail(
        &self,
        expected_name: &str,
        candidates: &[TextBox],
        min_confidence: f32,
    ) -> (Result<TextBox, AutomationError>, MatchTrail) {
        trail::contains_judge(self, expected_name, candidates, min_confidence)
    }

    fn accepts(&self, expected_name: &str, candidate: &TextBox) -> bool {
        // 空名的 `contains("")` 恒为真 —— 必须和 `find_unique_exact_match` 一样先挡住，
        // 否则「目标名为空」会变成「任何候选都算命中」。
        if StrictContactMatcher::ensure_name_present(expected_name).is_err() {
            return false;
        }
        self.strict.accepts(expected_name, candidate)
            || candidate.text.contains(expected_name.trim())
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

    // ── 放宽匹配（`ContainsNameMatcher`）────────────────────────────────
    //
    // 这一组用例守的是"临时放宽层"的**边界**：放宽到哪里、在哪里必须停住。
    // 它存在的理由是先把链路跑通（见类型文档与 `docs/todo.md`），
    // 所以更要钉清楚它**没有**放宽掉哪些安全检查。

    fn lenient() -> ContainsNameMatcher {
        ContainsNameMatcher::default()
    }

    /// 真实数据复现：OCR 把头像红点并进了姓名行，读出 `0 李四`。
    /// 严格匹配必然失败，放宽层要能认出来。
    #[test]
    fn contains_matching_finds_the_name_inside_a_noisy_block() {
        let strict = StrictContactMatcher::default();
        let candidates = vec![tb("0 李四", 10, 10, 0.99)];
        assert!(
            strict.find_unique_exact_match("李四", &candidates, MIN).is_err(),
            "严格模式本来就不该匹配上带噪声的文本"
        );
        let found = lenient().find_unique_exact_match("李四", &candidates, MIN).unwrap();
        assert_eq!(found.text, "0 李四");
    }

    /// 命中多个时取**最短**的那个：姓名行只有姓名，群消息预览行是
    /// 「发送者：消息内容」，一定更长。用实测数据钉住。
    #[test]
    fn contains_matching_prefers_the_shorter_candidate() {
        let candidates = vec![
            tb("李四", 10, 10, 0.99),
            tb("李四：可以了，登录进去了", 10, 120, 0.99),
        ];
        let found = lenient().find_unique_exact_match("李四", &candidates, MIN).unwrap();
        assert_eq!(found.text, "李四", "应当选中较短的姓名行，而不是群消息预览行");
        assert_eq!(found.bounds.y, 10, "落点应当还在姓名那一行");
    }

    /// 最短的仍有并列 ⇒ 按歧义拒绝。**放宽的是"怎么算命中"，
    /// 不是"命中之后可以不看歧义"**。
    #[test]
    fn contains_matching_still_rejects_a_length_tie() {
        let candidates = vec![tb("李四甲", 10, 10, 0.99), tb("李四乙", 10, 200, 0.99)];
        let err = lenient().find_unique_exact_match("李四", &candidates, MIN).unwrap_err();
        assert!(matches!(err, AutomationError::AmbiguousVision(_)), "实际：{err:?}");
    }

    /// 精确匹配报歧义时**不降级**：放宽只会让它更歧义，绝不会更确定。
    #[test]
    fn contains_matching_does_not_degrade_on_an_exact_ambiguity() {
        let candidates = vec![tb("李四", 10, 10, 0.99), tb("李四", 10, 300, 0.99)];
        let err = lenient().find_unique_exact_match("李四", &candidates, MIN).unwrap_err();
        assert!(
            matches!(err, AutomationError::AmbiguousVision(_)),
            "两个逐字相同的联系人必须照旧拒绝，实际：{err:?}"
        );
    }

    /// 空目标名**必须先被拦掉**：`contains("")` 恒为真，
    /// 放任它走下去会变成"随便点一个联系人"。
    #[test]
    fn contains_matching_never_runs_with_an_empty_name() {
        let candidates = vec![tb("随便谁", 10, 10, 0.99)];
        for name in ["", "   "] {
            let err = lenient().find_unique_exact_match(name, &candidates, MIN).unwrap_err();
            assert!(
                matches!(err, AutomationError::NeedsHumanReview(_)),
                "空名必须直接拒绝，实际：{err:?}"
            );
        }
    }

    /// 名字**压根不在这屏**时，放宽层也必须失败——不能因为"放宽了"
    /// 就退化成"挑一个最像的"。
    #[test]
    fn contains_matching_still_fails_when_the_name_is_absent() {
        let candidates = vec![tb("张三", 10, 10, 0.99), tb("李四", 10, 200, 0.99)];
        let err = lenient().find_unique_exact_match("赵六", &candidates, MIN).unwrap_err();
        assert!(matches!(err, AutomationError::NeedsHumanReview(_)), "实际：{err:?}");
    }

    /// 低置信度候选在两种模式下都要被滤掉，且滤光之后报同一类错。
    #[test]
    fn contains_matching_filters_by_confidence_too() {
        let candidates = vec![tb("李四", 10, 10, 0.40)];
        let err = lenient().find_unique_exact_match("李四", &candidates, MIN).unwrap_err();
        assert!(matches!(err, AutomationError::NeedsHumanReview(_)), "实际：{err:?}");
    }

    /// 别名规则照旧生效（放宽层**复用**内层的严格匹配，不是另起一套）。
    #[test]
    fn contains_matching_keeps_aliases_working() {
        let mut aliases = BTreeMap::new();
        aliases.insert("张三".to_string(), vec!["三哥".to_string()]);
        let matcher = ContainsNameMatcher::new(StrictContactMatcher {
            aliases,
            ..Default::default()
        });
        let candidates = vec![tb("三哥", 10, 10, 0.99)];
        assert_eq!(matcher.find_unique_exact_match("张三", &candidates, MIN).unwrap().text, "三哥");
    }
}
