//! 远程 YOLO UI 检测端口与共享判据。
//!
//! ★ 判据只此一处：按 class 取最高 conf、截图像素→屏幕、列表首行姓名匹配。
//! 桌面端 HTTP 适配器与 GhostBox 鼠标都不得另写一套换算。

use serde::{Deserialize, Serialize};

use crate::ports::{AutomationError, Point, Rect, Screenshot, TextBox};

/// YOLO 类别名（与 `yolo26/API.md` / `CLASS_NAMES_CANONICAL` 对齐；当前权威为 14 类）。
pub mod class {
    pub const SELF_AVATAR: &str = "self_avatar";
    pub const NAV_CHAT_ICON: &str = "nav_chat_icon";
    pub const NAV_CONTACTS_ICON: &str = "nav_contacts_icon";
    pub const SEARCH_BAR: &str = "search_bar";
    pub const CONTACT_ITEM: &str = "contact_item";
    /// 已从 14 类模型移除；仅作旧权重兼容，新流程用 [`estimate_message_input_region`]。
    pub const MESSAGE_INPUT: &str = "message_input";
    pub const SEND_BUTTON: &str = "send_button";
    pub const CONVERSATION_ITEM: &str = "conversation_item";
    pub const INCOMING_BUBBLE: &str = "incoming_bubble";
    pub const OUTGOING_BUBBLE: &str = "outgoing_bubble";
    pub const INPUT_BAR: &str = "input_bar";
    pub const SINGLE_CHAT: &str = "single_chat";
    pub const GROUP_CHAT: &str = "group_chat";
    pub const CONTACT_SEND_MESSAGE: &str = "contact_send_message";
    pub const NAV_GROUPS_ICON: &str = "nav_groups_icon";
}

/// 单条检测（坐标相对**原图**像素，与 API `detections[]` 一致）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct YoloDetection {
    pub class_id: i32,
    pub class_name: String,
    pub conf: f32,
    /// `[cx, cy]` 原图像素中心。
    pub center: [f32; 2],
    /// `[x1, y1, x2, y2]` 原图像素。
    pub xyxy: [f32; 4],
}

impl YoloDetection {
    pub fn center_point(&self) -> Point {
        Point {
            x: self.center[0].round() as i32,
            y: self.center[1].round() as i32,
        }
    }

    /// 原图像素 bbox → [`Rect`]（宽高至少为 1）。
    pub fn bounds_rect(&self) -> Rect {
        let x1 = self.xyxy[0].round() as i32;
        let y1 = self.xyxy[1].round() as i32;
        let x2 = self.xyxy[2].round() as i32;
        let y2 = self.xyxy[3].round() as i32;
        Rect {
            x: x1,
            y: y1,
            width: (x2 - x1).max(1),
            height: (y2 - y1).max(1),
        }
    }
}



/// 在框的**中心半区**内均匀随机取一点：`x ∈ [cx±w/4]`，`y ∈ [cy±h/4]`。
///
/// 与 [`YoloDetection::center_point`] 同一坐标系（截图像素）；Retina 缩放仍走
/// [`shot_point_to_screen`]。用 xorshift64* 线程局部 + 时间种子，避免给本 crate 加 `rand` 依赖。
pub fn random_point_in_central_half(bounds: Rect) -> Point {
    let w = bounds.width.max(1) as f32;
    let h = bounds.height.max(1) as f32;
    let cx = bounds.x as f32 + w / 2.0;
    let cy = bounds.y as f32 + h / 2.0;
    let half_w = w / 4.0;
    let half_h = h / 4.0;
    let u = next_unit_f32();
    let v = next_unit_f32();
    let x = cx - half_w + u * (2.0 * half_w);
    let y = cy - half_h + v * (2.0 * half_h);
    Point {
        x: x.round() as i32,
        y: y.round() as i32,
    }
}

fn next_unit_f32() -> f32 {
    use std::cell::Cell;
    use std::time::{SystemTime, UNIX_EPOCH};
    thread_local! {
        static STATE: Cell<u64> = Cell::new(0);
    }
    STATE.with(|cell| {
        let mut x = cell.get();
        if x == 0 {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0xA5A5_1234_C3D2_E1F0);
            x = nanos ^ 0x9E37_79B9_7F4A_7C15;
            if x == 0 {
                x = 0xA5A5_1234_C3D2_E1F0;
            }
        }
        // xorshift64*
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        cell.set(x);
        // [0, 1)
        ((x >> 11) as f32) / ((1u64 << 53) as f32)
    })
}

