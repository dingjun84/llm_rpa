//! 重放的回归用例，外加**现场 fixture 的生成器**。
//!
//! ## 为什么 fixture 是"生成"出来的
//!
//! `tests/fixtures/task-a859ae74/events.jsonl` 照着 2026-09-21 那次实机失败的
//! **结构**造（19 块文字、「联系人」置信度 0.61 低于阈值 0.85）。
//! ⚠️ **但里面的人名与群名全是假数据**（`李小明` / `王小明` / `李四` / `测试群一`…）：
//! 真实姓名不进仓库，这是隐私口径的一部分（见 `docs/todo.md` T30）。
//! 它里面那些 `candidates` 是**判据当时给的**逐块理由，所以只能由判据本身产出——
//! 手写一份就变成 "照着一个已经错的猜想做出来的数据"，而它正是用来判断判据的。
//!
//! 于是留一个**被忽略**的用例 [`regenerate_the_2026_09_21_fixture`]：
//! 判据的措辞改了，重跑它一次（`cargo test -p replay -- --ignored`），
//! fixture 跟着更新，回归不会因为文案变了而假装通过。
//!
//! ## 现场数据为什么在这一侧再写一遍
//!
//! `crates/automation-core/src/dropdown/tests.rs` 里也有一份同样的 19 块。
//! 跨 crate 共用不了同一份数据，而两边钉的不是同一件事：
//! 那边钉**判据的边界**，这边钉**事件流 → 重跑 → 报告**这一整段。

use std::path::Path;

use serde_json::{json, Value};

use super::*;

/// 2026-09-21 那次失败现场的事件流。
const FAILING_SCENE: &str = include_str!("../tests/fixtures/task-a859ae74/events.jsonl");

/// 现场用的阈值：`min_confidence` 默认值。
///
/// ⚠️ 下面几样与 [`the_2026_09_21_boxes`] 是 `pub(crate)`：真 OCR 那几条用例
/// （`src/ocr_tests.rs`）要拿**同一份现场数据**造自己的临时场景。现场只有一份，
/// 两处各抄一份，改了一处另一处不会跟着动——那比不写用例更糟。
pub(crate) const MIN_CONFIDENCE: f32 = 0.85;

/// 现场用的分组标题配置原文。
pub(crate) const LABELS: &str = "联系人 / 最常使用";

/// 现场搜的关键词。
pub(crate) const KEYWORD: &str = "李小明";

// ── 验收 ───────────────────────────────────────────────────────────────

/// ★ 整件事的验收第 1 条：重放能在**一行**里回答
/// 「19 块里有「联系人」，为什么没找到」。
///
/// 那一行必须是**判据自己给的**理由（带它自己的置信度与阈值），
/// 而不是这个工具另写的一句话。
#[test]
fn the_fixture_answers_why_the_contact_label_was_missed() {
    let replay = replay_text(FAILING_SCENE, None);
    assert_eq!(replay.event_count, 2, "fixture 就两条事件");
    assert_eq!(replay.decisions.len(), 1, "fixture 就一条判定");

    let item = &replay.decisions[0];
    assert_eq!(item.box_count, 19, "现场就是 19 块文字");
    assert!(!item.recorded_passed, "盘上记的是失败");
    let decision = item.decision.as_ref().expect("这一条必须能重跑");
    assert!(!decision.passed, "重跑也要得到同样的失败");
    assert!(!decision.outcome.is_empty(), "结论不能是空的");

    let text = render(Path::new("data/tasks/task-a859ae74"), &replay, None);
    let line = text
        .lines()
        .find(|line| line.contains("「联系人」淘汰"))
        .unwrap_or_else(|| panic!("重点里要念到那一块：\n{text}"));
    assert!(line.contains("0.61"), "要有它自己的置信度：{line}");
    assert!(line.contains("0.85"), "要有阈值：{line}");
    assert!(line.contains("没参与判定"), "要说清是被阈值挡下的、不是没读到：{line}");
}

