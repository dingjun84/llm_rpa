//! **准备消息**：聚焦输入框 → 记下发送前基线 → 把正文逐字填进去 → 发送。
//!
//! ## 发送动作由什么组成
//!
//! 「逐字输入正文 + 点发送按钮」，两样都是（2026-09-22 操作者指定）。
//!
//! - **不粘贴**：粘贴要过剪贴板，一来会覆盖操作者自己的剪贴板内容，
//!   二来它在客户端那边是"一次到达"的，中间没有任何可核对的痕迹。
//!   逐字输入与搜索框那条路共用同一个端口，就不存在"某一条输入路径从没被验证过"。
//! - **不按快捷键**：客户的发送键设置（Enter / ⌘+Enter）是**因人而异**的，
//!   按错键的表现是"输入框里多了一个换行、消息没出去"，看起来像发送失败。
//!   点按钮不依赖任何键盘设置。
//!
//! ## 「发还是不发」由谁保证
//!
//! 判据**只有一条**：配置里的「只填不发」（`stop_before_send`）。
//! 勾了就停在 [`TaskState::Prepared`]，没勾就往下走人工确认与发送——
//! 与走的是哪条工作流无关。
//!
//! ⚠️ 不要把这条判据再摊开成"某条工作流特殊"：曾经搜索式无条件停在这里
//! （`docs/todo.md` T16 的临时取舍），现在它和列表扫描式同路。
//! 两条路**各写一套**"发不发"的判断，迟早会出现「界面上勾了什么、实际按哪条算」
//! 说不清，而这恰好是最不能说不清的一件事。
//!
//! ⚠️ 「只填不发」那条分支**必须在人工确认之前**，而且**不能走到发送**：
//! 它存在的全部意义就是"绝不发送"，所以既不申请发送台账、也不写消息摘要——
//! 审计里不该留下任何"像发过了"的痕迹。
//!
//! ## 演练模式为什么不在这里提前停
//!
//! 演练模式下端口整组都是替身（`platform-mock`），`send` 只在本进程里记一笔，
//! 碰不到任何真实窗口。所以"不会真的发出去"**由替身端口保证**，
//! 不靠在这里提前停——否则演练模式下流程会缺掉后半截，
//! 而人拿它当"流程验证"的时候并不知道少了什么。
//!
//! ## 真实模式下的顺序
//!
//! 必然先过人工确认（`confirm_send` 会阻塞等操作者），确认之后才 claim 发送台账、
//! 写消息摘要、进入 [`TaskState::Sending`]。

use super::*;

use crate::dropdown::normalize_text;