/// 从 `input_bar` / `send_button` 估计文字输入区（原图像素）。
///
/// - top = input_bar 底边 + 小间隙
/// - left = input_bar 左边（无 bar 时用 send 左边往左推一段）
/// - right = send_button 左边 − 小间隙（无 send 时用 bar 右边）
/// - bottom = send_button 底边（无 send 时 = top + 合理高度）
///
/// 二者都缺则返回 `None`（调用方可再试遗留 `message_input`）。
pub fn estimate_message_input_region(detections: &[YoloDetection]) -> Option<Rect> {
    const GAP: i32 = 4;
    const FALLBACK_HEIGHT: i32 = 56;
    const FALLBACK_WIDTH_FRAC_OF_SEND: i32 = 8; // send 左侧约 8×send 宽

    let bar = best_by_class(detections, class::INPUT_BAR).map(|d| d.bounds_rect());
    let send = best_by_class(detections, class::SEND_BUTTON).map(|d| d.bounds_rect());

    match (bar, send) {
        (None, None) => None,
        (Some(bar), Some(send)) => {
            let top = bar.y + bar.height + GAP;
            let left = bar.x;
            let right = (send.x - GAP).max(left + 1);
            let bottom = send.y + send.height;
            Some(Rect {
                x: left,
                y: top,
                width: (right - left).max(1),
                height: (bottom - top).max(1),
            })
        }
        (Some(bar), None) => {
            let top = bar.y + bar.height + GAP;
            Some(Rect {
                x: bar.x,
                y: top,
                width: bar.width.max(1),
                height: FALLBACK_HEIGHT,
            })
        }
        (None, Some(send)) => {
            let width = (send.width.max(1) * FALLBACK_WIDTH_FRAC_OF_SEND).max(120);
            let right = (send.x - GAP).max(1);
            let left = (right - width).max(0);
            let bottom = send.y + send.height;
            let top = (send.y - FALLBACK_HEIGHT / 2).max(0);
            Some(Rect {
                x: left,
                y: top,
                width: (right - left).max(1),
                height: (bottom - top).max(1),
            })
        }
    }
}

/// 把 YOLO 检测框转成 [`TextBox`]，供过程诊断 / TaskReplay 复用文字框叠加层。
///
/// ## 为什么复用 TextBox 而不是另开 overlay 类型
///
/// `Observation::text_boxes` 与 `annotate_step` 已经约定「本帧图像坐标 + 右侧文字栏」。
/// YOLO 框的 `xyxy` 本来就是原图像素，与这条约定一致；`text` 写成 `class conf`，
/// 复盘时一眼能对上检出类别与置信度，前端也不必为 YOLO 单独改渲染。
pub fn detections_as_text_boxes(dets: &[YoloDetection]) -> Vec<TextBox> {
    dets
        .iter()
        .map(|d| TextBox {
            text: format!("{} {:.2}", d.class_name, d.conf),
            bounds: d.bounds_rect(),
            confidence: d.conf,
        })
        .collect()
}

/// 远程 / 本地 YOLO 检测端口。实现方负责编码 PNG 与 HTTP；核心层只吃结果。
pub trait YoloDetector: Send + Sync {
    /// 对一帧**全分辨率**截图做检测。`conf` 为置信度阈值（0–1）。
    fn detect(
        &self,
        frame: &Screenshot,
        conf: f32,
    ) -> Result<Vec<YoloDetection>, AutomationError>;
}

/// 截图像素点 → 屏幕绝对坐标（与桌面 `shot_point_to_screen` **同一公式**）。
///
/// Retina：`shot_w/h` 是物理像素，`window` 是逻辑点；按比例还原。
pub fn shot_point_to_screen(
    x: i32,
    y: i32,
    window: Rect,
    shot_w: u32,
    shot_h: u32,
) -> Point {
    let sx = if shot_w > 0 {
        window.width as f32 / shot_w as f32
    } else {
        1.0
    };
    let sy = if shot_h > 0 {
        window.height as f32 / shot_h as f32
    } else {
        1.0
    };
    Point {
        x: window.x + (x as f32 * sx).round() as i32,
        y: window.y + (y as f32 * sy).round() as i32,
    }
}

