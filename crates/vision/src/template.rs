//! 模板匹配：在一帧局部截图里找出一个小图（图标）的位置。
//!
//! ## 算法
//!
//! 逐位置计算**归一化互相关**（NCC），即 OpenCV `cv::matchTemplate` 的
//! `TM_CCOEFF_NORMED`：
//!
//! ```text
//!          Σ (T - T̄)(I - Ī)
//! R = ─────────────────────────────
//!      √( Σ (T - T̄)² · Σ (I - Ī)² )
//! ```
//!
//! 这个量对**整体亮度偏移**与**对比度缩放**都免疫。这不是数学上的洁癖，
//! 是真实界面上的必需品：窗口失去焦点时整片会变灰（亮度整体下移）、
//! 半透明主题会让对比度变化，而 `TM_SQDIFF` 这类差值型度量会把这些
//! 统统算成"不像"，于是同一个图标在窗口失焦时匹配分数掉到及格线以下。
//!
//! 彩色图按 **B/G/R 三个通道各算一遍再取平均**，而不是先转灰度：
//! 左侧导航栏那排图标是同一种线条风格的**单色**图标，形状彼此相近、
//! 只有内容和颜色不同；丢掉颜色等于主动扔掉一半判别力。
//!
//! ## 为什么不用 OpenCV
//!
//! 见 `docs/todo.md` T9。简要版：本机没有 OpenCV，而为了一个几十行的算法
//! 引入一个需要 CMake + LLVM 才能构建的 C++ 原生依赖，对这个仓库不划算
//! （它已经反复吃过原生依赖与链接缓存的苦）。这里按 OpenCV 的公式实现同一套
//! 数学，并且把"怎么匹配"与"谁来匹配"分开——[`automation_core::IconLocator`]
//! 是端口，将来真要换成 OpenCV，换的是端口实现，调用方一行都不用动。
//!
//! ## 性能
//!
//! 一次匹配的开销是 `搜索位置数 × 模板像素数 × 通道数`。以默认的导航条
//! （窗口的左侧 7.5% 宽、整高）与 26×26 的模板为例：约 3.4 万个位置 ×
//! 676 像素 × 3 通道 ≈ 7000 万次乘加，debug 构建下是**几百毫秒**量级。
//!
//! 这里刻意**不做**近似加速（抽稀采样、先粗后精、子集预筛）：它们的共同代价是
//! "某些位置永远不会被认真比一遍"，而这类漏检**不会报错**，只会表现为
//! "分数不高，转人工"——那时人只会去调阈值，找不到真原因。
//!
//! 真正能降开销的旋钮是**缩小搜索区**（`RunnerConfig.nav_strip`）：
//! 高度减半就快一倍。而这一步在每次任务里只跑一次，几百毫秒完全可以接受。
//!
//! 唯一用到的加速是**积分图**（前缀和）：窗口的像素和与平方和 O(1) 取得，
//! 用来算 Ī 与分母。它是精确的，不改变任何一个位置的分数。

use std::path::Path;

use automation_core::{
    AutomationError, IconLocator, IconMatch, IconQuery, IconTemplate, Point, Rect, Screenshot,
};

use crate::{VisionError, VisionResult};

/// 参与匹配的通道数：B、G、R（跳过 A）。
const CHANNELS: usize = 3;

/// 模板允许的最小边长（像素）。
///
/// 低于这个尺寸的"图标"更可能是截图时手抖截到的一个点。
/// 与其让它在任务里以一个很低的匹配分数表现出来（那时人只会去怀疑阈值），
/// 不如在载入时就报错说清"模板太小了"。
pub const MIN_TEMPLATE_SIDE: u32 = 4;

/// 模板允许的最大边长（像素）。
///
/// 导航图标实测约 20–40px。这个上限是**防御性的**，不是"推荐值"：
/// 模板边长每翻一倍，匹配开销翻四倍。有人误把整屏截图当模板传进来时，
/// 这个上限会把它拦成一条明确的报错，而不是让任务卡住几分钟毫无输出。
pub const MAX_TEMPLATE_SIDE: u32 = 128;

