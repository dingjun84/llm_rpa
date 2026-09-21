//! 按显示器缩放保存多份界面标定。
//!
//! ## 为什么要多份
//!
//! DPI / Retina 缩放不同时，同一物理窗口的**逻辑布局**不同（导航栏、头像列是固定像素宽）。
//! 只留一份 `calibrated_window` 时，外接屏（scale=1）标定过、内建屏（scale=2）上跑任务，
//! 会在装配期被 `ensure_calibrated_size` 直接拦下——而图标库预览不走那道门，看起来像「测试能点、任务不能点」。
//!
//! ## 存什么
//!
//! 每一份 [`CalibrationSnapshot`] 绑死一个 `scale_factor`，并带上当时的窗口尺寸、四个核心区域、
//! `area_marks`、导航搜索区与滚动落点。任务装配时按**当前**显示器缩放挑一份；没有匹配的就拒绝开跑，
//! 绝不用错缩放的区域去点。
//!
//! ## 与顶层字段的关系
//!
//! `RuntimeConfig` 顶层的 `calibrated_window` / `regions` / `area_marks` / `nav_strip` / `scroll_anchor`
//! 仍是「界面标定」页正在编辑的那一份（工作副本）。保存时会 upsert 进 `calibrations`；
//! 开跑时再按当前缩放从 `calibrations` 挑出来覆盖工作副本后交给核心层。

use serde::{Deserialize, Serialize};

use super::{RegionConfig, ScrollAnchorConfig, WindowGeometry};
use crate::calibration;

/// 与核心层 `SCALE_FACTOR_TOLERANCE` 同量级：缩放差超过这个就当另一份标定。
pub(super) const SCALE_MATCH_TOLERANCE: f32 = 0.01;

/// 一份与某个显示器缩放绑定的界面标定。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationSnapshot {
    /// 这份标定对应的显示器缩放（例如 1.0 / 2.0）。
    pub scale_factor: f32,
    /// 界面上可选的短标签（例如「外接屏」「内建 Retina」）。空串 = 不显示。
    #[serde(default)]
    pub label: String,
    pub window: WindowGeometry,
    pub regions: RegionConfig,
    #[serde(default)]
    pub area_marks: calibration::AreaMarks,
    pub nav_strip: [f32; 4],
    #[serde(default)]
    pub scroll_anchor: ScrollAnchorConfig,
}

impl super::RuntimeConfig {
    /// 把旧配置（只有顶层 `calibrated_window`、没有 `calibrations`）迁成至少一份快照。
    ///
    /// 幂等：已有列表就不改。没有窗口几何时列表保持空——真实模式仍会在装配期被拦。
    pub fn ensure_calibrations_migrated(&mut self) {
        if !self.calibrations.is_empty() {
            return;
        }
        let Some(window) = self.calibrated_window else {
            return;
        };
        self.calibrations.push(CalibrationSnapshot {
            scale_factor: window.scale_factor,
            label: String::new(),
            window,
            regions: self.regions.clone(),
            area_marks: self.area_marks.clone(),
            nav_strip: self.nav_strip,
            scroll_anchor: self.scroll_anchor,
        });
    }

    /// 用当前工作副本（顶层字段）按缩放 upsert 进 `calibrations`。
    ///
    /// 没有 `calibrated_window` 时不动列表——否则会用假尺寸污染多份标定。
    pub fn upsert_working_snapshot(&mut self) {
        let Some(window) = self.calibrated_window else {
            return;
        };
        let scale = window.scale_factor;
        let snap = CalibrationSnapshot {
            scale_factor: scale,
            label: self
                .calibrations
                .iter()
                .find(|c| (c.scale_factor - scale).abs() <= SCALE_MATCH_TOLERANCE)
                .map(|c| c.label.clone())
                .unwrap_or_default(),
            window,
            regions: self.regions.clone(),
            area_marks: self.area_marks.clone(),
            nav_strip: self.nav_strip,
            scroll_anchor: self.scroll_anchor,
        };
        if let Some(slot) = self
            .calibrations
            .iter_mut()
            .find(|c| (c.scale_factor - scale).abs() <= SCALE_MATCH_TOLERANCE)
        {
            *slot = snap;
        } else {
            self.calibrations.push(snap);
        }
        self.calibrations
            .sort_by(|a, b| a.scale_factor.partial_cmp(&b.scale_factor).unwrap_or(std::cmp::Ordering::Equal));
    }

    /// 把某份快照拷回顶层工作副本，供界面编辑或装配前覆盖。
    pub fn apply_snapshot(&mut self, snap: &CalibrationSnapshot) {
        self.calibrated_window = Some(snap.window);
        self.regions = snap.regions.clone();
        self.area_marks = snap.area_marks.clone();
        self.nav_strip = snap.nav_strip;
        self.scroll_anchor = snap.scroll_anchor;
    }

    /// 按当前显示器缩放挑一份标定。
    pub fn pick_calibration(&self, scale: f32) -> Result<&CalibrationSnapshot, String> {
        if let Some(snap) = self
            .calibrations
            .iter()
            .find(|c| (c.scale_factor - scale).abs() <= SCALE_MATCH_TOLERANCE)
        {
            return Ok(snap);
        }
        if self.calibrations.is_empty() {
            return Err(
                "还没有任何按缩放保存的界面标定。请到「界面标定」页：\
                 先点「记录窗口尺寸」，再框区域，最后保存配置。"
                    .to_string(),
            );
        }
        let available: Vec<String> = self
            .calibrations
            .iter()
            .map(|c| {
                if c.label.is_empty() {
                    format!("{:.2}", c.scale_factor)
                } else {
                    format!("{:.2}（{}）", c.scale_factor, c.label)
                }
            })
            .collect();
        Err(format!(
            "当前显示器缩放 {:.2} 没有对应标定（已有：{}）。\
             缩放不同意味着同一物理尺寸下的界面布局本来就不同——\
             请把客户端窗口移到该缩放的显示器上，在「界面标定」页重新「记录窗口尺寸」、框区域并保存；\
             或切回已有标定对应的那块显示器再跑。",
            scale,
            available.join("、")
        ))
    }

    /// 删掉某个缩放的标定；若删的是当前工作副本那份，清空顶层窗口几何。
    pub fn remove_calibration_at_scale(&mut self, scale: f32) {
        self.calibrations
            .retain(|c| (c.scale_factor - scale).abs() > SCALE_MATCH_TOLERANCE);
        if let Some(window) = self.calibrated_window {
            if (window.scale_factor - scale).abs() <= SCALE_MATCH_TOLERANCE {
                self.calibrated_window = None;
            }
        }
    }
}
