//! `lib`（IPC 命令层）的用例。
//!
//! 拆成独立文件是因为主体已经接近文件行数上限，而**测试文件不限行数**
//! （`CONVENTIONS.md` §2）。组织方式跟 `calibration/tests.rs` 一致。

use super::*;

/// 没有缩放时（窗口宽度本来就没超过预览上限）框应当**原样**通过。
#[test]
fn a_frame_without_scaling_maps_one_to_one() {
    let rect = window_rect_from_preview([100, 50, 26, 26], [974, 734], 974, 734).unwrap();
    assert_eq!(
        (rect.x, rect.y, rect.width, rect.height),
        (100, 50, 26, 26)
    );
}

/// 窗口比预览上限宽时，框要按比例放大回原始分辨率。
///
/// 这一条盯的是"模板大小必须与真实图标一致"：换算错了，截出来的模板
/// 会是图标的一部分或者连着一圈背景，而匹配分数只是"偏低一点"。
#[test]
fn a_scaled_preview_maps_the_box_back_up() {
    // 2560x1440 的窗口被缩到 1280x720 做预览 ⇒ 比例 2。
    let rect = window_rect_from_preview([100, 50, 26, 26], [1280, 720], 2560, 1440).unwrap();
    assert_eq!((rect.x, rect.y), (200, 100));
    assert_eq!((rect.width, rect.height), (52, 52));
}

/// 两条边分别取整：宽高不能"用宽度乘比例"算出来，否则会差一个像素。
///
/// 用一个必然踩到取整边界的比例（1.5，框宽 1）：
///
/// - 分别取整：左边界 `round(1×1.5)=2`、右边界 `round(2×1.5)=3` ⇒ 宽 1（正确，
///   它盖住的正是画面里 `[2, 3)` 这一个像素）；
/// - 直接乘：`round(1×1.5)=2` ⇒ 宽 2，多吃了旁边一列像素。
///
/// 图标只有二十几个像素，多吃一列就是整条边都带着隔壁的背景。
#[test]
fn both_edges_are_rounded_independently() {
    let rect = window_rect_from_preview([1, 1, 1, 1], [100, 60], 150, 90).unwrap();
    assert_eq!(rect.x, 2);
    assert_eq!(
        rect.width, 1,
        "宽度必须由两条边相减得出，不能直接用宽度乘比例"
    );
    assert_eq!(rect.height, 1);
}

/// 越界的框只**平移**进画面，不缩尺寸。
///
/// 缩尺寸会把模板改小，而模板大小必须与图标严格一致：改小之后分数会掉下来，
/// 且没有任何提示告诉人"是坐标换算把它改小了"。
#[test]
fn an_out_of_bounds_box_is_shifted_not_shrunk() {
    // 比例 2，框右边界超出预览宽度 ⇒ 换算后越过画面右边缘。
    let rect = window_rect_from_preview([1270, 0, 20, 20], [1280, 720], 2560, 1440).unwrap();
    assert_eq!(rect.width, 40, "尺寸必须保持，不能被缩");
    assert_eq!(rect.x + rect.width, 2560, "整体平移到贴住右边缘");

    // 负数左边界：夹到 0。比例是 2，所以 40 个预览像素对应 80 个窗口像素——
    // 尺寸按比例走，夹的只是位置。
    let rect = window_rect_from_preview([-30, -30, 40, 40], [1280, 720], 2560, 1440).unwrap();
    assert_eq!((rect.x, rect.y), (0, 0));
    assert_eq!((rect.width, rect.height), (80, 80));
}

/// 换算后不足一个像素、或者比画面还大的框，一律明确报错。
#[test]
fn a_degenerate_or_oversized_box_is_refused() {
    assert!(window_rect_from_preview([0, 0, 0, 0], [1280, 720], 2560, 1440).is_err());
    assert!(window_rect_from_preview([0, 0, 3000, 20], [1280, 720], 2560, 1440).is_err());
    // 预览尺寸为 0：说明界面传了个没截过图的空值。
    assert!(window_rect_from_preview([0, 0, 10, 10], [0, 0], 2560, 1440).is_err());
}

/// 极端输入不能 panic（前端传的是原始 JSON 数字，什么都有可能）。
#[test]
fn absurd_numbers_are_rejected_without_panicking() {
    assert!(window_rect_from_preview(
        [i32::MAX, i32::MAX, i32::MAX, i32::MAX],
        [1280, 720],
        2560,
        1440
    )
    .is_err());
    assert!(window_rect_from_preview([i32::MIN, 0, 10, 10], [1280, 720], 2560, 1440).is_err());
}
