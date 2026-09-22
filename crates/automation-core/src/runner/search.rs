//! **搜索式工作流**（`Workflow::SearchContact`）：点搜索框 → 逐字输入 →
//! 从联想下拉里挑人 → 资料页 → 点「发消息」→ 输入正文。
//!
//! 单独成文件是因为它与列表扫描式**看的是完全不同的界面**：
//! 前者看顶部的联想下拉与右侧资料页，后者看左侧的会话列表。
//! 两条路的失败现象一模一样（都是「找不到联系人」），而处置方向完全相反——
//! 所以它们既要在编排上分开（`Workflow`），也要在代码上分开，
//! 免得改一条路时误伤另一条。
//!
//! 这里的东西全都只被这条路用到：`resolve_extra` / `profile_scroll_anchor` 也一样
//! （列表式不读任何"新增区域"，它只用有出厂默认值的 `regions` 那四项）。
//!
//! ⚠️ **判据本身不在这里**：`在下拉里挑人`那条判据搬到了 [`crate::dropdown`]，
//! 因为离线重放要拿同一份判据重跑（`docs/todo.md` T29）。这里只剩"怎么走到那一步"。

use super::*;

use crate::dropdown::{judge_dropdown, normalize_text};

/// [`Run::verify_profile`] 的结论。
///
/// 为什么"没看到目标"要单独成为一种结论：点完下拉那一行**不保证**落在资料页
/// （见 [`Run::open_chat_from_dropdown`]）。资料页上确实是别人 ⇒ 点错了人，转人工；
/// 这一步压根没发生 ⇒ 该换另一条路继续。两者的**判据是同一份**，
/// 所以把结论交回调用方定夺，而不是在这里猜哪种算失败。
pub(super) enum ProfileReview {
    /// 资料页上就是他，可以点「发消息」了。
    Verified,
    /// 资料页上没看到目标。带着原因：万一后面那条路也走不通，
    /// 它才是"为什么停下来"的答案（见 [`Run::open_chat_from_dropdown`]）。
    NotFound(AutomationError),
}

