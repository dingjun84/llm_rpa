//! 会话列表发送 / 通讯录搜索发送两条工作流共用的**纯判据**（无 I/O，可单测）。
//!
//! - 输入框里是否已有正文、发送后是否已清空：[`boxes_contain_text`]
//! - 同名歧义：[`count_same_name`]
//! - 滚动后列表指纹没变：到底了还是卡死了：[`classify_scroll`]
//! - 送达判据（与旧 `message.rs::verify_delivery` 同口径）：[`judge_delivery`]
//! - 点击前复检：在新一帧里找「原坐标附近」那一行：[`nearest_row`]
//! - 会话标题带的锚点：[`header_anchor`]

use crate::dropdown::normalize_text;
use crate::ports::{Rect, TextBox};

/// 输入框 OCR 只比对正文前这么多个字：长正文在输入框里会折行 / 滚出可见区。
pub(super) const INPUT_PROBE_CHARS: usize = 20;

/// 把文字框按阅读序（上→下、左→右）拼起来，去掉空白。
fn joined_reading_order(boxes: &[TextBox], min_conf: f32) -> String {
    let mut sorted: Vec<&TextBox> = boxes.iter().filter(|b| b.confidence >= min_conf).collect();
    sorted.sort_by_key(|b| (b.bounds.y, b.bounds.x));
    sorted.iter().map(|b| normalize_text(&b.text)).collect()
}

/// 正文前 [`INPUT_PROBE_CHARS`] 个字（去空白后）是否出现在 `boxes` 拼出的文字里。
pub(super) fn boxes_contain_text(boxes: &[TextBox], wanted: &str) -> bool {
    let want = normalize_text(wanted);
    if want.is_empty() {
        return false;
    }
    let probe: String = want.chars().take(INPUT_PROBE_CHARS).collect();
    joined_reading_order(boxes, 0.0).contains(&probe)
}

/// 首行 / 文字框与目标名**逐字相等**（去空白）的个数。>1 即同名歧义。
pub(super) fn count_same_name<'a>(texts: impl IntoIterator<Item = &'a str>, name: &str) -> usize {
    let want = normalize_text(name);
    if want.is_empty() {
        return 0;
    }
    texts.into_iter().filter(|t| normalize_text(t) == want).count()
}

/// 一次滚动之后列表区的结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScrollOutcome {
    /// 指纹变了：列表确实动了。
    Moved,
    /// 指纹没变、客户端仍响应：滚到头了。
    ReachedEnd,
    /// 指纹没变且系统判定客户端未响应：界面卡死，不能当"到底"。
    Frozen,
}

/// `responsive` 只在指纹没变时才有意义；读不到（`None`）按"仍响应"处理——诊断不改结论。
pub(super) fn classify_scroll(before: &str, after: &str, responsive: Option<bool>) -> ScrollOutcome {
    if before != after {
        ScrollOutcome::Moved
    } else if responsive == Some(false) {
        ScrollOutcome::Frozen
    } else {
        ScrollOutcome::ReachedEnd
    }
}

/// 送达结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DeliveryVerdict {
    Confirmed,
    /// 聊天区指纹相对发送前没变。
    Unchanged,
    /// 画面变了，但 OCR 没读到正文。
    TextMissing,
}

/// 送达判据：**画面变了且读得到正文**，缺一不可（同旧 `verify_delivery`）。
pub(super) fn judge_delivery(
    before_fp: &str,
    after_fp: &str,
    boxes: &[TextBox],
    wanted: &str,
    min_conf: f32,
) -> DeliveryVerdict {
    if before_fp == after_fp {
        return DeliveryVerdict::Unchanged;
    }
    let want = normalize_text(wanted);
    if !want.is_empty() && joined_reading_order(boxes, min_conf).contains(&want) {
        DeliveryVerdict::Confirmed
    } else {
        DeliveryVerdict::TextMissing
    }
}

