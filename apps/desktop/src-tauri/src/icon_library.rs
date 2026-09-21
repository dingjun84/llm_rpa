//! 图标库：把「从窗口画面上框出来的一个小图标」存成**有名字的 PNG**。
//!
//! ## 为什么需要它
//!
//! 左侧导航栏那排图标上**没有文字**，OCR 读到的是一片空白，所以「先切到某个视图」
//! 这件事只能靠模板匹配（见 `docs/architecture.md`）。而模板从哪来？只能从
//! **真实画面**上截——那是操作者对着屏幕做的一次判断，程序替不了。
//!
//! 既然必须由人来框，就得有个地方把框出来的结果存下来、起个名字、下次还能用。
//! 否则「截一次、手抄一个路径」这件事每次都要重来，而且路径是抄不错才怪的东西。
//!
//! ## 一个名字 = 一组图
//!
//! 同一个图标在**选中 / 未选中 / 带气泡提醒 / 气泡里数字不一样**时长得都不一样，
//! 而它们指的是**同一个**图标。所以库里的组织方式是「一个名字一个目录」：
//!
//! ```text
//! data/icons/聊天/1.png    ← 未选中
//! data/icons/聊天/2.png    ← 选中
//! data/icons/聊天/3.png    ← 带未读气泡「3」
//! data/icons/聊天/4.png    ← 带未读气泡「99+」
//! ```
//!
//! 配置里引用的也是**名字**（`聊天`），不是路径。这样以后补一张变体不必回配置页
//! 重勾一次——每补一张都要重勾，迟早会漏，而漏掉的那张**不报任何错**，
//! 只表现为「这个状态下匹配不上」。
//!
//! 为什么不能只留一张：点完图标界面会**停在这个视图上**，图标随之变成选中态；
//! 下一次跑的时候画面上是选中态，而库里只有未选中那张 ⇒ 再也匹配不上。
//!
//! ## 为什么名字要卡这么严
//!
//! 名字会被拼成**目录名**（`<图标库>/<名字>/`）。于是：
//!
//! - **不允许路径分隔符与保留字符**——名字一旦能带 `\` 或 `..`，
//!   「保存图标」就变成了「往任意路径写文件」；
//! - **不允许结尾的点**——Windows 会在创建文件时**静默**去掉结尾的点，
//!   于是「保存成功」之后按原名再也找不到那个文件，症状是「明明存了却说没有」；
//! - **拒绝设备名**（`CON` / `NUL` / `COM1` …）——这些名字在 Windows 上即使
//!   带上扩展名也建不出来，报出来的是没头没尾的「系统找不到指定的文件」。
//!
//! 这些规则只在**一处**（[`validate_name`]）实现，界面与命令都走它。
//!
//! ## 为什么不自动裁
//!
//! 程序绝不自己去猜「图标大概在这块」。猜错位置、裁到空白，都会变成一次
//! **静默的、看起来一切正常**的运行——匹配分数照样很高（它匹配的就是它自己
//! 刚裁的那块），点击照样发出去，只是点到了别的地方。模板只能由人框。
//!
//! ## 存哪儿
//!
//! 默认在**数据目录**下的 `icons/`，不是 AppData。理由很实际：AppData 底下
//! 那层目录名是包标识符（`com.example.wecom-local-rpa`），没人记得住，找一次要翻半天；
//! 而图标模板是**人对着屏幕一张一张框出来的素材**，找得到、备份得了才是最重要的。
//! 数据目录 = **程序运行当前路径**下的 `data/`（见 [`crate::data_dir`]），
//! 与配置文件、任务日志、证据图同在一处，整个目录可以整体拷走。
//!
//! 目录本身可以在界面上改（`RuntimeConfig.icons_dir`，留空即用默认）。
//!
//! ## 这个模块怎么读
//!
//! 按职责分成三块，[`layout`] 是另外两块都要用的那点换算：
//!
//! - [`layout`] —— 目录在哪儿、名字怎么拼成路径；
//! - [`read`] —— 目录 → 列表，以及配置里的名字 → 全部变体路径；
//! - [`write`] —— 存一张新变体，以及两种粒度的删除。
//!
//! 对外仍然只有一份接口（下面那几条 `pub use`），调用方写 `icon_library::list(...)`
//! 这样用，不必知道它内部拆成了几个文件。

mod layout;
mod read;
#[cfg(test)]
mod tests;
mod write;

