//! 热键解析、虚拟键码换算与失败文案的测试。
//!
//! 真正「注册之后按一下会不会触发」测不了——那要有人按键。这里测的是
//! **解析与换算**：它们错了不会报错，只会让注册的键和界面显示的不是同一个，
//! 而症状是「按了没反应」，最难往解析上想。

use super::*;

#[test]
fn parses_letters_case_insensitively_and_ignores_padding() {
    assert_eq!(HotkeyKey::parse("s"), Some(HotkeyKey::Letter(b'S')));
    assert_eq!(HotkeyKey::parse("S"), Some(HotkeyKey::Letter(b'S')));
    assert_eq!(HotkeyKey::parse("  q  "), Some(HotkeyKey::Letter(b'Q')));
    assert_eq!(HotkeyKey::parse("A"), Some(HotkeyKey::Letter(b'A')));
    assert_eq!(HotkeyKey::parse("Z"), Some(HotkeyKey::Letter(b'Z')));
}

#[test]
fn parses_digits() {
    assert_eq!(HotkeyKey::parse("0"), Some(HotkeyKey::Digit(b'0')));
    assert_eq!(HotkeyKey::parse("9"), Some(HotkeyKey::Digit(b'9')));
}

#[test]
fn parses_function_keys() {
    assert_eq!(HotkeyKey::parse("F1"), Some(HotkeyKey::Function(1)));
    assert_eq!(HotkeyKey::parse("f7"), Some(HotkeyKey::Function(7)));
    assert_eq!(HotkeyKey::parse("F12"), Some(HotkeyKey::Function(12)));
}

/// `F` 是字母键，`F1` 才是功能键——两种写法共用一个前缀，这一支最容易写错。
#[test]
fn a_lone_f_is_the_letter_not_a_function_key() {
    assert_eq!(HotkeyKey::parse("F"), Some(HotkeyKey::Letter(b'F')));
    assert_eq!(HotkeyKey::parse("f"), Some(HotkeyKey::Letter(b'F')));
}

#[test]
fn rejects_out_of_range_function_keys() {
    // F0 与 F13 都不是合法编号。**不能**退化成"按字母 F 处理"——
    // 那会让界面写着 F13、实际注册的是 F，症状是「按 F13 没反应，按 F 反而触发了」。
    assert_eq!(HotkeyKey::parse("F0"), None);
    assert_eq!(HotkeyKey::parse("F13"), None);
    assert_eq!(HotkeyKey::parse("F999"), None);
}

#[test]
fn rejects_junk() {
    assert_eq!(HotkeyKey::parse(""), None);
    assert_eq!(HotkeyKey::parse("   "), None);
    assert_eq!(HotkeyKey::parse("AB"), None);
    assert_eq!(HotkeyKey::parse("FF"), None);
    assert_eq!(HotkeyKey::parse("!"), None);
    assert_eq!(HotkeyKey::parse("F1A"), None);
    assert_eq!(HotkeyKey::parse("-"), None);
}

/// 三组键各占一段连续区间。这里只钉住**端点**：中间的值由区间连续性保证，
/// 把 26 个字母逐个抄一遍只会让这个测试跟着常量表一起腐烂。
#[test]
fn maps_keys_onto_their_virtual_key_ranges() {
    assert_eq!(HotkeyKey::Letter(b'A').vk(), 0x41);
    assert_eq!(HotkeyKey::Letter(b'Z').vk(), 0x5A);
    assert_eq!(HotkeyKey::Digit(b'0').vk(), 0x30);
    assert_eq!(HotkeyKey::Digit(b'9').vk(), 0x39);
    assert_eq!(HotkeyKey::Function(1).vk(), 0x70);
    assert_eq!(HotkeyKey::Function(12).vk(), 0x7B);
}

#[test]
fn label_round_trips_through_parse() {
    for raw in ["A", "Z", "0", "9", "F1", "F12", "F"] {
        let key = HotkeyKey::parse(raw).expect("应当能解析");
        assert_eq!(key.label(), raw, "label 与 parse 必须互为逆运算");
        assert_eq!(HotkeyKey::parse(&key.label()), Some(key));
    }
}

