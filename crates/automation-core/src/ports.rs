//! 平台、视觉与确认端口。
//!
//! 业务流程只依赖这些端口，不得直接调用 Win32、OCR SDK 或鼠标键盘库。
//!
//! ## 坐标约定
//!
//! - [`Rect`] / [`Point`] 在**屏幕坐标系**下表示绝对像素位置；
//! - [`DesktopPlatform::capture`] 接收屏幕坐标系的区域，调用方不得传入窗口外的区域；
//! - [`TextBox::bounds`] 位于**截图图像坐标系**（原点为该截图的左上角），
//!   由核心层通过 `区域原点 + 图像坐标` 换算为屏幕坐标，OCR 实现无需感知屏幕偏移。

use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::diagnostics::MatchTrail;

pub type TaskId = Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    /// 区域中心点，用于把 OCR 文本框转换为可点击坐标。
    pub fn center(&self) -> Point {
        Point { x: self.x + self.width / 2, y: self.y + self.height / 2 }
    }

    /// 把截图图像坐标系下的矩形换算为屏幕坐标。
    pub fn to_screen(&self, region_origin: Point) -> Rect {
        Rect {
            x: region_origin.x + self.x,
            y: region_origin.y + self.y,
            width: self.width,
            height: self.height,
        }
    }

    pub fn is_degenerate(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }
}

#[derive(Debug, Clone)]
pub struct Screenshot {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub captured_at: SystemTime,
    /// 用于把证据与具体一次截图绑定；不参与任何相等性判断。
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextBox {
    pub text: String,
    pub bounds: Rect,
    pub confidence: f32,
}

/// 一张用于**模板匹配**的小图。
///
/// 像素表示与 [`Screenshot`] 完全一致：BGRA、自上而下、32 位。
///
/// ## 为什么模板必须由人给
///
/// 模板决定了"程序会去点哪儿"。如果让程序自己"顺手从画面上裁一块"当模板，
/// 那么裁错位置、裁到了空白，都会变成一次**静默的、看起来一切正常的**运行——
/// 匹配分数照样很高（因为它匹配的是它自己刚裁的那块），点击照样发出去，
/// 只是点到了别的地方。所以模板只能来自操作者确认过的那张图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconTemplate {
    /// 人类可读名称（一般就是文件名），只用于日志与失败信息。
    pub label: String,
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// 一次模板匹配的命中结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IconMatch {
    /// 命中位置，位于**传入截图的图像坐标系**（与 [`TextBox::bounds`] 同一套语义），
    /// 由调用方加上区域原点换算成屏幕坐标。
    pub bounds: Rect,
    /// 归一化相关系数，`-1.0 ~ 1.0`。1.0 = 完全一致。
    pub score: f32,
    /// 命中的是第几个模板（对应传入的 `templates` 下标）。
    pub template_index: usize,
    pub template_label: String,
}

/// 图标匹配的**位置先验**：这一帧里"图标大概应该在哪"。
///
/// ## 为什么需要它
///
/// 左侧导航栏是一列**纵向排列**、彼此长得很像的线性图标。逐张模板取最高分时，
/// 偶尔会出现"旁边那个图标得分略高一点"——而点错图标的代价是后面整条流程
/// 都作用在错误的视图上，比直接失败严重得多。
///
/// 这件事其实有先验信息可用：**越靠近导航区中心的命中越可信**。
/// 导航区是一列窄带，图标都排在它的纵向中心线附近；离中心很远的命中
/// 多半是列表里的头像、未读红点这类"看起来也像个小方块"的东西。
///
/// ## 为什么必须带分数容差
///
/// 只用"离中心最近"会引入一个更坏的缺陷：一个 0.82 分的噪声命中恰好压在中心上，
/// 就能把 0.99 分的正确命中挤掉。所以先验只在**分数本来就相近**时才参与决策——
/// 见 [`IconPrior::score_tolerance`]。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct IconPrior {
    /// 期望的命中位置，位于**传入截图的图像坐标系**（与 [`IconMatch::bounds`] 同一套语义）。
    pub at: Point,
    /// 位置先验生效所需的分数容差。
    ///
    /// 只有分数不低于「最高分 − 本值」的候选才有资格参与位置比较；
    /// 高分段里一个都没有时，退回"取最高分"。
    /// 取 0 等于关掉先验（只有并列最高分才会看位置）。
    pub score_tolerance: f32,
}

