//! `runtime` 的用例。
//!
//! 拆成独立文件是因为主体已经接近文件行数上限，而**测试文件不限行数**
//! （`CONVENTIONS.md` §2）。组织方式跟 `calibration/tests.rs` 一致。

use super::*;

use automation_core::{MemoryAudit, MemorySendLedger};

/// 造一张真的 PNG 当模板。
///
/// 刻意**不引 `image` 这个依赖**：`vision::pixels` 已经能把 BGRA 缓冲编码成
/// PNG，而构造 BGRA 缓冲只需要一个 `Vec<u8>`。少一个依赖就少一处版本漂移
/// （`RgbaImage` 在两个 crate 里是两个不同的类型，版本不一致时很难看出原因）。
fn write_png(path: &std::path::Path, width: u32, height: u32) {
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            pixels.extend_from_slice(&[(x * 7) as u8, (y * 11) as u8, ((x + y) * 5) as u8, 255]);
        }
    }
    let shot = automation_core::Screenshot {
        pixels,
        width,
        height,
        captured_at: std::time::SystemTime::now(),
        fingerprint: String::new(),
    };
    let rgba = vision::pixels::to_rgba(&shot).unwrap();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, vision::pixels::encode_png(&rgba).unwrap()).unwrap();
}

/// 一个用例专用的图标库目录。
fn temp_icons_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rpa-llm-nav-icons-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 往图标库里放一个名为 `name` 的图标，底下带 `count` 张变体。
///
/// 名字对应一个**目录**、变体是目录里的编号 PNG——这正是任务装配时会去读的形状。
fn write_icon(icons_dir: &std::path::Path, name: &str, count: u32, width: u32, height: u32) {
    for index in 1..=count {
        write_png(&icons_dir.join(name).join(format!("{index}.png")), width, height);
    }
}

fn sample_task() -> SendTask {
    SendTask {
        id: uuid::Uuid::new_v4(),
        external_contact_name: "外部测试联系人".into(),
        text: "测试正文".into(),
        created_by: "测试操作者".into(),
    }
}

/// 走一遍真实的装配路径，只关心它成不成功、以及装配出来的运行器里有什么。
///
/// 图标库目录按用例给：它决定了"配置里的图标名能展开出哪些图"，
/// 而这正是导航那组用例要验的东西。
fn assemble_with_icons(
    config: &RuntimeConfig,
    icons_dir: &std::path::Path,
) -> Result<WorkflowRunner, String> {
    // 用例把"要跑哪条路"写在 `RuntimeConfig` 的那几个**默认值**字段上，
    // 这里照抄成运行参数。生产路径上这一步由 `start_task` 从任务请求里取
    // （见 `RunChoice`）——**配置不再是运行时的判据来源**，所以用例必须
    // 显式地把它转成参数，不能指望装配函数自己去读配置。
    let choice = RunChoice {
        mode: config.mode,
        workflow: config.workflow,
        nav_target: config.nav_target.clone(),
    };
    build_runner(
        config,
        &choice,
        &sample_task(),
        icons_dir,
        Arc::new(MemoryAudit::new()),
        Arc::new(MemorySendLedger::new()),
    )
}

/// 单步超时不能小于 OCR 超时，否则把 OCR 超时调大是白调的。
///
/// 这两个超时是嵌套关系：一个步骤里就包含一次 `capture + 本地 OCR`。
/// 外层先到点的话，任务报的是 `Timeout`，使用者会误以为是 OCR 的问题。
#[test]
fn step_timeout_never_undercuts_the_ocr_timeout() {
    let config = RuntimeConfig {
        ocr_timeout_ms: 60_000,
        // 故意配一个比 OCR 超时小得多的下限。
        step_timeout_secs: 5,
        ..RuntimeConfig::default()
    };
    let effective = config.to_runner_config().step_timeout;
    assert!(
        effective >= Duration::from_secs(65),
        "单步超时应至少是 OCR 超时加余量，实际是 {effective:?}"
    );
}

/// 反过来：单步下限配得很大时，不能被 OCR 超时压下去。
#[test]
fn a_large_step_floor_is_respected() {
    let config = RuntimeConfig {
        ocr_timeout_ms: 1_000,
        step_timeout_secs: 120,
        ..RuntimeConfig::default()
    };
    assert_eq!(
        config.to_runner_config().step_timeout,
        Duration::from_secs(120)
    );
}

