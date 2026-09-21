//! 单步标注图的渲染与命名。
//!
//! ## 为什么单独一个文件
//!
//! 这一段是**画图**的活：把一帧画面、要看的区域、读到的文字框叠成一张 PNG。
//! 它与「什么时候落盘、落多少张、收尾拼总图」不是一回事，
//! 而 `task_diagnostics.rs` 还要塞下事件流那条线（`docs/todo.md` T29）。
//!
//! 拆出去的另一个好处是这里的函数**不碰任何状态**：给同样的一帧、同样的时间戳，
//! 出来的 PNG 就该一模一样——排查时"那张图是什么时候画的"不该影响它长什么样。

use automation_core::Observation;

/// 一步的图片文件名：`03-搜索下拉识别.png`（序号 + 步骤名）。
pub(super) fn image_name(index: usize, label: &str) -> String {
    format!("{:02}-{}.png", index, file_label(label))
}

/// 一步**原始输出**的文件名：`03-搜索下拉识别.json`（与同一张图同一个序号）。
///
/// 与 [`image_name`] **共用同一个序号与步骤名**：`raw/` 下那对文件的用途就是
/// "把当时的输入图与它的读出结果配起来"，名字对不上就得靠猜谁配谁。
pub(super) fn raw_json_name(index: usize, label: &str) -> String {
    format!("{:02}-{}.json", index, file_label(label))
}

/// 画一张单步标注图（PNG 字节）；没有字体或渲染失败时返回 `None`。
pub(super) fn render_page(
    font: &vision::text::TextFont,
    stamp: &str,
    observation: &Observation<'_>,
) -> Option<Vec<u8>> {
    let step = vision::render::Step {
        label: observation.label,
        stamp,
        region: observation.region,
        frame: observation.frame,
        text_boxes: observation.text_boxes,
    };
    vision::render::annotate_step(&step, font)
        .ok()
        .and_then(|page| vision::pixels::encode_png(&page).ok())
}

/// 把步骤名变成能当文件名的一段：路径分隔符之类的换成下划线，其余（含中文）原样保留。
///
/// 为什么保留中文：这些文件名就是给人看的目录列表，写成 `01-step_3.png`
/// 还不如直接写 `01-搜索下拉列表.png`。
fn file_label(label: &str) -> String {
    let cleaned: String = label
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "step".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_name_keeps_chinese_and_defuses_separators() {
        assert_eq!(image_name(3, "搜索下拉识别"), "03-搜索下拉识别.png");
        assert_eq!(image_name(12, "a/b:c"), "12-a_b_c.png");
        assert_eq!(image_name(1, "   "), "01-step.png", "空名字要有个兜底，否则文件名只剩序号");
    }
}