/// 一次图标匹配的输入。
///
/// 打包成结构体而不是继续加参数：这几个字段是**一起被决定**的
/// （阈值、模板、期望位置），调用方每次都要一次性想清楚，
/// 而参数表越长，越容易在某个调用点漏掉其中一项——漏掉先验不会报错，
/// 只会静默退回"取最高分"。
pub struct IconQuery<'a> {
    pub templates: &'a [IconTemplate],
    /// 最高分低于它转人工。见 [`IconLocator::locate`]。
    pub min_score: f32,
    /// 期望命中位置。`None` = 不做位置先验，取最高分。
    pub prior: Option<IconPrior>,
}

impl<'a> IconQuery<'a> {
    /// 不带位置先验的查询（取最高分）。
    pub fn new(templates: &'a [IconTemplate], min_score: f32) -> Self {
        Self { templates, min_score, prior: None }
    }

    /// 带上位置先验。
    pub fn with_prior(mut self, prior: Option<IconPrior>) -> Self {
        self.prior = prior;
        self
    }
}

/// 当前显示器的分辨率与缩放比例，用于点击前的标定一致性校验。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScreenMetrics {
    pub width: u32,
    pub height: u32,
    pub scale_factor: f32,
}

#[derive(Debug, Error)]
pub enum AutomationError {
    #[error("企业微信窗口不可用或不是前台窗口")]
    ClientNotReady,
    #[error("屏幕状态在操作前发生变化")]
    ScreenChanged,
    #[error("本地视觉识别结果不确定：{0}")]
    AmbiguousVision(String),
    #[error("需要人工处理：{0}")]
    NeedsHumanReview(String),
    #[error("平台操作失败：{0}")]
    Platform(String),
    #[error("任务已被取消")]
    Cancelled,
    #[error("步骤超时：{0}")]
    Timeout(String),
}

impl AutomationError {
    /// 审计用的稳定失败代码，不随提示文案变化。
    pub fn code(&self) -> &'static str {
        match self {
            Self::ClientNotReady => "CLIENT_NOT_READY",
            Self::ScreenChanged => "SCREEN_CHANGED",
            Self::AmbiguousVision(_) => "AMBIGUOUS_VISION",
            Self::NeedsHumanReview(_) => "NEEDS_HUMAN_REVIEW",
            Self::Platform(_) => "PLATFORM_ERROR",
            Self::Cancelled => "CANCELLED",
            Self::Timeout(_) => "TIMEOUT",
        }
    }

    /// 该错误是否应转入人工处理，而不是判定为失败。
    ///
    /// 依据 `docs/architecture.md` §5：超时、失焦、窗口被替换、
    /// OCR 结果冲突、风控或登录界面出现时转 `NeedsHumanReview`。
    pub fn requires_human_review(&self) -> bool {
        match self {
            Self::ClientNotReady
            | Self::ScreenChanged
            | Self::AmbiguousVision(_)
            | Self::NeedsHumanReview(_)
            | Self::Timeout(_) => true,
            Self::Platform(_) | Self::Cancelled => false,
        }
    }

    /// 是否属于可重试的瞬时错误。
    ///
    /// 只有平台层 I/O 与超时可重试；识别结果不确定、窗口状态异常、
    /// 需要人工判断的情况一律不重试，避免"越重试越错"。
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Platform(_) | Self::Timeout(_))
    }
}

/// 真实实现只可对用户可见、已解锁的交互式桌面执行操作。
pub trait DesktopPlatform: Send + Sync {
    /// 启动已由用户配置且经验证的企业微信可执行文件；不得猜测路径或提权启动。
    ///
    /// **不属于任务流程**：客户端由操作者自己启动并登录，编排器不再调用它。
    /// 保留这个端口是为了界面上的「启动客户端」按钮——那是一个显式的、人点的动作，
    /// 与"任务跑到一半自己去拉一个程序起来"是两回事。
    fn launch_wecom(&self) -> Result<(), AutomationError>;

    /// 将已验证的企业微信窗口置于前台，并返回它在屏幕上的边界。
    fn focus_wecom(&self) -> Result<Rect, AutomationError>;

