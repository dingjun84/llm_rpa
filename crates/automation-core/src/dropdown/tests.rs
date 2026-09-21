//! `judge_dropdown` 的用例：判据的**边界**与「为什么」的措辞都要钉住。
//!
//! 拆成独立文件是因为主体已经接近文件行数上限，而**测试文件不限行数**
//! （`CONVENTIONS.md` §2/§10）。
//!
//! 其中 [`the_2026_09_21_scene_answers_its_own_question`] 用的是实机现场：
//! 那一屏**读到了「联系人」三个字**，任务却报「没有任何一组」。原因就是
//! 那一块的置信度没过阈值——而当时的日志里一个字都没记，只能靠猜。

use super::*;
use crate::ports::Rect;

fn tb(text: &str, x: i32, y: i32, confidence: f32) -> TextBox {
    TextBox { text: text.into(), bounds: Rect { x, y, width: 120, height: 24 }, confidence }
}

const MIN: f32 = 0.85;
const LABELS: &str = "联系人 / 最常使用";

/// 2026-09-21 实机现场：19 块文字，「联系人」与「最常使用」都在，但**置信度不够**。
///
/// 这一帧的失败文案是「搜索下拉列表里没有找到「联系人 / 最常使用」中的任何一组，
/// 读到 19 块文字」——看的人只会以为客户端没画出这个分组。
fn the_2026_09_21_boxes() -> Vec<TextBox> {
    vec![
        tb("联系人", 8, 4, 0.61),
        tb("最常使用", 8, 30, 0.68),
        tb("李小明", 20, 60, 0.97),
        tb("Jerry-李小明同学", 20, 84, 0.95),
        tb("太过活跃李小明", 20, 108, 0.93),
        tb("测试群一", 20, 132, 0.96),
        tb("和 李小明 的聊天", 20, 156, 0.94),
        tb("群聊", 8, 180, 0.92),
        tb("测试群二", 20, 204, 0.91),
        tb("李小明：可以了，登录进去了", 20, 228, 0.96),
        tb("李小明 邀请你加入了群聊", 300, 228, 0.93),
        tb("联系人", 620, 4, 0.66),
        tb("搜索", 300, 4, 0.90),
        tb("聊天记录", 8, 252, 0.90),
        tb("李小明", 20, 276, 0.40),
        tb("文件传输助手", 20, 300, 0.99),
        tb("王小明", 20, 324, 0.98),
        tb("李四", 20, 348, 0.97),
        tb("外部测试账号", 20, 372, 0.96),
    ]
}

/// ★ 这一条是整件事的验收：**一行回答**「19 块里有「联系人」，为什么没找到」。
#[test]
fn the_2026_09_21_scene_answers_its_own_question() {
    let boxes = the_2026_09_21_boxes();
    assert_eq!(boxes.len(), 19, "现场就是 19 块，用例要跟着现场走");
    let judged = judge_dropdown(&boxes, "李小明", LABELS, MIN);

    let err = judged.result.as_ref().expect_err("这一帧确实找不到分组");
    let msg = err.to_string();
    assert!(msg.contains("任何一组"), "失败文案应当说清是分组没认出来：{msg}");
    assert!(msg.contains("19 块"), "块数要报出来：{msg}");
    assert!(!judged.decision.passed, "成没成要结构化地记一笔，界面靠它定位失败那一步");

    // 关键的那一行：它**读到了**，只是没过阈值。理由里带两个数字，一眼可对。
    let title_row = judged
        .decision
        .candidates
        .iter()
        .find(|v| v.text.trim() == "联系人")
        .expect("那一块「联系人」必须留在轨迹里");
    assert!(!title_row.passed);
    assert!(title_row.reason.contains("0.61"), "要有它自己的置信度：{}", title_row.reason);
    assert!(title_row.reason.contains("0.85"), "要有阈值：{}", title_row.reason);
    assert!(
        title_row.reason.contains("没参与判定"),
        "要说清它是被阈值挡下的，不是没读到：{}",
        title_row.reason
    );
}

/// 轨迹必须逐块交代，且**顺序与画面一致**——界面上是按这个顺序画表的。
#[test]
fn every_block_gets_a_row_in_screen_order() {
    let boxes = the_2026_09_21_boxes();
    let judged = judge_dropdown(&boxes, "李小明", LABELS, MIN);
    assert_eq!(judged.decision.candidates.len(), boxes.len());
    for (verdict, block) in judged.decision.candidates.iter().zip(boxes.iter()) {
        assert_eq!(verdict.text, block.text, "轨迹要按画面顺序逐块对上");
        assert_eq!(verdict.bounds, block.bounds);
    }
}

/// 分组里命中多行 ⇒ 取**最上面**那一行，并且要给落选的那一行写清原因
/// （2026-09-21 操作者两次修正后的判据，见模块文档）。
#[test]
fn the_topmost_hit_wins_and_the_loser_is_told_why() {
    let boxes = vec![
        tb("联系人", 8, 4, 0.99),
        tb("李小明", 20, 60, 0.97),
        tb("Jerry-李小明同学", 20, 84, 0.96),
    ];
    let judged = judge_dropdown(&boxes, "李小明", LABELS, MIN);
    let hit = judged.result.expect("分组里明明有这个人");
    assert!(judged.decision.passed, "选中了就是通过了");
    assert_eq!(hit.text, "李小明", "客户端自己把最匹配的排在最上面，要信它");
    assert!(judged.decision.outcome.contains("取最上面"), "{}", judged.decision.outcome);
    let loser = &judged.decision.candidates[2];
    assert!(!loser.passed);
    assert!(loser.reason.contains("取最上面"), "{}", loser.reason);
    assert!(judged.decision.candidates[1].passed);
}

