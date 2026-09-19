//! **搜索式工作流**（`Workflow::SearchContact`）：点搜索框 → 逐字输入 →
//! 从联想下拉里挑人 → 资料页 → 点「发消息」→ 输入正文。
//!
//! 单独成文件是因为它与列表扫描式**看的是完全不同的界面**：
//! 前者看顶部的联想下拉与右侧资料页，后者看左侧的会话列表。
//! 两条路的失败现象一模一样（都是「找不到联系人」），而处置方向完全相反——
//! 所以它们既要在编排上分开（`Workflow`），也要在代码上分开，
//! 免得改一条路时误伤另一条。
//!
//! 这里的东西全都只被这条路用到：`normalize_text` / `resolve_extra` /
//! `profile_scroll_anchor` 也一样（列表式不读任何"新增区域"，
//! 它只用有出厂默认值的 `regions` 那四项）。

use super::*;

/// 把一行识别结果压成"只留可见字符"的形式，用于**包含**判断。
///
/// ## 为什么必须归一化
///
/// `Windows.Media.Ocr` 经常在字与字之间塞进空格（实测把「外部测试联系人」
/// 读成「外部 测试 联系人」），全角与半角也会混。不归一化的话，
/// "下拉里那一行是否包含输入的关键词"就会因为一个空格而判否——
/// 而现象是"搜出来的联系人一个都没匹配上"，看起来像搜索没生效。
///
/// 只去掉空白与控制字符，**不做同音字/近形字替换**：那是另一回事，
/// 而且会引入"看起来像就算匹配"这种本项目明确拒绝的判据。
pub(super) fn normalize_text(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace() && !c.is_control()).collect()
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
        match self.pick_contact_from_dropdown(&boxes, &keyword) {
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
    /// 在下拉列表里挑出目标联系人。
    ///
    /// ## 判据（操作者 2026-09-19 指定）
    ///
    /// 下拉列表是**分组**的：先一行「联系人」标题，标题下面才是匹配到的人；
    /// 再往下可能还有「聊天记录」「群聊」之类的分组。所以不能整块找
    /// "文字包含输入词"——那样会把聊天记录里提到这个名字的消息也算进来，
    /// 点下去就点进了别的地方。
    ///
    /// 规则：取「联系人」标题**下方**、文字**包含**输入词的块。
    ///
    /// - 找不到「联系人」标题 ⇒ 转人工（这一屏根本没有联系人分组）
    /// - 标题下方一个都没匹配上 ⇒ 转人工
    /// - 匹配上多个 ⇒ 转人工（同名，或者备注里也带着这个名字），列出候选
    ///
    /// ## 为什么是"包含"而不是逐字相等
    ///
    /// 下拉里的行常带着附加信息（备注名、微信号），逐字相等会一个都匹配不上。
    /// 这是操作者明确要求的判据，代价是**可能**选中一行只是"备注里含这个名字"
    /// 的记录——所以下面那两道"多个就转人工"的闸门不能省。
    pub(super) fn pick_contact_from_dropdown(
        &self,
        boxes: &[TextBox],
        keyword: &str,
    ) -> Result<TextBox, AutomationError> {
        let group = self.cfg().search_contact_group_label.trim().to_string();
        let group_needle = normalize_text(&group);
        let needle = normalize_text(keyword);
        let min_confidence = self.cfg().min_confidence;

        // ── 先找出「联系人」这个分组标题 ────────────────────────────
        //
        // **先要逐字相等，找不到才退到「包含」**。
        //
        // 只用「包含」是不够的，而且会错得很隐蔽：下拉里的聊天记录行常常长成
        // 「和 张三 的聊天」，它**也**含「联系人」这三个字。于是一旦按
        // "最上面那个含「联系人」的块"去认标题，就可能认到一行聊天记录上，
        // 而它下面根本没有联系人分组——后面整段判据全部错位，
        // 症状是"点到了不相干的一行"，看不出是标题认错了。
        //
        // 逐字相等先命中就轮不到聊天记录行来冒充；退到「包含」是留给
        // OCR 把标题多读出一个字符的情况（那是"包含"要兜的原始场景）。
        // 两级都取**最上面**那一个：同一个词可能因为排版被拆成两块。
        let title = boxes
            .iter()
            .filter(|b| b.confidence >= min_confidence)
            .filter(|b| normalize_text(&b.text) == group_needle)
            .min_by_key(|b| b.bounds.y)
            .or_else(|| {
                boxes
                    .iter()
                    .filter(|b| b.confidence >= min_confidence)
                    .filter(|b| normalize_text(&b.text).contains(&group_needle))
                    .min_by_key(|b| b.bounds.y)
            });
        let Some(title) = title else {
            return Err(AutomationError::NeedsHumanReview(format!(
                "搜索下拉列表里没有找到「{group}」这一组，读到 {} 块文字。\
                 常见原因：关键词没匹配到任何联系人（下拉里只有聊天记录或群聊），\
                 或者「搜索下拉列表」区域标定偏了。",
                boxes.len()
            )));
        };

        // 标题**下方**的块，按 y 取——OCR 给出的块顺序不保证是按位置排的。
        let below_title = title.bounds.y + title.bounds.height;
        let hits: Vec<&TextBox> = boxes
            .iter()
            .filter(|b| b.confidence >= min_confidence)
            .filter(|b| b.bounds.y >= below_title)
            .filter(|b| normalize_text(&b.text).contains(&needle))
            .collect();

        match hits.len() {
            1 => Ok(hits[0].clone()),
            0 => {
                let seen: Vec<TextBox> = boxes
                    .iter()
                    .filter(|b| b.bounds.y > title.bounds.y)
                    .cloned()
                    .collect();
                Err(AutomationError::NeedsHumanReview(format!(
                    "「{group}」分组下方没有匹配「{keyword}」的那一行。\
                     这一组下方的文字是：{}",
                    describe_candidates(&seen)
                )))
            }
            count => {
                let names: Vec<&str> = hits.iter().map(|b| b.text.trim()).collect();
                Err(AutomationError::AmbiguousVision(format!(
                    "「{group}」分组下方有 {count} 行都匹配「{keyword}」：{}。\
                     拒绝猜测该点哪一个——同名或备注里含这个名字时，点错人会把消息发错对象。",
                    names.join(" / ")
                )))
            }
        }
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
    /// 核验资料页：右侧面板上显示的人是不是目标。
    ///
    /// 这一步真正的价值在于**重名**。下拉里点的那一行只是"文字包含了关键词"，
    /// 而资料页上是这个人自己的名字——两处对上了，才说明点对了人。
    /// 判据仍然只问匹配器（[`ContactMatcher::accepts`]）。
    pub(super) fn verify_profile(&mut self) -> Result<(), AutomationError> {
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
            return Ok(());
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

        match found {
            Ok(b) => Err(AutomationError::AmbiguousVision(format!(
                "资料页上读到的是「{}」，与目标「{}」不一致——可能点错了人",
                b.text.trim(),
                self.task.external_contact_name.trim()
            ))),
            // 匹配器自己报的错（找不到 / 多个候选）原样透传：
            // 它比这里能编出来的任何一句话都更清楚。
            Err(err) => Err(err),
        }
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