/// 判定"这个通道是纯色的"的方差阈值。
///
/// 纯色模板在 NCC 里没有定义（分母为 0），必须**跳过**而不是当成 0 分：
/// 当成 0 分会把一个本来能用的双色模板整体拉低。
const FLAT_VARIANCE: f64 = 1e-6;

/// 失败信息里每个模板报几个候选。
///
/// 三个够分辨"有一个明显的峰"和"到处都差不多"，再多只是把日志拉长——
/// 人看的是"第二名离第一名有多远"，不是完整分布。
const DIAGNOSTIC_PEAKS: usize = 3;

/// 单通道像素平面，附带两张积分图。
#[derive(Debug, Clone)]
struct Plane {
    width: usize,
    height: usize,
    values: Vec<f32>,
    /// 前缀和，尺寸 `(width+1) × (height+1)`，首行与首列恒为 0。
    sat: Vec<f64>,
    /// 平方的前缀和，同样尺寸。
    sat_sq: Vec<f64>,
}

impl Plane {
    fn new(width: usize, height: usize, values: Vec<f32>) -> Self {
        let stride = width + 1;
        let mut sat = vec![0.0f64; stride * (height + 1)];
        let mut sat_sq = vec![0.0f64; stride * (height + 1)];
        for y in 0..height {
            let mut row = 0.0f64;
            let mut row_sq = 0.0f64;
            for x in 0..width {
                let value = values[y * width + x] as f64;
                row += value;
                row_sq += value * value;
                sat[(y + 1) * stride + x + 1] = sat[y * stride + x + 1] + row;
                sat_sq[(y + 1) * stride + x + 1] = sat_sq[y * stride + x + 1] + row_sq;
            }
        }
        Self { width, height, values, sat, sat_sq }
    }

    /// 窗口 `[x, x+w) × [y, y+h)` 内的像素和。
    fn window_sum(&self, x: usize, y: usize, w: usize, h: usize) -> f64 {
        let stride = self.width + 1;
        self.sat[(y + h) * stride + x + w] - self.sat[y * stride + x + w]
            - self.sat[(y + h) * stride + x]
            + self.sat[y * stride + x]
    }

    /// 窗口内的像素平方和，用于 O(1) 求出窗口方差。
    fn window_sum_sq(&self, x: usize, y: usize, w: usize, h: usize) -> f64 {
        let stride = self.width + 1;
        self.sat_sq[(y + h) * stride + x + w] - self.sat_sq[y * stride + x + w]
            - self.sat_sq[(y + h) * stride + x]
            + self.sat_sq[y * stride + x]
    }
}

/// 把 BGRA 缓冲拆成三个单通道平面。
fn planes_from_bgra(pixels: &[u8], width: u32, height: u32) -> VisionResult<Vec<Plane>> {
    if width == 0 || height == 0 {
        return Err(VisionError::InvalidDimensions);
    }
    let count = width as usize * height as usize;
    let expected = count * 4;
    if pixels.len() != expected {
        return Err(VisionError::PixelBufferMismatch { expected, actual: pixels.len() });
    }
    let mut channels: Vec<Vec<f32>> = (0..CHANNELS).map(|_| vec![0.0f32; count]).collect();
    for index in 0..count {
        let base = index * 4;
        // 缓冲区是 BGRA：0=B、1=G、2=R。顺序本身不影响结果，只要模板与画面一致。
        for (channel, values) in channels.iter_mut().enumerate() {
            values[index] = pixels[base + channel] as f32;
        }
    }
    Ok(channels
        .into_iter()
        .map(|values| Plane::new(width as usize, height as usize, values))
        .collect())
}

/// NCC 的分子：`Σ (T - T̄) · I`。
///
/// 展开 `Σ(T-T̄)(I-Ī)` 时中间两项正好抵消（因为 `Σ(T-T̄) = 0`），
/// 所以不需要先算 Ī，直接拿去均值的模板乘原始画面即可。
///
/// 写成"按行切片 + 行内累加"而不是双重下标：这样内层循环是一段连续内存，
/// 而且省掉了逐元素的边界检查——在这段被调用几百万次的代码里，那是主要开销。
fn numerator(hay: &Plane, tpl_zero: &[f64], x: usize, y: usize, tpl_w: usize, tpl_h: usize) -> f64 {
    let mut total = 0.0f64;
    for dy in 0..tpl_h {
        let start = (y + dy) * hay.width + x;
        let hay_row = &hay.values[start..start + tpl_w];
        let tpl_row = &tpl_zero[dy * tpl_w..dy * tpl_w + tpl_w];
        for (value, weight) in hay_row.iter().zip(tpl_row.iter()) {
            total += *value as f64 * *weight;
        }
    }
    total
}

