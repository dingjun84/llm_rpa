//! 过程诊断端口：把每一步"看到的画面 + 读到的文字 + 要看的区域"留下来。
//!
//! ## 为什么和 [`crate::ports::EvidenceRecorder`] 不是同一个端口
//!
//! 那个是**审计证据**：必须脱敏，落库、算哈希、到期清理。
//! 这个是**排查材料**：给操作者复盘"程序当时到底看到了什么"用。
//! 两者目的相反（一个要遮住文字，一个要把文字显示出来），所以拆成两条线，
//! 谁也不该去改对方的行为。
//!
//! ## 为什么单独成一个模块而不是留在 `ports.rs` 里
//!
//! `ports.rs` 已经贴着 500 行的硬上限（见 `CONVENTIONS.md` §2/§9）。
//! 新东西搬成同级模块，是那个文件不继续变胖的唯一办法。

use crate::ports::{Rect, Screenshot, TaskId, TextBox};

/// 一次「看画面」的完整记录：截了哪块、读到了什么。
///
/// ## 为什么要把「区域」也带上
///
/// [`crate::ports::EvidenceRecorder`] 拿到的只有画面与文字框，**没有"这一步本来要看哪儿"**。
/// 于是失败复盘时缺了最要紧的一条对照：区域标定偏了（框到了头像列、框到了标题栏），
/// 与"区域标得对但内容读错了"，在脱敏证据图上长得一模一样。
/// 把区域一起交出，才画得出「要识别的区域」那个框。
///
/// ## 坐标约定
///
/// [`Observation::region`] 是**屏幕坐标**（与 [`Rect`] 的约定一致）；
/// `text_boxes` 的 `bounds` 是**本帧图像坐标**（原点即 region 左上角），
/// 与 [`TextBox::bounds`] 的约定一致。两者不要混着用。
pub struct Observation<'a> {
    /// 这一步在做什么（区域名 / 步骤名，与任务日志里的措辞一致）。
    pub label: &'a str,
    /// 这一步要识别的区域，屏幕坐标。
    pub region: Rect,
    /// 本次截图（BGRA，见 [`Screenshot`]）。
    pub frame: &'a Screenshot,
    /// 识别到的文字块；只截了指纹、没做 OCR 的步骤为空。
    pub text_boxes: &'a [TextBox],
    /// 这一步 OCR 引擎的**原始输出**（stdout 原文）。
    ///
    /// 三种取值说清三件事：
    ///
    /// - `None`：这一步**没做 OCR**（只截了指纹看画面动没动）；
    /// - `Some("")`：做了，但引擎没留下原始文本（替身引擎，或引擎本身不给）；
    /// - `Some(text)`：这就是那次识别读出的原文。
    ///
    /// 为什么要区分前两种：落盘时按它决定"要不要留这一帧的干净输入图"。
    /// 干净输入图 + 原始输出合起来才让"重跑一次 OCR"成为可能（`docs/todo.md` T30），
    /// 而"没做 OCR"的步骤根本没有输入图可言。
    pub ocr_raw: Option<&'a str>,
}

/// 一个候选在这一次判定里的遭遇：它有没有通过判据、为什么。
///
/// ## 为什么必须有它
///
/// 一张标注图能回答「程序看到了什么」，**回答不了「它为什么这么判」**。
/// 2026-09-21 的实机现场正是卡在这里：失败文案说「下拉里没找到联系人分组」，
/// 而同一帧的文字列表里**明明有**「联系人」三个字——要么它的置信度低于阈值，
/// 要么归一化改写了它，两者日志里一个字都没记，只能靠猜。
/// 每个候选写清「过没过、为什么」，这一句才不需要猜。
#[derive(Debug, Clone)]
pub struct Verdict {
    /// 候选上的文字，**原样**（带前后的空格、换行）——排查时那本身就是线索。
    pub text: String,
    pub confidence: f32,
    /// 文字框，**本帧图像坐标**（同 [`TextBox::bounds`]）。
    pub bounds: Rect,
    /// 是否通过了它该过的那一道判据。
    pub passed: bool,
    /// 通过 / 淘汰的**具体原因**，带数字（阈值、置信度、它上面那一行是什么）。
    pub reason: String,
}

impl Verdict {
    /// 记一个**淘汰**的候选。
    pub fn rejected(candidate: &TextBox, reason: impl Into<String>) -> Self {
        Self::from_candidate(candidate, false, reason)
    }

    /// 记一个**通过**的候选（含被选中的那一个）。
    pub fn passed(candidate: &TextBox, reason: impl Into<String>) -> Self {
        Self::from_candidate(candidate, true, reason)
    }

    fn from_candidate(candidate: &TextBox, passed: bool, reason: impl Into<String>) -> Self {
        Self {
            text: candidate.text.clone(),
            confidence: candidate.confidence,
            bounds: candidate.bounds,
            passed,
            reason: reason.into(),
        }
    }
}

