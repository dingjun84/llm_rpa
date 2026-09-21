//! 真 OCR 重跑（`--ocr`）的回归用例。
//!
//! ## 为什么用假引擎，而不是真跑一次 OCR
//!
//! 真跑要有一张**现场截图**，而现场素材不进仓库（`docs/todo.md` T30 的隐私口径）。
//! 这里要钉的也不是"引擎读得准不准"（那是 `tools/macosocr` 自己的事），
//! 而是这条链：读 `raw/` 那张图 → 起进程喂字节 → 比读数 → 归因到层。
//! 所以用一个假引擎（`sh` 脚本）：它收下 stdin、记下参数、再吐一份事先写好的 JSON。
//! 这样"喂进去的到底是哪串字节、参数有没有传对、引擎没起来时会怎么报"都能直接断言。
//!
//! ## 只钉"读数变没变"，不钉"读得对不对"
//!
//! 假引擎读出来的东西是我们指定的，所以这几条用例证明的是**归因链本身**：
//! 换一份读数就过 ⇒ 卡 OCR；同一份读数照旧没过 ⇒ 卡判据；这一帧里压根没有
//! 要找的东西 ⇒ 卡坐标；引擎没起来 ⇒ 分不出层（如实说，不硬猜）。
//! 真机上一次读错能不能被认出来，是 [T30] 的「期望值」那一档，本轮不做。
//!
//! [T30]: ../../../docs/todo.md

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use super::*;
use crate::tests::{
    boxes_json, decision_line, read_line, the_2026_09_21_boxes, KEYWORD, LABELS, MIN_CONFIDENCE,
};

/// 事件流里记的那张**未标注**输入图（相对任务目录）。
const RAW_INPUT: &str = "raw/05-搜索下拉识别.png";

/// 假引擎：把 stdin 落到自己旁边、把参数记下来，再吐 `stdout.json`。
const FAKE_ENGINE: &str = "#!/bin/sh\n\
here=$(dirname \"$0\")\n\
cat > \"$here/stdin.bin\"\n\
printf '%s' \"$*\" > \"$here/args.txt\"\n\
cat \"$here/stdout.json\"\n";

/// 一块假 PNG。内容无所谓（假引擎不解析它），长度固定下来给用例断言
/// 「喂回引擎的就是 `raw/` 里那串字节」。
const PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\n-stand-in-for-a-frame";

/// 一个临时的"现场"：`events.jsonl` + `raw/05-搜索下拉识别.png`，外加一个假引擎。
struct Scene {
    root: PathBuf,
    engine_dir: PathBuf,
    engine: PathBuf,
}

impl Scene {
    /// `recorded` = 盘上那一帧的文字块，`fresh` = 假引擎这次该读出来的。
    fn new(name: &str, recorded: &[TextBox], fresh: &[TextBox]) -> Self {
        let root = std::env::temp_dir().join(format!("replay-real-ocr-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("raw")).expect("建临时任务目录");
        // `raw/` 里那张干净图：与真机上一样，是**没叠框**的那一串字节。
        std::fs::write(root.join(RAW_INPUT), PNG_BYTES).expect("写 raw 图");

        let judged = judge_dropdown(recorded, KEYWORD, LABELS, MIN_CONFIDENCE);
        let events = format!(
            "{}\n{}\n",
            serde_json::to_string(&read_line(recorded, Some(RAW_INPUT))).expect("事件流是 JSON"),
            serde_json::to_string(&decision_line(&judged.decision)).expect("事件流是 JSON"),
        );
        std::fs::write(root.join("events.jsonl"), events).expect("写事件流");

        // 假引擎放在**另一棵树**里：它会在自己旁边写 stdin.bin / args.txt，
        // 放进任务目录会把"引擎读到了什么"与"任务目录里有什么"搅在一起。
        let engine_dir = std::env::temp_dir().join(format!("replay-real-ocr-engine-{name}"));
        let _ = std::fs::remove_dir_all(&engine_dir);
        std::fs::create_dir_all(&engine_dir).expect("建引擎目录");
        let engine = engine_dir.join("fake-ocr");
        std::fs::write(&engine, FAKE_ENGINE).expect("写假引擎");
        std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755))
            .expect("给假引擎可执行位");
        let stdout = serde_json::to_string(&boxes_json(fresh)).expect("引擎输出是 JSON");
        std::fs::write(engine_dir.join("stdout.json"), stdout).expect("写引擎输出");

        Self { root, engine_dir, engine }
    }