/// 在 `hay` 上滑动 `tpl`，返回最佳位置（图像坐标系）与分数。
///
/// 返回 `None` 表示**这个模板根本放不下**（画面比模板小），或者模板是纯色的
/// （没有任何图案可匹配）——两种情况都不是"找到了 0 分的位置"。
fn best_match(hay: &[Plane], tpl: &[Plane]) -> Option<(Rect, f32)> {
    let prepared = prepare(hay, tpl)?;
    let rows = hay[0].height - prepared.height + 1;
    let found = scan_rows_parallel(
        rows,
        |range| scan_rows(hay, &prepared, range),
        |current, candidate| {
            if is_better(current, candidate.2, candidate.0, candidate.1) {
                Some(candidate)
            } else {
                current
            }
        },
    )?;
    Some((hit_rect(found.0, found.1, &prepared), found.2 as f32))
}

/// 在**分数已经够格**的位置里，找离 `prior` 最近的那个。
///
/// 与 [`best_match`] 的分工是刻意的：那个回答"最像的位置在哪"，
/// 这个回答"在像到可以接受的位置里，哪一个离期望位置最近"。
/// 合成一遍写会让"分数够不够"与"位置合不合适"两条判据纠缠在一处，
/// 而它们必须能被**分开验证**——先验绝不能把低分命中抬上来。
///
/// 它要**再扫一遍**（第一次扫描只带回最高分，没带回"有哪些位置够格"）。
/// 代价是匹配耗时翻倍，但这只在配了位置先验时发生，且这一步每次任务只跑一次。
fn best_match_near(
    hay: &[Plane],
    prepared: &Prepared,
    prior: Point,
    min_score: f64,
) -> Option<(Rect, f32)> {
    let rows = hay[0].height - prepared.height + 1;
    let target = (prior.x as f64, prior.y as f64);
    let found = scan_rows_parallel(
        rows,
        |range| scan_rows_near(hay, prepared, range, target, min_score),
        |current, candidate| {
            if is_nearer(current, candidate, target) {
                Some(candidate)
            } else {
                current
            }
        },
    )?;
    Some((hit_rect(found.0, found.1, prepared), found.2 as f32))
}

/// 模板的预处理结果：去均值后的三个通道 + 各自的方差。
///
/// 单独拿出来是因为"取最高分"与"取离先验最近"两次扫描吃的是**同一份**
/// 预处理结果——分开算两遍等于把模板的均值与方差算两次。
struct Prepared {
    width: usize,
    height: usize,
    zero: Vec<Vec<f64>>,
    variance: [f64; CHANNELS],
}

/// 给模板做去均值与方差；`None` 表示它**没有判别力**。
fn prepare(hay: &[Plane], tpl: &[Plane]) -> Option<Prepared> {
    let width = tpl[0].width;
    let height = tpl[0].height;
    if width == 0 || height == 0 || hay[0].width < width || hay[0].height < height {
        return None;
    }
    let count = (width * height) as f64;

    // 模板去均值 + 方差。方差为 0 表示这个通道是纯色的，没有判别力。
    let mut zero: Vec<Vec<f64>> = Vec::with_capacity(CHANNELS);
    let mut variance = [0.0f64; CHANNELS];
    for (channel, plane) in tpl.iter().enumerate() {
        let sum: f64 = plane.values.iter().map(|v| *v as f64).sum();
        let sq: f64 = plane
            .values
            .iter()
            .map(|v| {
                let v = *v as f64;
                v * v
            })
            .sum();
        let mean = sum / count;
        variance[channel] = (sq - count * mean * mean).max(0.0);
        // 去均值后**保留 f64**：这里降到 f32 会让分数带上约 1e-7 的相对误差，
        // 而阈值判定（默认 0.8）虽然不在乎这点误差，用例却在乎——
        // 「与定义式逐位置一致」这条用例正是靠它才盯得住积分图那套等价变形。
        zero.push(plane.values.iter().map(|v| *v as f64 - mean).collect());
    }
    // 三个通道全是纯色 ⇒ 模板上没有任何图案，匹配结果没有意义。
    if variance.iter().all(|v| *v <= FLAT_VARIANCE) {
        return None;
    }
    Some(Prepared { width, height, zero, variance })
}

