//! 界面标定：**该把客户端的哪一块框出来**，以及这些框存在哪。
//!
//! ## 这个模块怎么读
//!
//! - [`catalog`] —— **清单本身**（要标哪几个界面、每个界面框哪几块），纯数据；
//! - 本文件 —— 清单的数据结构、标定结果的存取、以及下发给界面的视图。
//!
//! 清单与代码分开，是因为改动节奏不同：调清单是业务调整，改组装方式是实现调整。
//! 加一项标定只改 [`catalog`]，界面把它当数据渲染，不必动 Rust 结构体、
//! TypeScript 类型和界面元数据表三处——那种改法漏一处**不报错**，
//! 只表现为「界面上少一个入口」或「多一个永远存不进去的框」。
//!
//! ## 一个场景 = 一次截图
//!
//! 标定项按**界面状态**分组，这不是分类癖：一块区域只有在它所在的界面
//! 显示出来时才能标——搜索框在下拉面板没打开时根本不在画面上。
//! 所以流程只能是「切到某个界面 → 截一张图 → 在这张图上框出这个界面的
//! 全部区域」，而界面的引导语也必须说清「现在应该看得见什么」。
//!
//! 四个场景：主界面 / 历史对话 / 联系人列表 / 搜索下拉列表。
//! 后两个都是**主界面切过去之后的形态**（点导航区的图标切换），
//! 但它们各自有独立的搜索框与列表，位置并不相同，所以分开截、分开标。
//!
//! ## 三处「搜索框」不是同一个框
//!
//! 主界面、历史对话、联系人列表各有一个搜索框（`main_search` /
//! `history_search` / `contacts_search`）。它们**必须分别标**：
//! 客户端里这是三个不同位置、不同尺寸的控件，用同一份坐标去点，
//! 另外两个必然点空——而点空的症状只是「点了一下没反应」。
//!
//! ## 坐标一律是**比例**
//!
//! 相对目标窗口的比例（0.0–1.0），不是像素。窗口挪动、换分辨率都不用重标。
//! ⚠️ 但**窗口尺寸变了要重标**——见 `docs/todo.md` T2：会话列表左侧的
//! 头像列是固定像素宽、不随窗口等比缩放，同一份比例在另一个尺寸下会落到
//! 别的地方。所以每一项都记下「是在多大的窗口上量的」（[`AreaMark::window`]）。
//!
//! ## 两套编号，别混
//!
//! - **清单编号**：按 [`SCENES`] / [`ITEMS`] 的顺序，界面上显示成 `2.3` 这样；
//! - **探针编号**：`screen_probe annotate` 按 [`CoreRegion`] 的固定顺序
//!   画在截图上的 `1`~`4`。
//!
//! 两者不是一回事（四个核心区域现在分散在主界面与历史对话两个场景里）。
//! 界面上把探针编号也显示出来（[`CalibrationItemView::probe_index`]），
//! 这样操作者说「把 3 往左挪」时能确定指的是哪一个。

use std::collections::BTreeMap;

use automation_core::{Rect, RelativeRegion};
use serde::{Deserialize, Serialize};

use crate::runtime::RuntimeConfig;

mod catalog;

pub use catalog::{ITEMS, SCENES};

// ── 清单的数据结构 ──────────────────────────────────────────────────────
// 清单的**内容**在 `catalog.rs`；这里只放它的形状。

/// 一个**界面状态**。标定项按它分组，界面上一次只引导标一个。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SceneSpec {
    pub id: &'static str,
    pub label: &'static str,
    /// 「现在请把客户端切成什么样」。
    ///
    /// 这句话决定了截图里有没有要标的东西，所以要说**看得见什么**，
    /// 而不是「切到搜索页面」这种说了等于没说的——后者会让人截到一张
    /// 少了目标区域的图，然后对着图找不到该框哪儿。
    pub instruction: &'static str,
}

