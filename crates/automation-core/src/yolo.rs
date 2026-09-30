//! 远程 YOLO UI 检测端口与共享判据。
//!
//! ★ 判据只此一处：按 class 取最高 conf、截图像素→屏幕、列表首行姓名匹配。
//! 桌面端 HTTP 适配器与 GhostBox 鼠标都不得另写一套换算。

use serde::{Deserialize, Serialize};

use crate::ports::{AutomationError, Point, Rect, Screenshot};

/// YOLO 类别名（与 `yolo26/API.md` / `CLASS_NAMES_CANONICAL` 对齐）。
pub mod class {
    pub const SELF_AVATAR: &str = "self_avatar";
    pub const NAV_CHAT_ICON: &str = "nav_chat_icon";
    pub const NAV_CONTACTS_ICON: &str = "nav_contacts_icon";
    pub const SEARCH_BAR: &str = "search_bar";
    pub const CONTACT_ITEM: &str = "contact_item";
    pub const MESSAGE_INPUT: &str = "message_input";
    pub const SEND_BUTTON: &str = "send_button";
    pub const CONVERSATION_ITEM: &str = "conversation_item";
    pub const INCOMING_BUBBLE: &str = "incoming_bubble";
    pub const OUTGOING_BUBBLE: &str = "outgoing_bubble";
    pub const INPUT_BAR: &str = "input_bar";
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