/// 默认值本身也要自洽：默认的单步超时必须容得下默认的 OCR 超时。
#[test]
fn the_defaults_are_self_consistent() {
    let config = RuntimeConfig::default();
    let effective = config.to_runner_config().step_timeout;
    assert!(
        effective > Duration::from_millis(config.ocr_timeout_ms),
        "默认单步超时 {effective:?} 容不下默认 OCR 超时 {}ms",
        config.ocr_timeout_ms
    );
}

/// 界面标定用的默认值必须与核心层的出厂值逐字段一致。
///
/// `RegionConfig` 用 `[f32; 4]`、核心层用 `RelativeRegion`，类型不同，
/// 以前是各写一份字面量——两边不一致时**不报任何错**，只是界面按一份画框、
/// 任务按另一份裁图，现场表现为「框明明画对了，却识别不到」。
/// 现在后者由前者派生，这条用例把它钉死。
#[test]
fn the_region_defaults_match_the_core_constants() {
    let regions = RegionConfig::default();
    let [panel, header, body, composer] = DEFAULT_REGIONS;
    assert_eq!(regions.contact_panel, [panel.x, panel.y, panel.width, panel.height]);
    assert_eq!(regions.chat_header, [header.x, header.y, header.width, header.height]);
    assert_eq!(regions.chat_body, [body.x, body.y, body.width, body.height]);
    assert_eq!(regions.composer, [composer.x, composer.y, composer.width, composer.height]);

    // 顺带钉住「区域经过校验」：比例写错要到任务跑起来才发现就太晚了。
    let runner = RuntimeConfig::default().to_runner_config();
    for (label, region) in [
        ("联系人候选区", runner.contact_panel),
        ("聊天页标题区", runner.chat_header),
        ("聊天正文区", runner.chat_body),
        ("消息输入框区", runner.composer),
    ] {
        assert!(region.validate().is_ok(), "{label} 的默认比例不合法：{region:?}");
    }
}

/// `contact_panel` 的左边界**不能是 0**，否则联系人姓名永远匹配不上。
///
/// 会话列表左侧还有导航图标栏和头像列（头像右上角带未读红点），
/// 它们与姓名在同一行高度上，会被 OCR **并进同一个文字块**。
/// 实测（960x734）：左边界取 0 时读到 `《明月（美、加、欧洲）清库存`、
/// `0 李四`，取 0.14 时读到干净的 `明月（美、加、欧洲）清库存`、`李四`。
/// 而姓名匹配是逐字精确的（`docs/architecture.md` §6.4/§6.6），
/// 多一个前导字符就永远找不到人。
///
/// 这条用例守的是「有人为了多看到点头像信息，顺手把左边界改回 0」。
#[test]
fn the_contact_panel_must_not_swallow_the_avatar_column() {
    let regions = RegionConfig::default();
    assert!(
        regions.contact_panel[0] > 0.0,
        "联系人候选区的左边界必须让开左侧图标栏与头像列，实际是 {}",
        regions.contact_panel[0]
    );
    assert!(
        regions.contact_panel[0] + regions.contact_panel[2] <= 1.0,
        "联系人候选区右边界越出了窗口：{regions:?}"
    );
}

/// 滚动落点默认是「上下居中、左右偏右一点」，并原样透传到核心层。
#[test]
fn scroll_anchor_defaults_to_slightly_right_of_center() {
    let config = RuntimeConfig::default();
    // 与核心层的常量对齐，而不是各写一份 `0.62 / 0.5`。
    assert_eq!(
        config.scroll_anchor,
        ScrollAnchorConfig { x: DEFAULT_SCROLL_ANCHOR.x, y: DEFAULT_SCROLL_ANCHOR.y }
    );
    let runner = config.to_runner_config();
    assert_eq!(runner.scroll_anchor, DEFAULT_SCROLL_ANCHOR);
    assert!(runner.scroll_anchor.validate().is_ok());
}

