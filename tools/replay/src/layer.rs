//! 归因到层：这一步**卡在哪一层**。
//!
//! ## 三层是怎么分的
//!
//! 一次「看一眼 → 判一下 → 点一下」里能出错的层只有三层（`docs/todo.md` T30 的「归因到层」）：
//!
//! 1. **OCR / 预处理层**：读出来的字就不对，或者两次读出来的不一样；
//! 2. **判据 / 阈值层**：字读出来了，判据把它淘汰了；
//! 3. **坐标 / 点击层**：这一帧里根本没有要找的东西——界面没到那一步
//!    （上一次点击没落到实处）、区域标错、或者那个面板压根没弹出来。
//!
//! ## 归因只用可核对的证据
//!
//! 依据只有三样：重跑读数与盘上那份的差异、判据在两份读数上分别成没成、
//! 以及"这一帧里有没有沾到要找的东西的文字"。
//!
//! ★ **"人眼能看到、OCR 却没读出来"这一类判断不在其中**：那需要期望值
//! （`expect.json`，T30 落地清单第 2、5 条，本轮不做）。没有期望值就把"读错"
//! 当成结论，是拿猜的东西冒充判据——而这份报告的用处正是"别再靠猜"。
//!
//! ## 「沾边」这件事只有一处
//!
//! 这次判定要找的东西（关键词 / 分组标题 / 目标姓名）由 [`needles`] 算一次，
//! 归因（[`attribute`]）与报告里那几行「重点」（`report.rs`）都用它，
//! 免得两处各写一套"算不算沾边"。

use automation_core::dropdown::{normalize_text, parse_search_contact_group_labels};
use automation_core::{Decision, ReplayInput};

use crate::ocr::BoxDiff;

/// 这一步卡在哪一层。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// 读出来的字就是错的 / 不可复现。
    Ocr,
    /// 字读出来了，判据或阈值把它淘汰了。
    Judge,
    /// 这一帧里没有要找的东西：界面 / 区域 / 点击的问题。
    Click,
    /// 没有可重跑的 OCR 读数，分不出是哪一层——**如实说**，不硬猜一个。
    Unknown,
}

impl Layer {
    pub fn label(self) -> &'static str {
        match self {
            Layer::Ocr => "OCR / 预处理层",
            Layer::Judge => "判据 / 阈值层",
            Layer::Click => "坐标 / 点击层",
            Layer::Unknown => "分不出层（缺 OCR 读数）",
        }
    }
}

/// 一条归因：卡在哪一层 + 一句为什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribution {
    pub layer: Layer,
    /// 为什么这么判。**只讲证据**，不写"应该是"。
    pub why: String,
}

/// 归因要用的证据。
pub struct Evidence<'a> {
    /// 盘上记的成没成。
    pub recorded_passed: bool,
    /// 用**盘上那份读数**跑判据的结论（`None` = 这条判据不支持重跑）。
    pub judged_on_recorded: Option<&'a Decision>,
    /// 用**重跑那份读数**跑判据的结论（没跑 OCR 时为 `None`）。
    pub judged_on_fresh: Option<&'a Decision>,
    /// 重跑读数与盘上那份的差异（没跑 OCR 时为 `None`）。
    pub diff: Option<&'a BoxDiff>,
}

/// 归因。盘上记的是**通过**时返回 `None`：过了的步骤没有"卡在哪一层"可言。
pub fn attribute(evidence: &Evidence<'_>) -> Option<Attribution> {
    if evidence.recorded_passed {
        return None;
    }

    // 盘上那份读数下判据就通过了：不是识别的问题，是当时那条判据 / 那个阈值。
    // （`--min-confidence` 覆盖阈值就落在这里：同一份读数、阈值一放就过。）
    if evidence.judged_on_recorded.map(|decision| decision.passed) == Some(true) {
        return Some(Attribution {
            layer: Layer::Judge,
            why: "同一份读数、换成现在这条判据与阈值就通过了 ⇒ 卡的是判据本身（阈值或规则），不是识别"
                .to_string(),
        });
    }

    // ★ 铁证：同一张干净图，换一份读数就通过了。
    if evidence.judged_on_fresh.map(|decision| decision.passed) == Some(true) {
        return Some(Attribution {
            layer: Layer::Ocr,
            why: "同一张未标注的输入图，这次读出来的字让判据通过了 ⇒ 判据没错，是当时那份读数把它挡下的"
                .to_string(),
        });
    }

    let Some(diff) = evidence.diff else {
        return Some(Attribution {
            layer: Layer::Unknown,
            why: "这一步没有可重跑的 OCR 读数（事件里 ocr_input 是 null，或引擎没跑成）⇒ \
                  能说的只是「判据在盘上那份读数下没通过」，分不出是识别还是判据。\
                  要分开得有一张干净图（raw/）能重跑"
                .to_string(),
        });
    };

    // 读数复现不了：判据拿到的输入本身就不稳。
    if !diff.is_empty() {
        return Some(Attribution {
            layer: Layer::Ocr,
            why: format!(
                "同一张未标注的输入图，重跑出来的读数与盘上那份不同（{}）⇒ \
                 判据拿到的输入不可复现，先定住这一层（倍率 / 缩放）再谈判据",
                diff.summary()
            ),
        });
    }

    // 读数逐块复现了，再分两种：这一帧里有没有要找的东西。
    if let Some(decision) = evidence.judged_on_recorded {
        if !touches(decision) {
            return Some(Attribution {
                layer: Layer::Click,
                why: "读数与盘上逐块一致，但这一帧里没有一块文字沾到要找的东西 ⇒ \
                      不是判据把对的淘汰了：先看 raw/ 上那张干净图——界面没到那一步\
                      （上一次点击没落到实处），或者那块区域标错了"
                    .to_string(),
            });
        }
    }
    Some(Attribution {
        layer: Layer::Judge,
        why: "读数与盘上逐块一致（同一张图重跑得到同一批文字），盘上那次也没通过 ⇒ \
              不是识别漂移；看上面的候选表，是阈值还是判据规则"
            .to_string(),
    })
}

/// 这次判定要找的东西：关键词 / 分组标题 / 目标联系人名（都先归一化）。
pub fn needles(decision: &Decision) -> Vec<String> {
    let raw = match decision.replay.as_ref() {
        Some(ReplayInput::Dropdown { keyword, group_labels }) => {
            let mut terms = vec![keyword.clone()];
            terms.extend(parse_search_contact_group_labels(group_labels));
            terms
        }
        Some(ReplayInput::NameMatch { expected_name, .. }) => vec![expected_name.clone()],
        None => Vec::new(),
    };
    raw.iter()
        .map(|term| normalize_text(term))
        .filter(|term| !term.is_empty())
        .collect()
}

/// 一句话里有没有沾到要找的东西（归一化之后按包含比）。
pub fn touches_text(needles: &[String], text: &str) -> bool {
    let text = normalize_text(text);
    needles.iter().any(|needle| text.contains(needle.as_str()))
}

/// 这一帧里有没有一块**没通过**的文字沾到要找的东西。
fn touches(decision: &Decision) -> bool {
    let needles = needles(decision);
    !needles.is_empty()
        && decision
            .candidates
            .iter()
            .any(|candidate| !candidate.passed && touches_text(&needles, &candidate.text))
}