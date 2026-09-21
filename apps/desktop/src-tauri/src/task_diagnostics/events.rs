//! 结构化事件流 `events.jsonl`：**一行一件事**，给界面「过程重放」与离线重放读。
//!
//! ## 它补的是什么
//!
//! 标注图能回答「程序看到了什么」，`task.log` 能回答「它走到哪一步了」，
//! 但**没有一个能回答「它为什么这么判」**。2026-09-21 的现场正是卡在这里：
//! 失败文案说下拉里没有「联系人」分组，而同一帧的文字列表里明明有三个字——
//! 是置信度不够还是归一化改写了它，哪里都没记，只能靠猜。
//!
//! 所以事件流记两种事，各自带上判据要的全部输入：
//!
//! - `read`：这一步**看到了什么**（区域、帧尺寸/指纹、每块文字的位置与置信度，
//!   以及那张标注图的相对路径）；
//! - `decision`：这一步**怎么判的**（按哪条判据、结论、每个候选过没过为什么，
//!   外加重跑这条判据要的输入）。
//!
//! ## 为什么是 JSONL 而不是一个大 JSON
//!
//! 每记一件事就**立刻**追加并 flush：任务被强杀时，已经走过的那几步仍在盘上
//! （同 `task_diagnostics.rs` 里那条理由）。也正因为如此，
//! 读的时候必须容忍**最后一行是半截**——半截行丢掉，前面的一行不落。
//!
//! ## 格式只在这里定义
//!
//! 字段名与含义写在这一处：界面（`TaskReplay.tsx`）与 `tools/replay` 都读它。
//! 改字段就改这里的 `v`（[`SCHEMA_VERSION`]），读的人靠它判断含义有没有变过。

use std::path::{Path, PathBuf};

use automation_core::{Decision, Observation, Rect, ReplayInput, TextBox, Verdict};
use serde_json::{json, Value};

use crate::task_log::{append_raw_line, now_stamp, EVENTS_FILE_NAME};

/// 事件流的格式版本。读的人靠它判断字段含义有没有变过。
///
/// ⚠️ **只加字段不算"含义变过"**：老读的人看到陌生的键就当没看见，结论不受影响
/// （`ocr_input` / `ocr_raw` 就是这么加上去的）。改了**已有**字段的含义才 +1，
/// 因为那才会让老读的人把话说错。
const SCHEMA_VERSION: u32 = 1;

/// 一个任务的事件流文件。
pub struct EventLog {
    path: PathBuf,
}

impl EventLog {
    /// `dir` 是这次任务的目录（`data/tasks/<任务ID>/`）。
    pub fn new(dir: &Path) -> Self {
        Self { path: dir.join(EVENTS_FILE_NAME) }
    }

    /// 记一次「看到了什么」。
    ///
    /// `image` 是这一步那张标注图在任务目录里的**相对路径**（`steps/03-xx.png`）：
    /// 界面拿它拼 asset URL 直接显示原图，不必把 PNG 以 base64 回传一遍。
    /// 时间戳由调用方给（与那张图用的是同一个），这样"图上写的时间"与"事件里的时间"
    /// 一定对得上——否则看图的人会怀疑这两条记录不是同一步。
    ///
    /// `ocr_input` / `ocr_raw` 指向 `raw/` 下的**原料**：前者是**未标注**的那张
    /// OCR 输入图，后者是引擎 stdout 的原文。两者一起才让"拿这一帧重跑一次 OCR"
    /// 成立——标注图上有框、有字，拿它重跑读出来的是另一套结果（`docs/todo.md` T30）。
    /// 没做 OCR 的步骤两者都是 `null`：那一步本来就没有 OCR 输入可谈。
    pub fn read(
        &self,
        index: usize,
        image: &str,
        stamp: &str,
        observation: &Observation<'_>,
        ocr_input: Option<&str>,
        ocr_raw: Option<&str>,
    ) {
        let line = json!({
            "v": SCHEMA_VERSION,
            "kind": "read",
            "t": stamp,
            "step": observation.label,
            "index": index,
            "image": image,
            "region": rect_json(observation.region),
            "frame": {
                "w": observation.frame.width,
                "h": observation.frame.height,
                "fingerprint": observation.frame.fingerprint,
            },
            "boxes": boxes_json(observation.text_boxes),
            "ocr_input": ocr_input,
            "ocr_raw": ocr_raw,
        });
        self.append(&line);
    }

    /// 记一次「怎么判的」。
    pub fn decide(&self, decision: &Decision) {
        let line = json!({
            "v": SCHEMA_VERSION,
            "kind": "decision",
            "t": now_stamp(),
            "step": decision.step,
            "question": decision.question,
            "rule": decision.rule,
            "outcome": decision.outcome,
            "passed": decision.passed,
            "min_confidence": confidence(decision.min_confidence),
            "replay": replay_json(decision.replay.as_ref()),
            "candidates": verdicts_json(&decision.candidates),
        });
        self.append(&line);
    }

    fn append(&self, value: &Value) {
        // `to_string` 出来是**紧凑 JSON**，本身不含换行；文字里的换行会被转义成 `\n`，
        // 所以"一行一条"这件事不会被候选文字里的换行破坏。
        if let Ok(line) = serde_json::to_string(value) {
            append_raw_line(&self.path, &line);
        }
    }
}

/// 读出一整个事件流。**解析不了的行直接跳过**：进程被强杀时最后一行可能是半截，
/// 而为了一个坏行把前面几十步全不显示，是把"看得见的线索"换成"什么都没有"。
pub fn read_events(path: &Path) -> Vec<Value> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines().filter_map(|line| serde_json::from_str(line).ok()).collect()
}

/// 置信度落盘前**收成三位小数**。
///
/// `f32` 的 0.61 实际存的是 0.6100000143051147，`serde_json` 会把它原样写成这一长串。
/// 事件流是给人读的（界面右侧那张候选表、`tools/replay` 的打印），
/// 一串"有效数字"只会让人怀疑是不是真读到了这么多位。
/// OCR 的置信度本来就到不了三位小数以外，收三位既不改判据、也不丢线索。
fn confidence(value: f32) -> f64 {
    (value as f64 * 1000.0).round() / 1000.0
}

fn rect_json(rect: Rect) -> Value {
    json!({ "x": rect.x, "y": rect.y, "w": rect.width, "h": rect.height })
}

/// 文字框的坐标是**本帧图像坐标**（原点即区域左上角），与图上画的框一致，
/// 界面直接按它叠在图上即可。
fn boxes_json(boxes: &[TextBox]) -> Value {
    Value::Array(
        boxes
            .iter()
            .map(|b| {
                json!({
                    "text": b.text,
                    "x": b.bounds.x,
                    "y": b.bounds.y,
                    "w": b.bounds.width,
                    "h": b.bounds.height,
                    "confidence": confidence(b.confidence),
                })
            })
            .collect(),
    )
}

fn verdicts_json(candidates: &[Verdict]) -> Value {
    Value::Array(
        candidates
            .iter()
            .map(|v| {
                json!({
                    "text": v.text,
                    "passed": v.passed,
                    "reason": v.reason,
                    "x": v.bounds.x,
                    "y": v.bounds.y,
                    "w": v.bounds.width,
                    "h": v.bounds.height,
                    "confidence": confidence(v.confidence),
                })
            })
            .collect(),
    )
}

/// 重放输入。**不支持重跑的判据写 `null`**：让读的人知道"这一条不是忘了记，
/// 而是它本来就没法离线重跑"，不要拿一个空对象假装有。
fn replay_json(replay: Option<&ReplayInput>) -> Value {
    match replay {
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
    }
}