    /// 把目标窗口的**外框**尺寸调整到 `width`×`height`（像素），返回调整后**重新量取**的边界。
    ///
    /// 只改尺寸：不移动位置、不改 Z 序、不抢前台。
    ///
    /// ## 为什么需要它
    ///
    /// 区域标定是相对窗口的比例，而真实界面**不是等比缩放**的——窗口尺寸一变，
    /// 四个区域就整体偏移，按偏了的区域去点击会点到别的地方。与其停下来让操作者
    /// 手工把窗口拖回标定尺寸，不如直接把它调回去：尺寸是程序完全能确定的量
    /// （标定记录里就写着目标值），这件事确定、可逆，不涉及对业务内容的任何猜测。
    ///
    /// ## 约定
    ///
    /// - **返回值必须是重新量取的**，不得为了"看起来成功"而回显请求值：
    ///   客户端可能有自己的最小尺寸限制，请求值会被应用夹住，
    ///   而 `SetWindowPos` 照样报成功。调用方拿量出来的尺寸做判据，
    ///   量不中就会转人工——回显请求值等于把这个判据废掉；
    /// - 定位不到窗口时如实报错，不要为了让调用方"继续跑"而假装成功；
    /// - 传进来的尺寸非法（非正数）应**报错**，不要"夹到某个最小值"接着调。
    fn resize_wecom(&self, width: i32, height: i32) -> Result<Rect, AutomationError>;

    /// 系统是否认为目标窗口**正在响应**（界面线程有没有在取消息）。
    ///
    /// 用途是"动作之前先确认客户端没卡死"：往一个卡死的窗口里点击、粘贴、回车，
    /// 什么都不会发生，而调用方从返回值上完全看不出区别——最坏的结果是
    /// "以为发出去了，其实一个字都没进去"。
    ///
    /// 约定：`Ok(true)` = 正在响应，`Ok(false)` = 未响应，`Err` = 无法判定
    /// （例如还没定位到窗口）。实现方不得为了让调用方"继续跑"而吞掉定位失败。
    fn is_responsive(&self) -> Result<bool, AutomationError>;

    /// 读取当前显示器分辨率与缩放比例，供点击前的标定校验使用。
    /// 只读探测目标窗几何与**该窗所在显示器**指标（不聚焦、不截屏）。
    ///
    /// 用于「记录窗口尺寸」与任务装配期按缩放挑选标定。默认实现只回
    /// [`Self::screen_metrics`]（无窗口矩形）；真实桌面后端应覆盖为定位目标窗后
    /// 按窗口矩形取所在屏缩放，与运行中的 [`Self::screen_metrics`] 同源。
    fn measure_target_window(&self) -> Result<(Rect, ScreenMetrics), AutomationError> {
        self.screen_metrics().map(|metrics| {
            (
                Rect {
                    x: 0,
                    y: 0,
                    width: metrics.width as i32,
                    height: metrics.height as i32,
                },
                metrics,
            )
        })
    }

    fn screen_metrics(&self) -> Result<ScreenMetrics, AutomationError>;

    /// 捕获指定屏幕区域。调用方不得传入企业微信窗口外的区域。
    fn capture(&self, region: Rect) -> Result<Screenshot, AutomationError>;

    /// 点击前验证当前前台窗口与预期窗口一致；不满足则拒绝输入。
    fn guarded_click(&self, target: Point, expected_window: Rect) -> Result<(), AutomationError>;

    /// 只把光标滑到 `target`，不点击、不做窗口守卫。
    ///
    /// 用于「先让人看见程序认的位置，再校验/点击」：任务路径若在守卫处失败，
    /// 操作者至少能看到鼠标有没有滑到绿框对应的地方。
    fn move_pointer(&self, target: Point) -> Result<(), AutomationError>;

    /// 在 `at` 处滚动鼠标滚轮，用于在联系人列表里向下翻找。
    ///
    /// `notches > 0` 表示**向下滚动内容**（看列表里更靠后的项），`< 0` 表示向上。
    /// 实现方需要先把光标移到 `at`（滚轮事件只送给光标下的窗口），
    /// 并和点击一样在动作前验证前台窗口与标定一致。
    ///
    /// 滚动不会误触收件人，但会改变界面内容，因此它**不是**只读操作：
    /// 调用方必须在滚动后重新截图识别，不能复用滚动前的结果。
    fn scroll(
        &self,
        at: Point,
        notches: i32,
        expected_window: Rect,
    ) -> Result<(), AutomationError>;

    /// 将文本写入剪贴板并粘贴到当前已聚焦控件；完成后清除临时剪贴板内容。
    fn paste_text(&self, text: &str, expected_window: Rect) -> Result<(), AutomationError>;