/// 落点比例**不在这里夹到 0–1**，非法值必须原样透传、由核心层报错。
///
/// 夹边界会把「配置写错了」变成「滚了半天没反应」——
/// 后者是现场最难查的一类现象，所以宁可让任务直接失败并说清原因。
#[test]
fn an_out_of_range_scroll_anchor_is_passed_through_not_clamped() {
    let config = RuntimeConfig {
        scroll_anchor: ScrollAnchorConfig { x: 1.5, y: -0.2 },
        ..RuntimeConfig::default()
    };
    assert!(!config.scroll_anchor.is_valid());
    let runner = config.to_runner_config();
    assert_eq!(runner.scroll_anchor, RelativePoint::new(1.5, -0.2));
    assert!(runner.scroll_anchor.validate().is_err());
}

/// 「滚动停稳等待」的默认值必须是个**有限的上限**，而不是 0（0 = 不等，
/// 等于把缓动动画的中间帧直接喂给 OCR），也不是一个大到让每滚一步都要
/// 等上几秒的数——它是上限，正常开销只是多截一帧。
#[test]
fn scroll_settle_defaults_to_a_small_bounded_wait() {
    let config = RuntimeConfig::default();
    assert_eq!(config.scroll_settle_ms, 600);
    let runner = config.to_runner_config();
    assert_eq!(runner.scroll_settle_timeout, Duration::from_millis(600));
    assert!(!runner.scroll_settle_timeout.is_zero());
    assert!(runner.scroll_settle_timeout <= Duration::from_secs(2));
}

/// 毫秒值原样换算成 `Duration`，**不夹到某个区间**：
/// 操作者填 0 就是明确要求"不等"，不能替他改主意。
#[test]
fn scroll_settle_ms_is_converted_verbatim() {
    let config = RuntimeConfig { scroll_settle_ms: 0, ..RuntimeConfig::default() };
    assert!(config.to_runner_config().scroll_settle_timeout.is_zero());

    let config = RuntimeConfig { scroll_settle_ms: 1500, ..RuntimeConfig::default() };
    assert_eq!(
        config.to_runner_config().scroll_settle_timeout,
        Duration::from_millis(1500)
    );
}

/// 默认要**记录识别结果**：排查"找不到联系人"时，没有这一项就分不清
/// 是 OCR 读错了还是名字根本不在这屏，两者的处置完全相反。
#[test]
fn reading_back_the_ocr_text_is_on_by_default() {
    let config = RuntimeConfig::default();
    assert!(config.log_ocr_candidates);
    assert!(config.to_runner_config().log_ocr_candidates);

    let config = RuntimeConfig { log_ocr_candidates: false, ..RuntimeConfig::default() };
    assert!(!config.to_runner_config().log_ocr_candidates);
}

/// 「放宽姓名匹配」这个开关必须**真的**换掉匹配器，两个方向都要验。
///
/// 为什么值得单独钉：选错匹配器**不报任何错**，只在现场表现为
/// 「它怎么点到别人身上去了」或者「明明在列表里却说找不到」。
/// 这里用实测过的那条数据（OCR 把头像红点并进了姓名行，读出 `0 李四`）。
#[test]
fn the_relaxed_switch_actually_selects_the_lenient_matcher() {
    use automation_core::{Rect, TextBox};

    let noisy = vec![TextBox {
        text: "0 李四".into(),
        bounds: Rect { x: 4, y: 29, width: 38, height: 19 },
        confidence: 1.0,
    }];

    let lenient = build_matcher(true);
    let found = lenient
        .find_unique_exact_match("李四", &noisy, DEFAULT_MIN_CONFIDENCE)
        .expect("放宽模式下应当认出「0 李四」里的「李四」");
    assert_eq!(found.text, "0 李四");

    let strict = build_matcher(false);
    assert!(
        strict.find_unique_exact_match("李四", &noisy, DEFAULT_MIN_CONFIDENCE).is_err(),
        "关掉开关就必须回到架构要求的逐字精确匹配"
    );
}

/// 默认**开**着放宽匹配——这是操作者当下要的「先把链路跑通」。
///
/// 顺带把「它只是临时措施」这件事钉在用例里：将来收紧默认值时，
/// 这条用例会失败，逼着改的人回来读一遍 `ContainsNameMatcher` 的文档。
#[test]
fn relaxed_name_matching_is_on_by_default_for_now() {
    assert!(RuntimeConfig::default().relaxed_name_match);
}

