//! YOLO 点击：检测框 / 估计区 → 中心半区随机落点 → 屏幕坐标 → `guarded_click`。
//!
//! ★ 落点只用 [`crate::yolo::random_point_in_central_half`]（`cx±w/4, cy±h/4`，截图像素），
//!   随后才走 [`crate::yolo::shot_point_to_screen`] 做截图→屏幕（含 Retina）换算。
//! ★ 每次点击都落一条「看图 + 判定」，过程回放可见被点的框与落点。

use crate::diagnostics::{Decision, Verdict};
use crate::ports::{AutomationError, Rect, Screenshot, TextBox};
use crate::yolo::{random_point_in_central_half, shot_point_to_screen, YoloDetection};

use super::Run;

impl Run<'_> {
    /// 在检测框的**中心半区**内随机取点（`cx±w/4, cy±h/4`，截图像素），再经
    /// [`shot_point_to_screen`] 换算成屏幕坐标并 `guarded_click`（Windows 上应由 GhostBox 实现）。
    ///
    /// 所有 YOLO 检测框点击（导航图标、会话/联系人条目、搜索框、发送按钮）都走这里，
    /// 不再点精确中心。同时落一条「看图 + 判定」：标注图只圈被点的那一框，决策写清 class / conf / 落点。
    pub(super) fn yolo_click_detection(
        &mut self,
        window: Rect,
        shot: &Screenshot,
        det: &YoloDetection,
        what: &str,
    ) -> Result<(), AutomationError> {
        let expected_window = self.ensure_calibrated()?;
        let img_pt = random_point_in_central_half(det.bounds_rect());
        let target = shot_point_to_screen(img_pt.x, img_pt.y, window, shot.width, shot.height);
        let point_rule = "中心半区随机点";
        self.evidence.push(format!(
            "点击{what}：{} conf={:.2} → 屏幕 ({}, {})（{point_rule}）",
            det.class_name, det.conf, target.x, target.y
        ));
        let click_step = format!("点击{what}");
        let overlay = TextBox {
            text: format!("{} {:.2}", det.class_name, det.conf),
            bounds: det.bounds_rect(),
            confidence: det.conf,
        };
        self.report(&click_step, window, shot, &[overlay.clone()], None, None);
        self.report_decision(
            &click_step,
            Decision {
                step: String::new(),
                question: format!("要点击哪个检测框（{what}）？"),
                rule: format!(
                    "YOLO class=`{}` {point_rule} → shot_point_to_screen → guarded_click",
                    det.class_name
                ),
                outcome: format!(
                    "点击 {} conf={:.2} → 屏幕 ({}, {})",
                    det.class_name, det.conf, target.x, target.y
                ),
                passed: true,
                min_confidence: det.conf,
                replay: None,
                candidates: vec![Verdict::passed(
                    &overlay,
                    format!(
                        "选中目标 class={} conf={:.2} img=({}, {}) screen=({}, {})",
                        det.class_name, det.conf, img_pt.x, img_pt.y, target.x, target.y
                    ),
                )],
            },
        );
        self.ensure_not_frozen(&format!("已取消点击{what}"))?;
        self.runner
            .ports
            .platform
            .guarded_click(target, expected_window)?;
        self.check_deadline(&format!("点击{what}"))
    }

    /// 在原图像素矩形的中心半区随机落点并点击（用于估计出的输入区）。
    pub(super) fn yolo_click_image_rect_jittered(
        &mut self,
        window: Rect,
        shot: &Screenshot,
        bounds: Rect,
        what: &str,
    ) -> Result<(), AutomationError> {
        let expected_window = self.ensure_calibrated()?;
        let img_pt = random_point_in_central_half(bounds);
        let target = shot_point_to_screen(img_pt.x, img_pt.y, window, shot.width, shot.height);
        self.evidence.push(format!(
            "点击{what}：估计区 {}×{}@({}, {}) → 屏幕 ({}, {})（中心半区随机）",
            bounds.width, bounds.height, bounds.x, bounds.y, target.x, target.y
        ));
        let click_step = format!("点击{what}");
        let overlay = TextBox {
            text: format!("{what} (estimated)"),
            bounds,
            confidence: 1.0,
        };
        self.report(&click_step, window, shot, &[overlay.clone()], None, None);
        self.report_decision(
            &click_step,
            Decision {
                step: String::new(),
                question: format!("要点击哪个区域（{what}）？"),
                rule: "input_bar/send_button 估计输入区 → 中心半区随机 → shot_point_to_screen → guarded_click"
                    .into(),
                outcome: format!(
                    "点击估计输入区 → 屏幕 ({}, {})",
                    target.x, target.y
                ),
                passed: true,
                min_confidence: 0.0,
                replay: None,
                candidates: vec![Verdict::passed(
                    &overlay,
                    format!(
                        "估计区 img=({}, {}) screen=({}, {})",
                        img_pt.x, img_pt.y, target.x, target.y
                    ),
                )],
            },
        );
        self.ensure_not_frozen(&format!("已取消点击{what}"))?;
        self.runner
            .ports
            .platform
            .guarded_click(target, expected_window)?;
        self.check_deadline(&format!("点击{what}"))
    }
}