/// 一个标定项的坐标**存在配置的哪个位置**。
///
/// 这个映射只有这一处定义，界面据此决定往草稿的哪个字段写。
/// 写成两份的话，迟早会有一项写进一个没人读的地方，
/// 而症状是「框明明拖了，任务里却用不上」——看不出是存错了地方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkStorage {
    /// `RuntimeConfig.regions` 里的具名字段。
    ///
    /// 这四个区域编排层已经在读（`RegionConfig::to_runner_regions`），
    /// 存储结构与读取路径都不能动：它们有默认值，且真实模式下缺一不可。
    Core(CoreRegion),
    /// `RuntimeConfig.area_marks`，键是标定项的 `key`。
    ///
    /// 这些区域还没有编排代码在读，所以**没有默认值**：`None` 就是「还没标」，
    /// 界面上要如实显示成未标定。给一个猜出来的默认值，症状会是
    /// 「任务照常跑完，只是点到了别的地方」——本项目最不能接受的一类失败。
    Extra,
}

/// `regions` 里的四个具名区域。
///
/// 变体的**声明顺序就是探针编号**（见 [`CoreRegion::probe_index`]），
/// 改动顺序会让命令行截图上的角标与配置里的字段对不上。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreRegion {
    ContactPanel,
    ChatHeader,
    ChatBody,
    Composer,
}

impl CoreRegion {
    /// 这一项在命令行探针截图上的角标编号（1~4）。
    ///
    /// **必须与 `automation_core::DEFAULT_REGIONS` 的顺序一致**：
    /// `screen_probe annotate` 就是按那个顺序画的，两边不一致时，
    /// 操作者会照着探针截图说「把 2 往右挪」，而界面上的 2 是另一个区域。
    pub const fn probe_index(self) -> usize {
        match self {
            CoreRegion::ContactPanel => 1,
            CoreRegion::ChatHeader => 2,
            CoreRegion::ChatBody => 3,
            CoreRegion::Composer => 4,
        }
    }

    /// 这一项在 `regions` 结构体里的**字段名**。
    ///
    /// ★★ 界面必须用它、**不能用 [`ItemSpec::key`]** —— 两者不一定相同：
    /// `list_area` 存的是 `regions.contact_panel`。
    /// 拿 `key` 当字段名的话，框会写进一个 `CoreRegions` 里不存在的键，
    /// serde 反序列化时**静默丢掉**，表现为「标完、保存，再打开就没了」。
    ///
    /// 字段名的**权威定义在这里**（只有这一处），界面从
    /// [`CalibrationItemView::region_field`] 拿。
    pub const fn field_name(self) -> &'static str {
        match self {
            CoreRegion::ContactPanel => "contact_panel",
            CoreRegion::ChatHeader => "chat_header",
            CoreRegion::ChatBody => "chat_body",
            CoreRegion::Composer => "composer",
        }
    }
}

/// 一个标定项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemSpec {
    pub key: &'static str,
    /// 属于哪个界面状态（[`SCENES`] 里的 `id`）。
    pub scene: &'static str,
    pub label: &'static str,
    /// 框哪儿、以及**框偏了会怎样**。后者才是这句话的价值所在：
    /// 「框住搜索框」谁都知道，而「左右要盖住整条框，点偏一点就点不进去」
    /// 是只有踩过才知道的事。
    pub hint: &'static str,
    /// 真实模式下**缺了它就跑不起来**的项。界面上要显眼地标出来。
    ///
    /// 只有 `regions` 那四项是 `true`：`build_runner` 在真实模式下会检查它们。
    /// 其余项标成 `false` 是**如实反映现状**——它们还没有编排代码在读，
    /// 硬标成必填只会让人以为「不标就跑不了」，然后去标一堆用不上的框。
    pub required: bool,
    pub storage: MarkStorage,
}

// ── 标定结果 ────────────────────────────────────────────────────────────

/// 一次区域标定的结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AreaMark {
    /// 相对窗口的比例 `[x, y, width, height]`，0.0–1.0。
    pub rect: [f32; 4],
    /// 标定时刻（Unix 毫秒）。用来回答「这个框是不是很久以前标的」。
    pub calibrated_at_ms: u64,
    /// 标定时目标窗口的几何。
    ///
    /// 记它是为了回答「这一份比例是在多大的窗口上量的」——比例本身跨尺寸可用，
    /// 但界面元素（左侧图标栏、头像列）是固定像素宽的，换了尺寸就得重标。
    /// 位置只用于显示，不参与任何判断。
    pub window: Rect,
}

