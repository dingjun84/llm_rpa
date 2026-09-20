//! [`circle_points`] 的用例。
//!
//! ## 为什么这几条必须有
//!
//! 圆周轨迹是**纯计算 + 副作用**两段拼起来的，而错误只会出现在前半段：
//! 半径当成直径、角度少走一圈、步长算错 —— 这些错了以后，`SendInput` 照样
//! **返回成功**，光标照样在动，只是动出来的不是圆。等有人肉眼发现"这不像圆"
//! 时，已经隔了好几层，查起来要从输入管线一路倒推。
//!
//! 所以这里把"每个点都落在圆周上""首尾闭合""四个象限都走到"钉死。
//!
//! ⚠️ 这些用例**不碰光标**：`circle_points` 只做算术，不调用任何 Win32 原语。
//! 别在这边加"跑一圈看看"的用例 —— 那会在测试时抢走用户的鼠标。

use super::cursor::{circle_points, CircleTrace, CIRCLE_MIN_STEPS};
use std::time::Duration;

/// 一个点离圆心的距离。
fn distance_from(center: (i32, i32), point: (i32, i32)) -> f64 {
    let dx = (point.0 - center.0) as f64;
    let dy = (point.1 - center.1) as f64;
    (dx * dx + dy * dy).sqrt()
}

#[test]
fn every_point_sits_on_the_circle() {
    let center = (500, 400);
    let radius = 320;

    for steps in [CIRCLE_MIN_STEPS, 60, 200, 1000] {
        for point in circle_points(center, radius, steps, 1.0) {
            let actual = distance_from(center, point);
            let error = (actual - radius as f64).abs();
            // 两个轴各自四舍五入，最坏情况下把点推出去 √2/2 ≈ 0.707 像素；
            // 再留一点余量，免得边界上的取整方向一变就红。
            assert!(
                error <= 0.75,
                "steps={steps} 的点 {point:?} 离圆心 {actual}，与半径 {radius} 差 {error}"
            );
        }
    }
}

#[test]
fn the_loop_closes_at_the_starting_point() {
    // 起点是正右方。最后一步的角度正好是整圈，所以它必须**回到**正右方 ——
    // 这就是"闭合"的意思：不闭合的话，每跑一圈都会留一道缝，缝会越积越大。
    let center = (300, 700);
    let radius = 128;

    let points = circle_points(center, radius, 64, 1.0);
    assert_eq!(points.last().copied(), Some((center.0 + radius, center.1)));

    // 第一步不是正右方，而是**正右方往前一格** —— 否则最后一步会原地踏步。
    let first = points[0];
    assert_ne!(first, (center.0 + radius, center.1));
    assert!(first.0 > center.0 && first.1 > center.1, "第一步应当在右下方（屏幕坐标 y 向下）：{first:?}");
}

#[test]
fn all_four_quadrants_are_visited() {
    // 只走半个圆、或者方向反了，都能靠这条查出来。
    let center = (400, 400);
    let radius = 200;
    let points = circle_points(center, radius, CIRCLE_MIN_STEPS, 1.0);

    let right_down = points.iter().any(|p| p.0 > center.0 && p.1 > center.1);
    let left_down = points.iter().any(|p| p.0 < center.0 && p.1 > center.1);
    let left_up = points.iter().any(|p| p.0 < center.0 && p.1 < center.1);
    let right_up = points.iter().any(|p| p.0 > center.0 && p.1 < center.1);

    assert!(right_down && left_down && left_up && right_up, "四个象限没有都走到");
}

#[test]
fn point_count_matches_the_requested_steps() {
    // 步数少一个的后果是"接缝处缺一格"，肉眼看不出来但会让轨迹有轻微顿挫。
    for steps in [1u32, 2, CIRCLE_MIN_STEPS, 97] {
        assert_eq!(circle_points((0, 0), 50, steps, 1.0).len(), steps as usize);
    }
}

#[test]
fn zero_steps_is_empty_instead_of_panicking() {
    // 除零会算出 NaN，而 `NaN as i32` 是未定义行为（实际会得到 0 或 i32::MIN）。
    // 这里要求它明确返回空表。
    assert!(circle_points((10, 20), 30, 0, 1.0).is_empty());
}

// ── 多走的那一段（终点不回起点）─────────────────────────────────────────
//
// 这两条守的是**这条自检到底有没有用**：它存在的唯一理由就是让人确认
// "光标真的按轨迹走了"。终点若落在起点上，事后看光标位置与"它根本没动"
// 无法区分——那这条自检就白跑了，而且看不出白跑。

#[test]
fn the_extra_turn_lands_away_from_the_starting_point() {
    let center = (400, 300);
    let radius = 200;
    let steps = 64;
    let turns = 1.25;

    let points = circle_points(center, radius, steps, turns);
    assert_eq!(points.len(), steps as usize, "步数不受 turns 影响");

    let start = (center.0 + radius, center.1);
    let end = points.last().copied().expect("非空");

    // 终点必须**明显**离开起点：这就是"终点不回起点"的全部意义。
    // 半径 200、差 90°，两点的距离是 200√2 ≈ 283 像素——远大于取整误差。
    let gap = distance_from(start, end);
    assert!(
        gap > radius as f64,
        "终点 {end:?} 离起点 {start:?} 只有 {gap} 像素，太近了：{turns} 圈应当差 90°"
    );

    // 而且它仍然**落在圆上**（多走的那段也是圆的一部分，不是随手画的一条线）。
    let off = (distance_from(center, end) - radius as f64).abs();
    assert!(off <= 0.75, "终点 {end:?} 不在圆上：离圆心 {}，半径 {radius}", distance_from(center, end));
}

#[test]
fn end_distance_reports_where_the_cursor_actually_ended() {
    // 这条是"光标到底动没动"的**唯一实测判据**。它自己算错的话，
    // 界面上那个数就成了误导——比没有更糟。
    let trace = |end: (i32, i32)| CircleTrace {
        center: (100, 200),
        radius: 150,
        steps: 32,
        duration: Duration::from_millis(400),
        end,
    };

    // 走对了：终点在圆上 ⇒ 与半径一致。
    assert_eq!(trace((250, 200)).end_distance_px(), 150);
    // 3-4-5：离圆心 150。
    assert_eq!(trace((190, 320)).end_distance_px(), 150);
    // 根本没动：光标还在圆心 ⇒ 0。这正是要能一眼看出来的那种情况。
    assert_eq!(trace((100, 200)).end_distance_px(), 0);
}