impl Run<'_> {
    /// 聚焦输入框、记下发送前基线，然后把正文填进去；
    /// 要么停在 [`TaskState::Prepared`]，要么往下走人工确认与发送。
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
            self.type_into_composer()?;
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

        self.send_and_verify()
    }

    /// 把正文**逐字**敲进当前已聚焦的输入框。
    ///
    /// 两条路共用（「只填不发」与真实发送），所以不存在"某一条输入路径
    /// 从没被验证过"。正文没有搜索框那种"必须逐字敲才触发联想"的限制，
    /// 但**粘贴会覆盖操作者的剪贴板**，而这一步没有任何非用它不可的理由。
    fn type_into_composer(&mut self) -> Result<(), AutomationError> {
        let expected_window = self.ensure_calibrated()?;
        self.ensure_not_frozen("已取消填入消息正文")?;
        self.runner.ports.platform.type_text(&self.task.text, expected_window)?;
        self.check_deadline("填入消息正文")
    }

    /// 人工确认之后的发送与送达核验。**走到这里就一定是要发出去的**。
    fn send_and_verify(&mut self) -> Result<(), AutomationError> {
        self.check_cancel()?;
        self.runner.ledger.claim(self.task.id)?;
        // 摘要先于状态转换写入，确保 Sending 的审计记录已带上摘要。
        self.message_digest = Some(MessageDigest::of(&self.task.text));
        self.advance(TaskState::Sending, None)?;

        self.type_into_composer()?;
        self.click_send_button()?;
        self.verify_delivery()
    }

    /// 在「发送按钮区」里认出按钮上的文字，点它的中心。
    ///
    /// ## 为什么是"认字 + 点命中框中心"，而不是点标定区域的中心
    ///
    /// 输入框的高度随内容变化（一行 → 多行），发送按钮跟着上下移动，
    /// 而标定记下的是一块固定的比例区域。区域内按文字定位，按钮挪一点
    /// 也照样点得中；点区域中心则会在输入框变高时落到按钮上方的空白处，
    /// 而现象只是"消息没发出去"。
    ///
    /// ## 为什么命中的是**最下面**那一块
    ///
    /// 区域要是画得偏大、把聊天正文也圈了进来，历史消息里同样可能出现
    /// 「发送」这两个字。按钮紧挨着输入框，是这一片里最靠下的；
    /// 同高再取最右（按钮一定在输入框右侧）。反过来取最上面的，
    /// 就会点到一条历史消息上。
    fn click_send_button(&mut self) -> Result<(), AutomationError> {
        let label = self.cfg().send_button_text.trim().to_string();
        // 空串会让下面的 `contains` 对**任何**文字都成立，于是随手点到区域里
        // 最下面那块文字上——那是"点错地方"里最难查的一种。宁可在这里停住。
        if label.is_empty() {
            return Err(AutomationError::NeedsHumanReview(
                "「发送按钮文字」是空的，无法在标定区域里认出按钮。\
                 请到运行参数里填上按钮上的文字（默认「发送」）。"
                    .into(),
            ));
        }

        let expected_window = self.ensure_calibrated()?;
        let region = self.resolve_extra(self.cfg().send_button, "发送按钮区")?;
        let (shot, boxes) = self.capture_and_recognize(region, "发送按钮识别")?;
        self.evidence.push(format!("send_button#{}", shot.fingerprint));
        if self.cfg().log_ocr_candidates {
            self.evidence.push(format!(
                "  读到 {} 块：{}",
                boxes.len(),
                describe_candidates(&boxes)
            ));
        }

        let needle = normalize_text(&label);
        let hit = boxes
            .iter()
            .filter(|b| b.confidence >= self.cfg().min_confidence)
            .filter(|b| normalize_text(&b.text).contains(&needle))
            .max_by_key(|b| (b.bounds.y, b.bounds.x))
            .cloned()
            .ok_or_else(|| {
                AutomationError::NeedsHumanReview(format!(
                    "「发送按钮区」里没有找到「{label}」这两个字。\
                     请确认这一项文字与当前客户端对得上，或到「界面标定」页核对「发送按钮」那一块。"
                ))
            })?;

        let screen = hit.bounds.to_screen(Point { x: region.x, y: region.y });
        let target = screen.center();
        self.evidence.push(format!(
            "点击发送按钮 : 屏幕 ({}, {})   命中文字「{}」框 ({}, {}) {}x{}   置信度 {:.2}",
            target.x,
            target.y,
            hit.text.trim(),
            screen.x,
            screen.y,
            screen.width,
            screen.height,
            hit.confidence
        ));

        self.ensure_not_frozen("已取消发送")?;
        self.runner.ports.platform.guarded_click(target, expected_window)?;
        self.check_deadline("点击发送按钮")
    }

    /// 核验消息真的出现在聊天区了。
    ///
    /// 判据是**两条**，缺一不可：画面相对发送前变了、且变出来的文字里有这条正文。
    /// 只看前者，客户端弹个无关提示也会被当成发送成功；只看后者，
    /// 输入框里那行字自己就能满足它。
    fn verify_delivery(&mut self) -> Result<(), AutomationError> {
        self.advance(TaskState::VerifyingDelivery, None)?;
        let body = self.resolve(self.cfg().chat_body, "聊天正文区")?;
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