/// 把内部的位置元组还原成对外的矩形。
fn hit_rect(x: usize, y: usize, prepared: &Prepared) -> Rect {
    Rect {
        x: x as i32,
        y: y as i32,
        width: prepared.width as i32,
        height: prepared.height as i32,
    }
}

/// 「按行分块 + 并行 + 按给定规则合并」这段骨架。
///
/// 两次扫描（取最高分 / 取离先验最近）共用它：并行策略只有一处，
/// 改分块方式不会只改到其中一条——那会让两个结果在不同机器上分叉。
///
/// 线程数由机器决定（`available_parallelism`），不写死——单核机器上
/// 这条分支自然退化成顺序执行，不需要额外的开关。
fn scan_rows_parallel<T, S, C>(rows: usize, scan: S, combine: C) -> Option<T>
where
    T: Copy + Send,
    S: Fn(std::ops::Range<usize>) -> Option<T> + Sync,
    C: Fn(Option<T>, T) -> Option<T>,
{
    if rows == 0 {
        return None;
    }
    let workers = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1)
        .clamp(1, rows);
    if workers == 1 {
        return scan(0..rows);
    }
    let chunk = rows.div_ceil(workers);
    let mut best: Option<T> = None;
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|worker| {
                let start = worker * chunk;
                let end = ((worker + 1) * chunk).min(rows);
                let scan = &scan;
                scope.spawn(move || if start >= end { None } else { scan(start..end) })
            })
            .collect();
        for handle in handles {
            if let Some(value) = handle.join().unwrap_or(None) {
                best = combine(best, value);
            }
        }
    });
    best
}

/// 在 `rows` 这段行区间里逐位置求分数，返回其中最好的一个。
fn scan_rows(
    hay: &[Plane],
    prepared: &Prepared,
    rows: std::ops::Range<usize>,
) -> Option<(usize, usize, f64)> {
    let max_x = hay[0].width - prepared.width;
    let mut best: Option<(usize, usize, f64)> = None;
    for y in rows {
        for x in 0..=max_x {
            let Some(score) = position_score(
                hay,
                &prepared.zero,
                &prepared.variance,
                x,
                y,
                prepared.width,
                prepared.height,
            ) else {
                continue;
            };
            if is_better(best, score, x, y) {
                best = Some((x, y, score));
            }
        }
    }
    best
}

/// 同 [`scan_rows`]，但只接受 `score >= min_score` 的位置，并在其中挑离 `prior` 最近的。
fn scan_rows_near(
    hay: &[Plane],
    prepared: &Prepared,
    rows: std::ops::Range<usize>,
    prior: (f64, f64),
    min_score: f64,
) -> Option<(usize, usize, f64)> {
    let max_x = hay[0].width - prepared.width;
    let mut best: Option<(usize, usize, f64)> = None;
    for y in rows {
        for x in 0..=max_x {
            let Some(score) = position_score(
                hay,
                &prepared.zero,
                &prepared.variance,
                x,
                y,
                prepared.width,
                prepared.height,
            ) else {
                continue;
            };
            if score < min_score {
                continue;
            }
            if is_nearer(best, (x, y, score), prior) {
                best = Some((x, y, score));
            }
        }
    }
    best
}

/// 位置到先验点的距离**平方**。这里只需要比大小，开方是白花的钱。
fn distance_sq(x: usize, y: usize, prior: (f64, f64)) -> f64 {
    let dx = x as f64 - prior.0;
    let dy = y as f64 - prior.1;
    dx * dx + dy * dy
}

