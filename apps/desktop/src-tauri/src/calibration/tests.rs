//! `calibration` 的用例。
//!
//! 拆成独立文件是因为主体已经接近文件行数上限，而**测试文件不限行数**
//! （`CONVENTIONS.md` §2）。组织方式跟 `icon_library/tests.rs` 一致。

use std::collections::HashSet;

use automation_core::Rect;

use super::*;
use crate::runtime::{RegionConfig, RuntimeConfig};

fn config() -> RuntimeConfig {
    RuntimeConfig::default()
}

fn mark(rect: [f32; 4]) -> AreaMark {
    AreaMark {
        rect,
        calibrated_at_ms: 1_700_000_000_000,
        window: Rect { x: 0, y: 0, width: 974, height: 734 },
    }
}

/// 在计划里找一项。找不到就 panic——**不要**用 `unwrap_or_default()`：
/// 那会让「项被删掉了」变成一条看起来正常的断言失败。
fn view_of(plan: &CalibrationPlan, key: &str) -> CalibrationItemView {
    plan.scenes
        .iter()
        .flat_map(|scene| &scene.items)
        .find(|item| item.key == key)
        .unwrap_or_else(|| panic!("标定计划里没有「{key}」这一项"))
        .clone()
}

// ── 清单自身的完整性 ────────────────────────────────────────────────────

#[test]
fn item_keys_are_unique() {
    let mut seen = HashSet::new();
    for item in ITEMS {
        assert!(seen.insert(item.key), "标定项 key 重复：{}", item.key);
    }
}

#[test]
fn every_item_belongs_to_a_declared_scene() {
    for item in ITEMS {
        assert!(
            SCENES.iter().any(|scene| scene.id == item.scene),
            "标定项「{}」挂在不存在的场景「{}」上，它永远不会出现在界面上",
            item.key,
            item.scene
        );
    }
}

/// 每个场景至少要有一项。空场景在界面上是个点不开的空壳，
/// 而引导语还会说「请切到某某界面」——白让人跑一趟。
#[test]
fn no_scene_is_empty() {
    for scene in SCENES {
        let count = ITEMS.iter().filter(|item| item.scene == scene.id).count();
        assert!(count > 0, "场景「{}」一项都没有", scene.id);
    }
}

#[test]
fn every_core_region_is_covered_by_exactly_one_item() {
    let core_items: Vec<&ItemSpec> = ITEMS
        .iter()
        .filter(|item| matches!(item.storage, MarkStorage::Core(_)))
        .collect();
    assert_eq!(
        core_items.len(),
        4,
        "regions 里那四个区域必须各有一项，多一项少一项都会让界面上的编号错位"
    );
}

/// 四个核心区域**不必**排在最前面——清单按界面场景分组，它们散在
/// 「主界面」与「历史对话」两个场景里。要紧的是它们各自挂对了字段，
/// 且带着正确的探针编号（命令行截图靠那个编号沟通）。
#[test]
fn core_items_carry_the_probe_numbering() {
    let mut seen: Vec<(usize, CoreRegion)> = ITEMS
        .iter()
        .filter_map(|item| match item.storage {
            MarkStorage::Core(slot) => Some((slot.probe_index(), slot)),
            MarkStorage::Extra => None,
        })
        .collect();
    seen.sort_by_key(|(index, _)| *index);

    assert_eq!(
        seen,
        vec![
            (1, CoreRegion::ContactPanel),
            (2, CoreRegion::ChatHeader),
            (3, CoreRegion::ChatBody),
            (4, CoreRegion::Composer),
        ],
        "探针编号必须与 automation_core::DEFAULT_REGIONS 的顺序一致"
    );
}

/// 三个搜索框必须**分别是三项**。合成一项的话，操作者只会标其中一处，
/// 另外两处拿同一份坐标去点必然点空——而症状只是「点了一下没反应」。
#[test]
fn the_three_search_boxes_are_separate_items() {
    const KEYS: [&str; 3] = ["main_search", "history_search", "contacts_search"];
    for key in KEYS {
        assert!(find_item(key).is_some(), "缺少搜索框项：{key}");
    }
    // 而且它们分属三个不同场景——客户端里就是三个不同位置的控件。
    let scenes: HashSet<&str> = KEYS
        .iter()
        .filter_map(|key| find_item(key).map(|item| item.scene))
        .collect();
    assert_eq!(scenes.len(), 3, "三个搜索框应当在三个不同场景里，实际：{scenes:?}");
}

/// 只有 `regions` 那四项算必填——其余项还没有编排代码在读，
/// 标成必填会让人以为「不标就跑不了」，然后去标一堆用不上的框。
#[test]
fn only_core_items_are_required() {
    for item in ITEMS {
        let is_core = matches!(item.storage, MarkStorage::Core(_));
        assert_eq!(
            item.required, is_core,
            "「{}」的 required={}，但它 {} regions 里的项",
            item.key,
            item.required,
            if is_core { "是" } else { "不是" }
        );
    }
}

