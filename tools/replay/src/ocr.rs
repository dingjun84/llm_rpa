//! 真 OCR 重跑：把 `raw/` 里那张**未标注**的输入图喂回本机引擎，看读数变没变。
//!
//! ## 它补的是哪一问
//!
//! 判据重跑（`lib.rs`）吃的是**盘上记下的那一份 boxes**，所以它永远答不了
//! 「是不是 OCR 读错了」——那份 boxes 正是它要审的东西（`docs/todo.md` T30 的天花板一）。
//!
//! 这里把当时那张干净图（`raw/NN-步骤名.png`，**没叠框**）原样喂回引擎再跑一次：
//!
//! - 读数一样 ⇒ 识别这一层是稳的，问题在判据；
//! - 读数不一样 ⇒ 判据拿到的输入本身就不可复现，先定住这一层。
//!
//! ⚠️ 拿 `steps/NN-*.png` 重跑是**错的**：那张图上压着蓝框与文字栏，
//! 读出来的是另一套结果，与盘上那份没法比。
//!
//! ## 为什么用 `vision::ExternalOcr`
//!
//! 重跑出来的 boxes 必须与盘上那份**可比**：盘上那份也是 `vision` 的解析器
//! 从引擎 stdout 解析出来的。自己再写一套解析，diff 出来的可能是两个解析器的差异，
//! 而不是读数的差异（`CONVENTIONS.md` §1.3 的同一条道理）。
//!
//! ⚠️ 引擎的**启动参数**（配置里的 `ocr_args`）没有被记进事件流，所以这里默认
//! 不传参数、用引擎自己的默认值。读出的东西与盘上不一样时，第一个该试的就是
//! `--upscale`——先把倍率对齐，再谈别的结论。

use std::path::{Path, PathBuf};
use std::time::Duration;

use automation_core::TextBox;
use vision::ocr::ExternalOcr;

/// `--ocr` 不写（或写 `auto`）时按顺序找这些位置。相对**当前目录**：
/// 本工具按文档都是从仓库根跑的（`cargo run -p replay -- …`）。
const CANDIDATE_ENGINES: &[&str] = &[
    "target/debug/macosocr",
    "target/debug/winocr.exe",
    "target/debug/winocr",
    "target/release/macosocr",
    "target/release/winocr.exe",
];

/// 引擎超时。比 `ExternalOcr` 默认的 10 秒宽：`--upscale 8` 的大图会慢一些。
const TIMEOUT: Duration = Duration::from_secs(30);

/// 左上角差到这个像素数才算"框挪了"。
///
/// 同一张图、同一套参数重跑，坐标本该一模一样；留一点余量是因为换倍率之后
/// 框的边缘会差一两个像素，那不是"读数变了"。
const POSITION_TOLERANCE: i32 = 2;

/// 跑一次 OCR 用的引擎（路径 + 参数）。
pub struct Engine {
    path: PathBuf,
    /// 传给引擎的放大倍数；`None` = 不传，用引擎自己的默认值。
    upscale: Option<f32>,
}

impl Engine {
    /// 按 [`CANDIDATE_ENGINES`] 找本机已经编好的那个引擎。
    pub fn discover() -> Result<Self, String> {
        CANDIDATE_ENGINES
            .iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
            .map(|path| Self { path, upscale: None })
            .ok_or_else(|| format!("没找到本机 OCR 引擎（找过 {}）", CANDIDATE_ENGINES.join("、")))
    }

    /// 指定引擎路径。路径不存在时不在这里报错：等真起进程时如实写进报告，
    /// 比在参数解析阶段拦下来更有用——那时人已经能看到是哪一步、哪张图。
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), upscale: None }
    }

    pub fn with_upscale(mut self, upscale: Option<f32>) -> Self {
        self.upscale = upscale;
        self
    }

    /// 引擎（含参数）的一句话描述：报告里要写清刚才跑的是哪个引擎、哪个倍率。
    pub fn describe(&self) -> String {
        match self.upscale {
            Some(upscale) => format!("{} --upscale {upscale}", self.path.display()),
            None => self.path.display().to_string(),
        }
    }

    /// 跑一张 `raw/` 下的图（`png_rel` 是它在任务目录里的相对路径）。
    pub(crate) fn run(
        &self,
        dir: &Path,
        png_rel: &str,
    ) -> Result<(Vec<TextBox>, String), String> {
        let path = dir.join(png_rel);
        let png = std::fs::read(&path)
            .map_err(|err| format!("读不到 {}：{err}", path.display()))?;
        let mut ocr = ExternalOcr::new(&self.path).with_timeout(TIMEOUT);
        if let Some(upscale) = self.upscale {
            ocr = ocr.with_args(["--upscale".to_string(), format!("{upscale}")]);
        }
        ocr.recognize_png(&png).map_err(|err| format!("{}：{err}", self.describe()))
    }
}

