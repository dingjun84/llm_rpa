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
    // 报错要点名**缺的是哪个图标**：图标库里通常有四五个目录，
    // 不说清就等于让人自己去猜该配哪一个。
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

/// ★ 回归用例：**判据是运行参数，不是配置里的那个默认值**。
///
/// 2026-09-19 的 bug —— 界面上把工作流切成别的，跑的还是配置里那条：
/// 连跑三条任务，三条 `task-*.log` 里记的全是 `SearchContact`。
/// 修法是把工作流变成运行参数（[`RunChoice`]），这条用例钉的就是
/// "装配期到底读的是哪一个"。
///
/// 手法：配置里放**搜索式**（默认值，且默认配置一块新增区域都没标 ⇒ 装配必拒），
/// 运行参数放**列表扫描式**（一块都不需要 ⇒ 应当装配成功）。
/// 装配成功即证明它读的是运行参数；一旦有人把它改回去读配置，这里立刻红。
#[test]
fn the_run_choice_decides_the_workflow_not_the_config() {
    let icons = temp_icons_dir("run-choice-wins");
    let config = RuntimeConfig {
        workflow: Workflow::SearchContact,
        ..RuntimeConfig::default()
    };
    // 前提：这份配置按它自己的 `workflow` 装配不起来（缺三块区域）。
    // 前提不成立的话，下面那条断言什么都证明不了。
    assert!(
        assemble_with_icons(&config, &icons).is_err(),
        "前提：默认配置按搜索式装配应当被拒（缺三块区域）"
    );

    let choice = RunChoice {
        mode: config.mode,
        workflow: Workflow::ScrollListContact,
        // 这条工作流不导航，所以这一项没有意义——填空串，别让它看起来像"选好了"。
        nav_target: String::new(),
    };
    let runner = build_runner(
        &config,
        &choice,
        &sample_task(),
        &icons,
        Arc::new(MemoryAudit::new()),
        Arc::new(MemorySendLedger::new()),
        Arc::new(MockHumanConfirmation::default()),
    )
    .expect("运行参数是列表式时，不该按配置里的搜索式去要求那三块区域");

    // 不只看"装配没报错"：运行器里带的那条路也必须是运行参数给的那条，
    // 否则会出现"装配按 A 检查、真正跑的是 B"。
    assert_eq!(
        runner.config().workflow,
        Workflow::ScrollListContact,
        "运行器里那条路必须来自运行参数"
    );
    // 这条路不导航，所以不该带上任何图标模板：带上了就说明有人把配置里那组
    // "联系人视图"图标无条件塞了进来——那会在别的场合让任务先点一个不该点的图标。
    assert!(runner.config().nav_icon_templates.is_empty());
}

/// ★ 回归用例：**模式也是运行参数**，不是配置里的那个默认值。
///
/// 与工作流那条同一个坑（界面上切成别的、跑的还是配置里那个），但后果更重：
/// 模式不只是"换一组端口"，它还决定**要不要带标定窗口**、以及**审计里记哪个平台**。
/// 两个方向都要钉住：
///
/// - 请求说**真实**、配置是演练 ⇒ 必须按真实校验：没有标定尺寸就拒；
/// - 请求说**演练**、配置是真实 ⇒ 必须按演练跑：不要求标定尺寸，
///   而且核心层拿到的标定窗口必须是 `None`、平台必须是 `dry-run`。
///
/// 后一个方向更危险——以为在演练、其实在真实客户端上操作，
/// 而审计里还记着 `dry-run`，事后根本查不出来。
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
            Arc::new(MockHumanConfirmation::default()),
        )
    };

    // ── 方向一：配置是演练、请求要真实 ──────────────────────────────
    let dry_config = RuntimeConfig {
        mode: RuntimeMode::DryRun,
        calibrated_window: None,
        workflow: Workflow::ScrollListContact,
        ..RuntimeConfig::default()
    };
    // 不用 `expect_err`：`WorkflowRunner` 没有实现 `Debug`，报不出成功那个值。
    let err = match runner_of(
        &dry_config,
        RunChoice {
            mode: RuntimeMode::Live,
            workflow: Workflow::ScrollListContact,
            nav_target: String::new(),
        },
    ) {
        Err(err) => err,
        Ok(_) => panic!("请求要真实模式时，没有标定尺寸必须被拒——否则说明它读的是配置里的演练"),
    };
    assert!(
        err.contains("记录窗口尺寸"),
        "拒绝理由要指向那条记录（否则操作者不知道该做什么）：{err}"
    );

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
        workflow: Workflow::ScrollListContact,
        ..RuntimeConfig::default()
    };
    let runner = runner_of(
        &live_config,
        RunChoice {
            mode: RuntimeMode::DryRun,
            workflow: Workflow::ScrollListContact,
            nav_target: String::new(),
        },
    )
    .expect("请求是演练模式时，不该按配置里的真实模式去要求标定尺寸");

    // 不只看"装配没报错"：交出去的那份必须真的是演练语义，
    // 否则会出现"按演练装配、真正跑的是真实"。
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

