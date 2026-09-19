//! **列表扫描式工作流**（`Workflow::ScrollListContact`）：在左侧会话列表里
//! 滚动扫描、用 OCR 一行行认名字，找到之后点它直接进聊天页。
//!
//! 这是本项目最早实现的那条路。它与搜索式互补：列表扫描不依赖搜索框能不能
//! 触发联想，而搜索式不依赖列表里滚得到人。
//!
//! 滚动相关的判据（落点、回顶、等停稳、滚不动了没有）都在这里，
//! 与资料页那一套**刻意不复用**：两者滚的是不同面板、用的是不同落点，
//! 合成一个带方向参数的函数会让"用错了落点"变成一次静默的滚错区域——
//! 而现象只是「找不到入口」。

use super::*;

impl Run<'_> {
    /// 在联系人候选区里找到目标。
    ///
    /// ## 为什么是"回顶 + 多轮扫描"
    ///
    /// 列表按**最近有消息**排序：任何一条新消息都会把对应的会话顶到最上面。
    /// 如果从上一次遗留的滚动位置一路向下找，目标可能**已经在身后**了——
    /// 这正是"下拉查找会错过"的成因。所以每一轮都先回到列表顶部（一个确定的起点），
    /// 一轮扫不到就回顶再扫一轮，把"扫描期间被新消息顶上去"这件事兜住。
    ///
    /// ## 三重设限（每一层都不能省）
    ///
    /// - 单轮的滚动次数上限（`max_scroll_attempts`）；
    /// - 完整扫描的轮数上限（`max_search_sweeps`）；
    /// - 取消令牌每一步都检查。
    ///
    /// **歧义不滚动重试**：滚动改变的是"现在能看到谁"，不改变"这个名字是否唯一"。
    /// 出现多个逐字匹配时滚下去只会把同一个歧义重复 N 次，所以立刻失败。
    pub(super) fn locate_contact(&mut self, panel: Rect) -> Result<TextBox, AutomationError> {
        let sweeps = self.cfg().max_search_sweeps.max(1);
        // 把**鼠标会被放到哪儿**写进日志。
        //
        // 为什么值得单独记一行：`scroll_anchor` 是相对比例，而候选区一改宽度，
        // 同一个比例就落到完全不同的像素上（实测：区域从 0.28 改到 0.46，
        // 落点从 x=178 跳到 x=273）。操作者看到"鼠标没动到列表上"时，
        // 日志里原来只有"读到几块"，**根本无从对照**——只能靠猜。
        // 有了这一行，"鼠标到底去了哪里"就变成可核对的事实。
        //
        // 而且这一行现在是**可信的落点**，不只是"打算放到哪"：平台层的 `scroll`
        // 在真正发滚轮之前会用 `GetCursorPos` 核对光标确实停在这里（对不上就报错
        // 退出，不会继续滚）。所以任务能走到下一行，就说明光标真的到过这个坐标。
        let anchor = self.scroll_anchor(panel)?;
        self.evidence.push(format!(
            "滚动落点 : 屏幕 ({}, {})   候选区 : 屏幕 ({}, {}) {}x{}",
            anchor.x, anchor.y, panel.x, panel.y, panel.width, panel.height
        ));
        for sweep in 1..=sweeps {
            // 第二轮开始之前先回顶。第一轮刻意**不**回顶：从当前位置往下扫是最省的，
            // 而"目标在当前位置上方"这种情况由第二轮（从顶部重扫）兜住——
            // 两轮合起来的覆盖范围就是整个列表，不需要额外那一次回顶。
            //
            // 所以 `max_search_sweeps` 配成 1 就等于退回"只往下扫一遍"的老行为。
            if sweep > 1 {
                self.scroll_to_top(panel)?;
            }
            if let Some(matched) = self.sweep_contact_list(panel)? {
                return Ok(matched);
            }
            self.evidence.push(format!("contact_sweep#{sweep}"));
        }
        Err(AutomationError::NeedsHumanReview(format!(
            "已把联系人列表从头到尾完整扫描 {sweeps} 轮，仍未找到「{}」。\
             请确认该联系人确实存在，且列表没有被搜索框或筛选条件限制。",
            self.task.external_contact_name.trim()
        )))
    }
}
impl Run<'_> {
    /// 把配置里的滚动落点换算到屏幕坐标。
    ///
    /// 配置不合法就**报错**，而不是夹到边界上凑合：夹边界会让「比例写错了」
    /// 表现为「滚了半天没反应」，那是本项目最难查的一类现象。
    pub(super) fn scroll_anchor(&self, panel: Rect) -> Result<Point, AutomationError> {
        let anchor = self.cfg().scroll_anchor;
        anchor.validate().map_err(|err| {
            AutomationError::NeedsHumanReview(format!("滚动落点配置不合法：{err}"))
        })?;
        Ok(anchor.resolve(panel))
    }
}
impl Run<'_> {
    /// 把联系人列表滚回顶部，为一次确定性的扫描定好起点。
    ///
    /// 判据是"向上滚之后画面不再变化"，而**不是**某个固定的滚动次数：
    /// 列表长度随联系人数量变化，写死次数换台机器、换个账号就不对了。
    ///
    /// 这里只截屏算指纹、**不做 OCR**——判断"画面动没动"不需要读字，
    /// 而 OCR 在真实模式下是一次几百毫秒的独立进程调用。
    pub(super) fn scroll_to_top(&mut self, panel: Rect) -> Result<(), AutomationError> {
        let mut previous: Option<String> = None;
        // 上滚次数比下扫上限**多一次**，不是随手加的：
        // 最后一次是"空滚"——只有再滚一下、看到画面不再变化，才能确认已经到顶。
        // 恰好用满下扫上限的那种情况（列表正好那么长），少这一次就会误报失败。
        let limit = self.cfg().max_scroll_attempts.max(1) + 1;
        // 落点只算一次：配置错了要在**滚动之前**就失败，而不是滚了几轮才发现。
        let anchor = self.scroll_anchor(panel)?;
        for _ in 0..=limit {
            self.check_cancel()?;
            let fingerprint = self.capture_frame(panel, "联系人列表")?.fingerprint;
            if previous.as_deref() == Some(fingerprint.as_str()) {
                // 向上滚了一格画面没动 ⇒ 已经在顶部（或者这个列表根本滚不动）。
                return Ok(());
            }
            previous = Some(fingerprint);

            let expected_window = self.ensure_calibrated()?;
            self.runner.ports.platform.scroll(
                anchor,
                -self.cfg().scroll_notches_per_step,
                expected_window,
            )?;
            // 等停稳再进入下一轮：截到缓动动画的中间帧，指纹会跟"真的到顶了"
            // 长得一样，于是把"还在动"误判成"到顶了"，回顶这一步就白做了。
            self.wait_for_settle(panel)?;
            self.step_started = Instant::now();
        }
        Err(AutomationError::NeedsHumanReview(format!(
            "向上滚动 {limit} 次仍未回到联系人列表顶部，拒绝继续猜测扫描起点。"
        )))
    }
}
impl Run<'_> {
    /// 从当前位置向下扫描一遍联系人列表。
    ///
    /// 返回 `Ok(None)` 表示"这一遍扫到底了、没找到"——**不是失败**，
    /// 调用方可以回顶再扫一轮。
    pub(super) fn sweep_contact_list(&mut self, panel: Rect) -> Result<Option<TextBox>, AutomationError> {
        let mut previous_fingerprint: Option<String> = None;
        let mut scrolled: u32 = 0;
        let step = self.cfg().scroll_notches_per_step;
        // 同 `scroll_to_top`：落点先算一次，配置错了立刻失败。
        let anchor = self.scroll_anchor(panel)?;

        loop {
            self.check_cancel()?;
            let (shot, candidates) = self.capture_and_recognize(panel, "联系人识别")?;
            self.evidence.push(format!("contact_panel#{}", shot.fingerprint));
            // 把这一帧**读成了什么**记下来。只记指纹的话，「找不到联系人」永远
            // 分不清两种原因：① OCR 读错了（要调放大倍数或把窗口调大）；
            // ② 名字根本不在这屏（要换列表或重做区域标定）。
            // 这两件事的处置完全相反，而原来在现场只能靠猜。
            let log_candidates = self.cfg().log_ocr_candidates;
            if log_candidates {
                self.evidence.push(format!(
                    "  读到 {} 块：{}",
                    candidates.len(),
                    describe_candidates(&candidates)
                ));
            }

            match self.runner.ports.matcher.find_unique_exact_match(
                &self.task.external_contact_name,
                &candidates,
                self.cfg().min_confidence,
            ) {
                Ok(matched) => return Ok(Some(matched)),
                // 歧义：滚动解决不了，立刻停下。
                Err(err @ AutomationError::AmbiguousVision(_)) => return Err(err),
                Err(_) => {
                    if scrolled >= self.cfg().max_scroll_attempts {
                        return Ok(None);
                    }
                    if previous_fingerprint.as_deref() == Some(shot.fingerprint.as_str()) {
                        // 向下滚了一格，画面没动。两种可能，必须区分开：
                        //   1) 已经到底了 —— 正常的收工条件；
                        //   2) 客户端卡死了 —— 必须立刻转人工，绝不能继续点。
                        // 唯一的区分办法是**往反方向滚一下**，看画面动不动。
                        if self.view_moves_when_scrolling(panel, -step)? {
                            return Ok(None);
                        }
                        self.ensure_not_frozen(
                            "向下滚动与向上滚动都无法改变联系人列表画面",
                        )?;
                        // 系统说它还活着 ⇒ 这个列表本来就没得滚（只有一屏），
                        // 按"这一遍扫到底了"处理，不要误报卡死。
                        return Ok(None);
                    }
                    previous_fingerprint = Some(shot.fingerprint.clone());

                    let expected_window = self.ensure_calibrated()?;
                    self.runner
                        .ports
                        .platform
                        .scroll(anchor, step, expected_window)?;
                    scrolled += 1;
                    // 等画面停稳再进入下一轮截图。不等的话，下一轮截到的是
                    // 缓动动画的中间帧：文字糊、行错位，OCR 读出来是乱的；
                    // 而且"上一帧和这一帧一样 ⇒ 到底了"这个判断也会被带偏。
                    self.wait_for_settle(panel)?;

                    // 滚动会让列表内容整体移动，上一轮的文字框位置全部作废，
                    // 下一轮必须重新截图识别——绝不能拿滚动前的结果去点击。
                    //
                    // 也正因为如此，每轮都把单步计时重置：滚动 N 次是 N 次独立尝试，
                    // 不重置的话滚动 20 次必然撑爆一次 `step_timeout`，
                    // 变成"搜到一半被判超时"。
                    self.step_started = Instant::now();
                }
            }
        }
    }
}
impl Run<'_> {
    /// 滚一下，看画面有没有变化。只回答"这个方向的滚动还有没有效果"，不做 OCR。
    pub(super) fn view_moves_when_scrolling(
        &mut self,
        panel: Rect,
        notches: i32,
    ) -> Result<bool, AutomationError> {
        let anchor = self.scroll_anchor(panel)?;
        let before = self.capture_frame(panel, "联系人列表")?.fingerprint;
        let expected_window = self.ensure_calibrated()?;
        self.runner.ports.platform.scroll(anchor, notches, expected_window)?;
        // **必须先等停稳再截"滚动之后"这一帧**：截早了，重绘还没发生，
        // `before` 与 `after` 相同 ⇒ 这一句会回答"滚不动"，而它的结论正是
        // "到底了没有"。判错的代价是整段列表被跳过，且完全不报错。
        self.wait_for_settle(panel)?;
        self.step_started = Instant::now();
        let after = self.capture_frame(panel, "联系人列表")?.fingerprint;
        Ok(before != after)
    }
}
impl Run<'_> {
    /// 列表扫描式：点一下候选人，他就被选中并打开聊天页。
    ///
    /// 先记下点击**之前**对话区的画面：一次生效的点击应该让它变化。
    /// 这条指纹用来区分"点错了人"和"点击根本没生效"——两者的处置完全不同。
    pub(super) fn open_chat_from_list(&mut self, matched: &TextBox) -> Result<(), AutomationError> {
        let panel = self.resolve(self.cfg().contact_panel, "联系人候选区")?;
        let body = self.resolve(self.cfg().chat_body, "聊天正文区")?;
        let body_before_click = self.capture_frame(body, "点击前聊天区")?.fingerprint;

        // 点击之前先确认客户端还活着：往一个卡死的窗口里点击，什么都查不出来，
        // 只会让后面每一步都建立在"没生效的动作"上。
        self.ensure_not_frozen("已取消点击联系人")?;
        let expected_window = self.ensure_calibrated()?;
        let contact_screen_rect = matched.bounds.to_screen(Point { x: panel.x, y: panel.y });
        // 记下"点了哪儿"。这是"准备给联系人发消息"的第一步，也是整个流程里
        // 唯一一个真的把光标放到某个联系人身上的动作——出问题时必须能核对坐标。
        let target = contact_screen_rect.center();
        self.evidence.push(format!(
            "点击联系人 : 屏幕 ({}, {})   命中文字「{}」框 ({}, {}) {}x{}   置信度 {:.2}",
            target.x,
            target.y,
            matched.text.trim(),
            contact_screen_rect.x,
            contact_screen_rect.y,
            contact_screen_rect.width,
            contact_screen_rect.height,
            matched.confidence
        ));
        self.runner.ports.platform.guarded_click(target, expected_window)?;
        self.check_deadline("点击联系人")?;
        self.last_click_reacted =
            Some(self.capture_frame(body, "点击后聊天区")?.fingerprint != body_before_click);
        Ok(())
    }
}