/// 姓名匹配的轨迹：每个候选的遭遇，外加**这条判据自己是什么**。
///
/// ## 为什么连"判据名"一起交出来
///
/// 编排层不知道用的是严格匹配还是放宽匹配——那是装配期选的（见 `policy`）。
/// 离线重放要按同一条判据重跑，就得知道当时用的是哪条。
/// 让**判据自己**报名字，比让编排层去猜要可靠。
#[derive(Debug, Clone)]
pub struct MatchTrail {
    /// 判据的可读名（写进决策记录）。
    pub rule: &'static str,
    /// 是否是从"逐字精确匹配"退化下去的放宽匹配。
    pub relaxed: bool,
    pub candidates: Vec<Verdict>,
}

/// 离线重放一条判据所需的输入。
///
/// ## 为什么输入要跟着决策一起留下
///
/// 「改一个阈值，结论会不会变」只有在**能重跑判据**时才验证得了（T29 验收第 2 条）。
/// 重跑需要的不只是候选本身，还有当时拿什么去判（关键词、目标名、分组标题）。
/// 输入与决策同源记录，才不会出现"重放用的输入"与"当时用的输入"不是一回事。
#[derive(Debug, Clone)]
pub enum ReplayInput {
    /// 搜索下拉挑人：关键词 + 分组标题原文（配置里那个字符串）。
    Dropdown { keyword: String, group_labels: String },
    /// 姓名匹配：目标联系人名 + 是否走的放宽匹配。
    ///
    /// ⚠️ 别名表**没有**记进来：重放按空别名表算。别名只在管理员显式配置过时才有值，
    /// 而重放要回答的是"阈值改了结论变不变"，不是"别名配得对不对"。
    NameMatch { expected_name: String, relaxed: bool },
}

/// 一次判定的完整记录：判什么、按哪条判据、结论是什么、每个候选为什么。
///
/// ## 与 [`Observation`] 的关系
///
/// `Observation` 是「看了什么」，`Decision` 是「怎么判的」。两者按 [`Decision::step`]
/// 与 [`Observation::label`] 对齐——同一步先看画面、再下判断。
///
/// ## 为什么字段是 `String` 而不是借来的 `&str`
///
/// 它在任务线程上构造、由实现方落盘，生命周期跨出构造点；借用会让每个字段都带一个
/// 生命周期参数，而这里没有任何省下克隆的价值（一次判定一条，几十字节）。
pub struct Decision {
    /// 这一步叫什么（与 [`Observation::label`] 同一套措辞）。
    pub step: String,
    /// 这一步在问什么，用一句人话写出来（带具体名字，例如"哪一行是「李小明」"）。
    pub question: String,
    /// 按什么判的（阈值、分组、截断规则…）——**判据的措辞**，与实现同源。
    pub rule: String,
    /// 结论：选中了什么，或者为什么停下来。
    pub outcome: String,
    /// 这条判据**通过了没有**。
    ///
    /// ## 为什么结论要有一份结构化的、而不是只写在那句人话里
    ///
    /// 界面「过程重放」打开时要**默认落在失败的那一步**——那一步才是要看的。
    /// 从 `outcome`（"转人工：没有找到分组"）里反推"成没成"是在猜字符串，
    /// 而措辞随时会改；`candidates` 里"有没有一个 `passed`"也只是个近似
    /// （判据通过了不等于这一步就一定没问题）。判据自己知道答案，
    /// 就在这里如实说一句。
    pub passed: bool,
    /// 判定时用的最低置信度（重放改阈值时覆盖它）。
    pub min_confidence: f32,
    /// 重跑这条判据要什么输入；`None` = 这条判据不支持离线重跑。
    pub replay: Option<ReplayInput>,
    /// 参加这次判定的候选，**顺序与画面上的文字块一致**。
    pub candidates: Vec<Verdict>,
}

/// 过程诊断记录端口。
///
/// 实现方**必须自己**决定怎么落盘与保留多久——不同实现可以完全不同
/// （真实实现落文件，测试实现只记内存）。
pub trait DiagnosticRecorder: Send + Sync {
    /// 记录一步。**不得阻塞**：它在任务线程上被调用，慢一秒就慢一秒流程。
    fn observe(&self, task_id: TaskId, observation: &Observation<'_>);

    /// 记录一次**判定**。
    ///
    /// 默认空实现：现有的实现者（含测试替身）不必为了轨迹而改。
    /// 但**生产者**不能偷懒——轨迹必须由判据函数本身产出（`CONVENTIONS.md` §1.3），
    /// 另写一处判据去"生成轨迹"正是本项目明确拒绝的做法：那样两条线迟早不一致，
    /// 而轨迹恰恰是用来判断判据的。
    ///
    /// 同样**不得阻塞**。
    fn decide(&self, _task_id: TaskId, _decision: &Decision) {}
}