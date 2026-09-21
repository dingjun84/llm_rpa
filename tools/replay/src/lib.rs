//! 离线重放：拿盘上记下的那一帧，把判据**重跑一遍**。
//!
//! ## 它回答哪个问题
//!
//! `task.log` 说的是一句结论——「搜索下拉列表里没有找到「联系人 / 最常使用」
//! 中的任何一组，读到 19 块文字」；标注图说的是"看到了什么"。两者都回答不了
//! **「那 19 块里明明有「联系人」，为什么没认出来」**。
//!
//! 答得了的是把当时的输入（那一帧的文字块 + 关键词 + 分组 + 阈值）喂回**同一个**
//! 判据函数重跑一次：结论与逐块理由都回来了，换个阈值还能立刻看出结论变不变。
//!
//! ## 为什么是"重跑"而不是"读出记下来的候选"
//!
//! 记在事件流里的候选是**当时那次判定**的产物；要回答"阈值改成 0.70 会怎样"，
//! 只能重跑。而重跑能成立的前提是**判据只有一处**（`CONVENTIONS.md` §1.3）——
//! 这正是 `docs/todo.md` T29 把判定体抽成纯函数、并把下拉判据 `pub` 出来的理由。
//!
//! ## 事件流的格式
//!
//! 格式定义在 `apps/desktop/src-tauri/src/task_diagnostics/events.rs`，这里只读。
//! 解析不了的行（进程被强杀时最后一行可能是半截）**直接跳过**：为一个坏行
//! 丢掉前面几十步，比什么都看不见更糟。
//!
//! ## 真 OCR 模式：把「是不是读错了」也问出来
//!
//! 判据重跑吃的是**盘上记下的那份 boxes**，所以它答不了"是不是 OCR 读错了"
//! ——那份 boxes 正是它要审的东西。加了 `--ocr` 之后，把 `raw/` 下那张
//! **未标注**的输入图（就是当时喂进引擎的那串 PNG 字节）喂回本机引擎再跑一遍，
//! 读数一比就知道卡在哪一层（`docs/todo.md` T30 的「归因到层」）。
//! 细节在 [`ocr`] 与 [`layer`]。
//!
//! ## 用法
//!
//! ```text
//! cargo run -p replay -- data/tasks/<任务ID>
//! cargo run -p replay -- data/tasks/<任务ID> --min-confidence 0.70
//! cargo run -p replay -- data/tasks/<任务ID> --upscale 3
//!```

use std::path::Path;

use automation_core::dropdown::judge_dropdown;
use automation_core::{
    name_match_decision, ContactMatcher, ContainsNameMatcher, Decision, Rect, ReplayInput,
    StrictContactMatcher, TextBox,
};
use serde_json::Value;

mod layer;
mod ocr;
mod report;

pub use layer::{Attribution, Layer};
pub use ocr::{diff_boxes, BoxDiff, Changed, Engine};
pub use report::render;

#[cfg(test)]
mod tests;

/// 真 OCR 重跑那几条用例要起一个假引擎进程（`sh` 脚本），只在 Unix 上编。
#[cfg(all(test, unix))]
mod ocr_tests;

/// 事件流文件名：`data/tasks/<任务ID>/events.jsonl`。
const EVENTS_FILE_NAME: &str = "events.jsonl";

/// 一个任务目录重放出来的全部结果。
pub struct Replay {
    /// 事件流里读出来的事件条数（解析不了的行不算）。
    pub event_count: usize,
    /// 按事件顺序排好的判定。
    pub decisions: Vec<Replayed>,
    /// 这次真 OCR 用的是哪个引擎（含参数）；`None` = 没跑（只重跑判据）。
    pub ocr_engine: Option<String>,
}