/// 在同 class 的检测里取 **conf 最高**的一条。并列取先出现的（API 已按 conf 降序）。
pub fn best_by_class<'a>(
    detections: &'a [YoloDetection],
    class_name: &str,
) -> Option<&'a YoloDetection> {
    detections
        .iter()
        .filter(|d| d.class_name == class_name)
        .max_by(|a, b| {
            a.conf
                .partial_cmp(&b.conf)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

/// 同 class 全部检测，按 conf 降序。
pub fn all_by_class<'a>(
    detections: &'a [YoloDetection],
    class_name: &str,
) -> Vec<&'a YoloDetection> {
    let mut items: Vec<_> = detections
        .iter()
        .filter(|d| d.class_name == class_name)
        .collect();
    items.sort_by(|a, b| {
        b.conf
            .partial_cmp(&a.conf)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    items
}

/// 列表条目是否高到足以容纳约两行（姓名 + 最近消息）。
///
/// 高度不够 ⇒ 多半是首尾被裁切的半截条目，应跳过或滚动，避免 OCR 到残缺名。
pub fn item_tall_enough_for_two_lines(bounds: Rect, min_two_line_px: i32) -> bool {
    bounds.height >= min_two_line_px
}

/// 默认：条目高度至少约 40 逻辑像素才算能放下两行（企业微信会话行实测）。
pub const DEFAULT_MIN_ITEM_HEIGHT_PX: i32 = 40;

/// 在首行文字里找目标名：优先**更靠前**出现的匹配（`find` 下标更小），
/// 同分再比置信度。`haystack` 已是 OCR 首行。
pub fn name_match_score(haystack: &str, needle: &str) -> Option<(usize, f32)> {
    let hay = haystack.trim();
    let needle = needle.trim();
    if hay.is_empty() || needle.is_empty() {
        return None;
    }
    hay.find(needle).map(|idx| (idx, 1.0))
}

/// 未配置 YOLO 时的占位：任何检测都失败，方便演练装配不崩在构造期。
#[derive(Debug, Default)]
pub struct UnconfiguredYolo;

impl YoloDetector for UnconfiguredYolo {
    fn detect(
        &self,
        _frame: &Screenshot,
        _conf: f32,
    ) -> Result<Vec<YoloDetection>, AutomationError> {
        Err(AutomationError::Platform(
            "未配置 YOLO 检测器：真实模式需要远程 /predict 适配器。".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn det(class_name: &str, conf: f32, xyxy: [f32; 4]) -> YoloDetection {
        YoloDetection {
            class_id: 0,
            class_name: class_name.into(),
            conf,
            center: [(xyxy[0] + xyxy[2]) / 2.0, (xyxy[1] + xyxy[3]) / 2.0],
            xyxy,
        }
    }

    #[test]
    fn detections_as_text_boxes_label_and_bounds() {
        let boxes = detections_as_text_boxes(&[det("contact_item", 0.91, [10.0, 20.0, 110.0, 60.0])]);
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].text, "contact_item 0.91");
        assert!((boxes[0].confidence - 0.91).abs() < f32::EPSILON);
        assert_eq!(boxes[0].bounds, Rect { x: 10, y: 20, width: 100, height: 40 });
    }

    #[test]
    fn random_point_stays_in_central_half() {
        let bounds = Rect { x: 100, y: 200, width: 80, height: 40 };
        // central half: x in [120,160], y in [210,230]
        for _ in 0..64 {
            let p = random_point_in_central_half(bounds);
            assert!((120..=160).contains(&p.x), "x={}", p.x);
            assert!((210..=230).contains(&p.y), "y={}", p.y);
        }
    }

    #[test]
    fn estimate_input_region_from_bar_and_send() {
        let dets = [
            det("input_bar", 0.9, [100.0, 500.0, 500.0, 540.0]),
            det("send_button", 0.9, [520.0, 560.0, 580.0, 600.0]),
        ];
        let r = estimate_message_input_region(&dets).expect("region");
        assert_eq!(r.x, 100);
        assert_eq!(r.y, 544); // 540 + 4
        assert_eq!(r.width, 416); // 520-4 - 100
        assert_eq!(r.height, 56); // 600 - 544
    }
}
