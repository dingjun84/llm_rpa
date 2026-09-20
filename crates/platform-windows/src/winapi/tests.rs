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

use super::cursor::{circle_points, CIRCLE_MIN_STEPS};

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
        for point in circle_points(center, radius, steps) {
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

    let points = circle_points(center, radius, 64);
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
    let points = circle_points(center, radius, CIRCLE_MIN_STEPS);

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
        assert_eq!(circle_points((0, 0), 50, steps).len(), steps as usize);
    }
}

#[test]
fn zero_steps_is_empty_instead_of_panicking() {
    // 除零会算出 NaN，而 `NaN as i32` 是未定义行为（实际会得到 0 或 i32::MIN）。
    // 这里要求它明确返回空表。
    assert!(circle_points((10, 20), 30, 0).is_empty());
}
