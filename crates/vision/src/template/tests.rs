//! `template` 的用例。
//!
//! 拆成独立文件是因为主体已经接近文件行数上限，而**测试文件不限行数**
//! （`CONVENTIONS.md` §2）。组织方式跟 `calibration/tests.rs` 一致。

use super::*;

/// 确定性的伪随机图案。用固定种子的 LCG，避免测试依赖随机数实现。
fn noise(x: u32, y: u32, seed: u32) -> u8 {
    let mut value = x
        .wrapping_mul(73_856_093)
        .wrapping_add(y.wrapping_mul(19_349_663))
        .wrapping_add(seed.wrapping_mul(83_492_791));
    value ^= value >> 13;
    value = value.wrapping_mul(1_274_126_177);
    (value >> 8) as u8
}

fn pattern(x: u32, y: u32) -> [u8; 4] {
    let v = noise(x, y, 7);
    [v, v.wrapping_add(40), v.wrapping_add(90), 255]
}

fn template_at(frame: &Screenshot, x: i32, y: i32, w: i32, h: i32) -> IconTemplate {
    crop_template(frame, Rect { x, y, width: w, height: h }, "测试模板").unwrap()
}

/// 朴素版 NCC：直接用定义式，每个位置老老实实重算 w×h 次乘法。
///
/// 它存在的唯一目的是给生产实现当**参照物**。生产实现用了积分图求均值与方差、
/// 用了"分子 = Σ(T-T̄)·I"这个等价变形；这些一旦写错，分数只会悄悄偏一点点，
/// 而"看起来还挺高"是看不出偏了的。
fn naive_score(frame: &Screenshot, tpl: &IconTemplate, x: usize, y: usize) -> Option<f64> {
    let count = (tpl.width * tpl.height) as f64;
    let mut total = 0.0f64;
    let mut usable = 0usize;
    for channel in 0..CHANNELS {
        let mut t = Vec::new();
        let mut i = Vec::new();
        for ty in 0..tpl.height {
            for tx in 0..tpl.width {
                t.push(tpl.pixels[((ty * tpl.width + tx) * 4) as usize + channel] as f64);
                i.push(
                    frame.pixels[(((y as u32 + ty) * frame.width + x as u32 + tx) * 4) as usize
                        + channel] as f64,
                );
            }
        }
        let t_mean = t.iter().sum::<f64>() / count;
        let i_mean = i.iter().sum::<f64>() / count;
        let t_var: f64 = t.iter().map(|v| (v - t_mean).powi(2)).sum();
        let i_var: f64 = i.iter().map(|v| (v - i_mean).powi(2)).sum();
        if t_var <= FLAT_VARIANCE || i_var <= FLAT_VARIANCE {
            continue;
        }
        let numer: f64 = t
            .iter()
            .zip(i.iter())
            .map(|(a, b)| (a - t_mean) * (b - i_mean))
            .sum();
        total += numer / (t_var * i_var).sqrt();
        usable += 1;
    }
    if usable == 0 {
        None
    } else {
        Some(total / usable as f64)
    }
}

/// 把一帧与一张模板都拆成平面，供直接调用内部函数。
fn planes(frame: &Screenshot, template: &IconTemplate) -> (Vec<Plane>, Vec<Plane>) {
    (
        planes_from_bgra(&frame.pixels, frame.width, frame.height).unwrap(),
        planes_from_bgra(&template.pixels, template.width, template.height).unwrap(),
    )
}

#[test]
fn finds_the_exact_position_of_a_pasted_patch() {
    let frame = frame_of(120, 80, pattern);
    let template = template_at(&frame, 37, 22, 16, 12);

    let (bounds, score) = match_template(&frame, &template).unwrap().unwrap();
    assert_eq!((bounds.x, bounds.y), (37, 22));
    assert_eq!((bounds.width, bounds.height), (16, 12));
    assert!(score > 0.999, "同一块像素自比应当接近 1.0，实际 {score}");
}

