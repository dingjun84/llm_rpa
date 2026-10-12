//! 转发 · 步骤5/6：消息区滚到底 + 向上分步滚动找目标气泡（任务 441744e5）。
//!
//! 滚轮统一走 `platform.scroll`：Windows 下由 `GhostboxMouseDesktop` 转成幽灵盒
//! `MoveMouseWheel(Z)`（Z 正上负下，已在 `notches_to_wheel_z` 换向并钳到 ±127）。
//! 本模块只用仓库约定：`notches > 0` = 向下（更新的消息），`< 0` = 向上（更早）。
//!
//! 到底 / 到顶：只对**气泡区**截图取指纹，滚动并停稳后连续 2 帧不变即判定。
//! 防漏：每步约滚气泡区高度的 60%（保留 ~40% 重叠），滚后核对上一屏最上面气泡的文字
//! 仍在新屏；不在则回退半步并减小步长。

use std::time::{Duration, Instant};

use crate::ports::{AutomationError, Point, Rect};
use crate::yolo::{
    all_by_class, class, random_point_in_central_half, shot_point_to_screen, YoloDetection,
};

use super::Run;

/// 一格滚轮在企业微信消息区大约滚动的像素（Windows 默认 3 行）。用于把「60% 高度」换成格数。
/// 任务 1decbea4：按 60px/格算出 5 格，实测滚得太少；改 20px/格（约 3 倍），过量由重叠核对回退兜底。
const PX_PER_NOTCH: i32 = 20;
/// 到底：先一次大跳，再小步确认。
const BOTTOM_JUMP_NOTCHES: i32 = 40;
const BOTTOM_CONFIRM_NOTCHES: i32 = 3;
const BOTTOM_CONFIRM_MAX: usize = 8;
/// 每次滚动后停稳等待。
const SCROLL_SETTLE: Duration = Duration::from_millis(250);
/// 到顶时企业微信可能懒加载更早的消息：指纹不变后再多等一下复核。
const TOP_LAZY_LOAD_WAIT: Duration = Duration::from_millis(700);
/// 向上找气泡的上限。
const SEARCH_MAX_STEPS: usize = 20;
const SEARCH_BUDGET: Duration = Duration::from_secs(40);
/// 每步滚动占气泡区高度的比例。
const STEP_RATIO: f32 = 0.6;
/// 气泡贴边判定（图像像素）。
const EDGE_PX: i32 = 4;

fn bubbles_of(dets: &[YoloDetection]) -> Vec<YoloDetection> {
    all_by_class(dets, class::INCOMING_BUBBLE)
        .into_iter()
        .chain(all_by_class(dets, class::OUTGOING_BUBBLE))
        .cloned()
        .collect()
}

/// 由检测结果推消息区：上沿 = 窗高 10%（会话标题下方），下沿 = input_bar 上沿（否则 78%），
/// 左右 = 气泡并集（否则窗宽 30%..98%）。
pub(super) fn chat_area_img(w: u32, h: u32, dets: &[YoloDetection]) -> Rect {
    let (w, h) = (w as i32, h as i32);
    let top = (h as f32 * 0.10).round() as i32;
    let bottom = all_by_class(dets, class::INPUT_BAR)
        .first()
        .map(|d| d.bounds_rect().y)
        .filter(|&y| y > top + 40)
        .unwrap_or((h as f32 * 0.78).round() as i32);
    // 左右固定取窗宽 30%..98%：任务 1decbea4 只有 2 个窄气泡时并集仅 125px 宽，指纹/滚轮点太偏。
    let (x1, x2) = ((w as f32 * 0.30) as i32, (w as f32 * 0.98) as i32);
    Rect {
        x: x1,
        y: top,
        width: (x2 - x1).max(1),
        height: (bottom - top).max(1),
    }
}

/// 按气泡区高度换算每步格数（≥1）。
pub(super) fn step_notches(area_h_img: i32, shot_h: u32, window_h: i32) -> i32 {
    let scale = window_h as f32 / shot_h.max(1) as f32;
    let px = area_h_img as f32 * scale * STEP_RATIO;
    ((px / PX_PER_NOTCH as f32).round() as i32).max(1)
}