/// 候选是否比当前的更靠近先验点。
///
/// 平局规则和 [`is_better`] 一样是**确定性**的：距离相同比分数，分数相同比
/// `(y, x)`。并行扫描之后"谁先被扫到"取决于线程调度，平局若按到达顺序决定，
/// 同一份输入在不同机器上会给出不同位置——而位置直接决定点哪儿。
fn is_nearer(
    current: Option<(usize, usize, f64)>,
    candidate: (usize, usize, f64),
    prior: (f64, f64),
) -> bool {
    match current {
        None => true,
        Some((best_x, best_y, best_score)) => {
            let best_distance = distance_sq(best_x, best_y, prior);
            let distance = distance_sq(candidate.0, candidate.1, prior);
            if distance != best_distance {
                distance < best_distance
            } else if candidate.2 != best_score {
                candidate.2 > best_score
            } else {
                (candidate.1, candidate.0) < (best_y, best_x)
            }
        }
    }
}

/// 候选 `(x, y, score)` 是否比当前的更好。
///
/// **平局时取更靠上、更靠左的那个**。这不是随手加的：分块并行之后，
/// "谁先被扫到"取决于线程调度，如果平局按到达顺序决定，
/// 同一份输入在不同机器上可能给出不同的位置——而位置直接决定点哪儿。
/// 有了这条规则，并行结果与顺序扫描**逐位相同**。
fn is_better(current: Option<(usize, usize, f64)>, score: f64, x: usize, y: usize) -> bool {
    match current {
        None => true,
        Some((best_x, best_y, best_score)) => {
            score > best_score || (score == best_score && (y, x) < (best_y, best_x))
        }
    }
}

/// 某个位置上的归一化互相关分数。
///
/// 返回 `None` 表示这个位置**算不出相关系数**——画面窗口是纯色的（分母为 0），
/// 或者模板三个通道都是纯色。这与"分数很低"是两回事：前者是没定义，后者是不像。
/// 混为一谈会把一片纯色背景判成"最不像"，从而把真正的图标压下去。
///
/// 抽成独立函数是为了让「按位置比一遍」这件事能被测试直接钉住：
/// 只要 `best_match` 与逐位置调用它给出同样的结果，就说明"取最大值"这一步没有错。
fn position_score(
    hay: &[Plane],
    tpl_zero: &[Vec<f64>],
    tpl_var: &[f64; CHANNELS],
    x: usize,
    y: usize,
    tpl_w: usize,
    tpl_h: usize,
) -> Option<f64> {
    let count = (tpl_w * tpl_h) as f64;
    let mut total = 0.0f64;
    let mut usable = 0usize;
    for channel in 0..CHANNELS {
        if tpl_var[channel] <= FLAT_VARIANCE {
            continue;
        }
        let plane = &hay[channel];
        let mean = plane.window_sum(x, y, tpl_w, tpl_h) / count;
        let variance = (plane.window_sum_sq(x, y, tpl_w, tpl_h) - count * mean * mean).max(0.0);
        let denominator = (tpl_var[channel] * variance).sqrt();
        if denominator <= FLAT_VARIANCE {
            continue;
        }
        let numer = numerator(plane, &tpl_zero[channel], x, y, tpl_w, tpl_h);
        total += numer / denominator;
        usable += 1;
    }
    if usable == 0 {
        None
    } else {
        Some(total / usable as f64)
    }
}

/// 在一帧截图里找一张模板；返回图像坐标系下的最佳位置与分数。
///
/// `None` 表示模板放不进这一帧（画面比模板还小）或模板是纯色的。
/// **不做阈值判断**——阈值属于策略，由 [`IconLocator`] 决定。
pub fn match_template(
    frame: &Screenshot,
    template: &IconTemplate,
) -> VisionResult<Option<(Rect, f32)>> {
    let hay = planes_from_bgra(&frame.pixels, frame.width, frame.height)?;
    let needle = planes_from_bgra(&template.pixels, template.width, template.height)?;
    Ok(best_match(&hay, &needle))
}