    /// 按给定倍率重放一次（阈值用盘上记的那个 0.85）。
    fn replay(&self, upscale: Option<f32>) -> Replay {
        let engine = Engine::at(&self.engine).with_upscale(upscale);
        replay_dir_with_ocr(&self.root, None, Some(engine)).expect("重放要成功")
    }

    fn engine_ran(&self) -> bool {
        self.engine_dir.join("args.txt").exists()
    }

    fn args(&self) -> String {
        std::fs::read_to_string(self.engine_dir.join("args.txt")).expect("引擎记过参数")
    }

    fn stdin_len(&self) -> u64 {
        std::fs::metadata(self.engine_dir.join("stdin.bin")).expect("引擎读过 stdin").len()
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.root);
        let _ = std::fs::remove_dir_all(&self.engine_dir);
    }
}

// ── 归因到层 ───────────────────────────────────────────────────────────

/// ★ 验收第 2 条的归因版：同一张干净图，**换一份读数就通过** ⇒ 卡在 OCR 层。
///
/// 这是最硬的证据：判据一个字没改、阈值一个字没改，变的只有"谁读的这份字"。
#[test]
fn a_reread_that_flips_the_verdict_blames_the_ocr_layer() {
    // 假引擎这次把「联系人」读成 0.95（盘上那份是 0.61），别的 18 块一模一样。
    let mut fresh = the_2026_09_21_boxes();
    fresh[0].confidence = 0.95;
    let scene = Scene::new("flips", &the_2026_09_21_boxes(), &fresh);

    let replay = scene.replay(Some(3.0));
    let item = &replay.decisions[0];
    assert!(!item.recorded_passed, "现场这一步是失败的（「联系人」0.61 < 0.85）");

    let ocr = item.ocr.as_ref().expect("没过就该重跑 OCR");
    assert_eq!(ocr.input, RAW_INPUT);
    let diff = ocr.diff.as_ref().expect("引擎跑成了");
    assert_eq!(diff.changed.len(), 1, "只该有「联系人」那一块变了：{diff:?}");
    assert!(diff.missing.is_empty() && diff.added.is_empty(), "{diff:?}");
    assert!(ocr.decision.as_ref().expect("判据要能重跑").passed, "换一份读数就该通过");

    let attribution = item.attribution.as_ref().expect("没过就该有归因");
    assert_eq!(attribution.layer, Layer::Ocr, "{}", attribution.why);

    // 真起了进程：喂进去的是 `raw/` 那串字节，参数原样传给了引擎。
    assert_eq!(scene.stdin_len(), PNG_BYTES.len() as u64, "喂回引擎的就是 raw/ 里那串字节");
    assert_eq!(scene.args(), "--upscale 3", "倍率要原样传下去");

    let text = render(&scene.root, &replay, None);
    assert!(text.contains("卡在OCR / 预处理层"), "{text}");
    assert!(text.contains("~ 「联系人」0.61 → 0.95"), "差异要逐条列出来：{text}");
    scene.cleanup();
}