/// 导航图标这一组的默认值必须与核心层的常量逐字段一致，并且**默认关**。
///
/// 默认关的理由要钉住：这一步需要操作者自己截一张图标模板，
/// 默认打开等于让每个还没准备模板的人都在点「开始任务」时撞上一次装配错误。
#[test]
fn navigation_defaults_come_from_the_core_constants_and_are_off() {
    let config = RuntimeConfig::default();
    assert!(!config.navigate_before_search);
    assert!(config.nav_icon_templates.is_empty());
    assert_eq!(config.nav_icon_min_score, DEFAULT_NAV_ICON_MIN_SCORE);
    assert_eq!(config.nav_strip, flatten(DEFAULT_NAV_STRIP));
    // 图标库目录默认留空 = 项目根下的 `data/icons/`（见 `icon_library::resolve_dir`）。
    assert!(config.icons_dir.is_none());

    let runner = config.to_runner_config();
    assert_eq!(runner.nav_strip, DEFAULT_NAV_STRIP);
    assert_eq!(runner.nav_icon_min_score, DEFAULT_NAV_ICON_MIN_SCORE);
    assert!(runner.nav_icon_templates.is_empty(), "模板由装配期载入，不在这个转换里");
    assert!(!runner.navigate_before_search);
}

/// 造一条标定记录。窗口取一个固定值——这些用例不关心它，只要求"标过"。
fn mark(rect: [f32; 4]) -> calibration::AreaMark {
    calibration::AreaMark {
        rect,
        calibrated_at_ms: 1_700_000_000_000,
        window: automation_core::Rect { x: 0, y: 0, width: 960, height: 734 },
    }
}

/// 装配用例的基线配置：任意产品工作流均可，不依赖标定/图标。
///
/// YOLO 路径下 `required_marks` 为空、装配不再载入导航模板，所以基线
/// 不再预埋 `send_button` 区域或 `chat_history_nav_templates`。
fn base_config() -> RuntimeConfig {
    RuntimeConfig {
        workflow: Workflow::ChatListSend,
        ..RuntimeConfig::default()
    }
}

/// YOLO 产品工作流装配时**不**要求界面标定、区域框选或导航图标模板。
#[test]
fn yolo_workflows_assemble_without_calibrations_marks_or_nav_icons() {
    let icons = temp_icons_dir("yolo-no-calib");
    for workflow in [Workflow::ChatListSend, Workflow::ContactsSearchSend] {
        let config = RuntimeConfig {
            workflow,
            calibrations: Vec::new(),
            calibrated_window: None,
            area_marks: calibration::AreaMarks::new(),
            nav_icon_templates: Vec::new(),
            chat_history_nav_templates: Vec::new(),
            ..RuntimeConfig::default()
        };
        let runner = assemble_with_icons(&config, &icons)
            .unwrap_or_else(|err| panic!("{workflow:?} 无标定也应装配成功：{err}"));
        assert!(runner.config().calibrated_window.is_none());
        assert!(runner.config().calibration_alts.is_empty());
        assert_eq!(runner.config().workflow, workflow);
        assert!(runner.config().nav_icon_templates.is_empty());
    }
}

/// 即便配置里还留着旧窗口标定，交给运行器的几何也必须被清空——
/// 否则 `ensure_calibrated_size` 会按旧标注尺寸强行改窗。
#[test]
fn build_runner_clears_calibrated_geometry_even_when_config_has_snapshots() {
    let icons = temp_icons_dir("clear-geometry");
    let mut config = RuntimeConfig {
        workflow: Workflow::ContactsSearchSend,
        calibrated_window: Some(WindowGeometry {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            scale_factor: 1.0,
        }),
        ..RuntimeConfig::default()
    };
    // 塞一份快照，模拟「界面标定」页曾经保存过。
    config.ensure_calibrations_migrated();
    assert!(!config.calibrations.is_empty(), "前提：配置里有标定快照");

    let runner = assemble_with_icons(&config, &icons).expect("有旧标定也不该挡住装配");
    assert!(
        runner.config().calibrated_window.is_none(),
        "运行器不得携带标定窗口"
    );
    assert!(
        runner.config().calibration_alts.is_empty(),
        "运行器不得携带备选标定"
    );
}

