//! 过程诊断落盘：每一步一张标注图，另拼一张纵向总图，全放在任务自己的目录里。
//!
//! ## 它解决的是什么
//!
//! 失败证据（`storage::EvidenceStore`）是**脱敏**的：识别到的文字框被涂成中灰，
//! 只能核对版面，答不了"它到底读成了什么"（`docs/todo.md` T6）。而真实故障里
//! 最要紧的判据恰恰是这一条——读到的是不是目标名字、置信度多少、框落在哪儿。
//!
//! 所以这里落的是**原图 + 叠加层**（不打码，操作者 2026-09-21 明确拍板）：
//! `steps/01-搜索下拉列表.png` 每步一张，`overview.png` 纵向拼起来。
//!
//! ## 为什么每步**立刻**落盘，而不是等跑完一起写
//!
//! 排查的往往是"跑到一半卡住/崩了"的任务。攒到结束再写，那一次就什么都看不到。
//! 每步写完立刻 flush，进程被强杀时盘上仍有已经走过的那几步。
//!
//! ## 为什么写盘失败只吞掉、不往上抛
//!
//! 这是**观测手段**，不是业务动作。诊断图写不下去（磁盘满、路径没权限）
//! 不该让一次本来能发出去的任务失败——那会把"记录不下来"变成"活儿干不成"。
//!
//! ## 几条落盘线，各答一个问题
//!
//! | 产出 | 回答 |
//! |---|---|
//! | `steps/*.png` | 它**看到了什么**（原图 + 区域框 + 文字框） |
//! | `overview.png` | 把这些步一次看全 |
//! | `events.jsonl` | 它**怎么判的**（每个候选过没过、为什么）——见 [`events`] |
//! | `raw/*.png` + `raw/*.json` | 拿什么读的、读出什么：**未标注**的 OCR 输入图 + 引擎 stdout 原文 |
//!
//! 最后那条是「能重跑」的前提（`docs/todo.md` T30）：`steps/` 上的图有框有字，
//! 拿它重跑 OCR 会读出另一套结果，所以原料必须单独留一份干净的。
//!
//! 画图那段在 [`page`]：这里只管"什么时候落、落多少张、收尾"。

mod events;
mod page;

/// 读出一个任务的事件流（界面「过程重放」用）。
///
/// 从子模块转出来：格式定义在 [`events`] 里，读的人（命令层、`tools/replay`）
/// 不必知道它是怎么分文件的。
pub use events::read_events;

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use automation_core::{Decision, DiagnosticRecorder, Observation, TaskId};

use crate::log_line;
use crate::task_log::{now_stamp, LOG_FILE_NAME, OVERVIEW_FILE_NAME, RAW_DIR, STEPS_DIR};

/// 总图最多拼多少步。
///
/// 单步图**一张都不少**（都在 `steps/` 里），这里限的只是那张拼图的高度：
/// 一次滚动扫描可能记下上百帧，全拼起来能有几万像素高，看图的人反而找不到
/// 出事的那一步。超了就只留**最后**这些步——失败就发生在尾部。
const MAX_OVERVIEW_STEPS: usize = 60;

/// 任务过程诊断的落盘器。
pub struct TaskDiagnostics {
    dir: PathBuf,
    title: String,
    /// 结构化事件流（每步"看到了什么 + 怎么判的"）。
    events: events::EventLog,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    font: Option<vision::text::TextFont>,
    /// 字体加载失败的原因，只留第一份（同一个原因重复报没有意义）。
    font_error: Option<String>,
    /// 已经记过多少步（与文件序号一致）。
    count: usize,
    /// 记过多少步的 OCR 原料（`raw/` 下那对文件）。收尾时用来决定要不要提一句。
    raw_count: usize,
    /// 已写好那几步的 PNG 字节，拼总图时用。
    ///
    /// 存 PNG 而不是 `RgbaImage`：一张图解码后是几 MB，六十张就能吃掉几百 MB
    /// 常驻内存，而压缩后的 PNG 只有几十 KB。
    pages: Vec<Vec<u8>>,
    /// 上一帧的（步骤名、指纹），用来跳过重复帧。
    previous: Option<(String, String)>,
}

impl TaskDiagnostics {
    /// `dir` 是这次任务的目录（`data/tasks/<任务ID>/`），`title` 写在总图顶上。
    pub fn new(dir: impl Into<PathBuf>, title: impl Into<String>) -> Self {
        let (font, font_error) = match vision::text::TextFont::load_system() {
            Ok(font) => (Some(font), None),
            Err(err) => (None, Some(err.to_string())),
        };
        let dir = dir.into();
        Self {
            events: events::EventLog::new(&dir),
            dir,
            title: title.into(),
            inner: Mutex::new(Inner { font, font_error, ..Inner::default() }),
        }
    }