// ── 计划视图 ────────────────────────────────────────────────────────────

#[test]
fn the_plan_lists_every_item_under_its_scene() {
    let plan = plan(&config());
    let total: usize = plan.scenes.iter().map(|scene| scene.items.len()).sum();
    assert_eq!(total, ITEMS.len());
    assert_eq!(plan.total_count, ITEMS.len());
    assert_eq!(plan.scenes.len(), SCENES.len());
}

/// 未标定的项必须如实报 `None`，不能给一个猜出来的默认值——
/// 猜出来的框会让人以为「已经标好了」，然后对着它调半天。
#[test]
fn an_unmarked_extra_item_reports_no_rect() {
    let item = view_of(&plan(&config()), "nav_bar");
    assert_eq!(item.rect, None);
    assert_eq!(item.mark, None);
}

/// `regions` 里的四项是**有默认值**的，所以它们一开始就是 `Some`——
/// 这是刻意的：任务在真实模式下必须有这四个区域，界面要能显示当前值。
#[test]
fn a_core_item_reports_its_default_rect() {
    let item = view_of(&plan(&config()), "list_area");
    assert!(item.rect.is_some());
    assert_eq!(item.storage, "regions");
}

/// 探针编号只给 `regions` 那四项，其余项为 `None`。
/// 给每一项都编个号会让界面上出现两个都叫"3"的东西。
#[test]
fn only_core_items_carry_a_probe_index() {
    let plan = plan(&config());
    assert_eq!(view_of(&plan, "list_area").probe_index, Some(1));
    assert_eq!(view_of(&plan, "chat_header").probe_index, Some(2));
    assert_eq!(view_of(&plan, "nav_bar").probe_index, None);
    assert_eq!(view_of(&plan, "send_button").probe_index, None);
}

// ── 字段名：`key` 与 `regions` 的字段名**不是一回事** ────────────────────

/// ★★ 界面拿 `key` 当 `regions` 的字段名会**静默丢框**。
///
/// 「列表区」的 key 是 `list_area`，存的却是 `regions.contact_panel`——
/// 两者名字不同。前端若用 `key` 去索引 `regions`，框会写进一个
/// [`RegionConfig`] 里不存在的键，serde 反序列化时**静默丢掉**：
/// 不报错、不警告，症状只是「标完、保存，再打开就没了」。
///
/// 这正是 2026-09-19 用户实测报上来的那个问题，而且**只有这一项**会暴露——
/// 其余三项的 key 碰巧与字段名相同。所以这一条测试盯的就是那个巧合。
#[test]
fn a_core_item_reports_the_regions_field_name_not_its_key() {
    let plan = plan(&config());
    let list = view_of(&plan, "list_area");

    assert_eq!(
        list.region_field.as_deref(),
        Some("contact_panel"),
        "「列表区」存的是 regions.contact_panel，界面必须按这个字段名写"
    );
    // 这一项就是「key 与字段名不一致」的活例子。哪天有人把 key 改成
    // `contact_panel`，这条会失败——那时先想清楚还有没有别的项也这样，
    // **不要**顺手删掉它。
    assert_ne!(list.key, "contact_panel");

    // 其余三项碰巧同名，但不能依赖这个巧合：`regions` 里的每一项都要有字段名。
    for key in ["chat_header", "chat_body", "composer"] {
        assert!(
            view_of(&plan, key).region_field.is_some(),
            "「{key}」是 regions 里的项，必须下发字段名"
        );
    }

    // 反过来：**不是** `regions` 的项不能有字段名，
    // 否则界面会拿它去写一个 `regions` 里不存在的字段，同样静默丢框。
    assert_eq!(view_of(&plan, "nav_bar").region_field, None);
}

/// 下发的字段名必须**真的存在于 `regions` 里**，而且写进去读得回来。
///
/// 只断言字符串相等还不够——字段名拼错了同样是静默丢框。
/// 所以这里照**界面那条路径**走一遍：按字段名写进 `regions`、序列化、
/// 再反序列化，看框还在不在。前端就是把框写进草稿、再交给后端保存的。
#[test]
fn every_core_field_name_round_trips_through_the_config() {
    let plan = plan(&config());

    for item in ITEMS {
        let MarkStorage::Core(slot) = item.storage else {
            continue;
        };
        let field = view_of(&plan, item.key)
            .region_field
            .expect("regions 里的项必须下发字段名");

        // 用二进制精确的分数，免得 f32→f64→f32 的舍入干扰断言本身。
        let rect = [0.25, 0.5, 0.25, 0.125];

        let mut raw = serde_json::to_value(RegionConfig::default()).unwrap();
        raw[field.as_str()] = serde_json::json!(rect);
        let regions: RegionConfig = serde_json::from_value(raw)
            .unwrap_or_else(|err| panic!("「{field}」不是 regions 的字段名：{err}"));

        let mut cfg = config();
        cfg.regions = regions;
        assert_eq!(
            core_rect(&cfg, slot),
            rect,
            "「{}」按字段名「{field}」写进去，却读不回来",
            item.key
        );
    }
}

