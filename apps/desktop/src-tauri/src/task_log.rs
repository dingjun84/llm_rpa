//! 任务日志：**后台活动的唯一落点**。
//!
//! ## 为什么需要它
//!
//! 界面只显示"当前状态 + 失败原因"，而排查「点了开始什么都没动」时真正要看的是
//! **每一步的先后顺序**——走到哪一步了、在哪一步停的、停之前的细节是什么。
//!
//! 为什么是文件而不是控制台：双击启动的 GUI 进程**没有控制台**，
//! 代码里那些 `eprintln!` 全都看不见。落文件是唯一可靠的观测点。
//!
//! ## 为什么单独成文件
//!
//! 开头那份**配置快照**（"当时到底按哪份配置跑的"）有一百来行，而它是一段
//! **写日志**的活，与命令层"装配、登记、起线程"的职责不是一回事。
//! `lib.rs` 早就贴着基线，所以这一段整段搬出来——加字段、加一行日志时
//! 改的是这里，不必再去挤 `lib.rs` 的额度。

use std::path::{Path, PathBuf};

use automation_core::{Failure, SendTask, TaskId, TaskState, Workflow};

use crate::runtime::{RunChoice, RuntimeConfig};

/// 一次任务一个目录：`data/tasks/<任务ID>/`。
///
/// ## 为什么从一个日志文件改成一个目录
///
/// 一次任务产出的东西不止日志：还有每一步的过程诊断图（`steps/`）和拼起来的
/// 总图（`overview.png`）。放进同一个目录，事后只要打开一个文件夹就能看全，
/// 不必在 `data/` 下按文件名前缀去捞。
pub const TASKS_DIR: &str = "tasks";

/// 任务日志的文件名（每个任务目录里固定叫这个）。
pub const LOG_FILE_NAME: &str = "task.log";

/// 过程诊断图的子目录名（任务目录里）。
pub const STEPS_DIR: &str = "steps";

/// **未标注**的 OCR 原料子目录名（任务目录里）。
///
/// 与 `steps/` 的分工：`steps/` 那些图上面**画了框和字**（给人看），
/// 拿它们重跑 OCR 会读出另一套结果；这里存的是当时**真正喂给 OCR 的那张图**
/// 与引擎的 stdout 原文，是"重跑一次 OCR"唯一可用的原料（`docs/todo.md` T30）。
pub const RAW_DIR: &str = "raw";

/// 过程诊断总图的文件名（任务目录里）。
pub const OVERVIEW_FILE_NAME: &str = "overview.png";

/// 结构化事件流的文件名（任务目录里）。
///
/// 与 `task.log` 的分工：那个是**给人读**的叙述，这个是**给程序读**的事件流
/// （界面「过程重放」与 `tools/replay` 都读它，见 `task_diagnostics/events.rs`）。
pub const EVENTS_FILE_NAME: &str = "events.jsonl";

/// 一个任务的目录。
pub fn task_dir(data_dir: &Path, task_id: TaskId) -> PathBuf {
    data_dir.join(TASKS_DIR).join(task_id.to_string())
}

/// 一个任务的日志文件。
pub fn task_log_path(data_dir: &Path, task_id: TaskId) -> PathBuf {
    task_dir(data_dir, task_id).join(LOG_FILE_NAME)
}

/// 把一行后台活动追加到任务日志（**低层写入**）。
///
/// 刻意保持极简：纯追加、每行带时间戳、**写完立刻 flush**——
/// 这样进程被强杀或崩溃时，已经写下的内容仍然在盘上。
///
/// ⚠️ 调用方不要直接用它，用 [`log_line!`]：那一版会把**调用点的文件、行号与模块**
/// 一并写进去。位置只有在宏的**展开处**取才是真的，转手一层就变成了"打日志的那一行"。
pub fn write_task_log_line(path: &Path, line: &str) {
    append_raw_line(path, &format!("[{}] {line}", now_stamp()));
}

/// 给一行日志挂上它的**出处**：`[src/runner/navigate.rs:93 runner::navigate] 正文`。
///
/// 摆在时间戳之后、正文之前：左边一律是"什么时候"与"从哪儿来"，
/// 正文从固定的第 4 段开始，扫日志时眼睛不用来回找。
pub fn with_origin(file: &str, line: u32, module: &str, text: String) -> String {
    format!("[{file}:{line} {module}] {text}")
}