/// 一次判定的重放结果：**重跑得到的结论** + 盘上记的那一份（用来对比）。
pub struct Replayed {
    /// 步骤名（与那一步的「看图」同名）。
    pub step: String,
    /// 这一步那次「看图」的序号（`steps/NN-….png` 的 NN）；没配上图时为 `None`。
    pub read_index: Option<usize>,
    /// 喂给判据的文字块数。
    pub box_count: usize,
    /// 盘上记的「成没成」。
    pub recorded_passed: bool,
    /// 盘上记的结论那句人话。
    pub recorded_outcome: String,
    /// 盘上记的阈值。
    pub recorded_min_confidence: f32,
    /// 重跑得到的决策；`None` = 这一次没法重跑，原因在 [`Replayed::note`]。
    pub decision: Option<Decision>,
    /// 没能重跑的原因（判据不支持 / 没配到那一帧）。**如实写出来**，
    /// 不要拿一条空判定假装重跑过。
    pub note: Option<String>,
    /// 真 OCR 重跑（`--ocr`）。`None` = 这一步没跑：盘上是通过的、
    /// 这一步没做 OCR、或者压根没配引擎——三者都不该在这儿编出一个结果来。
    pub ocr: Option<OcrRerun>,
    /// 归因到层。`None` = 没有"卡在哪一层"可言：盘上记的是通过，
    /// 或者连那一帧都没配上（读数都没有，归不了因）。
    pub attribution: Option<Attribution>,
}

/// 一次真 OCR 重跑：喂进去的是哪张图、跑的是哪个引擎、读数差多少。
pub struct OcrRerun {
    /// 引擎（含参数）的描述。
    pub engine: String,
    /// 喂进去的那张**未标注**的输入图（相对任务目录的路径）。
    pub input: String,
    /// 重跑出来的读数与盘上那份的差异；`None` = 没跑成（原因在 `note`）。
    pub diff: Option<BoxDiff>,
    /// 重跑读出来几块文字（没跑成时为 0）。
    pub box_count: usize,
    /// 用重跑读数再跑一遍判据的结论（判据不支持重跑时为 `None`）。
    pub decision: Option<Decision>,
    /// 没跑成的原因；跑成了但读数可疑（一块文字都没读出来）时也写在这里。
    pub note: Option<String>,
}

/// 重放一个任务目录（只重跑判据，不碰 OCR）。
pub fn replay_dir(dir: &Path, override_min_confidence: Option<f32>) -> Result<Replay, String> {
    replay_dir_with_ocr(dir, override_min_confidence, None)
}

/// 重放一个任务目录，并（可选）用真 OCR 引擎把 `raw/` 下的干净图重跑一遍。
///
/// `override_min_confidence` 给了就用它重跑所有判据（这才是"改阈值看结论变不变"
/// 的用法）；不给就用盘上各条自己记的值。真 OCR 只对**盘上没过**的步骤做：
/// 过了的步骤没有"卡在哪一层"可言，而每跑一次都要起一个进程。
pub fn replay_dir_with_ocr(
    dir: &Path,
    override_min_confidence: Option<f32>,
    ocr: Option<Engine>,
) -> Result<Replay, String> {
    let path = dir.join(EVENTS_FILE_NAME);
    let text = std::fs::read_to_string(&path)
        .map_err(|err| format!("读不到事件流 {}：{err}", path.display()))?;
    Ok(replay_with(dir, &text, override_min_confidence, ocr.as_ref()))
}

/// 重放一份事件流文本（测试与 [`replay_dir`] 共用这一条路）。
pub fn replay_text(text: &str, override_min_confidence: Option<f32>) -> Replay {
    replay_with(Path::new("."), text, override_min_confidence, None)
}

/// 重放的正体：`dir` 用来把事件里记的 `raw/` 相对路径接成真路径。
fn replay_with(
    dir: &Path,
    text: &str,
    override_min_confidence: Option<f32>,
    ocr: Option<&Engine>,
) -> Replay {
    let mut reads: Vec<ReadRow> = Vec::new();
    let mut rows: Vec<DecisionRow> = Vec::new();
    let mut event_count = 0usize;

    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        event_count += 1;
        match value.get("kind").and_then(Value::as_str) {
            Some("read") => {
                if let Some(row) = parse_read(&value) {
                    reads.push(row);
                }
            }
            Some("decision") => {
                if let Some(row) = parse_decision(&value) {
                    rows.push(row);
                }
            }
            // 认不出的 kind（将来加了新事件）跳过：读不懂的东西不该被当成坏数据。
            _ => {}
        }
    }

    let session = Session { dir, override_min_confidence, ocr };
    let decisions = rows.into_iter().map(|row| run(row, &mut reads, &session)).collect();
    Replay {
        event_count,
        decisions,
        ocr_engine: ocr.map(Engine::describe),
    }
}

/// 一轮重放要用的东西：任务目录（接 `raw/` 路径）、阈值覆盖、可选的真 OCR 引擎。
struct Session<'a> {
    dir: &'a Path,
    override_min_confidence: Option<f32>,
    ocr: Option<&'a Engine>,
}