#[test]
fn normalised_correlation_survives_brightness_and_contrast_changes() {
    let base = frame_of(120, 80, pattern);
    let template = template_at(&base, 37, 22, 16, 12);

    // 整体压暗并降低对比度：模拟窗口失去焦点 / 半透明主题。
    // 差值型度量（TM_SQDIFF）在这里会掉到不及格，NCC 不应该。
    let dimmed = frame_of(120, 80, |x, y| {
        let p = pattern(x, y);
        let f = |v: u8| ((v as f32 * 0.7) as u8).wrapping_add(20);
        [f(p[0]), f(p[1]), f(p[2]), 255]
    });

    let (bounds, score) = match_template(&dimmed, &template).unwrap().unwrap();
    assert_eq!((bounds.x, bounds.y), (37, 22), "亮度变化不该挪动命中位置");
    assert!(score > 0.99, "NCC 应当对亮度/对比度变化免疫，实际 {score}");
}

/// 生产实现（积分图 + 等价变形）必须与**定义式**逐位置一致。
///
/// 用一幅 40×30 的小画面 + 9×7 的模板，把每个候选位置都比一遍：
/// 这样"积分图边界差一列""方差用了错的窗口"这类错误无处可藏。
#[test]
fn the_production_score_matches_the_ncc_definition_at_every_position() {
    let frame = frame_of(40, 30, pattern);
    let template = template_at(&frame, 11, 9, 9, 7);
    let (hay, needle) = planes(&frame, &template);

    let tpl_w = needle[0].width;
    let tpl_h = needle[0].height;
    let count = (tpl_w * tpl_h) as f64;
    let mut tpl_zero = Vec::new();
    let mut tpl_var = [0.0f64; CHANNELS];
    for (channel, plane) in needle.iter().enumerate() {
        let sum: f64 = plane.values.iter().map(|v| *v as f64).sum();
        let mean = sum / count;
        tpl_var[channel] = plane
            .values
            .iter()
            .map(|v| (*v as f64 - mean).powi(2))
            .sum();
        tpl_zero.push(
            plane
                .values
                .iter()
                .map(|v| *v as f64 - mean)
                .collect::<Vec<f64>>(),
        );
    }

    let mut compared = 0usize;
    for y in 0..=(frame.height - template.height) as usize {
        for x in 0..=(frame.width - template.width) as usize {
            let expected = naive_score(&frame, &template, x, y);
            let actual =
                position_score(&hay, &tpl_zero, &tpl_var, x, y, tpl_w, tpl_h);
            match (expected, actual) {
                (Some(e), Some(a)) => {
                    assert!(
                        (e - a).abs() < 1e-9,
                        "位置 ({x}, {y})：定义式给 {e}，生产实现给 {a}"
                    );
                    compared += 1;
                }
                (None, None) => {}
                _ => panic!(
                    "位置 ({x}, {y}) 上「没定义」的判定不一致：定义式 {expected:?}，生产实现 {actual:?}"
                ),
            }
        }
    }
    assert!(compared > 500, "比较的位置太少（{compared}），这条用例没起到作用");
}

/// `best_match` 必须等于"逐位置求分数后取最大值"。
///
/// 它钉住的是"取最大值"这一步：起点、终点、以及最大值的位置。
/// 上一条用例钉的是分数本身对不对，两条合起来才覆盖整条路径。
#[test]
fn best_match_is_the_maximum_over_every_position() {
    let frame = frame_of(48, 36, pattern);
    let template = template_at(&frame, 13, 7, 10, 9);
    let (hay, needle) = planes(&frame, &template);
    let (bounds, score) = best_match(&hay, &needle).unwrap();

    let mut expected: Option<(usize, usize, f64)> = None;
    for y in 0..=(frame.height - template.height) as usize {
        for x in 0..=(frame.width - template.width) as usize {
            let value = naive_score(&frame, &template, x, y).unwrap();
            if expected.map(|(_, _, best)| value > best).unwrap_or(true) {
                expected = Some((x, y, value));
            }
        }
    }
    let (ex, ey, ev) = expected.unwrap();
    assert_eq!((bounds.x, bounds.y), (ex as i32, ey as i32));
    assert!((score as f64 - ev).abs() < 1e-6, "分数应当与逐位置最大值一致");
    assert_eq!((bounds.width, bounds.height), (10, 9));
}