/// 命中行落到下一个已知分区标题之后就不算联系人：否则会把「群聊」里的
/// 同名消息当成联系人点下去。
#[test]
fn a_hit_below_the_next_section_header_is_not_a_contact() {
    let boxes = vec![
        tb("联系人", 8, 4, 0.99),
        tb("李小明", 20, 60, 0.97),
        tb("群聊", 8, 120, 0.98),
        tb("李小明：可以了，登录进去了", 20, 150, 0.96),
    ];
    let judged = judge_dropdown(&boxes, "李小明", LABELS, MIN);
    assert_eq!(judged.result.expect("联系人分组里有他").bounds.y, 60);
    let below = &judged.decision.candidates[3];
    assert!(!below.passed);
    assert!(below.reason.contains("群聊"), "要指名被哪个分区截断：{}", below.reason);
}

/// 「联系人」缺席时，同一个人可能只出现在「最常使用」底下（Mac 微信的常态），
/// 所以配置里可以写多项，每一项各自找标题。
#[test]
fn the_second_configured_label_is_tried_too() {
    let boxes = vec![tb("最常使用", 8, 4, 0.99), tb("王小明", 20, 60, 0.98)];
    let judged = judge_dropdown(&boxes, "王小明", LABELS, MIN);
    assert_eq!(judged.result.expect("「最常使用」里应当认得出他").text, "王小明");
    assert!(judged.decision.candidates[0].reason.contains("最常使用"), "{}", judged.decision.candidates[0].reason);
}

/// 标题认**逐字相等**优先，退到"包含"是留给 OCR 多读一个字符的情况。
/// 反过来（先看包含）会把一行含「联系人」的聊天记录认成标题，
/// 症状是"点到了不相干的一行"，看不出是标题认错了。
#[test]
fn the_title_prefers_an_exact_row_over_a_containing_one() {
    let boxes = vec![
        tb("联系人：李小明", 20, 100, 0.99),
        tb("联系人", 8, 4, 0.99),
        tb("李小明", 20, 60, 0.97),
    ];
    let judged = judge_dropdown(&boxes, "李小明", LABELS, MIN);
    assert_eq!(judged.result.expect("应当认在逐字相等那一行下面").bounds.y, 60);
    let title_row = judged
        .decision
        .candidates
        .iter()
        .find(|v| v.text == "联系人")
        .expect("标题那一块要在轨迹里");
    assert!(title_row.reason.contains("逐字相等"), "{}", title_row.reason);
}

/// 空下拉（区域标错、或者界面还没弹出来）要报"没有分组"，而不是报"没有这个人"：
/// 两者的处置方向完全相反。
#[test]
fn an_empty_dropdown_reports_the_region_or_the_client() {
    let judged = judge_dropdown(&[], "李小明", LABELS, MIN);
    let msg = judged.result.as_ref().expect_err("空的一帧不可能挑出人").to_string();
    assert!(msg.contains("任何一组"), "{msg}");
    assert!(msg.contains("标定偏了"), "要提示区域标定的可能：{msg}");
    assert!(judged.decision.candidates.is_empty());
}

/// 重放要的三样输入（关键词、分组标题原文、阈值）必须跟着决策一起留下，
/// 否则离线重放只能靠人再猜一遍当时搜的是什么。
#[test]
fn the_decision_carries_everything_a_replay_needs() {
    let boxes = vec![tb("联系人", 8, 4, 0.99), tb("李小明", 20, 60, 0.97)];
    let judged = judge_dropdown(&boxes, "李小明", LABELS, MIN);
    assert_eq!(judged.decision.min_confidence, MIN);
    match judged.decision.replay {
        Some(ReplayInput::Dropdown { keyword, group_labels }) => {
            assert_eq!(keyword, "李小明");
            assert_eq!(group_labels, LABELS);
        }
        other => panic!("下拉判据必须留下重放输入，实际：{other:?}"),
    }
    assert!(judged.decision.question.contains("李小明"), "{}", judged.decision.question);
}

/// 归一化只去掉空白与控制字符：`Windows.Media.Ocr` 会在字与字之间塞空格
/// （实测把「外部测试联系人」读成「外部 测试 联系人」），不归一化就一个都匹配不上。
#[test]
fn spaces_between_characters_do_not_break_the_containment() {
    let boxes = vec![tb("联系人", 8, 4, 0.99), tb("外部 测试 联系人", 20, 60, 0.98)];
    assert_eq!(normalize_text("外部 测试 联系人"), "外部测试联系人");
    // 名字压根不在这屏：照样如实失败，不能因为"归一化过了"就宽松一点。
    let judged = judge_dropdown(&boxes, "李小明", LABELS, MIN);
    assert!(judged.result.is_err(), "这一屏没有「李小明」，必须失败");
    // 关键词「外部」与那一行之间隔着 OCR 塞进去的空格：不归一化就匹配不上。
    let judged = judge_dropdown(&boxes, "外部", LABELS, MIN);
    assert_eq!(judged.result.expect("归一化之后应当匹配上").text, "外部 测试 联系人");
}