/// 事件流里一条「看图」。
struct ReadRow {
    step: String,
    index: usize,
    boxes: Vec<TextBox>,
    /// 这一步的 OCR 原料（`raw/NN-*.png`，相对任务目录）；没做 OCR 时为 `None`。
    ocr_input: Option<String>,
    /// 已经被后面的判定配走了没有——一次「看图」只喂一次判定。
    taken: bool,
}

/// 事件流里一条「判定」（只留重跑用得上的那几个字段）。
struct DecisionRow {
    step: String,
    passed: bool,
    outcome: String,
    min_confidence: f32,
    replay: Option<ReplayInput>,
}

/// 重跑一条判定：判据（盘上那份读数）+ 真 OCR（`raw/` 那张干净图）+ 归因到层。
fn run(row: DecisionRow, reads: &mut [ReadRow], session: &Session<'_>) -> Replayed {
    let DecisionRow { step, passed, outcome, min_confidence, replay } = row;

    // 配那一帧：**同名、最近一次、还没被配走**的那一条。
    //
    // ★ 与界面（`replayView.ts` 的 `lastUnjudgedIndex`）是同一条规矩：重试会让
    // 同一个步骤名出现多次，真正做判断的是最后一帧。两处若不一致，会出现
    // "报告念的这一帧"与"界面显示的那一帧"不是同一张——而两张都来自真实数据。
    let matched = reads.iter_mut().rev().find(|read| read.step == step && !read.taken);
    let (frame, ocr_input) = match matched {
        Some(read) => {
            read.taken = true;
            (Some((read.index, read.boxes.clone())), read.ocr_input.clone())
        }
        None => (None, None),
    };

    let mut replayed = Replayed {
        step,
        read_index: frame.as_ref().map(|(index, _)| *index),
        box_count: frame.as_ref().map(|(_, boxes)| boxes.len()).unwrap_or(0),
        recorded_passed: passed,
        recorded_outcome: outcome,
        recorded_min_confidence: min_confidence,
        decision: None,
        note: None,
        ocr: None,
        attribution: None,
    };

    let Some((_, boxes)) = frame else {
        // 没有那一帧：判据重跑不了，读数也没有，归因更归不了——如实说缺的是哪一样。
        replayed.note = Some(format!(
            "事件流里没有与它同名的「看图」，没有可重跑的那一帧（步骤名「{}」）",
            replayed.step
        ));
        return replayed;
    };

    let confidence = session.override_min_confidence.unwrap_or(min_confidence);
    match replay.as_ref() {
        Some(input) => replayed.decision = Some(rerun(input, &boxes, confidence)),
        None => {
            replayed.note =
                Some("这条判据不支持离线重跑（事件流里 replay 写的是 null）".to_string())
        }
    }

    // 真 OCR 只对**盘上没过**的步骤做：过了的步骤没有"卡在哪一层"可言，
    // 而每跑一次都要起一个进程。
    if !passed {
        replayed.ocr = rerun_ocr(session, ocr_input.as_deref(), &boxes, replay.as_ref(), confidence);
    }
    let attribution = layer::attribute(&layer::Evidence {
        recorded_passed: passed,
        judged_on_recorded: replayed.decision.as_ref(),
        judged_on_fresh: replayed.ocr.as_ref().and_then(|ocr| ocr.decision.as_ref()),
        diff: replayed.ocr.as_ref().and_then(|ocr| ocr.diff.as_ref()),
    });
    replayed.attribution = attribution;
    replayed
}

/// 真 OCR 重跑：把这一步的干净输入图喂回引擎，再拿新读数跑一遍判据。
///
/// `recorded` 是盘上那份读数，比一比就是"读数变没变"。没做 OCR 的步骤返回
/// `None`——不是忘了做，而是那一步本来就没有 OCR 输入（事件里 `ocr_input` 是 null）。
fn rerun_ocr(
    session: &Session<'_>,
    ocr_input: Option<&str>,
    recorded: &[TextBox],
    replay: Option<&ReplayInput>,
    confidence: f32,
) -> Option<OcrRerun> {
    let engine = session.ocr?;
    let input = ocr_input?;
    let engine_desc = engine.describe();

    let (boxes, _raw) = match engine.run(session.dir, input) {
        Ok(result) => result,
        Err(err) => {
            return Some(OcrRerun {
                engine: engine_desc,
                input: input.to_string(),
                diff: None,
                box_count: 0,
                decision: None,
                note: Some(err),
            })
        }
    };

    // 一块都没读出来、盘上却有：这句警告比差异表更要紧——引擎没起来 / 参数被拒
    // 也是这个样子，而那种情况下"读数变了"的结论不算数。
    let note = (boxes.is_empty() && !recorded.is_empty()).then(|| {
        format!(
            "这次一块文字都没读出来（盘上有 {} 块）——先单独跑一次引擎确认它能用：{engine_desc}",
            recorded.len()
        )
    });
    Some(OcrRerun {
        engine: engine_desc,
        input: input.to_string(),
        diff: Some(diff_boxes(recorded, &boxes)),
        box_count: boxes.len(),
        decision: replay.map(|input| rerun(input, &boxes, confidence)),
        note,
    })
}