/// 校验一份标定坐标。
///
/// **复用编排层那一条判据**（`RelativeRegion::validate`），不在这里另写一套：
/// 两处各写一份的话，迟早会出现「界面说合法、任务却拒绝」，
/// 而那种报错会把人引向完全无关的方向。
pub fn validate_rect(rect: [f32; 4]) -> Result<(), String> {
    RelativeRegion::new(rect[0], rect[1], rect[2], rect[3])
        .validate()
        .map_err(|err| err.to_string())
}

/// 这个 key 是不是已知的标定项。
///
/// 保存来自界面的坐标之前必须先过这一关：否则配置里会攒下一堆
/// 拼错的键，而它们**永远不会被读到**，也不会报错。
pub fn find_item(key: &str) -> Option<&'static ItemSpec> {
    ITEMS.iter().find(|item| item.key == key)
}

// ── 下发给界面的视图 ────────────────────────────────────────────────────

/// 一项的当前状态。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationItemView {
    pub key: String,
    pub label: String,
    pub hint: String,
    pub required: bool,
    /// `"regions"` 或 `"area_marks"`——界面据此决定把拖出来的框写进草稿的哪个字段。
    pub storage: String,
    /// 当前坐标。`None` = **还没标定**（只有 `area_marks` 存储的项会为 `None`）。
    pub rect: Option<[f32; 4]>,
    /// 标定元数据。只有 `area_marks` 存储的项才有——`regions` 里存不下这些。
    pub mark: Option<AreaMark>,
    /// 命令行探针截图上的角标编号（1~4）。只有 `regions` 那四项有。
    ///
    /// 与界面上的清单编号是两套（见模块文档）。两个都显示出来，
    /// 操作者按截图沟通时才不会指错。
    pub probe_index: Option<usize>,
    /// 写进 `regions` 时用的**字段名**（`storage == "regions"` 时才有）。
    ///
    /// ★★ 界面必须用它当键，**不能用 `key`**：两者不一定相同
    /// （`list_area` 存的是 `regions.contact_panel`）。
    /// 权威定义在 [`CoreRegion::field_name`]。
    pub region_field: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationSceneView {
    pub id: String,
    pub label: String,
    pub instruction: String,
    pub items: Vec<CalibrationItemView>,
}

/// 完整标定计划。界面拿它渲染整个向导。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationPlan {
    pub scenes: Vec<CalibrationSceneView>,
    /// 已标定的项数 / 总项数，界面用来显示进度。
    pub marked_count: usize,
    pub total_count: usize,
    /// 配置里存着、但清单里已经没有的标定项。见 [`stale_keys`]。
    pub stale_keys: Vec<String>,
}

/// 按当前配置生成标定计划。
pub fn plan(config: &RuntimeConfig) -> CalibrationPlan {
    let scenes: Vec<CalibrationSceneView> = SCENES
        .iter()
        .map(|scene| CalibrationSceneView {
            id: scene.id.to_string(),
            label: scene.label.to_string(),
            instruction: scene.instruction.to_string(),
            items: ITEMS
                .iter()
                .filter(|item| item.scene == scene.id)
                .map(|item| item_view(item, config))
                .collect(),
        })
        .collect();

    let marked_count = ITEMS
        .iter()
        .filter(|item| item_view(item, config).rect.is_some())
        .count();

    CalibrationPlan {
        scenes,
        marked_count,
        total_count: ITEMS.len(),
        stale_keys: stale_keys(config),
    }
}

fn item_view(item: &ItemSpec, config: &RuntimeConfig) -> CalibrationItemView {
    let (rect, mark) = match item.storage {
        MarkStorage::Core(slot) => (Some(core_rect(config, slot)), None),
        MarkStorage::Extra => match config.area_marks.get(item.key) {
            Some(mark) => (Some(mark.rect), Some(mark.clone())),
            None => (None, None),
        },
    };

    CalibrationItemView {
        key: item.key.to_string(),
        label: item.label.to_string(),
        hint: item.hint.to_string(),
        required: item.required,
        storage: match item.storage {
            MarkStorage::Core(_) => "regions",
            MarkStorage::Extra => "area_marks",
        }
        .to_string(),
        rect,
        mark,
        probe_index: match item.storage {
            MarkStorage::Core(slot) => Some(slot.probe_index()),
            MarkStorage::Extra => None,
        },
        region_field: match item.storage {
            MarkStorage::Core(slot) => Some(slot.field_name().to_string()),
            MarkStorage::Extra => None,
        },
    }
}

