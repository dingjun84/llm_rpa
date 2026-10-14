//! 转发 · 步骤5/6：消息区滚到底 + 向上分步滚动找目标气泡（任务 441744e5）。
//!
//! 滚轮统一走 `platform.scroll`：Windows 下由 `GhostboxMouseDesktop` 转成幽灵盒
//! `MoveMouseWheel(Z)`（Z 正上负下，已在 `notches_to_wheel_z` 换向并钳到 ±127）。
//! 本模块只用仓库约定：`notches > 0` = 向下（更新的消息），`< 0` = 向上（更早）。
//!
//! 到底 / 到顶：只对**气泡区**截图取指纹，滚动并停稳后连续 2 帧不变即判定。
//! 防漏：每步约滚气泡区高度的 35%（封顶 8 格；overlap 失败减半并本轮记住），滚后核对上一屏最上面气泡的文字
//! 仍在新屏；不在则回退半步并减小步长。

use std::time::{Duration, Instant};

use crate::ports::{AutomationError, Point, Rect, Screenshot};
use crate::yolo::{
    all_by_class, class, random_point_in_central_half, shot_point_to_screen, YoloDetection,
};

use super::Run;

/// 一格滚轮在企业微信消息区大约滚动的像素（Windows 默认 3 行）。用于把「60% 高度」换成格数。
/// 任务 1decbea4：按 60px/格算出 5 格，实测滚得太少；改 20px/格（约 3 倍），过量由重叠核对回退兜底。
const PX_PER_NOTCH: i32 = 20;
/// 初始向上步长上限：公式常给出 15+，VhidLab 实测过大→反复 overlap 回退。
const MAX_INITIAL_STEP: i32 = 8;
/// 到底：先一次大跳，再小步确认。
const BOTTOM_JUMP_NOTCHES: i32 = 120;
/// 任务 3f028f56：3 格×8 次没滚到底（幽灵盒每格位移小），改 15 格×最多 15 次。
const BOTTOM_CONFIRM_NOTCHES: i32 = 15;
const BOTTOM_CONFIRM_MAX: usize = 15;
/// 每次滚动后停稳等待。
const SCROLL_SETTLE: Duration = Duration::from_millis(250);
/// 到顶时企业微信可能懒加载更早的消息：指纹不变后再多等一下复核。
const TOP_LAZY_LOAD_WAIT: Duration = Duration::from_millis(700);
/// 向上找气泡的上限。
const SEARCH_MAX_STEPS: usize = 20;
/// 右击后等菜单出现；未见则重识别再点。
const RIGHT_CLICK_MENU_WAIT: Duration = Duration::from_millis(900);
const RIGHT_CLICK_MAX_TRIES: usize = 3;
const SEARCH_BUDGET: Duration = Duration::from_secs(40);
/// 每步滚动占气泡区高度的比例。
const STEP_RATIO: f32 = 0.35;
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
    let n = ((px / PX_PER_NOTCH as f32).round() as i32).max(1);
    n.min(MAX_INITIAL_STEP)
}

/// 重叠核对用短钥匙：整段 OCR 偶发抖动会导致假失败；取前若干字即可。
fn overlap_key(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).take(16).collect()
}

fn looks_like_context_menu(w: &crate::ports::PeerTopWindow) -> bool {
    let title = w.title.to_lowercase();
    title.contains("menu")
        || w.class_name.contains("DuiMenu")
        || w.class_name.contains("Menu")
        || (!w.is_main && w.rect.height < 400 && w.rect.width < 400)
}