/// ★ 验收第 2 条：改 `min_confidence` 再重放，**结论随之改变**。
///
/// 这一条证明的是"报告里的理由确实来自判据本身"——如果报告念的是盘上记的候选，
/// 换个阈值它一个字都不会变，用例就会挂。
#[test]
fn lowering_the_threshold_flips_the_verdict() {
    let relaxed = replay_text(FAILING_SCENE, Some(0.60));
    let decision = relaxed.decisions[0].decision.as_ref().expect("这一条必须能重跑");
    assert!(decision.passed, "阈值降到 0.60 之后「联系人」这一组就认出来了");
    assert!(
        decision.candidates.iter().any(|candidate| candidate.text == "李小明" && candidate.passed),
        "认出来了就该挑中最上面那个「李小明」"
    );

    let text = render(Path::new("."), &relaxed, Some(0.60));
    assert!(text.contains("结论变了"), "结论变了要显式写出来，不能让人自己比对：\n{text}");
    assert!(text.contains("（盘上记的是 0.85）"), "要交代被覆盖掉的那个值：\n{text}");
    assert!(text.contains("判据"), "报告里要写清按哪条判据重跑的：\n{text}");
}

/// 阈值一放就过 ⇒ 卡的是**判据**，不是识别。
///
/// 这是最省事的一种归因：不用起 OCR 引擎，同一份读数换个阈值就分出层了。
/// 它与真 OCR 那条"换一份读数才过"（`src/ocr_tests.rs`）恰好是一对——
/// 两条都成立时以判据那条为准（先换阈值，再怀疑识别）。
#[test]
fn a_threshold_that_flips_the_verdict_blames_the_judge() {
    let replay = replay_text(FAILING_SCENE, Some(0.60));
    assert!(!replay.decisions[0].recorded_passed, "盘上记的是失败");
    let attribution = replay.decisions[0].attribution.as_ref().expect("没过就该有归因");
    assert_eq!(attribution.layer, Layer::Judge, "{}", attribution.why);
    assert!(attribution.why.contains("阈值"), "理由里要点出阈值这条路：{}", attribution.why);

    let text = render(Path::new("."), &replay, Some(0.60));
    assert!(text.contains("归因：卡在判据 / 阈值层"), "{text}");
}

/// 通过了的步骤没有"卡在哪一层"可言——不给它编一个归因。
#[test]
fn a_passing_step_has_no_attribution() {
    let read = r#"{"v":1,"kind":"read","t":"t","step":"标题核验","index":7,"image":"steps/07-标题核验.png","region":{"x":0,"y":0,"w":360,"h":420},"frame":{"w":360,"h":420,"fingerprint":"x"},"boxes":[{"text":"李小明","x":10,"y":10,"w":120,"h":24,"confidence":0.99}]}"#;
    let decision = r#"{"v":1,"kind":"decision","t":"t","step":"标题核验","question":"q","rule":"r","outcome":"o","passed":true,"min_confidence":0.85,"replay":null,"candidates":[]}"#;
    let replay = replay_text(&format!("{read}\n{decision}\n"), None);
    let item = &replay.decisions[0];
    assert!(item.recorded_passed);
    assert!(item.attribution.is_none(), "过了的步骤不该有归因");
    let text = render(Path::new("."), &replay, None);
    assert!(!text.contains("归因"), "报告里也不该出现归因那一行：{text}");
}

// ── 读盘 ───────────────────────────────────────────────────────────────

/// 进程被强杀时最后一行可能是**半截**。丢掉它，前面那些步一步都不能少
/// （与界面那边同一条规矩：为一个坏行丢掉几十步，比什么都看不见更糟）。
#[test]
fn a_half_written_last_line_does_not_blind_the_replay() {
    let text = format!("{FAILING_SCENE}\n{{\"v\":1,\"kind\":\"read\",\"step\":\"标题核验\"");
    let replay = replay_text(&text, None);
    assert_eq!(replay.decisions.len(), 1, "半截行不能把前面那条判定也吞掉");
    assert!(replay.decisions[0].decision.is_some());
}

