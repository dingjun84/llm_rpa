//! 读：把图标库目录读成**列表**，以及把配置里的名字展开成模板路径。
//!
//! 贯穿这里的一条原则：**单项失败不让整体失败**。图标库目录是给人看、
//! 也给人手工放的，混进一张坏图、一个 `说明.txt`，都不该让整个列表打不开——
//! 坏了就带上原因显示出来，而不是从列表里藏掉。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::layout::{group_dir, is_png, legacy_file};
use super::{IconEntry, IconVariant, validate_name};

/// 变体排序用的键：`2.png` 必须排在 `10.png` 前面。
///
/// 按字符串排会得到 1, 10, 2——而列表里那排缩略图是给人看的，
/// 顺序乱跳会让人以为存丢了。人工丢进来的非数字名排在后面，按名字排。
fn variant_order(stem: &str) -> (u8, u32, String) {
    match stem.parse::<u32>() {
        Ok(number) => (0, number, String::new()),
        Err(_) => (1, 0, stem.to_lowercase()),
    }
}

/// 一个名字下的全部变体。旧式单文件排在最前（它是最早那张）。
pub(super) fn variants_of(icons_dir: &Path, name: &str) -> Vec<IconVariant> {
    let mut found: Vec<PathBuf> = Vec::new();

    let legacy = legacy_file(icons_dir, name);
    if legacy.is_file() {
        found.push(legacy);
    }

    let group = group_dir(icons_dir, name);
    if let Ok(items) = std::fs::read_dir(&group) {
        let mut inside: Vec<PathBuf> = items
            .filter_map(|item| item.ok())
            .map(|item| item.path())
            .filter(|path| path.is_file() && is_png(path))
            .collect();
        inside.sort_by_key(|path| {
            variant_order(&path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default())
        });
        found.extend(inside);
    }

    found
        .iter()
        .map(|path| read_variant(icons_dir, path))
        .collect()
}

/// 读一张变体。任何单项失败都变成 `problem`，**不让整个列表失败**。
fn read_variant(icons_dir: &Path, path: &Path) -> IconVariant {
    let bytes = std::fs::read(path).ok();
    let size = bytes.as_ref().map(|data| data.len() as u64).unwrap_or(0);
    let image = bytes
        .as_ref()
        .map(|data| {
            use base64::Engine;
            format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(data)
            )
        })
        .unwrap_or_default();

    let relative = path
        .strip_prefix(icons_dir)
        .unwrap_or(path)
        .display()
        .to_string()
        .replace('\\', "/");

    // 用**载入模板的同一个函数**量尺寸：顺手就能发现「这张图已经不能用了」。
    // 在这里显示出来，比等到任务装配时才炸好得多——那时人已经在准备发消息了。
    let (width, height, problem) = match vision::load_icon_template(path, relative.clone()) {
        Ok(template) => (template.width, template.height, None),
        Err(err) => (0, 0, Some(err.to_string())),
    };

    IconVariant {
        file: path.display().to_string(),
        relative,
        width,
        height,
        bytes: size,
        image,
        problem,
    }
}

/// 列出图标库里的全部图标。
///
/// 目录不存在 = 一张图标都还没存过 ⇒ 返回空列表，**不是**错误。
/// 把「还没开始用」当成失败，会让界面在第一次打开时就报一个红条。
pub fn list(icons_dir: &Path) -> Result<Vec<IconEntry>, String> {
    if !icons_dir.exists() {
        return Ok(Vec::new());
    }

    let mut names: Vec<String> = Vec::new();
    for item in std::fs::read_dir(icons_dir)
        .map_err(|err| format!("读取图标库目录 {} 失败：{err}", icons_dir.display()))?
    {
        let path = item
            .map_err(|err| format!("读取图标库目录项失败：{err}"))?
            .path();
        let name = if path.is_dir() {
            path.file_name().map(|name| name.to_string_lossy().into_owned())
        } else if path.is_file() && is_png(&path) {
            // 只认 PNG：混进别的文件（说明.txt）就跳过，而不是报错——
            // 不认识的扩展名不该让整个列表打不开。
            path.file_stem().map(|stem| stem.to_string_lossy().into_owned())
        } else {
            None
        };
        let Some(name) = name else { continue };
        if name.trim().is_empty() {
            continue;
        }
        // 大小写不敏感去重：Windows 的文件系统本来就不区分大小写，
        // `Foo.png` 与 `foo/` 同时存在时不该在列表里出现两行同一个图标。
        if !names.iter().any(|existing| existing.eq_ignore_ascii_case(&name)) {
            names.push(name);
        }
    }

    // 大小写不敏感排序：按区分大小写排会让「Apple」和「apple」分在两处，看着像丢了东西。
    names.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| a.cmp(b)));

    Ok(names
        .into_iter()
        .filter_map(|name| {
            let variants = variants_of(icons_dir, &name);
            // 空目录（上一次保存失败留下的）不算一个图标：它底下什么都没有。
            if variants.is_empty() {
                return None;
            }
            let group = group_dir(icons_dir, &name);
            let path = if group.is_dir() { group } else { legacy_file(icons_dir, &name) };
            Some(IconEntry::new(name, path, variants))
        })
        .collect())
}

/// 一个图标的名字 → 它**全部变体**的 PNG 路径。
///
/// 配置里存的是名字，要载入时才展开成文件：这样以后往那个名字下补一张变体
/// 不必回配置页重勾一次。
///
/// 只做「名字 → 路径」，不读图——载入与校验由 `runtime::load_nav_icon_templates`
/// 统一负责，判据只留一处。
pub fn resolve_selection(icons_dir: &Path, names: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for raw in names {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        // 早先的配置里存的是**文件路径**。给一句能照着做的提示，
        // 而不是「图标库里没有叫 D:\...\x.png 的图标」——那看起来像是图标丢了。
        if raw.contains(['/', '\\']) || raw.to_ascii_lowercase().ends_with(".png") {
            return Err(format!(
                "配置里的「{raw}」看起来是个文件路径。图标库现在按名字引用：\
                 到「图标库」页勾一下要用的图标（一个名字下可以有多张图），再保存配置。"
            ));
        }
        let name = validate_name(raw)?;
        let variants = variants_of(icons_dir, &name);
        if variants.is_empty() {
            return Err(format!(
                "图标库里没有叫「{name}」的图标（目录 {}）。\
                 它可能被删掉了，或者图标库目录被改到了别处——到「图标库」页看一眼列表。",
                icons_dir.display()
            ));
        }
        for variant in variants {
            // 同一个文件被两个名字引用到（理论上不会）时只留一份，免得白匹配一遍。
            if seen.insert(variant.file.to_lowercase()) {
                paths.push(PathBuf::from(variant.file));
            }
        }
    }

    Ok(paths)
}
