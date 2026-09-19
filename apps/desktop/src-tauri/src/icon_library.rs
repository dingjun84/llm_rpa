//! 图标库：把「从窗口画面上框出来的一个小图标」存成一张**有名字的 PNG**。
//!
//! ## 为什么需要它
//!
//! 左侧导航栏那排图标上**没有文字**，OCR 读到的是一片空白，所以「先切到某个视图」
//! 这件事只能靠模板匹配（见 `docs/architecture.md`）。而模板从哪来？只能从
//! **真实画面**上截——那是操作者对着屏幕做的一次判断，程序替不了。
//!
//! 既然必须由人来框，就得有个地方把框出来的结果存下来、起个名字、下次还能用。
//! 否则「截一次、手抄一个路径」这件事每次都要重来，而且路径是抄不错才怪的东西。
//!
//! ## 为什么名字要卡这么严
//!
//! 名字会被拼成文件名（`<数据目录>/icons/<名字>.png`）。于是：
//!
//! - **不允许路径分隔符与保留字符**——名字一旦能带 `\` 或 `..`，
//!   「保存图标」就变成了「往任意路径写文件」；
//! - **不允许结尾的点**——Windows 会在创建文件时**静默**去掉结尾的点，
//!   于是「保存成功」之后按原名再也找不到那个文件，症状是「明明存了却说没有」；
//! - **拒绝设备名**（`CON` / `NUL` / `COM1` …）——这些名字在 Windows 上即使
//!   带上扩展名也建不出来，报出来的是没头没尾的「系统找不到指定的文件」。
//!
//! 这些规则只在**一处**（[`validate_name`]）实现，界面与命令都走它。
//!
//! ## 为什么不自动裁
//!
//! 程序绝不自己去猜「图标大概在这块」。猜错位置、裁到空白，都会变成一次
//! **静默的、看起来一切正常**的运行——匹配分数照样很高（它匹配的就是它自己
//! 刚裁的那块），点击照样发出去，只是点到了别的地方。模板只能由人框。

use std::path::{Path, PathBuf};

use automation_core::{Rect, Screenshot};
use serde::{Deserialize, Serialize};

/// 图标库在应用数据目录下的子目录名。
pub const ICONS_SUBDIR: &str = "icons";

/// 图标名的最大字符数。
///
/// 按**字符**算而不是字节：中文名三个字占 9 字节，按字节算会让人莫名其妙被拒。
const MAX_NAME_CHARS: usize = 40;

/// 文件名里一律不允许出现的字符（Windows 的非法字符集）。
const FORBIDDEN_CHARS: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Windows 保留设备名：带上扩展名也一样建不出文件。
const RESERVED_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// 图标库里的一项。
///
/// `image` 直接带一个 data URL，是为了让界面**一次调用**就能把列表画出来
/// （缩略图 + 名字 + 尺寸）。图标本身很小（上限 128×128），多带这一点载荷
/// 比让界面为每一项再发一次请求划算得多。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IconEntry {
    /// 名字（不含扩展名），也是配置里引用它的依据。
    pub name: String,
    /// PNG 的完整路径。
    pub file: String,
    /// 模板尺寸（窗口像素）。读不出来时是 0。
    pub width: u32,
    pub height: u32,
    /// 文件字节数。
    pub bytes: u64,
    /// `data:image/png;base64,...`；文件读不出来时是空串。
    pub image: String,
    /// **这张图现在不能当模板用**的原因（尺寸越界、文件损坏）。
    ///
    /// 刻意保留坏条目而不是从列表里藏掉：图标库里的文件是会被外部工具改的
    /// （有人用画图另存一遍），藏起来只会让人以为「我明明存过」。
    /// 显示出来，顺手就把「任务装配时才发现模板不可用」提前到了列表里。
    pub problem: Option<String>,
}