    /// 落一张**未标注**的 OCR 输入图，返回它在任务目录里的相对路径。
    ///
    /// 就是当时喂给 OCR 的那一帧（[`Observation::frame`]）的字节，**不叠任何框**——
    /// 叠了框再重跑 OCR，读出来的就不是当时那套字了（`docs/todo.md` T30）。
    /// 编不出来/写不下去就返回 `None`：事件里如实记 `null`，不假装留过。
    fn write_ocr_input(&self, index: usize, observation: &Observation<'_>) -> Option<String> {
        let image = vision::pixels::to_rgba(observation.frame)
            .ok()
            .and_then(|rgba| vision::pixels::encode_png(&rgba).ok())?;
        let rel = format!("{RAW_DIR}/{}", page::image_name(index, observation.label));
        let path = self.dir.join(&rel);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(path, image).ok()?;
        Some(rel)
    }

    /// 落引擎 stdout 的**原文**，返回它在任务目录里的相对路径。
    ///
    /// 空原文**不落文件**（返回 `None`）：那说明引擎没留下原始文本，
    /// 而不是"读到了空"，留一个 0 字节的 json 会被读成后者。
    fn write_ocr_text(
        &self,
        index: usize,
        observation: &Observation<'_>,
        raw: &str,
    ) -> Option<String> {
        if raw.trim().is_empty() {
            return None;
        }
        let rel = format!("{RAW_DIR}/{}", page::raw_json_name(index, observation.label));
        let path = self.dir.join(&rel);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(path, raw.as_bytes()).ok()?;
        Some(rel)
    }

    /// 拼总图，并把这批诊断材料的去处写进任务日志。
    ///
    /// 由命令层在 `runner.run()` 返回后调用一次。
    pub fn finish(&self) {
        let Ok(inner) = self.inner.lock() else {
            return;
        };
        let total = inner.count;
        if total == 0 {
            return;
        }
        let decoded: Vec<_> = inner
            .pages
            .iter()
            .filter_map(|png| vision::pixels::decode_png(png).ok())
            .collect();
        let dropped = total.saturating_sub(decoded.len());

        let note = if dropped > 0 {
            format!("（总图只拼最后 {} 步，另有 {} 步在 {} 里）", decoded.len(), dropped, STEPS_DIR)
        } else {
            String::new()
        };
        let title = format!("{}  ·  共 {total} 步", self.title);

        // 总图是**收尾时**一次性拼的：解码每步 PNG、纵向拼装、再编码一张大图。
        // 它同样跑在任务线程上（命令层 `finish()` 就在 `run()` 返回之后），
        // 所以耗时必须自己报出来——否则"任务跑完还卡了好几秒"会找不到主人。
        let composing = Instant::now();
        if let Some(font) = inner.font.as_ref() {
            if let Ok(sheet) = vision::render::compose_overview(&title, &note, font, &decoded) {
                if let Ok(png) = vision::pixels::encode_png(&sheet) {
                    let _ = std::fs::write(self.dir.join(OVERVIEW_FILE_NAME), png);
                }
            }
        }
        let compose_ms = composing.elapsed().as_millis();
        let log_path = self.dir.join(LOG_FILE_NAME);
        log_line!(
            &log_path,
            &format!("⏱ 总图拼装 {compose_ms}ms（{total} 步：解码 + 纵向拼装 + 编码）"),
        );
        log_line!(
            &log_path,
            &format!(
                "过程诊断 : {STEPS_DIR}/ 共 {total} 步，{OVERVIEW_FILE_NAME} 是拼起来的总图{note}"
            ),
        );
        if inner.raw_count > 0 {
            // 这一句是给"要把现场拿出去重跑 OCR"的人看的：`raw/` 那对文件才是原料，
            // `steps/` 上的图有框有字，拿它重跑读出来的是另一套结果。
            log_line!(
                &log_path,
                &format!(
                    "过程诊断 : {RAW_DIR}/ 有 {} 步的 OCR 原料（未标注的输入图 + 引擎原文），\
                     可用离线重放工具重跑识别",
                    inner.raw_count
                ),
            );
        }
        if let Some(err) = inner.font_error.as_deref() {
            log_line!(
                &log_path,
                &format!("过程诊断 : ⚠️ 本机没有中文字体，图上只有框没有文字（{err}）"),
            );
        }
    }
}