/// 写一行任务日志，**自动带上调用点**（文件、行号、模块路径）。
///
/// ## 为什么非要机器来记
///
/// 排查时最常问的一句是「这行是谁打的、是从哪儿走到这儿的」——而日志自己答不出来。
/// 靠人维护"文案 → 代码位置"的对应，改一次文案就全错，而且是**静默**全错。
/// `file!()/line!()/module_path!()` 在**展开处**求值，位置永远是真的，
/// 也不必为此给每个调用点加参数。
///
/// 用法很直白——**一个已经算好的字符串**：
/// `log_line!(log_path, &format!("窗口 {}x{}", w, h))`、`log_line!(log_path, "=== 结束 ===")`。
///
/// 刻意**不收** `format!` 的参数列表：那样写的人会以为 `{x}` 内联捕获照常生效，
/// 而它在"先算字符串"这条路上是**静默失效**的（原样打印 `{x}`）。
///
/// 位置由 `log_line!` 自己取、再传给内部的 [`__log_line_at!`]：位置必须取在
/// **调用 `log_line!` 的那一行**，转一手就会变成宏定义所在的那一行。
/// 拆成两个宏是为了让多行调用点末尾那个逗号（`);` 前的 `,`）也能照常写——
/// `expr` 片段后面不允许跟可选的 `,`，所以只能显式列一条带逗号的规则。
#[macro_export]
macro_rules! log_line {
    ($path:expr, $line:expr) => {
        $crate::__log_line_at!(file!(), line!(), module_path!(), $path, $line)
    };
    ($path:expr, $line:expr,) => {
        $crate::__log_line_at!(file!(), line!(), module_path!(), $path, $line)
    };
}

/// [`log_line!`] 的内部实现，不要直接用（见它的说明：位置要取在调用点）。
#[macro_export]
macro_rules! __log_line_at {
    ($file:expr, $line_no:expr, $module:expr, $path:expr, $line:expr) => {
        $crate::task_log::write_task_log_line(
            &$path,
            &$crate::task_log::with_origin($file, $line_no, $module, $line.to_string()),
        )
    };
}

/// 追加一行**原样**文本（不加时间戳前缀）。
///
/// 事件流（`events.jsonl`）要的是纯 JSON，加前缀就没法解析了。
/// 这里的"建目录 → 追加 → flush"三件事与 [`write_task_log_line`] **共用一份实现**：
/// 各写一份的话，漏掉 `create_dir_all` 的那一份会静默失败——
/// 现象是"跑完一个任务，这个文件一个字节都没有"，而任务本身可能一切正常。
///
/// 目录不存在时**顺手建出来**：调用方只管给路径，不该还要记得先 `create_dir_all`。
pub fn append_raw_line(path: &Path, line: &str) {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    let _ = writeln!(file, "{line}");
    let _ = file.flush();
}

/// 当前时刻，**本地时区、给人读**的格式：`2026-09-21 17:52:01.123`。
///
/// ## 为什么不再用 Unix 毫秒
///
/// 原来是 `[1789985467562]`。排查时要把这个数换算成"那是几点几分"才能对上
/// 界面上的操作，而这一步每次都得在脑子里做一遍——日志正是给人读的，
/// 就不该逼人做这种换算。毫秒保留到最后三位：同一秒内相邻的两步要分得出先后。
pub fn now_stamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