pub use layout::{default_dir, ensure_dir, resolve_dir};
pub use read::{list, resolve_selection, variants_of};
pub use write::{delete, delete_variant, save};

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 图标库目录名。
pub const ICONS_SUBDIR: &str = "icons";

/// 图标名的最大字符数。
///
/// 按**字符**算而不是字节：中文名三个字占 9 字节，按字节算会让人莫名其妙被拒。
const MAX_NAME_CHARS: usize = 40;

/// 文件名里一律不允许出现的字符（Windows 的非法字符集）。
const FORBIDDEN_CHARS: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Windows 保留设备名：带上扩展名也一样建不出文件。
const RESERVED_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// 图标库里的**一张图**：同一个图标的某一种样子。
///
/// `image` 直接带一个 data URL，是为了让界面**一次调用**就能把列表画出来
/// （缩略图 + 尺寸）。图标本身很小（上限 128×128），多带这一点载荷
/// 比让界面为每一项再发一次请求划算得多。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IconVariant {
    /// PNG 的完整路径。
    pub file: String,
    /// 相对**图标库目录**的路径（`聊天/2.png`）。删除时用它指名道姓。
    pub relative: String,
    /// 模板尺寸（窗口像素）。读不出来时是 0。
    pub width: u32,
    pub height: u32,
    /// 文件字节数。
    pub bytes: u64,
    /// `data:image/png;base64,...`；文件读不出来时是空串。
    pub image: String,
    /// **这张图现在不能当模板用**的原因（尺寸越界、文件损坏）。
    ///
    /// 刻意保留坏条目而不是从列表里藏掉：图标库里的文件是会被外部工具改的
    /// （有人用画图另存一遍），藏起来只会让人以为「我明明存过」。
    /// 显示出来，顺手就把「任务装配时才发现模板不可用」提前到了列表里。
    pub problem: Option<String>,
}

/// 图标库里的一项：**一个名字 + 它的全部变体**。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IconEntry {
    /// 名字。配置里引用的就是它。
    pub name: String,
    /// 这个名字对应的目录（旧式的单文件图标则是那个文件本身）。
    pub path: String,
    /// 这个名字下的全部图。**顺序即变体编号**，界面按它排缩略图。
    pub variants: Vec<IconVariant>,
    /// 能不能拿去当导航模板：**每一张**都要读得出来。
    ///
    /// 由后端算好下发，而不是让界面自己拿 `variants` 再判一遍：
    /// 判据写在两处，就一定会有一天不一致——而这里不一致的后果是
    /// 「界面说能用、任务装配时却被拒」，正是最难查的那种组合。
    pub usable: bool,
}

impl IconEntry {
    /// `usable` 在这里算，是因为它必须与「什么样的变体算能用」保持同一处判据。
    pub(crate) fn new(name: String, path: PathBuf, variants: Vec<IconVariant>) -> Self {
        let usable = !variants.is_empty() && variants.iter().all(|item| item.problem.is_none());
        Self { name, path: path.display().to_string(), variants, usable }
    }
}

/// 校验并规范化图标名。
///
/// 返回值是**去掉首尾空白之后**的名字——调用方必须用它，不要再拿原始输入去拼路径，
/// 否则「保存时用了 trim 后的名字、删除时用了原始输入」这种不一致会漏出去。
pub fn validate_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();

    if name.is_empty() {
        return Err("图标名不能为空——起一个你自己认得出的名字，例如「聊天」。".to_string());
    }

    let chars = name.chars().count();
    if chars > MAX_NAME_CHARS {
        return Err(format!(
            "图标名太长（{chars} 个字符，上限 {MAX_NAME_CHARS}）——它会变成目录名，短一点更好认。"
        ));
    }

    if let Some(bad) = name
        .chars()
        .find(|c| c.is_control() || FORBIDDEN_CHARS.contains(c))
    {
        return Err(format!(
            "图标名里不能有 {bad:?}——名字会被拼成目录名，含路径分隔符就等于往任意路径写文件。"
        ));
    }

    if name.starts_with('.') || name.ends_with('.') {
        return Err(
            "图标名不能以点开头或结尾——Windows 建目录/文件时会静默去掉结尾的点，\
             之后按原名就再也找不到它了。"
                .to_string(),
        );
    }

    if RESERVED_NAMES.contains(&name.to_ascii_lowercase().as_str()) {
        return Err(format!(
            "「{name}」是 Windows 的保留设备名，带上扩展名也建不出来，换一个吧。"
        ));
    }

    Ok(name.to_string())
}
