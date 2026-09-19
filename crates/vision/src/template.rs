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

use automation_core::{AutomationError, IconLocator, IconMatch, IconTemplate, Rect, Screenshot};

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
    let tpl_w = tpl[0].width;
    let tpl_h = tpl[0].height;
    if tpl_w == 0 || tpl_h == 0 || hay[0].width < tpl_w || hay[0].height < tpl_h {
        return None;
    }
    let count = (tpl_w * tpl_h) as f64;

    // 模板去均值 + 方差。方差为 0 表示这个通道是纯色的，没有判别力。
    let mut tpl_zero: Vec<Vec<f64>> = Vec::with_capacity(CHANNELS);
    let mut tpl_var = [0.0f64; CHANNELS];
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
        tpl_var[channel] = (sq - count * mean * mean).max(0.0);
        // 去均值后**保留 f64**：这里降到 f32 会让分数带上约 1e-7 的相对误差，
        // 而阈值判定（默认 0.8）虽然不在乎这点误差，用例却在乎——
        // 「与定义式逐位置一致」这条用例正是靠它才盯得住积分图那套等价变形。
        tpl_zero.push(plane.values.iter().map(|v| *v as f64 - mean).collect());
    }
    // 三个通道全是纯色 ⇒ 模板上没有任何图案，匹配结果没有意义。
    if tpl_var.iter().all(|variance| *variance <= FLAT_VARIANCE) {
        return None;
    }

    let max_y = hay[0].height - tpl_h;

    // 按**行**分块并行：每个块的结果互不影响，最后按同一个比较规则合并。
    // 线程数由机器决定（`available_parallelism`），不写死——单核机器上
    // 这条分支自然退化成顺序执行，不需要额外的开关。
    let rows = max_y + 1;
    let workers = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1)
        .clamp(1, rows);

    let mut best: Option<(usize, usize, f64)> = None;
    if workers == 1 {
        best = scan_rows(hay, &tpl_zero, &tpl_var, tpl_w, tpl_h, 0..rows);
    } else {
        let chunk = rows.div_ceil(workers);
        let mut partial: Vec<Option<(usize, usize, f64)>> = Vec::with_capacity(workers);
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..workers)
                .map(|worker| {
                    let start = worker * chunk;
                    let end = ((worker + 1) * chunk).min(rows);
                    let hay = &hay;
                    let tpl_zero = &tpl_zero;
                    let tpl_var = &tpl_var;
                    scope.spawn(move || {
                        if start >= end {
                            return None;
                        }
                        scan_rows(hay, tpl_zero, tpl_var, tpl_w, tpl_h, start..end)
                    })
                })
                .collect();
            for handle in handles {
                partial.push(handle.join().unwrap_or(None));
            }
        });
        for candidate in partial {
            if let Some((x, y, score)) = candidate {
                if is_better(best, score, x, y) {
                    best = Some((x, y, score));
                }
            }
        }
    }

    best.map(|(x, y, score)| {
        (
            Rect { x: x as i32, y: y as i32, width: tpl_w as i32, height: tpl_h as i32 },
            score as f32,
        )
    })
}

/// 在 `rows` 这段行区间里逐位置求分数，返回其中最好的一个。
fn scan_rows(
    hay: &[Plane],
    tpl_zero: &[Vec<f64>],
    tpl_var: &[f64; CHANNELS],
    tpl_w: usize,
    tpl_h: usize,
    rows: std::ops::Range<usize>,
) -> Option<(usize, usize, f64)> {
    let max_x = hay[0].width - tpl_w;
    let mut best: Option<(usize, usize, f64)> = None;
    for y in rows {
        for x in 0..=max_x {
            let Some(score) = position_score(hay, tpl_zero, tpl_var, x, y, tpl_w, tpl_h) else {
                continue;
            };
            if is_better(best, score, x, y) {
                best = Some((x, y, score));
            }
        }
    }
    best
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
    let rgba = image::open(path)?.to_rgba8();
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
        templates: &[IconTemplate],
        min_score: f32,
    ) -> Result<IconMatch, AutomationError> {
        if templates.is_empty() {
            // 与"没匹配上"是两回事：这是配置缺失，不能静默当成"这里没有图标"。
            return Err(AutomationError::NeedsHumanReview(
                "没有配置任何图标模板，无法定位图标。".into(),
            ));
        }
        let hay = planes_from_bgra(&frame.pixels, frame.width, frame.height)?;

        let mut best: Option<IconMatch> = None;
        for (index, template) in templates.iter().enumerate() {
            let needle = planes_from_bgra(&template.pixels, template.width, template.height)?;
            let Some((bounds, score)) = best_match(&hay, &needle) else {
                continue;
            };
            if best.as_ref().map(|current| score > current.score).unwrap_or(true) {
                best = Some(IconMatch {
                    bounds,
                    score,
                    template_index: index,
                    template_label: template.label.clone(),
                });
            }
        }

        match best {
            // 分数不够 ⇒ 转人工，**不**把"最高分那个"先拿去用。
            // 点错图标的后果是后面每一步都作用在错误的界面上，比直接失败严重得多。
            Some(found) if found.score < min_score => Err(AutomationError::AmbiguousVision(format!(
                "图标模板匹配不确定：最高分 {:.3}（模板「{}」，位置 ({}, {})），低于阈值 {:.3}。\
                 请确认模板确实截自这个图标，且它此刻在搜索区域内可见。",
                found.score, found.template_label, found.bounds.x, found.bounds.y, min_score
            ))),
            Some(found) => Ok(found),
            None => Err(AutomationError::AmbiguousVision(format!(
                "在 {}x{} 的搜索区域里，没有一个模板放得下（最小模板 {}x{}）",
                frame.width,
                frame.height,
                templates.iter().map(|t| t.width).min().unwrap_or(0),
                templates.iter().map(|t| t.height).min().unwrap_or(0)
            ))),
        }
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
mod tests {
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

        let err = TemplateLocator.locate(&frame, &[template], 0.8).unwrap_err();
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

        let found = TemplateLocator.locate(&frame, &[wrong, right], 0.9).unwrap();
        assert_eq!(found.template_index, 1, "应当选中真正对得上的那一张");
        assert_eq!((found.bounds.x, found.bounds.y), (44, 12));
        assert!(found.score > 0.99);
    }

    #[test]
    fn the_locator_refuses_an_empty_template_list() {
        let frame = frame_of(40, 40, pattern);
        let err = TemplateLocator.locate(&frame, &[], 0.8).unwrap_err();
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
        assert!(load_icon_template(&dir.join("missing.png"), "缺失").is_err());
    }
}