/// 读数**逐块复现**、盘上那次也没过 ⇒ 卡在判据层，不是识别。
#[test]
fn a_reading_that_repeats_on_disk_puts_the_blame_on_the_judge() {
    let boxes = the_2026_09_21_boxes();
    let scene = Scene::new("repeats", &boxes, &boxes);
    let replay = scene.replay(None);
    let item = &replay.decisions[0];

    let ocr = item.ocr.as_ref().expect("没过就该重跑 OCR");
    let diff = ocr.diff.as_ref().expect("引擎跑成了");
    assert_eq!(diff.summary(), "逐块一致", "同一张图、同一个引擎，读数应当一样");
    assert!(ocr.note.is_none(), "读数正常就不该有警告：{:?}", ocr.note);
    assert!(!ocr.decision.as_ref().expect("判据要能重跑").passed, "读数没变，判据也不该变");

    assert_eq!(item.attribution.as_ref().expect("没过就该有归因").layer, Layer::Judge);
    scene.cleanup();
}

/// 读数逐块一致，但这一帧里**没有一块文字沾到要找的东西** ⇒ 卡在坐标层。
///
/// 这时候再去调阈值是白费劲：命中的前提是那一块得先出现。
#[test]
fn a_frame_without_the_needle_blames_the_coordinates() {
    let boxes = vec![TextBox {
        text: "聊天记录".to_string(),
        bounds: Rect { x: 8, y: 252, width: 120, height: 24 },
        confidence: 0.90,
    }];
    let scene = Scene::new("no-needle", &boxes, &boxes);
    let replay = scene.replay(None);
    let item = &replay.decisions[0];

    assert!(!item.recorded_passed);
    let diff = item.ocr.as_ref().expect("没过就该重跑 OCR").diff.as_ref().expect("引擎跑成了");
    assert_eq!(diff.summary(), "逐块一致");

    let attribution = item.attribution.as_ref().expect("没过就该有归因");
    assert_eq!(attribution.layer, Layer::Click, "{}", attribution.why);
    let text = render(&scene.root, &replay, None);
    assert!(text.contains("这一帧没有一块文字沾到要找的东西"), "{text}");
    scene.cleanup();
}

/// 引擎没起来（路径写错、没编出来）⇒ **分不出层就说分不出**，不硬猜一个。
#[test]
fn a_broken_engine_is_reported_not_guessed() {
    let boxes = the_2026_09_21_boxes();
    let scene = Scene::new("broken", &boxes, &boxes);
    // 引擎路径故意不存在。`raw/` 那张图是**在**的，所以这测的是"引擎起不来"，
    // 不是"图丢了"——两种失败必须能分开，否则报告会把人指到错的方向去。
    let missing = std::env::temp_dir().join("replay-no-such-engine-9f2c1a");
    let replay = replay_dir_with_ocr(&scene.root, None, Some(Engine::at(&missing)))
        .expect("重放本身要成功：引擎跑不跑得成是报告里的事");

    let item = &replay.decisions[0];
    let ocr = item.ocr.as_ref().expect("没过就该重跑 OCR");
    assert!(ocr.diff.is_none(), "引擎没起来就不能编出一份读数来");
    let note = ocr.note.as_ref().expect("要说清为什么没跑成");
    assert!(note.contains("no-such-engine"), "要点名是哪个引擎起不来：{note}");
    assert_eq!(item.attribution.as_ref().expect("没过就该有归因").layer, Layer::Unknown);

    let text = render(&scene.root, &replay, None);
    assert!(text.contains("没跑成"), "{text}");
    assert!(text.contains("分不出层"), "分不出就如实说分不出：{text}");
    scene.cleanup();
}

/// 盘上记的是**通过**时，连进程都不该起：过了的步骤没有"卡在哪一层"可言，
/// 而每跑一次都要起一个进程。
#[test]
fn a_passing_step_never_starts_the_engine() {
    let mut boxes = the_2026_09_21_boxes();
    boxes[0].confidence = 0.95;
    let scene = Scene::new("passes", &boxes, &boxes);
    let replay = scene.replay(None);
    let item = &replay.decisions[0];

    assert!(item.recorded_passed, "这份读数下判据该通过（「联系人」0.95 ≥ 0.85）");
    assert!(item.ocr.is_none(), "过了的步骤不该重跑 OCR");
    assert!(item.attribution.is_none(), "过了的步骤没有归因");
    assert!(!scene.engine_ran(), "连进程都不该起");
    scene.cleanup();
}