#[test]
fn requires_at_least_one_modifier() {
    // 裸按键会把那个键在整个系统里占掉，必须拒绝。
    for key in ["A", "5", "F9"] {
        let err = HotkeySpec::new(false, false, false, false, key).unwrap_err();
        assert!(err.contains("修饰键"), "错误文案要说清为什么：{err}");
    }
}

#[test]
fn accepts_any_single_modifier() {
    assert!(HotkeySpec::new(true, false, false, false, "A").is_ok());
    assert!(HotkeySpec::new(false, true, false, false, "A").is_ok());
    assert!(HotkeySpec::new(false, false, true, false, "A").is_ok());
    assert!(HotkeySpec::new(false, false, false, true, "A").is_ok());
}

#[test]
fn reports_which_key_it_could_not_use() {
    let err = HotkeySpec::new(true, false, false, false, "F13").unwrap_err();
    assert!(err.contains("F13"), "错误文案要点名是哪个键：{err}");
}

/// `MOD_NOREPEAT` 必须**无条件**带上。漏了它按住不放会连打好几张图，
/// 而症状只是"多截了几张"，没人会怀疑到修饰键位上。
#[test]
fn always_asks_the_system_not_to_repeat() {
    let bare = HotkeySpec::new(true, false, false, false, "A").expect("应当能构造");
    assert_ne!(bare.modifiers().0 & MOD_NOREPEAT.0, 0);

    let full = HotkeySpec::new(true, true, true, true, "A").expect("应当能构造");
    assert_ne!(full.modifiers().0 & MOD_NOREPEAT.0, 0);
}

#[test]
fn sets_exactly_the_requested_modifier_bits() {
    let spec = HotkeySpec::new(true, false, true, false, "A").expect("应当能构造");
    let bits = spec.modifiers().0;
    assert_ne!(bits & MOD_CONTROL.0, 0);
    assert_ne!(bits & MOD_SHIFT.0, 0);
    assert_eq!(bits & MOD_ALT.0, 0, "没勾 Alt 就不能带上");
    assert_eq!(bits & MOD_WIN.0, 0, "没勾 Win 就不能带上");
}

#[test]
fn label_lists_modifiers_then_the_key() {
    let spec = HotkeySpec::new(true, true, false, false, "s").expect("应当能构造");
    assert_eq!(spec.label(), "Ctrl+Alt+S");

    let spec = HotkeySpec::new(false, false, true, true, "F7").expect("应当能构造");
    assert_eq!(spec.label(), "Shift+Win+F7");
}

#[test]
fn explains_the_taken_hotkey_instead_of_dumping_a_code() {
    let message = describe_failure(ERROR_HOTKEY_ALREADY_REGISTERED, "occupied");
    assert!(message.contains("占用"), "要说明是被别人占了：{message}");
    assert!(message.contains("换一个"), "要给出下一步动作：{message}");
}

#[test]
fn other_failures_still_name_the_code() {
    let message = describe_failure(5, "拒绝访问");
    assert!(message.contains("5"), "要带上错误码便于排查：{message}");
    assert!(message.contains("拒绝访问"), "要带上原始描述：{message}");
    assert!(message.contains("延时截图"), "任何失败都要留一条出路：{message}");
}

/// 真机用例：注册 → 注销 → **再注册同一个组合**。
///
/// 第二次注册能成功，才证明注销真的生效了。只测「注册成功」是不够的——
/// 注册之后什么都不做也算成功，而那正是会留下一个占坑热键的写法。
///
/// 默认跳过（会占用一个真实的全系统热键）。要跑：
/// `cargo test -p platform-windows -- --ignored hotkey`
#[test]
#[ignore]
fn release_actually_frees_the_combination() {
    let spec = HotkeySpec::new(true, true, true, false, "F12").expect("应当能构造");

    let first = register(spec, || {}).expect("第一次注册应当成功（Ctrl+Alt+Shift+F12 一般没人占）");
    first.release();

    let second = register(spec, || {})
        .expect("注销之后再注册同一个组合应当成功——失败说明注销没生效、热键还占着");
    second.release();
}
