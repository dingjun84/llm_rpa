//! 图标库的行为测试。
//!
//! 单独放一个文件，是因为它同时压着四个部分：名字校验、目录定位、读取、写入。
//! 按「测哪个模块就放哪个模块」拆开，那几个共用的夹具（[`frame`] / [`temp_dir`] /
//! [`BOX`]）就得复制三份——复制出来的夹具迟早会各自长歪。

use super::layout::{group_dir, legacy_file};
use super::*;

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use automation_core::{Rect, Screenshot};

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

/// 一张 16×16 的框，够大到能当模板。
const BOX: Rect = Rect { x: 5, y: 5, width: 16, height: 16 };

#[test]
fn a_plain_name_is_accepted_and_trimmed() {
    assert_eq!(validate_name("  聊天 ").unwrap(), "聊天");
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
    let err = validate_name("聊天.").unwrap_err();
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

/// 图标库目录**默认**落在数据目录下的 `icons/`，而不是 AppData。
///
/// 这条盯的是「图标到底存哪去了」：默认位置写错不会报任何错，
/// 只会让人翻半天、或者以为图标丢了。
#[test]
fn the_default_directory_lives_under_the_data_dir() {
    let fallback = std::path::Path::new("Z:/fallback-data");
    let resolved = resolve_dir(None, fallback);
    let data_dir = crate::data_dir::resolve().expect("测试进程一定有当前工作目录");

    assert_eq!(resolved, data_dir.join(ICONS_SUBDIR));
    assert!(
        resolved.ends_with(std::path::Path::new("data").join(ICONS_SUBDIR)),
        "默认图标库是数据目录下的 icons/：{}",
        resolved.display()
    );
    assert!(
        !resolved.starts_with(fallback),
        "拿得到当前工作目录时不该退回兜底目录：{}",
        resolved.display()
    );
}

/// 配置里写的目录优先；相对路径挂在**程序运行当前路径**上——
/// 与数据目录同一条规则，基准取自 `data_dir`，不在这儿另写一次 `current_dir()`。
#[test]
fn a_configured_directory_wins_and_relative_paths_hang_off_the_working_dir() {
    let fallback = std::path::Path::new("Z:/fallback-data");

    assert_eq!(
        resolve_dir(Some("  D:/somewhere/icons  "), fallback),
        PathBuf::from("D:/somewhere/icons"),
        "绝对路径原样用，两边的空白要去掉"
    );
    assert_eq!(
        resolve_dir(Some("assets/icons"), fallback),
        std::env::current_dir().unwrap().join("assets/icons"),
        "相对路径挂在程序运行当前路径上——写 `data/icons` 比写一整条绝对路径好读"
    );
    // 留空（没配）与只有空白是一回事：用默认。
    assert_eq!(resolve_dir(Some("   "), fallback), resolve_dir(None, fallback));
    assert_eq!(resolve_dir(Some(""), fallback), resolve_dir(None, fallback));
}

#[test]
fn an_empty_library_is_an_empty_list_not_an_error() {
    let dir = temp_dir("empty");
    assert!(list(&dir).unwrap().is_empty(), "还没存过图标不是错误");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 同一个名字存多次 = 攒出多张变体，**不覆盖**。
///
/// 这是这个模块最核心的一条：同一个图标在选中 / 未选中 / 带气泡时长得不一样，
/// 而它们指的是同一个图标。旧的行为是"重名就拒绝"，那等于逼着人给同一件事
/// 起四个名字，然后在配置里勾四次——漏一次就有一个状态匹配不上。
#[test]
fn one_name_holds_every_variant_you_save() {
    let dir = temp_dir("variants");
    let shot = frame(60, 40);

    for expected in 1..=4 {
        let saved = save(&dir, "聊天", &shot, BOX).unwrap();
        assert_eq!(
            saved.variants.len(),
            expected,
            "第 {expected} 次保存之后应当有 {expected} 张变体"
        );
    }

    let listed = list(&dir).unwrap();
    assert_eq!(listed.len(), 1, "四次保存都是同一个图标，列表里只该有一行");
    assert_eq!(listed[0].name, "聊天");
    assert_eq!(listed[0].variants.len(), 4);
    assert_eq!(
        listed[0].variants.iter().map(|item| item.relative.clone()).collect::<Vec<_>>(),
        ["聊天/1.png", "聊天/2.png", "聊天/3.png", "聊天/4.png"],
        "变体按编号排，编号从 1 开始"
    );
    for variant in &listed[0].variants {
        assert_eq!((variant.width, variant.height), (16, 16));
        assert!(variant.problem.is_none());
        assert!(variant.image.starts_with("data:image/png;base64,"));
        assert!(Path::new(&variant.file).exists());
    }
    assert!(listed[0].usable);

    // 两次保存的内容确实是**两张不同的文件**，不是同一张被写了两遍。
    assert_ne!(listed[0].variants[0].file, listed[0].variants[1].file);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 删一张变体只影响那一张；删到一张不剩，这个名字就整个消失。
#[test]
fn deleting_a_variant_leaves_the_others_and_emptying_the_icon_removes_it() {
    let dir = temp_dir("delete-variant");
    let shot = frame(60, 40);
    for _ in 0..3 {
        save(&dir, "聊天", &shot, BOX).unwrap();
    }

    delete_variant(&dir, "聊天", "聊天/2.png").unwrap();
    let listed = list(&dir).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0].variants.iter().map(|item| item.relative.clone()).collect::<Vec<_>>(),
        ["聊天/1.png", "聊天/3.png"],
        "删掉中间那张之后，剩下的编号不重排"
    );

    delete_variant(&dir, "聊天", "聊天/1.png").unwrap();
    delete_variant(&dir, "聊天", "聊天/3.png").unwrap();
    assert!(
        list(&dir).unwrap().is_empty(),
        "一张不剩之后这个名字不该还在列表里（留个空目录会让人以为图标还在）"
    );
    assert!(!group_dir(&dir, "聊天").exists(), "空目录要顺手清掉");

    // 再删一次要明确报"没有"，而不是静默成功。
    assert!(delete_variant(&dir, "聊天", "聊天/1.png").is_err());

    let _ = std::fs::remove_dir_all(&dir);
}

/// 删除命令**不能**被前端传来的一串路径带出图标库。
///
/// 这是个删除命令，`relative` 来自界面。不校验的话，一个 `../../重要文件.png`
/// 就能删掉磁盘上任何东西——而界面上什么异常都看不出来。
#[test]
fn delete_variant_refuses_anything_outside_its_own_icon() {
    let dir = temp_dir("delete-escape");
    let shot = frame(60, 40);
    save(&dir, "聊天", &shot, BOX).unwrap();
    save(&dir, "通讯录", &shot, BOX).unwrap();

    for bad in [
        "../evil.png",
        "..\\evil.png",
        "C:/Windows/System32/calc.png",
        "通讯录/1.png",        // 别人家的图标
        "聊天/../../evil.png", // 想往上爬
        "聊天",                // 不是文件
        "",
    ] {
        let err = delete_variant(&dir, "聊天", bad);
        assert!(err.is_err(), "「{bad}」不该被当成「聊天」的变体删掉");
    }

    // 两个图标都还在，一张不少。
    let listed = list(&dir).unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().all(|entry| entry.variants.len() == 1));

    let _ = std::fs::remove_dir_all(&dir);
}

