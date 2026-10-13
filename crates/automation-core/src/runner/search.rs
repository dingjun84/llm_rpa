//! **旧·搜索式工作流**（未编入 `runner/mod.rs`，YOLO 工作流已替换，见 `yolo_contacts.rs`）。
//!
//! 2026-10 清理：入口与只被入口引用的方法已删除（`search_contact_by_keyword`、
//! `pick_contact_from_dropdown`、`click_dropdown_row`、`open_chat_from_dropdown`、
//! `open_chat_from_profile`、`scroll_profile_to_bottom`、`profile_scroll_anchor`、
//! `ChatEntry`）。`resolve_extra` 只剩这里一份（原与 `message.rs` 重复定义）。保留的是仍有参考 / 复用价值的判据：
//! 资料页核验 [`Run::verify_profile`] 与搜索框诊断 `diagnose_search_field`。
//! 文件按操作者决定保留，不删。

use super::*;

use crate::dropdown::normalize_text;

/// [`Run::verify_profile`] 的结论。
///
/// 为什么"没看到目标"要单独成为一种结论：点完下拉那一行**不保证**落在资料页
/// （见 旧 `open_chat_from_dropdown`（已删））。资料页上确实是别人 ⇒ 点错了人，转人工；
/// 这一步压根没发生 ⇒ 该换另一条路继续。两者的**判据是同一份**，
/// 所以把结论交回调用方定夺，而不是在这里猜哪种算失败。
pub(super) enum ProfileReview {
    /// 资料页上就是他，可以点「发消息」了。
    Verified,
    /// 资料页上没看到目标。带着原因：万一后面那条路也走不通，
    /// 它才是"为什么停下来"的答案（见 旧 `open_chat_from_dropdown`（已删））。
    NotFound(AutomationError),
}

impl Run<'_> {
    /// 解析可选标定区域；`None` 如实转人工（不猜默认值）。
    pub(super) fn resolve_extra(
        &self,
        region: Option<crate::regions::RelativeRegion>,
        label: &str,
    ) -> Result<Rect, AutomationError> {
        let region = region.ok_or_else(|| {
            AutomationError::NeedsHumanReview(format!(
                "「{label}」还没有标定，无法继续。\
                 请到「界面标定」页把这一块框出来，保存配置后再跑。"
            ))
        })?;
        self.resolve(region, label)
    }


    /// 给「下拉里找不到人」的失败补一句**搜索框里到底是什么**。
    ///
    /// ## 为什么要补这一句
    ///
    /// 失败信息只说「「联系人」分组下方没有匹配的那一行」时，看的人会默认
    /// "确实没有这个人"。但实测踩过另一种情况：**输入根本没落进搜索框**——
    /// 那时下拉里是一片与关键词无关的内容（甚至是刚点开搜索框时的默认列表），
    /// 而过程证据里照样写着"已在搜索框逐字输入 N 个字符"。
    /// 两种情况处置方向完全相反：前者该换个词或换条工作流，后者要查输入为什么没进去。
    ///
    /// 把搜索框里的原文报出来，一眼就能分开。
    fn diagnose_search_field(
        &mut self,
        err: AutomationError,
        search: Rect,
        keyword: &str,
    ) -> AutomationError {
        // 只给「转人工」的失败补话。别的变体（平台错误、超时…）各有各的失败码，
        // 往它们的文案里塞一段识别结果会把"是什么错"这件事搅浑。
        let reason = match err {
            AutomationError::NeedsHumanReview(reason) => reason,
            other => return other,
        };
        let read = match self.capture_and_recognize(search, "搜索框内容核对") {
            Ok((_, boxes)) => describe_candidates(&boxes),
            // 核对本身失败**不能顶掉原来的原因**：原原因才是"为什么停下来"的答案，
            // 这里只是补充说明，如实说一句核对失败就够了。
            Err(cause) => format!("（核对失败：{cause}）"),
        };
        AutomationError::NeedsHumanReview(format!(
            "{reason}；另外核对了一次搜索框，读到：{read}。\
             若这里面没有「{keyword}」，说明关键词没有落进搜索框，\
             而不是「没有这个联系人」。"
        ))
    }
}
impl Run<'_> {
    /// 核验资料页：右侧面板上显示的人是不是目标。
    ///
    /// 这一步真正的价值在于**重名**。下拉里点的那一行只是"文字包含了关键词"，
    /// 而资料页上是这个人自己的名字——两处对上了，才说明点对了人。
    /// 判据仍然只问匹配器（[`ContactMatcher::accepts`]）。
    ///
    /// 返回 [`ProfileReview::NotFound`] 而不是直接报错，同样是因为这里
    /// 分不清"资料页上是别人"和"这一步压根没发生"——两种成因的**判据相同**，
    /// 而处置相反，所以交给 旧 `open_chat_from_dropdown`（已删） 定夺。
    /// 但"点击根本没生效"仍然是硬失败：那时画面一个像素都没动，
    /// 继续往下走等于在一个可能卡死的界面上打字。
    pub(super) fn verify_profile(&mut self) -> Result<ProfileReview, AutomationError> {
        self.advance(TaskState::VerifyingProfile, None)?;
        let profile = self.resolve_extra(self.cfg().contact_profile, "联系人资料区域")?;
        let (shot, boxes) = self.capture_and_recognize(profile, "资料页识别")?;
        self.evidence.push(format!("contact_profile#{}", shot.fingerprint));
        if self.cfg().log_ocr_candidates {
            self.evidence.push(format!(
                "  读到 {} 块：{}",
                boxes.len(),
                describe_candidates(&boxes)
            ));
        }

        let found = self.runner.ports.matcher.find_unique_exact_match(
            &self.task.external_contact_name,
            &boxes,
            self.cfg().min_confidence,
        );
        let accepted = found
            .as_ref()
            .map(|b| self.runner.ports.matcher.accepts(&self.task.external_contact_name, b))
            .unwrap_or(false);
        if accepted {
            return Ok(ProfileReview::Verified);
        }

        // 上一步点完下拉那一行之后，资料区一个像素都没变 ⇒ 那次点击很可能
        // 根本没落到界面上。此时报"资料页上没有这个人"会把人引向
        // "是不是点错了人 / 名字识别错了"，而真正的原因在别处。
        // 这和 `verify_chat_header` 里那条分支是同一件事，判据也取自同一个字段。
        if self.last_click_reacted == Some(false) {
            return Err(AutomationError::NeedsHumanReview(format!(
                "点搜索下拉里那一行之后，资料区画面没有任何变化——这次点击可能没有生效。\
                 请检查客户端是否卡死、是否被其它窗口遮挡，然后重试。\
                 （目标「{}」）",
                self.task.external_contact_name.trim()
            )));
        }

        Ok(ProfileReview::NotFound(match found {
            Ok(b) => AutomationError::AmbiguousVision(format!(
                "资料页上读到的是「{}」，与目标「{}」不一致——可能点错了人",
                b.text.trim(),
                self.task.external_contact_name.trim()
            )),
            // 匹配器自己报的错（找不到 / 多个候选）原样透传：
            // 它比这里能编出来的任何一句话都更清楚。
            Err(err) => err,
        }))
    }
}
