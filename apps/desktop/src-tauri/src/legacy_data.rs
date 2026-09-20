//! 把**旧版**留在 AppData 里的数据搬一次到新的数据目录。
//!
//! ## 为什么需要它
//!
//! 数据目录从 `%APPDATA%\com.example.wecom-local-rpa\` 换成了
//! 「程序运行当前路径下的 `data/`」（见 [`crate::data_dir`]）。
//! 换位置本身是一行代码的事，代价却全落在用户身上：配置文件里存着
//! **窗口几何、OCR 命令、四个核心区域的比例**，图标库里存着
//! **人对着屏幕一张一张框出来的模板**。不搬的话，升级后打开程序会看到
//! 「配置全空、图标库是空的」——而且**不报任何错**，像是数据丢了。
//!
//! 所以这里补一次搬迁。
//!
//! ## 只搬两样，且只搬一次
//!
//! 搬 `config.json` 与 `icons/`。这两样是**重做一遍很贵**的东西：
//! 前者要重新标定窗口、重新填 OCR 命令，后者要重新截一遍图。
//! 剩下的（`audit.sqlite`、`evidence/`、`task-*.log`）都是历史轨迹，
//! 重做不贵、留着也帮不上新位置什么忙，就不动它们了。
//!
//! 「只搬一次」的判据是**数据目录里还没有 `config.json`**：那一条足以说明
//! 这台机器还没在新位置落过配置。搬过之后、以及全新安装，都是空操作。
//!
//! ## 搬完不删旧的
//!
//! 旧目录**原样留着**。删除是不可逆的，而这里没有任何理由付这个代价——
//! 万一新位置用着不对，旧的还在原地可以再看一眼。多占的那点空间不值一提。

use std::path::{Path, PathBuf};

/// 旧位置里搬哪两样。
const CONFIG_FILE: &str = "config.json";
const ICONS_SUBDIR: &str = "icons";

/// 一次搬迁的结果，用来在界面上说清「搬了什么、从哪儿搬的」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    /// 旧位置。
    pub source: PathBuf,
    /// 搬过来的东西（人话，直接给界面和日志用）。
    pub items: Vec<String>,
}

impl Migration {
    /// 一句话描述这次搬迁。
    pub fn summary(&self) -> String {
        format!(
            "已从旧位置 {} 搬来 {}",
            self.source.display(),
            self.items.join("、")
        )
    }
}

/// 需要时把旧数据搬到 `data_dir`。
///
/// 返回 `Ok(None)` 表示**没什么可搬的**——旧位置不存在、已经搬过、
/// 或者旧位置里根本没有这两样东西。这三种都不是错误。
///
/// 返回 `Err` 表示**确实有东西要搬但搬不动**（磁盘满、权限不足）。
/// 这时**不阻断启动**：调用方记一条日志继续跑即可——
/// 搬不过来只是"要重标一次"，而启动失败是整个程序都用不了，两者不对等。
pub fn migrate(legacy: &Path, data_dir: &Path) -> Result<Option<Migration>, String> {
    if !legacy.is_dir() || data_dir.join(CONFIG_FILE).exists() {
        return Ok(None);
    }

    let mut items = Vec::new();

    let config = legacy.join(CONFIG_FILE);
    if config.is_file() {
        std::fs::copy(&config, data_dir.join(CONFIG_FILE))
            .map_err(|err| format!("无法搬来旧配置 {}：{err}", config.display()))?;
        items.push(CONFIG_FILE.to_string());
    }

    let icons = legacy.join(ICONS_SUBDIR);
    if icons.is_dir() {
        copy_tree(&icons, &data_dir.join(ICONS_SUBDIR))
            .map_err(|err| format!("无法搬来旧图标库 {}：{err}", icons.display()))?;
        items.push(format!("{ICONS_SUBDIR}/"));
    }

    if items.is_empty() {
        return Ok(None);
    }
    Ok(Some(Migration { source: legacy.to_path_buf(), items }))
}