/// 从磁盘载入一张图标模板（PNG / JPEG / BMP 等 `image` 支持的格式）。
///
/// 载入时就把尺寸卡住：太小多半是截图时手抖，太大说明拿错了图。
/// 两种情况都**当场报错**，而不是留到任务里表现为"匹配分数很低"——
/// 那时人只会去怀疑阈值，不会想到"模板根本不是图标"。
pub fn load_icon_template(path: &Path, label: impl Into<String>) -> VisionResult<IconTemplate> {
    let label = label.into();
    // 读不进来时**带上标签**：一个图标名底下可以有多张图，只说"图像编解码失败"
    // 没法告诉人是哪一张坏了——而修的时候正是要精确到那一张。
    let rgba = image::open(path)
        .map_err(|err| VisionError::TemplateUnreadable {
            label: label.clone(),
            reason: err.to_string(),
        })?
        .to_rgba8();
    let (width, height) = (rgba.width(), rgba.height());
    if width < MIN_TEMPLATE_SIDE || height < MIN_TEMPLATE_SIDE {
        return Err(VisionError::TemplateTooSmall { label, width, height });
    }
    if width > MAX_TEMPLATE_SIDE || height > MAX_TEMPLATE_SIDE {
        return Err(VisionError::TemplateTooLarge { label, width, height });
    }
    let mut pixels = rgba.into_raw();
    // RGBA -> BGRA，回到与平台层一致的表示。
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Ok(IconTemplate { label, pixels, width, height })
}

/// 把一张 BGRA 缓冲区切成 `Rect` 指定的小块，供"从截图里裁模板"使用。
///
/// 与 [`crate::crop`] 的区别：那个吃 [`Screenshot`] 并重新算指纹，
/// 这个只是取像素，用来产出 [`IconTemplate`]。
pub fn crop_template(
    frame: &Screenshot,
    region: Rect,
    label: impl Into<String>,
) -> VisionResult<IconTemplate> {
    if region.is_degenerate() {
        return Err(VisionError::CropOutOfBounds);
    }
    let right = region.x + region.width;
    let bottom = region.y + region.height;
    if region.x < 0
        || region.y < 0
        || right > frame.width as i32
        || bottom > frame.height as i32
    {
        return Err(VisionError::CropOutOfBounds);
    }
    let (x, y) = (region.x as usize, region.y as usize);
    let (width, height) = (region.width as usize, region.height as usize);
    let stride = frame.width as usize;
    let mut pixels = Vec::with_capacity(width * height * 4);
    for row in 0..height {
        let start = ((y + row) * stride + x) * 4;
        pixels.extend_from_slice(&frame.pixels[start..start + width * 4]);
    }
    Ok(IconTemplate { label: label.into(), pixels, width: width as u32, height: height as u32 })
}

