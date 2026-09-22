//! `task_log` 的单元测试。
//!
//! 搬出来是因为 `task_log.rs` 贴着 `CONVENTIONS.md` §2 的 500 行上限
//! （§9 的做法：先把测试抽成同级 `xxx/tests.rs`，一行 `mod tests;`）。

use super::*;

/// 写日志要顺手把任务目录建出来。
///
/// **为什么钉住它**：`create(true)` 在父目录不存在时**静默失败**，
/// 现象是"跑完一个任务，日志一个字节都没有"——而任务本身可能一切正常，
/// 于是看起来像"日志功能坏了"，实际只是少了一次 `create_dir_all`。
#[test]
fn writing_a_line_creates_the_task_directory_and_stamps_the_time() {
    let root = temp_root("append");
    let path = task_log_path(&root, TaskId::nil());

    log_line!(&path, "=== 任务开始 ===");

    let text = std::fs::read_to_string(&path).unwrap();
    let line = text.lines().next().unwrap();
    assert!(line.ends_with("=== 任务开始 ==="), "实际：{line}");
    // 出处（文件:行号 + 模块）必须真的落在**这一行**上。
    //
    // **为什么钉住它**：排查时最常问的是「这行是谁打的」。靠人维护"文案 → 代码位置"
    // 的对应表，改一次文案就全错，而且是静默全错——所以位置必须由 `log_line!`
    // 在展开处取。这条断言就是防它哪天被"简化"掉。
    //
    // ⚠️ 判据写成 `file!()` / `module_path!()`，**不写死文件名**：这些测试
    // 2026-09-22 从 `task_log.rs` 搬到了 `task_log/tests.rs`（为了压回 §2 的行数上限），
    // 写死 "task_log.rs:" 的断言当场就失效了——而它想守的东西（"落在调用点"）没变。
    assert!(
        line.contains(file!()) && line.contains(module_path!()),
        "应当带上调用点的文件、行号与模块，实际：{line}"
    );
    let stamp = line.trim_start_matches('[').split(']').next().unwrap();
    // 可读的本地时间（而不是 Unix 毫秒）：能被按同一个格式解回来。
    assert!(
        chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%d %H:%M:%S%.3f").is_ok(),
        "时间戳应当是给人读的「年-月-日 时:分:秒.毫秒」，实际：{stamp}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 升级后「任务历史」必须两种布局都列出来。
///
/// **为什么钉住它**：旧的 `data/task-*.log` 不收，界面上就会凭空少一截历史，
/// 看起来像数据丢了，而文件其实都还在盘上。
#[test]
fn the_history_lists_both_the_new_directory_and_the_old_flat_file() {
    let root = temp_root("both_layouts");
    let id = TaskId::nil();
    log_line!(&task_log_path(&root, id), "=== 任务开始 ===");
    let old = root.join("task-abcd1234.log");
    log_line!(&old, "=== 任务开始 ===");

    let paths = list_task_log_paths(&root);

    assert_eq!(paths.len(), 2, "两种布局都要收：{paths:?}");
    assert!(paths.contains(&task_log_path(&root, id)));
    assert!(paths.contains(&old));
    assert_eq!(task_id_from_path(&task_log_path(&root, id)), id.to_string());
    assert_eq!(task_id_from_path(&old), "abcd1234");
    let _ = std::fs::remove_dir_all(root);
}

/// 行首前缀有几段就剥几段，正文里那个 `[` 不许动。
///
/// **为什么钉住它**：剥前缀用的是"行首是不是 `[`"这条判据，而正文里带方括号
/// 本来就很常见（列表元信息里的 `[通讯录]`）。判据一旦写成"找到第一个 `]`"，
/// 正文就会被从中间劈开——症状是详情里少半句话，不报任何错。
#[test]
fn the_line_prefixes_are_peeled_until_the_body_starts() {
    assert_eq!(
        strip_line_prefixes(
            "[2026-09-22 15:49:28.744] [a/b.rs:1 m] 任务 ID    : 1111-2222"
        ),
        "任务 ID    : 1111-2222"
    );
    // 老格式只有时间戳一段。
    assert_eq!(strip_line_prefixes("[1789982003734] 目标联系人 : 张三"), "目标联系人 : 张三");
    // 正文里的方括号是正文的一部分。
    assert_eq!(
        strip_line_prefixes("[ts] [a/b.rs:1 m] 切换视图   : 开    图标 1 个 [通讯录]"),
        "切换视图   : 开    图标 1 个 [通讯录]"
    );
    // 没有前缀的行原样返回（前导空白照旧去掉）。
    assert_eq!(strip_line_prefixes("  裸行"), "裸行");
}

/// 带「代码出处」段的日志，仍然要能解析成一条**真实**的任务摘要。
///
/// **为什么钉住它**：每行多一段前缀（`[file:line module]`）之后，
/// 解析里每一条 `strip_prefix` 都落空——于是从磁盘恢复的任务**全部**
/// 显示成「失败 / 只做导航」，详情里还挂着 `task_log.rs:178` 这种字样。
/// 这种故障看起来像"历史记录坏了/路径不对"，实际只是少剥了一层前缀，
/// 所以必须有用例把它钉死。
#[test]
fn a_disk_task_with_code_origins_still_parses_as_a_real_task() {
    let root = temp_root("origins");
    let id = TaskId::nil();
    let path = task_log_path(&root, id);
    // 逐行手写而不是 log_line!：要的就是"时间戳 + 出处"两段前缀这个形态，
    // 而 log_line! 只加时间戳（出处由调用点传进来）。
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        concat!(
            "[2026-09-22 15:49:28.744] [apps/desktop/src-tauri/src/task_log.rs:178 desktop_lib::task_log] 任务 ID    : 11111111-2222-3333-4444-555555555555\n",
            "[2026-09-22 15:49:28.744] [apps/desktop/src-tauri/src/task_log.rs:181 desktop_lib::task_log] 目标联系人 : 张三\n",
            "[2026-09-22 15:49:28.745] [apps/desktop/src-tauri/src/lib.rs:257 desktop_lib] Draft -> LaunchingClient\n",
            "[2026-09-22 15:49:39.327] [apps/desktop/src-tauri/src/lib.rs:257 desktop_lib] Sending -> Completed\n",
            "[2026-09-22 15:49:39.400] [apps/desktop/src-tauri/src/lib.rs:679 desktop_lib] 终态     : Completed\n",
        ),
    )
    .unwrap();

    let view = summary_from_log(&path).expect("一条正常的日志必须解析得出来");

    assert_eq!(view.id, "11111111-2222-3333-4444-555555555555");
    assert_eq!(view.external_contact_name, "张三");
    assert_eq!(view.state, TaskState::Completed, "终态读错了就会整列显示成失败");
    assert!(view.failure.is_none(), "这条没失败：{:?}", view.failure);
    assert_eq!(view.detail.as_deref(), Some("Sending -> Completed"), "取最后一条状态行");
    // 诊断信息（出处）不能漏进给人看的详情里。
    assert!(
        !view.detail.unwrap().contains(".rs:"),
        "详情里不该带代码出处"
    );
    let _ = std::fs::remove_dir_all(root);
}

fn temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("llm-rpa-log-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}
