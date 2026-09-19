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
    build_runner(
        config,
        &sample_task(),
        icons_dir,
        Arc::new(MemoryAudit::new()),
        Arc::new(MemorySendLedger::new()),
        Arc::new(MockHumanConfirmation::default()),
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
/// `0 丁俊`，取 0.14 时读到干净的 `明月（美、加、欧洲）清库存`、`丁俊`。
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
/// 这里用实测过的那条数据（OCR 把头像红点并进了姓名行，读出 `0 丁俊`）。
#[test]
fn the_relaxed_switch_actually_selects_the_lenient_matcher() {
    use automation_core::{Rect, TextBox};

    let noisy = vec![TextBox {
        text: "0 丁俊".into(),
        bounds: Rect { x: 4, y: 29, width: 38, height: 19 },
        confidence: 1.0,
    }];

    let lenient = build_matcher(true);
    let found = lenient
        .find_unique_exact_match("丁俊", &noisy, DEFAULT_MIN_CONFIDENCE)
        .expect("放宽模式下应当认出「0 丁俊」里的「丁俊」");
    assert_eq!(found.text, "0 丁俊");

    let strict = build_matcher(false);
    assert!(
        strict.find_unique_exact_match("丁俊", &noisy, DEFAULT_MIN_CONFIDENCE).is_err(),
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

/// 装配用例的基线配置：**列表扫描式**。
///
/// 为什么要显式选：`RuntimeConfig::default()` 现在是**搜索式**，而搜索式
/// 要求先标好搜索框 / 下拉 / 资料页三块。下面这些用例验的是导航图标那一组，
/// 与工作流无关；沿用默认值会让它们全部卡在「缺标定区域」上，
/// 而那条报错看起来像是导航配置有问题——排查方向就歪了。
fn base_config() -> RuntimeConfig {
    RuntimeConfig { workflow: Workflow::ScrollListContact, ..RuntimeConfig::default() }
}

/// 打开开关却没配图标 ⇒ **装配期**就拒绝，而不是留一条跑到一半才失败的记录。
#[test]
fn turning_on_navigation_without_a_template_is_refused() {
    let icons = temp_icons_dir("no-template");
    let config = RuntimeConfig {
        navigate_before_search: true,
        ..base_config()
    };
    let err = assemble_with_icons(&config, &icons)
        .err()
        .expect("没有图标时必须拒绝装配");
    // 报错要说清是**哪一组**模板缺了：两个目标各有一组，不说清就会去改错的那组。
    assert!(err.contains("没有配置「联系人」图标的模板"), "{err}");
    assert!(err.contains("图标库"), "要告诉人下一步去哪配：{err}");

    // 只填空白也算没配——不然会变成"名字叫空字符串"这种更难查的错。
    let config = RuntimeConfig {
        navigate_before_search: true,
        nav_icon_templates: vec!["   ".into(), String::new()],
        ..base_config()
    };
    assert!(assemble_with_icons(&config, &icons).is_err());

    // 名字写了个库里没有的：也要在装配期说清楚，而不是等到匹配的时候。
    let config = RuntimeConfig {
        navigate_before_search: true,
        nav_icon_templates: vec!["聊天".into()],
        ..base_config()
    };
    let err = assemble_with_icons(&config, &icons).err().expect("名字对不上要拒绝");
    assert!(err.contains("聊天"), "{err}");
}

/// 一个名字底下的**全部**变体都要被带进运行器。
///
/// 这是"选中 / 未选中 / 带气泡"能同时生效的前提：只载入第一张的话，
/// 界面停在选中态时照样匹配不上，而失败现象和"模板没配"一模一样。
#[test]
fn every_variant_of_an_icon_reaches_the_runner() {
    let icons = temp_icons_dir("variants");
    write_icon(&icons, "聊天", 3, 20, 18);
    let config = RuntimeConfig {
        navigate_before_search: true,
        nav_icon_templates: vec!["聊天".into(), "  ".into()],
        ..base_config()
    };

    let runner = assemble_with_icons(&config, &icons).expect("应当装配成功");
    let templates = &runner.config().nav_icon_templates;
    assert_eq!(templates.len(), 3, "三张变体都要载入，空白项忽略");
    assert_eq!(
        templates.iter().map(|t| t.label.clone()).collect::<Vec<_>>(),
        ["聊天/1.png", "聊天/2.png", "聊天/3.png"],
        "label 取相对图标库目录的路径：失败信息里要能一眼看出是哪个图标的哪一张"
    );
    for template in templates {
        assert_eq!((template.width, template.height), (20, 18));
    }
}

/// 模板本身不可用时也要在装配期报错。
///
/// 关键在"什么时候报"：留到运行期的话，症状是"匹配分数很低"，
/// 而人只会去怀疑阈值，不会想到"这张图根本不是图标"。
#[test]
fn an_unusable_template_is_refused_at_assembly_time() {
    let icons = temp_icons_dir("unusable");
    write_icon(&icons, "太大", 1, 300, 40);
    let config = RuntimeConfig {
        navigate_before_search: true,
        nav_icon_templates: vec!["太大".into()],
        ..base_config()
    };
    let err = assemble_with_icons(&config, &icons).err().expect("过大的模板必须被拒绝");
    assert!(err.contains("太大"), "报错要说清是尺寸问题：{err}");

    // 三张里有一张坏的 ⇒ 整个图标不能用。留着它会在运行时表现为"某个状态匹配不上"。
    let icons = temp_icons_dir("unusable-one-of-three");
    write_icon(&icons, "聊天", 3, 20, 18);
    std::fs::write(icons.join("聊天").join("2.png"), b"not a png").unwrap();
    let config = RuntimeConfig {
        navigate_before_search: true,
        nav_icon_templates: vec!["聊天".into()],
        ..base_config()
    };
    let err = assemble_with_icons(&config, &icons).err().expect("坏的那张必须被拒绝");
    assert!(err.contains("聊天/2.png"), "要说清是哪个图标的哪一张：{err}");
}

/// 搜索区比例非法 / 阈值越界，同样在装配期拦下。
#[test]
fn an_illegal_nav_strip_or_threshold_is_refused() {
    let icons = temp_icons_dir("illegal");
    write_icon(&icons, "聊天", 1, 20, 18);

    let config = RuntimeConfig {
        navigate_before_search: true,
        nav_icon_templates: vec!["聊天".into()],
        nav_strip: [0.0, 0.0, 1.5, 1.0],
        ..base_config()
    };
    let err = assemble_with_icons(&config, &icons).err().expect("比例越界必须被拒绝");
    assert!(err.contains("导航图标搜索区"), "{err}");

    let config = RuntimeConfig {
        navigate_before_search: true,
        nav_icon_templates: vec!["聊天".into()],
        nav_icon_min_score: 1.4,
        ..base_config()
    };
    let err = assemble_with_icons(&config, &icons).err().expect("阈值越界必须被拒绝");
    assert!(err.contains("0–1"), "{err}");
}

/// 关着的时候不该去读图标：名字写错了也不该拦住任务。
#[test]
fn icons_are_not_touched_while_navigation_is_off() {
    let icons = temp_icons_dir("off");
    let config = RuntimeConfig {
        navigate_before_search: false,
        nav_icon_templates: vec!["根本不存在的图标".into()],
        nav_strip: [0.0, 0.0, 9.0, 9.0],
        ..base_config()
    };
    let runner = assemble_with_icons(&config, &icons).expect("关着的时候这些配置项都不该被读");
    assert!(runner.config().nav_icon_templates.is_empty());
}

// ── 工作流：该要求哪些标定区域 ──────────────────────────────────

/// 搜索式缺区域 ⇒ 装配期拒绝，并且**说清缺的是哪几块**。
///
/// 为什么要按工作流分别要求：搜索式要的三块（搜索框 / 下拉 / 资料页）
/// 列表扫描式一块都不用。一概全要等于让人去标三块永远走不到的区域；
/// 一概不要则会让搜索式走到一半才转人工，而那时任务已经登记进列表了。
#[test]
fn the_search_workflow_refuses_to_start_without_its_regions() {
    let icons = temp_icons_dir("search-regions");
    let config = RuntimeConfig {
        workflow: Workflow::SearchContact,
        ..RuntimeConfig::default()
    };
    let err = assemble_with_icons(&config, &icons)
        .err()
        .expect("搜索式缺三块区域时必须拒绝装配");

    // 报错要说清"哪条工作流、缺哪几块"，并且用界面上的说法（label）而不是 key：
    // 只报 `main_search` 的话，人得自己把它翻译成标定页里的某一项。
    assert!(err.contains("搜索式查找联系人"), "{err}");
    for label in ["搜索框区", "下拉列表区域", "联系人资料区域"] {
        assert!(err.contains(label), "缺哪块要说清（{label}）：{err}");
    }
    assert!(err.contains("界面标定"), "要告诉人下一步去哪标：{err}");
    assert!(err.contains("3"), "要说清缺了几块：{err}");
}

/// 反过来：三块标好了就能装配，而且它们**真的**被带进了运行器。
///
/// 只验"不报错"是不够的——`mark_region` 写错了键（比如把 `main_search`
/// 写成 `regions.main_search`）时装配照样成功，只是运行器里是 `None`，
/// 症状是"跑到那一步才转人工"。所以这里逐项核对。
#[test]
fn the_search_regions_reach_the_runner_after_they_are_marked() {
    let icons = temp_icons_dir("search-regions-ok");
    let mark = |rect: [f32; 4]| calibration::AreaMark {
        rect,
        calibrated_at_ms: 1_700_000_000_000,
        window: automation_core::Rect { x: 0, y: 0, width: 960, height: 734 },
    };
    let mut config = RuntimeConfig {
        workflow: Workflow::SearchContact,
        ..RuntimeConfig::default()
    };
    config.area_marks.insert("main_search".into(), mark([0.10, 0.02, 0.60, 0.05]));
    config.area_marks.insert("search_dropdown".into(), mark([0.10, 0.07, 0.60, 0.50]));
    config.area_marks.insert("contact_profile".into(), mark([0.72, 0.05, 0.27, 0.90]));

    let runner = assemble_with_icons(&config, &icons).expect("三块都标了应当装配成功");
    let runner_config = runner.config();
    assert!(runner_config.main_search.is_some(), "搜索框区没被带进运行器");
    assert!(runner_config.search_dropdown.is_some(), "下拉区域没被带进运行器");
    assert!(runner_config.contact_profile.is_some(), "资料区域没被带进运行器");
    // 没标的那一项（导航区只用来算位置先验的中心）**必须仍是 `None`**：
    // 给一个猜出来的中心，症状是"先验把命中往一个错的方向拉"。
    assert!(runner_config.nav_bar.is_none(), "没标的区域不该凭空出现");
}

/// 列表扫描式**不**需要搜索式那三块——这是"按工作流分别要求"的另一半。
///
/// 没有这条，上面那条用例可以被"一概全要"糊弄过去，而代价是
/// 每个只想跑列表式的人都被逼着去标三块用不上的区域。
#[test]
fn the_list_workflow_does_not_need_the_search_regions() {
    let icons = temp_icons_dir("list-no-search-regions");
    let config = RuntimeConfig {
        workflow: Workflow::ScrollListContact,
        ..RuntimeConfig::default()
    };
    let runner = assemble_with_icons(&config, &icons).expect("列表式不该要求搜索式的区域");
    assert!(runner.config().main_search.is_none());
}

/// 「只做导航」需要**它自己那个目标**的模板，而不是联系人那一组。
///
/// 两个目标各有一组模板，混用不会报错——只会拿聊天历史的模板去匹配联系人图标，
/// 然后转人工。所以这里把"目标 → 模板组"这条对应关系钉死。
#[test]
fn navigate_only_loads_the_template_group_of_its_own_target() {
    let icons = temp_icons_dir("navigate-only");
    write_icon(&icons, "聊天历史", 2, 20, 18);
    write_icon(&icons, "联系人", 1, 20, 18);

    // 目标 = 聊天历史，只配了聊天历史的模板 ⇒ 用 history 那一组。
    let config = RuntimeConfig {
        workflow: Workflow::NavigateOnly,
        nav_target: NavTarget::History,
        history_icon_templates: vec!["聊天历史".into()],
        // 刻意**不**配联系人的那一组：不该被读到。
        ..RuntimeConfig::default()
    };
    let runner = assemble_with_icons(&config, &icons).expect("导航就是任务本身，不受开关约束");
    let runner_config = runner.config();
    assert_eq!(runner_config.history_icon_templates.len(), 2, "两张变体都要载入");
    assert!(runner_config.nav_icon_templates.is_empty(), "不该去读联系人那一组");

    // 目标 = 聊天历史，却只配了联系人的模板 ⇒ 装配期就拒绝。
    let config = RuntimeConfig {
        workflow: Workflow::NavigateOnly,
        nav_target: NavTarget::History,
        nav_icon_templates: vec!["联系人".into()],
        ..RuntimeConfig::default()
    };
    let err = assemble_with_icons(&config, &icons)
        .err()
        .expect("配错了一组必须拒绝，否则会在运行期表现为匹配分数很低");
    assert!(err.contains("没有配置「聊天历史」图标的模板"), "{err}");

    // 「只做导航」是任务本身，所以 `navigate_before_search` 关着也照样要求模板。
    let config = RuntimeConfig {
        workflow: Workflow::NavigateOnly,
        nav_target: NavTarget::Contact,
        navigate_before_search: false,
        ..RuntimeConfig::default()
    };
    let err = assemble_with_icons(&config, &icons)
        .err()
        .expect("关着开关也要模板：导航就是这条工作流的全部内容");
    assert!(err.contains("没有配置「联系人」图标的模板"), "{err}");
}

/// 靶标文字留空 ⇒ 装配期拒绝。
///
/// 空串在「包含」判断里**匹配一切**：空的分组标题会让下拉里的第一行被当成
/// 「联系人」组的标题，于是后面整段判据全部错位。而这**不会报错**——
/// 只会表现为"点到了不相干的一行"。属于"配置写错了"，所以在装配期拦。
#[test]
fn blank_target_texts_are_refused() {
    let icons = temp_icons_dir("blank-text");
    let mut config = RuntimeConfig {
        workflow: Workflow::ScrollListContact,
        ..RuntimeConfig::default()
    };
    config.profile_chat_entry_text = "  ".into();
    let err = assemble_with_icons(&config, &icons).err().expect("空文字必须被拒绝");
    assert!(err.contains("不能留空"), "{err}");
    assert!(err.contains("profile_chat_entry_text"), "要说清是哪一个字段：{err}");

    let mut config = RuntimeConfig {
        workflow: Workflow::ScrollListContact,
        ..RuntimeConfig::default()
    };
    config.search_contact_group_label = String::new();
    let err = assemble_with_icons(&config, &icons).err().expect("空标题必须被拒绝");
    assert!(err.contains("search_contact_group_label"), "{err}");
}

/// 默认值必须自洽：默认那条工作流（搜索式）要的三块区域，默认配置里**没有**。
///
/// 这不是缺陷，是如实反映现状——那些区域只有对着真实窗口框一次才知道在哪儿。
/// 钉住它，是为了让"给它们补一个猜出来的默认值"这种改法在改的当场就失败：
/// 猜出来的默认值不会让任何东西报错，只会让任务点到一个错的地方。
#[test]
fn the_default_workflow_is_search_and_its_regions_have_no_defaults() {
    let config = RuntimeConfig::default();
    assert_eq!(config.workflow, Workflow::SearchContact);
    assert_eq!(config.nav_target, NavTarget::Contact);
    assert!(config.area_marks.is_empty(), "默认一个新增区域都没标");
    assert!(config.history_icon_templates.is_empty());
    assert_eq!(config.icon_prior_score_tolerance, DEFAULT_ICON_PRIOR_SCORE_TOLERANCE);
    assert_eq!(config.typing_interval_ms, 30);

    // 转成运行器之后仍然是 `None`——**不兜底**。
    let runner = config.to_runner_config();
    assert!(runner.main_search.is_none());
    assert!(runner.search_dropdown.is_none());
    assert!(runner.contact_profile.is_none());
    assert!(runner.nav_bar.is_none());
    // 这两个是常量派生的，不是"标定出来的"，所以一定有值。
    assert_eq!(runner.profile_chat_entry_text, DEFAULT_PROFILE_CHAT_ENTRY_TEXT);
    assert_eq!(runner.search_contact_group_label, DEFAULT_SEARCH_CONTACT_GROUP_LABEL);
    assert_eq!(runner.profile_scroll_anchor, DEFAULT_PROFILE_SCROLL_ANCHOR);
}

/// 资料页的滚动落点是**核心层的常量**，不是这一层另写的一份。
///
/// 与 `scroll_anchor` 不同，它**不开放配置**：资料页是一整块可滚动内容，
/// 正中一定落在内容上，没有需要避开的列。多一个旋钮就多一处会被设错的地方。
#[test]
fn the_profile_scroll_anchor_comes_from_the_core_constant() {
    let runner = RuntimeConfig::default().to_runner_config();
    assert_eq!(runner.profile_scroll_anchor, DEFAULT_PROFILE_SCROLL_ANCHOR);
    assert_eq!(runner.profile_scroll_anchor.x, 0.5);
    assert!(runner.profile_scroll_anchor.validate().is_ok());
}

/// 位置先验的容差配成负数 ⇒ 装配期拒绝。
///
/// 负容差在核心层等于"没有任何候选够得着最高分减负数"，
/// 也就是先验永远不生效——而它**不报错**，只是悄悄地什么都不做。
#[test]
fn a_negative_prior_tolerance_is_refused() {
    let icons = temp_icons_dir("negative-tolerance");
    write_icon(&icons, "聊天", 1, 20, 18);
    let config = RuntimeConfig {
        navigate_before_search: true,
        nav_icon_templates: vec!["聊天".into()],
        icon_prior_score_tolerance: -0.1,
        ..base_config()
    };
    let err = assemble_with_icons(&config, &icons).err().expect("负容差必须被拒绝");
    assert!(err.contains("位置先验"), "{err}");

    // 0 是**合法**的：它表示"关掉先验"，是一个明确的配置意图。
    let config = RuntimeConfig {
        navigate_before_search: true,
        nav_icon_templates: vec!["聊天".into()],
        icon_prior_score_tolerance: 0.0,
        ..base_config()
    };
    assert!(assemble_with_icons(&config, &icons).is_ok(), "0 = 关掉先验，不是错误");
}
