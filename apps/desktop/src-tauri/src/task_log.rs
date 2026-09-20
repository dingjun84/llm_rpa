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

use std::path::Path;

use automation_core::{SendTask, TaskId, Workflow};

use crate::runtime::{RunChoice, RuntimeConfig};

/// 把一行后台活动追加到任务日志。
///
/// 刻意保持极简：纯追加、每行带毫秒时间戳、**写完立刻 flush**——
/// 这样进程被强杀或崩溃时，已经写下的内容仍然在盘上。
pub fn append_task_log(path: &Path, line: &str) {
    use std::io::Write;
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let _ = writeln!(file, "[{ms}] {line}");
    let _ = file.flush();
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

    append_task_log(log_path, "=== 任务开始 ===");
    append_task_log(log_path, &format!("任务 ID    : {task_id}"));
    // 「只做导航」根本不找人，任务请求里那个联系人字段是空的——
    // 那不是"漏填了"，所以不能显示成空白，否则看日志的人会以为操作者忘了填。
    append_task_log(
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
    append_task_log(log_path, &format!("运行模式   : {:?}", choice.mode));
    // 「哪条工作流」必须写进日志。
    //
    // 三条路的失败现象**一模一样**（都是「找不到联系人」），而处置方向完全相反：
    // 搜索式要查联想下拉，列表式要查会话列表，导航式根本不找人。
    // 日志里没有这一行时，看日志的人只能靠"有没有点过搜索框"去反推，
    // 而那条证据要往后翻十几行才看得到——于是很容易把"跑的不是这条工作流"
    // 误判成"这条工作流坏了"。
    append_task_log(
        log_path,
        &format!("工作流     : {}（{:?}）", choice.workflow.describe(), choice.workflow),
    );
    if navigate_only {
        // 记的是**图标库目录名**（`data/icons/` 下一级）；空 = 界面还没选。
        let target = choice.nav_target.trim();
        let shown = if target.is_empty() { "（没选）" } else { target };
        append_task_log(log_path, &format!("导航目标   : {shown}"));
    }
    append_task_log(log_path, &format!("窗口类名   : {}", config.window_class));
    append_task_log(log_path, &format!("目标程序   : {:?}", config.wecom_exe));
    append_task_log(log_path, &format!("OCR 程序   : {:?}", config.ocr_command));
    append_task_log(log_path, &format!("标定尺寸   : {:?}", config.calibrated_window));
    append_task_log(
        log_path,
        &format!(
            "滚动/扫描  : 每次 {} 格，最多 {} 次，最多扫 {} 轮，停稳等待 ≤{}ms",
            config.scroll_notches_per_step,
            config.max_scroll_attempts,
            config.max_search_sweeps,
            config.scroll_settle_ms
        ),
    );
    append_task_log(
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
        append_task_log(
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
        append_task_log(
            log_path,
            "⚠️ 姓名匹配 : 已放宽为「包含即可」（临时措施，非架构要求的逐字精确匹配）",
        );
        if !config.stop_before_send {
            append_task_log(
                log_path,
                "⚠️ 注意     : 放宽匹配 + 允许真实发送同时打开 —— 存在「找错人」的风险",
            );
        }
    }
    append_task_log(log_path, "=== 状态轨迹 ===");
}