/// 区域若已标，`to_runner_config` 仍应原样带上（标定页数据还在）；
/// 只是装配门槛不再要求它们。
#[test]
fn marked_regions_still_map_into_to_runner_config() {
    let mut config = base_config();
    config.area_marks.insert("main_search".into(), mark([0.10, 0.02, 0.60, 0.05]));
    config.area_marks.insert("search_dropdown".into(), mark([0.10, 0.07, 0.60, 0.50]));
    config.area_marks.insert("contact_profile".into(), mark([0.72, 0.05, 0.27, 0.90]));
    config.area_marks.insert("send_button".into(), mark([0.86, 0.90, 0.12, 0.07]));

    let runner = config.to_runner_config();
    assert!(runner.main_search.is_some());
    assert!(runner.search_dropdown.is_some());
    assert!(runner.contact_profile.is_some());
    assert!(runner.send_button.is_some());
    assert!(runner.nav_bar.is_none(), "没标的区域不该凭空出现");
}

/// ★ 回归：工作流是运行参数，不是配置里的默认值。
#[test]
fn the_run_choice_decides_the_workflow_not_the_config() {
    let icons = temp_icons_dir("run-choice-wins");
    let config = RuntimeConfig {
        workflow: Workflow::ChatListSend,
        ..RuntimeConfig::default()
    };
    let choice = RunChoice {
        mode: config.mode,
        workflow: Workflow::ContactsSearchSend,
        nav_target: String::new(),
    };
    let runner = build_runner(
        &config,
        &choice,
        &sample_task(),
        &icons,
        Arc::new(MemoryAudit::new()),
        Arc::new(MemorySendLedger::new()),
    )
    .expect("运行参数是通讯录搜索式时应当装配成功");
    assert_eq!(
        runner.config().workflow,
        Workflow::ContactsSearchSend,
        "运行器里那条路必须来自运行参数"
    );
}

/// ★ 回归：模式也是运行参数；YOLO 路径下真实模式**不再**要求窗口标定。
#[test]
fn the_run_choice_decides_the_mode_not_the_config() {
    let icons = temp_icons_dir("run-choice-mode");
    let runner_of = |config: &RuntimeConfig, choice: RunChoice| {
        build_runner(
            config,
            &choice,
            &sample_task(),
            &icons,
            Arc::new(MemoryAudit::new()),
            Arc::new(MemorySendLedger::new()),
        )
    };

    // ── 方向一：配置是演练、请求要真实、没有任何标定 ──────────────
    let dry_config = RuntimeConfig {
        mode: RuntimeMode::DryRun,
        calibrated_window: None,
        calibrations: Vec::new(),
        workflow: Workflow::ChatListSend,
        ..RuntimeConfig::default()
    };
    let live_runner = runner_of(
        &dry_config,
        RunChoice {
            mode: RuntimeMode::Live,
            workflow: Workflow::ChatListSend,
            nav_target: String::new(),
        },
    )
    .expect("请求要真实模式时，无标定也应装配成功（YOLO 路径）");
    assert_ne!(
        live_runner.config().platform_label,
        "dry-run",
        "审计平台必须跟着运行参数走，不能仍是 dry-run"
    );
    assert!(
        live_runner.config().calibrated_window.is_none(),
        "YOLO 真实模式也不得携带标定窗口"
    );
    assert!(live_runner.config().calibration_alts.is_empty());

    // ── 方向二：配置是真实（且记了标定尺寸）、请求要演练 ────────────
    let live_config = RuntimeConfig {
        mode: RuntimeMode::Live,
        calibrated_window: Some(WindowGeometry {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            scale_factor: 1.0,
        }),
        workflow: Workflow::ContactsSearchSend,
        ..RuntimeConfig::default()
    };
    let runner = runner_of(
        &live_config,
        RunChoice {
            mode: RuntimeMode::DryRun,
            workflow: Workflow::ContactsSearchSend,
            nav_target: String::new(),
        },
    )
    .expect("请求是演练模式时应当装配成功");
    assert_eq!(
        runner.config().platform_label,
        "dry-run",
        "审计里记的平台必须跟着运行参数走"
    );
    assert!(
        runner.config().calibrated_window.is_none(),
        "演练模式跑的是替身窗口，不该带标定窗口"
    );
}