/// 一整个图标（含全部变体）可以一次删掉。
#[test]
fn deleting_an_icon_removes_every_variant_of_it() {
    let dir = temp_dir("delete-icon");
    let shot = frame(60, 40);
    for _ in 0..3 {
        save(&dir, "聊天", &shot, BOX).unwrap();
    }
    save(&dir, "通讯录", &shot, BOX).unwrap();

    delete(&dir, "聊天").unwrap();
    let listed = list(&dir).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "通讯录");

    // 再删一次要明确报"没有"，而不是静默成功——静默成功会让界面以为删掉了。
    assert!(delete(&dir, "聊天").is_err());

    let _ = std::fs::remove_dir_all(&dir);
}

/// 名字 → 全部变体的路径。配置里存的就是名字，靠它展开。
#[test]
fn a_name_resolves_to_every_one_of_its_variants() {
    let dir = temp_dir("resolve");
    let shot = frame(60, 40);
    for _ in 0..3 {
        save(&dir, "聊天", &shot, BOX).unwrap();
    }
    save(&dir, "通讯录", &shot, BOX).unwrap();

    let paths = resolve_selection(&dir, &["聊天".into(), "  通讯录 ".into(), "".into()]).unwrap();
    assert_eq!(paths.len(), 4, "聊天三张 + 通讯录一张，空白项忽略：{paths:?}");
    assert!(paths.iter().all(|path| path.is_file()));

    // 同一个名字写两遍不该把同一张图载入两次。
    let once = resolve_selection(&dir, &["聊天".into()]).unwrap();
    let twice = resolve_selection(&dir, &["聊天".into(), "聊天".into()]).unwrap();
    assert_eq!(once.len(), twice.len());

    let _ = std::fs::remove_dir_all(&dir);
}