/// 落一份**开跑前的配置快照**，返回日志文件路径。
///
/// 排查时第一个要问的就是"当时到底按哪份配置跑的"，而配置随时可能被改，
/// 事后再读 `config.json` 未必是当时那份。
///
/// 这里记的**运行参数**（模式 / 工作流 / 导航目标）全部取自 `choice`，
/// 也就是本次请求带来的那一份——它与界面上选的必须始终一致。
/// 不一致就说明请求那条链路出了问题，而不是"用户选错了"。
pub fn write_start_header(
    log_path: &Path,
    task_id: TaskId,
    task: &SendTask,
    choice: &RunChoice,
    config: &RuntimeConfig,
    icons_dir: &Path,
) {
    let navigate_only = choice.workflow == Workflow::NavigateOnly;

    log_line!(log_path, "=== 任务开始 ===");
    log_line!(log_path, &format!("任务 ID    : {task_id}"));
    // 「只做导航」根本不找人，任务请求里那个联系人字段是空的——
    // 那不是"漏填了"，所以不能显示成空白，否则看日志的人会以为操作者忘了填。
    log_line!(
        log_path,
        &format!(
            "目标联系人 : {}",
            if navigate_only {
                "（不适用：只做导航）".to_string()
            } else {
                task.external_contact_name.clone()
            }
        ),
    );
    // 记的是**本次请求带来的**模式（运行参数），不是配置里那个默认值。
    //
    // 模式与工作流是同一类东西：界面上选完就该按这个跑，不需要先点保存。
    // 所以"日志里记的"与"界面上选的"必须始终一致——不一致就说明请求那条链路
    // 出了问题，而不是"用户选错了"。
    log_line!(log_path, &format!("运行模式   : {:?}", choice.mode));
    // 「哪条工作流」必须写进日志。
    //
    // 三条路的失败现象**一模一样**（都是「找不到联系人」），而处置方向完全相反：
    // 搜索式要查联想下拉，列表式要查会话列表，导航式根本不找人。
    // 日志里没有这一行时，看日志的人只能靠"有没有点过搜索框"去反推，
    // 而那条证据要往后翻十几行才看得到——于是很容易把"跑的不是这条工作流"
    // 误判成"这条工作流坏了"。
    log_line!(
        log_path,
        &format!("工作流     : {}（{:?}）", choice.workflow.describe(), choice.workflow),
    );
    if navigate_only {
        // 记的是**图标库目录名**（`data/icons/` 下一级）；空 = 界面还没选。
        let target = choice.nav_target.trim();
        let shown = if target.is_empty() { "（没选）" } else { target };
        log_line!(log_path, &format!("导航目标   : {shown}"));
    }
    log_line!(log_path, &format!("窗口类名   : {}", config.window_class));
    log_line!(log_path, &format!("目标程序   : {:?}", config.wecom_exe));
    log_line!(log_path, &format!("OCR 程序   : {:?}", config.ocr_command));
    log_line!(log_path, &format!("标定尺寸   : {:?}", config.calibrated_window));
    log_line!(
        log_path,
        &format!(
            "滚动/扫描  : 每次 {} 格，最多 {} 次，最多扫 {} 轮，停稳等待 ≤{}ms",
            config.scroll_notches_per_step,
            config.max_scroll_attempts,
            config.max_search_sweeps,
            config.scroll_settle_ms
        ),
    );
    log_line!(
        log_path,
        &format!(
            "只填不发   : {}    卡死检测: {}    记录识别结果: {}",
            config.stop_before_send, config.liveness_check, config.log_ocr_candidates
        ),
    );
    // 「先点导航图标切视图」会改变"在哪一屏找联系人"，
    // 出问题时第一件要确认的就是"当时到底开没开、用的哪个图标"。
    if config.navigate_before_search {
        let names = config
            .nav_icon_templates
            .iter()
            .map(|name| name.trim())
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>()
            .join(" | ");
        log_line!(
            log_path,
            &format!(
                "切换视图   : 开    图标 {} 个 [{}]    图标库 {}    搜索区 {:?}    最低分 {:.2}",
                config.nav_icon_templates.iter().filter(|n| !n.trim().is_empty()).count(),
                names,
                icons_dir.display(),
                config.nav_strip,
                config.nav_icon_min_score
            ),
        );
    }
    // 放宽匹配是**临时措施**，但它会改变"点到谁"这个结果，
    // 所以必须在每次任务的配置快照里留痕：事后复盘时不用去翻当时改没改配置。
    if config.relaxed_name_match {
        log_line!(
            log_path,
            "⚠️ 姓名匹配 : 已放宽为「包含即可」（临时措施，非架构要求的逐字精确匹配）",
        );
        if !config.stop_before_send {
            log_line!(
                log_path,
                "⚠️ 注意     : 放宽匹配 + 允许真实发送同时打开 —— 存在「找错人」的风险",
            );
        }
    }
    log_line!(log_path, "=== 状态轨迹 ===");
}



/// 读出整份任务日志（界面「过程日志」面板用）。
pub fn read_task_log(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|err| {
        format!("读不到任务日志 {}：{err}", path.display())
    })
}

/// 数据目录里所有任务日志（按修改时间新→旧）。
///
/// 两种布局都要收：`data/tasks/<任务ID>/task.log`（现在）与
/// `data/task-xxxxxxxx.log`（历史任务）。**旧的不能不收**——那会让界面上的
/// 「任务历史」在升级后凭空少掉一截，看起来像数据丢了，而文件其实还在盘上。
pub fn list_task_log_paths(data_dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(data_dir.join(TASKS_DIR)) {
        paths.extend(
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.path().join(LOG_FILE_NAME))
                .filter(|p| p.is_file()),
        );
    }

    if let Ok(entries) = std::fs::read_dir(data_dir) {
        paths.extend(
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with("task-") && n.ends_with(".log"))
                        .unwrap_or(false)
                }),
        );
    }

    paths.sort_by_key(|p| {
        std::cmp::Reverse(
            p.metadata()
                .and_then(|m| m.modified())
                .ok()
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
        )
    });
    paths
}

