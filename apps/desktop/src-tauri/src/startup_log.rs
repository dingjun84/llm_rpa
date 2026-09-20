//! 启动期的落文件日志：`<数据目录>/startup.log`。
//!
//! ## 为什么需要它
//!
//! **启动期是最看不到东西的一段**：窗口还没画出来，进程就没了。从操作者的角度看
//! 就是「窗口闪现了一下又退出」—— 没有文字、没有错误码、没有可查的东西。
//! 而真正的原因（数据目录建不出来、WebView2 起不来、状态初始化失败）全都发生在
//! **窗口出现之前**，也就是发生在"还看不到任何界面"的那一小段时间里。
//!
//! 本项目的观测手段一直是**落文件**（任务日志就是 `task-<ID>.log`），理由相同：
//! GUI 没有控制台。这里只是把同一条规矩往前挪到启动期。
//!
//! ## 为什么不能靠 `eprintln!`
//!
//! debug 版是控制台程序，`eprintln!` 确实能看见 —— 但那是**靠 run.cmd 的窗口**在传话，
//! 而 run.cmd 用 `start` 启动时，那个窗口一闪就没了。落文件与"怎么启动的"无关，
//! 双击 exe、从别的程序拉起、被计划任务拉起，都留得下现场。
//!
//! ## 每次启动截断
//!
//! 第一条写下去时**截断**旧文件，之后追加。这样这个文件永远只描述**这一次**启动，
//! 不会出现"翻到一半不知道哪几行是这次的"。代价是上一次的现场会被覆盖——
//! 所以 `run.cmd` 在启动失败时会把内容 `type` 出来给人看。
//!
//! ## 写不进去不报错
//!
//! 日志写失败**一律静默**。理由：它只是个观测手段，不该成为新的失败点——
//! 尤其"数据目录建不出来"本身就是要记录的那种失败，此时再去报"日志写不了"
//! 只会把真正的错误淹掉。

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

/// 进程启动时刻。日志里的 `+123ms` 是相对它的偏移。
///
/// 用**相对时间**而不是墙上时间：这个文件只描述一次启动，绝对时间没有意义，
/// 而"卡在哪一步、隔了多久"才是要看的。也省掉了一个日期格式化依赖。
static ORIGIN: OnceLock<Instant> = OnceLock::new();

/// 本次进程是否还没写过第一行。第一行负责截断旧文件。
static FIRST_LINE: AtomicBool = AtomicBool::new(true);

fn origin() -> &'static Instant {
    ORIGIN.get_or_init(Instant::now)
}

/// 日志文件放哪。
///
/// 正常情况下是数据目录；**数据目录建不出来时退回工作目录** —— 那正是最需要
/// 这条日志的一种失败，不能因为"日志目录建不出来"就把现场丢掉。
fn log_path() -> Option<PathBuf> {
    let working = crate::data_dir::working_dir().ok()?;
    match crate::data_dir::ensure() {
        Ok(dir) => Some(dir.join("startup.log")),
        Err(_) => Some(working.join("startup.log")),
    }
}

/// 记一行。
pub fn note(message: &str) {
    let Some(path) = log_path() else {
        return;
    };

    let first = FIRST_LINE.swap(false, Ordering::SeqCst);
    let opened = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(!first)
        .truncate(first)
        .open(&path);
    let Ok(mut file) = opened else {
        return;
    };

    let elapsed = origin().elapsed().as_millis();
    let _ = writeln!(file, "+{elapsed:>6}ms  {message}");
    // 崩在下一步时，这一行必须已经在盘上，否则日志会缺最后一句。
    let _ = file.flush();
}

/// 启动期要记的第一批东西，外加 panic 钩子。
///
/// 由 [`crate::run`] 在最开头调一次。**必须在建窗口之前**调：窗口建不出来时
/// 才来得及留下现场。
pub fn begin() {
    let working = crate::data_dir::working_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|err| format!("读不出来（{err}）"));
    let data = crate::data_dir::resolve()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|err| format!("读不出来（{err}）"));
    let exe = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|err| format!("读不出来（{err}）"));

    note("── 启动 ──");
    note(&format!("工作目录：{working}"));
    note(&format!("数据目录：{data}"));
    note(&format!("可执行文件：{exe}"));

    install_panic_hook();
}

/// panic 时先把消息写进日志，再交给原来的钩子。
///
/// 不替换、只包一层：原来的钩子会照常把消息打到 stderr（debug 版的控制台里能看到），
/// 那是另一条独立的观测路径，不该被这里吃掉。
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        note(&format!("!! panic：{info}"));
        note("!! 进程即将退出 —— 上面最后一条成功记录就是它走到的地方。");
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 这个文件只描述**一次**启动，所以第一行必须截断旧内容。
    ///
    /// 不截断的后果很能骗人：上一次启动失败留下的记录还在，人会照着旧现场查。
    #[test]
    fn the_first_line_truncates_and_the_rest_append() {
        let dir = std::env::temp_dir().join(format!("rpa-startup-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("startup.log");

        // 直接测写文件的两种模式，不走 `note`（它认的是进程工作目录，测试里改不了）。
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(false)
            .truncate(true)
            .open(&path)
            .unwrap();
        writeln!(file, "旧内容").unwrap();
        drop(file);

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "新内容").unwrap();
        drop(file);

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("新内容"), "追加的那行没写进去：{text}");
    }

    /// `note` 不该 panic，也不该因为写不进去就把调用方带崩。
    #[test]
    fn note_never_panics() {
        note("测试");
        note("测试");
    }
}