/// 名字对不上（被删了 / 图标库目录换了）要报出来，而且要说清去哪儿看。
#[test]
fn resolving_a_name_that_is_not_in_the_library_says_so() {
    let dir = temp_dir("resolve-missing");
    let err = resolve_selection(&dir, &["聊天".into()]).unwrap_err();
    assert!(err.contains("聊天"), "{err}");
    assert!(err.contains("图标库"), "要告诉人下一步去哪看：{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 旧配置里存的是**文件路径**。给一句能照着做的提示，而不是"图标不见了"。
#[test]
fn a_leftover_path_from_the_old_config_is_explained_not_swallowed() {
    let dir = temp_dir("resolve-legacy");
    let err = resolve_selection(&dir, &["D:\\icons\\聊天.png".into()]).unwrap_err();
    assert!(err.contains("名字"), "要说清现在按名字引用：{err}");

    let err = resolve_selection(&dir, &["icons/聊天".into()]).unwrap_err();
    assert!(err.contains("路径"), "{err}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 旧式的单文件图标（`<名字>.png`）照样认。
///
/// 图标库目录是给人看也给人手工放的：命令行 `screen_probe template` 产出的
/// 就是单文件。往里丢一张应当在列表里看得见，而不是静默消失。
#[test]
fn a_legacy_single_file_icon_is_still_listed_and_resolvable() {
    let dir = temp_dir("legacy");
    let shot = frame(60, 40);
    let saved = save(&dir, "聊天", &shot, BOX).unwrap();
    // 把目录里的那张搬成旧式单文件。
    let inside = PathBuf::from(&saved.variants[0].file);
    std::fs::rename(&inside, legacy_file(&dir, "聊天")).unwrap();
    std::fs::remove_dir(group_dir(&dir, "聊天")).unwrap();

    let listed = list(&dir).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].variants.len(), 1);
    assert_eq!(listed[0].variants[0].relative, "聊天.png");
    assert!(listed[0].usable);

    let paths = resolve_selection(&dir, &["聊天".into()]).unwrap();
    assert_eq!(paths.len(), 1);
    assert!(paths[0].is_file());

    // 旧式单文件也能单独删掉。
    delete_variant(&dir, "聊天", "聊天.png").unwrap();
    assert!(list(&dir).unwrap().is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

/// 变体按**数字**排，不按字符串：`2.png` 要排在 `10.png` 前面。
#[test]
fn variants_are_ordered_by_number_not_by_text() {
    let dir = temp_dir("order");
    let shot = frame(60, 40);
    let group = group_dir(&dir, "聊天");
    std::fs::create_dir_all(&group).unwrap();

    // 直接放文件，绕开 save 的编号分配，专门造出 1/2/10 这种顺序陷阱。
    for name in ["10.png", "2.png", "1.png"] {
        let single = save(&dir, "别的", &shot, BOX).unwrap();
        std::fs::rename(&single.variants[0].file, group.join(name)).unwrap();
    }
    std::fs::remove_dir_all(group_dir(&dir, "别的")).unwrap();

    let listed = list(&dir).unwrap();
    let order: Vec<String> = listed[0]
        .variants
        .iter()
        .map(|item| item.relative.clone())
        .collect();
    assert_eq!(order, ["聊天/1.png", "聊天/2.png", "聊天/10.png"]);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 不可用的框不留下任何东西——**包括那个刚建出来的空目录**。
///
/// 留下来的话，界面上会多出一行"空的图标"，而它会在任务装配时炸。
#[test]
fn an_unusable_crop_leaves_nothing_behind() {
    let dir = temp_dir("unusable");
    let shot = frame(60, 40);

    // 太小：裁出来存不下一个有意义的模板。
    let err = save(&dir, "太小", &shot, Rect { x: 4, y: 4, width: 2, height: 2 })
        .unwrap_err();
    assert!(err.contains("不能当图标模板"), "{err}");

    // 关键的一半：**文件不能留在库里**。留下来的话它会在任务里
    // 表现为"分数很低"，而那时人只会去怀疑阈值。
    assert!(list(&dir).unwrap().is_empty(), "被拒绝的模板不能在图标库里留下文件");
    assert!(!group_dir(&dir, "太小").exists(), "空目录也要清掉");

    // 越出画面：同样是拒绝，同样不留文件。
    assert!(save(&dir, "越界", &shot, Rect { x: 50, y: 30, width: 30, height: 30 }).is_err());
    assert!(list(&dir).unwrap().is_empty());

    // 已经有变体时，一次失败的保存不该动到已有那些。
    save(&dir, "聊天", &shot, BOX).unwrap();
    assert!(save(&dir, "聊天", &shot, Rect { x: 4, y: 4, width: 2, height: 2 }).is_err());
    let listed = list(&dir).unwrap();
    assert_eq!(listed[0].variants.len(), 1, "失败的保存不该影响已有变体");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 坏文件**留在列表里并带上原因**，而不是让整个列表打不开。
#[test]
fn a_broken_file_shows_up_with_a_reason_instead_of_breaking_the_list() {
    let dir = temp_dir("broken");
    let shot = frame(60, 40);
    save(&dir, "好的", &shot, BOX).unwrap();
    save(&dir, "坏的", &shot, BOX).unwrap();

    // 模拟"有人拿别的工具把文件改坏了"：列表要照常打开，坏的那项带上原因。
    let broken_path = group_dir(&dir, "坏的").join("1.png");
    std::fs::write(&broken_path, b"this is not a png").unwrap();
    std::fs::write(dir.join("说明.txt"), b"ignored").unwrap();

    let listed = list(&dir).unwrap();
    assert_eq!(listed.len(), 2, "非 PNG 文件要跳过，坏 PNG 要留下");
    let broken = listed.iter().find(|item| item.name == "坏的").unwrap();
    assert!(broken.variants[0].problem.is_some(), "坏文件必须带上原因");
    assert_eq!((broken.variants[0].width, broken.variants[0].height), (0, 0));
    assert!(!broken.usable, "有一张坏的，这个图标就不能当导航模板");
    let good = listed.iter().find(|item| item.name == "好的").unwrap();
    assert!(good.variants[0].problem.is_none());
    assert!(good.usable);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_listing_is_sorted_case_insensitively() {
    let dir = temp_dir("sorted");
    let shot = frame(60, 40);
    for name in ["banana", "Apple", "apple2", "Cherry"] {
        save(&dir, name, &shot, BOX).unwrap();
    }
    let names: Vec<String> = list(&dir).unwrap().into_iter().map(|e| e.name).collect();
    assert_eq!(names, ["Apple", "apple2", "banana", "Cherry"]);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 同一个名字底下混着旧式单文件和新目录时，两边都算变体，且只占一行。
#[test]
fn a_legacy_file_and_a_group_merge_into_one_entry() {
    let dir = temp_dir("merge");
    let shot = frame(60, 40);
    let saved = save(&dir, "聊天", &shot, BOX).unwrap();
    std::fs::copy(&saved.variants[0].file, legacy_file(&dir, "聊天")).unwrap();

    let listed = list(&dir).unwrap();
    assert_eq!(listed.len(), 1, "同一个名字只该有一行");
    assert_eq!(
        listed[0].variants.iter().map(|item| item.relative.clone()).collect::<Vec<_>>(),
        ["聊天.png", "聊天/1.png"],
        "旧式单文件排在最前（它是最早那张）"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
