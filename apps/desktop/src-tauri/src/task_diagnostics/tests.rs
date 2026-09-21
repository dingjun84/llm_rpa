//! 过程诊断落盘的回归用例。
//!
//! 从 `task_diagnostics.rs` 抽出来：那个文件要同时放下"什么时候落、落多少张、
//! 收尾"与事件流那条线，测试内联进去就过了 `CONVENTIONS.md` §2 的 500 行上限
//! （同 `policy/trail/tests.rs` 的做法）。

use super::*;
use automation_core::{Rect, Screenshot, TextBox};
use std::time::SystemTime;

/// 一份 OCR 引擎 stdout 的原文，钉「原文落盘」这件事。
const OCR_RAW: &str = "[{\"text\":\"联系人\",\"x\":1,\"y\":2,\"w\":30,\"h\":12,\"confidence\":0.61}]";

fn frame(fingerprint: &str) -> Screenshot {
    Screenshot {
        fingerprint: fingerprint.to_string(),
        pixels: vec![40, 40, 40, 255].repeat(60 * 40),
        width: 60,
        height: 40,
        captured_at: SystemTime::now(),
    }
}

/// 观察一步**没做 OCR** 的画面（只截了指纹）。
fn observe(diagnostics: &TaskDiagnostics, label: &str, fingerprint: &str) {
    let shot = frame(fingerprint);
    let boxes: Vec<TextBox> = vec![];
    diagnostics.observe(
        TaskId::nil(),
        &Observation {
            label,
            region: Rect { x: 10, y: 20, width: 60, height: 40 },
            frame: &shot,
            text_boxes: &boxes,
            ocr_raw: None,
        },
    );
}