    /// 把文本**逐字符**输入到当前已聚焦的控件（不经过剪贴板）。
    ///
    /// ## 为什么不能一律用 [`DesktopPlatform::paste_text`]
    ///
    /// 客户端顶部的搜索框是**联想式**的：它按输入事件逐次刷新下拉列表。
    /// 一次性粘贴整段文字时，下拉列表要么不弹、要么只按第一次输入匹配，
    /// 于是"在下拉里找联系人"这一步永远找不到人——而现象看起来像是搜索没生效，
    /// 不会让人想到是**输入方式**的问题。
    ///
    /// 聊天输入框没有这个限制，但同样走本方法：两处共用一条输入路径，
    /// 就不存在"某一条路从没被验证过"。
    ///
    /// ## 约定
    ///
    /// - 逐字符发送 Unicode 输入事件，**不经过剪贴板**（因此不会留下残留正文）；
    /// - 字符之间留出间隔，让客户端的联想请求跟得上——间隔是平台侧配置，
    ///   不是写死的常量；
    /// - 输入前后校验前台窗口与标定一致；不满足则**拒绝输入**，
    ///   而不是"已经敲了一半才发现"。
    ///
    /// 空字符串是**合法输入**（什么都不敲），不是错误——调用方可能传一个
    /// 被裁掉内容的正文，那属于它自己的判据，不该在这里变成一个失败。
    fn type_text(&self, text: &str, expected_window: Rect) -> Result<(), AutomationError>;

    /// 清空当前已聚焦输入框里的内容（全选后删除）。
    ///
    /// ## 为什么必须有这一步
    ///
    /// 客户端的搜索框**保留上一次的输入**：上一次搜过「张三」，这一次搜「李四」时
    /// 若不清空，框里会变成「张三李四」。而它是联想式的——会拿这个混合词去查，
    /// 结果是一片与目标无关的内容。现象是"搜出来的东西不对"，
    /// 不会让人想到是**上一次的词还在**。
    ///
    /// ## 约定
    ///
    /// - 调用方应**先点击**目标输入框。本方法会先等焦点落定再发按键：
    ///   "点击"到"控件真正拿到键盘焦点"是异步的（点击事件要等目标程序的
    ///   消息循环处理完），不等就发按键会落到**上一个**有焦点的控件上，
    ///   而发送按键的 API 照样返回成功——错误要到很久以后才以别的面目出现；
    /// - 只提供"清空"这一个语义，**不暴露任意按键序列**。需要新动作时加方法，
    ///   而不是给调用方一根能敲任意键的管子：后者会让"程序到底按了什么"
    ///   变得无法从端口清单上看出来；
    /// - 前后校验前台窗口与标定一致，与其它输入动作同一套判据。
    fn clear_text_field(&self, expected_window: Rect) -> Result<(), AutomationError>;
}

/// OCR 实现必须仅使用本地模型和本机内存中的图像。
pub trait LocalOcr: Send + Sync {
    fn recognize(&self, image: &Screenshot) -> Result<Vec<TextBox>, AutomationError>;

    /// 识别，并顺手交出引擎 stdout 的**原文**：`(文字框, 原文)`。
    ///
    /// 原文是"那次到底读出了什么"的凭证（理由见 `docs/todo.md` T30）；
    /// 空串 = 没留下原文（替身引擎或引擎本身不给），**不是**"读到了空"。
    /// 默认实现只转调 [`Self::recognize`]，所以测试替身不必为了诊断而改。
    fn recognize_with_raw(
        &self,
        image: &Screenshot,
    ) -> Result<(Vec<TextBox>, String), AutomationError> {
        Ok((self.recognize(image)?, String::new()))
    }
}

/// 图标定位端口：在一帧**局部截图**里用模板匹配找出一个小图的位置。
///
/// 与 [`LocalOcr`] 的分工很清楚——OCR 回答"这一片文字写的是什么"，
/// 本端口回答"这个图标在哪儿"。两者都是本地的、都只吃局部截图、都不联网。
///
/// ## 为什么要有它
///
/// 靠 OCR 认字来找入口有个结构性弱点：图标**根本没有文字**。
/// 左侧导航栏那排图标在 OCR 眼里是空白的，于是"先切到通讯录再找联系人"
/// 这件事无从表达。模板匹配补的正是这一段：图标是固定的像素图案，
/// 拿它跟画面比一比就知道在哪。
///
/// ## 约定
///
/// - `query.templates` 为空 ⇒ 实现方**必须报错**，不得当成"没找到"静默通过；
/// - 最高分低于 `query.min_score` ⇒ 返回 [`AutomationError::AmbiguousVision`]
///   （识别不确定 ⇒ 转人工），**不得**返回一个"分数不高但先用了"的结果。
///   本项目不接受"凑合着点"：点错图标的代价是后面整条流程都作用在错误的界面上；
/// - 带 [`IconPrior`] 时，只在分数不低于「最高分 − 容差」的候选里挑**离先验最近**的那个；
///   高分段一个都没有就退回"取最高分"。**先验绝不能把低分命中抬上来**；
/// - 返回的 `bounds` 是图像坐标系，由调用方换算成屏幕坐标。
///
/// ## 为什么是"一组模板取最高分"
///
/// 同一个图标在**选中 / 未选中 / 带未读气泡**几种状态下长得不一样
/// （选中态通常有高亮底色，气泡里的数字还会变）。只留一张模板，就会出现
/// "上一次运行点完停在这个页面上，这一次就再也匹配不上"。
/// 多张模板是这里唯一诚实的解法——而不是把阈值调低到"所有状态都能过"。
///
/// ## 位置先验不是"猜"
///
/// 它只回答「**分数已经够格**的几个候选里，哪一个更可能是目标」，
/// 不改变"够不够格"这件事——阈值仍然是唯一的准入判据。
/// 两者混在一起会让"分数不够"表现为"点到了别的地方"。
pub trait IconLocator: Send + Sync {
    fn locate(
        &self,
        frame: &Screenshot,
        query: &IconQuery<'_>,
    ) -> Result<IconMatch, AutomationError>;
}