/// 「只做导航」点的那个图标由**图标库里的目录名**指定，目录下的全部图都要载入。
///
/// 这条路曾经是"两个写死的目标各配一组模板"，于是图标库里四五个图标在下拉里
/// 根本选不出来（想测「收藏夹」都没得选）。现在目标就是图标库里的名字，
/// 这里把三件事钉住：**目录下的图全都参与匹配**、**名字不存在要拒**、
/// **一个名字都没选也要拒**（不兜底挑一个——那会变成"点到了别的地方"）。
#[test]
fn navigate_only_loads_every_variant_of_the_chosen_icon() {
    let icons = temp_icons_dir("navigate-only");
    write_icon(&icons, "聊天历史", 2, 20, 18);
    write_icon(&icons, "联系人", 1, 20, 18);

    // 选「聊天历史」：它底下的 2 张变体都要载入。
    // 同时刻意把配置里那组「用于联系人导航」填上——它**不该**被读到，
    // 所以下面"载入了 2 张"这个数就已经证明了没走错来源（那一组只有 1 张）。
    let config = RuntimeConfig {
        workflow: Workflow::NavigateOnly,
        nav_target: "聊天历史".into(),
        nav_icon_templates: vec!["联系人".into()],
        ..RuntimeConfig::default()
    };
    let runner = assemble_with_icons(&config, &icons).expect("导航就是任务本身，不受开关约束");
    let runner_config = runner.config();
    assert_eq!(runner_config.nav_icon_templates.len(), 2, "两张变体都要载入");
    assert_eq!(
        runner_config.nav_target_label, "聊天历史",
        "日志与失败信息里要出现的是那个目录名"
    );

    // 名字在图标库里不存在 ⇒ 装配期就拒绝（不是跑到一半才"匹配不上"）。
    let config = RuntimeConfig {
        workflow: Workflow::NavigateOnly,
        nav_target: "不存在的图标".into(),
        ..RuntimeConfig::default()
    };
    let err = assemble_with_icons(&config, &icons)
        .err()
        .expect("名字不存在必须拒绝，否则会在运行期表现为一次分数很低的匹配");
    assert!(err.contains("没有叫「不存在的图标」的图标"), "{err}");

    // 一个名字都没选 ⇒ 同样拒绝。**不兜底**：随手挑一个图标去点，
    // 症状会是"任务照常跑完，只是点到了别的地方"。
    let config = RuntimeConfig {
        workflow: Workflow::NavigateOnly,
        nav_target: "  ".into(),
        navigate_before_search: false,
        ..RuntimeConfig::default()
    };
    let err = assemble_with_icons(&config, &icons)
        .err()
        .expect("没选图标必须拒绝：导航就是这条工作流的全部内容");
    assert!(err.contains("要指定点哪一个图标"), "{err}");
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
    assert!(
        config.nav_target.is_empty(),
        "默认没选过图标——不兜底挑一个，那会变成「点到了别的地方」"
    );
    assert!(config.area_marks.is_empty(), "默认一个新增区域都没标");
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

/// ★ 任务输入按工作流校验，而**判据只有一处**（`workflow_inputs`）。
///
/// 2026-09-20 实测的现象：选了「只做导航」+ 通讯录，点「开始任务」**什么都不发生**
/// ——`data/` 下连 `task-*.log` 都没生成。原因是命令层无条件要求联系人与正文非空，
/// 而界面上那两个框（对这条路毫无意义）空着时按钮根本点不动、也不说为什么。
///
/// 这条用例钉两件事：
/// ① 每条工作流的答案本身（只做导航：都不用；另外两条：都要）；
/// ② `workflow_requirements` 下发的与 `workflow_inputs` 说的是同一份
///    —— 界面按前者渲染、命令层按后者拒绝，两者分叉就又回到"点不动/白填"。
#[test]
fn task_inputs_are_decided_by_the_workflow() {
    for (workflow, contact, message) in [
        (Workflow::NavigateOnly, false, false),
        (Workflow::SearchContact, true, true),
        (Workflow::ScrollListContact, true, true),
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
