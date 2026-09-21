//! 「过程重放」的读盘侧：把一次任务的事件流交给界面。
//!
//! ## 它补的是哪一环
//!
//! 盘上已经有三种材料（见 [`crate::task_diagnostics`]）：单步标注图（看到了什么）、
//! 总图（一次看全）、事件流（怎么判的）。界面原先只能看 `task.log` 那一段文字，
//! 而排查之所以卡住，恰恰是因为文字回答不了「19 块文字里有「联系人」，为什么没找到」。
//!
//! 这个模块负责把事件流读出来、把图交给 WebView。
//!
//! ## 两件事，都不该挤进命令层
//!
//! 1. **定位这次任务的目录**：新旧两种布局的判断（见 [`task_dir_for`]）；
//! 2. **把事件里那个相对路径补成绝对路径**（见 [`attach_image_path`]）。
//!
//! 后一件的理由值得写下来：事件流存的是**相对任务目录**的路径，
//! 因为它要能整体挪走、也要能被人直接读（`cat events.jsonl` 时不该出现
//! 一台机器的绝对路径）。而界面读图走 asset 协议，协议要的是**本平台的绝对路径**
//! （Windows 是 `C:\...`、macOS 是 `/...`），且它的放行范围是按目录做前缀匹配的
//! ——路径里混进另一种分隔符就会匹配不上，图直接 403。
//! 所以"拼绝对路径"必须留在这边用 `Path::join` 做，不能让前端去猜分隔符。

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::task_diagnostics::read_events;
use crate::task_log::{self, EVENTS_FILE_NAME};

/// 「过程重放」要的东西：这次任务的目录 + 整条事件流。
#[derive(Debug, Serialize)]
pub struct TaskReplay {
    /// 任务目录的绝对路径（界面显示"材料在哪"，也用来拼图）。
    ///
    /// 旧布局的任务没有目录，为 `null` —— 界面据此说一句「这次任务没有过程重放」。
    pub dir: Option<String>,
    /// 事件流，一行一个对象（字段含义见 `task_diagnostics/events.rs`）。
    pub events: Vec<Value>,
}

/// 读出一次任务的过程事件流。
pub fn read_task_replay(data_dir: &Path, task_id: &str) -> TaskReplay {
    let Some(dir) = task_dir_for(data_dir, task_id) else {
        return TaskReplay { dir: None, events: Vec::new() };
    };
    let mut events = read_events(&dir.join(EVENTS_FILE_NAME));
    for event in &mut events {
        attach_image_path(event, &dir);
    }
    TaskReplay { dir: Some(dir.display().to_string()), events }
}

/// 任务目录：`data/tasks/<任务ID>/`。**目录真的存在**才算数。
///
/// ## 为什么认目录、不认日志文件
///
/// 旧布局的一次任务是一个**文件**（`data/task-<前8位>.log`），那个年代还没有
/// 过程诊断，自然也没有事件流。这时返回 `None`，界面说一句「这次任务只有日志」
/// ——比给一个空面板好：空面板看起来像程序坏了。
///
/// 也正因为只认这条路径，旧布局不会误读到 `data/events.jsonl` 这种
/// 本来就不存在的文件上（父目录是 `data/` 而不是任务目录）。
///
/// `pub(crate)`：界面上的「打开任务目录」按钮（`lib.rs` 的 `open_task_dir`）
/// 要的正是同一个答案。两处各拼一次路径，迟早会出现"这边打开的是新布局、
/// 那边认的是旧布局"。
pub(crate) fn task_dir_for(data_dir: &Path, task_id: &str) -> Option<PathBuf> {
    let trimmed = task_id.trim();
    if trimmed.is_empty() {
        return None;
    }
    let dir = data_dir.join(task_log::TASKS_DIR).join(trimmed);
    dir.is_dir().then_some(dir)
}

/// 给「看到了什么」那一条补上**绝对**图片路径（`image_path`）。
///
/// 只补 `kind == "read"` 的那些：判定条（`decision`）没有图。
/// 补不出来（比如老事件里没有 `image` 字段）就**原样留着**——
/// 界面拿不到 `image_path` 时会显示"这一步没有图"，而不是崩掉。
fn attach_image_path(event: &mut Value, dir: &Path) {
    let Some(object) = event.as_object_mut() else {
        return;
    };
    if object.get("kind").and_then(Value::as_str) != Some("read") {
        return;
    }
    let Some(relative) = object.get("image").and_then(Value::as_str) else {
        return;
    };
    let absolute = dir.join(relative).display().to_string();
    object.insert("image_path".into(), Value::String(absolute));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 新布局的任务：事件流读得出来，且每一步的图给的是**绝对**路径。
    ///
    /// 这一条是界面「过程重放」能不能显示出图的地基：
    /// 相对路径交给 `convertFileSrc` 是取不到图的（asset 协议按绝对路径放行）。
    #[test]
    fn a_new_layout_task_carries_absolute_image_paths() {
        let root = temp_root("replay_ok");
        let data_dir = root.join("data");
        let task = data_dir.join(task_log::TASKS_DIR).join("task-abc");
        std::fs::create_dir_all(task.join(task_log::STEPS_DIR)).unwrap();
        std::fs::write(
            task.join(EVENTS_FILE_NAME),
            concat!(
                r#"{"v":1,"kind":"read","step":"搜索下拉识别","image":"steps/01-x.png"}"#,
                "\n",
                r#"{"v":1,"kind":"decision","step":"搜索下拉识别","outcome":"转人工"}"#,
                "\n",
            ),
        )
        .unwrap();

        let replay = read_task_replay(&data_dir, "task-abc");
        assert_eq!(replay.dir.as_deref(), Some(task.display().to_string().as_str()));
        assert_eq!(replay.events.len(), 2, "读 + 判两条都要交出去");
        assert_eq!(
            replay.events[0]["image_path"],
            task.join("steps/01-x.png").display().to_string(),
            "读图要的是绝对路径"
        );
        assert!(
            replay.events[1].get("image_path").is_none(),
            "判定那一条没有图，不该凭空补一个"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// 旧布局的任务只有日志、没有目录：如实说"没有"，不拿空面板冒充。
    #[test]
    fn an_old_layout_task_reports_no_replay_material() {
        let root = temp_root("replay_old");
        let data_dir = root.join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        // 旧布局的文件名（`data/task-xxxxxxxx.log`），旁边**没有**同名目录。
        std::fs::write(data_dir.join("task-12345678.log"), "=== 任务开始 ===\n").unwrap();

        let replay = read_task_replay(&data_dir, "12345678");
        assert!(replay.dir.is_none(), "旧布局没有任务目录");
        assert!(replay.events.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    /// 任务刚起来、一步还没走：目录在、事件流还没生成。这是**正常**状态，
    /// 不是错误——界面显示"还没有可重放的步骤"等着它跑就行。
    #[test]
    fn a_task_that_has_not_moved_yet_is_not_an_error() {
        let root = temp_root("replay_empty");
        let data_dir = root.join("data");
        let task = data_dir.join(task_log::TASKS_DIR).join("task-abc");
        std::fs::create_dir_all(&task).unwrap();

        let replay = read_task_replay(&data_dir, "task-abc");
        assert_eq!(replay.dir.as_deref(), Some(task.display().to_string().as_str()));
        assert!(replay.events.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("llm-rpa-replay-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }
}