/// 把输入喂回**判据本身**重跑一次。
///
/// 这里一句判据都没有：下拉走 [`judge_dropdown`]，姓名匹配走匹配器的
/// `find_unique_exact_match_with_trail` —— 与真机跑的是同一个函数。
fn rerun(replay: &ReplayInput, boxes: &[TextBox], min_confidence: f32) -> Decision {
    match replay {
        ReplayInput::Dropdown { keyword, group_labels } => {
            judge_dropdown(boxes, keyword, group_labels, min_confidence).decision
        }
        ReplayInput::NameMatch { expected_name, relaxed } => {
            // 放宽与否是**装配期选的**，盘上记下来了，照它还原（`ReplayInput` 的理由）。
            let (result, trail) = if *relaxed {
                ContainsNameMatcher::default().find_unique_exact_match_with_trail(
                    expected_name,
                    boxes,
                    min_confidence,
                )
            } else {
                StrictContactMatcher::default().find_unique_exact_match_with_trail(
                    expected_name,
                    boxes,
                    min_confidence,
                )
            };
            name_match_decision(&trail, expected_name, min_confidence, &result)
        }
    }
}

fn parse_read(value: &Value) -> Option<ReadRow> {
    Some(ReadRow {
        step: value.get("step")?.as_str()?.to_string(),
        index: value.get("index")?.as_u64()? as usize,
        boxes: parse_boxes(value.get("boxes")?),
        // 没做 OCR 的步骤是 `null`（格式见 `task_diagnostics/events.rs::EventLog::read`）。
        ocr_input: value.get("ocr_input").and_then(Value::as_str).map(str::to_string),
        taken: false,
    })
}

fn parse_decision(value: &Value) -> Option<DecisionRow> {
    Some(DecisionRow {
        step: value.get("step")?.as_str()?.to_string(),
        passed: value.get("passed")?.as_bool()?,
        outcome: value.get("outcome")?.as_str()?.to_string(),
        min_confidence: value.get("min_confidence")?.as_f64()? as f32,
        replay: value.get("replay").and_then(parse_replay),
    })
}

fn parse_boxes(value: &Value) -> Vec<TextBox> {
    value.as_array().map(|items| items.iter().filter_map(parse_box).collect()).unwrap_or_default()
}

/// 事件流里的文字框坐标是 `x/y/w/h`（见 `boxes_json`），与 [`Rect`] 的字段名不同，
/// 所以这里逐字段搬一次，而不是靠 serde 直接反序列化。
fn parse_box(item: &Value) -> Option<TextBox> {
    Some(TextBox {
        text: item.get("text")?.as_str()?.to_string(),
        confidence: item.get("confidence")?.as_f64()? as f32,
        bounds: Rect {
            x: int(item, "x")?,
            y: int(item, "y")?,
            width: int(item, "w")?,
            height: int(item, "h")?,
        },
    })
}

fn int(item: &Value, key: &str) -> Option<i32> {
    i32::try_from(item.get(key)?.as_i64()?).ok()
}

fn parse_replay(value: &Value) -> Option<ReplayInput> {
    match value.get("kind")?.as_str()? {
        "dropdown" => Some(ReplayInput::Dropdown {
            keyword: value.get("keyword")?.as_str()?.to_string(),
            group_labels: value.get("group_labels")?.as_str()?.to_string(),
        }),
        "name_match" => Some(ReplayInput::NameMatch {
            expected_name: value.get("expected_name")?.as_str()?.to_string(),
            relaxed: value.get("relaxed")?.as_bool()?,
        }),
        // 认不出的 kind（将来加了新判据）当成"没法重跑"，不去猜它的输入。
        _ => None,
    }
}