#[test]
fn an_unknown_event_kind_is_skipped_not_guessed() {
    let text = format!("{FAILING_SCENE}\n{{\"v\":9,\"kind\":\"future_thing\",\"payload\":1}}");
    let replay = replay_text(&text, None);
    assert_eq!(replay.decisions.len(), 1);
    assert_eq!(replay.event_count, 3, "认不出的 kind 算一条事件，但不能变成一次判定");
}

/// 配不到那一帧就**如实说**，不退到"最近的那一帧"去猜——
/// 猜错了会给出一个看起来很像真的结论。
#[test]
fn a_decision_without_its_frame_says_so_instead_of_guessing() {
    let text = r#"{"v":1,"kind":"decision","t":"2026-09-21 18:00:00.000","step":"搜索下拉识别","question":"q","rule":"r","outcome":"转人工：搜索下拉列表里没有找到任何一组","passed":false,"min_confidence":0.85,"replay":{"kind":"dropdown","keyword":"李小明","group_labels":"联系人"},"candidates":[]}"#;
    let replay = replay_text(text, None);
    let item = &replay.decisions[0];
    assert!(item.decision.is_none(), "没有那一帧就不能假装重跑过");
    let note = item.note.as_ref().expect("要说清为什么没能重跑");
    assert!(note.contains("看图"), "要点名缺的是哪一样：{note}");

    let rendered = render(Path::new("."), &replay, None);
    assert!(rendered.contains("没能重跑"), "{rendered}");
    assert!(rendered.contains("转人工：搜索下拉列表里没有找到任何一组"), "盘上那句要原文摆出来：{rendered}");
}

/// `replay` 写成 `null`（判据不支持离线重跑）时也要如实说，
/// 而不是把"没记"当成"没判错"。
#[test]
fn a_decision_that_cannot_be_replayed_is_reported_as_such() {
    let text = concat!(
        r#"{"v":1,"kind":"read","t":"t","step":"标题核验","index":7,"image":"steps/07-标题核验.png","region":{"x":0,"y":0,"w":360,"h":420},"frame":{"w":360,"h":420,"fingerprint":"x"},"boxes":[{"text":"李小明","x":10,"y":10,"w":120,"h":24,"confidence":0.99}]}"#,
        "\n",
        r#"{"v":1,"kind":"decision","t":"t","step":"标题核验","question":"q","rule":"r","outcome":"o","passed":true,"min_confidence":0.85,"replay":null,"candidates":[]}"#,
        "\n",
    );
    let replay = replay_text(text, None);
    let item = &replay.decisions[0];
    assert!(item.decision.is_none());
    assert!(item.note.as_ref().unwrap().contains("不支持离线重跑"));
}