/// 靶标文字留空不再挡住装配：YOLO 路径用检测类 / 流程内 OCR，
/// 这些字符串的默认值已在 `RunnerConfig`；空串顶多影响旧的文字匹配旁路。
#[test]
fn blank_target_texts_no_longer_block_assembly() {
    let icons = temp_icons_dir("blank-text-ok");
    let mut config = base_config();
    config.profile_chat_entry_text = "  ".into();
    config.search_contact_group_label = String::new();
    config.send_button_text = String::new();
    assemble_with_icons(&config, &icons).expect("空靶标文字不应再拒绝装配");
}

/// 非法滚动落点不再在装配期拒绝：YOLO 主路径不靠它定位；
/// `to_runner_config` 仍原样透传，由核心层在真用到时再报。
#[test]
fn an_out_of_range_scroll_anchor_no_longer_blocks_assembly() {
    let icons = temp_icons_dir("bad-anchor-ok");
    let config = RuntimeConfig {
        scroll_anchor: ScrollAnchorConfig { x: 1.5, y: -0.2 },
        ..base_config()
    };
    assemble_with_icons(&config, &icons).expect("非法 scroll_anchor 不应再拒绝装配");
}

/// 默认工作流是会话列表发送；新增区域默认都没标（不猜坐标）。
#[test]
fn the_default_workflow_regions_have_no_defaults() {
    let config = RuntimeConfig::default();
    assert_eq!(config.workflow, Workflow::ChatListSend);
    assert!(
        config.nav_target.is_empty(),
        "默认没选过图标——不兜底挑一个"
    );
    assert!(config.area_marks.is_empty(), "默认一个新增区域都没标");
    assert_eq!(config.icon_prior_score_tolerance, DEFAULT_ICON_PRIOR_SCORE_TOLERANCE);
    assert_eq!(config.typing_interval_ms, 30);

    let runner = config.to_runner_config();
    assert!(runner.main_search.is_none());
    assert!(runner.search_dropdown.is_none());
    assert!(runner.contact_profile.is_none());
    assert!(runner.nav_bar.is_none());
    assert_eq!(runner.profile_chat_entry_text, DEFAULT_PROFILE_CHAT_ENTRY_TEXT);
    assert_eq!(runner.search_contact_group_label, DEFAULT_SEARCH_CONTACT_GROUP_LABEL);
    assert_eq!(runner.profile_scroll_anchor, DEFAULT_PROFILE_SCROLL_ANCHOR);
}

/// 资料页的滚动落点是**核心层的常量**，不是这一层另写的一份。
#[test]
fn the_profile_scroll_anchor_comes_from_the_core_constant() {
    let runner = RuntimeConfig::default().to_runner_config();
    assert_eq!(runner.profile_scroll_anchor, DEFAULT_PROFILE_SCROLL_ANCHOR);
    assert_eq!(runner.profile_scroll_anchor.x, 0.5);
    assert!(runner.profile_scroll_anchor.validate().is_ok());
}

/// 产品工作流的 `required_marks` 必须是空表（与装配门槛一致）。
#[test]
fn product_workflows_require_no_calibration_marks() {
    let config = RuntimeConfig::default();
    for workflow in Workflow::ALL {
        let req = workflow_requirements(&config)
            .into_iter()
            .find(|item| item.workflow == workflow)
            .expect("每条工作流都要有一份要求");
        assert!(
            req.required.is_empty(),
            "{workflow:?} 不应再要求标定区域，实际 {:?}",
            req.required
        );
    }
}

/// ★ 任务输入按工作流校验，而**判据只有一处**（`workflow_inputs`）。
#[test]
fn task_inputs_are_decided_by_the_workflow() {
    for (workflow, contact, message) in [
        (Workflow::ChatListSend, true, true),
        (Workflow::ContactsSearchSend, true, true),
    ] {
        let inputs = workflow_inputs(workflow);
        assert_eq!(inputs.contact, contact, "{workflow:?} 的联系人要求");
        assert_eq!(inputs.message, message, "{workflow:?} 的正文要求");

        let requirement = workflow_requirements(&RuntimeConfig::default())
            .into_iter()
            .find(|item| item.workflow == workflow)
            .expect("每条工作流都要有一份要求");
        assert_eq!(requirement.needs_contact, contact, "{workflow:?} 下发的要求");
        assert_eq!(requirement.needs_message, message, "{workflow:?} 下发的要求");
    }
}