/// 引擎起来了、却**一块文字都没读出来**（脚本假成这样，真实原因是引擎没起来
/// 或参数被拒）：这种读数不算数，得显式警告，而不是把"读数全变了"当结论。
#[test]
fn an_engine_that_reads_nothing_is_flagged_as_suspicious() {
    let boxes = the_2026_09_21_boxes();
    let scene = Scene::new("reads-nothing", &boxes, &[]);
    let replay = scene.replay(None);
    let item = &replay.decisions[0];

    let ocr = item.ocr.as_ref().expect("没过就该重跑 OCR");
    let diff = ocr.diff.as_ref().expect("进程起来了，读数就是一份真读数");
    assert_eq!(diff.missing.len(), 19, "盘上 19 块这次一块都没读到");
    assert_eq!(ocr.box_count, 0);
    let note = ocr.note.as_ref().expect("一块都没读出来要显式警告");
    assert!(note.contains("一块文字都没读出来"), "{note}");
    assert!(render(&scene.root, &replay, None).contains("⚠️"), "警告要看得见");
    scene.cleanup();
}

// ── 读数比对（纯函数） ─────────────────────────────────────────────────

/// 置信度按**三位小数**比：盘上那份收过三位小数（`0.61`），
/// 这次是引擎 stdout 的全精度值（`0.6100000143…`）。
/// 直接比 `f32` 会把每一次重跑都报成"置信度变了"——那报告就没人信了。
#[test]
fn a_confidence_that_differs_below_three_decimals_is_not_a_difference() {
    let recorded = [block("联系人", 8, 4, 0.61)];
    let fresh = [block("联系人", 8, 4, 0.610_000_014_3_f32)];
    assert!(diff_boxes(&recorded, &fresh).is_empty());
}

/// 左上角差一两个像素不算"框挪了"：换倍率之后框的边缘本来就会差一点。
#[test]
fn a_frame_that_moved_by_one_pixel_is_not_a_difference() {
    let recorded = [block("联系人", 8, 4, 0.61)];
    assert!(diff_boxes(&recorded, &[block("联系人", 9, 3, 0.61)]).is_empty(), "差 1px 不算");

    let diff = diff_boxes(&recorded, &[block("联系人", 13, 4, 0.61)]);
    assert_eq!(diff.changed.len(), 1, "差 5px 就算：{diff:?}");
    assert_eq!(diff.changed[0].shift, (5, 0));
}

/// 多出来的与丢掉的分开报，摘要按「多 / 缺 / 改」的顺序念。
#[test]
fn missing_and_added_blocks_are_both_reported() {
    let recorded = [block("联系人", 0, 0, 0.61), block("最常使用", 0, 30, 0.68)];
    let fresh = [block("联系人", 0, 0, 0.61), block("搜索", 300, 4, 0.90)];
    let diff = diff_boxes(&recorded, &fresh);

    assert_eq!(diff.missing, vec![("最常使用".to_string(), 0.68)]);
    assert_eq!(diff.added, vec![("搜索".to_string(), 0.90)]);
    assert!(diff.changed.is_empty());
    assert!(!diff.is_empty());
    assert_eq!(diff.summary(), "多 1 块 / 缺 1 块");
}

/// 同一句话可能出现多次（现场那 19 块里「联系人」就有两处）：**按文字配对**，
/// 不按序号——按序号比会把"两块换了个位置"报成"两块全变了"。
#[test]
fn repeated_text_is_paired_by_text_not_by_order() {
    let recorded = [block("联系人", 8, 4, 0.61), block("联系人", 620, 4, 0.66)];
    let fresh = [block("联系人", 620, 4, 0.66), block("联系人", 8, 4, 0.61)];
    assert!(diff_boxes(&recorded, &fresh).is_empty());
}

fn block(text: &str, x: i32, y: i32, confidence: f32) -> TextBox {
    TextBox {
        text: text.to_string(),
        bounds: Rect { x, y, width: 120, height: 24 },
        confidence,
    }
}