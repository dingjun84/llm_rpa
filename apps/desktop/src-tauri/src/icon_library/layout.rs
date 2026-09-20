//! 图标库**放在哪**，以及「名字 ↔ 路径」之间那点换算。
//!
//! 这里只做位置与形状，不碰文件内容：目录怎么定位、名字怎么变成目录名、
//! 旧式的单文件长什么样。读图在 [`super::read`]，写盘在 [`super::write`]。
//!
//! 「一个名字 = 一个目录」这条约定（见模块文档）就落在 [`group_dir`] 上——
//! 它是全模块唯一把名字拼成路径的地方。
//!
//! ## 位置跟着数据目录走
//!
//! 图标库默认在**数据目录**下的 `icons/`。数据目录由 [`crate::data_dir`] 说了算
//! （程序运行当前路径下的 `data/`），这里**不再自己拼一遍** `data` 这个名字：
//! 两处各写一份，迟早有一天只改了一处，症状是「图标存进去了、列表里却看不见」。
//!
//! 这里**曾经**用编译期的 `CARGO_MANIFEST_DIR` 往上找 workspace 根，好处是
//! 「不管从哪儿启动都落在仓库里」。换成运行期的当前路径是有意的取舍：
//! 那种做法在二进制被拷到别的机器上跑时会**静默失效**——往上找不到
//! `[workspace]`，于是悄悄退回数据目录；而图标库找错了地方不报任何错，
//! 只表现为列表空着。现在它与配置文件、日志、证据图**同在一处**，
//! 界面上也显示得出来，找错了一眼能看见。

use std::path::{Path, PathBuf};

use super::ICONS_SUBDIR;

/// 图标库的默认位置：**数据目录**下的 `icons/`。
///
/// 返回 `None` 只在一种情况下出现：拿不到当前工作目录（`current_dir` 失败）。
/// 那时调用方要退回它自己的兜底目录，而不是硬用一个猜出来的路径。
pub fn default_dir() -> Option<PathBuf> {
    crate::data_dir::resolve().ok().map(|dir| dir.join(ICONS_SUBDIR))
}

/// 图标库目录。
///
/// 优先级：**配置里写的** > 数据目录下的 `icons/` > `fallback/icons`。
/// `fallback` 只在拿不到当前工作目录时用（生产入口给数据目录，测试入口给临时目录）。
///
/// 配置里写**相对路径**时挂在**程序运行当前路径**上（与数据目录同一条规则）：
/// 写 `data/icons` 比写一整条绝对路径好读，也不至于换个盘符就失效。
pub fn resolve_dir(configured: Option<&str>, fallback: &Path) -> PathBuf {
    if let Some(text) = configured.map(str::trim).filter(|text| !text.is_empty()) {
        let path = PathBuf::from(text);
        if path.is_absolute() {
            return path;
        }
        // 相对路径的基准取 `data_dir` 里那**唯一一处**定义，不在这儿另写一次
        // `current_dir()`。拿不到工作目录时原样返回相对路径：后续
        // `create_dir_all` / 读目录仍按进程当前目录解析，落在同一个地方；
        // 硬拼一个猜出来的基准反而会把它指到别处。
        return crate::data_dir::working_dir().map(|base| base.join(&path)).unwrap_or(path);
    }
    default_dir().unwrap_or_else(|| fallback.join(ICONS_SUBDIR))
}

/// 图标库目录，确保它存在。
pub fn ensure_dir(icons_dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(icons_dir)
        .map_err(|err| format!("无法创建图标库目录 {}：{err}", icons_dir.display()))
}

/// 名字 → 目录名。调用方**必须先过 [`super::validate_name`]**。
pub(super) fn group_dir(icons_dir: &Path, name: &str) -> PathBuf {
    icons_dir.join(name)
}

/// 旧式的**单文件**图标：`<名字>.png`（命令行 `screen_probe template` 产出的是这种）。
///
/// 现在仍然认它：图标库目录是给人看也给人手工放的，往里面丢一张 `聊天.png`
/// 应当在列表里看得见，而不是静默消失。
pub(super) fn legacy_file(icons_dir: &Path, name: &str) -> PathBuf {
    icons_dir.join(format!("{name}.png"))
}

pub(super) fn is_png(path: &Path) -> bool {
    path.extension()
        .map(|ext| ext.eq_ignore_ascii_case("png"))
        .unwrap_or(false)
}