/// 两次读数之间的差异。
///
/// 按**文字**配对，不按序号：同一句话在结果里可能出现多次（现场那 19 块里
/// 「联系人」就有两处），按序号比会把"换了个顺序"当成"全变了"。
#[derive(Debug, Default, Clone, PartialEq)]
pub struct BoxDiff {
    /// 盘上有、这次没读出来的（文字 + 盘上那份的置信度）。
    pub missing: Vec<(String, f32)>,
    /// 这次读出来、盘上没有的（文字 + 这次的置信度）。
    pub added: Vec<(String, f32)>,
    /// 两次都读到了、但置信度或位置不同。
    pub changed: Vec<Changed>,
}

/// 同一句话在两次读数里的差别。
#[derive(Debug, Clone, PartialEq)]
pub struct Changed {
    pub text: String,
    /// 盘上那份的置信度。
    pub recorded: f32,
    /// 这次读出来的置信度。
    pub fresh: f32,
    /// 框左上角挪了几个像素。
    pub shift: (i32, i32),
}

impl BoxDiff {
    /// 逐块一致（文字、置信度、位置都对得上）。
    pub fn is_empty(&self) -> bool {
        self.missing.is_empty() && self.added.is_empty() && self.changed.is_empty()
    }

    /// 一行摘要：「逐块一致」或「多 2 块 / 缺 1 块 / 改 3 块」。
    pub fn summary(&self) -> String {
        if self.is_empty() {
            return "逐块一致".to_string();
        }
        let mut parts = Vec::new();
        if !self.added.is_empty() {
            parts.push(format!("多 {} 块", self.added.len()));
        }
        if !self.missing.is_empty() {
            parts.push(format!("缺 {} 块", self.missing.len()));
        }
        if !self.changed.is_empty() {
            parts.push(format!("改 {} 块", self.changed.len()));
        }
        parts.join(" / ")
    }
}

/// 比两次读数。
///
/// 配对规则：**文字相同里挑离得最近的那一块**。
///
/// 为什么不按序号配：同一句话在读数里可能出现多次（现场那 19 块里「联系人」
/// 就有两处），序号一错位就会把"两块换了个位置"报成"两块全变了"。
/// 为什么还要挑最近的：同一个字出现两次时，只按"文字相同"配也还是会错位
/// （上面那两处「联系人」相隔 600 多像素），而两次读数的坐标本来就该对得上——
/// 按距离挑能把真正的"这一块挪了"与"配错了对象"分开。
pub fn diff_boxes(recorded: &[TextBox], fresh: &[TextBox]) -> BoxDiff {
    let mut used = vec![false; fresh.len()];
    let mut diff = BoxDiff::default();

    for block in recorded {
        let hit = (0..fresh.len())
            .filter(|index| !used[*index] && fresh[*index].text == block.text)
            .min_by_key(|index| distance(&fresh[*index], block));
        let Some(index) = hit else {
            diff.missing.push((block.text.clone(), block.confidence));
            continue;
        };
        used[index] = true;
        let other = &fresh[index];
        let shift = (other.bounds.x - block.bounds.x, other.bounds.y - block.bounds.y);
        if !same_confidence(block.confidence, other.confidence) || moved(shift) {
            diff.changed.push(Changed {
                text: block.text.clone(),
                recorded: block.confidence,
                fresh: other.confidence,
                shift,
            });
        }
    }

    for (index, block) in fresh.iter().enumerate() {
        if !used[index] {
            diff.added.push((block.text.clone(), block.confidence));
        }
    }
    diff
}

/// 两块文字离多远（曼哈顿距离，只用来在同样文字的候选里挑最近的一个）。
fn distance(left: &TextBox, right: &TextBox) -> i32 {
    (left.bounds.x - right.bounds.x).abs() + (left.bounds.y - right.bounds.y).abs()
}

/// 置信度是否算"同一个"。
///
/// ⚠️ 必须按**三位小数**比：盘上那份是收过三位小数才落盘的（`events.rs::confidence`），
/// 而这次是从引擎 stdout 直接解析出来的全精度值（`0.61` 实际是 `0.6100000143…`）。
/// 直接比 `f32` 会把每一次重跑都报成"置信度变了"。
fn same_confidence(recorded: f32, fresh: f32) -> bool {
    (recorded * 1000.0).round() == (fresh * 1000.0).round()
}

fn moved((dx, dy): (i32, i32)) -> bool {
    dx.abs() > POSITION_TOLERANCE || dy.abs() > POSITION_TOLERANCE
}