//! 把一帧识别到的文字压成**一行**，供事后回答「OCR 到底读成了什么」。
//!
//! ## 为什么它不在 `runner/mod.rs` 里
//!
//! 使用它的地方有三处（搜索下拉的失败文案、列表扫描的日志、离线重放），
//! 而它本身与"编排"没有关系——纯粹是一段**给人看的格式化**。
//! 判据（搜索下拉那一套）搬进 [`crate::dropdown`] 之后也要用它，
//! 留在 `runner` 里就会让一个纯函数被两处依赖倒挂。

use crate::ports::TextBox;

/// 一帧最多记多少个文字块、总共多少个字符。
///
/// 取值依据：联系人列表一屏最多也就 20 来行、每行名字 10~20 字，
/// 24 块 / 600 字符足够覆盖一整屏。再多出来的多半是乱码碎块。
const OCR_LOG_MAX_BLOCKS: usize = 24;
const OCR_LOG_MAX_CHARS: usize = 600;

/// 把一帧识别到的文字拼成一行，供事后回答「OCR 到底读成了什么」。
///
/// 为什么要截断：一帧乱码可能识别出上百个碎块，全写进日志会把真正有用的那几行
/// 淹掉。**截断会显式标出来**（写一个 `…`），不是悄悄丢掉——否则
/// 「本来只读到 24 块」和「读到 200 块、这里只显示 24 块」看起来一模一样，
/// 而这两种情况的含义完全不同。
pub fn describe_candidates(candidates: &[TextBox]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut used = 0usize;
    for item in candidates.iter().take(OCR_LOG_MAX_BLOCKS) {
        let text = item.text.trim();
        if text.is_empty() {
            continue;
        }
        // 识别结果里可能带换行，压成字面量 `\n`，保住日志「一行一条」的格式。
        let text = text.replace('\n', "\\n");
        let len = text.chars().count();
        if used + len > OCR_LOG_MAX_CHARS {
            parts.push("…".to_string());
            break;
        }
        used += len;
        parts.push(text);
    }
    if candidates.len() > OCR_LOG_MAX_BLOCKS {
        parts.push(format!(
            "（共 {} 块，只列前 {}）",
            candidates.len(),
            OCR_LOG_MAX_BLOCKS
        ));
    }
    parts.join(" / ")
}