/// 点击前复检：在 `rows` 里找与 `prev` 垂直中心相差不超过半行高、且水平有重叠的最近一行。
pub(super) fn nearest_row(rows: &[Rect], prev: Rect) -> Option<usize> {
    let pcy = prev.y + prev.height / 2;
    let tol = (prev.height / 2).max(1);
    rows.iter()
        .enumerate()
        .filter(|(_, r)| r.x < prev.x + prev.width && prev.x < r.x + r.width)
        .map(|(i, r)| (i, (r.y + r.height / 2 - pcy).abs()))
        .filter(|(_, d)| *d <= tol)
        .min_by_key(|(_, d)| *d)
        .map(|(i, _)| i)
}

/// 给 `forward_verify_chat_header` 的"命中框"：让标题带从列表右缘起、到窗口右缘，
/// 高约窗口上 1/4——不会把左侧列表里那一行同名文字也圈进来。
pub(super) fn header_anchor(window: Rect, list_right: i32) -> Rect {
    Rect {
        x: list_right.clamp(window.x, window.x + window.width - 1),
        y: window.y,
        width: 0,
        height: (window.height / 8).max(20),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tb(text: &str, x: i32, y: i32) -> TextBox {
        TextBox { text: text.into(), bounds: Rect { x, y, width: 50, height: 20 }, confidence: 0.9 }
    }
    fn r(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, width: w, height: h }
    }

    #[test]
    fn input_text_matches_across_wrapped_lines() {
        let boxes = vec![tb("世界", 0, 30), tb("你好 ", 0, 0)];
        assert!(boxes_contain_text(&boxes, "你好世界"));
        assert!(!boxes_contain_text(&boxes, "再见"));
        assert!(!boxes_contain_text(&boxes, "  "));
    }

    #[test]
    fn long_text_only_needs_prefix() {
        let long = "一".repeat(INPUT_PROBE_CHARS) + "尾巴看不见";
        let boxes = vec![tb(&"一".repeat(INPUT_PROBE_CHARS), 0, 0)];
        assert!(boxes_contain_text(&boxes, &long));
    }

    #[test]
    fn same_name_counts_exact_only() {
        let t = ["张三", "张三丰", " 张 三", "李四"];
        assert_eq!(count_same_name(t.iter().copied(), "张三"), 2);
        assert_eq!(count_same_name(t.iter().copied(), ""), 0);
    }

    #[test]
    fn scroll_classification() {
        assert_eq!(classify_scroll("a", "b", Some(false)), ScrollOutcome::Moved);
        assert_eq!(classify_scroll("a", "a", Some(true)), ScrollOutcome::ReachedEnd);
        assert_eq!(classify_scroll("a", "a", None), ScrollOutcome::ReachedEnd);
        assert_eq!(classify_scroll("a", "a", Some(false)), ScrollOutcome::Frozen);
    }

    #[test]
    fn delivery_needs_change_and_text() {
        let boxes = vec![tb("你好", 0, 0)];
        assert_eq!(judge_delivery("a", "a", &boxes, "你好", 0.5), DeliveryVerdict::Unchanged);
        assert_eq!(judge_delivery("a", "b", &boxes, "再见", 0.5), DeliveryVerdict::TextMissing);
        assert_eq!(judge_delivery("a", "b", &boxes, "你好", 0.5), DeliveryVerdict::Confirmed);
        assert_eq!(judge_delivery("a", "b", &boxes, "你好", 0.95), DeliveryVerdict::TextMissing);
    }

    #[test]
    fn nearest_row_within_half_height() {
        let rows = [r(0, 0, 100, 40), r(0, 40, 100, 40), r(0, 80, 100, 40)];
        assert_eq!(nearest_row(&rows, r(0, 45, 100, 40)), Some(1));
        assert_eq!(nearest_row(&rows, r(0, 200, 100, 40)), None);
        assert_eq!(nearest_row(&rows, r(300, 40, 100, 40)), None);
    }

    #[test]
    fn header_anchor_starts_at_list_right() {
        let a = header_anchor(r(100, 50, 800, 600), 400);
        assert_eq!((a.x, a.y, a.height), (400, 50, 75));
        assert_eq!(header_anchor(r(100, 50, 800, 600), 5000).x, 899);
    }
}
