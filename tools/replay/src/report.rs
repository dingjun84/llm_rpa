//! 把重放结果写成**给人看**的文本。
//!
//! ## 为什么排版也算功能
//!
//! 这个工具的读者是"刚在真机上失败、现在要一眼看出为什么"的人。
//! 所以：每条判定一行小标题（成没成、几块文字、阈值多少），
//! **归因（卡在哪一层）放在最前面**——那是这条命令存在的理由，
//! 结论**单列一行**，"结论变了没有"**显式写出来**（而不是让人自己比对两段文字），
//! 候选逐块一行（置信度、结果、为什么）。
//!
//! ## 「重点」那几行只挑不判
//!
//! 未通过的判定后面会念几行"沾到这次要找的东西、却没通过"的候选。
//! 挑哪几行是**显示规则**（按判据自己的输入：关键词 / 分组标题 / 目标名去撞），
//! 而"为什么淘汰"一律**照抄判据给的理由**——这里不重写任何一句判据
//! （`CONVENTIONS.md` §1.3）。「沾边」这条规矩与归因共用一份：`layer.rs`。
//!
//! ⚠️ 输出里**不许出现 Markdown 加粗**（`CONVENTIONS.md` §8）：这是终端纯文本，
//! 两个星号会原样打出来。

use std::fmt::Write as _;
use std::path::Path;

use automation_core::Decision;

use crate::layer::{needles, touches_text};
use crate::{BoxDiff, Replay, Replayed};

/// 「重点」最多念几行：再多就把上面那张候选表本身淹了。
const FOCUS_LIMIT: usize = 4;

/// 读数差异最多列几行。差异多的时候通常是"两个倍率读出来完全不是一回事"，
/// 全列出来会把报告淹掉，而结论一句就说清了。
const DIFF_LIMIT: usize = 6;

/// 渲染整个重放结果。
pub fn render(dir: &Path, replay: &Replay, override_min_confidence: Option<f32>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "离线重放 {}", dir.display());
    match override_min_confidence {
        Some(value) => {
            let _ = writeln!(
                out,
                "事件 {} 条 · 判定 {} 次 · 阈值一律按 {value:.2} 重跑",
                replay.event_count,
                replay.decisions.len()
            );
        }
        None => {
            let _ = writeln!(
                out,
                "事件 {} 条 · 判定 {} 次 · 阈值用盘上各条自己记的值",
                replay.event_count,
                replay.decisions.len()
            );
        }
    }
    match replay.ocr_engine.as_deref() {
        Some(engine) => {
            let _ = writeln!(out, "真 OCR：{engine}（把 raw/ 下未标注的输入图喂回引擎再跑一遍）");
        }
        None => {
            let _ = writeln!(out, "真 OCR：没跑（只重跑判据；加 --ocr <引擎路径> 可打开）");
        }
    }

    if replay.decisions.is_empty() {
        let _ = writeln!(out, "\n这次任务的事件流里没有判定，没有可重跑的东西。");
        return out;
    }

    for (index, item) in replay.decisions.iter().enumerate() {
        render_one(&mut out, index + 1, item, override_min_confidence);
    }
    out
}

/// 一条判定。
fn render_one(
    out: &mut String,
    ordinal: usize,
    item: &Replayed,
    override_min_confidence: Option<f32>,
) {
    let _ = writeln!(
        out,
        "\n[{ordinal:02}] {}  {}",
        item.step,
        headline(item, override_min_confidence)
    );

    // 归因放在最前面：这一条命令存在的理由就是回答"卡在哪一层"。
    if let Some(attribution) = item.attribution.as_ref() {
        let _ = writeln!(out, "     归因：卡在{}", attribution.layer.label());
        let _ = writeln!(out, "       {}", attribution.why);
    }
    if let Some(ocr) = item.ocr.as_ref() {
        render_ocr(out, ocr);
    }

    let Some(decision) = item.decision.as_ref() else {
        // 没能重跑就**如实说**，并把盘上那句结论原文摆出来——
        // 补上一条空判定，比少一条结论更糟。
        let _ = writeln!(out, "     盘上那句：{}", item.recorded_outcome);
        if let Some(note) = item.note.as_ref() {
            let _ = writeln!(out, "     没能重跑：{note}");
        }
        return;
    };

    let _ = writeln!(out, "     问：{}", decision.question);
    let _ = writeln!(out, "     判据：{}", decision.rule);
    let _ = writeln!(out, "     重跑得到：{}", decision.outcome);

    if decision.passed == item.recorded_passed {
        let _ = writeln!(out, "     （与盘上记的一致）");
    } else {
        let _ = writeln!(
            out,
            "     结论变了：盘上记的是「{}」，重跑得到「{}」",
            label(item.recorded_passed),
            label(decision.passed)
        );
        let _ = writeln!(out, "     盘上那句：{}", item.recorded_outcome);
    }

    if decision.candidates.is_empty() {
        let _ = writeln!(out, "     候选：一块文字都没读到。");
        return;
    }

    let _ = writeln!(out, "     候选 {} 块：", decision.candidates.len());
    for (index, candidate) in decision.candidates.iter().enumerate() {
        let _ = writeln!(
            out,
            "       {:02}  {:.2}  {}  「{}」 —— {}",
            index + 1,
            candidate.confidence,
            if candidate.passed { "通过" } else { "淘汰" },
            candidate.text,
            candidate.reason
        );
    }

    render_focus(out, decision);
}