/// 从日志路径推任务 ID。
///
/// 新布局取**目录名**（它就是完整 UUID）；旧布局取文件名，去掉 `task-` 前缀。
/// 判据只有这一处——`read_task_log` 与「任务历史」都靠它，两处各写一份的话，
/// 迟早会出现"列表里点得开、打开却没有内容"。
pub fn task_id_from_path(path: &Path) -> String {
    let is_new_layout = path.file_name().and_then(|n| n.to_str()) == Some(LOG_FILE_NAME);
    if is_new_layout {
        if let Some(dir) = path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()) {
            return dir.to_string();
        }
    }
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .trim_start_matches("task-")
        .to_string()
}

/// 剥掉一行的前缀，只留下正文：`[时间戳] [代码出处] 正文` → `正文`。
///
/// ## ⚠️ 为什么必须**循环**剥，不能只剥一段
///
/// 每行日志的前缀不止一个：时间戳由 [`write_task_log_line`] 加，代码出处
/// （`[file:line module]`）由 [`with_origin`] 加，两个都要剥掉，正文才露出来。
///
/// 只剥一段的后果很难看：正文前面还挂着 `[apps/.../task_log.rs:178 desktop_lib::task_log] `，
/// 于是 [`summary_from_log`] 里每一条 `strip_prefix` **全部落空**，
/// 而从磁盘恢复的每一条任务都会退化成「失败 / 只做导航」——
/// 日志文件本身好好地在盘上，看起来却像历史记录坏了（2026-09-22 实测）。
///
/// 为什么「剥到行首不再以 `[` 开头为止」是安全的：正文里出现 `[` 是常事
/// （`切换视图   : 开    图标 1 个 [通讯录] …`），但**行首**那个位置
/// 只有前缀会占着——正文前面永远垫着时间戳。
fn strip_line_prefixes(raw: &str) -> &str {
    let mut rest = raw.trim_start();
    while rest.starts_with('[') {
        let Some(end) = rest.find(']') else { break };
        rest = rest[end + 1..].trim_start();
    }
    rest
}

/// 从日志文件粗解析一份可展示的任务摘要（重启后恢复「任务历史」用）。
///
/// 解析失败返回 `None`，不把坏文件塞进列表。
pub fn summary_from_log(path: &Path) -> Option<crate::TaskView> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut id = String::new();
    let mut contact = String::new();
    let created_by = "（来自日志）".to_string();
    let mut state = TaskState::Failed;
    let mut failure_code: Option<String> = None;
    let mut failure_reason: Option<String> = None;
    let mut detail: Option<String> = None;
    let mut nav_target = String::new();

    for raw in text.lines() {
        let line = strip_line_prefixes(raw);
        if let Some(rest) = line.strip_prefix("任务 ID    : ") {
            id = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("目标联系人 : ") {
            contact = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("导航目标   : ") {
            nav_target = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("终态     : ") {
            state = TaskState::from_str_name(rest.trim()).unwrap_or(TaskState::Failed);
        } else if let Some(rest) = line.strip_prefix("失败码   : ") {
            failure_code = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("失败原因 : ") {
            failure_reason = Some(rest.trim().to_string());
        }
        // 状态迁移行（`Draft -> LaunchingClient`）留**最后一条**：它既是"走到哪儿了"，
        // 也是"停在哪一步"最直接的一句话。后面还有 `终态` 那几行，所以不能取第一条。
        if line.contains(" -> ") {
            detail = Some(line.to_string());
        }
    }
    if id.is_empty() {
        id = task_id_from_path(path);
    }
    if contact.is_empty() || contact.starts_with("（不适用") {
        if !nav_target.is_empty() && nav_target != "（没选）" {
            contact = format!("导航：{nav_target}");
        } else {
            contact = "（只做导航）".into();
        }
    }
    let failure = match (failure_code, failure_reason) {
        (Some(code), reason) => Some(Failure {
            code,
            reason: reason.unwrap_or_default(),
        }),
        (None, Some(reason)) if !reason.is_empty() => Some(Failure {
            code: "FROM_LOG".into(),
            reason,
        }),
        _ => None,
    };
    Some(crate::TaskView {
        id,
        external_contact_name: contact,
        text: String::new(),
        text_length: 0,
        created_by,
        state,
        state_label: state.describe().to_string(),
        detail,
        failure,
        evidence: Vec::new(),
        history: Vec::new(),
        awaiting_confirmation: false,
        evidence_artifacts: Vec::new(),
        log_path: Some(path.display().to_string()),
        from_log: true,
    })
}

#[cfg(test)]
mod tests;
