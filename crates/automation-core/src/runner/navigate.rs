//! **导航图标**：用模板匹配找到左侧那一列图标里的某一个，点它把视图切过去。
//!
//! 图标上**没有文字**，OCR 读不到，所以这一步只能靠模板匹配。
//! 它既是"查找之前先切视图"的一个可选步骤（`navigate_before_search`），
//! 也是「只做导航」那条工作流（`Workflow::NavigateOnly`）的全部内容。

use super::*;

/// 量一段代码花了多久，并把它记成一条证据行（见 [`Run::note_elapsed`]）。
///
/// 位置（`file:line`）取在**展开处**——`file!()`/`line!()` 对 `macro_rules!` 是
/// 透明的，所以证据里那条指的就是"被量的这段代码"，不是这个宏自己。
/// 与 `log_line!` 同一个道理。
macro_rules! timed {
    ($run:expr, $what:expr, $body:expr) => {{
        let started = Instant::now();
        let value = $body;
        ($run).note_elapsed($what, concat!(file!(), ":", line!()), started.elapsed());
        value
    }};
}

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
    /// 要点的是**哪一个**图标不在这里判断：装配期已经把那个名字底下的全部图
    /// 载进了 [`RunnerConfig::nav_icon_templates`]，这里只管拿它们去匹配。
    /// 名字（`nav_target_label`）只用来把失败信息写清楚。
    pub(super) fn navigate_to_view(&mut self) -> Result<(), AutomationError> {
        let (strip, min_score, panel, prior, what) = {
            let cfg = self.cfg();
            let strip = self.resolve(cfg.nav_strip, "导航图标搜索区")?;
            (
                strip,
                cfg.nav_icon_min_score,
                self.resolve(cfg.contact_panel, "联系人候选区")?,
                self.nav_prior(strip),
                cfg.nav_target_label.clone(),
            )
        };

        // 用**联系人候选区**当"视图变了没有"的参照物：切换成功的话，
        // 这一块的内容必然整体换掉。用它而不是整窗，是因为整窗里有闪烁的光标、
        // 未读红点之类会自己变的东西，"变了"就不再是"切换成功了"的证据。
        //
        // 每一段都记一条耗时证据（[`Run::note_elapsed`]）：单步超时只回答
        // "超了没有"，而"超在哪一段"只能靠这些行——它们必须在**超时之前**
        // 就写好，因为超时那一刻现场已经过去了。
        let before =
            timed!(self, "导航·截「切换视图前」", self.capture_frame(panel, "切换视图前")?)
                .fingerprint;

        let frame =
            timed!(self, "导航·截「导航图标搜索区」", self.capture_frame(strip, "导航图标搜索区")?);
        self.evidence.push(format!(
            "nav_strip#{}   搜索区 屏幕 ({}, {}) {}x{}",
            frame.fingerprint, strip.x, strip.y, strip.width, strip.height
        ));

        // 只借用 `self.runner`（它是一个共享引用），不碰 `self` 的可变部分——
        // 否则下面 `self.evidence.push` 会和这次调用打架。
        let runner = self.runner;
        let templates: &[IconTemplate] = &runner.config().nav_icon_templates;
        // 模板为空是**配置缺失**，不是"这个图标不在画面上"。
        //
        // 装配期已经拦过一道，这里再拦一道是因为核心层是个库：它的调用方
        // 不只有 `apps/desktop` 的装配期（测试、探针都直接构造 `RunnerConfig`）。
        // 空模板会让匹配器拿到一个空列表——那会退化成"找不到图标"，
        // 而真正的原因是**没给模板**，两者的处置方向完全不同。
        if templates.is_empty() {
            return Err(AutomationError::NeedsHumanReview(format!(
                "没有「{what}」图标的模板，无法定位它。\
                 到「图标库」页对着那个图标截一张图存下来，再回到「任务」页选它。"
            )));
        }
        // 图标匹配是这一步里唯一"算"的活（模板匹配要逐位置比一遍），
        // 也是实测里最慢的一段——单独计时，别让它藏在总数里。
        let found = timed!(
            self,
            "导航·图标匹配 icons.locate",
            runner
                .ports
                .icons
                .locate(&frame, &IconQuery::new(templates, min_score).with_prior(prior))?
        );

        // `frame` 在 macOS Retina 上可能是物理像素，而 `strip` 是屏幕逻辑点。
        // 命中框要先按帧尺寸相对搜索区比例，再加回搜索区原点。
        let scale_x = if strip.width > 0 {
            frame.width as f32 / strip.width as f32
        } else {
            1.0
        };
        let scale_y = if strip.height > 0 {
            frame.height as f32 / strip.height as f32
        } else {
            1.0
        };
        let hit_screen = Rect {
            x: strip.x + (found.bounds.x as f32 / scale_x).round() as i32,
            y: strip.y + (found.bounds.y as f32 / scale_y).round() as i32,
            width: (found.bounds.width as f32 / scale_x).round() as i32,
            height: (found.bounds.height as f32 / scale_y).round() as i32,
        };
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

        // 把命中框也交给诊断记录器：图标上没有文字，这一步唯一的可读结果就是
        // "它把哪个图标认成了目标"。复用**刚匹配过的那一帧**，不重截——
        // 重截一张会让画上的框与画面对不上。
        //
        // 这一次上报会顺带截一张整窗、渲染标注图并编码成 PNG 落盘。那是**观测**
        // 的开销，不是任务本身该花的，所以单独计时：它要是把单步预算吃掉一大半，
        // 该改的是诊断的落盘方式，而不是把单步上限调大。
        timed!(
            self,
            "导航·上报命中（整窗截图+标注渲染+落盘）",
            self.report_icon_hit(
                "导航图标命中",
                strip,
                &frame,
                &IconHit {
                    bounds: found.bounds,
                    score: found.score,
                    template: found.template_label.clone(),
                },
            )
        );

        // 守卫与点击分开计时：前者是只读校验，后者会真的落到界面上。
        let expected_window = timed!(self, "导航·点击前守卫（存活检查+移到命中点+标定复核）", {
            self.ensure_not_frozen("已取消切换视图")?;
            // 先滑到命中点（不点）：任务若在标定校验处失败，操作者仍能看见认到的位置。
            self.runner.ports.platform.move_pointer(target)?;
            self.ensure_calibrated()?
        });
        timed!(
            self,
            "导航·guarded_click 点击",
            self.runner.ports.platform.guarded_click(target, expected_window)?
        );
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