/// 颜色必须参与判断，不能只比亮度。
///
/// 构造两块**通道均值完全相同、只有色相不同**的图案（B 与 R 互换）。
/// 只算灰度的话两者一模一样，实现会在两个位置之间随便挑一个；
/// 三个通道各算一遍再平均，才能分辨出哪一块才是模板那一块。
#[test]
fn colour_is_used_not_only_luminance() {
    let frame = frame_of(60, 20, |x, y| {
        let in_a = (10..22).contains(&x);
        let in_b = (40..52).contains(&x);
        if !in_a && !in_b {
            return [30, 30, 30, 255];
        }
        let local_x = if in_a { x - 10 } else { x - 40 };
        let v = noise(local_x, y, 3);
        let w = noise(local_x, y, 5);
        // A: B=v, G=w, R=w   B: B=w, G=w, R=v —— 均值都是 (v+2w)/3。
        if in_a {
            [v, w, w, 255]
        } else {
            [w, w, v, 255]
        }
    });

    let a = template_at(&frame, 10, 4, 12, 12);
    let b = template_at(&frame, 40, 4, 12, 12);

    let (bounds, score) = match_template(&frame, &a).unwrap().unwrap();
    assert_eq!((bounds.x, bounds.y), (10, 4));
    assert!(score > 0.99);

    // 关键的一半：模板 B 必须命中右边那块。只比亮度的实现会在
    // 左边先撞见一个"完全相同"的位置，于是选错。
    let (bounds, score) = match_template(&frame, &b).unwrap().unwrap();
    assert_eq!((bounds.x, bounds.y), (40, 4), "颜色不同就该区分得开");
    assert!(score > 0.99);
}

#[test]
fn a_flat_template_is_not_a_match() {
    let frame = frame_of(60, 40, pattern);
    let flat = IconTemplate {
        label: "纯色".into(),
        pixels: [128u8, 128, 128, 255].repeat(100),
        width: 10,
        height: 10,
    };
    assert!(match_template(&frame, &flat).unwrap().is_none());
}

#[test]
fn a_template_larger_than_the_frame_is_not_a_match() {
    let frame = frame_of(20, 20, pattern);
    let big = IconTemplate {
        label: "比画面还大".into(),
        pixels: vec![0; 40 * 40 * 4],
        width: 40,
        height: 40,
    };
    assert!(match_template(&frame, &big).unwrap().is_none());
}

#[test]
fn the_locator_refuses_to_guess_below_the_threshold() {
    let frame = frame_of(80, 40, pattern);
    // 模板取自另一张完全无关的图 ⇒ 分数必然很低。
    let other = frame_of(80, 40, |x, y| {
        let v = noise(x, y, 991);
        [v, v, v, 255]
    });
    let template = template_at(&other, 5, 5, 12, 12);

    let err = TemplateLocator
        .locate(&frame, &IconQuery::new(&[template], 0.8))
        .unwrap_err();
    assert!(
        matches!(err, AutomationError::AmbiguousVision(_)),
        "低分必须转人工而不是硬用，实际是 {err:?}"
    );
    assert!(err.requires_human_review());
}

#[test]
fn the_locator_picks_the_best_of_several_templates() {
    let frame = frame_of(80, 40, pattern);
    let wrong = {
        let other = frame_of(80, 40, |x, y| [noise(x, y, 991), 0, 0, 255]);
        template_at(&other, 5, 5, 12, 12)
    };
    let right = template_at(&frame, 44, 12, 12, 12);

    let found = TemplateLocator
        .locate(&frame, &IconQuery::new(&[wrong, right], 0.9))
        .unwrap();
    assert_eq!(found.template_index, 1, "应当选中真正对得上的那一张");
    assert_eq!((found.bounds.x, found.bounds.y), (44, 12));
    assert!(found.score > 0.99);
}

#[test]
fn the_locator_refuses_an_empty_template_list() {
    let frame = frame_of(40, 40, pattern);
    let err = TemplateLocator.locate(&frame, &IconQuery::new(&[], 0.8)).unwrap_err();
    // 配置缺失必须报错，不能当成"这里没有图标"静默跳过。
    assert!(matches!(err, AutomationError::NeedsHumanReview(_)), "实际是 {err:?}");
}

