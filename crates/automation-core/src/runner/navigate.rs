//! **导航图标**：用模板匹配找到左侧那一列图标里的某一个，点它把视图切过去。
//!
//! 图标上**没有文字**，OCR 读不到，所以这一步只能靠模板匹配。
//! 它既是"查找之前先切视图"的一个可选步骤（`navigate_before_search`），
//! 也是「只做导航」那条工作流（`Workflow::NavigateOnly`）的全部内容。

use super::*;

impl Run<'_> {
    /// 算出"图标大概应该在哪"，供 [`IconPrior`] 使用。
    ///
    /// 返回的是**图像坐标系**（相对搜索区左上角）的期望位置。
    /// 优先取 `nav_bar` 标定区的中心——那是操作者真的框出来的那一列图标；
    /// 没标就退回搜索区自己的中心（它更宽，但中心仍在同一列上）。
    ///
    /// ## 容差配成 0 就是**关掉**先验
    ///
    /// 这时返回 `None`，而不是"用一个默认容差把它打开"：
    /// 关掉是一个明确的配置意图，不该被代码猜回去。
    pub(super) fn nav_prior(&self, strip: Rect) -> Option<IconPrior> {
        let tolerance = self.cfg().icon_prior_score_tolerance;
        if tolerance <= 0.0 {
            return None;
        }
        let anchor = match self.cfg().nav_bar {
            Some(region) => region.resolve_within(self.window?).ok()?.center(),
            None => strip.center(),
        };
        Some(IconPrior {
            at: Point { x: anchor.x - strip.x, y: anchor.y - strip.y },
            score_tolerance: tolerance,
        })
    }
}
impl Run<'_> {
    /// 用模板匹配找到左侧导航图标，点它一下，把视图切到"能查到联系人"的那个页面。
    ///
    /// ## 这一步和"点击联系人"是同一类动作
    ///
    /// 它**不是只读的**：点下去之后界面会重绘。所以和点击联系人一样，
    /// 动作前要确认客户端还活着、前台窗口与标定一致；动作后要确认画面真的变了。
    ///
    /// ## 为什么"点完必须看到画面变化"
    ///
    /// 因为"点到了图标"和"点击生效了"是两件事。图标可能被别的窗口挡住、
    /// 客户端可能正好卡了一下、坐标可能因为某种原因落在图标边缘的空白上——
    /// 这些情况下 `guarded_click` 会正常返回（鼠标确实点下去了），
    /// 而视图**根本没切**。不校验的话，后面整条查找流程都作用在一个
    /// 没切换成功的界面上，失败原因会表现为"找不到联系人"——那是错的方向。
    pub(super) fn navigate_to_view(&mut self, nav_target: NavTarget) -> Result<(), AutomationError> {
        let (strip, min_score, panel, prior) = {
            let cfg = self.cfg();
            let strip = self.resolve(cfg.nav_strip, "导航图标搜索区")?;
            (
                strip,
                cfg.nav_icon_min_score,
                self.resolve(cfg.contact_panel, "联系人候选区")?,
                self.nav_prior(strip),
            )
        };
        let what = nav_target.describe();

        // 用**联系人候选区**当"视图变了没有"的参照物：切换成功的话，
        // 这一块的内容必然整体换掉。用它而不是整窗，是因为整窗里有闪烁的光标、
        // 未读红点之类会自己变的东西，"变了"就不再是"切换成功了"的证据。
        let before = self.capture_frame(panel, "切换视图前")?.fingerprint;

        let frame = self.capture_frame(strip, "导航图标搜索区")?;
        self.evidence.push(format!(
            "nav_strip#{}   搜索区 屏幕 ({}, {}) {}x{}",
            frame.fingerprint, strip.x, strip.y, strip.width, strip.height
        ));

        // 只借用 `self.runner`（它是一个共享引用），不碰 `self` 的可变部分——
        // 否则下面 `self.evidence.push` 会和这次调用打架。
        let runner = self.runner;
        let templates: &[IconTemplate] = match nav_target {
            NavTarget::Contact => &runner.config().nav_icon_templates,
            NavTarget::History => &runner.config().history_icon_templates,
        };
        // 模板为空是**配置缺失**，不是"这个图标不在画面上"。
        // 装配期已经拦过一道，这里再拦一道是因为 `NavigateOnly` 那条路
        // 可能在另一个目标上跑起来——两个目标各有各的模板，漏配哪一个都不会报错。
        if templates.is_empty() {
            return Err(AutomationError::NeedsHumanReview(format!(
                "没有配置「{what}」图标的模板，无法定位它。\
                 到「图标库」页对着那个图标截一张图存下来，再到配置里把它勾上。"
            )));
        }
        let found = runner.ports.icons.locate(
            &frame,
            &IconQuery::new(templates, min_score).with_prior(prior),
        )?;

        let hit_screen = found.bounds.to_screen(Point { x: strip.x, y: strip.y });
        let target = hit_screen.center();
        // 记下"点的是哪儿、分数多少"。图标匹配不像文字识别那样有天然的可读结果，
        // 这一行是事后唯一能回答"它到底认成了什么"的地方。
        // 位置先验有没有生效、生效在哪，必须写下来：它**会改变点击位置**，
        // 而"点错了一个图标"和"点对了但界面没切"在证据里看起来很像。
        let prior_note = match prior {
            Some(prior) => format!(
                "位置先验 ({}, {}) 容差 {:.3}",
                prior.at.x, prior.at.y, prior.score_tolerance
            ),
            None => "位置先验 关".to_string(),
        };
        self.evidence.push(format!(
            "导航图标 : 目标「{what}」模板「{}」分数 {:.3}   {prior_note}\n\
             命中框 屏幕 ({}, {}) {}x{}   点击 屏幕 ({}, {})",
            found.template_label,
            found.score,
            hit_screen.x,
            hit_screen.y,
            hit_screen.width,
            hit_screen.height,
            target.x,
            target.y
        ));

        self.ensure_not_frozen("已取消切换视图")?;
        let expected_window = self.ensure_calibrated()?;
        self.runner
            .ports
            .platform
            .guarded_click(target, expected_window)?;
        self.check_deadline("点击导航图标")?;

        // 视图切换是重绘，同样要等停稳再比指纹——否则会截到动画中间帧，
        // 与"切换前"偶然相同，于是把一次成功的切换误判成"没生效"。
        self.wait_for_settle(panel)?;
        let after = self.capture_frame(panel, "切换视图后")?.fingerprint;
        if after == before {
            // ── 为什么这里只记警告，**不**转人工 ──────────────────────────
            //
            // "点下去画面没变"有两种成因，而它们在画面上**无法区分**：
            //
            // 1. 界面本来就已经停在这个视图上（上一次运行点完就留在这里了）
            //    ⇒ 点击无效是**正确**行为，一切正常；
            // 2. 客户端卡死 / 图标被别的窗口挡住 / 匹配到了一个不响应点击的位置
            //    ⇒ 点击真的没生效。
            //
            // 如果在这里直接转人工，第 1 种情况就会变成"第二次跑必然失败"——
            // 一个正常操作被判成故障，而且报错文案（"点击可能没有生效"）
            // 会把人引向排查客户端，方向完全错了。
            //
            // 那为什么不等一等再判、或者重试一次？因为"点击没生效"不是**时机**问题，
            // 重试与等待都解决不了（见 `REFERENCE.md` §15 的同型教训）。
            //
            // 于是把判定交给**下一步**：`locate_contact` 是纯只读的，
            // 视图不对它就在候选区里找不到目标，照样转人工。判据落在能真正
            // 决断的地方，而不是在这里猜。这一行警告的作用是——真出问题时，
            // 日志里已经写明了"视图可能根本没切过去"，不必再从"找不到联系人"倒推。
            self.evidence.push(format!(
                "⚠️ 点击导航图标后联系人候选区画面未变化（模板「{}」分数 {:.3}，点击 屏幕 ({}, {})）。\
                 若界面本来就停在这个视图上，这属于正常；否则说明这次点击没有生效，\
                 后面若报「找不到联系人」，先从这里查。",
                found.template_label, found.score, target.x, target.y
            ));
        }
        Ok(())
    }
}