/// 递归复制目录。
///
/// 同名文件**直接覆盖**：能走到这里就说明数据目录里还没有 `config.json`，
/// 也就是新位置一次都没被真正用过，那底下不可能有用户自己攒的东西。
/// 反过来「已存在就跳过」会留下隐患——新位置里恰好有个同名文件时，
/// 搬过来的结果就取决于谁先到，而这种事没有任何人查得出来。
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rpa-legacy-data-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 全新安装：旧位置根本不存在。这不该报错，也不该留下任何东西。
    #[test]
    fn a_missing_legacy_location_is_not_a_migration() {
        let root = temp_root("missing");
        let data = root.join("data");
        std::fs::create_dir_all(&data).unwrap();

        assert_eq!(migrate(&root.join("nowhere"), &data).unwrap(), None);
    }

    /// 旧位置在、但里面什么都没有（比如只跑过一次没保存过配置）。
    #[test]
    fn an_empty_legacy_location_is_not_a_migration() {
        let root = temp_root("empty");
        let legacy = root.join("appdata");
        let data = root.join("data");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(&data).unwrap();

        assert_eq!(migrate(&legacy, &data).unwrap(), None);
    }

    /// 已经搬过一次（数据目录里有配置了）⇒ 空操作。
    ///
    /// 这条挡的是**每次启动都搬一遍**：那会把用户在新位置改过的配置
    /// 用旧的那份覆盖回去，症状是「我明明改了设置，重启又变回去了」。
    #[test]
    fn an_existing_config_stops_the_migration() {
        let root = temp_root("already");
        let legacy = root.join("appdata");
        let data = root.join("data");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(legacy.join(CONFIG_FILE), br#"{"mode":"live"}"#).unwrap();
        std::fs::write(data.join(CONFIG_FILE), br#"{"mode":"demo"}"#).unwrap();

        assert_eq!(migrate(&legacy, &data).unwrap(), None);
        assert_eq!(
            std::fs::read_to_string(data.join(CONFIG_FILE)).unwrap(),
            r#"{"mode":"demo"}"#,
            "新位置的配置不能被旧的覆盖回去"
        );
    }

    /// 配置与图标库一起搬过来，并且图标库里**嵌套的目录**也要跟着来。
    ///
    /// 嵌套那层是必须的：一个图标一个目录（`icons/聊天/1.png`），
    /// 只搬顶层文件的话图标库里会只剩几个空的目录名。
    #[test]
    fn the_config_and_the_whole_icon_library_come_over() {
        let root = temp_root("full");
        let legacy = root.join("appdata");
        let data = root.join("data");
        std::fs::create_dir_all(legacy.join(ICONS_SUBDIR).join("聊天")).unwrap();
        std::fs::create_dir_all(&data).unwrap();

        std::fs::write(legacy.join(CONFIG_FILE), br#"{"mode":"live"}"#).unwrap();
        std::fs::write(legacy.join(ICONS_SUBDIR).join("聊天").join("1.png"), b"png").unwrap();
        std::fs::write(legacy.join(ICONS_SUBDIR).join("聊天").join("2.png"), b"png").unwrap();
        // 旧式的单文件图标也要跟着来。
        std::fs::write(legacy.join(ICONS_SUBDIR).join("通讯录.png"), b"png").unwrap();

        let migration = migrate(&legacy, &data).unwrap().expect("有东西可搬");
        assert_eq!(migration.source, legacy);
        assert_eq!(migration.items, vec![CONFIG_FILE.to_string(), "icons/".to_string()]);

        assert!(data.join(CONFIG_FILE).is_file());
        assert!(data.join(ICONS_SUBDIR).join("聊天").join("1.png").is_file());
        assert!(data.join(ICONS_SUBDIR).join("聊天").join("2.png").is_file());
        assert!(data.join(ICONS_SUBDIR).join("通讯录.png").is_file());
    }

    /// 同名文件以**旧的那份**为准。
    ///
    /// 这条钉的是 `copy_tree` 的覆盖语义：能搬就说明新位置一次都没被用过，
    /// 底下那份只可能是测试残留或手工乱放的，留着它反而会盖住真正的图标。
    #[test]
    fn a_same_named_file_is_overwritten_by_the_legacy_one() {
        let root = temp_root("overwrite");
        let legacy = root.join("appdata");
        let data = root.join("data");
        std::fs::create_dir_all(legacy.join(ICONS_SUBDIR)).unwrap();
        std::fs::create_dir_all(data.join(ICONS_SUBDIR)).unwrap();

        std::fs::write(legacy.join(ICONS_SUBDIR).join("通讯录.png"), b"real").unwrap();
        std::fs::write(data.join(ICONS_SUBDIR).join("通讯录.png"), b"stale").unwrap();

        migrate(&legacy, &data).unwrap().expect("有东西可搬");
        assert_eq!(
            std::fs::read(data.join(ICONS_SUBDIR).join("通讯录.png")).unwrap(),
            b"real"
        );
    }

    /// 旧目录原样留着，不删。
    #[test]
    fn the_legacy_location_is_left_alone() {
        let root = temp_root("keeps");
        let legacy = root.join("appdata");
        let data = root.join("data");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(legacy.join(CONFIG_FILE), b"{}").unwrap();

        migrate(&legacy, &data).unwrap().expect("有东西可搬");
        assert!(legacy.join(CONFIG_FILE).is_file(), "旧文件要留着，删除是不可逆的");
    }

    /// 摘要里要同时出现「从哪儿来」和「搬了什么」。
    #[test]
    fn the_summary_says_where_it_came_from_and_what_came_over() {
        let migration = Migration {
            source: PathBuf::from("C:/Users/someone/AppData/Roaming/com.example.wecom-local-rpa"),
            items: vec![CONFIG_FILE.to_string(), "icons/".to_string()],
        };
        let text = migration.summary();
        assert!(text.contains("AppData"), "{text}");
        assert!(text.contains(CONFIG_FILE), "{text}");
        assert!(text.contains(ICONS_SUBDIR), "{text}");
    }
}