fn core_rect(config: &RuntimeConfig, slot: CoreRegion) -> [f32; 4] {
    match slot {
        CoreRegion::ContactPanel => config.regions.contact_panel,
        CoreRegion::ChatHeader => config.regions.chat_header,
        CoreRegion::ChatBody => config.regions.chat_body,
        CoreRegion::Composer => config.regions.composer,
    }
}

// ── 写入与清理 ──────────────────────────────────────────────────────────

/// 把一份标定结果写进配置。
///
/// `regions` 与 `area_marks` 的分流**按 [`ITEMS`] 里声明的 `storage` 走**，
/// 调用方不自己判断——否则「哪一项存哪」就有了第二个说法。
/// 返回 `Err` 时配置**没有被改动**（先全部校验再写入）。
pub fn apply_mark(
    config: &mut RuntimeConfig,
    key: &str,
    mark: AreaMark,
) -> Result<(), String> {
    let item = find_item(key).ok_or_else(|| format!("未知的标定项「{key}」"))?;
    validate_rect(mark.rect)?;

    match item.storage {
        MarkStorage::Core(slot) => match slot {
            CoreRegion::ContactPanel => config.regions.contact_panel = mark.rect,
            CoreRegion::ChatHeader => config.regions.chat_header = mark.rect,
            CoreRegion::ChatBody => config.regions.chat_body = mark.rect,
            CoreRegion::Composer => config.regions.composer = mark.rect,
        },
        MarkStorage::Extra => {
            config.area_marks.insert(key.to_string(), mark);
        }
    }
    Ok(())
}

/// 清掉一项的标定。
///
/// 只对 `area_marks` 的项有意义：`regions` 那四个是**必填**的，
/// 「清除」只会把它们打回默认值——那不是"未标定"，只是换回了猜的那一份。
/// 界面上对这四个不提供清除。
pub fn clear_mark(config: &mut RuntimeConfig, key: &str) -> Result<(), String> {
    let item = find_item(key).ok_or_else(|| format!("未知的标定项「{key}」"))?;
    if let MarkStorage::Core(_) = item.storage {
        return Err(format!(
            "「{}」是任务的必填区域，不能清空——只能重新标定。",
            item.label
        ));
    }
    config.area_marks.remove(key);
    Ok(())
}

/// 配置里存着、但清单里已经没有的标定项。
///
/// ## 为什么需要它
///
/// 清单会随流程完善而变（改名、拆并、删项），而配置是**持久化**的：
/// 清单改过之后，旧 key 就留在了 `area_marks` 里。它们**不会被任何流程读到**，
/// 而且会**挡住保存**——`set_runtime_config` 会拒绝未知 key。
///
/// 于是升级到新清单的人会卡在「一保存就报错，但界面上找不到那个项」。
/// 报错信息本身说得没错（那个键确实没人读），可**没有出口**。
/// 这个函数与 [`prune_stale_marks`] 就是那个出口。
///
/// 顺序取自 `BTreeMap`，所以是稳定的（按 key 排序），界面不必再排一次。
pub fn stale_keys(config: &RuntimeConfig) -> Vec<String> {
    config
        .area_marks
        .keys()
        .filter(|key| find_item(key).is_none())
        .cloned()
        .collect()
}

/// 清掉全部失效的标定项，返回清掉的个数。
///
/// 只动 `area_marks` 里的**未知键**，已知项一个不碰。
/// `regions` 那四个具名字段更不受影响——它们按名字存，不存在"失效"这回事。
pub fn prune_stale_marks(config: &mut RuntimeConfig) -> usize {
    let stale = stale_keys(config);
    for key in &stale {
        config.area_marks.remove(key);
    }
    stale.len()
}

/// 标定结果的集合类型（配置里那个字段的类型）。
pub type AreaMarks = BTreeMap<String, AreaMark>;

#[cfg(test)]
mod tests;
