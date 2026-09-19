//! 图标定位端口替身。
//!
//! 不读屏幕、不做任何图像运算：按构造时给定的方式给出一个命中位置（或失败）。
//! 这样编排层里"切换视图"那一步的成功与失败路径都能脱离真实桌面测到。

use std::sync::Mutex;

use automation_core::{AutomationError, IconLocator, IconMatch, IconTemplate, Rect, Screenshot};

/// 命中方式。
#[derive(Debug, Clone, PartialEq)]
enum Behaviour {
    /// 命中搜索区的正中（默认）。
    Centre,
    /// 命中在指定位置（图像坐标系）。
    At(Rect),
    /// 永远匹配不到 —— 用于演示「图标不在搜索区里 / 模板不对」。
    Never,
}

/// 可脚本化的图标定位替身。
#[derive(Debug)]
pub struct MockIconLocator {
    behaviour: Behaviour,
    score: f32,
    /// 每次调用时传入的搜索区尺寸，用来断言"只在这个区域里找"。
    pub regions: Mutex<Vec<(u32, u32)>>,
    pub calls: Mutex<usize>,
}

impl Default for MockIconLocator {
    fn default() -> Self {
        Self::new()
    }
}

impl MockIconLocator {
    /// 默认行为：在搜索区正中命中，分数 1.0。
    ///
    /// 为什么是"正中"而不是"随便一个位置"：切换视图之后编排层要点击它，
    /// 而"点击有没有落到搜索区里"在真实实现里是一条会被核对的约束。
    /// 替身给一个一定落在区域内的位置，就不会把编排层的问题
    /// 和"替身给了一个越界坐标"混在一起。
    pub fn new() -> Self {
        Self { behaviour: Behaviour::Centre, score: 1.0, regions: Mutex::new(Vec::new()), calls: Mutex::new(0) }
    }

    /// 命中在图像坐标系的指定位置。
    pub fn at(bounds: Rect) -> Self {
        Self { behaviour: Behaviour::At(bounds), ..Self::new() }
    }

    /// 永远匹配不到（分数低于阈值）。
    pub fn never() -> Self {
        Self { behaviour: Behaviour::Never, score: 0.0, ..Self::new() }
    }

    /// 指定命中分数，用来测"刚好过线 / 刚好不过线"。
    pub fn with_score(mut self, score: f32) -> Self {
        self.score = score;
        self
    }

    pub fn call_count(&self) -> usize {
        *self.calls.lock().unwrap()
    }
}

impl IconLocator for MockIconLocator {
    fn locate(
        &self,
        frame: &Screenshot,
        templates: &[IconTemplate],
        min_score: f32,
    ) -> Result<IconMatch, AutomationError> {
        *self.calls.lock().unwrap() += 1;
        self.regions.lock().unwrap().push((frame.width, frame.height));

        // 与真实实现保持一致：没有模板是**配置缺失**，不是"这里没有图标"。
        let template = templates.first().ok_or_else(|| {
            AutomationError::NeedsHumanReview("没有配置任何图标模板，无法定位图标。".into())
        })?;

        if self.behaviour == Behaviour::Never || self.score < min_score {
            return Err(AutomationError::AmbiguousVision(format!(
                "图标模板匹配不确定：最高分 {:.3}，低于阈值 {:.3}",
                self.score, min_score
            )));
        }

        let bounds = match self.behaviour {
            Behaviour::At(bounds) => bounds,
            // 正中：模板放得下就贴着中心放。
            Behaviour::Centre => Rect {
                x: (frame.width as i32 - template.width as i32).max(0) / 2,
                y: (frame.height as i32 - template.height as i32).max(0) / 2,
                width: template.width as i32,
                height: template.height as i32,
            },
            Behaviour::Never => unreachable!("上面已经返回"),
        };

        Ok(IconMatch {
            bounds,
            score: self.score,
            template_index: 0,
            template_label: template.label.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn frame(width: u32, height: u32) -> Screenshot {
        Screenshot {
            pixels: vec![0; (width * height * 4) as usize],
            width,
            height,
            captured_at: SystemTime::now(),
            fingerprint: "mock".into(),
        }
    }

    fn template(width: u32, height: u32) -> IconTemplate {
        IconTemplate {
            label: "图标".into(),
            pixels: vec![0; (width * height * 4) as usize],
            width,
            height,
        }
    }

    #[test]
    fn the_centre_behaviour_lands_inside_the_frame() {
        let locator = MockIconLocator::new();
        let found = locator.locate(&frame(100, 60), &[template(20, 20)], 0.8).unwrap();
        assert_eq!(found.bounds, Rect { x: 40, y: 20, width: 20, height: 20 });
        assert!(found.score >= 0.8);
        assert_eq!(locator.call_count(), 1);
    }

    #[test]
    fn a_missing_template_is_a_configuration_error_not_a_miss() {
        let locator = MockIconLocator::new();
        let err = locator.locate(&frame(100, 60), &[], 0.8).unwrap_err();
        assert!(matches!(err, AutomationError::NeedsHumanReview(_)), "实际是 {err:?}");
    }

    #[test]
    fn never_and_below_threshold_both_turn_into_ambiguous_vision() {
        let err = MockIconLocator::never()
            .locate(&frame(100, 60), &[template(20, 20)], 0.8)
            .unwrap_err();
        assert!(matches!(err, AutomationError::AmbiguousVision(_)), "实际是 {err:?}");

        let err = MockIconLocator::new()
            .with_score(0.5)
            .locate(&frame(100, 60), &[template(20, 20)], 0.8)
            .unwrap_err();
        assert!(matches!(err, AutomationError::AmbiguousVision(_)), "实际是 {err:?}");
    }

    #[test]
    fn it_records_which_region_was_searched() {
        let locator = MockIconLocator::new();
        locator.locate(&frame(73, 734), &[template(26, 26)], 0.8).unwrap();
        assert_eq!(*locator.regions.lock().unwrap(), vec![(73, 734)]);
    }
}