impl Run<'_> {
    /// 取一个**新增区域**（标定页里那些还没有出厂默认值的项）并换算成屏幕坐标。
    ///
    /// 与 [`Self::resolve`] 的唯一区别是这些区域可能是 `None`。**没标就是没有**：
    /// 给一个猜出来的默认值，症状会是"任务照常跑完，只是点到了别的地方"——
    /// 那是本项目最难查的一类现象。所以这里如实报错，并说清去哪儿标。
    pub(super) fn resolve_extra(
        &self,
        region: Option<RelativeRegion>,
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
}
impl Run<'_> {
    /// 把资料页的滚动落点换算到屏幕坐标。非法比例**报错**，不夹边界。
    pub(super) fn profile_scroll_anchor(&self, profile: Rect) -> Result<Point, AutomationError> {
        let anchor = self.cfg().profile_scroll_anchor;
        anchor.validate().map_err(|err| {
            AutomationError::NeedsHumanReview(format!("资料页滚动落点配置不合法：{err}"))
        })?;
        Ok(anchor.resolve(profile))
    }
}
impl Run<'_> {
    /// 用顶部搜索框查找联系人：点搜索框 → 逐字输入 → 在下拉列表里挑出他并点击。
    ///
    /// ## 为什么必须**逐字**输入
    ///
    /// 搜索框是联想式的：它按输入事件逐次刷新下拉列表。一次性粘贴整段文字时，
    /// 下拉要么不弹、要么只按第一次输入匹配——于是"在下拉里找联系人"这一步
    /// 永远找不到人，而现象看起来像是搜索功能没生效，不会让人想到是输入方式的问题。
    ///
    /// ## 为什么点搜索框之前要确认客户端没卡死
    ///
    /// 光标原来可能停在会话列表上。不先点一下搜索框，那些字符会敲进列表的
    /// 快捷键处理里——最坏的情况是触发一个谁也没想到的动作。
    /// 点击走的是受守卫的 `guarded_click`：它先核对前台窗口与标定。
    pub(super) fn search_contact_by_keyword(&mut self) -> Result<TextBox, AutomationError> {
        let keyword = self.task.external_contact_name.trim().to_string();
        if keyword.is_empty() {
            return Err(AutomationError::NeedsHumanReview("目标联系人为空".into()));
        }

        // ── 1. 点搜索框，让它进入输入状态 ───────────────────────────
        let search = self.resolve_extra(self.cfg().main_search, "主界面搜索框")?;
        self.ensure_not_frozen("已取消点击搜索框")?;
        let expected_window = self.ensure_calibrated()?;
        self.evidence.push(format!(
            "点击搜索框 : 屏幕 ({}, {})   搜索框区 : 屏幕 ({}, {}) {}x{}",
            search.center().x,
            search.center().y,
            search.x,
            search.y,
            search.width,
            search.height
        ));
        self.runner.ports.platform.guarded_click(search.center(), expected_window)?;
        self.check_deadline("点击搜索框")?;

        // ── 1.5 先清空搜索框里原有的内容 ───────────────────────────
        //
        // 客户端的搜索框**保留上一次的输入**。不清空的话，这一次的关键词会接在
        // 上一次的后面（「张三」→「张三李四」），而它是联想式的——会拿这个混合词
        // 去查，结果是一片与目标无关的内容。现象是"搜出来的东西不对"，
        // 不会让人想到是**上一次的词还在**。
        //
        // 顺序不能省：点击 → 清空 → 输入。清空本身会先等焦点落定
        // （见 `DesktopPlatform::clear_text_field` 的约定）。
        let expected_window = self.ensure_calibrated()?;
        self.ensure_not_frozen("已取消清空搜索框")?;
        self.runner.ports.platform.clear_text_field(expected_window)?;
        self.check_deadline("清空搜索框")?;
        self.evidence.push("已清空搜索框原有内容（全选 → 删除）".into());

        // ── 2. 逐字输入关键词 ───────────────────────────────────────
        let expected_window = self.ensure_calibrated()?;
        self.ensure_not_frozen("已取消输入搜索词")?;
        self.runner.ports.platform.type_text(&keyword, expected_window)?;
        self.check_deadline("输入搜索词")?;
        self.evidence.push(format!("已在搜索框逐字输入 {} 个字符", keyword.chars().count()));

        // ── 3. 等下拉弹出来，再识别 ─────────────────────────────────
        //
        // 必须等：下拉是**动画**弹出来的，紧接着截图会截到中间帧，
        // 文字还没画完，识别结果会是空的——而那看起来像"搜索没有结果"，
        // 排查会一路往关键词、往客户端上找。
        let dropdown = self.resolve_extra(self.cfg().search_dropdown, "搜索下拉列表")?;
        self.wait_for_settle(dropdown)?;
        let (shot, boxes) = self.capture_and_recognize(dropdown, "搜索下拉识别")?;
        self.evidence.push(format!("search_dropdown#{}", shot.fingerprint));
        if self.cfg().log_ocr_candidates {
            self.evidence.push(format!(
                "  读到 {} 块：{}",
                boxes.len(),
                describe_candidates(&boxes)
            ));
        }

        // ── 4. 在「联系人」分组里挑出他 ─────────────────────────────
        //
        // 失败时顺手核对一次搜索框里到底是什么。**只在失败路径上做**：
        // 顺利时它是一次多余的截图 + 一次多余的识别，而顺利时没有任何疑问要回答。
        match self.pick_contact_from_dropdown(&boxes, &keyword, "搜索下拉识别") {
            Ok(hit) => Ok(hit),
            Err(err) => Err(self.diagnose_search_field(err, search, &keyword)),
        }
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
    /// 在下拉列表里挑出目标联系人，并把**每个候选为什么**交给诊断记录器。
    ///
    /// 判据本身在 [`crate::dropdown::judge_dropdown`]：**同一个函数**同时产出结论与轨迹
    /// （见那边的模块文档）。这里只把配置喂进去、把结论用起来、把轨迹交出去——
    /// 一句判据都不再写，免得"界面上看到的理由"与"实际用的规则"各说各话
    /// （`CONVENTIONS.md` §1.3）。
    ///
    /// `step` 是这一步在任务日志里的名字（与上面那次 `capture_and_recognize` 用的同一个），
    /// 决策记录靠它对上那一帧画面。
    pub(super) fn pick_contact_from_dropdown(
        &self,
        boxes: &[TextBox],
        keyword: &str,
        step: &str,
    ) -> Result<TextBox, AutomationError> {
        let judgement = judge_dropdown(
            boxes,
            keyword,
            &self.cfg().search_contact_group_label,
            self.cfg().min_confidence,
        );
        self.report_decision(step, judgement.decision);
        judgement.result
    }
}