/// 真机尺寸下的一次匹配耗时（只打印，不断言）。
///
/// 为什么要量：默认导航条是"窗口宽 7.5% × 整高"，模板 26×26 时
/// 位置数约 3.4 万、每个位置 676 次乘加。这个量级在 debug 构建下
/// 是"几十毫秒"还是"几秒"，直接决定了这一步能不能放在任务主路径上。
#[test]
fn measure_a_realistic_navigation_strip() {
    // 974x734 窗口下实测的导航条：73 宽、整高。
    let frame = frame_of(73, 734, pattern);
    let template = template_at(&frame, 8, 40, 26, 26);

    let started = std::time::Instant::now();
    let found = match_template(&frame, &template).unwrap().unwrap();
    let elapsed = started.elapsed();

    assert_eq!((found.0.x, found.0.y), (8, 40));
    println!(
        "导航条 73x734 + 26x26 模板：{elapsed:?}（分数 {:.4}）",
        found.1
    );
}

#[test]
fn crop_template_reads_the_requested_pixels() {
    let frame = frame_of(20, 10, pattern);
    let template = crop_template(&frame, Rect { x: 3, y: 2, width: 4, height: 3 }, "t").unwrap();
    assert_eq!((template.width, template.height), (4, 3));
    assert_eq!(template.pixels.len(), 4 * 3 * 4);
    assert_eq!(template.pixels[0], frame.pixels[((2 * 20 + 3) * 4) as usize]);
}

#[test]
fn crop_template_refuses_to_leave_the_frame() {
    let frame = frame_of(20, 10, pattern);
    assert!(matches!(
        crop_template(&frame, Rect { x: 18, y: 0, width: 4, height: 4 }, "t"),
        Err(VisionError::CropOutOfBounds)
    ));
    assert!(matches!(
        crop_template(&frame, Rect { x: 0, y: 0, width: 0, height: 4 }, "t"),
        Err(VisionError::CropOutOfBounds)
    ));
}

#[test]
fn load_icon_template_round_trips_a_png_and_guards_the_size() {
    let frame = frame_of(200, 60, pattern);
    let dir = std::env::temp_dir().join("rpa-llm-template-test");
    std::fs::create_dir_all(&dir).unwrap();

    // 正常尺寸：载入后像素与裁出来的完全一致（BGRA 顺序也要对）。
    let source = template_at(&frame, 30, 10, 16, 14);
    let path = dir.join("icon.png");
    let rgba = crate::pixels::to_rgba(&Screenshot {
        pixels: source.pixels.clone(),
        width: source.width,
        height: source.height,
        captured_at: std::time::SystemTime::now(),
        fingerprint: String::new(),
    })
    .unwrap();
    std::fs::write(&path, crate::pixels::encode_png(&rgba).unwrap()).unwrap();

    let loaded = load_icon_template(&path, "图标").unwrap();
    assert_eq!(loaded.label, "图标");
    assert_eq!((loaded.width, loaded.height), (source.width, source.height));
    assert_eq!(loaded.pixels, source.pixels, "载入后必须与原始 BGRA 逐字节一致");

    // 太小 / 太大都必须**当场报错**，而不是留到任务里表现为"分数很低"。
    let small = dir.join("small.png");
    std::fs::write(
        &small,
        crate::pixels::encode_png(&image::RgbaImage::new(2, 2)).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        load_icon_template(&small, "太小"),
        Err(VisionError::TemplateTooSmall { .. })
    ));

    let huge = dir.join("huge.png");
    std::fs::write(
        &huge,
        crate::pixels::encode_png(&image::RgbaImage::new(200, 40)).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        load_icon_template(&huge, "太大"),
        Err(VisionError::TemplateTooLarge { .. })
    ));

    // 文件不存在时要报 IO/解码错误，而不是当成"空模板"。
    // 而且**要带上是哪一张**：一个图标名底下可以有多张图，
    // 只说"图像编解码失败"没法告诉人去修哪一张。
    let err = load_icon_template(&dir.join("missing.png"), "缺失").unwrap_err();
    assert!(
        err.to_string().contains("缺失"),
        "读不出来的报错必须带上标签，实际是：{err}"
    );
}
