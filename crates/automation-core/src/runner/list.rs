//! **旧·列表扫描式**（未编入 `runner/mod.rs`，YOLO 工作流已替换，见 `yolo_list_find.rs`）。
//!
//! 2026-10 清理：`locate_contact`、`sweep_contact_list`、`open_chat_from_list` 已删除
//! （无任何引用）。保留滚动判据 `scroll_anchor` / `scroll_to_top` /
//! `view_moves_when_scrolling` 供参考。文件按操作者决定保留，不删。

use super::*;

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