#[test]
fn every_step_gets_its_own_file_and_the_overview_is_stitched() {
    let root = temp_root("steps_and_overview");
    let diagnostics = TaskDiagnostics::new(root.join("task"), "任务 x");
    observe(&diagnostics, "搜索下拉列表", "a");
    observe(&diagnostics, "联系人候选区", "b");
    diagnostics.finish();

    let steps: Vec<_> = std::fs::read_dir(root.join("task").join(STEPS_DIR))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(steps.len(), 2, "两步就该有两个文件：{steps:?}");
    assert!(steps.iter().any(|n| n.starts_with("01-")));
    assert!(root.join("task").join(OVERVIEW_FILE_NAME).is_file());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_repeated_frame_is_not_recorded_twice() {
    let root = temp_root("dedupe");
    let diagnostics = TaskDiagnostics::new(root.join("task"), "任务 x");
    observe(&diagnostics, "联系人列表", "same");
    observe(&diagnostics, "联系人列表", "same");
    observe(&diagnostics, "联系人列表", "moved");
    diagnostics.finish();

    let count = std::fs::read_dir(root.join("task").join(STEPS_DIR)).unwrap().count();
    assert_eq!(count, 2, "内容一样的重复帧不该各留一张");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn finishing_without_a_single_step_writes_nothing() {
    let root = temp_root("empty");
    let diagnostics = TaskDiagnostics::new(root.join("task"), "任务 x");
    diagnostics.finish();
    assert!(!root.join("task").exists(), "一步都没走就不该凭空建出目录");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn finishing_twice_does_not_write_the_overview_twice() {
    let root = temp_root("twice");
    let diagnostics = TaskDiagnostics::new(root.join("task"), "任务 x");
    observe(&diagnostics, "一步", "a");
    diagnostics.finish();
    let before = std::fs::read_to_string(root.join("task").join(LOG_FILE_NAME)).unwrap();
    diagnostics.finish();
    let after = std::fs::read_to_string(root.join("task").join(LOG_FILE_NAME)).unwrap();
    assert!(after.starts_with(&before), "重复收尾不该把日志清掉");
    let _ = std::fs::remove_dir_all(root);
}

/// 事件流要「一行一条」，并且**看到什么**与**怎么判的**各成一条。
///
/// 这一条是 T29 的地基：界面「过程重放」按 `step` 把两条对上，
/// 于是同一屏上，左图是当时那一帧、右表是它为什么这么判。
#[test]
fn the_event_stream_records_the_frame_and_the_decision() {
    use crate::task_log::EVENTS_FILE_NAME;
    use automation_core::{ReplayInput, Verdict};

    let root = temp_root("events");
    let diagnostics = TaskDiagnostics::new(root.join("task"), "任务 x");
    let shot = frame("a");
    let boxes = vec![TextBox {
        text: "联系人".into(),
        bounds: Rect { x: 1, y: 2, width: 30, height: 12 },
        confidence: 0.61,
    }];
    diagnostics.observe(
        TaskId::nil(),
        &Observation {
            label: "搜索下拉识别",
            region: Rect { x: 10, y: 20, width: 60, height: 40 },
            frame: &shot,
            text_boxes: &boxes,
            ocr_raw: Some(OCR_RAW),
        },
    );
    diagnostics.decide(
        TaskId::nil(),
        &Decision {
            step: "搜索下拉识别".into(),
            question: "下拉里哪一行是「李小明」？".into(),
            rule: "取包含关键词的最上面一行".into(),
            outcome: "转人工：没有找到分组".into(),
            passed: false,
            min_confidence: 0.85,
            replay: Some(ReplayInput::Dropdown {
                keyword: "李小明".into(),
                group_labels: "联系人 / 最常使用".into(),
            }),
            candidates: vec![Verdict {
                text: "联系人".into(),
                confidence: 0.61,
                bounds: Rect { x: 1, y: 2, width: 30, height: 12 },
                passed: false,
                reason: "置信度 0.61 低于阈值 0.85".into(),
            }],
        },
    );
    diagnostics.finish();

    let events = events::read_events(&root.join("task").join(EVENTS_FILE_NAME));
    assert_eq!(events.len(), 2, "一读一判就是两条：{events:?}");
    let read = &events[0];
    assert_eq!(read["kind"], "read");
    assert_eq!(read["step"], "搜索下拉识别");
    assert_eq!(read["region"]["w"], 60, "要记下「这一步本来要看哪儿」");
    assert_eq!(read["frame"]["fingerprint"], "a");
    assert_eq!(read["image"], "steps/01-搜索下拉识别.png", "路径是相对任务目录的");
    assert_eq!(read["boxes"][0]["text"], "联系人");
    assert_eq!(read["boxes"][0]["confidence"], 0.61, "置信度是关键线索，不能丢");
    assert!(root.join("task").join("steps/01-搜索下拉识别.png").is_file());

    let decision = &events[1];
    assert_eq!(decision["kind"], "decision");
    assert_eq!(decision["outcome"], "转人工：没有找到分组");
    assert_eq!(decision["passed"], false, "界面默认落在失败那一步，靠的就是这一笔");
    assert_eq!(decision["replay"]["keyword"], "李小明", "重放要的输入跟着决策一起留下");
    assert_eq!(decision["candidates"][0]["passed"], false);
    assert!(decision["candidates"][0]["reason"].as_str().unwrap().contains("0.61"));
    let _ = std::fs::remove_dir_all(root);
}

/// ★ T30 第 1 条：做过 OCR 的那一步要留下**原料**——未标注的输入图 + 引擎 stdout 原文。
///
/// **为什么钉住它**：这是"拿这一帧重跑一次 OCR"的唯一前提。
/// `steps/` 那张图上有区域框、有文字框，拿它重跑会读出另一套结果，
/// 于是"卡在 OCR 层还是判据层"就永远分不清。两样东西少一样，这条链就断了。
#[test]
fn an_ocr_step_keeps_the_clean_input_and_the_raw_engine_output() {
    use crate::task_log::EVENTS_FILE_NAME;

    let root = temp_root("raw");
    let diagnostics = TaskDiagnostics::new(root.join("task"), "任务 x");
    let shot = frame("a");
    let boxes = vec![TextBox {
        text: "联系人".into(),
        bounds: Rect { x: 1, y: 2, width: 30, height: 12 },
        confidence: 0.61,
    }];
    diagnostics.observe(
        TaskId::nil(),
        &Observation {
            label: "搜索下拉识别",
            region: Rect { x: 10, y: 20, width: 60, height: 40 },
            frame: &shot,
            text_boxes: &boxes,
            ocr_raw: Some(OCR_RAW),
        },
    );
    // 没做 OCR 的一步（只看画面）：不该跟着留原料。
    observe(&diagnostics, "画面停稳", "b");
    diagnostics.finish();

    let input = root.join("task").join(format!("{RAW_DIR}/01-搜索下拉识别.png"));
    assert!(input.is_file(), "要留下喂给 OCR 的那张**未标注**图：{}", input.display());
    let text = root.join("task").join(format!("{RAW_DIR}/01-搜索下拉识别.json"));
    assert_eq!(
        std::fs::read_to_string(&text).unwrap(),
        OCR_RAW,
        "引擎 stdout 要**原文**落盘，一个字符都不改"
    );
    assert!(
        !root.join("task").join(format!("{RAW_DIR}/02-画面停稳.png")).exists(),
        "没做 OCR 的步骤没有输入图可留"
    );

    let events = events::read_events(&root.join("task").join(EVENTS_FILE_NAME));
    assert_eq!(events[0]["ocr_input"], "raw/01-搜索下拉识别.png");
    assert_eq!(events[0]["ocr_raw"], "raw/01-搜索下拉识别.json");
    assert!(events[1]["ocr_input"].is_null(), "没做 OCR 就如实记 null");
    let _ = std::fs::remove_dir_all(root);
}

/// 引擎没留下原始文本时（替身引擎、或引擎本身不给）：输入图照留，原文如实记 `null`。
///
/// 一个 0 字节的 json 会被读成"读到了空"——那与"没留下"是两件事。
#[test]
fn a_step_without_raw_text_keeps_the_input_but_no_empty_file() {
    use crate::task_log::EVENTS_FILE_NAME;

    let root = temp_root("raw_empty");
    let diagnostics = TaskDiagnostics::new(root.join("task"), "任务 x");
    let shot = frame("a");
    let boxes: Vec<TextBox> = vec![];
    diagnostics.observe(
        TaskId::nil(),
        &Observation {
            label: "标题核验",
            region: Rect { x: 10, y: 20, width: 60, height: 40 },
            frame: &shot,
            text_boxes: &boxes,
            ocr_raw: Some(""),
        },
    );
    diagnostics.finish();

    let events = events::read_events(&root.join("task").join(EVENTS_FILE_NAME));
    assert_eq!(events[0]["ocr_input"], format!("{RAW_DIR}/01-标题核验.png"));
    assert!(events[0]["ocr_raw"].is_null(), "没有原文就不记路径");
    assert!(!root.join("task").join(format!("{RAW_DIR}/01-标题核验.json")).exists());
    let _ = std::fs::remove_dir_all(root);
}

/// 进程被强杀时最后一行可能是半截：坏行丢掉，前面的一行不落。
///
/// 为一行坏数据把几十步全不显示，等于把"看得见的线索"换成"什么都没有"。
#[test]
fn a_half_written_last_line_does_not_blind_the_replay_view() {
    use crate::task_log::EVENTS_FILE_NAME;

    let root = temp_root("half_line");
    let path = root.join("task").join(EVENTS_FILE_NAME);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        "{\"v\":1,\"kind\":\"read\",\"step\":\"一步\"}\n{\"v\":1,\"kind\":\"deci",
    )
    .unwrap();

    let events = events::read_events(&path);
    assert_eq!(events.len(), 1, "半截行丢掉、前面的一行不落：{events:?}");
    assert_eq!(events[0]["step"], "一步");
    let _ = std::fs::remove_dir_all(root);
}

fn temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("llm-rpa-diag-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}