/// 图像坐标矩形 → 屏幕矩形。
pub(super) fn img_rect_to_screen(r: Rect, window: Rect, sw: u32, sh: u32) -> Rect {
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
    ///
    /// 向上步长：公式初值（封顶 [`MAX_INITIAL_STEP`]）；overlap 失败则减半并**本轮记住**，
    /// 后续不再用过大步子。右击仍中心半区随机；点后短等菜单，未见则重识别再点。
    pub(super) fn forward_right_click_bubble(&mut self, message_text: &str) -> Result<(), AutomationError> {
        let started = Instant::now();
        let mut notches: Option<i32> = None;
        let mut prev_top_key: Option<String> = None;
        let mut edge_fixes = 0;
        let mut backed_off = false;
        let mut learned = false;
        for step in 0..=SEARCH_MAX_STEPS {
            let label = format!("转发·6·找气泡#{step}");
            let (window, shot, dets) = self.yolo_detect_window(&label)?;
            let area_img = chat_area_img(shot.width, shot.height, &dets);
            let area = img_rect_to_screen(area_img, window, shot.width, shot.height);
            let at = Point { x: area.x + area.width / 2, y: area.y + area.height / 2 };
            let n = *notches.get_or_insert_with(|| {
                let v = step_notches(area_img.height, shot.height, window.height);
                self.evidence.push(format!("步骤6：初始向上步长 {v} 格（封顶 {MAX_INITIAL_STEP}）"));
                v
            });

            let mut bubbles = bubbles_of(&dets);
            bubbles.sort_by_key(|b| b.bounds_rect().y);
            let ocr = self.yolo_ocr_bboxes(window, &shot, &bubbles, &format!("{label}·OCR"))?;
            let texts: Vec<String> = ocr
                .iter()
                .map(|bs| bs.iter().map(|b| b.text.as_str()).collect::<Vec<_>>().join(""))
                .collect();

            // 防漏：上一屏顶部气泡短钥匙应仍在本屏（重叠区）。
            if let Some(pk) = prev_top_key.as_ref().filter(|t| t.chars().count() >= 4) {
                let seen = texts.iter().any(|t| {
                    let k = overlap_key(t);
                    !k.is_empty() && (k.contains(pk.as_str()) || pk.contains(k.as_str()) || t.contains(pk.as_str()))
                });
                if !seen && !backed_off && n > 1 {
                    let back = (n / 2).max(1);
                    self.evidence.push(format!(
                        "步骤6：重叠核对失败（钥匙「{pk}」不在本屏）→ 回退 {back} 格，本轮记住步长 {n}→{back}"
                    ));
                    self.wheel(at, back, window)?;
                    notches = Some(back);
                    learned = true;
                    backed_off = true;
                    continue;
                }
                if seen && !learned {
                    // 首轮 overlap 通过：锁定当前步长，避免后面又被公式抬高（notches 已固定）。
                    learned = true;
                    self.evidence.push(format!("步骤6：重叠通过，本轮锁定向上步长 {n} 格"));
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
                self.right_click_bubble_with_retry(message_text, &det, &window, &shot)?;
                self.note_elapsed("步骤6 找气泡", concat!(file!(), ":", line!()), started.elapsed());
                return self.check_deadline("右击气泡");
            }

            if step == SEARCH_MAX_STEPS || started.elapsed() >= SEARCH_BUDGET {
                break;
            }

            prev_top_key = texts.first().map(|t| overlap_key(t)).filter(|k| !k.is_empty());
            let before = self.chat_fp(area)?;
            self.wheel(at, -n, window)?;
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

    /// 右击气泡：中心半区随机落点；短等菜单；未见则整窗重识别同文气泡再点（最多 [`RIGHT_CLICK_MAX_TRIES`]）。
    fn right_click_bubble_with_retry(
        &mut self,
        message_text: &str,
        first: &YoloDetection,
        window0: &Rect,
        shot0: &Screenshot,
    ) -> Result<(), AutomationError> {
        let mut det = first.clone();
        let mut window = *window0;
        let mut shot_w = shot0.width;
        let mut shot_h = shot0.height;
        for try_no in 1..=RIGHT_CLICK_MAX_TRIES {
            if try_no > 1 {
                let label = format!("转发·6·右击重识别#{try_no}");
                let (w, shot, dets) = self.yolo_detect_window(&label)?;
                window = w;
                shot_w = shot.width;
                shot_h = shot.height;
                let mut bubbles = bubbles_of(&dets);
                bubbles.sort_by_key(|b| b.bounds_rect().y);
                let ocr = self.yolo_ocr_bboxes(window, &shot, &bubbles, &format!("{label}·OCR"))?;
                let texts: Vec<String> = ocr
                    .iter()
                    .map(|bs| bs.iter().map(|b| b.text.as_str()).collect::<Vec<_>>().join(""))
                    .collect();
                let hit = bubbles
                    .iter()
                    .zip(texts.iter())
                    .filter(|(_, t)| t.contains(message_text))
                    .map(|(d, _)| d.clone())
                    .last();
                let Some(d) = hit else {
                    self.evidence.push(format!(
                        "步骤6：右击重试 {try_no}/{RIGHT_CLICK_MAX_TRIES} 未再找到含「{message_text}」的气泡"
                    ));
                    continue;
                };
                det = d;
            }
            let r = det.bounds_rect();
            // 落点仍用中心半区随机（与其它 YOLO 点击一致）；点偏靠重试兜底。
            let img_pt = random_point_in_central_half(r);
            let target = shot_point_to_screen(img_pt.x, img_pt.y, window, shot_w, shot_h);
            self.evidence.push(format!(
                "步骤6：右击尝试 {try_no}/{RIGHT_CLICK_MAX_TRIES} {} conf={:.2} → 随机半区 ({}, {})",
                det.class_name, det.conf, target.x, target.y
            ));
            self.ensure_not_frozen("已取消右击气泡")?;
            timed!(self, "右击 guarded_right_click", {
                self.runner.ports.platform.guarded_right_click(target, window)
            })?;
            if self.wait_context_menu(RIGHT_CLICK_MENU_WAIT)? {
                self.evidence.push(format!("步骤6：右击尝试 {try_no} 后菜单已出现"));
                return Ok(());
            }
            self.evidence.push(format!(
                "步骤6：右击尝试 {try_no} 后未见菜单窗，准备重识别再点"
            ));
            std::thread::sleep(Duration::from_millis(120));
        }
        // 仍交给步骤7 完整轮询 OCR「转发」——这里只尽量提高菜单出现概率。
        self.evidence.push("步骤6：多次右击仍未见菜单窗，交步骤7 继续轮询".into());
        Ok(())
    }

    fn wait_context_menu(&self, budget: Duration) -> Result<bool, AutomationError> {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            let peers = self.runner.ports.platform.list_peer_top_windows()?;
            if peers.iter().any(looks_like_context_menu) {
                return Ok(true);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(false)
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, width: w, height: h }
    }

    #[test]
    fn step_is_capped_fraction_of_area() {
        // 500px 高 ×0.35 → 175px → 9 格，封顶 8
        assert_eq!(step_notches(500, 1000, 1000), 8);
        assert_eq!(step_notches(20, 1000, 1000), 1);
        assert_eq!(step_notches(200, 1000, 1000), 4); // 70px → 4
    }

    #[test]
    fn overlap_key_trims() {
        assert_eq!(overlap_key("  hello world 12345"), "helloworld12345");
        assert_eq!(overlap_key("一二三四五六七八九十一二三四五六七八").chars().count(), 16);
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
