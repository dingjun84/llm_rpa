//! **准备消息**：聚焦输入框 → 记下发送前基线 → 把正文逐字填进去。
//!
//! 这里也是"发还是不发"的分岔点，而它**只有一条判据**：配置里的「只填不发」
//! （`stop_before_send`）。勾了就停在 `Prepared`，没勾就往下走人工确认与发送——
//! 与走的是哪条工作流无关。
//!
//! ⚠️ 不要把这条判据再摊开成"某条工作流特殊"：曾经搜索式无条件停在这里
//! （`docs/todo.md` T16 的临时取舍），现在它和列表扫描式同路。
//! 两条路**各写一套**"发不发"的判断，迟早会出现「界面上勾了什么、实际按哪条算」
//! 说不清，而这恰好是最不能说不清的一件事。

use super::*;

impl Run<'_> {
    /// 聚焦输入框、记下发送前基线，然后把正文填进去。
    ///
    /// ## 「发还是不发」由谁保证
    ///
    /// - **勾了「只填不发」**：正文入框后停在 [`TaskState::Prepared`] 就地结束。
    ///   这条分支必须在人工确认**之前**，而且不能复用下面的发送路径——
    ///   它存在的全部意义就是"绝不发送"，所以既不申请发送台账，
    ///   也不写消息摘要，审计里不该留下任何"像发过了"的痕迹。
    /// - **演练模式**：端口整组都是替身（`platform-mock`），`send` 只在本进程里
    ///   记一笔，碰不到任何真实窗口。所以"不会真的发出去"**由替身端口保证**，
    ///   不靠在这里提前停——否则演练模式下流程会缺掉后半截，
    ///   而人拿它当"流程验证"的时候并不知道少了什么。
    /// - **真实模式**：必然先过人工确认（`confirm_send` 会阻塞等操作者），
    ///   确认之后才 claim 发送台账、写消息摘要、进入 [`TaskState::Sending`]。
    pub(super) fn prepare_message(&mut self) -> Result<(), AutomationError> {
        // 选中联系人之后，焦点仍在会话列表（甚至搜索框）上，**不在消息输入框**里。
        // 不先点进输入框的话，后面那次输入会落到错误的位置——最坏情况是打进
        // 搜索框，把搜索结果本身改掉。所以这里必须先做一次受守卫的点击。
        let expected_window = self.ensure_calibrated()?;
        let composer = self.resolve(self.cfg().composer, "消息输入框区")?;
        // 同样记下坐标：这一步点错地方，后面那次输入就会落到别的控件里
        // （最坏是落到搜索框，把搜索结果本身改掉），而现象只是"字没进去"。
        self.evidence.push(format!(
            "聚焦输入框 : 屏幕 ({}, {})   输入框区 : 屏幕 ({}, {}) {}x{}",
            composer.center().x,
            composer.center().y,
            composer.x,
            composer.y,
            composer.width,
            composer.height
        ));
        self.runner.ports.platform.guarded_click(composer.center(), expected_window)?;
        self.check_deadline("聚焦消息输入框")?;

        let body = self.resolve(self.cfg().chat_body, "聊天正文区")?;
        let (before_shot, _) = self.capture_and_recognize(body, "发送前聊天区识别")?;
        self.baseline_fingerprint = Some(before_shot.fingerprint.clone());
        self.evidence.push(format!("chat_before#{}", before_shot.fingerprint));

        // ── 「只填不发」：正文入框后就地结束 ────────────────────────
        if self.cfg().stop_before_send {
            let expected_window = self.ensure_calibrated()?;
            self.ensure_not_frozen("已取消填入消息正文")?;
            // 逐字输入而不是粘贴：搜索框必须逐字敲才能触发联想，
            // 消息正文没有这个限制，但两条路共用同一个方法，
            // 就不存在"某一条输入路径从没被验证过"。
            self.runner.ports.platform.type_text(&self.task.text, expected_window)?;
            self.check_deadline("填入消息正文")?;
            self.advance(
                TaskState::Prepared,
                Some(format!(
                    "已把 {} 个字符逐字填入输入框，未发送",
                    self.task.text.chars().count()
                )),
            )?;
            return Ok(());
        }

        // ── 人工确认 ────────────────────────────────────────────────
        self.advance(TaskState::AwaitingHumanConfirmation, None)?;
        self.runner
            .ports
            .confirmation
            .confirm_send(self.task, self.cfg().confirmation_ttl)?;
        self.confirmation_at = Some(SystemTime::now());

        // ── 发送 ────────────────────────────────────────────────────
        self.check_cancel()?;
        self.runner.ledger.claim(self.task.id)?;
        // 摘要先于状态转换写入，确保 Sending 的审计记录已带上摘要。
        self.message_digest = Some(MessageDigest::of(&self.task.text));
        self.advance(TaskState::Sending, None)?;
        let expected_window = self.ensure_calibrated()?;
        // 发送前最后一次确认客户端还活着。往一个卡死的窗口里输入 + 回车，
        // 结果是"看起来发出去了，其实什么都没发生"——比失败更糟。
        self.ensure_not_frozen("已取消发送")?;
        self.runner
            .ports
            .platform
            .paste_text(&self.task.text, expected_window)?;
        self.runner
            .ports
            .platform
            .send_message_shortcut(expected_window)?;
        self.check_deadline("发送消息")?;

        // ── 核验送达 ────────────────────────────────────────────────
        self.advance(TaskState::VerifyingDelivery, None)?;
        let (after_shot, after_boxes) = self.capture_and_recognize(body, "送达核验识别")?;
        self.evidence.push(format!("chat_after#{}", after_shot.fingerprint));
        if Some(&after_shot.fingerprint) == self.baseline_fingerprint.as_ref() {
            // 画面没变有两种成因：消息真的没出现，或者客户端已经卡死。
            // 系统判定读得出来就带上这句；读不出来（Err）就不加——
            // 这只是诊断提示，不该因为诊断本身失败而改变结论。
            let frozen_hint = match self.runner.ports.platform.is_responsive() {
                Ok(false) => "（且系统判定客户端未响应，疑似卡死）",
                _ => "",
            };
            return Err(AutomationError::NeedsHumanReview(format!(
                "发送后聊天区截图未发生变化{frozen_hint}，无法确认消息已出现"
            )));
        }
        let wanted = self.task.text.trim();
        if wanted.is_empty() {
            return Err(AutomationError::NeedsHumanReview("消息正文为空".into()));
        }
        let appeared = after_boxes.iter().any(|b| {
            b.confidence >= self.cfg().min_confidence && b.text.contains(wanted)
        });
        if !appeared {
            return Err(AutomationError::NeedsHumanReview(
                "聊天区未识别到本条消息，拒绝判定为已送达".into(),
            ));
        }

        self.advance(TaskState::Completed, None)?;
        Ok(())
    }
}
