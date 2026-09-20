//! 「这条工作流到底要什么」——**装配期与界面共用的那一份判据**。
//!
//! 拆成独立文件是因为 `runtime.rs` 又贴到 `CONVENTIONS.md` §9 的行数基线上了
//! （见 `docs/todo.md` T17），而这一块**自成一体的**：纯数据 + 纯函数，
//! 不碰端口、不碰装配流程。
//!
//! ⚠️ **它不是"几张表"那么简单。** 这里回答的两件事都会直接决定
//! 「点开始之后会发生什么」：
//!
//! - [`required_marks`]：这条工作流必须标好哪几块区域。装配期按它拒绝任务，
//!   界面按它显示"还缺哪几块"。
//! - [`workflow_inputs`]：要不要填「外部联系人名称」/「消息正文」。
//!   命令层按它拒绝空值，界面按它决定那两个框显不显示、「开始任务」能不能点。
//!
//! **两边各写一份的话，分叉的表现都是最难查的那种**：界面说齐了、点开始却被拒；
//! 或者按钮点不动、也不说为什么（2026-09-20 实测过后者）。
//!
//! ⚠️ 可见性（搬过来时最容易漏的一点）：
//! - [`workflow_requirements`] / [`workflow_inputs`] 与两个结构体被 `lib.rs` 用 ⇒ `pub`；
//! - [`required_marks`] / [`missing_marks`] 只被父模块 `runtime.rs` 用 ⇒ `pub(super)`。
//!
//! 父模块用 `pub use requirements::{...}` 再导出，所以
//! `crate::runtime::workflow_requirements` 这条路径照旧成立。

use automation_core::Workflow;
use serde::{Deserialize, Serialize};

use super::{mark_region, RunChoice, RuntimeConfig};

/// 所选工作流**必须**标好的新增区域（键 = 标定清单里的 `key`）。
///
/// ## 为什么按工作流分别要求，而不是"一概全要"或"一概不要"
///
/// - 一概全要：只想跑列表扫描式的人也得去标搜索框、下拉、资料页——三块
///   他根本不会走到的区域。多标一块就是多一次"框歪了"的机会。
/// - 一概不要：搜索式会在走到那一块时才转人工，而那时任务**已经登记进列表**，
///   看起来像是真跑过一遍。
///
/// ## 为什么 `NavigateOnly` 一个都不要
///
/// 它连人都不找，只用导航区与图标模板。但它在**真实模式**下仍然要求
/// 标定窗口尺寸——那是 `build_runner` 开头那道与工作流无关的检查。
pub(super) fn required_marks(workflow: Workflow) -> &'static [&'static str] {
    match workflow {
        // 搜索式：点搜索框 → 在下拉里挑人 → 在资料页点「发消息」。
        // 这三块各自对应一步点击，缺任何一块都走不下去。
        Workflow::SearchContact => &["main_search", "search_dropdown", "contact_profile"],
        // 列表扫描式只用 `regions.contact_panel`（必填、有默认值）。
        Workflow::ScrollListContact | Workflow::NavigateOnly => &[],
    }
}

/// 缺哪些区域、分别叫什么（给操作者看的名字取自标定清单，**不在这里另起一份**）。
///
/// 判据按**本次要跑的那条路**（`choice.workflow`）算，不按配置里那个默认值——
/// 否则会出现「界面选了搜索式、后端按列表式检查」，缺的区域一个都不报。
pub(super) fn missing_marks(config: &RuntimeConfig, choice: &RunChoice) -> Vec<String> {
    required_marks(choice.workflow)
        .iter()
        .filter(|key| mark_region(config, key).is_none())
        .map(|key| {
            // 清单里的 `label` 才是界面上的说法（「搜索框区」「下拉列表区域」…）。
            // 用 `key` 报错的话，人得自己把 `main_search` 翻译成界面上那一项。
            crate::calibration::find_item(key).map_or_else(
                || (*key).to_string(),
                |item| format!("{}（{}）", item.label, item.key),
            )
        })
        .collect()
}