// ── 写入 ────────────────────────────────────────────────────────────────

#[test]
fn marking_an_extra_item_stores_it_under_area_marks() {
    let mut cfg = config();
    let mark = mark([0.1, 0.2, 0.3, 0.4]);
    apply_mark(&mut cfg, "nav_bar", mark.clone()).unwrap();
    assert_eq!(cfg.area_marks.get("nav_bar"), Some(&mark));
    // 不能顺手写进 regions —— 那会让「存哪」有两个说法。
    assert_ne!(cfg.regions.contact_panel, mark.rect);
}

#[test]
fn marking_a_core_item_writes_the_named_field() {
    let mut cfg = config();
    apply_mark(&mut cfg, "chat_header", mark([0.2, 0.1, 0.5, 0.8])).unwrap();
    assert_eq!(cfg.regions.chat_header, [0.2, 0.1, 0.5, 0.8]);
    assert!(cfg.area_marks.is_empty());
}

#[test]
fn rejects_an_unknown_key() {
    let mut cfg = config();
    assert!(apply_mark(&mut cfg, "no_such_area", mark([0.1, 0.1, 0.1, 0.1])).is_err());
}

#[test]
fn rejects_a_rect_that_escapes_the_window() {
    assert!(validate_rect([0.8, 0.0, 0.5, 0.5]).is_err());
    assert!(validate_rect([0.0, 0.0, 0.0, 0.5]).is_err());
    assert!(validate_rect([-0.1, 0.0, 0.5, 0.5]).is_err());
}

#[test]
fn accepts_a_full_window_rect() {
    assert!(validate_rect([0.0, 0.0, 1.0, 1.0]).is_ok());
}

/// 必填的那四项不能清空：清掉之后真实模式会直接拒绝装配，
/// 而界面上看起来只是「少了一个框」。
#[test]
fn a_core_region_cannot_be_cleared() {
    let mut cfg = config();
    assert!(clear_mark(&mut cfg, "list_area").is_err());
}

#[test]
fn clearing_an_extra_item_removes_it() {
    let mut cfg = config();
    apply_mark(&mut cfg, "nav_bar", mark([0.1, 0.1, 0.2, 0.2])).unwrap();
    clear_mark(&mut cfg, "nav_bar").unwrap();
    assert!(cfg.area_marks.is_empty());
}

// ── 失效项 ──────────────────────────────────────────────────────────────

/// 配置里存着、清单里已经没有的键要**被报出来**。
///
/// 这是清单改过之后唯一能被看见的信号：它们不会被任何流程读到，
/// 还会挡住保存（`set_runtime_config` 拒绝未知 key）。
#[test]
fn stale_keys_lists_marks_the_catalog_no_longer_has() {
    let mut cfg = config();
    apply_mark(&mut cfg, "nav_bar", mark([0.1, 0.1, 0.2, 0.2])).unwrap();
    cfg.area_marks
        .insert("search_box".to_string(), mark([0.2, 0.2, 0.2, 0.2]));
    cfg.area_marks
        .insert("contact_group".to_string(), mark([0.3, 0.3, 0.2, 0.2]));

    assert_eq!(
        stale_keys(&cfg),
        vec!["contact_group".to_string(), "search_box".to_string()],
        "只报清单里没有的；nav_bar 是已知项，不该出现"
    );
    assert_eq!(plan(&cfg).stale_keys.len(), 2);
}

#[test]
fn a_config_without_stale_marks_reports_none() {
    let mut cfg = config();
    apply_mark(&mut cfg, "nav_bar", mark([0.1, 0.1, 0.2, 0.2])).unwrap();
    assert!(stale_keys(&cfg).is_empty());
    assert!(plan(&cfg).stale_keys.is_empty());
}

/// 清理**只动失效项**：已知项一个不碰。
/// 顺手多清一个的话，操作者会发现自己刚标好的框没了，而且没有任何提示。
#[test]
fn pruning_stale_marks_keeps_the_known_ones() {
    let mut cfg = config();
    apply_mark(&mut cfg, "nav_bar", mark([0.1, 0.1, 0.2, 0.2])).unwrap();
    let kept = cfg.area_marks.get("nav_bar").cloned().unwrap();
    cfg.area_marks
        .insert("search_box".to_string(), mark([0.2, 0.2, 0.2, 0.2]));

    assert_eq!(prune_stale_marks(&mut cfg), 1);
    assert_eq!(cfg.area_marks.get("nav_bar"), Some(&kept));
    assert!(stale_keys(&cfg).is_empty());
    // `regions` 那四个按名字存，不存在"失效"这回事，不该被碰。
    assert_eq!(cfg.regions.contact_panel, RegionConfig::default().contact_panel);
}

#[test]
fn pruning_an_already_clean_config_is_a_no_op() {
    let mut cfg = config();
    let before = cfg.regions.contact_panel;
    assert_eq!(prune_stale_marks(&mut cfg), 0);
    assert_eq!(cfg.regions.contact_panel, before);
}