/// 校验并规范化图标名。
///
/// 返回值是**去掉首尾空白之后**的名字——调用方必须用它，不要再拿原始输入去拼路径，
/// 否则「保存时用了 trim 后的名字、删除时用了原始输入」这种不一致会漏出去。
pub fn validate_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();

    if name.is_empty() {
        return Err("图标名不能为空——起一个你自己认得出的名字，例如「通讯录」。".to_string());
    }

    let chars = name.chars().count();
    if chars > MAX_NAME_CHARS {
        return Err(format!(
            "图标名太长（{chars} 个字符，上限 {MAX_NAME_CHARS}）——它会变成文件名，短一点更好认。"
        ));
    }

    if let Some(bad) = name
        .chars()
        .find(|c| c.is_control() || FORBIDDEN_CHARS.contains(c))
    {
        return Err(format!(
            "图标名里不能有 {bad:?}——名字会被拼成文件名，含路径分隔符就等于往任意路径写文件。"
        ));
    }

    if name.starts_with('.') || name.ends_with('.') {
        return Err(
            "图标名不能以点开头或结尾——Windows 建文件时会静默去掉结尾的点，\
             之后按原名就再也找不到那个文件了。"
                .to_string(),
        );
    }

    if RESERVED_NAMES.contains(&name.to_ascii_lowercase().as_str()) {
        return Err(format!(
            "「{name}」是 Windows 的保留设备名，带上扩展名也建不出文件，换一个吧。"
        ));
    }

    Ok(name.to_string())
}

/// 图标库目录。
pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join(ICONS_SUBDIR)
}

/// 图标库目录，确保它存在。
pub fn ensure_dir(data_dir: &Path) -> Result<PathBuf, String> {
    let dir = dir(data_dir);
    std::fs::create_dir_all(&dir)
        .map_err(|err| format!("无法创建图标库目录 {}：{err}", dir.display()))?;
    Ok(dir)
}

/// 名字 → 文件路径。调用方**必须先过 [`validate_name`]**。
pub fn file_for(data_dir: &Path, name: &str) -> PathBuf {
    dir(data_dir).join(format!("{name}.png"))
}

/// 列出图标库里的全部图标。
///
/// 目录不存在 = 一张图标都还没存过 ⇒ 返回空列表，**不是**错误。
/// 把「还没开始用」当成失败，会让界面在第一次打开时就报一个红条。
pub fn list(data_dir: &Path) -> Result<Vec<IconEntry>, String> {
    let dir = dir(data_dir);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    for item in std::fs::read_dir(&dir)
        .map_err(|err| format!("读取图标库目录 {} 失败：{err}", dir.display()))?
    {
        let path = item
            .map_err(|err| format!("读取图标库目录项失败：{err}"))?
            .path();
        if !path.is_file() {
            continue;
        }
        let is_png = path
            .extension()
            .map(|ext| ext.eq_ignore_ascii_case("png"))
            .unwrap_or(false);
        if !is_png {
            // 只认 PNG：图标库目录是给人看也给人手工放的，混进别的文件就跳过，
            // 而不是报错——不认识的扩展名不该让整个列表打不开。
            continue;
        }
        let name = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        entries.push(read_entry(&path, name));
    }

    // 大小写不敏感排序：Windows 的文件系统本来就不区分大小写，
    // 按区分大小写排会让「Apple」和「apple」分在两处，看着像丢了东西。
    entries.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(entries)
}

/// 读一项。任何单项失败都变成 `problem`，**不让整个列表失败**。
fn read_entry(path: &Path, name: String) -> IconEntry {
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

    // 用**载入模板的同一个函数**量尺寸：顺手就能发现「这张图已经不能用了」。
    // 在这里显示出来，比等到任务装配时才炸好得多——那时人已经在准备发消息了。
    let (width, height, problem) = match vision::load_icon_template(path, name.clone()) {
        Ok(template) => (template.width, template.height, None),
        Err(err) => (0, 0, Some(err.to_string())),
    };

    IconEntry {
        name,
        file: path.display().to_string(),
        width,
        height,
        bytes: size,
        image,
        problem,
    }
}