/// 三条链里的第二条：姓名匹配也走**同一个**判据函数重跑，
/// 而且按盘上记的 `relaxed` 还原成当时那条判据——严格与放宽是两个结论。
#[test]
fn the_name_match_chain_replays_with_the_recorded_judge() {
    let read = r#"{"v":1,"kind":"read","t":"t","step":"联系人识别","index":4,"image":"steps/04-联系人识别.png","region":{"x":0,"y":0,"w":360,"h":420},"frame":{"w":360,"h":420,"fingerprint":"x"},"boxes":[{"text":"0 李四","x":10,"y":10,"w":120,"h":24,"confidence":0.99}]}"#;
    let decision = |relaxed: bool| {
        format!(
            r#"{{"v":1,"kind":"decision","t":"t","step":"联系人识别","question":"q","rule":"r","outcome":"o","passed":false,"min_confidence":0.85,"replay":{{"kind":"name_match","expected_name":"李四","relaxed":{relaxed}}},"candidates":[]}}"#
        )
    };

    // 放宽层：OCR 把红点并进姓名行读出「0 李四」，包含匹配认得出。
    let lenient = replay_text(&format!("{read}\n{}\n", decision(true)), None);
    let replayed = lenient.decisions[0].decision.as_ref().expect("这一条必须能重跑");
    assert!(replayed.passed, "放宽层应当认得出「0 李四」");
    assert!(replayed.outcome.contains("李四"), "结论里要点出选中了谁：{}", replayed.outcome);
    assert!(!lenient.decisions[0].recorded_passed);

    // 严格层：同一帧、另一条判据 —— 结论必须不一样，否则"按盘上记的判据还原"是假的。
    let strict = replay_text(&format!("{read}\n{}\n", decision(false)), None);
    assert!(!strict.decisions[0].decision.as_ref().unwrap().passed, "逐字精确匹配认不出「0 李四」");
}