pub trait ContactMatcher: Send + Sync {
    fn find_unique_exact_match(
        &self,
        expected_name: &str,
        candidates: &[TextBox],
        min_confidence: f32,
    ) -> Result<TextBox, AutomationError>;

    /// 单个候选是否**被本策略接受**为目标联系人。
    ///
    /// **为什么它必须由匹配器回答**：编排层在选定候选之后还要复检两次
    /// （`execute` 的「核验候选人」与「核验聊天页标题」）。那两处如果自己写一套
    /// 「文字是否等于目标名」，任何放宽/收紧都会被它们**静默挡回去**——
    /// 现象是「匹配器明明放宽了，任务照样转人工」，而且失败文案看起来像是
    /// 视觉识别不确定，排查时会一路往 OCR 上找，永远找不到。
    ///
    /// 判据只有一处权威：本方法。`find_unique_exact_match` 负责「在候选集里挑一个」，
    /// 本方法负责「这一个行不行」，两者必须对同一个名字给出同样的答案。
    fn accepts(&self, expected_name: &str, candidate: &TextBox) -> bool;

    /// 与 [`Self::find_unique_exact_match`] 同一条判据，但**顺带交出轨迹**。
    ///
    /// 返回的每一条 [`Verdict`] 说清"这个候选过没过、为什么"——一张标注图能回答
    /// 「看到了什么」，回答不了「为什么这么判」，而后者才是排查时要的
    /// （2026-09-21 的现场：失败文案说下拉里没有「联系人」分组，而同一帧里
    /// 明明有这三个字，是置信度不够还是归一化改写了它，日志里一个字都没记）。
    ///
    /// ## 为什么有默认实现，而默认实现又不给轨迹
    ///
    /// 有默认实现是为了让**现有实现者不必为此改动**（测试替身尤其如此）。
    /// 默认不给轨迹，是因为轨迹必须由判据自己产出（`CONVENTIONS.md` §1.3）——
    /// 让端口这层拼一份"看起来像理由"的东西，就成了第二套判据：
    /// 它迟早与真正的判据不一致，而轨迹恰恰是用来判断判据的。
    /// 换句话说：**给不出轨迹就如实空着**，不要编。
    fn find_unique_exact_match_with_trail(
        &self,
        expected_name: &str,
        candidates: &[TextBox],
        min_confidence: f32,
    ) -> (Result<TextBox, AutomationError>, MatchTrail) {
        (
            self.find_unique_exact_match(expected_name, candidates, min_confidence),
            MatchTrail { rule: "（该匹配器不提供轨迹）", relaxed: false, candidates: Vec::new() },
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendTask {
    pub id: TaskId,
    pub external_contact_name: String,
    pub text: String,
    pub created_by: String,
}

pub trait HumanConfirmation: Send + Sync {
    fn confirm_send(&self, task: &SendTask, expires_in: std::time::Duration)
        -> Result<(), AutomationError>;
}

/// 失败证据记录端口。
///
/// 实现方**必须**先做局部裁切与脱敏（至少遮盖已识别出的文字区域），
/// 只允许把脱敏后的画面落盘；不得保存整屏原图、消息正文或剪贴板内容。
pub trait EvidenceRecorder: Send + Sync {
    fn record(&self, task_id: TaskId, label: &str, frame: &Screenshot, text_boxes: &[TextBox]);
}

// 排查材料的端口（`Observation` / `DiagnosticRecorder`）在 `crate::diagnostics`：
// 本文件已经贴着 500 行的硬上限，新端口一律搬成同级模块。
