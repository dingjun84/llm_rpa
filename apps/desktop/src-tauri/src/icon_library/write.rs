//! 写与删：把框选的结果存成新变体，以及两种粒度的删除。
//!
//! 两条规矩在这里最要紧：
//!
//! - **同名追加，不覆盖**——同一个图标的不同样子指的是同一个图标（见模块文档）。
//!   存之前先用**任务装配时的同一个载入函数**验一遍，不能当模板用的绝不留在库里；
//! - **删除的路径由校验过的名字重新拼**，不拿前端传来的字符串直接去开文件。
//!   这是个删除命令，那串字符串是从界面来的。

use std::path::Path;

use automation_core::{Rect, Screenshot};

use super::layout::{ensure_dir, group_dir, is_png, legacy_file};
use super::read::variants_of;
use super::{IconEntry, validate_name};

/// 从一帧窗口截图里裁出 `rect`，**追加**成名字 `name` 的一张新图。
///
/// `rect` 是**窗口图像坐标系**（调用方负责把界面上的框换算过来）。
///
/// 同一个名字反复调用 = 攒出多张变体（选中 / 未选中 / 带气泡…），**不覆盖**已有那些。
/// 文件编号取「当前没被占用的最小正整数」，不重排已有编号——重排会让正在看列表的人
/// 以为自己删错了。
pub fn save(
    icons_dir: &Path,
    name: &str,
    shot: &Screenshot,
    rect: Rect,
) -> Result<IconEntry, String> {
    let name = validate_name(name)?;
    ensure_dir(icons_dir)?;
    let group = group_dir(icons_dir, &name);
    std::fs::create_dir_all(&group)
        .map_err(|err| format!("无法创建图标目录 {}：{err}", group.display()))?;

    // 挑一个没被占用的编号。**不覆盖**：那个文件已经在的话，说明有人手工往里放过东西，
    // 跳过它比覆盖它安全——覆盖掉的是别人可能还想要的那张图。
    let mut index = 1u32;
    while group.join(format!("{index}.png")).exists() {
        index += 1;
    }
    let path = group.join(format!("{index}.png"));

    let cropped = vision::crop(shot, rect).map_err(|err| format!("按框选的位置裁图失败：{err}"))?;
    let rgba = vision::pixels::to_rgba(&cropped).map_err(|err| err.to_string())?;
    let png = vision::pixels::encode_png(&rgba).map_err(|err| err.to_string())?;
    std::fs::write(&path, &png).map_err(|err| format!("写入 {} 失败：{err}", path.display()))?;

    // 写完立刻用**任务装配时的同一个载入函数**验一遍：尺寸不像图标、或者编码坏了，
    // 都要在这里报出来，并**把文件删掉**——图标库里只允许留能用的模板。
    // 留下来的话，它会以「分数很低」的形式在任务里发作，而那时人只会去怀疑阈值，
    // 不会想到「这张图根本不是图标」。
    let relative = format!("{name}/{index}.png");
    if let Err(err) = vision::load_icon_template(&path, relative) {
        let _ = std::fs::remove_file(&path);
        remove_group_if_empty(&group);
        return Err(format!("这张图不能当图标模板：{err}"));
    }

    Ok(IconEntry::new(name.clone(), group, variants_of(icons_dir, &name)))
}

/// 删除一整个图标（它的全部变体）。
pub fn delete(icons_dir: &Path, name: &str) -> Result<(), String> {
    let name = validate_name(name)?;
    let group = group_dir(icons_dir, &name);
    let legacy = legacy_file(icons_dir, &name);

    let mut removed = false;
    if group.is_dir() {
        std::fs::remove_dir_all(&group)
            .map_err(|err| format!("删除 {} 失败：{err}", group.display()))?;
        removed = true;
    }
    if legacy.is_file() {
        std::fs::remove_file(&legacy)
            .map_err(|err| format!("删除 {} 失败：{err}", legacy.display()))?;
        removed = true;
    }
    if !removed {
        return Err(format!(
            "图标库里没有「{name}」——可能已经被删掉了，刷新一下列表。"
        ));
    }
    Ok(())
}

/// 删除**一张**变体，留下同一个名字下的其他图。
///
/// `relative` 是**相对图标库目录**的路径（`list` 带回来的那个，形如 `聊天/2.png`
/// 或旧式的 `聊天.png`）。
///
/// ## 为什么要校验它
///
/// 这是个**删除**命令，而 `relative` 来自前端。不校验的话，一个
/// `..\..\重要文件.png` 就能删掉磁盘上任何东西。所以这里把它限制成
/// 「图标库目录下的 1–2 段、最后一段是 .png、且第一段与名字对得上」，
/// 路径**由校验过的名字重新拼**，而不是拿前端那串字符串直接去开文件。
pub fn delete_variant(icons_dir: &Path, name: &str, relative: &str) -> Result<(), String> {
    let name = validate_name(name)?;
    let normalized = relative.trim().replace('\\', "/");
    let parts: Vec<&str> = normalized.split('/').filter(|part| !part.is_empty()).collect();

    let path = match parts.as_slice() {
        // 旧式单文件：`<名字>.png`
        [file] if file.eq_ignore_ascii_case(&format!("{name}.png")) => legacy_file(icons_dir, &name),
        // 一个变体：`<名字>/<编号>.png`
        [group, file]
            if group.eq_ignore_ascii_case(&name)
                && is_png(Path::new(file))
                && !file.contains(['\\', ':', '*', '?', '"', '<', '>', '|'])
                && *file != ".." =>
        {
            group_dir(icons_dir, &name).join(file)
        }
        _ => {
            return Err(format!(
                "「{relative}」不是「{name}」的图标文件，拒绝删除。\
                 （要删的是列表里那几张缩略图之一，路径由列表给出。）"
            ))
        }
    };

    if !path.is_file() {
        return Err(format!(
            "{} 已经不在了——可能刚才删过一次，刷新一下列表。",
            path.display()
        ));
    }
    std::fs::remove_file(&path).map_err(|err| format!("删除 {} 失败：{err}", path.display()))?;
    // 最后一张删掉之后，这个名字就不该再出现在列表里（留一个空目录会让人以为图标还在）。
    remove_group_if_empty(&group_dir(icons_dir, &name));
    Ok(())
}

/// 空目录就删掉。只在「刚删完/刚写失败」之后调用。
fn remove_group_if_empty(group: &Path) {
    let empty = std::fs::read_dir(group)
        .map(|mut items| items.next().is_none())
        .unwrap_or(false);
    if empty {
        let _ = std::fs::remove_dir(group);
    }
}