/// 从一帧窗口截图里裁出 `rect`，存成名为 `name` 的图标。
///
/// `rect` 是**窗口图像坐标系**（调用方负责把界面上的框换算过来）。
pub fn save(
    data_dir: &Path,
    name: &str,
    shot: &Screenshot,
    rect: Rect,
) -> Result<IconEntry, String> {
    let name = validate_name(name)?;
    let dir = ensure_dir(data_dir)?;
    let path = dir.join(format!("{name}.png"));

    if path.exists() {
        // 不静默覆盖：重截一张同名图标是常见动作，但"覆盖"必须是人明确说出来的。
        // 静默覆盖的代价是——上一次那张还能用的模板没了，而人以为只是又存了一张。
        return Err(format!(
            "图标库里已经有「{name}」了。想重截就先在列表里把它删掉，\
             或者换个名字（例如「{name}-选中」）。"
        ));
    }

    let cropped = vision::crop(shot, rect).map_err(|err| format!("按框选的位置裁图失败：{err}"))?;
    let rgba = vision::pixels::to_rgba(&cropped).map_err(|err| err.to_string())?;
    let png = vision::pixels::encode_png(&rgba).map_err(|err| err.to_string())?;
    std::fs::write(&path, &png).map_err(|err| format!("写入 {} 失败：{err}", path.display()))?;

    // 写完立刻用**任务装配时的同一个载入函数**验一遍：尺寸不像图标、或者编码坏了，
    // 都要在这里报出来，并**把文件删掉**——图标库里只允许留能用的模板。
    // 留下来的话，它会以「分数很低」的形式在任务里发作，而那时人只会去怀疑阈值，
    // 不会想到「这张图根本不是图标」。
    if let Err(err) = vision::load_icon_template(&path, name.clone()) {
        let _ = std::fs::remove_file(&path);
        return Err(format!("这张图不能当图标模板：{err}"));
    }

    Ok(read_entry(&path, name))
}

