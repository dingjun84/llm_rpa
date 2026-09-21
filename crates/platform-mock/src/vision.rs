//! 视觉端口替身：按脚本返回 OCR 结果，并按真实策略做联系人匹配。

use std::collections::VecDeque;
use std::sync::Mutex;

use automation_core::{
    AutomationError, ContactMatcher, LocalOcr, MatchTrail, Screenshot, StrictContactMatcher,
    TextBox,
};

use crate::fault::Fault;

/// 一次脚本化的识别结果。
#[derive(Debug, Clone)]
pub enum ScriptedCall {
    Ok(Vec<TextBox>),
    Err(Fault),
}

impl ScriptedCall {
    pub fn boxes(boxes: Vec<TextBox>) -> Self {
        Self::Ok(boxes)
    }

    pub fn err(fault: Fault) -> Self {
        Self::Err(fault)
    }
}

/// 按调用顺序返回脚本化结果的 OCR 替身。
#[derive(Debug, Default)]
pub struct MockOcr {
    script: Mutex<VecDeque<ScriptedCall>>,
    /// 每次识别时输入图像的尺寸，用于断言只识别了局部区域。
    pub calls: Mutex<Vec<(u32, u32)>>,
    pub fingerprints: Mutex<Vec<String>>,
}

impl MockOcr {
    pub fn new(script: impl IntoIterator<Item = ScriptedCall>) -> Self {
        Self {
            script: Mutex::new(script.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
            fingerprints: Mutex::new(Vec::new()),
        }
    }

    pub fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    pub fn remaining_script(&self) -> usize {
        self.script.lock().unwrap().len()
    }
}

impl LocalOcr for MockOcr {
    fn recognize(&self, image: &Screenshot) -> Result<Vec<TextBox>, AutomationError> {
        self.calls.lock().unwrap().push((image.width, image.height));
        self.fingerprints.lock().unwrap().push(image.fingerprint.clone());
        match self.script.lock().unwrap().pop_front() {
            Some(ScriptedCall::Ok(boxes)) => Ok(boxes),
            Some(ScriptedCall::Err(fault)) => Err(fault.into_error()),
            None => Ok(Vec::new()),
        }
    }
}

/// 默认复用真实的严格匹配策略，只额外支持强制注入失败。
///
/// 这样"同名联系人""近似名"等场景可以直接用 OCR 数据构造，
/// 走的是生产代码里的同一套匹配逻辑。
#[derive(Debug, Default)]
pub struct MockContactMatcher {
    pub inner: StrictContactMatcher,
    forced: Mutex<Option<Fault>>,
    pub calls: Mutex<usize>,
}

impl MockContactMatcher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_aliases(aliases: impl IntoIterator<Item = (String, Vec<String>)>) -> Self {
        let mut inner = StrictContactMatcher::default();
        inner.aliases = aliases.into_iter().collect();
        Self { inner, forced: Mutex::new(None), calls: Mutex::new(0) }
    }

    /// 强制下一次匹配失败，用于测试编排器的错误处理。
    pub fn force_failure(&self, fault: Fault) {
        *self.forced.lock().unwrap() = Some(fault);
    }

    pub fn call_count(&self) -> usize {
        *self.calls.lock().unwrap()
    }

    /// 计数 + 「让这一次匹配失败」的注入。
    ///
    /// **两条匹配入口共用它**：编排层现在拿到的是带轨迹的那一条（
    /// [`ContactMatcher::find_unique_exact_match_with_trail`]），
    /// 注入若只挂在老那条上，`force_failure` 就会静默失效——
    /// 测试照样绿，而被测的失败分支一次都没跑到。
    fn guard(&self) -> Result<(), AutomationError> {
        *self.calls.lock().unwrap() += 1;
        if let Some(fault) = self.forced.lock().unwrap().take() {
            return Err(fault.into_error());
        }
        Ok(())
    }
}

impl ContactMatcher for MockContactMatcher {
    fn find_unique_exact_match(
        &self,
        expected_name: &str,
        candidates: &[TextBox],
        min_confidence: f32,
    ) -> Result<TextBox, AutomationError> {
        self.guard()?;
        self.inner.find_unique_exact_match(expected_name, candidates, min_confidence)
    }

    /// 委托内层的**同一条判据**（含轨迹）。替身自己不给轨迹——
    /// 轨迹必须由判据产出（`CONVENTIONS.md` §1.3），照抄一份就成了第二套判据。
    fn find_unique_exact_match_with_trail(
        &self,
        expected_name: &str,
        candidates: &[TextBox],
        min_confidence: f32,
    ) -> (Result<TextBox, AutomationError>, MatchTrail) {
        if let Err(err) = self.guard() {
            let trail = MatchTrail {
                rule: "（替身注入了故障，没有走到判据）",
                relaxed: false,
                candidates: Vec::new(),
            };
            return (Err(err), trail);
        }
        self.inner.find_unique_exact_match_with_trail(expected_name, candidates, min_confidence)
    }

    /// 委托内层策略 —— 替身不改变判据，只额外支持注入失败。
    ///
    /// `forced` 故障**刻意不在这里生效**：它的用途是「让这一次匹配失败」，
    /// 而本方法回答的是「这个候选符不符合判据」。混在一起会让
    /// 「强制失败」顺带把复检也弄失败，测试意图变得含混。
    fn accepts(&self, expected_name: &str, candidate: &TextBox) -> bool {
        self.inner.accepts(expected_name, candidate)
    }
}