impl DiagnosticRecorder for TaskDiagnostics {
    fn observe(&self, _task_id: TaskId, observation: &Observation<'_>) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        // 同一块区域、同一帧内容连着来多次（例如"等画面停稳"的轮询）不是新信息，
        // 只会在 `steps/` 里堆出一串一模一样的图。指纹是**内容**哈希，内容一不同就不算重复。
        let key = (
            observation.label.to_string(),
            observation.frame.fingerprint.clone(),
        );
        if inner.previous.as_ref() == Some(&key) {
            return;
        }
        inner.previous = Some(key);

        inner.count += 1;
        let index = inner.count;
        // 图片与事件用**同一个**时间戳：看图的人不该怀疑"图上的时间和事件里的时间"
        // 是不是同一步。
        let stamp = now_stamp();
        // 这一段是**在任务线程上**跑的（见端口契约）：渲染、编码、写盘花掉的每一毫秒
        // 都直接吃单步预算。所以要分段计时写进日志——它慢起来的表现是
        // "某一步莫名超时"，而超时那一刻现场已经过去了，只能靠这些行回溯。
        let started = Instant::now();
        let encoded = Instant::now();
        let png = match inner.font.as_ref() {
            Some(font) => page::render_page(font, &stamp, observation),
            // 没有字体也要留下画面：这一步的价值大半在"画面上是什么样"。
            None => vision::pixels::to_rgba(observation.frame)
                .ok()
                .and_then(|image| vision::pixels::encode_png(&image).ok()),
        };
        let encode_ms = encoded.elapsed().as_millis();
        let Some(png) = png else { return };

        let writing = Instant::now();
        let steps = self.dir.join(STEPS_DIR);
        let _ = std::fs::create_dir_all(&steps);
        let name = page::image_name(index, observation.label);
        // 写不下去也往下走：单步图没了，但过程不算白跑，总图那份还在内存里。
        let _ = std::fs::write(steps.join(&name), &png);

        // OCR 原料（未标注输入图 + 引擎 stdout 原文）**只在这几步**留：
        // 没做 OCR 的步骤没有"OCR 输入图"这回事，凭空留一张只会让人以为它读过。
        let (ocr_input, ocr_raw) = match observation.ocr_raw {
            Some(raw) => (
                self.write_ocr_input(index, observation),
                self.write_ocr_text(index, observation, raw),
            ),
            None => (None, None),
        };
        let write_ms = writing.elapsed().as_millis();
        if ocr_input.is_some() {
            inner.raw_count += 1;
        }
        // 事件流记的是**相对任务目录**的路径，界面用它拼 asset URL 直接读原图。
        self.events.read(
            index,
            &format!("{STEPS_DIR}/{name}"),
            &stamp,
            observation,
            ocr_input.as_deref(),
            ocr_raw.as_deref(),
        );

        inner.pages.push(png);
        // 总图只用得上最后这些步，更早的没有留着的必要（单步图已经在盘上了）。
        while inner.pages.len() > MAX_OVERVIEW_STEPS {
            inner.pages.remove(0);
        }
        let log_path = self.dir.join(LOG_FILE_NAME);
        log_line!(
            &log_path,
            &format!(
                "⏱ 单步图 {index:02} {}：渲染+编码 {encode_ms}ms  写盘 {write_ms}ms  \
                 合计 {}ms（底图 {}）",
                observation.label,
                started.elapsed().as_millis(),
                if observation.window.is_some() { "整窗" } else { "裁图" }
            ),
        );
    }

    /// 记一次判定。**与 [`Self::observe`] 分开落盘**：不是每一步都有判定
    /// （等着画面停稳的轮询只截不判），而每一次判定都必须能被单独读出来。
    fn decide(&self, _task_id: TaskId, decision: &Decision) {
        self.events.decide(decision);
    }
}

/// 组装总图标题。命令层用它拼一行"这是哪一次任务"。
///
/// 放在这里而不是命令层：标题的措辞与总图是同一件事，
/// 而这个模块已经知道任务目录里都有什么（`lib.rs` 正贴着行数上限）。
pub fn overview_title(task_id: TaskId, what: &str) -> String {
    let id = task_id.to_string();
    let short = &id[..8.min(id.len())];
    format!("任务 {short} 过程诊断  ·  {what}  ·  {}", now_stamp())
}

#[cfg(test)]
mod tests;