/// 目录里没有事件流是**最常见的用法错误**（拿旧布局的任务目录来跑），
/// 报错必须点名少的是哪个文件。
#[test]
fn a_task_directory_without_an_event_stream_says_which_file_is_missing() {
    let dir = std::env::temp_dir().join("replay-no-events-2f6a1c");
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let err = match replay_dir(&dir, None) {
        Ok(_) => panic!("没有 events.jsonl 必须报错"),
        Err(err) => err,
    };
    assert!(err.contains("events.jsonl"), "要点名少的是哪个文件：{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

// ── 现场 fixture 的生成器 ──────────────────────────────────────────────

/// 重新生成 [`FAILING_SCENE`]。**平时不跑**（`#[ignore]`）：它会往仓库里写文件。
///
/// ```text
/// cargo test -p replay --lib -- --ignored regenerate
/// ```
#[test]
#[ignore = "现场 fixture 的生成器，会写仓库里的文件"]
fn regenerate_the_2026_09_21_fixture() {
    let boxes = the_2026_09_21_boxes();
    let judged = judge_dropdown(&boxes, KEYWORD, LABELS, MIN_CONFIDENCE);
    assert!(!judged.decision.passed, "fixture 必须是那次失败的现场，否则回归就假了");

    let text = format!(
        "{}\n{}\n",
        serde_json::to_string(&read_line(&boxes, None)).expect("事件流是 JSON"),
        serde_json::to_string(&decision_line(&judged.decision)).expect("事件流是 JSON"),
    );
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/task-a859ae74/events.jsonl");
    std::fs::create_dir_all(path.parent().expect("fixture 的目录")).expect("建 fixture 目录");
    std::fs::write(&path, text).expect("写 fixture");
}

/// 2026-09-21 实机现场那一帧的 19 块文字。
///
/// 区域与帧尺寸是**示意值**（判据用不到它们，界面只拿它们显示"这一步看的是哪块"）；
/// 19 块文字、它们的位置与置信度才是现场数据——「联系人」就在里面，置信度 0.61。
pub(crate) fn the_2026_09_21_boxes() -> Vec<TextBox> {
    let raw: [(&str, i32, i32, f32); 19] = [
        ("联系人", 8, 4, 0.61),
        ("最常使用", 8, 30, 0.68),
        ("李小明", 20, 60, 0.97),
        ("Jerry-李小明同学", 20, 84, 0.95),
        ("太过活跃李小明", 20, 108, 0.93),
        ("测试群一", 20, 132, 0.96),
        ("和 李小明 的聊天", 20, 156, 0.94),
        ("群聊", 8, 180, 0.92),
        ("测试群二", 20, 204, 0.91),
        ("李小明：可以了，登录进去了", 20, 228, 0.96),
        ("李小明 邀请你加入了群聊", 300, 228, 0.93),
        ("联系人", 620, 4, 0.66),
        ("搜索", 300, 4, 0.90),
        ("聊天记录", 8, 252, 0.90),
        ("李小明", 20, 276, 0.40),
        ("文件传输助手", 20, 300, 0.99),
        ("王小明", 20, 324, 0.98),
        ("李四", 20, 348, 0.97),
        ("外部测试账号", 20, 372, 0.96),
    ];
    raw.iter()
        .map(|(text, x, y, confidence)| TextBox {
            text: (*text).to_string(),
            bounds: Rect { x: *x, y: *y, width: 120, height: 24 },
            confidence: *confidence,
        })
        .collect()
}

/// 造一行 `read`。字段名与 `apps/desktop/…/task_diagnostics/events.rs` 的
/// `EventLog::read` 一致——**格式只在那里定义**，这里只是照着写一份现场数据。
///
/// `ocr_input` 给 `None` 就是"这一步没做 OCR"（事件流里写 `null`）：这份假场景
/// 里没有图（仓库里不放现场素材，隐私口径见 `docs/todo.md` T30），所以它也没有
/// 可重跑的 OCR 原料。要造有原料的场景见 `src/ocr_tests.rs`。
pub(crate) fn read_line(boxes: &[TextBox], ocr_input: Option<&str>) -> Value {
    json!({
        "v": 1,
        "kind": "read",
        "t": "2026-09-21 18:12:07.418",
        "step": "搜索下拉识别",
        "index": 5,
        "image": "steps/05-搜索下拉识别.png",
        "region": { "x": 216, "y": 96, "w": 360, "h": 420 },
        "frame": {
            "w": 360,
            "h": 420,
            "fingerprint": "b1f0c2a4e7d93b5a6c8e0f1d2a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d",
        },
        "boxes": boxes_json(boxes),
        "ocr_input": ocr_input,
        "ocr_raw": Value::Null,
    })
}

/// 造一行 `decision`。字段名与 `EventLog::decide` 一致。
pub(crate) fn decision_line(decision: &Decision) -> Value {
    json!({
        "v": 1,
        "kind": "decision",
        "t": "2026-09-21 18:12:07.441",
        "step": "搜索下拉识别",
        "question": decision.question,
        "rule": decision.rule,
        "outcome": decision.outcome,
        "passed": decision.passed,
        "min_confidence": confidence(decision.min_confidence),
        "replay": match decision.replay.as_ref() {
            Some(ReplayInput::Dropdown { keyword, group_labels }) => json!({
                "kind": "dropdown",
                "keyword": keyword,
                "group_labels": group_labels,
            }),
            Some(ReplayInput::NameMatch { expected_name, relaxed }) => json!({
                "kind": "name_match",
                "expected_name": expected_name,
                "relaxed": relaxed,
            }),
            None => Value::Null,
        },
        "candidates": decision
            .candidates
            .iter()
            .map(|candidate| json!({
                "text": candidate.text,
                "passed": candidate.passed,
                "reason": candidate.reason,
                "x": candidate.bounds.x,
                "y": candidate.bounds.y,
                "w": candidate.bounds.width,
                "h": candidate.bounds.height,
                "confidence": confidence(candidate.confidence),
            }))
            .collect::<Vec<Value>>(),
    })
}

pub(crate) fn boxes_json(boxes: &[TextBox]) -> Vec<Value> {
    boxes
        .iter()
        .map(|block| {
            json!({
                "text": block.text,
                "x": block.bounds.x,
                "y": block.bounds.y,
                "w": block.bounds.width,
                "h": block.bounds.height,
                "confidence": confidence(block.confidence),
            })
        })
        .collect()
}

/// 置信度落盘前收成三位小数——与写事件流那一侧同一条理由：
/// `f32` 的 0.61 存的是 0.6100000143051147，落进 JSON 就是那一长串。
fn confidence(value: f32) -> f64 {
    (value as f64 * 1000.0).round() / 1000.0
}