/// 图像坐标矩形 → 屏幕矩形。
fn img_rect_to_screen(r: Rect, window: Rect, sw: u32, sh: u32) -> Rect {
    let a = shot_point_to_screen(r.x, r.y, window, sw, sh);
    let b = shot_point_to_screen(r.x + r.width, r.y + r.height, window, sw, sh);
    Rect { x: a.x, y: a.y, width: (b.x - a.x).max(1), height: (b.y - a.y).max(1) }
}

/// 气泡是否被消息区上/下沿截断：返回需要滚的格数（上截断 → -1 向上，下截断 → +1）。
pub(super) fn edge_adjust(bubble: Rect, area: Rect) -> i32 {
    if bubble.y <= area.y + EDGE_PX {
        -1
    } else if bubble.y + bubble.height >= area.y + area.height - EDGE_PX {
        1
    } else {
        0
    }
}

impl Run<'_> {
    fn chat_fp(&self, area_screen: Rect) -> Result<String, AutomationError> {
        Ok(self.runner.ports.platform.capture(area_screen)?.fingerprint)
    }

    fn wheel(&self, at: Point, notches: i32, window: Rect) -> Result<(), AutomationError> {
        self.runner.ports.platform.scroll(at, notches, window)?;
        std::thread::sleep(SCROLL_SETTLE);
        Ok(())
    }

    /// 步骤5：大跳到底，再小步确认（气泡区指纹连续 2 次不变）。
    pub(super) fn forward_scroll_chat_to_bottom(&mut self) -> Result<(), AutomationError> {
        let started = Instant::now();
        let (window, shot, dets) = self.yolo_detect_window("转发·5·气泡区")?;
        let area_img = chat_area_img(shot.width, shot.height, &dets);
        let area = img_rect_to_screen(area_img, window, shot.width, shot.height);
        let at = Point { x: area.x + area.width / 2, y: area.y + area.height / 2 };
        self.evidence.push(format!(
            "步骤5：气泡 {} 个；气泡区屏幕 ({}, {}) {}x{}；滚轮点 ({}, {})",
            bubbles_of(&dets).len(), area.x, area.y, area.width, area.height, at.x, at.y
        ));
        self.runner.ports.platform.move_pointer(at)?;
        self.wheel(at, BOTTOM_JUMP_NOTCHES, window)?;
        let mut prev = self.chat_fp(area)?;
        let mut stable = 0;
        let mut rounds = 0;
        while rounds < BOTTOM_CONFIRM_MAX {
            rounds += 1;
            self.wheel(at, BOTTOM_CONFIRM_NOTCHES, window)?;
            let next = self.chat_fp(area)?;
            if next == prev {
                stable += 1;
                if stable >= 2 {
                    break;
                }
            } else {
                stable = 0;
                prev = next;
            }
        }
        self.evidence.push(format!(
            "步骤5：大跳 {BOTTOM_JUMP_NOTCHES} 格后小步确认 {rounds} 次 → {}",
            if stable >= 2 { "已到底" } else { "达上限，按到底继续" }
        ));
        self.note_elapsed("滚轮 scroll_to_bottom", concat!(file!(), ":", line!()), started.elapsed());
        self.check_deadline("消息区滚到底")
    }

    /// 步骤6：当前屏找含 `message_text` 的气泡（多条取最下=最新）；没有就向上滚一步再找。
    pub(super) fn forward_right_click_bubble(&mut self, message_text: &str) -> Result<(), AutomationError> {
        let started = Instant::now();
        let mut notches: Option<i32> = None;
        let mut prev_top_text: Option<String> = None;
        let mut edge_fixes = 0;
        let mut backed_off = false;
        for step in 0..=SEARCH_MAX_STEPS {
            let label = format!("转发·6·找气泡#{step}");
            let (window, shot, dets) = self.yolo_detect_window(&label)?;
            let area_img = chat_area_img(shot.width, shot.height, &dets);
            let area = img_rect_to_screen(area_img, window, shot.width, shot.height);
            let at = Point { x: area.x + area.width / 2, y: area.y + area.height / 2 };
            let n = *notches.get_or_insert_with(|| step_notches(area_img.height, shot.height, window.height));

            let mut bubbles = bubbles_of(&dets);
            bubbles.sort_by_key(|b| b.bounds_rect().y);
            let ocr = self.yolo_ocr_bboxes(window, &shot, &bubbles, &format!("{label}·OCR"))?;
            let texts: Vec<String> = ocr
                .iter()
                .map(|bs| bs.iter().map(|b| b.text.as_str()).collect::<Vec<_>>().join(""))
                .collect();

            // 防漏：上一屏最上面气泡的文字应仍在本屏（重叠区）。
            if let Some(pt) = prev_top_text.as_ref().filter(|t| !t.trim().is_empty()) {
                let seen = texts.iter().any(|t| t.contains(pt.as_str()) || pt.contains(t.as_str()) && !t.is_empty());
                if !seen && !backed_off && n > 1 {
                    let back = (n / 2).max(1);
                    self.evidence.push(format!(
                        "步骤6：重叠核对失败（上屏顶部「{pt}」不在本屏）→ 回退 {back} 格，步长 {n}→{back}"
                    ));
                    self.wheel(at, back, window)?;
                    notches = Some(back);
                    backed_off = true;
                    continue;
                }
            }
            backed_off = false;

            // 本屏命中：取最下面的（最新）。
            let hit = bubbles
                .iter()
                .zip(texts.iter())
                .filter(|(_, t)| t.contains(message_text))
                .map(|(d, _)| d.clone())
                .last();
            if let Some(det) = hit {
                let r = det.bounds_rect();
                let adj = edge_adjust(r, area_img);
                if adj != 0 && edge_fixes < 2 {
                    edge_fixes += 1;
                    self.evidence.push(format!("步骤6：命中气泡贴边，滚 {adj} 格后重定位"));
                    self.wheel(at, adj, window)?;
                    continue;
                }
                let img_pt = random_point_in_central_half(r);
                let target = shot_point_to_screen(img_pt.x, img_pt.y, window, shot.width, shot.height);
                self.evidence.push(format!(
                    "步骤6：第 {step} 屏命中 {} conf={:.2} → 右击 ({}, {})",
                    det.class_name, det.conf, target.x, target.y
                ));
                self.ensure_not_frozen("已取消右击气泡")?;
                timed!(self, "右击 guarded_right_click", {
                    self.runner.ports.platform.guarded_right_click(target, window)
                })?;
                self.note_elapsed("步骤6 找气泡", concat!(file!(), ":", line!()), started.elapsed());
                return self.check_deadline("右击气泡");
            }

            if step == SEARCH_MAX_STEPS || started.elapsed() >= SEARCH_BUDGET {
                break;
            }

            // 向上滚一步；指纹不变（含懒加载复核）即到顶。
            prev_top_text = texts.first().cloned();
            let before = self.chat_fp(area)?;
            self.wheel(at, -n, window)?;
            // 每滚一步是一次独立尝试：重置单步计时（同 list.rs），总上限由 SEARCH_BUDGET 管。
            self.step_started = Instant::now();
            if self.chat_fp(area)? == before {
                std::thread::sleep(TOP_LAZY_LOAD_WAIT);
                if self.chat_fp(area)? == before {
                    self.evidence.push(format!("步骤6：向上第 {} 步画面不变 → 已到顶", step + 1));
                    return Err(AutomationError::NeedsHumanReview(format!(
                        "步骤6失败：已滚到最早消息，仍未找到包含「{message_text}」的气泡。"
                    )));
                }
            }
        }
        Err(AutomationError::NeedsHumanReview(format!(
            "步骤6失败：向上翻找 {} 步 / {}s 内未找到包含「{message_text}」的气泡。",
            SEARCH_MAX_STEPS,
            started.elapsed().as_secs()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, width: w, height: h }
    }

    #[test]
    fn step_is_sixty_percent_of_area() {
        // 500px 高（1:1 缩放）→ 300px → 15 格
        assert_eq!(step_notches(500, 1000, 1000), 15);
        assert_eq!(step_notches(20, 1000, 1000), 1);
    }

    #[test]
    fn edge_cut_detection() {
        let area = r(0, 100, 500, 400);
        assert_eq!(edge_adjust(r(10, 101, 100, 50), area), -1);
        assert_eq!(edge_adjust(r(10, 460, 100, 39), area), 1);
        assert_eq!(edge_adjust(r(10, 200, 100, 50), area), 0);
    }

    #[test]
    fn chat_area_defaults_without_dets() {
        let a = chat_area_img(1000, 1000, &[]);
        assert_eq!((a.y, a.y + a.height), (100, 780));
        assert_eq!((a.x, a.x + a.width), (300, 980));
    }
}