/// 删除一张图标。
pub fn delete(data_dir: &Path, name: &str) -> Result<(), String> {
    let name = validate_name(name)?;
    let path = file_for(data_dir, &name);
    if !path.exists() {
        return Err(format!(
            "图标库里没有「{name}」——可能已经被删掉了，刷新一下列表。"
        ));
    }
    std::fs::remove_file(&path).map_err(|err| format!("删除 {} 失败：{err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    /// 一块**有纹理**的画面。纯色裁出来的模板没有判别力，
    /// `load_icon_template` 不会拒绝它，但保存它没有意义——所以测试里也不造纯色。
    fn frame(width: u32, height: u32) -> Screenshot {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[
                    (x * 13) as u8,
                    (y * 29) as u8,
                    ((x * 7 + y * 3) % 251) as u8,
                    255,
                ]);
            }
        }
        Screenshot {
            pixels,
            width,
            height,
            captured_at: SystemTime::now(),
            fingerprint: "test-frame".into(),
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rpa-llm-icon-lib-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_plain_name_is_accepted_and_trimmed() {
        assert_eq!(validate_name("  通讯录 ").unwrap(), "通讯录");
        assert_eq!(validate_name("nav-contacts_2").unwrap(), "nav-contacts_2");
        assert_eq!(validate_name("聊天 (选中)").unwrap(), "聊天 (选中)");
    }

    #[test]
    fn a_name_that_could_escape_the_directory_is_refused() {
        // 这些名字一旦放过去，"保存图标"就等于"往任意路径写文件"。
        for bad in [
            "../evil",
            "a/b",
            "a\\b",
            "C:evil",
            "a*b",
            "a?b",
            "a|b",
            "a\"b",
            "a<b",
            "a>b",
            "",
            "   ",
        ] {
            assert!(
                validate_name(bad).is_err(),
                "「{bad}」不该被接受，它会变成一个路径而不是一个名字"
            );
        }
    }

    #[test]
    fn a_name_windows_would_silently_mangle_is_refused() {
        // 结尾的点：Windows 建文件时会静默去掉 ⇒ 存完就找不到。
        let err = validate_name("通讯录.").unwrap_err();
        assert!(err.contains("静默"), "报错要说清为什么：{err}");
        // 开头是点：会变成一个隐藏文件。
        assert!(validate_name(".hidden").is_err());
        // 保留设备名：带上 .png 也建不出来。
        assert!(validate_name("CON").is_err());
        assert!(validate_name("nul").is_err());
        assert!(validate_name("COM1").is_err());
        // 但只是**包含**这些词的名字没问题——判据是"整个名字就是它"。
        assert!(validate_name("CON2").is_ok());
        assert!(validate_name("我的 NUL 图标").is_ok());
    }

    #[test]
    fn an_overlong_name_is_refused_by_character_count_not_bytes() {
        // 40 个汉字 = 120 字节。按字节算会误伤，按字符算才对。
        let exactly = "图".repeat(MAX_NAME_CHARS);
        assert!(validate_name(&exactly).is_ok(), "刚好 {MAX_NAME_CHARS} 个字符应当通过");
        assert!(validate_name(&"图".repeat(MAX_NAME_CHARS + 1)).is_err());
    }

    #[test]
    fn an_empty_library_is_an_empty_list_not_an_error() {
        let dir = temp_dir("empty");
        assert!(list(&dir).unwrap().is_empty(), "还没存过图标不是错误");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_then_list_then_delete_round_trips() {
        let dir = temp_dir("roundtrip");
        let shot = frame(60, 40);

        let saved = save(
            &dir,
            "通讯录",
            &shot,
            Rect { x: 10, y: 8, width: 20, height: 18 },
        )
        .unwrap();
        assert_eq!(saved.name, "通讯录");
        assert_eq!((saved.width, saved.height), (20, 18));
        assert!(saved.problem.is_none());
        assert!(saved.image.starts_with("data:image/png;base64,"));
        assert!(saved.bytes > 0);
        assert!(Path::new(&saved.file).exists());

        let listed = list(&dir).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "通讯录");
        assert_eq!((listed[0].width, listed[0].height), (20, 18));

        delete(&dir, "通讯录").unwrap();
        assert!(list(&dir).unwrap().is_empty());
        // 再删一次要明确报"没有"，而不是静默成功——静默成功会让界面以为删掉了。
        assert!(delete(&dir, "通讯录").is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_over_an_existing_name_is_refused() {
        let dir = temp_dir("collision");
        let shot = frame(60, 40);
        let rect = Rect { x: 5, y: 5, width: 16, height: 16 };
        save(&dir, "通讯录", &shot, rect).unwrap();

        let err = save(&dir, "通讯录", &shot, rect).unwrap_err();
        assert!(err.contains("已经有"), "要说清是重名：{err}");
        assert!(err.contains("删掉"), "要给出下一步怎么做：{err}");

        // 名字两边的空白不算另一个名字：`validate_name` 会先 trim，
        // 所以「 通讯录 」照样撞上已经存在的「通讯录」。
        assert!(save(&dir, " 通讯录 ", &shot, rect).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unusable_crop_leaves_nothing_behind() {
        let dir = temp_dir("unusable");
        let shot = frame(60, 40);

        // 太小：裁出来存不下一个有意义的模板。
        let err = save(
            &dir,
            "太小",
            &shot,
            Rect { x: 4, y: 4, width: 2, height: 2 },
        )
        .unwrap_err();
        assert!(err.contains("不能当图标模板"), "{err}");

        // 关键的一半：**文件不能留在库里**。留下来的话它会在任务里
        // 表现为"分数很低"，而那时人只会去怀疑阈值。
        assert!(
            list(&dir).unwrap().is_empty(),
            "被拒绝的模板不能在图标库里留下文件"
        );
        assert!(!file_for(&dir, "太小").exists());

        // 越出画面：同样是拒绝，同样不留文件。
        assert!(save(
            &dir,
            "越界",
            &shot,
            Rect { x: 50, y: 30, width: 30, height: 30 }
        )
        .is_err());
        assert!(list(&dir).unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_file_shows_up_with_a_reason_instead_of_breaking_the_list() {
        // 注意区分**数据目录**与**图标库目录**：`list` / `save` 收的是数据目录，
        // 图标库是它下面的 `icons/` 子目录。
        let data = temp_dir("broken");
        let icons = ensure_dir(&data).unwrap();
        let shot = frame(60, 40);
        save(&data, "好的", &shot, Rect { x: 5, y: 5, width: 16, height: 16 }).unwrap();

        // 模拟"有人拿别的工具把文件改坏了"：列表要照常打开，坏的那项带上原因。
        std::fs::write(icons.join("坏的.png"), b"this is not a png").unwrap();
        std::fs::write(icons.join("说明.txt"), b"ignored").unwrap();

        let listed = list(&data).unwrap();
        assert_eq!(listed.len(), 2, "非 PNG 文件要跳过，坏 PNG 要留下");
        let broken = listed.iter().find(|item| item.name == "坏的").unwrap();
        assert!(broken.problem.is_some(), "坏文件必须带上原因");
        assert_eq!((broken.width, broken.height), (0, 0));
        let good = listed.iter().find(|item| item.name == "好的").unwrap();
        assert!(good.problem.is_none());

        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn the_listing_is_sorted_case_insensitively() {
        let dir = temp_dir("sorted");
        let shot = frame(60, 40);
        for name in ["banana", "Apple", "apple2", "Cherry"] {
            save(&dir, name, &shot, Rect { x: 5, y: 5, width: 16, height: 16 }).unwrap();
        }
        let names: Vec<String> = list(&dir).unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["Apple", "apple2", "banana", "Cherry"]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