/// 一条工作流对**任务输入**的要求：要不要填「外部联系人名称」/「消息正文」。
///
/// ## 为什么它必须是个判据，而不是各写各的
///
/// 命令层（`start_task`）拿它决定要不要拒绝空值，界面拿它决定那两个框要不要填、
/// 「开始任务」能不能点。两边各写一份的话，不一致的表现是两种都很难查的样子：
///
/// - 界面说必填、后端不要 ⇒ 白填一个没人用的字段；
/// - 界面说不用填、后端要 ⇒ **按钮点不动，也不说为什么**。
///
/// 2026-09-20 实测的就是后一种：选了「只做导航」（那条根本不找人、也发不出消息的
/// 工作流）之后点「开始任务」什么都不发生，`data/` 下连 `task-*.log` 都没生成。
///
/// ## 为什么不是"只要非空就放行"
///
/// 空字符串在「包含」判断里**匹配一切**（见 `search_contact_group_label` 那条），
/// 所以"能不能留空"取决于这条工作流到底用不用它，而不是"用户填没填"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowInputs {
    /// 要不要「外部联系人名称」。
    pub contact: bool,
    /// 要不要「消息正文」。
    pub message: bool,
}

/// 一条工作流要不要填那两个输入框 —— **这条判据只此一处**。
pub fn workflow_inputs(workflow: Workflow) -> WorkflowInputs {
    match workflow {
        // 「只做导航」的任务就是"找到那个图标并点它"，既不找人也发不出消息：
        // 那两个框在界面上根本不显示，命令层也不该拿它们当门槛。
        Workflow::NavigateOnly => WorkflowInputs { contact: false, message: false },
        // 另外两条路都要"找到某个人、发一句话给他"。
        Workflow::SearchContact | Workflow::ScrollListContact => {
            WorkflowInputs { contact: true, message: true }
        }
    }
}

/// 一项标定区域对某条工作流的必要性（下发给界面）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkRequirement {
    pub key: String,
    /// 界面上的说法（标定清单里的 `label`）。
    pub label: String,
    /// 当前配置里标了没有。
    pub marked: bool,
}

/// 一条工作流要用到哪些标定区域、要不要那两个输入框。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRequirement {
    pub workflow: Workflow,
    /// 工作流的名字，取自 `Workflow::describe`。
    pub label: String,
    /// 必须标好的区域；空表 = 这条工作流一块新增区域都不需要。
    pub required: Vec<MarkRequirement>,
    /// 这条工作流要不要填「外部联系人名称」。见 [`workflow_inputs`]。
    pub needs_contact: bool,
    /// 这条工作流要不要填「消息正文」。见 [`workflow_inputs`]。
    pub needs_message: bool,
}

/// 列出每条工作流需要哪些区域、要不要那两个输入框，以及**这份配置里标了没有**。
///
/// ## 为什么由后端算，而不是界面自己列一张表
///
/// 「这条工作流需要哪几块」是一个**判据**——装配期就是按 [`required_marks`]
/// 拒绝任务的。界面再写一份的话，两边不一致时的表现是
/// 「界面说齐了、点开始却被拒」，而人只会去怀疑标定本身。
/// 所以判据留在这一处，界面只负责渲染。
///
/// ## 为什么参数是配置、而不是读服务端那份
///
/// 界面是**草稿式**的：操作者刚把工作流改成搜索式、还没点保存时，
/// 他要看的是"我现在这份配置还缺什么"。读服务端那份会答非所问。
pub fn workflow_requirements(config: &RuntimeConfig) -> Vec<WorkflowRequirement> {
    Workflow::ALL
        .iter()
        .map(|workflow| {
            let inputs = workflow_inputs(*workflow);
            WorkflowRequirement {
                workflow: *workflow,
                label: workflow.describe().to_string(),
                required: required_marks(*workflow)
                    .iter()
                    .map(|key| MarkRequirement {
                        key: (*key).to_string(),
                        label: crate::calibration::find_item(key).map_or_else(
                            || (*key).to_string(),
                            |item| item.label.to_string(),
                        ),
                        marked: mark_region(config, key).is_some(),
                    })
                    .collect(),
                needs_contact: inputs.contact,
                needs_message: inputs.message,
            }
        })
        .collect()
}
