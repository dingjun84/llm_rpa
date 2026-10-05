//! 远程 YOLO UI 检测端口与共享判据。
//!
//! ★ 判据只此一处：按 class 取最高 conf、截图像素→屏幕、列表首行姓名匹配。
//! 桌面端 HTTP 适配器与 GhostBox 鼠标都不得另写一套换算。

use serde::{Deserialize, Serialize};

use crate::ports::{AutomationError, Point, Rect, Screenshot, TextBox};

/// YOLO 类别名（与 `yolo26/API.md` / `CLASS_NAMES_CANONICAL` 对齐；当前权威为 13 类 c13）。
///
/// c13 把旧 14 类的 `conversation_item`（会话行）和 `contact_item`（联系人行）合并成
/// [`LIST_ITEM`]：两者外观几乎一样，模型分不清。是会话还是联系人由 runner 所在页面
/// （[`super::ListPage`]）决定，不再看 class 名。旧名字只作为别名保留，见 [`super::is_list_item`]。
pub mod class {
    pub const SELF_AVATAR: &str = "self_avatar";
    pub const NAV_CHAT_ICON: &str = "nav_chat_icon";
    pub const NAV_CONTACTS_ICON: &str = "nav_contacts_icon";
    pub const SEARCH_BAR: &str = "search_bar";
    /// 列表中的一行：会话列表行 / 通讯录行 / 搜索结果行（c13 id 4）。
    pub const LIST_ITEM: &str = "list_item";
    /// 旧 14 类权重的通讯录行；按 class 匹配时等同 [`LIST_ITEM`]。新代码请用 `LIST_ITEM`。
    pub const CONTACT_ITEM: &str = "contact_item";
    /// 已从 14 类模型移除；仅作旧权重兼容，新流程用 [`estimate_message_input_region`]。
    pub const MESSAGE_INPUT: &str = "message_input";
    pub const SEND_BUTTON: &str = "send_button";
    /// 旧 14 类权重的会话行；按 class 匹配时等同 [`LIST_ITEM`]。新代码请用 `LIST_ITEM`。
    pub const CONVERSATION_ITEM: &str = "conversation_item";
    pub const INCOMING_BUBBLE: &str = "incoming_bubble";
    pub const OUTGOING_BUBBLE: &str = "outgoing_bubble";
    pub const INPUT_BAR: &str = "input_bar";
    pub const SINGLE_CHAT: &str = "single_chat";
    pub const GROUP_CHAT: &str = "group_chat";
    pub const CONTACT_SEND_MESSAGE: &str = "contact_send_message";
    pub const NAV_GROUPS_ICON: &str = "nav_groups_icon";

    /// 都表示「列表中的一行」的类别名：13 类新权重的 `list_item` + 14 类旧权重的两个名字。
    pub const LIST_ITEM_ALIASES: [&str; 3] = [LIST_ITEM, CONVERSATION_ITEM, CONTACT_ITEM];
}

/// 检测出的 class 名是不是「列表中的一行」（`list_item` / `conversation_item` / `contact_item`）。
///
/// ★ 列表行别名只在这里判断。新旧权重（13 类 / 14 类）都靠它通用，调用方不要自己比字符串。
pub fn is_list_item(class_name: &str) -> bool {
    class::LIST_ITEM_ALIASES.contains(&class_name)
}

/// 检测出的 `actual` 是否满足想要的 `wanted`：名字相同，或二者都是列表行别名。
///
/// [`best_by_class`] / [`all_by_class`] 都走这里，所以传 `class::LIST_ITEM`（或旧的
/// `CONVERSATION_ITEM` / `CONTACT_ITEM`）都会同时命中三种列表行。
pub fn class_matches(actual: &str, wanted: &str) -> bool {
    actual == wanted || (is_list_item(actual) && is_list_item(wanted))
}

/// 列表行所在的页面。c13 里会话行与联系人行同为 `list_item`，**语义只由 runner 走到的页面决定**：
///
/// - [`ListPage::ChatList`]：工作流 `chat_list_send`，刚点过 `nav_chat_icon` → 中间栏是会话列表；
/// - [`ListPage::ContactsSearchResults`]：工作流 `contacts_search_send`，刚点过 `nav_contacts_icon`
///   并在 `search_bar` 里输入了联系人名 → 中间栏是搜索结果（联系人行）。
///
/// 不再用 class 名（旧 `conversation_item` / `contact_item`）去猜当前在哪个页面。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListPage {
    ChatList,
    ContactsSearchResults,
}