/// 同一个模板里，分数最高的前 `top` 个**互不重叠**的候选。
///
/// ## 为什么只报最高分是不够的
///
/// 一个"分数 0.55、低于阈值"有两种**完全不同的成因**，而它们在日志里长得
/// 一模一样，人只能靠猜：
///
/// 1. **有一个明显的峰，只是峰不够高** —— 模板确实是那个图标，但它现在画出来的
///    样子和模板不一样（选中/未选中换了字形、缩放了、主题变了）⇒ 处置是**重截模板**；
/// 2. **到处都差不多** —— 搜索区里根本没有对应物，最高分只是噪声里的偶然
///    ⇒ 处置是查**搜索区与几何**，重截模板一点用都没有。
///
/// 分辨这两者只需要知道"第二名离第一名有多远"。
///
/// ## 为什么要抑制重叠
///
/// 同一个图标上相邻的几个像素会拿到几乎一样的高分。不抑制的话"前 3 名"会是
/// 同一个峰上的 3 个相邻像素，等于只报了一个位置。抑制半径取模板自己的宽高：
/// 两个候选只要在任一方向上的距离都小于模板尺寸，就算落在同一处。
///
/// ⚠️ 抑制**必须按分数定强弱，不能按扫描顺序**。扫描是从左上往右下走的，
/// 先扫到的不一定更"像"：一个 0.42 的边缘位置会先把真正的峰（0.98）压掉，
/// 而结果是"只报了一个低分候选"——恰好把要说清的事情说反了。
/// 所以规则是：**被更强者压住的丢弃，压住更弱者的把它挤出去**。
///
/// ## 代价
///
/// 单线程、不做任何近似，是 [`best_match_near`] 同款的"再扫一遍"。
/// 它**只在失败路径上跑**（失败本来就要转人工，人比机器贵），
/// 而且不影响任何一个位置上的分数——它读的是同一份 [`Prepared`]。
///
/// 量级：默认导航条（97×734）配 40×38 的模板，约 4 万个位置 × 1520 像素 × 3 通道，
/// debug 构建下每张模板是**秒级**。这不是可以放进主路径的开销，所以它**只**在
/// 已经决定转人工之后才被调用——那几秒换来的是"下一次该改哪里"，值。
fn top_candidates(hay: &[Plane], prepared: &Prepared, top: usize) -> Vec<(usize, usize, f64)> {
    let max_x = hay[0].width - prepared.width;
    let rows = hay[0].height - prepared.height + 1;
    // 落在同一处：两个方向上的距离都小于模板尺寸。
    let overlaps = |x: usize, y: usize, px: usize, py: usize| {
        x.abs_diff(px) < prepared.width && y.abs_diff(py) < prepared.height
    };
    // 按分数从高到低维持，末位就是"目前最弱的那个入选者"。
    let mut peaks: Vec<(usize, usize, f64)> = Vec::with_capacity(top);
    for y in 0..rows {
        for x in 0..=max_x {
            let Some(score) = position_score(
                hay,
                &prepared.zero,
                &prepared.variance,
                x,
                y,
                prepared.width,
                prepared.height,
            ) else {
                continue;
            };
            // 名额已满且连最弱的入选者都打不过 ⇒ 这一位不可能进榜。
            if peaks.len() >= top && score <= peaks[top - 1].2 {
                continue;
            }
            if peaks.iter().any(|(px, py, best)| score <= *best && overlaps(x, y, *px, *py)) {
                continue;
            }
            // 反过来：被这一位压住的、比它弱的峰要让位，否则它会一直占着
            // "同一处只留一个"的名额，把真正的峰挡在外面。
            peaks.retain(|(px, py, best)| !(score > *best && overlaps(x, y, *px, *py)));
            let at = peaks.iter().position(|(_, _, best)| score > *best).unwrap_or(peaks.len());
            peaks.insert(at, (x, y, score));
            peaks.truncate(top);
        }
    }
    peaks
}