/// 真 OCR 重跑那一段：跑的什么引擎、喂的哪张图、读数差多少、这份读数下判据怎么判。
fn render_ocr(out: &mut String, ocr: &crate::OcrRerun) {
    let _ = writeln!(out, "     真 OCR：{} → {}", ocr.input, ocr.engine);
    let Some(diff) = ocr.diff.as_ref() else {
        if let Some(note) = ocr.note.as_ref() {
            let _ = writeln!(out, "       没跑成：{note}");
        }
        return;
    };

    let _ = writeln!(out, "       读数 {} 块；与盘上：{}", ocr.box_count, diff.summary());
    for line in diff_lines(diff) {
        let _ = writeln!(out, "         {line}");
    }
    match ocr.decision.as_ref() {
        Some(decision) => {
            let _ = writeln!(out, "       这份读数下的判据：{}（{}）", label(decision.passed), decision.outcome);
        }
        None => {
            let _ = writeln!(out, "       这份读数下的判据：没法重跑（事件流里 replay 是 null）");
        }
    }
    if let Some(note) = ocr.note.as_ref() {
        let _ = writeln!(out, "       ⚠️ {note}");
    }
}

/// 差异逐条列出来，最多 [`DIFF_LIMIT`] 行。
///
/// 一个 `-` 是盘上有、这次没了，`+` 是这次多出来的，`~` 是同一句话但读出来的
/// 置信度或位置变了——`~` 那几条最值得看：字还在，只是分数变了，
/// 正好是"卡在阈值"还是"卡在识别"的分界。
fn diff_lines(diff: &BoxDiff) -> Vec<String> {
    let mut lines = Vec::new();
    for (text, confidence) in diff.missing.iter() {
        lines.push(format!("- 「{text}」{confidence:.2}（盘上有，这次没读到）"));
    }
    for (text, confidence) in diff.added.iter() {
        lines.push(format!("+ 「{text}」{confidence:.2}（这次多出来的）"));
    }
    for changed in diff.changed.iter() {
        let mut line = format!(
            "~ 「{}」{:.2} → {:.2}",
            changed.text, changed.recorded, changed.fresh
        );
        if changed.shift != (0, 0) {
            let _ = write!(line, "（框挪了 {},{}）", changed.shift.0, changed.shift.1);
        }
        lines.push(line);
    }
    if lines.len() > DIFF_LIMIT {
        let extra = lines.len() - DIFF_LIMIT;
        lines.truncate(DIFF_LIMIT);
        lines.push(format!("……还有 {extra} 处差异"));
    }
    lines
}

/// 一行小标题：成没成、几块文字、用的哪个阈值。
fn headline(item: &Replayed, override_min_confidence: Option<f32>) -> String {
    let confidence = override_min_confidence.unwrap_or(item.recorded_min_confidence);
    let verdict = match item.decision.as_ref() {
        Some(decision) => label(decision.passed),
        None => "没能重跑",
    };
    let mut line = format!("{verdict} · {} 块文字 · 阈值 {confidence:.2}", item.box_count);
    if (item.recorded_min_confidence - confidence).abs() > f32::EPSILON {
        let _ = write!(line, "（盘上记的是 {:.2}）", item.recorded_min_confidence);
    }
    line
}

/// 未通过的判定后面念几行"沾到要找的东西、却没通过"的候选。
///
/// ⚠️ 只挑行、不判东西：理由照抄判据给的 [`automation_core::Verdict::reason`]。
/// 挑行的依据是**判据自己的输入**（关键词 / 分组标题 / 目标名），
/// 所以不是另立一套判据，只是换个方式把同一份输入摆出来。
fn render_focus(out: &mut String, decision: &Decision) {
    if decision.passed {
        return;
    }
    let needles = needles(decision);
    if needles.is_empty() {
        return;
    }

    let hits: Vec<String> = decision
        .candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| !candidate.passed)
        .filter(|(_, candidate)| touches_text(&needles, &candidate.text))
        .map(|(index, candidate)| {
            format!("{:02} 「{}」淘汰 —— {}", index + 1, candidate.text, candidate.reason)
        })
        .collect();

    if hits.is_empty() {
        // 反过来的情形也要如实说：这一帧压根没有沾边的文字，
        // 那问题就不在阈值上，而在"区域标错 / 界面没弹出来"那一类。
        let _ = writeln!(out, "     重点：这一帧没有一块文字沾到要找的东西（{}）。", needles.join(" / "));
        return;
    }

    // 按**画面顺序**念（不是按长短或分数重排）：这样与上面那张候选表、
    // 与 `steps/` 里那张图对得上，读的人不用在两处之间换坐标。
    let _ = writeln!(out, "     重点：");
    for line in hits.iter().take(FOCUS_LIMIT) {
        let _ = writeln!(out, "       {line}");
    }
    if hits.len() > FOCUS_LIMIT {
        let _ = writeln!(out, "       ……还有 {} 块", hits.len() - FOCUS_LIMIT);
    }
}

fn label(passed: bool) -> &'static str {
    if passed {
        "通过"
    } else {
        "未通过"
    }
}