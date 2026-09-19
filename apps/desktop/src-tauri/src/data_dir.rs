//! 运行期数据放在哪：**程序运行当前路径**下的 `data/`。
//!
//! ## 为什么不放 AppData
//!
//! 以前用 `app_data_dir()`（`%APPDATA%\com.example.wecom-local-rpa\`）。
//! 那个位置的问题是**没人找得到**：目录名是包标识符，翻一次要半天；
//! 而这里存的恰恰是最需要人看的几样东西——配置文件、图标模板、任务日志、证据图。
//! 更麻烦的是备份与迁移：想把这个项目拷到另一台机器上接着用，
//! 得先知道去哪儿找这些文件。
//!
//! 现在统一到「程序运行当前路径下的 `data/`」，整个目录可以整体拷走。
//!
//! ## 「当前路径」的准确含义
//!
//! 是进程的**工作目录**（[`std::env::current_dir`]），**不是**可执行文件所在目录。
//! 两者在**双击 exe 时通常一致**，但从别处启动时不同：
//!
//! ```text
//! C:\> D:\tools\rpa\desktop.exe      ← 工作目录是 C:\，data 会建在 C:\data
//! ```
//!
//! 这是「相对路径」这个词的必然含义，也是它唯一的代价。
//! 之所以接受这个代价：实际使用的目录**会显示在界面上**
//! （`RuntimeInfo::data_dir`，见 `RuntimePanel` 底部那一行），
//! 路径不对时一眼就能看见，而不是等到「配置怎么没生效」再来查。
//!
//! ## 启动时检查
//!
//! [`ensure`] 在 `AppState::new` 里被调用，也就是**每次启动都会走一遍**：
//! 不存在就建，建不出来就让启动失败。
//!
//! 不降级、不回退到别的目录——数据目录是配置、图标库、审计库的唯一落脚点，
//! 找不到它时程序**没有**正确的工作方式；硬跑下去只会把数据写到
//! 用户不知道的地方，而那种问题事后极难查（「我明明保存了」）。

use std::path::{Path, PathBuf};

/// 数据目录名（相对程序运行当前路径）。
pub const DATA_DIR_NAME: &str = "data";

/// 程序运行当前路径（进程的**工作目录**）。
///
/// 单独暴露出来，是因为「相对路径挂在哪儿」这条规则只有一处定义：
/// 数据目录自己是 `working_dir()/data`，图标库目录里那条相对路径也挂在
/// 同一个地方（见 `icon_library::layout`）。各写一份 `current_dir()` 的话，
/// 迟早会有一天只改了一处，而症状是「图标存进去了，列表里却没有」。
pub fn working_dir() -> Result<PathBuf, String> {
    std::env::current_dir().map_err(|err| format!("无法确定程序运行目录：{err}"))
}

/// 数据目录的路径，**不检查是否存在**。
///
/// 需要「拿到路径并且确保能用」时用 [`ensure`]。
pub fn resolve() -> Result<PathBuf, String> {
    Ok(working_dir()?.join(DATA_DIR_NAME))
}

/// 数据目录的路径，**确保它存在且可用**。程序启动时调用。
pub fn ensure() -> Result<PathBuf, String> {
    let dir = resolve()?;
    ensure_dir(&dir)?;
    Ok(dir)
}

/// 确保这个目录存在。拆出来是为了能对**任意路径**做测试——
/// [`ensure`] 固定按当前工作目录算，测试跑去建 `current_dir()/data`
/// 会把仓库弄脏（`icon_library` 那边踩过一次同类问题）。
///
/// 三种情况：
///
/// - 不存在 → 创建；
/// - 已是目录 → 直接通过（**幂等**，重复调用不算错）；
/// - 被一个**同名文件**占着 → 明确报错。
///
/// 最后那种不单独判的话，`create_dir_all` 会给出「文件已存在」之类的话，
/// 而人对着一条提到 `data` 的报错，很难想到「去把那个文件改个名」。
pub fn ensure_dir(dir: &Path) -> Result<(), String> {
    if dir.exists() && !dir.is_dir() {
        return Err(format!(
            "{} 已经存在，但它是个文件而不是目录——它挡住了数据目录。请把它改名或删掉。",
            dir.display()
        ));
    }
    std::fs::create_dir_all(dir).map_err(|err| format!("无法创建数据目录 {}：{err}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rpa-data-dir-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 数据目录挂在**工作目录**下，而不是可执行文件所在目录。
    /// 这条钉住的是「相对路径」这个约定的具体含义。
    #[test]
    fn the_data_dir_hangs_off_the_working_directory() {
        let expected = std::env::current_dir().unwrap().join(DATA_DIR_NAME);
        assert_eq!(resolve().unwrap(), expected);
    }

    #[test]
    fn resolve_is_stable_within_one_run() {
        assert_eq!(resolve().unwrap(), resolve().unwrap());
    }

    #[test]
    fn ensure_dir_creates_a_missing_directory() {
        let dir = temp_root("create").join("data");
        ensure_dir(&dir).unwrap();
        assert!(dir.is_dir());
    }

    /// 重复调用不算错：每次启动都会走一遍，第二次启动时目录已经在了。
    #[test]
    fn ensure_dir_is_idempotent() {
        let dir = temp_root("idempotent").join("data");
        ensure_dir(&dir).unwrap();
        ensure_dir(&dir).unwrap();
        assert!(dir.is_dir());
    }

    #[test]
    fn ensure_dir_refuses_a_path_occupied_by_a_file() {
        let root = temp_root("occupied");
        let file = root.join("data");
        std::fs::write(&file, b"not a directory").unwrap();

        let err = ensure_dir(&file).unwrap_err();
        assert!(
            err.contains("文件"),
            "错误要说清是被文件挡住了，否则人会去查权限：{err}"
        );
    }

    #[test]
    fn ensure_dir_creates_nested_parents() {
        let dir = temp_root("nested").join("a").join("b").join("data");
        ensure_dir(&dir).unwrap();
        assert!(dir.is_dir());
    }
}