/// 把每个模板的候选排成一段可读文本，附在"分数不够"的失败信息后面。
///
/// 它回答的是**下一次该动哪里**，所以每张模板都要带上自己的**尺寸**：
/// 模板尺寸和画面上图标的实际尺寸对不上时，分数会整体偏低而位置看着又没错，
/// 那是"模板截大了/截小了"，和"图标不在这里"是两回事。
fn candidate_report(hay: &[Plane], templates: &[IconTemplate]) -> String {
    let mut lines = Vec::with_capacity(templates.len());
    for template in templates {
        let Ok(needle) = planes_from_bgra(&template.pixels, template.width, template.height) else {
            continue;
        };
        let size = format!("{}x{}", template.width, template.height);
        let Some(prepared) = prepare(hay, &needle) else {
            lines.push(format!("{} {size}：放不进搜索区", template.label));
            continue;
        };
        let ranked: Vec<String> = top_candidates(hay, &prepared, DIAGNOSTIC_PEAKS)
            .iter()
            .map(|(x, y, score)| format!("{score:.3}@({x},{y})"))
            .collect();
        if ranked.is_empty() {
            lines.push(format!("{} {size}：没有可比较的位置", template.label));
        } else {
            lines.push(format!("{} {size}：{}", template.label, ranked.join("  ")));
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    // ★ 这一段是给人看的：**第一名与第二名差多少**决定下一步往哪查。
    format!(
        "\n搜索区 {}x{}。各模板前 {} 个候选（分数@坐标，坐标相对搜索区左上角）：\n  {}\n\
         若第一名明显高于后面 ⇒ 位置没错、是模板与当前画面对不上（重截模板）；\
         若前几名挤在一起 ⇒ 搜索区里根本没有它（先查搜索区与窗口几何）。",
        hay[0].width,
        hay[0].height,
        DIAGNOSTIC_PEAKS,
        lines.join("\n  ")
    )
}

/// 纯 Rust 的模板匹配定位器。
///
/// 无状态、无外部依赖：一帧画面进，一个命中位置出。
/// 之所以做成类型而不是自由函数，是为了满足 [`IconLocator`] 端口——
/// 调用方（编排层）只认识端口，不认识这个实现。
#[derive(Debug, Default, Clone, Copy)]
pub struct TemplateLocator;

impl IconLocator for TemplateLocator {
    fn locate(
        &self,
        frame: &Screenshot,
        query: &IconQuery<'_>,
    ) -> Result<IconMatch, AutomationError> {
        if query.templates.is_empty() {
            // 与"没匹配上"是两回事：这是配置缺失，不能静默当成"这里没有图标"。
            return Err(AutomationError::NeedsHumanReview(
                "没有配置任何图标模板，无法定位图标。".into(),
            ));
        }
        let hay = planes_from_bgra(&frame.pixels, frame.width, frame.height)?;

        // 逐张模板取最高分，并**留住胜出那张的预处理结果**：位置先验只在
        // 同一张模板的候选位置之间比较——换一张模板比位置没有意义，
        // 不同变体本来就落在图标框内的不同像素上。
        let mut best: Option<(IconMatch, Prepared)> = None;
        for (index, template) in query.templates.iter().enumerate() {
            let needle = planes_from_bgra(&template.pixels, template.width, template.height)?;
            let Some(prepared) = prepare(&hay, &needle) else {
                continue;
            };
            let rows = hay[0].height - prepared.height + 1;
            let Some(found) = scan_rows_parallel(
                rows,
                |range| scan_rows(&hay, &prepared, range),
                |current, candidate| {
                    if is_better(current, candidate.2, candidate.0, candidate.1) {
                        Some(candidate)
                    } else {
                        current
                    }
                },
            ) else {
                continue;
            };
            let score = found.2 as f32;
            if best.as_ref().map(|(current, _)| score > current.score).unwrap_or(true) {
                best = Some((
                    IconMatch {
                        bounds: hit_rect(found.0, found.1, &prepared),
                        score,
                        template_index: index,
                        template_label: template.label.clone(),
                    },
                    prepared,
                ));
            }
        }

        let Some((mut found, prepared)) = best else {
            return Err(AutomationError::AmbiguousVision(format!(
                "在 {}x{} 的搜索区域里，没有一个模板放得下（最小模板 {}x{}）",
                frame.width,
                frame.height,
                query.templates.iter().map(|t| t.width).min().unwrap_or(0),
                query.templates.iter().map(|t| t.height).min().unwrap_or(0)
            )));
        };

        // 分数不够 ⇒ 转人工，**不**把"最高分那个"先拿去用。
        // 点错图标的后果是后面每一步都作用在错误的界面上，比直接失败严重得多。
        //
        // 注意这一步在位置先验**之前**：先验只回答"够格的几个里挑哪个"，
        // 绝不能把本来不及格的命中抬上来。顺序反了，"分数不够"就会表现为
        // "点到了别的地方"。
        if found.score < query.min_score {
            return Err(AutomationError::AmbiguousVision(format!(
                "图标模板匹配不确定：最高分 {:.3}（模板「{}」，位置 ({}, {})），低于阈值 {:.3}。\
                 请确认模板确实截自这个图标，且它此刻在搜索区域内可见。{}",
                found.score,
                found.template_label,
                found.bounds.x,
                found.bounds.y,
                query.min_score,
                candidate_report(&hay, query.templates)
            )));
        }

        // 位置先验：在分数不低于「最高分 − 容差」的候选里，挑离期望位置最近的那个。
        if let Some(prior) = query.prior {
            if prior.score_tolerance > 0.0 {
                let floor = (found.score - prior.score_tolerance) as f64;
                if let Some((bounds, score)) = best_match_near(&hay, &prepared, prior.at, floor) {
                    found.bounds = bounds;
                    found.score = score;
                }
            }
        }

        Ok(found)
    }
}

/// 把一张模板渲染成可直接用于断言的小画面（测试辅助）。
#[cfg(test)]
pub fn frame_of(width: u32, height: u32, paint: impl Fn(u32, u32) -> [u8; 4]) -> Screenshot {
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            pixels.extend_from_slice(&paint(x, y));
        }
    }
    Screenshot {
        pixels,
        width,
        height,
        captured_at: std::time::SystemTime::now(),
        fingerprint: "test-frame".into(),
    }
}

#[cfg(test)]
mod tests;