impl Run<'_> {
    /// 搜索式：点一下下拉列表里那一行，进入这个人的**资料页**。
    ///
    /// ## 为什么单独成一个步骤
    ///
    /// 它和列表扫描式那一次点击**落点不同**：列表里点一下直接进聊天页，
    /// 而下拉里点一下只到资料页，还要再从资料页点一次「发消息」才进得去。
    /// 合成一个"点一下候选人"的函数，就得在里面按工作流分叉——
    /// 那和现在分成两个函数是一样的，只是把分叉藏得更深。
    ///
    /// ## 为什么记下点击前后的画面
    ///
    /// 点完之后落在资料页上，而"资料页上是不是这个人"由 [`Self::verify_profile`]
    /// 判断。但"那次点击根本没生效"（窗口被挡、坐标落在空白上、客户端卡死）
    /// 会表现成"资料页上没看到目标本人"——两种原因处置完全不同。
    /// 这里记下这一帧，就是为了让 `verify_profile` 能把它们分开说。
    pub(super) fn click_dropdown_row(&mut self, matched: &TextBox) -> Result<(), AutomationError> {
        let dropdown = self.resolve_extra(self.cfg().search_dropdown, "搜索下拉列表")?;
        let profile = self.resolve_extra(self.cfg().contact_profile, "联系人资料区域")?;
        let before = self.capture_frame(profile, "点击下拉行之前")?.fingerprint;

        self.ensure_not_frozen("已取消点击搜索下拉里的联系人")?;
        let expected_window = self.ensure_calibrated()?;
        // 下拉里的文字框坐标是**相对下拉区**的，要加上区域原点才是屏幕坐标。
        let screen = matched.bounds.to_screen(Point { x: dropdown.x, y: dropdown.y });
        let target = screen.center();
        self.evidence.push(format!(
            "点击搜索下拉行 : 屏幕 ({}, {})   命中文字「{}」框 ({}, {}) {}x{}   置信度 {:.2}",
            target.x,
            target.y,
            matched.text.trim(),
            screen.x,
            screen.y,
            screen.width,
            screen.height,
            matched.confidence
        ));
        self.runner.ports.platform.guarded_click(target, expected_window)?;
        self.check_deadline("点击搜索下拉里的联系人")?;

        // 资料页是**换了一整块内容**，等它画完再比：截早了会截到过渡动画的
        // 中间帧，与"没生效"看起来一模一样。
        self.wait_for_settle(profile)?;
        self.last_click_reacted =
            Some(self.capture_frame(profile, "点击下拉行之后")?.fingerprint != before);
        Ok(())
    }
}
impl Run<'_> {
    /// 搜索式：点下拉里那一行，**把聊天打开**，并核验聊天页上的人就是目标。
    ///
    /// ## 为什么要分叉
    ///
    /// 这一点击的落点**不唯一**（实测）：常态落在**资料页**，还要再点一次
    /// 「发消息」；但目标**已经有会话**时，客户端直接打开那份聊天记录
    /// （历史对话），资料页那两步根本不会发生。只按第一条路走，第二种情况
    /// 会在核验资料页处失败，而文案（"资料页上没有这个人"）看起来像是
    /// "点错了人"——方向完全错了。
    ///
    /// ## 判据的顺序不能颠倒
    ///
    /// 先按**资料页**核验（它是常态，也是唯一能挡住重名的那一关），只有
    /// "资料页上没有他"时才去问**聊天页标题区**。反过来先看标题区不行：
    /// 资料页上那个名字也在右侧面板顶部，与本机标定的标题区只差 2 像素，
    /// 照它判断会把"还在资料页"认成"已经在聊天里"，接着那次输入就落在
    /// 没有输入框的界面上。
    ///
    /// 两条路最后都过同一道标题核验（[`Self::verify_chat_header`]）：
    /// "点对了人"只有它能定论，这里不另立一套"看起来像聊天页"的猜测。
    pub(super) fn open_chat_from_dropdown(
        &mut self,
        matched: &TextBox,
    ) -> Result<(), AutomationError> {
        self.click_dropdown_row(matched)?;
        let profile_reason = match self.verify_profile()? {
            ProfileReview::Verified => {
                self.open_chat_from_profile()?;
                self.advance(TaskState::VerifyingChatHeader, None)?;
                return self.verify_chat_header();
            }
            ProfileReview::NotFound(reason) => reason,
        };

        // 资料页上没有他 ⇒ 多半是客户端直接打开了已有的会话。是不是，
        // 交给人就在聊天页上的那份判据去说——不再多截一帧、不再多一套判断。
        self.advance(TaskState::VerifyingChatHeader, None)?;
        match self.verify_chat_header() {
            Ok(()) => {
                self.evidence.push(format!(
                    "资料页上没认到目标（{profile_reason}），但聊天页标题就是目标 ⇒ \
                     判定客户端直接打开了已有的会话，已跳过「资料页 / 点发消息」两步。"
                ));
                Ok(())
            }
            Err(header_reason) => {
                // 两条路都没认下来。报**资料页**那条原因：它回答的是"那一次点击
                // 把界面带到哪儿去了"，比标题核验的结果更贴近起点。
                // 标题核验的结果也不能丢——它是"不在资料页"这个判断的另一半依据。
                self.evidence.push(format!(
                    "（跳过资料页之后，聊天页标题也没认下来：{header_reason}）"
                ));
                Err(profile_reason)
            }
        }
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
    /// 而处置相反，所以交给 [`Self::open_chat_from_dropdown`] 定夺。
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
impl Run<'_> {
    /// 在资料页里找到"进入聊天"的入口并点击它。
    ///
    /// ## 为什么先滚到最下面
    ///
    /// 入口在资料页的**底部**。直接在当前视口里找，会在"资料很短、入口已经在
    /// 屏幕上"时碰巧成功，而在"资料长"时表现为"找不到入口"——同一个配置
    /// 在两个联系人身上表现不同，这种缺陷最难查。
    ///
    /// ## 为什么不用图标匹配
    ///
    /// 资料页上那个入口**有文字**（默认「发消息」），而图标没有。有文字的
    /// 地方就用 OCR：模板必须由人对着资料页再截一张图，而文字判据不用。
    /// 实测发现读不到时再补模板那条退路——在那之前不做投机性的抽象。
    pub(super) fn open_chat_from_profile(&mut self) -> Result<(), AutomationError> {
        self.advance(TaskState::OpeningChatFromProfile, None)?;
        let profile = self.resolve_extra(self.cfg().contact_profile, "联系人资料区域")?;
        let body = self.resolve(self.cfg().chat_body, "聊天正文区")?;
        let body_before = self.capture_frame(body, "打开聊天前")?.fingerprint;

        self.scroll_profile_to_bottom(profile)?;

        let entry_text = self.cfg().profile_chat_entry_text.trim().to_string();
        let (shot, boxes) = self.capture_and_recognize(profile, "资料页入口识别")?;
        self.evidence.push(format!("profile_entry#{}", shot.fingerprint));
        if self.cfg().log_ocr_candidates {
            self.evidence.push(format!(
                "  读到 {} 块：{}",
                boxes.len(),
                describe_candidates(&boxes)
            ));
        }

        let needle = normalize_text(&entry_text);
        // 取**最上面**那一块：同一个词可能在资料里出现不止一次，
        // 而入口是其中最先出现的那一个。
        let entry = boxes
            .iter()
            .filter(|b| b.confidence >= self.cfg().min_confidence)
            .filter(|b| normalize_text(&b.text).contains(&needle))
            .min_by_key(|b| b.bounds.y)
            .cloned()
            .ok_or_else(|| {
                AutomationError::NeedsHumanReview(format!(
                    "资料页里没有找到「{entry_text}」这个入口（已经滚到最下面）。\
                     请确认这一项文字与当前客户端对得上，或到「界面标定」页核对「联系人资料区域」。"
                ))
            })?;

        let screen = entry.bounds.to_screen(Point { x: profile.x, y: profile.y });
        let target = screen.center();
        self.evidence.push(format!(
            "点击「{entry_text}」入口 : 屏幕 ({}, {})   命中文字框 ({}, {}) {}x{}   置信度 {:.2}",
            target.x, target.y, screen.x, screen.y, screen.width, screen.height, entry.confidence
        ));

        self.ensure_not_frozen("已取消点击资料页入口")?;
        let expected_window = self.ensure_calibrated()?;
        self.runner.ports.platform.guarded_click(target, expected_window)?;
        self.check_deadline("点击资料页入口")?;

        // 等聊天页画出来再比：这一步的产物是"换了一整页"，
        // 截早了会截到过渡动画的中间帧，与"没生效"看起来一模一样。
        self.wait_for_settle(body)?;
        self.last_click_reacted =
            Some(self.capture_frame(body, "打开聊天后")?.fingerprint != body_before);
        Ok(())
    }
}
impl Run<'_> {
    /// 把资料页滚到最下面。
    ///
    /// 判据是"向下滚之后画面不再变化"，**不是**某个固定的滚动次数：
    /// 资料长短随人变化，写死次数换个人就不对了。
    ///
    /// 与 [`Self::scroll_to_top`] 是对称的，但刻意不复用同一个函数：那个滚的是
    /// 会话列表、用的是列表的落点；这个滚的是资料面板。合成一个带方向参数的函数，
    /// 会让"用错了落点"变成一次静默的滚错区域——而现象只是"找不到入口"。
    pub(super) fn scroll_profile_to_bottom(&mut self, profile: Rect) -> Result<(), AutomationError> {
        let mut previous: Option<String> = None;
        // 多滚一次是"空滚"：只有再滚一下、看到画面不再变化，才能确认已经到底。
        let limit = self.cfg().max_scroll_attempts.max(1) + 1;
        let anchor = self.profile_scroll_anchor(profile)?;
        self.evidence.push(format!(
            "资料页滚动落点 : 屏幕 ({}, {})   资料区 : 屏幕 ({}, {}) {}x{}",
            anchor.x, anchor.y, profile.x, profile.y, profile.width, profile.height
        ));
        for _ in 0..=limit {
            self.check_cancel()?;
            let fingerprint = self.capture_frame(profile, "联系人资料")?.fingerprint;
            if previous.as_deref() == Some(fingerprint.as_str()) {
                // 向下滚了一格画面没动 ⇒ 到底了（或者这个面板本来就滚不动）。
                return Ok(());
            }
            previous = Some(fingerprint);

            let expected_window = self.ensure_calibrated()?;
            self.runner.ports.platform.scroll(
                anchor,
                self.cfg().scroll_notches_per_step,
                expected_window,
            )?;
            // 等停稳再进入下一轮：截到缓动动画的中间帧，指纹会跟"真的到底了"
            // 长得一样，于是把"还在动"误判成"到底了"。
            self.wait_for_settle(profile)?;
            self.step_started = Instant::now();
        }
        Err(AutomationError::NeedsHumanReview(format!(
            "向下滚动 {limit} 次仍未让资料页画面稳定下来，拒绝继续猜测滚动位置。"
        )))
    }
}