impl ListPage {
    /// 页面名，用于证据 / 决策文案。
    pub fn label(self) -> &'static str {
        match self {
            ListPage::ChatList => "会话列表",
            ListPage::ContactsSearchResults => "通讯录搜索结果",
        }
    }

    /// 本页一行是什么，用于证据 / 决策文案。
    pub fn item_label(self) -> &'static str {
        match self {
            ListPage::ChatList => "会话条目",
            ListPage::ContactsSearchResults => "联系人条目",
        }
    }
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
/// 列表行三种名字互为别名（见 [`class_matches`]）。
pub fn best_by_class<'a>(
    detections: &'a [YoloDetection],
    class_name: &str,
) -> Option<&'a YoloDetection> {
    detections
        .iter()
        .filter(|d| class_matches(&d.class_name, class_name))
        .max_by(|a, b| {
            a.conf
                .partial_cmp(&b.conf)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

/// 同 class 全部检测，按 conf 降序。列表行三种名字互为别名（见 [`class_matches`]）。
pub fn all_by_class<'a>(
    detections: &'a [YoloDetection],
    class_name: &str,
) -> Vec<&'a YoloDetection> {
    let mut items: Vec<_> = detections
        .iter()
        .filter(|d| class_matches(&d.class_name, class_name))
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
        let boxes = detections_as_text_boxes(&[det("list_item", 0.91, [10.0, 20.0, 110.0, 60.0])]);
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].text, "list_item 0.91");
        assert!((boxes[0].confidence - 0.91).abs() < f32::EPSILON);
        assert_eq!(boxes[0].bounds, Rect { x: 10, y: 20, width: 100, height: 40 });
    }

    #[test]
    fn is_list_item_accepts_all_aliases() {
        assert!(is_list_item("list_item"));
        assert!(is_list_item("conversation_item"));
        assert!(is_list_item("contact_item"));
        assert!(is_list_item(class::LIST_ITEM));
        assert!(!is_list_item("search_bar"));
        assert!(!is_list_item("single_chat"));
        assert!(!is_list_item("List_Item"));
        assert!(!is_list_item(""));
    }

    #[test]
    fn class_matches_aliases_only_for_list_items() {
        assert!(class_matches("conversation_item", class::LIST_ITEM));
        assert!(class_matches("contact_item", class::LIST_ITEM));
        assert!(class_matches("list_item", class::CONVERSATION_ITEM));
        assert!(class_matches("contact_item", class::CONVERSATION_ITEM));
        assert!(class_matches("send_button", class::SEND_BUTTON));
        assert!(!class_matches("send_button", class::LIST_ITEM));
        assert!(!class_matches("list_item", class::SEARCH_BAR));
    }

    #[test]
    fn all_by_class_list_item_merges_old_and_new_names() {
        // 旧 14 类权重在同一屏可能同时给出 conversation_item 和 contact_item；新权重给 list_item。
        let dets = [
            det("contact_item", 0.35, [0.0, 100.0, 200.0, 150.0]),
            det("search_bar", 0.95, [0.0, 0.0, 200.0, 30.0]),
            det("conversation_item", 0.80, [0.0, 40.0, 200.0, 100.0]),
            det("list_item", 0.60, [0.0, 150.0, 200.0, 200.0]),
        ];
        let items = all_by_class(&dets, class::LIST_ITEM);
        let names: Vec<&str> = items.iter().map(|d| d.class_name.as_str()).collect();
        assert_eq!(names, ["conversation_item", "list_item", "contact_item"]);
        // 旧常量也走别名：传 CONTACT_ITEM 照样拿到会话行。
        assert_eq!(all_by_class(&dets, class::CONTACT_ITEM).len(), 3);
        let best = best_by_class(&dets, class::LIST_ITEM).expect("best");
        assert_eq!(best.class_name, "conversation_item");
        assert_eq!(best_by_class(&dets, class::SEARCH_BAR).map(|d| d.class_name.as_str()), Some("search_bar"));
        assert!(best_by_class(&dets[1..2], class::LIST_ITEM).is_none());
    }

    #[test]
    fn list_page_labels() {
        assert_eq!(ListPage::ChatList.item_label(), "会话条目");
        assert_eq!(ListPage::ContactsSearchResults.item_label(), "联系人条目");
        assert_eq!(ListPage::ChatList.label(), "会话列表");
        assert_eq!(ListPage::ContactsSearchResults.label(), "通讯录搜索结果");
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
