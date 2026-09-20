//! 预置场景：主路径与各类故障路径。
//!
//! 编排器对 OCR 的调用顺序固定为：
//! 联系人候选区 → 聊天标题区 → 发送前聊天正文 → 发送后聊天正文。
//! 本模块的 [`MockScenario::script`] 按该顺序产出脚本。

use automation_core::{Rect, TextBox};

use crate::vision::ScriptedCall;

/// 构造一个文字框，`y` 为图像坐标系下的纵坐标。
pub fn tb(text: &str, y: i32, confidence: f32) -> TextBox {
    TextBox {
        text: text.to_string(),
        bounds: Rect { x: 8, y, width: 200, height: 28 },
        confidence,
    }
}

#[derive(Debug, Clone)]
pub struct MockScenario {
    pub contact: String,
    pub message: String,
    /// 联系人候选区识别结果。
    pub panel: Vec<TextBox>,
    /// 聊天标题区识别结果。
    pub header: Vec<TextBox>,
    /// 发送前聊天正文。
    pub body_before: Vec<TextBox>,
    /// 发送后聊天正文。
    pub body_after: Vec<TextBox>,
}

const OK: f32 = 0.99;

impl MockScenario {
    fn base(contact: &str, message: &str) -> Self {
        Self {
            contact: contact.to_string(),
            message: message.to_string(),
            panel: vec![tb(contact, 20, OK)],
            header: vec![tb(contact, 16, OK)],
            body_before: vec![tb("上一条历史消息", 40, OK)],
            body_after: vec![tb("上一条历史消息", 40, OK), tb(message, 90, OK)],
        }
    }

    /// 正常主路径。
    pub fn happy(contact: &str, message: &str) -> Self {
        Self::base(contact, message)
    }

    /// 候选区出现两个同名联系人 —— 必须拒绝猜测。
    pub fn duplicate_contact(contact: &str, message: &str) -> Self {
        let mut scenario = Self::base(contact, message);
        scenario.panel = vec![tb(contact, 20, OK), tb(contact, 220, OK)];
        scenario
    }

    /// 候选区只有近似名，没有逐字匹配 —— 必须拒绝。
    pub fn near_name_only(contact: &str, message: &str) -> Self {
        let mut scenario = Self::base(contact, message);
        scenario.panel = vec![tb(&format!("{contact}丰"), 20, OK)];
        scenario
    }

    /// 联系人置信度低于阈值 —— 必须拒绝。
    pub fn low_confidence_contact(contact: &str, message: &str) -> Self {
        let mut scenario = Self::base(contact, message);
        scenario.panel = vec![tb(contact, 20, 0.32)];
        scenario
    }

    /// 聊天页标题与目标不一致 —— 双重核验失败。
    pub fn header_mismatch(contact: &str, message: &str) -> Self {
        let mut scenario = Self::base(contact, message);
        scenario.header = vec![tb("另一个联系人", 16, OK)];
        scenario
    }

    /// 候选区出现登录/风控提示而非联系人列表。
    pub fn login_prompt(contact: &str, message: &str) -> Self {
        let mut scenario = Self::base(contact, message);
        scenario.panel = vec![tb("请登录企业微信", 20, OK), tb("账号存在安全风险", 60, OK)];
        scenario
    }

    /// 发送后聊天区未出现本条消息 —— 不得判定为已送达。
    pub fn delivery_missing(contact: &str, message: &str) -> Self {
        let mut scenario = Self::base(contact, message);
        scenario.body_after = vec![tb("上一条历史消息", 40, OK)];
        scenario
    }

    pub fn script(&self) -> Vec<ScriptedCall> {
        vec![
            ScriptedCall::Ok(self.panel.clone()),
            ScriptedCall::Ok(self.header.clone()),
            ScriptedCall::Ok(self.body_before.clone()),
            ScriptedCall::Ok(self.body_after.clone()),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_follows_the_documented_call_order() {
        let scenario = MockScenario::happy("张三", "你好");
        let script = scenario.script();
        assert_eq!(script.len(), 4);
    }

    #[test]
    fn delivery_missing_scenario_omits_the_message_after_send() {
        let scenario = MockScenario::delivery_missing("张三", "你好");
        assert!(scenario.body_after.iter().all(|b| !b.text.contains("你好")));
    }
}
